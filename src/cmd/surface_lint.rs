//! Build-time surface-configuration lint (validations #58 + #61).
//!
//! Motivation — the `app.forge.run → code.forge.run` outage: a commit deleted
//! the `domains` block from `app.json` believing it inert. It is NOT: the
//! runtime's deploy-ingest calls `register_surface_config(app_manifest.domains)`
//! and that **replaces the workspace's surface config wholesale**, per app. So:
//!
//! - If apps DISAGREE on `domains`, the last one ingested wins nondeterministically.
//! - If SOME apps carry `domains` and others don't, the empty one's ingest calls
//!   `register_surface_config(ws, None)` — which WIPES the config for the whole
//!   workspace, defaulting the surface gate open and breaking host routing.
//!
//! This lint runs inside `forge build` (workspace-graph path) and fails the build
//! on any incoherence, turning a silent production routing break into a loud
//! build error. It also flags unrecognized top-level `app.json` keys (#61 —
//! "declared but not consumed"): a key the runtime never reads is dead config,
//! and — the inverse lesson — every key here is annotated as consumed so no one
//! deletes a live one as inert again.

use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// Top-level `app.json` keys the platform actually consumes. Kept in sync with
/// the runtime's manifest parse + deploy ingest. A key NOT in this set is either
/// dead config or a typo — either way the runtime ignores it, so we warn. A key
/// IN this set is load-bearing: `domains` in particular drives the surface gate.
const KNOWN_APP_KEYS: &[&str] = &[
    "schema_version",
    "app",
    "shell",
    "branding_overrides",
    "routing_claim",
    "auth",
    "consumes",
    "uses_capabilities",
    "domains", // ← consumed by the runtime surface gate (register_surface_config). NOT inert.
];

/// One app's parsed manifest, reduced to what the lint needs.
struct AppManifest {
    name: String,
    /// The raw `domains` block (`None` if the key is absent).
    domains: Option<serde_json::Value>,
    /// All top-level keys, for the unconsumed-key check.
    top_level_keys: Vec<String>,
}

/// Entry point. Walk `apps/*/app.json`, validate surface coherence, and return
/// an error describing the first class of violation found. Non-fatal findings
/// (unknown keys) are printed as warnings. A workspace with no `apps/` dir or no
/// `domains` anywhere is fine (single-surface / ungated workspaces are valid).
pub fn lint_surface_config(root: &Path) -> Result<()> {
    let apps_dir = root.join("apps");
    if !apps_dir.is_dir() {
        return Ok(()); // single-crate / non-multi-app workspace — nothing to check.
    }
    let apps = load_app_manifests(&apps_dir)?;
    if apps.is_empty() {
        return Ok(());
    }
    lint_manifests(&apps)
}

/// The workspace's agreed `domains` block (all apps carry an identical one after
/// the lint passes), for the post-converge smoke gate. `None` if the workspace
/// declares no surface config or has no `apps/` dir. Best-effort: parse errors
/// yield `None` rather than failing (the lint already ran at build time).
pub fn agreed_domains(root: &Path) -> Option<serde_json::Value> {
    let apps_dir = root.join("apps");
    let apps = load_app_manifests(&apps_dir).ok()?;
    apps.into_iter().find_map(|a| a.domains)
}

/// Pure over the parsed manifests so it's unit-testable without a filesystem.
fn lint_manifests(apps: &[AppManifest]) -> Result<()> {
    // ── #61: unrecognized top-level keys → warn (dead / typo'd config). ──────
    for app in apps {
        for k in &app.top_level_keys {
            if !KNOWN_APP_KEYS.contains(&k.as_str()) {
                eprintln!(
                    "  ⚠ surface-lint: apps/{}/app.json has unrecognized top-level key `{}` \
                     — the runtime ignores it (dead config or typo).",
                    app.name, k
                );
            }
        }
    }

    // ── #58a: None-wipe hazard. If ANY app declares `domains`, EVERY app must,
    // because a `domains`-less app ingests `register_surface_config(ws, None)`
    // and wipes the shared config. This is the exact 99339aa failure. ─────────
    let with: Vec<&AppManifest> = apps.iter().filter(|a| a.domains.is_some()).collect();
    let without: Vec<&AppManifest> = apps.iter().filter(|a| a.domains.is_none()).collect();
    if !with.is_empty() && !without.is_empty() {
        bail!(
            "surface-lint: inconsistent `domains` across apps — [{}] declare it but [{}] do NOT. \
             Deploy ingest replaces the surface config wholesale PER APP, so the app(s) without \
             `domains` will wipe it (register_surface_config(ws, None)) and break host routing \
             (this is the app→code redirect outage). Every app must carry the SAME `domains` block.",
            with.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "),
            without.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "),
        );
    }

    // No app gates surfaces → nothing more to check.
    if with.is_empty() {
        return Ok(());
    }

    // ── #58b: cross-app agreement. All declared `domains` blocks must be
    // byte-identical (canonicalized), else last-ingest-wins is nondeterministic. ─
    let canon = |v: &serde_json::Value| serde_json::to_string(&canonicalize(v)).unwrap();
    let reference = canon(with[0].domains.as_ref().unwrap());
    for app in &with[1..] {
        if canon(app.domains.as_ref().unwrap()) != reference {
            bail!(
                "surface-lint: apps/{}/app.json `domains` differs from apps/{}/app.json. \
                 All apps must declare an IDENTICAL `domains` block — ingest replaces the \
                 workspace surface config wholesale, so divergent blocks race on deploy.",
                app.name, with[0].name,
            );
        }
    }

    // ── #58c: internal coherence of the agreed block. ────────────────────────
    lint_domain_block(with[0].domains.as_ref().unwrap(), &with[0].name)
}

