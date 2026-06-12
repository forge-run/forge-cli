//! `forge components list` — walk the project's `components/` tree
//! and print the component registry.
//!
//! Phase G.2 of the v0.10 substrate. Inspection-only: parses
//! `*.component.json` files via
//! `forge_schema::validate_component_manifest` and prints a
//! summary of:
//! - Component name (key the runtime resolves against in
//!   `{% component "X" %}` calls).
//! - Declared props (count + required count).
//! - Declared slots (count).
//! - Behavior triggers attached (count).
//! - Whether the component has a Rust render fn (Tier 3) or is
//!   pure-declarative (Tier 1/2).
//!
//! Tenant + built-in scopes (the cascading shadow chain) require
//! a runtime roundtrip and are out of scope for v1; this command
//! reports the **app-local** scope only. The output makes that
//! explicit in the heading.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use forge_schema::{ComponentManifest, validate_component_manifest};

#[derive(Debug, Args)]
pub struct ComponentsArgs {
    /// Project root. Defaults to the current working directory.
    #[arg(long)]
    project_dir: Option<PathBuf>,

    /// Directory containing `*.component.json` files. Defaults
    /// to `<project>/components/`. Walked recursively.
    #[arg(long)]
    components_dir: Option<PathBuf>,

    /// Emit JSON instead of the human table.
    #[arg(long)]
    json: bool,
}

pub async fn run(args: ComponentsArgs) -> Result<()> {
    let project = args
        .project_dir
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("resolving --project-dir")?;
    let components_dir = args
        .components_dir
        .map(|p| if p.is_absolute() { p } else { project.join(p) })
        .unwrap_or_else(|| project.join("components"));

    if !components_dir.exists() {
        anyhow::bail!(
            "no components/ directory at {} — pass --components-dir or run from a v0.10 app root",
            components_dir.display(),
        );
    }

    let mut rows = Vec::new();
    walk_components(&components_dir, &components_dir, &mut rows)?;
    rows.sort_by(|a, b| a.name.cmp(&b.name));

    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if rows.is_empty() {
        eprintln!(
            "no *.component.json files under {}",
            components_dir.display()
        );
        return Ok(());
    }

    println!("# App-scope components ({} total)", rows.len());
    println!("# Tenant + built-in (forge-ui) scopes shadow these at render time.");
    println!();

    let w_name = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    let w_path = rows.iter().map(|r| r.path.len()).max().unwrap_or(4).max(4);
    println!(
        "{:<w_name$}  PROPS  REQ  SLOTS  BEHV  RS  HTM  CSS  {:<w_path$}",
        "NAME", "FILE",
    );
    println!(
        "{}  -----  ---  -----  ----  --  ---  ---  {}",
        "-".repeat(w_name),
        "-".repeat(w_path),
    );
    for r in &rows {
        println!(
            "{:<w_name$}  {:>5}  {:>3}  {:>5}  {:>4}  {:<2}  {:<3}  {:<3}  {:<w_path$}",
            r.name,
            r.props,
            r.props_required,
            r.slots,
            r.behaviors,
            if r.has_rust { "y" } else { "-" },
            if r.has_template { "y" } else { "-" },
            if r.has_css { "y" } else { "-" },
            r.path,
        );
    }
    Ok(())
}

#[derive(Debug, serde::Serialize)]
struct ComponentRow {
    name: String,
    path: String,
    props: usize,
    props_required: usize,
    slots: usize,
    behaviors: usize,
    has_rust: bool,
    has_template: bool,
    has_css: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn walk_components_parses_props() {
        let tmp = tempdir().unwrap();
        let comps = tmp.path().join("components");
        fs::create_dir_all(&comps).unwrap();
        fs::write(
            comps.join("c.component.json"),
            r#"{
                "schema_version": "0.1",
                "name": "c",
                "props": {
                    "title":    { "type": "string", "required": true },
                    "subtitle": { "type": "string" }
                }
            }"#,
        )
        .unwrap();
        let mut rows = Vec::new();
        walk_components(&comps, &comps, &mut rows).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "c");
        assert_eq!(rows[0].props, 2);
        assert_eq!(rows[0].props_required, 1);
    }

    #[test]
    fn walk_components_detects_html_and_css_siblings() {
        let tmp = tempdir().unwrap();
        let comps = tmp.path().join("components");
        fs::create_dir_all(&comps).unwrap();
        fs::write(
            comps.join("x.component.json"),
            r#"{ "schema_version": "0.1", "name": "x", "props": {} }"#,
        )
        .unwrap();
        fs::write(comps.join("x.component.html"), "").unwrap();
        fs::write(comps.join("x.component.css"), "").unwrap();
        let mut rows = Vec::new();
        walk_components(&comps, &comps, &mut rows).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].has_template);
        assert!(rows[0].has_css);
        assert!(!rows[0].has_rust);
    }
}

fn walk_components(root: &Path, dir: &Path, rows: &mut Vec<ComponentRow>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk_components(root, &path, rows)?;
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if !name.ends_with(".component.json") {
            continue;
        }
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let manifest: ComponentManifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", path.display()))?;
        let errors = validate_component_manifest(&manifest);
        if !errors.is_empty() {
            eprintln!("warning: {} has validation errors:", path.display());
            for e in &errors {
                eprintln!("  - {e}");
            }
        }
        let stem = name.trim_end_matches(".component.json");
        let parent = path.parent().unwrap_or(Path::new("."));
        let has_rust = parent.join(format!("{stem}.component.rs")).exists();
        let has_template = parent.join(format!("{stem}.component.html")).exists();
        let has_css = parent.join(format!("{stem}.component.css")).exists();
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());

        let props_required = manifest.props.props.values().filter(|p| p.required).count();

        rows.push(ComponentRow {
            name: manifest.name.clone(),
            path: rel,
            props: manifest.props.props.len(),
            props_required,
            slots: manifest.slots.len(),
            behaviors: manifest.behaviors.len(),
            has_rust,
            has_template,
            has_css,
        });
    }
    Ok(())
}
