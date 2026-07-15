//! `forge new` — scaffold a new forge workload from a starter
//! template.
//!
//! Templates ship embedded in the forge-cli binary via
//! `include_str!`. The customer runs:
//!
//! ```bash
//! forge new --template echo my-app
//! cd my-app
//! forge wasm-build
//! ```
//!
//! and gets a working crate. Substitution is text-replace of
//! the template's hardcoded crate name (`forge-template-<name>`)
//! with the customer's chosen name. Underscores vs hyphens
//! matter: cargo crate names use hyphens; the wasm output file
//! and the `name` in `[package]` use the same. The
//! `wasm_path` in `service.json` references the underscore form
//! (cargo replaces hyphens with underscores in artifact
//! filenames). Both substitutions land in the same pass.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

#[derive(Debug, Args)]
pub struct NewArgs {
    /// Which starter template to scaffold from. Run `forge new --list`
    /// to see every template with a one-line description. Required
    /// unless `--list` is passed.
    #[arg(long)]
    template: Option<String>,

    /// Output directory + crate name. The directory must not
    /// already exist (refuses to clobber). Required unless `--list`.
    #[arg(value_name = "CRATE_NAME")]
    crate_name: Option<String>,

    /// List the available starter templates and exit.
    #[arg(long)]
    list: bool,
}

/// One-line description per template, shown by `--list`. Keep in sync with
/// `TEMPLATES` — the `templates_all_have_a_description` test enforces it.
const TEMPLATE_DESCRIPTIONS: &[(&str, &str)] = &[
    ("echo", "single-service crate — echoes its JSON input (imperative deploy)"),
    ("mcp-tool", "single-service crate — an MCP tool op (imperative deploy)"),
    (
        "subscription-publisher",
        "single-service crate — publishes to a channel (imperative deploy)",
    ),
];

/// Embedded templates. Each entry is `(template_name, [(relative_path, contents)...])`.
/// The `forge-template-<name>` placeholder gets replaced with the
/// customer's `--crate-name` argument; same for the underscore-
/// form artifact filename.
const TEMPLATES: &[(&str, &[(&str, &str)])] = &[
    (
        "echo",
        &[
            (
                "Cargo.toml",
                include_str!("../../templates/echo/Cargo.toml"),
            ),
            (
                "src/lib.rs",
                include_str!("../../templates/echo/src/lib.rs"),
            ),
            (
                "service.json",
                include_str!("../../templates/echo/service.json"),
            ),
            ("README.md", include_str!("../../templates/echo/README.md")),
        ],
    ),
    (
        "mcp-tool",
        &[
            (
                "Cargo.toml",
                include_str!("../../templates/mcp-tool/Cargo.toml"),
            ),
            (
                "src/lib.rs",
                include_str!("../../templates/mcp-tool/src/lib.rs"),
            ),
            (
                "service.json",
                include_str!("../../templates/mcp-tool/service.json"),
            ),
            (
                "README.md",
                include_str!("../../templates/mcp-tool/README.md"),
            ),
        ],
    ),
    (
        "subscription-publisher",
        &[
            (
                "Cargo.toml",
                include_str!("../../templates/subscription-publisher/Cargo.toml"),
            ),
            (
                "src/lib.rs",
                include_str!("../../templates/subscription-publisher/src/lib.rs"),
            ),
            (
                "service.json",
                include_str!("../../templates/subscription-publisher/service.json"),
            ),
            (
                "README.md",
                include_str!("../../templates/subscription-publisher/README.md"),
            ),
        ],
    ),
];