/// Validate one `domains` block: hosts, allowed_surfaces, and canonical must be
/// mutually consistent, or the surface gate mis-routes / 404s / bounces.
fn lint_domain_block(domains: &serde_json::Value, app: &str) -> Result<()> {
    let hosts = domains
        .get("hosts")
        .and_then(|h| h.as_object())
        .ok_or_else(|| anyhow::anyhow!("surface-lint: apps/{app}/app.json `domains.hosts` missing or not an object"))?;
    let canonical = domains
        .get("canonical")
        .and_then(|c| c.as_object())
        .ok_or_else(|| anyhow::anyhow!("surface-lint: apps/{app}/app.json `domains.canonical` missing or not an object"))?;

    // Collect every surface that is served on some host.
    let mut served: BTreeMap<String, Vec<String>> = BTreeMap::new(); // surface → hosts
    for (host, policy) in hosts {
        let allowed = policy
            .get("allowed_surfaces")
            .and_then(|a| a.as_array())
            .ok_or_else(|| anyhow::anyhow!(
                "surface-lint: apps/{app}/app.json host `{host}` has no `allowed_surfaces` array"
            ))?;
        for s in allowed {
            let s = s.as_str().ok_or_else(|| {
                anyhow::anyhow!("surface-lint: apps/{app}/app.json host `{host}` allowed_surfaces has a non-string entry")
            })?;
            served.entry(s.to_string()).or_default().push(host.clone());
        }
    }

    // Rule 1: every served surface must have a canonical home — else a request
    // for it on a non-serving host has nowhere to redirect (404 / hidden).
    for (surface, hosts_serving) in &served {
        if !canonical.contains_key(surface) {
            bail!(
                "surface-lint: apps/{app}/app.json surface `{surface}` is served (on {}) but has \
                 no `canonical` host — cross-host requests for it can't be routed. Add \
                 `canonical.{surface}`.",
                hosts_serving.join(", "),
            );
        }
    }

    // Rule 2: every canonical mapping must point at a DECLARED host that ACTUALLY
    // serves that surface — else the canonical redirect target is wrong/dead.
    for (surface, host) in canonical {
        let host = host.as_str().ok_or_else(|| {
            anyhow::anyhow!("surface-lint: apps/{app}/app.json canonical.{surface} is not a string")
        })?;
        if !hosts.contains_key(host) {
            bail!(
                "surface-lint: apps/{app}/app.json canonical.{surface} → `{host}`, but `{host}` \
                 is not a declared host in `domains.hosts`. The canonical redirect would target \
                 an unconfigured host.",
            );
        }
        match served.get(surface) {
            Some(serving) if serving.iter().any(|h| h == host) => {}
            _ => bail!(
                "surface-lint: apps/{app}/app.json canonical.{surface} → `{host}`, but `{host}` \
                 does not list `{surface}` in its allowed_surfaces. The canonical host must serve \
                 the surface it's canonical for.",
            ),
        }
    }
    Ok(())
}

