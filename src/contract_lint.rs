//! ADR-0030 / ADR-0031 contract lint — the client-side half of the
//! deploy validator's warn phase.
//!
//! Two ADRs added declarations the substrate defaults for you, silently:
//!
//! - ADR-0031 renamed the service grouping field `namespace` → `domain`
//!   and added `api_version`, the `v{major}` segment of
//!   `/api/domains/{domain}/{service}/v{major}/{op}`. An undeclared
//!   `api_version` keeps the legacy `/api/v1/services/{domain}/{name}/{op}`
//!   route.
//! - ADR-0030 added `auth` per operation. Undeclared is enforced as
//!   `anonymous` — which is a decision most authors did not make on
//!   purpose.
//!
//! The server warns about all three on deploy (they ride back on
//! `DeployServicesResponse.warnings`). The CLI warns about them at the
//! point they are authored — reading `service.json` — so a `forge
//! wasm-build` surfaces them before anything reaches the network. Every
//! finding here is a WARNING: nothing in this module fails a command.

use std::path::Path;

use serde_json::Value;

/// Above this many per-file findings of one kind, collapse to a count.
/// A handful of lines is actionable; forty is noise the author scrolls past.
const SUMMARY_THRESHOLD: usize = 3;

/// Warning lines for one parsed service manifest. `label` is what the
/// author typed (the manifest path), used verbatim in the message.
///
/// Accepts both authored shapes, exactly as the workspace compiler does:
/// the full `{services:[…], wasm_modules:[…]}` deploy manifest and the
/// flat per-domain `{name, domain, operations:[…]}`.
pub fn service_manifest_warnings(label: &str, doc: &Value) -> Vec<String> {
    let services: Vec<&Value> = match doc.get("services").and_then(Value::as_array) {
        Some(list) => list.iter().collect(),
        // Flat per-domain shape — the document IS the service.
        None if doc.get("operations").is_some() => vec![doc],
        None => return Vec::new(),
    };

    let mut lines = Vec::new();

    // The legacy key, in either place it can appear. One line per file:
    // the fix is a global rename, so naming every site adds nothing.
    let legacy_service_key = services.iter().any(|s| s.get("namespace").is_some());
    let legacy_module_key = doc
        .get("wasm_modules")
        .and_then(Value::as_array)
        .is_some_and(|mods| mods.iter().any(|m| m.get("service_namespace").is_some()));
    if legacy_service_key || legacy_module_key {
        lines.push(format!(
            "warning: {label} uses `namespace`; the field is now `domain` (ADR-0031) \
             — accepted for now"
        ));
    }

    let mut missing_api_version: Vec<String> = Vec::new();
    let mut missing_auth: Vec<String> = Vec::new();
    for svc in &services {
        let name = service_label(svc);
        if svc.get("api_version").is_none() {
            missing_api_version.push(name.clone());
        }
        let ops = svc.get("operations").and_then(Value::as_array);
        for op in ops.into_iter().flatten() {
            if op.get("auth").is_none() {
                let op_name = op
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("<unnamed>");
                missing_auth.push(format!("{name}::{op_name}"));
            }
        }
    }

    if missing_api_version.len() > SUMMARY_THRESHOLD {
        lines.push(format!(
            "warning: {} services without `api_version` in {label} — declare \
             `api_version: 1` to opt into /api/domains/{{domain}}/{{service}}/v1/… \
             routes (ADR-0031)",
            missing_api_version.len(),
        ));
    } else {
        for svc in &missing_api_version {
            lines.push(format!(
                "warning: service `{svc}` in {label} declares no `api_version` — declare \
                 `api_version: 1` to opt into /api/domains/{{domain}}/{{service}}/v1/… \
                 routes (ADR-0031)"
            ));
        }
    }

    if missing_auth.len() > SUMMARY_THRESHOLD {
        lines.push(format!(
            "warning: {} ops without `auth` in {label} — declare \
             `auth: anonymous|authenticated|admin`; undeclared is enforced as \
             anonymous (ADR-0030)",
            missing_auth.len(),
        ));
    } else {
        for op in &missing_auth {
            lines.push(format!(
                "warning: operation `{op}` in {label} declares no `auth` — declare \
                 `auth: anonymous|authenticated|admin`; undeclared is enforced as \
                 anonymous (ADR-0030)"
            ));
        }
    }

    lines
}

