//! `forge dev` — watch a workspace project's source tree and
//! redeploy on change.
//!
//! Phase G.1 of the v0.10 substrate. The agent-author loop wants
//! a single foreground process that rebuilds + redeploys when a
//! template, page.json, component.css, or `.rs` file changes —
//! without retyping `forge build && forge deploy` after every
//! edit. This is the third leg of the inner-loop stool
//! (cargo-watch for Rust, vite for JS, `forge dev` for forge
//! workspaces).
//!
//! Scope (intentionally minimal v1):
//! - Watch the working directory's `pages/`, `components/`,
//!   `src/`, `static/`, `templates/`, plus top-level `app.json`
//!   and `service.json`.
//! - Debounce 500ms (file-saves often arrive in bursts when
//!   multi-file edits land via editor "save all").
//! - On debounce fire: shell out to `forge build`, then
//!   `forge deploy --manifest service.json [--app-manifest app.json]
//!   --wasm target/wasm32-wasip1/release/<crate>.wasm`, then
//!   `forge static upload static`.
//! - Stop on Ctrl-C (notify-debouncer's thread cleanly drops).
//!
//! Out of scope for v1:
//! - File-granular partial deploys (only-changed-pages /
//!   only-changed-templates). The runtime's deploy-ingest path
//!   already debounces redundant template registrations via
//!   content-hashing; the CLI side stays dumb.
//! - HMR (browser-side live-reload). Future work.
//! - Concurrent rebuilds. We serialize: in-flight build runs to
//!   completion, then we re-coalesce pending events.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::channel;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use notify::RecursiveMode;
use notify_debouncer_full::new_debouncer;

#[derive(Debug, Args)]
pub struct DevArgs {
    /// Project root. Defaults to the current working directory.
    #[arg(long)]
    project_dir: Option<PathBuf>,

    /// Path to the service manifest. Defaults to
    /// `<project>/service.json`.
    #[arg(long)]
    manifest: Option<PathBuf>,

    /// Path to `app.json`. If present, the deploy command is
    /// invoked with `--app-manifest` so the runtime ingests the
    /// v0.10 substrate bundle (pages, components, branding).
    /// Defaults to `<project>/app.json` when that file exists.
    #[arg(long)]
    app_manifest: Option<PathBuf>,

    /// Path to the compiled wasm module. Defaults to
    /// `target/wasm32-wasip1/release/<crate>.wasm`. Optional —
    /// pure-declarative apps (no `.page.rs`) deploy without a
    /// customer wasm and route through the runtime-provided
    /// generic-renderer.
    #[arg(long)]
    wasm: Option<PathBuf>,

    /// Debounce window in milliseconds. Bursty editor saves
    /// collapse into one rebuild. Default 500ms.
    #[arg(long, default_value_t = 500)]
    debounce_ms: u64,

    /// Skip the initial deploy on startup. By default `forge dev`
    /// runs one rebuild+deploy immediately so the workspace state
    /// matches local source before the first file change.
    #[arg(long)]
    skip_initial: bool,
}

