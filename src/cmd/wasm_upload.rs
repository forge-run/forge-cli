//! `forge wasm-upload` — stage the built WebAssembly Components to the
//! workspace's content-addressed store so a subsequent `git push` (GitOps
//! converge) resolves every module by hash.
//!
//! Reads the Components `forge wasm-build` persisted to
//! `.forge/artifacts/{hash}.wasm` — the file name IS the blake3 content address
//! the lock records and the runtime verifies at converge — and HEAD-then-PUTs
//! each to `/api/v1/manage/artifacts/{hash}` (dedup: an already-stored artifact
//! transfers zero bytes). This is the step that was missing between build and
//! push: `forge build` stamped the lock but never uploaded the bytes, so a
//! git-pushed manifest referenced components the content store didn't hold and
//! the converge silently never applied.

use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::client::ForgeClient;

#[derive(Debug, clap::Args)]
pub struct WasmUploadArgs {
    /// Workspace root — must contain `.forge/artifacts/`, produced by
    /// `forge wasm-build`. Defaults to the current directory.
    #[arg(long)]
    manifest_dir: Option<PathBuf>,
}

impl WasmUploadArgs {
    /// Programmatic constructor for `forge ship`.
    pub fn for_dir(manifest_dir: Option<PathBuf>) -> Self {
        Self { manifest_dir }
    }
}

pub async fn run(args: WasmUploadArgs, client: &ForgeClient) -> Result<()> {
    let root = args
        .manifest_dir
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("resolving --manifest-dir")?;
    let art_dir = root.join(".forge").join("artifacts");
    if !art_dir.is_dir() {
        anyhow::bail!(
            "no built artifacts at {} — run `forge wasm-build` first",
            art_dir.display()
        );
    }

    let mut uploads: Vec<(String, Vec<u8>, &'static str)> = Vec::new();
    for entry in
        std::fs::read_dir(&art_dir).with_context(|| format!("reading {}", art_dir.display()))?
    {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wasm") {
            continue;
        }
        let hash = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .context("artifact filename is not valid UTF-8")?;
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        // Verify the file name matches its content: uploading bytes under the
        // wrong content address would silently fail the converge (the runtime
        // re-hashes on resolve). Fail loud here instead.
        let computed = blake3::hash(&bytes).to_hex().to_string();
        if computed != hash {
            anyhow::bail!(
                "artifact {} content-hash mismatch (computed {}) — re-run `forge wasm-build`",
                hash,
                computed
            );
        }
        uploads.push((hash, bytes, "application/wasm"));
    }

    if uploads.is_empty() {
        anyhow::bail!(
            "no .wasm artifacts in {} — run `forge wasm-build` first",
            art_dir.display()
        );
    }

    let n = uploads.len();
    let (uploaded, deduped) =
        crate::cmd::deploy::upload_artifacts_by_hash(client, &uploads).await?;
    eprintln!(
        "wasm-upload: {n} component(s) staged — {uploaded} uploaded, {deduped} already present. \
         Commit `forge.lock` + source and `git push` to converge."
    );
    Ok(())
}
