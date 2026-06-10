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

/// One ordered step in a `ship` run. Extracted so the load-bearing
/// invariant — a static upload ALWAYS precedes the deploy — is
/// unit-testable without a live client. Reordering these steps is the
/// exact regression `ship` exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShipStep {
    UploadStatic,
    Deploy,
}

/// Pure plan for a `ship` invocation: the steps `run` will execute, in
/// order. Upload is included only when not suppressed (`--no-static`)
/// and the static dir actually exists; `Deploy` is always last.
pub(crate) fn ship_plan(no_static: bool, static_dir_exists: bool) -> Vec<ShipStep> {
    let mut steps = Vec::new();
    if !no_static && static_dir_exists {
        steps.push(ShipStep::UploadStatic);
    }
    steps.push(ShipStep::Deploy);
    steps
}

pub async fn run(args: ShipArgs, client: &ForgeClient) -> Result<()> {
    let dir_exists = args.static_dir.exists();
    let steps = ship_plan(args.no_static, dir_exists);

    if steps.contains(&ShipStep::UploadStatic) {
        eprintln!(
            "ship: uploading static assets from {} …",
            args.static_dir.display()
        );
        let cmd = static_cmd::StaticCmd::Upload(static_cmd::UploadArgs::for_dir(
            args.static_dir.clone(),
        ));
        static_cmd::run(cmd, client).await?;
    } else if args.no_static {
        eprintln!("ship: --no-static set, skipping static upload");
    } else {
        eprintln!(
            "ship: static dir {} not found — skipping upload (pass --static-dir to override)",
            args.static_dir.display()
        );
    }

    eprintln!("ship: deploying …");
    deploy::run(args.deploy, client).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_uploads_static_before_deploy() {
        let steps = ship_plan(false, true);
        assert_eq!(steps, vec![ShipStep::UploadStatic, ShipStep::Deploy]);
        // The invariant the command exists to enforce: upload precedes
        // deploy, never the other way around.
        let up = steps.iter().position(|s| *s == ShipStep::UploadStatic).unwrap();
        let dep = steps.iter().position(|s| *s == ShipStep::Deploy).unwrap();
        assert!(up < dep, "static upload must precede deploy");
    }

    #[test]
    fn plan_skips_upload_with_no_static_flag() {
        // --no-static deploys only, even when the static dir is present.
        assert_eq!(ship_plan(true, true), vec![ShipStep::Deploy]);
        assert_eq!(ship_plan(true, false), vec![ShipStep::Deploy]);
    }

    #[test]
    fn plan_skips_upload_when_dir_absent() {
        // App-only workloads with no static dir still deploy.
        assert_eq!(ship_plan(false, false), vec![ShipStep::Deploy]);
    }

    #[test]
    fn plan_always_deploys() {
        for no_static in [false, true] {
            for exists in [false, true] {
                assert!(
                    ship_plan(no_static, exists).contains(&ShipStep::Deploy),
                    "deploy must always run (no_static={no_static}, exists={exists})"
                );
            }
        }
    }
}
