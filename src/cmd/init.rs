//! `forge init` — scaffold a new GitOps-native Forge workspace repository.
//!
//! Unlike `forge new` (which scaffolds a single WASM service from a starter
//! template), `forge init` lays out the WHOLE-workspace git repo the GitOps
//! converge consumes: the declarative `app.json` + the per-kind config
//! directories the deploy projects into managed registry tables. The result is
//! a git repo whose `main` IS the workspace's desired state — push it to
//! `git.forge.run/<workspace>/<repo>` and the control plane converges it.
//!
//! Layout (spec §3.2):
//!   app.json            — the app manifest (identity, routing, capability blocks)
//!   service.json        — service/wasm manifest (empty until you add a service)
//!   pages/              — *.page.{json,html,css}
//!   components/         — *.component.{json,html,css}
//!   schemas/            — *.json table schemas (applied additively on converge)
//!   data/               — <table>.json config-data (projected into managed tables)
//!   triggers/           — declarative write-hooks / scheduled jobs (app.json blocks)
//!   workflows/          — declarative workflow definitions
//!   messaging/          — declarative channels
//!   .gitignore          — keeps build/artifact noise out of the desired state
//!   README.md           — the push-to-deploy contract
//!
//! Secrets and data never live here (symbolic `${secret:…}` refs only) — they
//! are the two exclusions the GitOps model keeps out of git.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Debug, clap::Args)]
pub struct InitArgs {
    /// Directory to scaffold into. Defaults to the current directory. A
    /// non-empty target is refused unless `--force`.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// App name written into `app.json` (`app.name`). Defaults to the target
    /// directory's file name.
    #[arg(long)]
    name: Option<String>,

    /// Scaffold into a non-empty directory (only writes files that don't yet
    /// exist; never overwrites). Off by default so an accidental `init` over a
    /// real project is refused.
    #[arg(long)]
    force: bool,

    /// Skip `git init` + the initial commit (just write the files).
    #[arg(long)]
    no_git: bool,
}

/// One scaffolded file: (relative path, contents). Directories with no concrete
/// starter file get a `.gitkeep` so the layout is committed + greppable.
fn scaffold_files(app_name: &str) -> Vec<(&'static str, String)> {
    let app_json = serde_json::to_string_pretty(&serde_json::json!({
        "schema_version": "0.1",
        "app": {
            "name": app_name,
            "version": "0.1.0",
            "description": format!("{app_name} — a git-native Forge workspace")
        },
        "shell": {},
        "routing_claim": { "host": "@default", "path": "/" },
        // Capability blocks start empty; the per-kind dirs document where their
        // sources live. Declared entries here reconcile into managed registry
        // tables on every converge (full-replace; console/api rows survive).
        "workflows": [],
        "write_hooks": [],
        "webhooks": [],
        "api_key_scopes": [],
        "channels": []
    }))
    .expect("static json");

    let readme = format!(
        "# {app_name}\n\n\
         A git-native Forge workspace. **`main` is the desired state** — push it \
         and the control plane converges your workspace to match.\n\n\
         ## Deploy\n\n\
         ```\n\
         forge ws use <workspace-id>\n\
         git remote add forge https://git.forge.run/<workspace-id>/{app_name}\n\
         git push forge main\n\
         ```\n\n\
         A push to `main` deploys by default: schema applies additively, \
         config-data + capabilities project into their managed tables, and the \
         app bundle ingests — all-or-nothing.\n\n\
         ## Layout\n\n\
         | Path | What |\n\
         |------|------|\n\
         | `app.json` | app identity, routing, capability blocks (workflows/hooks/channels) |\n\
         | `service.json` | service + wasm manifest |\n\
         | `schemas/` | table schemas (applied additively; destructive deltas refused) |\n\
         | `data/` | `<table>.json` config-data projected into managed tables |\n\
         | `pages/`, `components/` | declarative UI |\n\n\
         ## Never in git\n\n\
         Secrets and data. Reference secrets symbolically — `${{secret:my_key}}` \
         — never paste a value. They resolve at runtime.\n"
    );

    let gitignore = "# Build + artifact noise — never part of the desired state.\n\
                     target/\n\
                     *.wasm\n\
                     forge.manifest.json\n\
                     .DS_Store\n"
        .to_string();

    let dir_note = |kind: &str| format!("# Drop {kind} here. See README.md.\n");

    vec![
        ("app.json", app_json),
        (
            "service.json",
            serde_json::to_string_pretty(&serde_json::json!({
                "services": [],
                "wasm_modules": []
            }))
            .expect("static json"),
        ),
        ("README.md", readme),
        (".gitignore", gitignore),
        ("schemas/.gitkeep", dir_note("table schema JSON (*.json)")),
        ("data/.gitkeep", dir_note("config-data (<table>.json)")),
        ("pages/.gitkeep", dir_note("pages (*.page.json/html/css)")),
        ("components/.gitkeep", dir_note("components (*.component.*)")),
    ]
}

