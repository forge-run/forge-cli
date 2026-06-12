//! `forge pages list` — walk the project's `pages/` tree and print
//! the resolved routing table.
//!
//! Phase G.2 of the v0.10 substrate. Inspection-only: parses
//! `*.page.json` files via `forge_schema::validate_page_manifest`
//! and prints a tabular routing summary. Surfaces:
//! - The route (path + auth tier + capabilities).
//! - The rendering tier (declarative / rust).
//! - Whether the page has a sibling `.page.rs` (Tier-3) or a
//!   template file (Tier-1/2).
//! - Any data bindings the page declares.
//!
//! Built so the agent-author loop has a single command to verify
//! "what does my app currently look like to the runtime" before
//! deploying.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use forge_schema::{PageManifest, validate_page_manifest};

#[derive(Debug, Args)]
pub struct PagesArgs {
    /// Project root. Defaults to the current working directory.
    #[arg(long)]
    project_dir: Option<PathBuf>,

    /// Directory containing `*.page.json` files. Defaults to
    /// `<project>/pages/`. Walked recursively.
    #[arg(long)]
    pages_dir: Option<PathBuf>,

    /// Emit JSON instead of the human table. Each row is one
    /// page record; the array preserves walk order.
    #[arg(long)]
    json: bool,
}

pub async fn run(args: PagesArgs) -> Result<()> {
    let project = args
        .project_dir
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("resolving --project-dir")?;
    let pages_dir = args
        .pages_dir
        .map(|p| if p.is_absolute() { p } else { project.join(p) })
        .unwrap_or_else(|| project.join("pages"));

    if !pages_dir.exists() {
        anyhow::bail!(
            "no pages/ directory at {} — run from a v0.10 app root or pass --pages-dir",
            pages_dir.display(),
        );
    }

    let mut rows = Vec::new();
    walk_pages(&pages_dir, &pages_dir, &mut rows)?;
    rows.sort_by(|a, b| a.path.cmp(&b.path));

    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if rows.is_empty() {
        eprintln!("no *.page.json files under {}", pages_dir.display());
        return Ok(());
    }

    print_table(&rows);
    Ok(())
}

#[derive(Debug, serde::Serialize)]
struct PageRow {
    /// Relative path from the pages/ root (stable identifier).
    path: String,
    /// Routed URL path (e.g. `/admin/cluster`).
    route: String,
    /// `anon` / `auth` / `system`.
    auth: String,
    /// `declarative` or `rust`.
    rendering: String,
    /// Capabilities declared on the page (e.g. `[storage, render]`).
    caps: Vec<String>,
    /// Has `.page.rs` sibling (Tier 3).
    has_rust: bool,
    /// Has `.page.html` sibling.
    has_template: bool,
    /// Has `.page.css` sibling.
    has_css: bool,
    /// Data-binding query names declared in the manifest.
    data_queries: Vec<String>,
}

