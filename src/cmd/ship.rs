//! `forge ship` — upload static assets, THEN deploy, in one command.
//!
//! `static upload` and `deploy` are separate commands, and the order
//! matters: if you deploy before uploading the freshly-hashed assets the
//! new build references, the edge (Cloudflare) can cache a 404 for the new
//! asset URL for hours, serving an unstyled page in the gap. `ship` removes
//! the footgun by doing both in the correct order from a single invocation.

use std::path::PathBuf;

use anyhow::Result;

use crate::client::ForgeClient;
use crate::cmd::{deploy, static_cmd};

#[derive(Debug, clap::Args)]
pub struct ShipArgs {
    /// Static assets directory to upload BEFORE deploying. Skipped if it
    /// doesn't exist (with a note), so app-only workloads still `ship`.
    #[arg(long, default_value = "static")]
    pub static_dir: PathBuf,

    /// Deploy without uploading static assets (equivalent to `forge deploy`).
    #[arg(long)]
    pub no_static: bool,

    /// All the usual `forge deploy` arguments.
    #[command(flatten)]
    pub deploy: deploy::DeployArgs,
}

pub async fn run(args: ShipArgs, client: &ForgeClient) -> Result<()> {
    if args.no_static {
        eprintln!("ship: --no-static set, skipping static upload");
    } else if args.static_dir.exists() {
        eprintln!(
            "ship: uploading static assets from {} …",
            args.static_dir.display()
        );
        let cmd = static_cmd::StaticCmd::Upload(static_cmd::UploadArgs::for_dir(
            args.static_dir.clone(),
        ));
        static_cmd::run(cmd, client).await?;
    } else {
        eprintln!(
            "ship: static dir {} not found — skipping upload (pass --static-dir to override)",
            args.static_dir.display()
        );
    }

    eprintln!("ship: deploying …");
    deploy::run(args.deploy, client).await
}