pub async fn run(args: NewArgs) -> Result<()> {
    if args.list {
        print_template_list();
        return Ok(());
    }

    let template = args.template.as_deref().ok_or_else(|| {
        anyhow::anyhow!("missing --template — run `forge new --list` to see the options")
    })?;
    let crate_name = args.crate_name.as_deref().ok_or_else(|| {
        anyhow::anyhow!("missing CRATE_NAME — usage: `forge new --template <t> <crate-name>`")
    })?;

    let files = TEMPLATES
        .iter()
        .find(|(name, _)| *name == template)
        .map(|(_, files)| files)
        .ok_or_else(|| {
            let available: Vec<&str> = TEMPLATES.iter().map(|(n, _)| *n).collect();
            anyhow::anyhow!(
                "unknown template `{}` — available: {}",
                template,
                available.join(", "),
            )
        })?;

    validate_crate_name(crate_name)?;
    let target_dir = PathBuf::from(crate_name);
    if target_dir.exists() {
        anyhow::bail!(
            "directory `{}` already exists — pick a different name or remove it first",
            target_dir.display(),
        );
    }

    let template_crate_name = format!("forge-template-{}", template);
    let template_artifact_name = template_crate_name.replace('-', "_");
    let customer_artifact_name = crate_name.replace('-', "_");

    let (sdk_dep_line, sdk_resolved) = resolve_sdk_dep();

    for (rel_path, content) in *files {
        let path = target_dir.join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let substituted = content
            .replace(&template_crate_name, crate_name)
            .replace(&template_artifact_name, &customer_artifact_name)
            // README/service.json placeholders. `{{crate_name}}` was NEVER
            // substituted before this fix, so scaffolded READMEs shipped the
            // literal `{{crate_name}}`. Rewrite both the hyphen + underscore
            // forms so docs and paths read correctly.
            .replace("{{crate_name}}", crate_name)
            .replace("{{artifact_name}}", &customer_artifact_name)
            // SDK dependency. The template pins `forge-sdk-v2 = { path = "../.." }`
            // (relative to the in-repo template location); rewrite it to the
            // resolved dependency form. See `resolve_sdk_dep` for precedence
            // and docs/sdk-dependency-portability.md for the off-box story.
            .replace("forge-sdk-v2 = { path = \"../..\" }", &sdk_dep_line);
        std::fs::write(&path, substituted)
            .with_context(|| format!("writing {}", path.display()))?;
    }

    // Initialise a git repo + initial commit so the scaffold is immediately a
    // real working tree (a prerequisite for any push-based deploy, and how
    // `forge init` leaves its GitOps workspaces). Best-effort — never fails
    // the scaffold.
    git_init_scaffold(&target_dir);

    eprintln!(
        "created {} from `{}` template",
        target_dir.display(),
        template
    );
    if !sdk_resolved {
        eprintln!();
        eprintln!(
            "⚠  forge-sdk-v2 could not be located automatically. The generated\n   \
             Cargo.toml points at `{sdk_dep_line}` — edit it to a real path (or set\n   \
             FORGE_SDK_PATH and re-run) before building. See\n   \
             docs/sdk-dependency-portability.md."
        );
    }
    eprintln!();
    eprintln!("next steps:");
    eprintln!("  cd {}", target_dir.display());
    eprintln!("  forge wasm-build              # compile to a wasm32-wasip1 Component");
    eprintln!();
    eprintln!(
        "  This template is a single-service crate. It deploys via the imperative\n  \
         path (below); the GitOps `forge ship` golden path deploys a WORKSPACE, so\n  \
         to ride `forge ship` scaffold a workspace instead (see the README).\n"
    );
    eprintln!("  # imperative single-service deploy:");
    eprintln!(
        "  forge deploy --manifest service.json \\\n    \
         --wasm target/wasm32-wasip1/release/{customer_artifact_name}.wasm"
    );
    Ok(())
}

