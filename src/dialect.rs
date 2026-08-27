//! Where a workspace keeps its dialect sources, and which schemas check them.
//!
//! Two questions, one answer each, in one place because `forge check` and
//! `forge wasm-build` must give the same ones. A checker that reads a
//! different set of files from the builder is a checker whose green means
//! nothing.
//!
//! The server half asks the same two questions of a pushed tree rather than
//! of a directory
//! (`forge-control-plane/src/git/reconcile/dialect.rs`); the shapes are
//! deliberately identical.

use std::path::{Path, PathBuf};

/// A dialect source's extension.
pub const DIALECT_SUFFIX: &str = "py";

/// The reference directory a workspace keeps runtime-owned table schemas in.
///
/// Read for CHECKING and never deployed: the workspace compiler reads only
/// `domains/*/schemas`, so a table declared here creates nothing on converge.
/// That is the point — 26 of the 27 tables the dogfood ops read belong to the
/// runtime rather than to the workspace, and rule zero needs their column
/// types to accept a `storage.query` against them.
pub const PLATFORM_SCHEMAS: &str = "platform-schemas";

/// Every dialect source in the workspace, sorted.
///
/// `domains/<d>/services/*.py` and nothing deeper: a `.py` under a
/// subdirectory of `services/` is not an op module, and treating it as one
/// would check — and later emit — a crate the author did not ask for.
pub fn sources(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    for domain in domains(root) {
        found.extend(sources_of(&domain));
    }
    found.sort();
    found
}

/// One domain's dialect sources, sorted — so the emitted crate is a function
/// of the tree and not of a directory read order.
pub fn sources_of(domain: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(domain.join("services")) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == DIALECT_SUFFIX))
        .collect();
    found.sort();
    found
}

/// The workspace's domain directories, sorted.
pub fn domains(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root.join("domains")) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// The directories rule zero loads table schemas from.
///
/// MEASURED, not chosen. With only its own domain's schemas plus the platform
/// set, 24 of the dogfood workspace's 44 sources are refused for reading a
/// table no loaded schema declares — billing reads `tenant_memberships`,
/// `tenants` and `workspaces`, tenancy reads `users`. With every domain's
/// schemas, all 44 are accepted. Domains read across each other, so the scope
/// is the WORKSPACE and not the domain.
pub fn schema_roots(root: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = domains(root)
        .into_iter()
        .map(|d| d.join("schemas"))
        .filter(|d| d.is_dir())
        .collect();
    let platform = root.join(PLATFORM_SCHEMAS);
    if platform.is_dir() {
        roots.push(platform);
    }
    roots
}

/// The cargo package an emitted domain crate is named, and the workspace
/// member path it lives at.
///
/// Both are conventions the build already hard-codes:
/// `stamp_built_wasm_hashes` looks for `forge_domain_<d>.wasm`
/// (`crate::cmd::build`), and the portal's own Rust domains are members at
/// `domains/<d>`. The dialect follows them rather than inventing a third.
pub fn crate_name(domain: &Path) -> String {
    format!("forge-domain-{}", domain_name(domain))
}

/// A domain directory's name.
pub fn domain_name(domain: &Path) -> String {
    domain
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(paths: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for path in paths {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, b"").unwrap();
        }
        dir
    }

    #[test]
    fn sources_are_the_services_directory_and_nothing_deeper() {
        let ws = workspace(&[
            "domains/billing/services/plan_catalog.py",
            "domains/billing/services/plan_detail.py",
            "domains/billing/services/ops.rs",
            "domains/billing/services/nested/helper.py",
            "domains/billing/notes.py",
            "scripts/tool.py",
        ]);
        let found: Vec<String> = sources(ws.path())
            .iter()
            .map(|p| p.strip_prefix(ws.path()).unwrap().display().to_string())
            .collect();
        assert_eq!(
            found,
            vec![
                "domains/billing/services/plan_catalog.py",
                "domains/billing/services/plan_detail.py",
            ]
        );
    }

    /// The measurement in `schema_roots`' doc, as an assertion: every domain's
    /// schemas, plus the platform reference set, because domains read across
    /// each other.
    #[test]
    fn the_schema_scope_is_the_whole_workspace() {
        let ws = workspace(&[
            "domains/billing/schemas/plan_products.table.json",
            "domains/tenancy/schemas/tenants.table.json",
            // A domain with no schemas of its own still reads the others'.
            "domains/release/services/workspace_overview.py",
            "platform-schemas/_tokens.table.json",
        ]);
        let found: Vec<String> = schema_roots(ws.path())
            .iter()
            .map(|p| p.strip_prefix(ws.path()).unwrap().display().to_string())
            .collect();
        assert_eq!(
            found,
            vec![
                "domains/billing/schemas",
                "domains/tenancy/schemas",
                "platform-schemas",
            ]
        );
    }

    #[test]
    fn a_domain_crate_is_named_the_way_the_build_already_looks_for_it() {
        assert_eq!(
            crate_name(Path::new("/w/domains/billing")),
            "forge-domain-billing"
        );
    }
}
