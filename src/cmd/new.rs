//! `forge new` — scaffold a new forge workload from a starter
//! template.
//!
//! Templates ship embedded in the forge-cli binary via
//! `include_str!`. The customer runs:
//!
//! ```bash
//! forge new --template echo my-app
//! cd my-app
//! forge build
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
    /// Which starter template to scaffold from. Currently:
    /// `echo`, `mcp-tool`, `subscription-publisher`.
    #[arg(long)]
    template: String,

    /// Output directory + crate name. The directory must not
    /// already exist (refuses to clobber).
    #[arg(value_name = "CRATE_NAME")]
    crate_name: String,
}

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
            (
                "README.md",
                include_str!("../../templates/echo/README.md"),
            ),
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
    let files = TEMPLATES
        .iter()
        .find(|(name, _)| *name == args.template.as_str())
        .map(|(_, files)| files)
        .ok_or_else(|| {
            let available: Vec<&str> = TEMPLATES.iter().map(|(n, _)| *n).collect();
            anyhow::anyhow!(
                "unknown template `{}` — available: {}",
                args.template,
                available.join(", "),
            )
        })?;

    validate_crate_name(&args.crate_name)?;
    let target_dir = PathBuf::from(&args.crate_name);
    if target_dir.exists() {
        anyhow::bail!(
            "directory `{}` already exists — pick a different name or remove it first",
            target_dir.display(),
        );
    }

    let template_crate_name = format!("forge-template-{}", args.template);
    let template_artifact_name = template_crate_name.replace('-', "_");
    let customer_artifact_name = args.crate_name.replace('-', "_");

    // The template's Cargo.toml has `forge-sdk = { path = "../.." }`,
    // which is correct *inside* the templates dir of the forge-sdk
    // repo but wrong everywhere else. Substitute it with an
    // absolute path to forge-sdk so the customer's freshly-
    // scaffolded crate compiles wherever they put it. v1 hack;
    // when forge-sdk publishes to crates.io, replace this with
    // `forge-sdk = "0.1"` and drop the substitution.
    //
    // We compute the SDK path from forge-cli's own
    // CARGO_MANIFEST_DIR at compile time. Falls back to a
    // sibling-`forge-sdk` directory, which matches the canonical
    // dev layout (`/Users/rory/Documents/{forge-cli,forge-sdk}/`).
    let sdk_path = env!("CARGO_MANIFEST_DIR")
        .strip_suffix("forge-cli")
        .map(|prefix| format!("{}forge-sdk", prefix))
        .unwrap_or_else(|| {
            // Fallback for unusual layouts — dev should adjust
            // manually if forge-cli is renamed/moved.
            "/Users/rory/Documents/forge-sdk".to_string()
        });

    for (rel_path, content) in *files {
        let path = target_dir.join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let substituted = content
            .replace(&template_crate_name, &args.crate_name)
            .replace(&template_artifact_name, &customer_artifact_name)
            .replace(
                "forge-sdk = { path = \"../..\" }",
                &format!("forge-sdk = {{ path = \"{sdk_path}\" }}"),
            );
        std::fs::write(&path, substituted)
            .with_context(|| format!("writing {}", path.display()))?;
    }

    eprintln!(
        "created {} from `{}` template",
        target_dir.display(),
        args.template
    );
    eprintln!();
    eprintln!("next steps:");
    eprintln!("  cd {}", target_dir.display());
    eprintln!("  forge build");
    eprintln!("  forge deploy --manifest service.json --wasm \\");
    eprintln!(
        "    target/wasm32-wasip1/release/{}.wasm",
        customer_artifact_name,
    );
    Ok(())
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