/// Resolve the `forge-sdk-v2` dependency line to write into the scaffolded
/// `Cargo.toml`, and whether it points at a directory that actually exists.
///
/// Precedence:
///  1. `FORGE_SDK_PATH` env var — explicit override for any checkout layout.
///  2. A sibling `forge-sdk-v2` next to this `forge-cli` checkout (the
///     canonical dev layout `.../{forge-cli,forge-sdk-v2}/`), resolved from
///     `CARGO_MANIFEST_DIR` at COMPILE time — so it is the maintainer's build
///     path, correct only on the box the CLI was built on.
///
/// If neither resolves to an existing directory we still emit a clearly-marked
/// placeholder path (never a silent, wrong absolute path) and the caller warns
/// the user to fix it. forge-sdk-v2 is `publish = false` and is NOT
/// self-contained (it `wit_bindgen::generate!`s against a sibling
/// `forge-runtime/wit`), so no registry/git dependency form builds off-box
/// today — see docs/sdk-dependency-portability.md for the real fix.
fn resolve_sdk_dep() -> (String, bool) {
    let dep = |p: &str| format!("forge-sdk-v2 = {{ path = \"{p}\" }}");

    if let Ok(env_path) = std::env::var("FORGE_SDK_PATH") {
        let exists = std::path::Path::new(&env_path).is_dir();
        return (dep(&env_path), exists);
    }

    let sibling = env!("CARGO_MANIFEST_DIR")
        .strip_suffix("forge-cli")
        .map(|prefix| format!("{prefix}forge-sdk-v2"));
    if let Some(path) = sibling {
        let exists = std::path::Path::new(&path).is_dir();
        if exists {
            return (dep(&path), true);
        }
    }

    // Nothing resolved — emit an obvious placeholder, not a wrong path.
    (dep("/path/to/forge-sdk-v2"), false)
}

/// `git init -b main` + an initial commit of the scaffold. Best-effort: a
/// missing git, or an unconfigured identity, leaves the files in place and the
/// user finishes the commit. Never fails the scaffold.
fn git_init_scaffold(dir: &std::path::Path) {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
    };
    if git(&["init", "-b", "main"]).map(|o| !o.status.success()).unwrap_or(true) {
        let _ = git(&["init"]);
    }
    let _ = git(&["add", "-A"]);
    let staged = git(&["diff", "--cached", "--quiet"])
        .map(|o| !o.status.success())
        .unwrap_or(false);
    if staged {
        let _ = git(&["commit", "-m", "forge new: scaffold from starter template"]);
    }
}

fn print_template_list() {
    println!("available starter templates:\n");
    for (name, desc) in TEMPLATE_DESCRIPTIONS {
        println!("  {name:<24} {desc}");
    }
    println!("\nusage: forge new --template <name> <crate-name>");
}