fn walk_pages(root: &Path, dir: &Path, rows: &mut Vec<PageRow>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk_pages(root, &path, rows)?;
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if !name.ends_with(".page.json") {
            continue;
        }
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let manifest: PageManifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", path.display()))?;
        let errors = validate_page_manifest(&manifest);
        if !errors.is_empty() {
            eprintln!("warning: {} has validation errors:", path.display());
            for e in &errors {
                eprintln!("  - {e}");
            }
        }
        let stem = name.trim_end_matches(".page.json");
        let dir = path.parent().unwrap_or(Path::new("."));
        let has_rust = dir.join(format!("{stem}.page.rs")).exists();
        let has_template = dir.join(format!("{stem}.page.html")).exists();
        let has_css = dir.join(format!("{stem}.page.css")).exists();
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());

        rows.push(PageRow {
            path: rel,
            route: format!("{} {}", manifest.route.method, manifest.route.path),
            auth: format!("{:?}", manifest.auth).to_lowercase(),
            // `PageRendering` describes the response shape
            // (buffered HTML / streaming / fragment), not the
            // authoring tier. Tier-1/Tier-3 is derivable from
            // whether `.page.rs` is present; we surface both
            // shape + tier in adjacent columns. PascalCase Debug
            // output → kebab-case for table readability.
            rendering: pascal_to_kebab(&format!("{:?}", manifest.rendering)),
            caps: manifest
                .caps
                .iter()
                .map(|c| pascal_to_kebab(&format!("{c:?}")))
                .collect(),
            has_rust,
            has_template,
            has_css,
            data_queries: manifest.data.values().map(|b| b.query.clone()).collect(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn pascal_to_kebab_handles_compound() {
        assert_eq!(pascal_to_kebab("BufferedHtml"), "buffered-html");
        assert_eq!(pascal_to_kebab("StreamingHtml"), "streaming-html");
        assert_eq!(pascal_to_kebab("Fragment"), "fragment");
        assert_eq!(pascal_to_kebab("ComponentRegistry"), "component-registry");
        // Idempotent for already-lowercase input.
        assert_eq!(pascal_to_kebab("buffered-html"), "buffered-html");
    }

    #[test]
    fn walk_pages_picks_up_valid_manifest() {
        let tmp = tempdir().unwrap();
        let pages = tmp.path().join("pages");
        fs::create_dir_all(&pages).unwrap();
        fs::write(
            pages.join("test.page.json"),
            r#"{
                "schema_version": "0.1",
                "name": "test_page",
                "route": { "method": "GET", "path": "/test" },
                "auth": "Anonymous",
                "caps": [],
                "rendering": "BufferedHtml",
                "data": {},
                "template": { "kind": "Path", "path": "pages/test.page.html" }
            }"#,
        )
        .unwrap();
        let mut rows = Vec::new();
        walk_pages(&pages, &pages, &mut rows).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.route, "GET /test");
        assert_eq!(r.auth, "anonymous");
        assert_eq!(r.rendering, "buffered-html");
        assert!(!r.has_rust);
        assert!(!r.has_template);
        assert!(!r.has_css);
    }

    #[test]
    fn walk_pages_recurses_subdirectories() {
        let tmp = tempdir().unwrap();
        let pages = tmp.path().join("pages");
        fs::create_dir_all(pages.join("admin")).unwrap();
        fs::write(
            pages.join("admin/users.page.json"),
            r#"{
                "schema_version": "0.1",
                "name": "users",
                "route": { "method": "GET", "path": "/admin/users" },
                "auth": "Admin",
                "caps": [],
                "rendering": "BufferedHtml",
                "data": {},
                "template": { "kind": "Path", "path": "pages/admin/users.page.html" }
            }"#,
        )
        .unwrap();
        let mut rows = Vec::new();
        walk_pages(&pages, &pages, &mut rows).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "admin/users.page.json");
        assert_eq!(rows[0].auth, "admin");
    }

    #[test]
    fn walk_pages_detects_sibling_files() {
        let tmp = tempdir().unwrap();
        let pages = tmp.path().join("pages");
        fs::create_dir_all(&pages).unwrap();
        fs::write(
            pages.join("p.page.json"),
            r#"{
                "schema_version": "0.1",
                "name": "p",
                "route": { "method": "GET", "path": "/p" },
                "auth": "Anonymous",
                "caps": [],
                "rendering": "BufferedHtml",
                "data": {},
                "template": { "kind": "Path", "path": "pages/p.page.html" }
            }"#,
        )
        .unwrap();
        fs::write(pages.join("p.page.html"), "").unwrap();
        fs::write(pages.join("p.page.css"), "").unwrap();
        fs::write(pages.join("p.page.rs"), "").unwrap();
        let mut rows = Vec::new();
        walk_pages(&pages, &pages, &mut rows).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].has_rust);
        assert!(rows[0].has_template);
        assert!(rows[0].has_css);
    }
}

/// Convert PascalCase to lower-kebab-case for table cells.
/// `BufferedHtml` → `buffered-html`; `StreamingHtml` →
/// `streaming-html`; single-word ids pass through unchanged.
fn pascal_to_kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn print_table(rows: &[PageRow]) {
    let w_route = rows.iter().map(|r| r.route.len()).max().unwrap_or(5).max(5);
    let w_path = rows.iter().map(|r| r.path.len()).max().unwrap_or(4).max(4);

    println!(
        "{:<w_route$}  {:<5}  {:<11}  {:<3}  {:<3}  {:<3}  {:<w_path$}  CAPS",
        "ROUTE", "AUTH", "RENDERING", "RS", "HTM", "CSS", "FILE",
    );
    println!(
        "{}  {}  {}  {}  {}  {}  {}  {}",
        "-".repeat(w_route),
        "-".repeat(5),
        "-".repeat(11),
        "--",
        "---",
        "---",
        "-".repeat(w_path),
        "----",
    );
    for r in rows {
        let caps = if r.caps.is_empty() {
            "-".to_string()
        } else {
            r.caps.join(",")
        };
        println!(
            "{:<w_route$}  {:<5}  {:<11}  {:<3}  {:<3}  {:<3}  {:<w_path$}  {}",
            r.route,
            r.auth,
            r.rendering,
            if r.has_rust { "y" } else { "-" },
            if r.has_template { "y" } else { "-" },
            if r.has_css { "y" } else { "-" },
            r.path,
            caps,
        );
    }
    println!();
    println!("{} page(s)", rows.len());
}