pub async fn run(args: DevArgs) -> Result<()> {
    let project_dir = args
        .project_dir
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("resolving --project-dir")?;

    let manifest = args
        .manifest
        .map(|p| resolve(&project_dir, p))
        .unwrap_or_else(|| project_dir.join("service.json"));
    if !manifest.exists() {
        anyhow::bail!(
            "no service manifest at {} — pass --manifest or run from a project root",
            manifest.display(),
        );
    }

    let app_manifest = match args.app_manifest {
        Some(p) => Some(resolve(&project_dir, p)),
        None => {
            let default = project_dir.join("app.json");
            default.exists().then_some(default)
        }
    };

    let wasm = args.wasm.map(|p| resolve(&project_dir, p));

    eprintln!("forge dev — watching {}", project_dir.display());
    if let Some(ref a) = app_manifest {
        eprintln!("  app manifest: {}", a.display());
    }
    if let Some(ref w) = wasm {
        eprintln!("  wasm output:  {}", w.display());
    }
    eprintln!("  service.json: {}", manifest.display());
    eprintln!("  debounce:     {}ms", args.debounce_ms);
    eprintln!();

    if !args.skip_initial {
        eprintln!("initial build + deploy...");
        if let Err(e) = build_and_deploy(&project_dir, &manifest, app_manifest.as_deref(), wasm.as_deref()) {
            eprintln!("initial deploy failed: {e:#}");
        }
        eprintln!();
    }

    let (tx, rx) = channel();
    let mut debouncer = new_debouncer(
        Duration::from_millis(args.debounce_ms),
        None,
        move |res| {
            // Forward events to the main thread. We don't care
            // *which* paths fired — any event in the watched set
            // triggers a full rebuild.
            let _ = tx.send(res);
        },
    )
    .context("creating file watcher")?;

    // Watch the load-bearing subtrees + top-level manifests. Each
    // path is optional (missing dirs in young projects are fine).
    // `Debouncer` re-exports `Watcher::watch` directly post-v0.6;
    // call it on the debouncer itself rather than the inner watcher.
    let watch_targets: &[&str] = &[
        "pages",
        "components",
        "src",
        "templates",
        "static",
        "app.json",
        "service.json",
    ];
    let mut watched_count = 0;
    for target in watch_targets {
        let path = project_dir.join(target);
        if path.exists() {
            debouncer
                .watch(&path, RecursiveMode::Recursive)
                .with_context(|| format!("watching {}", path.display()))?;
            watched_count += 1;
            eprintln!("  watching {}", path.display());
        }
    }
    if watched_count == 0 {
        anyhow::bail!(
            "no watchable paths found under {} — expected one of: {}",
            project_dir.display(),
            watch_targets.join(", "),
        );
    }
    eprintln!();
    eprintln!("ready. edit a file to trigger rebuild. Ctrl-C to exit.");
    eprintln!();

    for res in rx {
        match res {
            Ok(events) => {
                let n = events.len();
                eprintln!("change detected ({n} event{}), rebuilding...", if n == 1 { "" } else { "s" });
                match build_and_deploy(&project_dir, &manifest, app_manifest.as_deref(), wasm.as_deref()) {
                    Ok(()) => eprintln!("deployed.\n"),
                    Err(e) => eprintln!("deploy failed: {e:#}\n"),
                }
            }
            Err(errs) => {
                for e in errs {
                    eprintln!("watcher error: {e}");
                }
            }
        }
    }

    Ok(())
}

fn resolve(root: &Path, p: PathBuf) -> PathBuf {
    if p.is_absolute() { p } else { root.join(p) }
}

/// Shell out to `forge build` + `forge deploy` + `forge static
/// upload`. Each step is best-effort: we surface failures with
/// the underlying stderr but don't abort the watcher — the next
/// save attempts the full pipeline again.
fn build_and_deploy(
    project_dir: &Path,
    manifest: &Path,
    app_manifest: Option<&Path>,
    wasm: Option<&Path>,
) -> Result<()> {
    // 1. `forge build` — only when the project actually has Rust
    //    sources. Pure-declarative apps skip this entirely.
    let cargo_toml = project_dir.join("Cargo.toml");
    if cargo_toml.exists() {
        eprintln!("  $ forge build");
        let status = Command::new(forge_bin()?)
            .arg("build")
            .arg("--manifest-dir")
            .arg(project_dir)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("spawning `forge build`")?;
        if !status.success() {
            anyhow::bail!("forge build failed (exit {status})");
        }
    }

    // 2. `forge deploy`. The CLI's deploy command supports both
    //    legacy single-service and v0.10 substrate (--app-manifest)
    //    shapes. We pass --app-manifest when one was resolved.
    let mut deploy = Command::new(forge_bin()?);
    deploy.arg("deploy").arg("--manifest").arg(manifest);
    if let Some(am) = app_manifest {
        deploy.arg("--app-manifest").arg(am);
    }
    if let Some(w) = wasm {
        deploy.arg("--wasm").arg(w);
    }
    eprintln!("  $ forge deploy");
    let status = deploy
        .current_dir(project_dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("spawning `forge deploy`")?;
    if !status.success() {
        anyhow::bail!("forge deploy failed (exit {status})");
    }

    // 3. `forge static upload static/` — when a static dir
    //    exists. Idempotent: same files re-upload with the same
    //    content-hash names and the runtime dedupes.
    let static_dir = project_dir.join("static");
    if static_dir.exists() {
        eprintln!("  $ forge static upload static");
        let status = Command::new(forge_bin()?)
            .arg("static")
            .arg("upload")
            .arg("static")
            .current_dir(project_dir)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("spawning `forge static upload`")?;
        if !status.success() {
            // Static upload failure is non-fatal — the deploy
            // already went through. Warn but keep watching.
            eprintln!("  warning: static upload failed (exit {status}) — keep going");
        }
    }

    Ok(())
}

/// Resolve the running `forge` binary path so child invocations
/// re-enter the same CLI we're in. Falls back to `forge` on PATH
/// when the current_exe() lookup fails (e.g. the binary was
/// piped or symlinked in a way that breaks resolution).
fn forge_bin() -> Result<PathBuf> {
    std::env::current_exe().or_else(|_| Ok(PathBuf::from("forge")))
}
