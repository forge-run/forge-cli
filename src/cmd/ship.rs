//! `forge ship` — the whole deploy in one command:
//!
//!   forge wasm-build → forge wasm-upload → git push → poll until converged
//!
//! Bundling the three guarantees the ordering the converge depends on — you
//! can't push stale/unstaged components by forgetting a step. Reclaims the
//! name of the removed pre-GitOps `forge ship`, now for the GitOps flow.

use anyhow::Result;
use std::path::PathBuf;

use crate::client::ForgeClient;

#[derive(Debug, clap::Args)]
pub struct ShipArgs {
    /// Skip `wasm-build` (use the Components already in `.forge/artifacts/`).
    #[arg(long)]
    no_build: bool,
    /// Push and return immediately; don't wait for the converge.
    #[arg(long)]
    no_wait: bool,
    /// Seconds to wait for convergence before failing. Default 300.
    #[arg(long, default_value_t = 300)]
    timeout: u64,
    /// Git remote to push to. Default: `forge`.
    #[arg(long, default_value = "forge")]
    remote: String,
    /// Branch to push. Defaults to the current branch.
    #[arg(long)]
    branch: Option<String>,
    /// Repo / workspace root. Defaults to the current directory.
    #[arg(long)]
    manifest_dir: Option<PathBuf>,
}

pub async fn run(args: ShipArgs, client: &ForgeClient) -> Result<()> {
    if !args.no_build {
        crate::cmd::build::run(crate::cmd::build::BuildArgs::for_dir(args.manifest_dir.clone()))
            .await?;
    }
    crate::cmd::wasm_upload::run(
        crate::cmd::wasm_upload::WasmUploadArgs::for_dir(args.manifest_dir.clone()),
        client,
    )
    .await?;
    crate::cmd::push::push_and_poll(
        client,
        args.manifest_dir.as_deref(),
        &args.remote,
        args.branch.as_deref(),
        args.no_wait,
        args.timeout,
    )
    .await
}
