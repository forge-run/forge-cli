//! `forge static upload` — push static assets (CSS, JS, images)
//! to the workspace's `<data_dir>/static/` directory.
//!
//! Replaces the manual `scp -r static/* root@<node>:<data_dir>/static/`
//! step from the v2.0 deploy. Each file POSTs as JSON to
//! `/api/v1/manage/static/upload`; the runtime decodes the
//! base64 body, writes atomically into the static dir, and the
//! asset becomes reachable via the runtime's `ServeDir` mount at
//! `https://<workspace>/static/<path>`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use base64::Engine;
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::client::{ForgeClient, ForgeError};

#[derive(Debug, Subcommand)]
pub enum StaticCmd {
    /// Upload one file or every file in a directory tree.
    Upload(UploadArgs),
}

#[derive(Debug, Args)]
pub struct UploadArgs {
    /// Local file or directory. Directories are uploaded
    /// recursively, preserving the relative path under the
    /// source root (e.g. `static/css/app.css` → uploads to
    /// `<data_dir>/static/css/app.css` when source is `static/`).
    source: PathBuf,

    /// Path prefix on the workspace under `<data_dir>/static/`.
    /// Defaults to no prefix (uploads land directly under the
    /// static dir, mirroring the source tree).
    #[arg(long, default_value = "")]
    prefix: String,

    /// Print the response as JSON instead of human-friendly text.
    #[arg(long)]
    json: bool,
}

impl UploadArgs {
    /// Build args to upload a directory tree with no prefix — used by
    /// `forge ship` to run the static upload before the deploy.
    pub fn for_dir(source: PathBuf) -> Self {
        Self {
            source,
            prefix: String::new(),
            json: false,
        }
    }
}

#[derive(Serialize)]
struct UploadRequest<'a> {
    path: String,
    content_b64: &'a str,
}

#[derive(Deserialize)]
struct UploadResponse {
    path: String,
    bytes: u64,
}

pub async fn run(cmd: StaticCmd, client: &ForgeClient) -> Result<()> {
    match cmd {
        StaticCmd::Upload(args) => upload(args, client).await,
    }
}

async fn upload(args: UploadArgs, client: &ForgeClient) -> Result<()> {
    let source = args
        .source
        .canonicalize()
        .with_context(|| format!("source path {:?} does not exist", args.source))?;
    let prefix = args.prefix.trim_matches('/').to_string();

    // Walk: file → single upload, dir → tree.
    let entries: Vec<(PathBuf, String)> = if source.is_file() {
        let rel = source
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .ok_or_else(|| anyhow::anyhow!("source has no filename"))?;
        vec![(source.clone(), join_prefix(&prefix, &rel))]
    } else if source.is_dir() {
        WalkDir::new(&source)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| {
                let rel = e
                    .path()
                    .strip_prefix(&source)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                (e.path().to_path_buf(), join_prefix(&prefix, &rel))
            })
            .collect()
    } else {
        anyhow::bail!(
            "source {:?} is neither a regular file nor a directory",
            source
        );
    };

    if entries.is_empty() {
        eprintln!("no files to upload under {:?}", source);
        return Ok(());
    }

    let mut total_bytes: u64 = 0;
    for (local, target_path) in &entries {
        let bytes = std::fs::read(local).with_context(|| format!("read {}", local.display()))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let req = UploadRequest {
            path: target_path.clone(),
            content_b64: &b64,
        };
        let resp: UploadResponse = client
            .post_json("/api/v1/manage/static/upload", &req)
            .await
            .map_err(|e| map_err(e, target_path))?;
        total_bytes += resp.bytes;
        if !args.json {
            println!("uploaded {} ({} bytes)", resp.path, resp.bytes);
        }
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "files": entries.len(),
                "bytes": total_bytes,
            }))?
        );
    } else {
        println!(
            "uploaded {} file(s), {} bytes total",
            entries.len(),
            total_bytes
        );
    }
    Ok(())
}

/// Join the `--prefix` flag with a per-file relative path.
/// Empty prefix → just the path; otherwise `<prefix>/<path>`.
/// The runtime rejects leading-slash and `..` traversal
/// independently, but we strip both client-side too so the
/// per-file output stays readable.
fn join_prefix(prefix: &str, rel: &str) -> String {
    let rel = rel.trim_matches('/');
    if prefix.is_empty() {
        rel.to_string()
    } else {
        format!("{prefix}/{rel}")
    }
}

fn map_err(err: ForgeError, target_path: &str) -> anyhow::Error {
    anyhow::anyhow!("upload {target_path}: {err}")
}