fn dir_is_nonempty(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

pub async fn run(args: InitArgs) -> Result<()> {
    let target = &args.path;
    std::fs::create_dir_all(target)
        .with_context(|| format!("create target dir {}", target.display()))?;

    if dir_is_nonempty(target) && !args.force {
        anyhow::bail!(
            "{} is not empty — refusing to scaffold over an existing project \
             (pass --force to write only the missing files)",
            target.display()
        );
    }

    let app_name = args
        .name
        .clone()
        .or_else(|| {
            target
                .canonicalize()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        })
        .unwrap_or_else(|| "app".to_string());

    let mut written = 0usize;
    let mut skipped = 0usize;
    for (rel, contents) in scaffold_files(&app_name) {
        let dest = target.join(rel);
        if dest.exists() {
            // --force only fills gaps; never clobbers authored files.
            skipped += 1;
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        std::fs::write(&dest, contents)
            .with_context(|| format!("write {}", dest.display()))?;
        written += 1;
    }

    if !args.no_git {
        init_git(target)?;
    }

    println!(
        "scaffolded git-native workspace `{app_name}` in {} ({written} file(s) written, {skipped} kept)",
        target.display()
    );
    println!("next: `forge ws use <id>` → `git remote add forge https://git.forge.run/<id>/{app_name}` → `git push forge main`");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_app_json_is_a_valid_app_manifest() {
        let files = scaffold_files("my-app");
        let app = files
            .iter()
            .find(|(p, _)| *p == "app.json")
            .map(|(_, c)| c)
            .expect("app.json scaffolded");
        // The scaffold must deserialize as a real AppManifest AND pass the same
        // validation `forge deploy` runs, so an `init`'d repo is push-ready.
        let manifest: forge_schema::AppManifest =
            serde_json::from_str(app).expect("scaffold app.json parses as AppManifest");
        assert_eq!(manifest.app.name, "my-app");
        let errors = forge_schema::validate_app_manifest(&manifest);
        assert!(errors.is_empty(), "scaffold app.json must validate: {errors:?}");
    }

    #[test]
    fn scaffold_lays_out_the_gitops_config_dirs() {
        let files = scaffold_files("x");
        let paths: Vec<&str> = files.iter().map(|(p, _)| *p).collect();
        for required in [
            "app.json",
            "service.json",
            ".gitignore",
            "schemas/.gitkeep",
            "data/.gitkeep",
        ] {
            assert!(paths.contains(&required), "missing scaffold file {required}");
        }
    }
}

/// `git init` (idempotent) + an initial commit of the scaffold, so the repo's
/// `main` is immediately a valid desired state ready to push. Best-effort on
/// the commit (a repo with no configured identity still gets `git init` + the
/// files; the user commits themselves). Never fails the scaffold.
fn init_git(target: &Path) -> Result<()> {
    let git = |args: &[&str]| -> std::io::Result<std::process::Output> {
        std::process::Command::new("git")
            .args(args)
            .current_dir(target)
            .output()
    };
    // Already a repo? Leave it (idempotent).
    let is_repo = git(&["rev-parse", "--is-inside-work-tree"])
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !is_repo {
        // -b main: the protected default branch the converge tracks.
        if git(&["init", "-b", "main"]).map(|o| !o.status.success()).unwrap_or(true) {
            // Older git without -b: fall back to plain init.
            let _ = git(&["init"]);
        }
    }
    let _ = git(&["add", "-A"]);
    // Only commit if something is staged AND an identity exists; otherwise the
    // user finishes the commit. Don't hard-fail the scaffold on commit issues.
    let has_staged = git(&["diff", "--cached", "--quiet"])
        .map(|o| !o.status.success()) // non-zero exit == there ARE staged changes
        .unwrap_or(false);
    if has_staged {
        let out = git(&["commit", "-m", "forge init: git-native workspace scaffold"]);
        match out {
            Ok(o) if o.status.success() => {}
            _ => println!(
                "(scaffold staged; finish with `git commit` — set user.name/user.email if git asks)"
            ),
        }
    }
    Ok(())
}