/// Recursively sort object keys so two logically-equal `domains` blocks compare
/// equal regardless of key order (JSON object key order is not semantic).
fn canonicalize(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(m) => {
            let sorted: serde_json::Map<String, serde_json::Value> = m
                .iter()
                .map(|(k, val)| (k.clone(), canonicalize(val)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect();
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(a) => {
            serde_json::Value::Array(a.iter().map(canonicalize).collect())
        }
        other => other.clone(),
    }
}

/// Load every `apps/<name>/app.json` under `apps_dir`.
fn load_app_manifests(apps_dir: &Path) -> Result<Vec<AppManifest>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(apps_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let manifest = entry.path().join("app.json");
        if !manifest.exists() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let raw = std::fs::read_to_string(&manifest)
            .map_err(|e| anyhow::anyhow!("reading apps/{name}/app.json: {e}"))?;
        let val: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("parsing apps/{name}/app.json: {e}"))?;
        let top_level_keys = val
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        out.push(AppManifest {
            name,
            domains: val.get("domains").cloned(),
            top_level_keys,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn app(name: &str, domains: Option<serde_json::Value>) -> AppManifest {
        let mut keys = vec!["app".to_string()];
        if domains.is_some() {
            keys.push("domains".to_string());
        }
        AppManifest { name: name.into(), domains, top_level_keys: keys }
    }

    fn good_domains() -> serde_json::Value {
        json!({
            "hosts": {
                "forge.run": {"allowed_surfaces": ["marketing", "docs"]},
                "app.forge.run": {"allowed_surfaces": ["app", "auth", "docs"]},
                "code.forge.run": {"allowed_surfaces": ["code"]},
                "auth.forge.run": {"allowed_surfaces": ["auth"]}
            },
            "canonical": {
                "marketing": "forge.run", "docs": "forge.run",
                "app": "app.forge.run", "auth": "auth.forge.run", "code": "code.forge.run"
            }
        })
    }

    #[test]
    fn all_apps_agree_and_block_is_coherent_passes() {
        let apps = vec![
            app("portal", Some(good_domains())),
            app("code", Some(good_domains())),
        ];
        assert!(lint_manifests(&apps).is_ok());
    }

    #[test]
    fn none_wipe_hazard_is_rejected() {
        // The 99339aa failure: one app drops `domains`, wiping the shared config.
        let apps = vec![
            app("portal", Some(good_domains())),
            app("code", None),
        ];
        let err = lint_manifests(&apps).unwrap_err().to_string();
        assert!(err.contains("wipe") || err.contains("inconsistent"), "got: {err}");
    }

    #[test]
    fn divergent_blocks_are_rejected() {
        let mut other = good_domains();
        other["canonical"]["app"] = json!("code.forge.run"); // app canonical wrong host
        let apps = vec![
            app("portal", Some(good_domains())),
            app("code", Some(other)),
        ];
        assert!(lint_manifests(&apps).is_err());
    }

    #[test]
    fn key_order_does_not_count_as_divergence() {
        let reordered = json!({
            "canonical": {
                "code": "code.forge.run", "auth": "auth.forge.run", "app": "app.forge.run",
                "docs": "forge.run", "marketing": "forge.run"
            },
            "hosts": {
                "auth.forge.run": {"allowed_surfaces": ["auth"]},
                "code.forge.run": {"allowed_surfaces": ["code"]},
                "app.forge.run": {"allowed_surfaces": ["app", "auth", "docs"]},
                "forge.run": {"allowed_surfaces": ["marketing", "docs"]}
            }
        });
        let apps = vec![
            app("portal", Some(good_domains())),
            app("code", Some(reordered)),
        ];
        assert!(lint_manifests(&apps).is_ok(), "reordered-but-equal must pass");
    }

    #[test]
    fn served_surface_without_canonical_is_rejected() {
        let mut d = good_domains();
        d["hosts"]["app.forge.run"]["allowed_surfaces"] = json!(["app", "auth", "docs", "beta"]);
        let apps = vec![app("portal", Some(d))];
        let err = lint_manifests(&apps).unwrap_err().to_string();
        assert!(err.contains("beta") && err.contains("canonical"), "got: {err}");
    }

    #[test]
    fn canonical_pointing_at_undeclared_host_is_rejected() {
        let mut d = good_domains();
        d["canonical"]["app"] = json!("nope.forge.run");
        let apps = vec![app("portal", Some(d))];
        assert!(lint_manifests(&apps).is_err());
    }

    #[test]
    fn canonical_host_not_serving_the_surface_is_rejected() {
        // canonical says code→forge.run, but forge.run doesn't serve `code`.
        let mut d = good_domains();
        d["canonical"]["code"] = json!("forge.run");
        let apps = vec![app("portal", Some(d))];
        assert!(lint_manifests(&apps).is_err());
    }

    #[test]
    fn no_domains_anywhere_is_fine() {
        let apps = vec![app("portal", None), app("code", None)];
        assert!(lint_manifests(&apps).is_ok());
    }
}