/// `domain::name` for a service object, falling back to whatever half is
/// present. The legacy `namespace` key is read too — a manifest that
/// still uses it must still get a legible label.
fn service_label(svc: &Value) -> String {
    let domain = svc
        .get("domain")
        .or_else(|| svc.get("namespace"))
        .and_then(Value::as_str);
    let name = svc.get("name").and_then(Value::as_str);
    match (domain, name) {
        (Some(d), Some(n)) => format!("{d}::{n}"),
        (None, Some(n)) => n.to_string(),
        (Some(d), None) => d.to_string(),
        (None, None) => "<unnamed>".to_string(),
    }
}

/// Lint one already-parsed manifest, printing to stderr. Never fails.
pub fn warn_on_service_manifest(path: &Path, doc: &Value) {
    for line in service_manifest_warnings(&path.display().to_string(), doc) {
        eprintln!("{line}");
    }
}

/// Lint every `service.json` in a workspace tree. Best-effort throughout:
/// an unreadable or malformed manifest is skipped silently — the build
/// (or the deploy) is what reports those, with a better error than a lint
/// could.
pub fn warn_on_workspace_contracts(root: &Path) {
    for path in workspace_service_manifests(root) {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(doc) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        warn_on_service_manifest(&path, &doc);
    }
}

/// Every authored `service.json` in a workspace, in the layout the
/// workspace compiler walks: the root manifest, `domains/<d>/`,
/// `apps/<a>/capabilities/<c>/`, and `platform/`.
fn workspace_service_manifests(root: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut consider = |p: std::path::PathBuf| {
        if p.is_file() {
            found.push(p);
        }
    };
    consider(root.join("service.json"));
    consider(root.join("platform").join("service.json"));
    for dir in read_dirs(&root.join("domains")) {
        consider(dir.join("service.json"));
    }
    for app in read_dirs(&root.join("apps")) {
        for cap in read_dirs(&app.join("capabilities")) {
            consider(cap.join("service.json"));
        }
    }
    found.sort();
    found
}

