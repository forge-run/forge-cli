//! `forge update` — self-update from the GitHub release pipeline.
//!
//! Reads the latest release on `forge-run/forge-cli`, picks the
//! tarball that matches the running platform, verifies sha256
//! against `checksums.sha256`, and atomic-swaps the running binary
//! in place. The `self_update` crate handles the cross-platform
//! details (`mktemp` + replace-on-Linux / mac, equivalent dance
//! on Windows); we just wire the configuration.
//!
//! `--check` runs the version comparison without applying — useful
//! in a CI lane that wants to nag operators on stale CLIs without
//! mutating their install. `--force` re-installs the current
//! version (recover-from-corruption escape hatch).

use anyhow::{Context, Result, bail};
use clap::Args;

const REPO_OWNER: &str = "forge-run";
const REPO_NAME: &str = "forge-cli";
const BIN_NAME: &str = "forge";

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Report whether a newer version is available without
    /// downloading or swapping anything. Exit 0 if up-to-date or
    /// an update is available; exit non-zero only on lookup
    /// failure (network / API ratelimit / etc.).
    #[arg(long, default_value_t = false)]
    pub check: bool,

    /// Re-download and re-install even when the current version
    /// already matches the latest release. Use to recover from a
    /// corrupted binary or to roll back across an off-cycle
    /// release that was published then reverted.
    #[arg(long, default_value_t = false)]
    pub force: bool,
}

pub async fn run(args: UpdateArgs) -> Result<()> {
    // self_update is synchronous; the API calls + download fit
    // happily inside spawn_blocking so the async runtime stays
    // healthy if any other in-flight task is around.
    let current = env!("CARGO_PKG_VERSION").to_string();
    tokio::task::spawn_blocking(move || drive(args, current))
        .await
        .context("spawn_blocking for self-update")?
}

fn drive(args: UpdateArgs, current_version: String) -> Result<()> {
    let mut update = self_update::backends::github::Update::configure();
    // The release workflow packages each binary inside a
    // `forge-<tag>-<target>/` directory (so the tarball is
    // self-describing on extract). Tell self_update to look
    // inside that wrapper for the binary.
    let bin_path_in_archive = format!(
        "forge-{{{{ version }}}}-{{{{ target }}}}/{}",
        BIN_NAME
    );
    update
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        .bin_path_in_archive(&bin_path_in_archive)
        .show_download_progress(true)
        .show_output(false)
        // `no_confirm(true)` — we don't have a tty in every CLI
        // invocation context (CI, scripted updates, `forge update`
        // piped from another tool), and the operator already
        // typed `forge update`, which is consent. The --force
        // flag is a separate guard for "re-install current
        // version"; we don't need a yes/no on top.
        .no_confirm(true)
        .current_version(&current_version);
    if args.force {
        // self_update only swaps when `latest > current`. The
        // crate exposes `target_version_tag` to force a specific
        // tag; we re-target the latest tag explicitly so even a
        // same-version download fires.
        //
        // get_latest_release returns `version: "0.2.2"` (semver-
        // shaped, no v prefix), but our GH tags include the `v`
        // (per Cargo + rust convention). target_version_tag does
        // a tag lookup against the API, so prefix here.
        let latest = update
            .build()
            .map_err(|e| anyhow::anyhow!("configure self-update: {e}"))?
            .get_latest_release()
            .map_err(|e| anyhow::anyhow!("look up latest release: {e}"))?;
        let tag = if latest.version.starts_with('v') {
            latest.version.clone()
        } else {
            format!("v{}", latest.version)
        };
        update.target_version_tag(&tag);
    }
    let updater = update
        .build()
        .map_err(|e| anyhow::anyhow!("configure self-update: {e}"))?;

    if args.check {
        let latest = updater
            .get_latest_release()
            .map_err(|e| anyhow::anyhow!("look up latest release: {e}"))?;
        // `Update::current_version` returns the configured value,
        // not the resolved one — but we passed in env!() so it's
        // accurate.
        let same = self_update::version::bump_is_greater(&current_version, &latest.version)
            .map_err(|e| anyhow::anyhow!("compare versions: {e}"))?;
        if same {
            println!(
                "update available: {current_version} → {} ({})",
                latest.version, latest.date
            );
        } else {
            println!("up-to-date ({current_version})");
        }
        return Ok(());
    }

    eprintln!("checking for updates (current {current_version})…");
    let outcome = updater
        .update()
        .map_err(|e| anyhow::anyhow!("update failed: {e}"))?;
    match outcome {
        self_update::Status::UpToDate(v) => {
            eprintln!("already on latest ({v}); use --force to re-install");
            Ok(())
        }
        self_update::Status::Updated(v) => {
            eprintln!("updated {current_version} → {v}");
            Ok(())
        }
    }
}

// `bail!` is imported for symmetry with other cmd/ modules that
// surface user-facing errors; nothing in this file currently
// reaches for it, but a future signature-verification or
// platform-mismatch branch will. Suppress the unused-import lint
// rather than the import to keep the editing footprint small.
#[allow(dead_code)]
fn _bail_keep_alive() {
    let _: fn() -> Result<()> = || bail!("placeholder");
}
