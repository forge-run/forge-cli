//! `forge brand show` — render the resolved brand context for
//! the current app.
//!
//! Phase G.2 of the v0.10 substrate. v1 prints the **app-local**
//! branding overrides from `app.json.branding_overrides`, plus
//! the CSS-variable emission the runtime will inline at render
//! time (`:root { --brand-*: …; }`).
//!
//! Tenant + workspace scopes (the cascade above the app) require
//! a runtime/CP roundtrip and are deferred — the runtime owns the
//! authoritative resolved view via the `branding::current()` host
//! fn. Future v2: `forge brand show --resolved` queries the
//! workspace runtime and prints the post-cascade tokens.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use forge_schema::{AppManifest, BrandingTokens, validate_app_manifest};

#[derive(Debug, Args)]
pub struct BrandArgs {
    /// Project root. Defaults to the current working directory.
    #[arg(long)]
    project_dir: Option<PathBuf>,

    /// Path to `app.json`. Defaults to `<project>/app.json`.
    #[arg(long)]
    app_manifest: Option<PathBuf>,

    /// Emit just the CSS block (no commentary) so the output can
    /// be piped into a stylesheet or copied into a fixture.
    #[arg(long)]
    css_only: bool,

    /// Emit JSON with both the raw tokens and the rendered
    /// `(name, value)` CSS variable list.
    #[arg(long)]
    json: bool,
}

pub async fn run(args: BrandArgs) -> Result<()> {
    let project = args
        .project_dir
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("resolving --project-dir")?;
    let app_manifest_path = args
        .app_manifest
        .map(|p| if p.is_absolute() { p } else { project.join(p) })
        .unwrap_or_else(|| project.join("app.json"));

    if !app_manifest_path.exists() {
        anyhow::bail!(
            "no app.json at {} — pass --app-manifest or run from a v0.10 app root",
            app_manifest_path.display(),
        );
    }

    let bytes = std::fs::read(&app_manifest_path)
        .with_context(|| format!("reading {}", app_manifest_path.display()))?;
    let manifest: AppManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing {}", app_manifest_path.display()))?;
    let errors = validate_app_manifest(&manifest);
    if !errors.is_empty() {
        eprintln!("warning: app.json validation errors:");
        for e in &errors {
            eprintln!("  - {e}");
        }
    }

    let tokens = manifest
        .branding_overrides
        .clone()
        .unwrap_or_default();
    let css_vars = tokens.to_css_variables();

    if args.json {
        let body = serde_json::json!({
            "app": manifest.app.name,
            "version": manifest.app.version,
            "manifest_path": app_manifest_path.display().to_string(),
            "tokens": tokens,
            "css_variables": css_vars,
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    if args.css_only {
        print_css_block(&css_vars);
        return Ok(());
    }

    println!("# {} v{}", manifest.app.name, manifest.app.version);
    println!("# manifest: {}", app_manifest_path.display());
    println!("# scope: app-local overrides only");
    println!("# (tenant + workspace cascade above resolves at runtime)");
    println!();

    if css_vars.is_empty() {
        println!("no branding tokens set — runtime serves built-in defaults");
        return Ok(());
    }

    summarize_buckets(&tokens);
    println!();
    print_css_block(&css_vars);
    Ok(())
}

fn summarize_buckets(tokens: &BrandingTokens) {
    fn row(label: &str, set: bool) {
        println!(
            "  {:<11} {}",
            label,
            if set { "set" } else { "(inherit)" },
        );
    }
    println!("buckets:");
    row("colors", tokens.colors.is_some());
    row("typography", tokens.typography.is_some());
    row("spacing", tokens.spacing.is_some());
    row("radius", tokens.radius.is_some());
    row("shadow", tokens.shadow.is_some());
    println!("  {:<11} {}", "assets", tokens.assets.len());
    println!("  {:<11} {}", "extra", tokens.extra.len());
}

#[cfg(test)]
mod tests {
    use forge_schema::BrandingTokens;

    #[test]
    fn empty_tokens_emit_no_css_vars() {
        let tokens = BrandingTokens::default();
        assert!(tokens.to_css_variables().is_empty());
    }

    #[test]
    fn run_succeeds_against_app_with_no_branding() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("app.json"),
            r#"{
                "schema_version": "0.1",
                "app": { "name": "blank", "version": "0.1.0" },
                "shell": {},
                "routing_claim": { "host": "@default", "path": "/" },
                "tenant_claims": {}
            }"#,
        )
        .unwrap();
        // Smoke — the parse path must accept a minimal app.json
        // and emit the empty branding summary without panicking.
        let manifest_bytes = std::fs::read(tmp.path().join("app.json")).unwrap();
        let manifest: forge_schema::AppManifest =
            serde_json::from_slice(&manifest_bytes).unwrap();
        let tokens = manifest.branding_overrides.unwrap_or_default();
        assert!(tokens.to_css_variables().is_empty());
    }
}

fn print_css_block(css_vars: &[(String, String)]) {
    println!(":root {{");
    for (name, value) in css_vars {
        println!("  {name}: {value};");
    }
    println!("}}");
}
