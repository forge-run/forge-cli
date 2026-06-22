//! `forge sdk` — generate a typed client SDK from the workspace registry.
//!
//! `forge sdk generate` calls `GET /api/v1/platform/sdk` (the
//! `_platform::sdk::generate` op), which walks the same canonical
//! registry the OpenAPI / MCP emitters use and returns a self-contained
//! TypeScript client package: `{ package_name, registry_version, files }`
//! where `files` maps a relative path to its source. We write each file
//! under `--out`, creating parent directories as needed.
//!
//! The package is content-addressed to the registry version it was
//! generated from (its `package.json` version is `0.0.<registry_version>`),
//! so re-running after an API change produces a new, bumped package.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde::Deserialize;

use crate::client::{ForgeClient, ForgeError};

#[derive(Debug, Subcommand)]
pub enum SdkCmd {
    /// Generate a typed client SDK from the workspace's live registry.
    Generate(GenerateArgs),
}

#[derive(Debug, Args)]
pub struct GenerateArgs {
    /// Directory to write the generated package into (created if absent).
    #[arg(long, default_value = "./sdk")]
    out: PathBuf,

    /// Client language to generate. Only `typescript` is available today.
    #[arg(long, default_value = "typescript")]
    target: String,

    /// Print the manifest (package name, version, file list) without
    /// writing any files.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
struct SdkPackage {
    package_name: String,
    registry_version: u64,
    /// Relative path → file source. Ordered for deterministic output.
    files: BTreeMap<String, String>,
}

pub async fn run(cmd: SdkCmd, client: &ForgeClient) -> Result<()> {
    match cmd {
        SdkCmd::Generate(args) => generate(args, client).await,
    }
}

async fn generate(args: GenerateArgs, client: &ForgeClient) -> Result<()> {
    let path = format!("/api/v1/platform/sdk?target={}", urlencoding(&args.target));
    let pkg: SdkPackage = client.get_json(&path).await.map_err(map_err)?;

    if pkg.files.is_empty() {
        // The op only returns an empty file set if the caller can see no
        // operations — surface that rather than silently writing nothing.
        anyhow::bail!(
            "registry produced no SDK files — the bearer may not have visibility \
             into any operations on this workspace"
        );
    }

    if args.dry_run {
        println!(
            "{} (registry v{}) — {} file(s):",
            pkg.package_name,
            pkg.registry_version,
            pkg.files.len()
        );
        for name in pkg.files.keys() {
            println!("  {name}");
        }
        println!("(dry run — nothing written)");
        return Ok(());
    }

    for (rel, contents) in &pkg.files {
        let dest = safe_join(&args.out, rel)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&dest, contents).with_context(|| format!("writing {}", dest.display()))?;
    }

    println!(
        "Wrote {} ({} file(s), registry v{}) to {}",
        pkg.package_name,
        pkg.files.len(),
        pkg.registry_version,
        args.out.display()
    );
    Ok(())
}

/// Minimal percent-encoding for a query value — the target is a short
/// alnum token (`typescript`), so we only need to keep it safe, not
/// pull in a dependency.
fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}

/// Join a server-supplied relative path under `out`, rejecting any path
/// that would escape the output directory (absolute paths, `..`
/// traversal). The registry is trusted, but a generated artifact writing
/// outside `--out` is never intended — fail loud instead.
fn safe_join(out: &Path, rel: &str) -> Result<PathBuf> {
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        anyhow::bail!("refusing to write absolute path from SDK artifact: {rel}");
    }
    for comp in candidate.components() {
        if matches!(comp, std::path::Component::ParentDir) {
            anyhow::bail!("refusing to write path escaping --out: {rel}");
        }
    }
    Ok(out.join(candidate))
}

fn map_err(e: ForgeError) -> anyhow::Error {
    match e {
        ForgeError::Http { status, body } if status.as_u16() == 401 => anyhow::anyhow!(
            "unauthorized (401) generating SDK — run `forge login` or check your token.\n{body}"
        ),
        other => anyhow::anyhow!("generating SDK: {other}"),
    }
}