fn read_dirs(parent: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// Render one entry of `DeployServicesResponse.warnings` — the deploy
/// validator's non-fatal findings, same JSON shape as a rejection's
/// `violations[]`: a `kind`, an optional `service` / `operation`, and a
/// `message` or `hint`.
pub fn deploy_warning_line(w: &Value) -> String {
    let kind = w.get("kind").and_then(Value::as_str);
    let detail = w
        .get("message")
        .or_else(|| w.get("hint"))
        .and_then(Value::as_str);
    // Unknown shape (a warning kind newer than this CLI): print the JSON
    // rather than swallow a finding the server thought worth sending.
    let Some(kind) = kind else {
        return format!("warning: {w}");
    };

    let mut line = format!("warning: {kind}");
    if let Some(service) = w.get("service").and_then(Value::as_str) {
        match w.get("operation").and_then(Value::as_str) {
            Some(op) => line.push_str(&format!(" [{service}::{op}]")),
            None => line.push_str(&format!(" [{service}]")),
        }
    }
    if let Some(detail) = detail {
        line.push_str(&format!(" — {detail}"));
    }
    line
}

/// Print the deploy response's non-fatal findings to stderr. Warnings
/// never fail the command and never go to stdout — `--json` output stays
/// machine-readable.
pub fn print_deploy_warnings(warnings: &[Value]) {
    for w in warnings {
        eprintln!("{}", deploy_warning_line(w));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_namespace_key_warns_once_per_file() {
        let doc = json!({
            "services": [
                {"name": "echo", "namespace": "echo", "api_version": 1,
                 "operations": [{"name": "ping", "auth": "authenticated"}]},
                {"name": "calc", "namespace": "tools", "api_version": 1,
                 "operations": [{"name": "add", "auth": "authenticated"}]},
            ],
            "wasm_modules": [{"service_namespace": "echo", "service_name": "echo"}],
        });
        let lines = service_manifest_warnings("service.json", &doc);
        assert_eq!(
            lines.len(),
            1,
            "one legacy-key line for the file: {lines:?}"
        );
        assert!(lines[0].contains("`namespace`"), "{}", lines[0]);
        assert!(lines[0].contains("ADR-0031"), "{}", lines[0]);
    }

    #[test]
    fn fully_declared_manifest_is_silent() {
        let doc = json!({
            "services": [{
                "name": "echo", "domain": "echo", "api_version": 1,
                "operations": [{"name": "ping", "auth": "authenticated"}],
            }],
            "wasm_modules": [{"service_domain": "echo", "service_name": "echo"}],
        });
        assert!(
            service_manifest_warnings("service.json", &doc).is_empty(),
            "a fully declared manifest must produce no warnings"
        );
    }

    #[test]
    fn undeclared_api_version_and_auth_each_warn() {
        let doc = json!({
            "services": [{
                "name": "echo", "domain": "echo",
                "operations": [{"name": "ping"}],
            }],
        });
        let lines = service_manifest_warnings("service.json", &doc);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("`api_version`") && lines[0].contains("echo::echo"));
        assert!(lines[1].contains("`auth`") && lines[1].contains("echo::echo::ping"));
        assert!(lines[1].contains("ADR-0030"));
    }

    #[test]
    fn many_undeclared_ops_collapse_to_a_count() {
        let ops: Vec<Value> = (0..12).map(|i| json!({"name": format!("op{i}")})).collect();
        let doc = json!({
            "services": [{
                "name": "big", "domain": "d", "api_version": 1, "operations": ops,
            }],
        });
        let lines = service_manifest_warnings("service.json", &doc);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("12 ops without `auth` in service.json"),
            "{}",
            lines[0]
        );
    }

    #[test]
    fn flat_per_domain_shape_is_linted_too() {
        // templates/workspace/service.json is `{name, domain, operations}`,
        // with no `services` wrapper — the workspace compiler accepts both,
        // so the lint must too.
        let doc = json!({
            "name": "billing", "domain": "billing",
            "operations": [{"name": "hello"}],
        });
        let lines = service_manifest_warnings("domains/billing/service.json", &doc);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[1].contains("billing::billing::hello"), "{}", lines[1]);
    }

    #[test]
    fn workspace_walk_finds_domain_manifests() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("domains/billing")).unwrap();
        std::fs::create_dir_all(root.join("apps/portal/capabilities/console")).unwrap();
        std::fs::write(root.join("service.json"), "{}").unwrap();
        std::fs::write(root.join("domains/billing/service.json"), "{}").unwrap();
        std::fs::write(
            root.join("apps/portal/capabilities/console/service.json"),
            "{}",
        )
        .unwrap();
        let found = workspace_service_manifests(root);
        assert_eq!(found.len(), 3, "{found:?}");
    }

    #[test]
    fn deploy_warning_lines_carry_kind_subject_and_detail() {
        assert_eq!(
            deploy_warning_line(&json!({
                "kind": "missing_auth_declaration",
                "service": "billing::checkout",
                "operation": "run",
                "hint": "declare `auth: anonymous | authenticated | admin` on the op",
            })),
            "warning: missing_auth_declaration [billing::checkout::run] — declare \
             `auth: anonymous | authenticated | admin` on the op",
        );
        assert_eq!(
            deploy_warning_line(&json!({
                "kind": "missing_api_version",
                "service": "billing::checkout",
                "hint": "declare `api_version: 1`",
            })),
            "warning: missing_api_version [billing::checkout] — declare `api_version: 1`",
        );
        // `message` wins over `hint`; a kind-less object still prints.
        assert_eq!(
            deploy_warning_line(&json!({"kind": "k", "message": "m", "hint": "h"})),
            "warning: k — m",
        );
        assert_eq!(deploy_warning_line(&json!({"x": 1})), "warning: {\"x\":1}");
    }
}