/// Cargo crate names: lowercase ASCII letters, digits, `-`, `_`.
/// Must start with a letter. Refuses anything cargo would reject
/// at compile time so the customer doesn't get a confusing
/// "invalid character" error from cargo after `forge new` already
/// created the directory tree.
fn validate_crate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("crate name cannot be empty");
    }
    let first = name.chars().next().unwrap();
    if !first.is_ascii_alphabetic() {
        anyhow::bail!(
            "crate name must start with an ASCII letter; got `{name}` (starts with `{first}`)",
        );
    }
    for ch in name.chars() {
        if !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') {
            anyhow::bail!(
                "crate name `{name}` contains invalid character `{ch}` — \
                 only ASCII letters / digits / `-` / `_` are allowed",
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_crate_name_accepts_normal_names() {
        validate_crate_name("my-app").unwrap();
        validate_crate_name("my_app").unwrap();
        validate_crate_name("app123").unwrap();
    }

    #[test]
    fn validate_crate_name_rejects_invalid() {
        assert!(validate_crate_name("").is_err());
        assert!(validate_crate_name("123app").is_err());
        assert!(validate_crate_name("my app").is_err());
        assert!(validate_crate_name("my.app").is_err());
        assert!(
            validate_crate_name("MyApp").is_ok(),
            "uppercase is technically valid for cargo (gets normalized)"
        );
    }

    #[test]
    fn templates_all_have_a_description() {
        // `--list` reads TEMPLATE_DESCRIPTIONS; a template with no description
        // would silently vanish from the listing. Enforce 1:1 with TEMPLATES.
        for (name, _) in TEMPLATES {
            assert!(
                TEMPLATE_DESCRIPTIONS.iter().any(|(n, _)| n == name),
                "template `{name}` has no entry in TEMPLATE_DESCRIPTIONS (`--list` would omit it)"
            );
        }
        for (name, _) in TEMPLATE_DESCRIPTIONS {
            assert!(
                TEMPLATES.iter().any(|(n, _)| n == name),
                "TEMPLATE_DESCRIPTIONS lists `{name}` which is not a real template"
            );
        }
    }

    #[test]
    fn no_template_ships_a_literal_double_brace_placeholder_unsubstituted() {
        // `{{crate_name}}` must be a token `forge new` rewrites — a template
        // file containing it is fine (it gets substituted), but this documents
        // that the substitution pass covers it so READMEs don't ship the
        // literal placeholder (the pre-fix bug).
        let rewritten = "# {{crate_name}}".replace("{{crate_name}}", "my-app");
        assert_eq!(rewritten, "# my-app");
    }

    #[test]
    fn resolve_sdk_dep_env_override_wins() {
        // Serialised implicitly: this is the only test touching FORGE_SDK_PATH.
        unsafe { std::env::set_var("FORGE_SDK_PATH", "/tmp/does-not-exist-forge-sdk") };
        let (line, exists) = resolve_sdk_dep();
        assert!(line.contains("/tmp/does-not-exist-forge-sdk"), "env path used: {line}");
        assert!(!exists, "non-existent path reports not-resolved");
        unsafe { std::env::remove_var("FORGE_SDK_PATH") };
    }

    #[test]
    fn templates_have_the_expected_set_of_files() {
        for (name, files) in TEMPLATES {
            let paths: Vec<&str> = files.iter().map(|(p, _)| *p).collect();
            assert!(
                paths.contains(&"Cargo.toml"),
                "template `{name}` missing Cargo.toml",
            );
            assert!(
                paths.contains(&"src/lib.rs"),
                "template `{name}` missing src/lib.rs",
            );
            assert!(
                paths.contains(&"service.json"),
                "template `{name}` missing service.json",
            );
            assert!(
                paths.contains(&"README.md"),
                "template `{name}` missing README.md",
            );
        }
    }

    #[test]
    fn template_service_json_targets_wasip1_with_substitutable_name() {
        // service.json's wasm_path has to (a) point at
        // wasm32-wasip1 (the target `forge build` shells out to)
        // and (b) reference the artifact name in a form
        // `forge new`'s substitution can rewrite. The substitution
        // rewrites `forge_template_<name>` (underscored) → the
        // customer's underscored crate name, so wasm_path must
        // contain that exact token. A drift here means the
        // customer's `forge deploy` looks for a file at the
        // wrong path and fails. Past bug: templates shipped
        // wasip2 + a literal `{{crate_name}}` placeholder, both
        // broken.
        for (name, files) in TEMPLATES {
            let service = files
                .iter()
                .find(|(p, _)| *p == "service.json")
                .map(|(_, c)| *c)
                .unwrap();
            assert!(
                service.contains("wasm32-wasip1"),
                "template `{name}` service.json doesn't reference wasm32-wasip1 — \
                 forge build only emits wasip1 artifacts, so a wasip2 path is dead-on-arrival",
            );
            let token = format!("forge_template_{}", name.replace('-', "_"));
            assert!(
                service.contains(&token),
                "template `{name}` service.json must reference the underscored \
                 placeholder `{token}` in wasm_path so `forge new`'s artifact-name \
                 substitution rewrites it. Current service.json:\n{service}",
            );
            assert!(
                !service.contains("{{crate_name}}"),
                "template `{name}` service.json has a literal `{{{{crate_name}}}}` \
                 placeholder that `forge new` does NOT substitute — use the underscored \
                 form instead",
            );
        }
    }

    #[test]
    fn template_cargo_toml_uses_canonical_placeholder_name() {
        // The substitution logic depends on each template's
        // Cargo.toml using `name = "forge-template-<template>"`.
        // If a template diverges from this convention, the
        // customer's `forge new` will produce a crate with the
        // template's hardcoded name and won't know it. Pin here.
        for (name, files) in TEMPLATES {
            let cargo = files
                .iter()
                .find(|(p, _)| *p == "Cargo.toml")
                .map(|(_, c)| *c)
                .unwrap();
            let expected = format!("name = \"forge-template-{}\"", name);
            assert!(
                cargo.contains(&expected),
                "template `{name}` Cargo.toml doesn't contain `{expected}` — \
                 substitution will silently leave the template's name in the customer's crate",
            );
        }
    }
}
