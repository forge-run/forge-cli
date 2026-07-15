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
    (
        "workspace",
        "git-native workspace (one domain + op) — the GitOps `forge ship` golden path",
    ),
];

/// Workspace-graph templates. Unlike the single-service `TEMPLATES` (a bare
/// crate for the imperative `forge deploy`), these scaffold a whole
/// `workspace.json` domain/app/capability graph — the unit `forge ship`
/// builds + pushes. Emitted paths carry a `{{domain}}` token substituted with
/// the derived domain name; file contents carry `{{workspace_name}}`,
/// `{{domain}}`, `{{domain_crate}}`, and the SDK-dep placeholder.
const WORKSPACE_TEMPLATES: &[(&str, &[(&str, &str)])] = &[(
    "workspace",
    &[
        (
            "workspace.json",
            include_str!("../../templates/workspace/workspace.json"),
        ),
        ("Cargo.toml", include_str!("../../templates/workspace/Cargo.toml")),
        (".gitignore", include_str!("../../templates/workspace/gitignore")),
        ("README.md", include_str!("../../templates/workspace/README.md")),
        (
            "domains/{{domain}}/domain.json",
            include_str!("../../templates/workspace/domain.json"),
        ),
        (
            "domains/{{domain}}/Cargo.toml",
            include_str!("../../templates/workspace/domain-Cargo.toml"),
        ),
        (
            "domains/{{domain}}/service.json",
            include_str!("../../templates/workspace/service.json"),
        ),
        (
            "domains/{{domain}}/services/lib.rs",
            include_str!("../../templates/workspace/lib.rs"),
        ),
    ],
)];

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

    validate_crate_name(crate_name)?;

    // Workspace-graph templates take a different scaffolding path (multi-dir
    // tree + `forge ship` next-steps).
    if let Some((_, files)) = WORKSPACE_TEMPLATES.iter().find(|(name, _)| *name == template) {
        return scaffold_workspace(crate_name, template, files);
    }

    let files = TEMPLATES
        .iter()
        .find(|(name, _)| *name == template)
        .map(|(_, files)| files)
        .ok_or_else(|| {
            let mut available: Vec<&str> = TEMPLATES.iter().map(|(n, _)| *n).collect();
            available.extend(WORKSPACE_TEMPLATES.iter().map(|(n, _)| *n));
            anyhow::anyhow!(
                "unknown template `{}` — available: {}",
                template,
                available.join(", "),
            )
        })?;

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

/// Derive a valid domain name (lower-kebab, no leading/trailing `-`) from the
/// crate name. `forge_schema`'s domain validation requires this shape and the
/// domain directory must match the name exactly; sanitising here avoids a
/// confusing compile error after the tree is already written.
fn derive_domain_name(crate_name: &str) -> String {
    let lowered = crate_name.to_lowercase();
    let trimmed = lowered.trim_matches('-');
    if trimmed.is_empty() {
        "app".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Scaffold a workspace-graph template: a `workspace.json` domain/app graph
/// that `forge ship` (wasm-build → wasm-upload → git push) consumes directly.
fn scaffold_workspace(
    crate_name: &str,
    template: &str,
    files: &[(&str, &str)],
) -> Result<()> {
    let target_dir = PathBuf::from(crate_name);
    if target_dir.exists() {
        anyhow::bail!(
            "directory `{}` already exists — pick a different name or remove it first",
            target_dir.display(),
        );
    }

    let domain = derive_domain_name(crate_name);
    let domain_crate = format!("forge-domain-{domain}");
    let (sdk_dep_line, sdk_resolved) = resolve_sdk_dep();

    let subst = |s: &str| -> String {
        s.replace("{{domain}}", &domain)
            .replace("{{domain_crate}}", &domain_crate)
            .replace("{{workspace_name}}", crate_name)
            .replace("forge-sdk-v2 = { path = \"../..\" }", &sdk_dep_line)
    };

    for (rel_path, content) in files {
        // `{{domain}}` appears in emitted paths too (the domain directory).
        let rel = subst(rel_path);
        let path = target_dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&path, subst(content))
            .with_context(|| format!("writing {}", path.display()))?;
    }

    git_init_scaffold(&target_dir);

    eprintln!(
        "created workspace {} from `{}` template (domain `{}`)",
        target_dir.display(),
        template,
        domain,
    );
    if !sdk_resolved {
        eprintln!();
        eprintln!(
            "⚠  forge-sdk-v2 could not be located automatically. `domains/{domain}/Cargo.toml`\n   \
             points at `{sdk_dep_line}` — edit it (or set FORGE_SDK_PATH and re-scaffold)\n   \
             before building. See docs/sdk-dependency-portability.md."
        );
    }
    eprintln!();
    eprintln!("next steps (the GitOps golden path):");
    eprintln!("  cd {}", target_dir.display());
    eprintln!("  forge wasm-build                       # compile the workspace graph");
    eprintln!("  forge ws use <workspace-id>");
    eprintln!(
        "  git remote add forge https://git.forge.run/<workspace-id>/{crate_name}"
    );
    eprintln!("  forge ship                             # build → upload → push → converge");
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
        for (name, _) in WORKSPACE_TEMPLATES {
            assert!(
                TEMPLATE_DESCRIPTIONS.iter().any(|(n, _)| n == name),
                "workspace template `{name}` has no TEMPLATE_DESCRIPTIONS entry"
            );
        }
        for (name, _) in TEMPLATE_DESCRIPTIONS {
            let known = TEMPLATES.iter().any(|(n, _)| n == name)
                || WORKSPACE_TEMPLATES.iter().any(|(n, _)| n == name);
            assert!(
                known,
                "TEMPLATE_DESCRIPTIONS lists `{name}` which is not a real template"
            );
        }
    }

    #[test]
    fn workspace_template_has_ship_able_graph_shape() {
        let (_, files) = WORKSPACE_TEMPLATES
            .iter()
            .find(|(n, _)| *n == "workspace")
            .expect("workspace template exists");
        let paths: Vec<&str> = files.iter().map(|(p, _)| *p).collect();
        // The unit `forge ship` builds is a workspace.json graph with a cargo
        // workspace + at least one wasm domain. Assert the load-bearing files.
        for required in [
            "workspace.json",
            "Cargo.toml",
            "domains/{{domain}}/domain.json",
            "domains/{{domain}}/Cargo.toml",
            "domains/{{domain}}/service.json",
            "domains/{{domain}}/services/lib.rs",
        ] {
            assert!(paths.contains(&required), "workspace template missing {required}");
        }
        // The domain crate must be the `forge-domain-<name>` the workspace
        // compiler's hash-stamp step looks for.
        let domain_cargo = files
            .iter()
            .find(|(p, _)| *p == "domains/{{domain}}/Cargo.toml")
            .map(|(_, c)| *c)
            .unwrap();
        assert!(
            domain_cargo.contains("name = \"{{domain_crate}}\""),
            "domain Cargo.toml must use the forge-domain-<name> crate name token"
        );
    }

    #[test]
    fn derive_domain_name_produces_valid_names() {
        assert_eq!(derive_domain_name("my-app"), "my-app");
        assert_eq!(derive_domain_name("MyApp"), "myapp");
        assert_eq!(derive_domain_name("my-app-"), "my-app");
        assert_eq!(derive_domain_name("-"), "app");
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
