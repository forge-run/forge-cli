//! `forge deploy` — upload a service manifest + WASM module(s) to
//! the workspace.
//!
//! Reads:
//! - `--manifest path/to/service.json` — a `DeployServicesRequest`
//!   shape with the `wasm_modules` array's `wasm_bytes` field
//!   omitted (CLI fills it in by reading the local `.wasm`).
//! - `--wasm path/to/module.wasm` — the compiled module the manifest
//!   references. Repeat the flag once per module if the manifest
//!   ships multiple services. The CLI matches by service
//!   `(namespace, name)`.
//!
//! Sends a `DeployServicesRequest` JSON body to
//! `/api/v1/manage/wasm/deploy`. The runtime side validates the
//! customer's bearer + role, re-encodes the request as bincode for
//! storage's `deploy_services` (which is bincode-only), and decodes
//! the response back to JSON for this CLI to read.
//!
//! v1 simplification: `--manifest` carries the entire
//! `DeployServicesRequest` shape verbatim, with one `wasm_modules`
//! entry per service. The CLI populates each entry's `wasm_bytes`
//! by reading the corresponding `.wasm` file from disk before
//! sending. This keeps the CLI's manifest format identical to the
//! storage-side wire shape, so customers can also POST directly via
//! curl if they want to.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::{ForgeClient, ForgeError};
// GitOps A5 — pure deploy-manifest assembly + the locked manifest-hash
// algorithm now live in the shared `forge-manifest` crate so forge-git
// derives a byte-identical manifest from the same code. The impure half
// (fs reads, wasm build/wrap, validation) stays here and feeds these.
use forge_manifest::{
    compute_manifest_hash, logical_unit_bytes, manifest_artifact, manifest_artifact_with_content,
    manifest_artifact_with_explicit_content, Lock,
};

#[derive(Debug, Args)]
pub struct DeployArgs {
    /// Path to the service manifest JSON. Mirrors the storage-side
    /// `DeployServicesRequest` shape; the CLI fills in the `wasm_bytes`
    /// fields by reading the local `.wasm` files supplied via
    /// `--wasm`.
    #[arg(long)]
    manifest: PathBuf,

    /// Path to a compiled `.wasm` module. Repeat once per service
    /// listed in the manifest. The CLI matches each module to a
    /// service by (namespace, name) — pass them in any order.
    #[arg(long)]
    wasm: Vec<PathBuf>,

    /// Phase E.4 — path to `app.json` for the v0.10 substrate.
    /// When set, the deploy uploads a Phase-D-style app bundle
    /// alongside the legacy service manifest. Validation: the
    /// app.json is parsed via `forge_schema::validate_app_manifest`
    /// pre-upload; any error aborts the deploy.
    #[arg(long)]
    app_manifest: Option<PathBuf>,

    /// Phase E.4 — directory containing `*.page.{json,html,css,rs}`
    /// files. Defaults to `pages/` relative to the
    /// `--app-manifest` directory. Each page is validated via
    /// `forge_schema::validate_page_manifest`.
    #[arg(long)]
    pages_dir: Option<PathBuf>,

    /// Phase E.4 — directory containing `*.component.{json,html,css,rs}`
    /// files. Defaults to `components/` relative to the
    /// `--app-manifest` directory. Each component is validated
    /// via `forge_schema::validate_component_manifest`.
    #[arg(long)]
    components_dir: Option<PathBuf>,

    /// GitOps schema converge — directory of declarative table-schema JSON
    /// (`*.json`, the same shape `forge schema apply` sends). Each file becomes
    /// an inline `kind:"schema"` manifest row the runtime's converge applies
    /// ADDITIVELY before the deploy (new table / add column/index/RLS / widen
    /// type); a destructive delta is refused. Defaults to `schemas/` relative to
    /// the `--app-manifest` directory (or the `--manifest` directory when no app
    /// manifest is set). A defaulted dir that's absent is skipped; an explicit
    /// `--schema-dir` that's missing is an error.
    #[arg(long)]
    schema_dir: Option<PathBuf>,

    /// GitOps config-data projection — directory of declarative config-data JSON
    /// (`data/<table>.json`, each `{"table","key"?,"rows":[…]}`). Each file
    /// becomes an inline `kind:"config_data"` manifest row; the runtime's
    /// converge PROJECTS the full rowset into the (managed) table after the
    /// schema applies — git is authoritative (insert/update/delete to match).
    /// A `key` names the operator-stable match column (storage mints `id`); omit
    /// it for full-replace. Defaults to `data/` relative to the `--app-manifest`
    /// directory (or the `--manifest` directory). A defaulted dir that's absent
    /// is skipped; an explicit `--data-dir` that's missing is an error.
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Print the response as JSON instead of human-friendly text.
    #[arg(long)]
    json: bool,

    /// Override the runtime's app-identity admission gate to REPLACE the
    /// workspace's current app with a different one (a different `app.name`).
    /// Off by default: the runtime rejects a mismatched-app deploy with a 409
    /// because it's almost always a mis-targeted deploy. Set this ONLY for an
    /// intentional app replacement.
    #[arg(long)]
    allow_app_replace: bool,

    /// Send wasm bytes INLINE in the deploy payload instead of the default
    /// GitOps reference mode (upload each module once by content hash, then
    /// deploy a reference). Use this against an older runtime that doesn't
    /// resolve references, or a node without the artifact store configured.
    #[arg(long)]
    inline_wasm: bool,

    /// Force a full re-converge, bypassing the no-op. Omits `manifest_hash`
    /// from the payload so the runtime's opt-in no-op never fires — the
    /// deploy always runs end-to-end (registry bump, warm-module eviction,
    /// deploy row), even when the artifact set is byte-identical to what's
    /// already live. Use it to redeploy identical artifacts on purpose.
    #[arg(long)]
    force: bool,

    /// Clear an incident rollback-pin and deploy anyway. A workspace pinned by
    /// a rollback (W6) REFUSES a normal deploy with 409 WORKSPACE_PINNED so a
    /// routine push can't silently clobber the rollback; pass this only when you
    /// intend to supersede the pin. The clean alternative is
    /// `POST /api/v1/manage/reconcile/promote` (promote the rolled-back state).
    #[arg(long)]
    force_unpin: bool,

    /// GitOps authoring — write the computed manifest array (the
    /// `[{path, content_hash, size, kind}, …]` rows this deploy content-
    /// addresses) to PATH as pretty-printed JSON. This is the file a developer
    /// commits as `forge.manifest.json`; on `git push`, forge-git reads it from
    /// the commit tree and PUTs it to the workspace's desired-state, which the
    /// reconcile loop converges. Emitted whether or not the deploy also applies,
    /// so it pairs with `--no-apply` for a stage-only step.
    #[arg(long, value_name = "PATH")]
    emit_manifest: Option<PathBuf>,

    /// GitOps authoring — write `forge.lock` (the build-output identities this
    /// deploy pins) to PATH as pretty-printed JSON:
    /// `{version, wasm_modules:{"<ns>::<name>":"<blake3>"}, app_bundle_hash}`.
    /// Sourced from the already-computed manifest rows (the `wasm` +
    /// `app_bundle` kinds), so it's exactly the subset forge-git can't rebuild
    /// without a build. Like `--emit-manifest`, emitted whether or not the
    /// deploy applies — pair with `--no-apply` for a stage-only step.
    #[arg(long, value_name = "PATH")]
    emit_lock: Option<PathBuf>,

    /// GitOps staging — upload the artifacts to the content store (so the blobs
    /// the manifest references are present) but SKIP the final apply, leaving
    /// desired + live state untouched. Pair with `--emit-manifest` to stage a
    /// deploy that a `git push` (or a later plain `forge deploy`) applies. With
    /// `--inline-wasm` there are no separate uploads, so this just skips apply.
    #[arg(long)]
    no_apply: bool,
}

/// Manifest shape on disk. `wasm_bytes` is OMITTED in the file —
/// the CLI populates it by reading the corresponding `.wasm`.
#[derive(Debug, Deserialize, Serialize)]
struct DeployManifest {
    services: Value,
    /// Customer writes this with `service_namespace` + `service_name` +
    /// `wasm_path` (relative to the manifest); CLI replaces `wasm_path`
    /// with `wasm_bytes` before sending.
    wasm_modules: Vec<WasmModuleEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
struct WasmModuleEntry {
    service_namespace: String,
    service_name: String,
    /// Path on disk, relative to the manifest's directory. Used to
    /// resolve `--wasm` flags by name when only one --wasm is given,
    /// or fallback when --wasm doesn't list this service explicitly.
    #[serde(default)]
    wasm_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeployedService {
    namespace: String,
    name: String,
    service_id: u32,
    wasm_hash_hex: String,
    action: String,
}

#[derive(Debug, Deserialize)]
struct DeployResponse {
    /// W4 — true when the runtime short-circuited a byte-identical
    /// redeploy (incoming manifest_hash == live registry's). A no-op
    /// response carries no `services` / `registry_version`, hence the
    /// `#[serde(default)]` on those below.
    #[serde(default)]
    no_op: bool,
    /// Desired-state identity echoed back (present on both a converged
    /// deploy and a no-op).
    #[serde(default)]
    manifest_hash: Option<String>,
    #[serde(default)]
    services: Vec<DeployedService>,
    #[serde(default)]
    registry_version: u64,
    /// Phase E.5 — present only when the deploy carried an
    /// `app_bundle` section (i.e., the customer passed
    /// `--app-manifest`). Mirrors forge-runtime's
    /// `AppBundleSummary` shape.
    #[serde(default)]
    app_bundle: Option<DeployAppBundleSummary>,
}

#[derive(Debug, Deserialize)]
struct DeployAppBundleSummary {
    app_name: String,
    pages: usize,
    components: usize,
}

/// Best-effort git provenance for the deploy source directory. Shells
/// out to `git` (no extra dep); returns (branch, head_sha,
/// commits_ahead_of_default). All None when it isn't a git repo or git
/// is unavailable — the deploy still proceeds, the row just records null.
fn git_context(dir: &std::path::Path) -> (Option<String>, Option<String>, Option<i64>) {
    fn git(dir: &std::path::Path, args: &[&str]) -> Option<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    }
    let branch = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let head_sha = git(dir, &["rev-parse", "HEAD"]);
    // Commits on HEAD not on the default branch (0 when on main/master).
    let ahead = git(dir, &["rev-list", "--count", "main..HEAD"])
        .or_else(|| git(dir, &["rev-list", "--count", "master..HEAD"]))
        .and_then(|s| s.parse::<i64>().ok());
    (branch, head_sha, ahead)
}

/// GitOps schema converge — collect each `schemas/*.json` (the declarative DDL
/// `forge schema apply` sends: `{name, archetype, columns, …}`) as an inline
/// `kind:"schema"` manifest row. The runtime's converge applies these ADDITIVELY
/// before the deploy; a destructive delta is refused. Content rides inline (the
/// files are small JSON), like `service.json`, so NO artifact upload is needed —
/// the rows land in both `--emit-manifest` output and the deploy's `manifest`.
///
/// Dir resolution mirrors `pages_dir`/`components_dir`: an explicit
/// `--schema-dir`, else `schemas/` next to `--app-manifest` (or `--manifest`
/// when no app manifest is set). A defaulted dir that's absent is simply skipped
/// (a deploy may carry no schema); an explicit dir that's missing is an error.
/// Each file is parsed here so a malformed schema fails fast in the CLI — the
/// runtime trusts the CLI's pre-validation.
fn collect_schema_artifacts(
    args: &DeployArgs,
    manifest_dir: &std::path::Path,
) -> Result<Vec<serde_json::Value>> {
    let (dir, explicit) = match args.schema_dir.as_ref() {
        Some(d) => (d.clone(), true),
        None => {
            let base = args
                .app_manifest
                .as_ref()
                .and_then(|p| p.parent())
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| manifest_dir.to_path_buf());
            (base.join("schemas"), false)
        }
    };
    if !dir.is_dir() {
        if explicit {
            anyhow::bail!("--schema-dir {} is not a directory", dir.display());
        }
        return Ok(Vec::new());
    }

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading schema dir {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    // Stable order so the manifest row-set (and its hash) is reproducible.
    files.sort();

    let mut out = Vec::with_capacity(files.len());
    for path in files {
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading schema {}", path.display()))?;
        let parsed: serde_json::Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing schema {}", path.display()))?;
        let has_name = parsed
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if !has_name {
            anyhow::bail!(
                "schema {} is missing a non-empty string `name` (the table name)",
                path.display()
            );
        }
        let file_name = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("schema.json");
        out.push(manifest_artifact_with_content(
            format!("schemas/{file_name}"),
            "schema",
            &bytes,
        ));
    }
    Ok(out)
}

/// GitOps config-data projection — collect each `data/<table>.json` (the
/// declarative `{"table","key"?,"rows":[…]}` rowset) as an inline
/// `kind:"config_data"` manifest row. The runtime's converge PROJECTS each into
/// its MANAGED table AFTER the schema applies — git authoritative, full-rowset
/// (insert/update/delete to match). Content rides inline (small JSON), like
/// `schema`, so NO artifact upload is needed.
///
/// Dir resolution mirrors `collect_schema_artifacts`: an explicit `--data-dir`,
/// else `data/` next to `--app-manifest` (or `--manifest` when no app manifest).
/// A defaulted dir that's absent is skipped; an explicit dir that's missing is
/// an error. Each file is parsed here so a malformed artifact fails fast in the
/// CLI — the runtime trusts the CLI's pre-validation.
fn collect_config_data_artifacts(
    args: &DeployArgs,
    manifest_dir: &std::path::Path,
) -> Result<Vec<serde_json::Value>> {
    let (dir, explicit) = match args.data_dir.as_ref() {
        Some(d) => (d.clone(), true),
        None => {
            let base = args
                .app_manifest
                .as_ref()
                .and_then(|p| p.parent())
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| manifest_dir.to_path_buf());
            (base.join("data"), false)
        }
    };
    if !dir.is_dir() {
        if explicit {
            anyhow::bail!("--data-dir {} is not a directory", dir.display());
        }
        return Ok(Vec::new());
    }

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading data dir {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    // Stable order so the manifest row-set (and its hash) is reproducible.
    files.sort();

    let mut out = Vec::with_capacity(files.len());
    for path in files {
        let bytes =
            std::fs::read(&path).with_context(|| format!("reading config-data {}", path.display()))?;
        let parsed: serde_json::Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing config-data {}", path.display()))?;
        let has_table = parsed
            .get("table")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if !has_table {
            anyhow::bail!(
                "config-data {} is missing a non-empty string `table`",
                path.display()
            );
        }
        if !parsed.get("rows").map(|v| v.is_array()).unwrap_or(false) {
            anyhow::bail!(
                "config-data {} is missing a `rows` array",
                path.display()
            );
        }
        let file_name = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("data.json");
        out.push(manifest_artifact_with_content(
            format!("data/{file_name}"),
            "config_data",
            &bytes,
        ));
    }
    Ok(out)
}

pub async fn run(args: DeployArgs, client: &ForgeClient) -> Result<()> {
    let manifest_bytes = std::fs::read(&args.manifest)
        .with_context(|| format!("reading manifest {}", args.manifest.display()))?;
    let manifest: DeployManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parsing manifest {}", args.manifest.display()))?;

    // Build a lookup from `(namespace, name)` → wasm bytes by
    // resolving every entry. Sources, in priority order:
    //   1. A `--wasm` whose filename stem matches `<service_name>.wasm`.
    //   2. The manifest entry's `wasm_path` (resolved relative to
    //      the manifest's directory).
    let manifest_dir = args
        .manifest
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let wasm_by_name: HashMap<String, PathBuf> = args
        .wasm
        .iter()
        .filter_map(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| (s.to_string(), p.clone()))
        })
        .collect();

    let mut wasm_modules = Vec::with_capacity(manifest.wasm_modules.len());
    // Content-addressed manifest rows for the wasm modules, collected
    // as each module's final (Component-wrapped) bytes are resolved.
    let mut wasm_artifacts: Vec<serde_json::Value> =
        Vec::with_capacity(manifest.wasm_modules.len());
    // W3 — (content_hash, bytes, content_type) per artifact to upload by hash
    // BEFORE the deploy (reference mode). Holds the wasm modules (empty in
    // `--inline-wasm` mode — those bytes ride inline in the payload) AND, when
    // an app bundle is present, the single content-addressed `app_bundle` blob
    // appended after `build_app_bundle` below.
    let mut artifact_uploads: Vec<(String, Vec<u8>, &'static str)> = Vec::new();
    for entry in &manifest.wasm_modules {
        let resolved = wasm_by_name
            .get(&entry.service_name)
            .cloned()
            .or_else(|| entry.wasm_path.as_ref().map(|rel| manifest_dir.join(rel)))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no wasm file resolved for service {}::{} — pass --wasm path/to/{}.wasm \
                     or set wasm_path in the manifest",
                    entry.service_namespace,
                    entry.service_name,
                    entry.service_name,
                )
            })?;
        let bytes = std::fs::read(&resolved)
            .with_context(|| format!("reading wasm {}", resolved.display()))?;
        // Phase F follow-up — auto-wrap core modules as Components.
        // Previously only `forge build` did this; if a customer ran
        // a raw `cargo build` then `forge deploy`, the runtime would
        // 500 with "wasm artifact is a core module" because forge-
        // runtime's W3 cutover rejects non-Component artifacts.
        // Auto-wrapping makes that failure mode impossible.
        let bytes = if crate::cmd::build::is_component(&bytes) {
            bytes
        } else {
            eprintln!(
                "wrapping {} as Component (was a core module — forge-runtime W3 requires Component Model)",
                resolved.display()
            );
            crate::cmd::build::wrap_as_component(&bytes)
                .with_context(|| format!("wit-component wrap of {}", resolved.display()))?
        };
        // Hash the final (post-wrap) bytes — the exact content the
        // runtime stores AND the content address the artifact upload uses.
        let content_hash = blake3::hash(&bytes).to_hex().to_string();
        wasm_artifacts.push(manifest_artifact(
            format!("wasm:{}::{}", entry.service_namespace, entry.service_name),
            "wasm",
            &bytes,
        ));
        // Reference mode (default): the module entry carries only its
        // identity; the runtime resolves the bytes from the artifact store
        // via the `wasm:<ns>::<name>` manifest row's content_hash. Inline
        // mode keeps the bytes in the payload for older runtimes.
        let mut module = serde_json::json!({
            "service_namespace": entry.service_namespace,
            "service_name": entry.service_name,
        });
        if args.inline_wasm {
            if let Some(obj) = module.as_object_mut() {
                obj.insert("wasm_bytes".into(), serde_json::json!(bytes));
            }
        } else {
            artifact_uploads.push((content_hash, bytes, "application/wasm"));
        }
        wasm_modules.push(module);
    }

    // Phase E.4 — substrate app bundle (optional). When
    // `--app-manifest` is set, validate the app.json + every
    // page.json + component.json before upload. Aborts on any
    // validation error so a bad bundle never reaches the
    // runtime's ingest path. The second tuple element is the
    // content-addressed manifest rows for the bundle's artifacts
    // (app.json + pages + components + static assets).
    let (app_bundle, mut manifest_artifacts) = build_app_bundle(&args).await?;

    // GitOps app-bundle converge — stage the renderable bundle as ONE
    // content-addressed `app_bundle` blob (only when `--app-manifest` produced
    // a bundle). The runtime's converge (`reconstruct_from_manifest_rows` →
    // `fetch_app_bundle_blob`) rebuilds the renderable half of the app from
    // this single blob: it finds the manifest row `{kind:"app_bundle",
    // content_hash:H}` and fetches the bytes `H` from the content store (the
    // `/api/v1/manage/artifacts/{hash}` PUT target it tries first). Without
    // this, a `--no-apply` stage + `git push` converge FAILS CLOSED — the
    // runtime refuses to deploy the service-only half of a manifest that
    // declares an app (the page/component bodies + head_extras live ONLY in
    // this blob). Mirrors the wasm-staging pattern: hash the EXACT bytes we
    // PUT, reference them by that hash in the manifest, stage through the same
    // HEAD-dedup + PUT path. The per-row `app_manifest`/`page`/`component`
    // entries below stay (they feed the deploy-diff surface); THIS row is what
    // the converge resolves.
    if !app_bundle.is_null() {
        // Byte-exact: hash and upload the SAME serialization. `to_vec` is the
        // compact, no-trailing-newline form — identical to how the runtime
        // itself serializes the bundle for its redeploy-capture path
        // (`serde_json::to_vec(&app_bundle_value)`), so the row's
        // `content_hash` equals blake3 of the uploaded bytes and the converge's
        // content-address resolution hits.
        let bundle_bytes = serde_json::to_vec(&app_bundle)
            .context("serialize app_bundle for content-addressed staging")?;
        let bundle_hash = blake3::hash(&bundle_bytes).to_hex().to_string();
        manifest_artifacts.push(serde_json::json!({
            "path": "app_bundle",
            "content_hash": bundle_hash,
            "size": bundle_bytes.len() as i64,
            "kind": "app_bundle",
        }));
        // Stage through the same HEAD-dedup + PUT path the wasm modules use so
        // the blob is present on a normal deploy AND on `--no-apply`. Uploaded
        // even under `--inline-wasm` (that flag concerns only wasm payload
        // mode; the GitOps converge always resolves the bundle from the store).
        artifact_uploads.push((bundle_hash, bundle_bytes, "application/json"));
    }

    // Manifest-as-unit-of-deploy — fold the wasm + service-manifest
    // artifacts into the bundle's artifact set. The runtime persists the
    // whole set to `_deployment_manifests` keyed on the deploy id, so the
    // deploy-history surface can diff two deploys by their (path,
    // content_hash) row-sets (file / logical-unit / semantic diff +
    // no-op detection). Path scheme: `wasm:<ns>::<name>` for modules,
    // `service.json` for the manifest — stable keys the diff joins on.
    manifest_artifacts.extend(wasm_artifacts);
    manifest_artifacts.push(manifest_artifact_with_content(
        "service.json",
        "service_manifest",
        &manifest_bytes,
    ));
    // GitOps schema converge — fold in each `schemas/*.json` as an inline
    // `kind:"schema"` row (see `collect_schema_artifacts`). The runtime's
    // converge applies these additively BEFORE the deploy; destructive deltas
    // are refused. They carry `content_hash`, so a schema edit changes the
    // manifest hash → the reconcile loop converges → the schema applies.
    manifest_artifacts.extend(collect_schema_artifacts(&args, &manifest_dir)?);
    // GitOps config-data projection — fold in each `data/<table>.json` as an
    // inline `kind:"config_data"` row (see `collect_config_data_artifacts`).
    // The runtime's converge projects each into its managed table AFTER the
    // schema applies. Inline content carries a `content_hash`, so a data edit
    // changes the manifest hash → reconcile converges → the projection runs.
    manifest_artifacts.extend(collect_config_data_artifacts(&args, &manifest_dir)?);
    // Deterministic order so the manifest row-set is stable across runs
    // (the diff joins on `path`, but a stable order keeps the stored
    // rows + any human-facing dump reproducible).
    manifest_artifacts.sort_by(|a, b| {
        a.get("path")
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .cmp(b.get("path").and_then(|p| p.as_str()).unwrap_or(""))
    });

    // GitOps authoring — emit the computed manifest array (the content-
    // addressed `[{path, content_hash, size, kind}, …]` rows, the exact set
    // sent in the deploy's `manifest` field) so it can be committed as
    // `forge.manifest.json`. Written regardless of whether the deploy also
    // applies; combined with `--no-apply` this is the GitOps stage step — the
    // file `git push` later hands to the reconcile loop.
    if let Some(path) = args.emit_manifest.as_ref() {
        let json = serde_json::to_string_pretty(&manifest_artifacts)
            .context("serialize manifest for --emit-manifest")?;
        std::fs::write(path, json)
            .with_context(|| format!("writing manifest to {}", path.display()))?;
        eprintln!(
            "wrote manifest ({} artifact(s)) to {}",
            manifest_artifacts.len(),
            path.display(),
        );
    }

    // GitOps authoring — emit `forge.lock`: the build-output identities (wasm
    // modules + app bundle) projected from the SAME manifest rows above, via
    // the shared `forge-manifest` crate so forge-git derives an identical lock
    // from an identical manifest. Written regardless of `--no-apply`, mirroring
    // `--emit-manifest`, so a stage-only step can commit both files together.
    if let Some(path) = args.emit_lock.as_ref() {
        let lock = Lock::from_manifest_rows(&manifest_artifacts);
        let json = serde_json::to_string_pretty(&lock).context("serialize lock for --emit-lock")?;
        std::fs::write(path, json)
            .with_context(|| format!("writing lock to {}", path.display()))?;
        eprintln!(
            "wrote lock ({} wasm module(s){}) to {}",
            lock.wasm_modules.len(),
            if lock.app_bundle_hash.is_some() {
                " + app bundle"
            } else {
                ""
            },
            path.display(),
        );
    }

    // Git provenance for the deploy record (branch / sha / commits-ahead).
    // The runtime pulls these off the body before the strict decode.
    let (git_branch, git_sha, git_ahead) = git_context(&manifest_dir);

    // Git provenance — the runtime strips these from the body before its
    // strict DeployServicesRequest decode. branch/head_sha + the manifest
    // are accepted by forge-runtime ≥ v0.32.44; commits_ahead_main ships
    // in the same runtime, so SEND_COMMITS_AHEAD flips true in lockstep.
    // (An unmodelled key — even null — would fail an older runtime's
    // decode, which is why it was gated.)
    // W3 — content-addressed desired-state identity, computed CLIENT-SIDE
    // reproducing forge-runtime's `compute_manifest_hash` byte-for-byte
    // (locked cross-repo contract; the golden-vector test pins it). Sent in
    // `DeployServicesRequest.manifest_hash`. The runtime recomputes the same
    // value from the `manifest` array and stamps it on the live registry;
    // sending it makes the contract explicit + verifiable on both sides.
    let manifest_hash = compute_manifest_hash(&manifest_artifacts);

    const SEND_COMMITS_AHEAD: bool = true;
    let mut payload = serde_json::json!({
        "services": manifest.services,
        "wasm_modules": wasm_modules,
        // Phase E.4 — extended bundle shape. forge-runtime's
        // manage_routes deploy ingest (E.5) reads this section
        // when present; legacy deploys (no app.json) omit it
        // and the runtime skips the new ingest path.
        "app_bundle": app_bundle,
        "branch": git_branch,
        "head_sha": git_sha,
        // Manifest-as-unit-of-deploy — content-addressed artifact set.
        // forge-runtime ≥ v0.32.44 strips this before the strict decode
        // and writes it to `_deployment_manifests` on a successful
        // deploy. The strict decode rejects unknown keys, so this CLI
        // requires that runtime (same release as commits_ahead_main).
        "manifest": manifest_artifacts,
    });
    // W4 desired-state identity. OMITTED under `--force` so the runtime's
    // opt-in no-op never fires — the deploy then always converges (MEDIUM-2b).
    // The artifact set is still sent in `manifest` (used for the deploy
    // history + reference resolution); only the explicit hash field is dropped.
    if !args.force && let Some(obj) = payload.as_object_mut() {
        obj.insert("manifest_hash".into(), serde_json::json!(manifest_hash));
    }
    if SEND_COMMITS_AHEAD && let Some(obj) = payload.as_object_mut() {
        obj.insert("commits_ahead_main".into(), serde_json::json!(git_ahead));
    }
    if args.allow_app_replace
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert("allow_app_identity_change".into(), serde_json::json!(true));
    }
    // HIGH-2 — explicit override to clear a rollback-pin (the runtime refuses a
    // normal deploy while pinned). Only sent when set, so a routine deploy
    // against a pinned workspace fails loudly rather than silently unpinning.
    if args.force_unpin
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert("force_unpin".into(), serde_json::json!(true));
    }

    // W3 — artifact upload stage. For each wasm module, HEAD the
    // content-addressed artifact route; on 404 PUT the raw bytes. An
    // unchanged binary across deploys HEADs 200 and transfers ZERO bytes
    // (server- + client-side dedup). Runs BEFORE the deploy so the
    // referenced bytes are guaranteed present when the runtime resolves
    // them — the upload-before-bump invariant `forge ship` mechanizes.
    // Skipped entirely in `--inline-wasm` mode (bytes ride in the payload).
    for (hash, bytes, content_type) in &artifact_uploads {
        let path = format!("/api/v1/manage/artifacts/{hash}");
        let status = client.head_status(&path).await.map_err(map_err)?;
        match status {
            StatusCode::OK => {
                eprintln!("artifact {} already stored (dedup)", &hash[..16.min(hash.len())]);
            }
            StatusCode::NOT_FOUND => {
                client
                    .put_bytes(&path, bytes, content_type)
                    .await
                    .map_err(map_err)?;
                eprintln!(
                    "uploaded artifact {} ({} bytes)",
                    &hash[..16.min(hash.len())],
                    bytes.len()
                );
            }
            StatusCode::SERVICE_UNAVAILABLE => {
                anyhow::bail!(
                    "the artifact store is not available on this node (HEAD {path} → 503). \
                     Re-run with --inline-wasm to send wasm bytes inline."
                );
            }
            other => {
                anyhow::bail!("unexpected status {other} from artifact HEAD {path}");
            }
        }
    }

    // GitOps staging — `--no-apply` uploaded the artifacts above so the content
    // store holds the blobs the manifest references, but stops short of the
    // apply call: desired + live state are untouched. The change goes live when
    // forge-git PUTs the committed manifest on `git push`, or on a later plain
    // `forge deploy`.
    if args.no_apply {
        eprintln!(
            "staged {} artifact(s) to the content store; skipped apply (--no-apply). \
             Commit the manifest and `git push` (or run a plain `forge deploy`) to apply.",
            artifact_uploads.len(),
        );
        return Ok(());
    }

    let resp: DeployResponse = client
        .post_json("/api/v1/manage/wasm/deploy", &payload)
        .await
        .map_err(map_err)?;

    // W4 — a manifest-hash no-op: the live deploy already matches this
    // desired state, so the runtime made no change (registry version
    // unchanged). Report it honestly and stop.
    if resp.no_op {
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "no_op": true,
                    "manifest_hash": resp.manifest_hash,
                }))?,
            );
        } else {
            eprintln!(
                "deploy: no-op — manifest unchanged{}. Registry version not bumped.",
                resp.manifest_hash
                    .as_deref()
                    .map(|h| format!(" ({})", &h[..16.min(h.len())]))
                    .unwrap_or_default(),
            );
        }
        return Ok(());
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "registry_version": resp.registry_version,
                "services": resp.services
                    .iter()
                    .map(|s| serde_json::json!({
                        "namespace": s.namespace,
                        "name": s.name,
                        "service_id": s.service_id,
                        "wasm_hash_hex": s.wasm_hash_hex,
                        "action": s.action,
                    }))
                    .collect::<Vec<_>>(),
            }))?,
        );
    } else {
        eprintln!("registry version: {}", resp.registry_version);
        eprintln!("deployed {} service(s):", resp.services.len());
        for s in &resp.services {
            eprintln!(
                "  {}::{}  (id={}, sha256={}, {})",
                s.namespace,
                s.name,
                s.service_id,
                &s.wasm_hash_hex[..16],
                s.action,
            );
        }
        if let Some(bundle) = resp.app_bundle.as_ref() {
            eprintln!(
                "deployed app `{}` with {} page(s), {} component(s)",
                bundle.app_name, bundle.pages, bundle.components,
            );
        }
    }
    Ok(())
}

fn map_err(e: ForgeError) -> anyhow::Error {
    match e {
        ForgeError::Http { status, body: _ } if status.as_u16() == 401 => {
            anyhow::anyhow!("401 Unauthorized — your token isn't valid. Run `forge login` first.",)
        }
        ForgeError::Http { status, body: _ } if status.as_u16() == 403 => {
            anyhow::anyhow!("403 Forbidden — deploy requires role=admin on the calling user.",)
        }
        ForgeError::Http { status, body } => anyhow::anyhow!("{status}: {body}"),
        other => anyhow::anyhow!(other),
    }
}

// ─── Phase E.4 — app-bundle assembly + validation ───────────────

/// Build the optional `app_bundle` payload section. Returns
/// `Value::Null` when `--app-manifest` is unset (legacy deploys).
/// Aborts the deploy when any validation fails — the runtime's
/// ingest path trusts the CLI for substrate validation, so a
/// bad bundle here would mean a runtime hard-fail mid-ingest.
async fn build_app_bundle(
    args: &DeployArgs,
) -> Result<(serde_json::Value, Vec<serde_json::Value>)> {
    let Some(app_path) = args.app_manifest.as_ref() else {
        return Ok((serde_json::Value::Null, Vec::new()));
    };
    let app_dir = app_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    // Content-addressed manifest rows for every bundle artifact —
    // app.json + each page (json+html+css as one logical unit) + each
    // component + every static asset. Returned alongside the bundle so
    // the runtime can persist them to `_deployment_manifests`.
    let mut artifacts: Vec<serde_json::Value> = Vec::new();

    // ── app.json ───────────────────────────────────────────────
    let app_bytes = std::fs::read(app_path)
        .with_context(|| format!("reading app manifest {}", app_path.display()))?;
    artifacts.push(manifest_artifact_with_content(
        "app.json",
        "app_manifest",
        &app_bytes,
    ));
    let app_manifest: forge_schema::AppManifest = serde_json::from_slice(&app_bytes)
        .with_context(|| format!("parsing app manifest {}", app_path.display()))?;
    let errors = forge_schema::validate_app_manifest(&app_manifest);
    if !errors.is_empty() {
        anyhow::bail!(
            "app manifest validation failed at {}:\n{}",
            app_path.display(),
            errors
                .iter()
                .map(|e| format!("  - {e}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    // ── pages/ ─────────────────────────────────────────────────
    let pages_dir = args
        .pages_dir
        .clone()
        .unwrap_or_else(|| app_dir.join("pages"));
    let pages = collect_page_jsons(&pages_dir)?;

    // ── components/ ────────────────────────────────────────────
    let components_dir = args
        .components_dir
        .clone()
        .unwrap_or_else(|| app_dir.join("components"));
    let components = collect_component_jsons(&components_dir)?;

    // ── Cross-validate `{{ component("name", ...) }}` invocations
    // against the app + (eventually) tenant + built-in scope. The
    // CLI fails fast when a page or component references an
    // unknown name so a bad bundle never reaches the runtime's
    // ingest path.
    cross_validate_component_invocations(&pages, &components)?;

    // ── Read forge-web/build's asset-manifest.json (Phase F gap #2)
    // so the runtime can inject the right `<link>` / `<script>`
    // tags into every page render. Without this every v2 page
    // renders unstyled. Missing manifest is benign — older apps
    // that don't ship one fall back to whatever shell config
    // app.json provides directly.
    let head_extras = build_head_extras_from_asset_manifest(&app_dir)?;

    // ── Phase E.4 (gap-close) — cross-check tenant_claims against
    // forge-cp's validated-domain catalog if operator credentials
    // are available. Without them (customer-mode CLI), we skip
    // the check and trust the runtime to catch unregistered
    // claims downstream.
    if let Err(err) = cross_validate_tenant_claims(&app_manifest).await {
        anyhow::bail!("tenant_claims validation failed: {err}");
    }

    // Manifest rows for the bundle's logical units. A "page" is three
    // sibling files (json + html + css) deployed as one unit, so its
    // content hash covers all three — an edit to any one re-hashes the
    // page. Same for components. Static assets hash per-file (the
    // build-hashed CSS already encodes its content in the filename, but
    // hashing the bytes is the canonical, uniform form).
    for page in &pages {
        // Hash the full logical unit (json+html+css) so any edit re-hashes
        // and shows in the deploy diff; persist only the small page.json
        // manifest as `content` for the services/pages navigator's route +
        // auth + op-binding introspection.
        let unit = logical_unit_bytes(&page.manifest, &page.template_body, &page.css_body)?;
        let page_json =
            serde_json::to_string(&page.manifest).context("serialize page manifest for content")?;
        artifacts.push(manifest_artifact_with_explicit_content(
            format!("pages/{}.page", page.name),
            "page",
            &unit,
            &page_json,
        ));
    }
    for component in &components {
        artifacts.push(manifest_artifact(
            format!("components/{}.component", component.name),
            "component",
            &logical_unit_bytes(
                &component.manifest,
                &component.template_body,
                &component.css_body,
            )?,
        ));
    }
    collect_static_artifacts(&app_dir, &mut artifacts)?;

    let bundle = serde_json::json!({
        "app_manifest": app_manifest,
        "pages": pages,
        "components": components,
        // Phase F gap #2 — pre-built head_extras string the runtime
        // injects into ShellConfig.head_extras on every page render.
        // Carries `<link rel="stylesheet">` + vendor / app JS tags
        // based on the build-time hashed bundle hrefs.
        "head_extras": head_extras,
    });
    Ok((bundle, artifacts))
}

/// Hash every file under `static/` into the manifest. Static assets
/// (hashed CSS, favicon, vendor JS, images) are part of the deployed
/// surface — a CSS change that re-hashes the bundle should show up in
/// the deploy diff even when no page/component/wasm changed. Walks
/// recursively; the path key is the slash-joined path relative to
/// `app_dir` (e.g. `static/app.ab12cd.css`). Missing dir is benign.
fn collect_static_artifacts(
    app_dir: &std::path::Path,
    artifacts: &mut Vec<serde_json::Value>,
) -> Result<()> {
    let static_dir = app_dir.join("static");
    if !static_dir.exists() {
        return Ok(());
    }
    for entry in walkdir::WalkDir::new(&static_dir).follow_links(false) {
        let entry = entry.with_context(|| format!("walking {}", static_dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel = path.strip_prefix(app_dir).unwrap_or(path);
        let key = rel
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join("/");
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        artifacts.push(manifest_artifact(key, "static_asset", &bytes));
    }
    Ok(())
}

/// Phase F gap #2 — build a head_extras string from the project's
/// `static/asset-manifest.json` (emitted by forge-web/build). The
/// manifest carries hashed hrefs the runtime needs to inject into
/// every page render. Returns an empty string when the manifest
/// is absent (legacy apps without the substrate build pipeline).
fn build_head_extras_from_asset_manifest(app_dir: &std::path::Path) -> Result<String> {
    let manifest_path = app_dir.join("static").join("asset-manifest.json");
    if !manifest_path.exists() {
        return Ok(String::new());
    }
    let body = std::fs::read(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let map: std::collections::BTreeMap<String, String> = serde_json::from_slice(&body)
        .with_context(|| format!("parse {}", manifest_path.display()))?;
    let mut out = String::new();

    // Customer-authored <head> extras. When the app ships a
    // `static/head-extras.html` file, prepend its contents to the
    // runtime-emitted head_extras. Apps use this for preconnect
    // hints, third-party font stylesheets, meta description /
    // theme-color / robots / OpenGraph / Twitter card, canonical
    // URLs — anything that doesn't fit the built-in CSS / favicon /
    // vendor-script pattern below.
    //
    // Ordered first so customer tags ship in source order and the
    // runtime-managed `<link rel="stylesheet">` lands last (lets the
    // customer's preconnect hints race the bundle fetch).
    let head_extras_html = app_dir.join("static").join("head-extras.html");
    if head_extras_html.exists() {
        let extra = std::fs::read_to_string(&head_extras_html)
            .with_context(|| format!("read {}", head_extras_html.display()))?;
        let stripped = strip_html_comments(&extra);
        out.push_str(stripped.trim());
    }

    if let Some(href) = map.get("css_href") {
        out.push_str(&format!(
            "<link rel=\"stylesheet\" href=\"{}\">",
            escape_html_attr(href),
        ));
    }
    // Favicon — emit when the app has a static/favicon.svg checked
    // in. Not driven by the asset-manifest (forge-web/build doesn't
    // hash arbitrary static files), just a "if the file exists,
    // link it" convenience so customer apps don't have to remember
    // to write the link tag themselves.
    if app_dir.join("static").join("favicon.svg").exists() {
        out.push_str(r#"<link rel="icon" type="image/svg+xml" href="/static/favicon.svg">"#);
    }
    if let Some(href) = map.get("vendor_htmx_href") {
        out.push_str(&format!(
            "<script src=\"{}\" defer></script>",
            escape_html_attr(href),
        ));
    }
    if let Some(href) = map.get("js_href") {
        out.push_str(&format!(
            "<script src=\"{}\" defer></script>",
            escape_html_attr(href),
        ));
    }
    Ok(out)
}

/// Strip HTML comments (`<!-- ... -->`) from a head-extras snippet.
/// Used by [`build_head_extras_from_asset_manifest`] so an authoring
/// file can carry doc comments without the bundle wearing them.
/// Naive scanner; safe for the head-extras use case (no `<script>`
/// bodies, no `<![CDATA[`).
fn strip_html_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start + 4..].find("-->") {
            Some(end) => {
                rest = &rest[start + 4 + end + 3..];
            }
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// HTML-attribute escape. Hashed asset hrefs come from
/// forge-web/build's own writer (no user input) — escape anyway
/// as defense-in-depth so a malicious manifest can't break out of
/// the `href="…"` attribute context.
fn escape_html_attr(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '"' => "&quot;".chars().collect::<Vec<_>>(),
            '&' => "&amp;".chars().collect(),
            '<' => "&lt;".chars().collect(),
            '>' => "&gt;".chars().collect(),
            other => vec![other],
        })
        .collect()
}

/// Phase E.4 (gap-close) — walk every page/component template body
/// for `component("<name>", ...)` invocations and assert each
/// referenced name resolves against the bundle's components set
/// or a built-in cascade qualifier (`@forge/...`, `@tenant/...`).
///
/// Limitations:
/// - We don't have access to the resolved built-in + tenant
///   component catalog at deploy time, so we accept any
///   qualified name and only fail on **unqualified** names that
///   don't exist in the app's `components/` directory. That
///   catches the common "typo'd component name" footgun while
///   leaving qualified references to the runtime's cascade
///   resolver.
/// - Regex-based parser. Templates with literal
///   `{{ "component(" }}` strings would trip a false-positive,
///   but minijinja's `{{ … }}` braces wrap the call so a literal
///   string would be `'component('` inside a template-string
///   constant — vanishingly rare and easy for the customer to
///   refactor.
fn cross_validate_component_invocations(
    pages: &[BundledPage],
    components: &[BundledComponent],
) -> Result<()> {
    use regex::Regex;
    // `component("name", …)` or `component('name', …)` — match the
    // bare global-function form minijinja registers.
    let re = Regex::new(r#"\bcomponent\s*\(\s*["']([^"']+)["']"#)
        .context("compile component-invocation regex")?;

    let known: std::collections::HashSet<&str> =
        components.iter().map(|c| c.name.as_str()).collect();

    let mut report = Vec::new();
    for page in pages {
        for capture in re.captures_iter(&page.template_body) {
            let name = capture.get(1).unwrap().as_str();
            if name.starts_with('@') || known.contains(name) {
                continue;
            }
            report.push(format!(
                "page `{}` invokes unknown component `{}`",
                page.name, name,
            ));
        }
    }
    for component in components {
        for capture in re.captures_iter(&component.template_body) {
            let name = capture.get(1).unwrap().as_str();
            if name.starts_with('@') || known.contains(name) {
                continue;
            }
            report.push(format!(
                "component `{}` invokes unknown component `{}`",
                component.name, name,
            ));
        }
    }
    if !report.is_empty() {
        anyhow::bail!(
            "component cross-validation failed:\n{}\n\
             Hint: declare the component under `components/`, or qualify the \
             call with `@forge/<name>` / `@tenant/<name>` when referencing a \
             built-in or tenant-shared component.",
            report
                .iter()
                .map(|l| format!("  - {l}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    Ok(())
}

/// Phase E.4 (gap-close) — cross-check `app.json.tenant_claims`'s
/// referenced domains against forge-cp's validated-domain catalog.
///
/// Best-effort: when `FORGE_CP_URL` + `FORGE_ADMIN_TOKEN` are set
/// (operator mode), the CLI calls the CP and fails fast if any
/// claimed domain is missing or not in `cert_status == "issued"`.
/// Without credentials (customer-mode CLI), the check is skipped
/// with a single-line warning so deploys remain unblocked by the
/// absence of operator config.
async fn cross_validate_tenant_claims(manifest: &forge_schema::AppManifest) -> Result<()> {
    let domains: Vec<&String> = manifest
        .tenant_claims
        .domains
        .iter()
        .filter(|d| !d.is_empty())
        .collect();
    if domains.is_empty() {
        return Ok(());
    }
    // Operator mode requires three env vars: CP base URL, admin
    // bearer, and the tenant whose domain catalog to consult.
    // tenant_id can't come from the manifest — app.json doesn't
    // carry tenant identity (it's resolved from the workspace
    // envelope at runtime). The operator passes FORGE_TENANT_ID
    // explicitly when running deploy in cross-check mode.
    let (Ok(base), Ok(bearer), Ok(tenant_id)) = (
        std::env::var("FORGE_CP_URL"),
        std::env::var("FORGE_ADMIN_TOKEN"),
        std::env::var("FORGE_TENANT_ID"),
    ) else {
        eprintln!(
            "warning: app.json declares {} tenant_claims.domains entries but \
             FORGE_CP_URL / FORGE_ADMIN_TOKEN / FORGE_TENANT_ID are unset — \
             skipping deploy-time validation. The runtime will reject \
             unregistered claims when the routing-table swap lands.",
            domains.len(),
        );
        return Ok(());
    };
    let base = base.trim_end_matches('/');
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("build http client")?;
    let url = format!("{base}/admin/tenants/{tenant_id}/domains");
    let resp = client
        .get(&url)
        .bearer_auth(&bearer)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        let s = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("forge-cp {s}: {body}");
    }
    #[derive(serde::Deserialize)]
    struct DomainRow {
        hostname: String,
        cert_status: String,
    }
    let catalog: Vec<DomainRow> = resp
        .json()
        .await
        .context("decode validated-domains catalog")?;
    let issued: std::collections::HashSet<&str> = catalog
        .iter()
        .filter(|d| d.cert_status == "issued")
        .map(|d| d.hostname.as_str())
        .collect();
    let mut missing = Vec::new();
    for domain in &domains {
        if !issued.contains(domain.as_str()) {
            missing.push(domain.as_str());
        }
    }
    if !missing.is_empty() {
        anyhow::bail!(
            "tenant `{tenant_id}` hasn't validated these claimed domains: {missing:?}.\n\
             Run `forge domain add <hostname>` + provision the DNS TXT record + \
             `forge domain validate <hostname>` for each before redeploying."
        );
    }
    Ok(())
}

#[derive(Debug, serde::Serialize)]
struct BundledPage {
    name: String,
    manifest: forge_schema::PageManifest,
    template_body: String,
    css_body: String,
}

#[derive(Debug, serde::Serialize)]
struct BundledComponent {
    name: String,
    manifest: forge_schema::ComponentManifest,
    template_body: String,
    css_body: String,
}

/// Parse a `forge-schema` manifest from JSON bytes with a friendlier
/// version-skew hint. When serde reports `unknown variant ...`, the
/// CLI's bundled `forge-schema` (compile-time path-dep) is older
/// than the manifest's schema; reinstalling the CLI usually fixes it.
fn parse_schema_json<T>(raw: &[u8], path: &std::path::Path, kind: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    match serde_json::from_slice::<T>(raw) {
        Ok(v) => Ok(v),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("unknown variant") || msg.contains("unknown field") {
                anyhow::bail!(
                    "parse {kind} at {}: {e}\n\n\
                     hint: \"unknown variant / field\" errors usually mean your `forge` CLI \
                     was built against an older `forge-schema` than this manifest. \
                     Reinstall the CLI:\n  \
                     cd $(dirname $(which forge))/../../Documents/forge-cli 2>/dev/null || cd ../forge-cli\n  \
                     cargo install --path . --locked\n  \
                     # If `which forge` points at ~/.local/bin/forge, overwrite that copy too:\n  \
                     cp ~/.cargo/bin/forge ~/.local/bin/forge",
                    path.display()
                );
            }
            Err(anyhow::anyhow!("parse {} at {}: {e}", kind, path.display()))
        }
    }
}

fn collect_page_jsons(dir: &PathBuf) -> Result<Vec<BundledPage>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let entry = entry.with_context(|| format!("walking {}", dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let fname = match path.file_name().and_then(|s| s.to_str()) {
            Some(f) => f,
            None => continue,
        };
        let Some(stem) = fname.strip_suffix(".page.json") else {
            continue;
        };
        let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let manifest: forge_schema::PageManifest = parse_schema_json(&raw, path, "page manifest")?;
        let errors = forge_schema::validate_page_manifest(&manifest);
        if !errors.is_empty() {
            anyhow::bail!(
                "page `{stem}` at {} failed validation:\n{}",
                path.display(),
                errors
                    .iter()
                    .map(|e| format!("  - {e}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        // Sibling template + css (optional).
        let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let template_path = parent.join(format!("{stem}.page.html"));
        let css_path = parent.join(format!("{stem}.page.css"));
        let template_body = if template_path.exists() {
            std::fs::read_to_string(&template_path)
                .with_context(|| format!("read {}", template_path.display()))?
        } else {
            String::new()
        };
        let css_body = if css_path.exists() {
            std::fs::read_to_string(&css_path)
                .with_context(|| format!("read {}", css_path.display()))?
        } else {
            String::new()
        };
        out.push(BundledPage {
            name: stem.to_string(),
            manifest,
            template_body,
            css_body,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn collect_component_jsons(dir: &PathBuf) -> Result<Vec<BundledComponent>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let entry = entry.with_context(|| format!("walking {}", dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let fname = match path.file_name().and_then(|s| s.to_str()) {
            Some(f) => f,
            None => continue,
        };
        let Some(stem) = fname.strip_suffix(".component.json") else {
            continue;
        };
        let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let manifest: forge_schema::ComponentManifest =
            parse_schema_json(&raw, path, "component manifest")?;
        let errors = forge_schema::validate_component_manifest(&manifest);
        if !errors.is_empty() {
            anyhow::bail!(
                "component `{stem}` at {} failed validation:\n{}",
                path.display(),
                errors
                    .iter()
                    .map(|e| format!("  - {e}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let template_path = parent.join(format!("{stem}.component.html"));
        let css_path = parent.join(format!("{stem}.component.css"));
        let template_body = if template_path.exists() {
            std::fs::read_to_string(&template_path)
                .with_context(|| format!("read {}", template_path.display()))?
        } else {
            String::new()
        };
        let css_body = if css_path.exists() {
            std::fs::read_to_string(&css_path)
                .with_context(|| format!("read {}", css_path.display()))?
        } else {
            String::new()
        };
        out.push(BundledComponent {
            name: stem.to_string(),
            manifest,
            template_body,
            css_body,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

// NOTE: the manifest-hash golden-vector test moved to the shared
// `forge-manifest` crate (crates/forge-manifest/src/lib.rs), where the
// cross-repo hash contract is now pinned for every producer (forge-cli +
// forge-git). See `forge_manifest::manifest_hash_tests`.

#[cfg(test)]
mod app_bundle_tests {
    use super::*;
    use tempfile::TempDir;

    fn write(root: &std::path::Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn deploy_args(app_manifest: Option<PathBuf>) -> DeployArgs {
        DeployArgs {
            manifest: PathBuf::from("/unused"),
            wasm: Vec::new(),
            app_manifest,
            pages_dir: None,
            components_dir: None,
            schema_dir: None,
            data_dir: None,
            json: false,
            allow_app_replace: false,
            inline_wasm: false,
            force: false,
            force_unpin: false,
            emit_manifest: None,
            emit_lock: None,
            no_apply: false,
        }
    }

    #[tokio::test]
    async fn missing_app_manifest_returns_null_bundle() {
        let args = deploy_args(None);
        let (bundle, _) = build_app_bundle(&args).await.unwrap();
        assert!(bundle.is_null());
    }

    #[test]
    fn collect_schema_artifacts_emits_inline_schema_rows() {
        let tmp = TempDir::new().unwrap();
        let app_path = tmp.path().join("app.json");
        write(tmp.path(), "app.json", "{}");
        // Two schema files (collected in sorted order) + a non-JSON file ignored.
        write(
            tmp.path(),
            "schemas/widgets.json",
            r#"{"name":"widgets","archetype":"Base","columns":[]}"#,
        );
        write(
            tmp.path(),
            "schemas/gadgets.json",
            r#"{"name":"gadgets","archetype":"Base","columns":[]}"#,
        );
        write(tmp.path(), "schemas/README.md", "not a schema");

        let args = deploy_args(Some(app_path));
        let rows = collect_schema_artifacts(&args, tmp.path()).expect("collect ok");
        assert_eq!(rows.len(), 2, "only the two *.json schema files are rows");
        // Sorted by path: gadgets before widgets.
        assert_eq!(rows[0]["path"], "schemas/gadgets.json");
        assert_eq!(rows[0]["kind"], "schema");
        assert!(rows[0]["content"].is_string(), "schema rides inline as content");
        assert!(rows[0]["content_hash"].is_string());
        assert_eq!(rows[1]["path"], "schemas/widgets.json");
    }

    #[test]
    fn collect_schema_artifacts_skips_absent_default_dir() {
        // No schemas/ dir next to the app manifest ⇒ no rows, no error.
        let tmp = TempDir::new().unwrap();
        let app_path = tmp.path().join("app.json");
        write(tmp.path(), "app.json", "{}");
        let args = deploy_args(Some(app_path));
        assert!(collect_schema_artifacts(&args, tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn collect_schema_artifacts_errors_on_missing_explicit_dir() {
        // An explicit --schema-dir that doesn't exist is a hard error (a typo'd
        // path must not silently drop the workspace's schema).
        let tmp = TempDir::new().unwrap();
        let mut args = deploy_args(None);
        args.schema_dir = Some(tmp.path().join("nope"));
        assert!(collect_schema_artifacts(&args, tmp.path()).is_err());
    }

    #[test]
    fn collect_schema_artifacts_rejects_malformed_and_unnamed() {
        let tmp = TempDir::new().unwrap();
        // Malformed JSON fails fast.
        write(tmp.path(), "schemas/bad.json", "{not json");
        let mut args = deploy_args(None);
        args.schema_dir = Some(tmp.path().join("schemas"));
        assert!(collect_schema_artifacts(&args, tmp.path()).is_err());

        // Valid JSON but no `name` is also rejected (the converge needs a table).
        let tmp2 = TempDir::new().unwrap();
        write(tmp2.path(), "schemas/nameless.json", r#"{"archetype":"Base"}"#);
        let mut args2 = deploy_args(None);
        args2.schema_dir = Some(tmp2.path().join("schemas"));
        assert!(collect_schema_artifacts(&args2, tmp2.path()).is_err());
    }

    #[test]
    fn collect_config_data_artifacts_emits_inline_rows() {
        let tmp = TempDir::new().unwrap();
        let app_path = tmp.path().join("app.json");
        write(tmp.path(), "app.json", "{}");
        write(
            tmp.path(),
            "data/plan_catalog.json",
            r#"{"table":"plan_catalog","key":"code","rows":[{"code":"hobby"}]}"#,
        );
        write(
            tmp.path(),
            "data/cms_content.json",
            r#"{"table":"cms_content","rows":[]}"#,
        );
        write(tmp.path(), "data/README.md", "not config-data");

        let args = deploy_args(Some(app_path));
        let rows = collect_config_data_artifacts(&args, tmp.path()).expect("collect ok");
        assert_eq!(rows.len(), 2, "only the two *.json files are rows");
        // Sorted by path: cms_content before plan_catalog.
        assert_eq!(rows[0]["path"], "data/cms_content.json");
        assert_eq!(rows[0]["kind"], "config_data");
        assert!(rows[0]["content"].is_string(), "rowset rides inline as content");
        assert!(rows[0]["content_hash"].is_string());
        assert_eq!(rows[1]["path"], "data/plan_catalog.json");
    }

    #[test]
    fn collect_config_data_artifacts_skips_absent_default_dir() {
        let tmp = TempDir::new().unwrap();
        let app_path = tmp.path().join("app.json");
        write(tmp.path(), "app.json", "{}");
        let args = deploy_args(Some(app_path));
        assert!(collect_config_data_artifacts(&args, tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn collect_config_data_artifacts_rejects_malformed_and_untabled() {
        // Malformed JSON fails fast.
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "data/bad.json", "{not json");
        let mut args = deploy_args(None);
        args.data_dir = Some(tmp.path().join("data"));
        assert!(collect_config_data_artifacts(&args, tmp.path()).is_err());

        // Valid JSON but no `table` is rejected.
        let tmp2 = TempDir::new().unwrap();
        write(tmp2.path(), "data/x.json", r#"{"rows":[]}"#);
        let mut args2 = deploy_args(None);
        args2.data_dir = Some(tmp2.path().join("data"));
        assert!(collect_config_data_artifacts(&args2, tmp2.path()).is_err());

        // `table` present but no `rows` array is rejected.
        let tmp3 = TempDir::new().unwrap();
        write(tmp3.path(), "data/y.json", r#"{"table":"t"}"#);
        let mut args3 = deploy_args(None);
        args3.data_dir = Some(tmp3.path().join("data"));
        assert!(collect_config_data_artifacts(&args3, tmp3.path()).is_err());
    }

    #[test]
    fn collect_config_data_artifacts_errors_on_missing_explicit_dir() {
        let tmp = TempDir::new().unwrap();
        let mut args = deploy_args(None);
        args.data_dir = Some(tmp.path().join("nope"));
        assert!(collect_config_data_artifacts(&args, tmp.path()).is_err());
    }

    #[tokio::test]
    async fn valid_app_with_no_pages_or_components_round_trips() {
        let tmp = TempDir::new().unwrap();
        let app_path = tmp.path().join("app.json");
        write(
            tmp.path(),
            "app.json",
            r#"{
                "schema_version":"0.1",
                "app":{"name":"hello","version":"1.0.0"},
                "shell":{},
                "routing_claim":{"host":"@default","path":"/"},
                "tenant_claims":{}
            }"#,
        );
        let args = deploy_args(Some(app_path.clone()));
        let (bundle, _) = build_app_bundle(&args).await.expect("valid");
        assert_eq!(bundle["app_manifest"]["app"]["name"], "hello");
        assert_eq!(bundle["pages"].as_array().unwrap().len(), 0);
        assert_eq!(bundle["components"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn invalid_app_manifest_aborts_deploy() {
        let tmp = TempDir::new().unwrap();
        // Routing-claim path must start with `/` — forge-schema
        // rejects with InvalidRoutePath.
        write(
            tmp.path(),
            "app.json",
            r#"{
                "schema_version":"0.1",
                "app":{"name":"hello","version":"1.0.0"},
                "shell":{},
                "routing_claim":{"host":"@default","path":"missing-slash"},
                "tenant_claims":{}
            }"#,
        );
        let args = deploy_args(Some(tmp.path().join("app.json")));
        let err = build_app_bundle(&args).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("InvalidRoutePath") || msg.contains("must start with"),
            "got: {msg}"
        );
    }

    #[tokio::test]
    async fn invalid_page_manifest_aborts_deploy() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "app.json",
            r#"{
                "schema_version":"0.1",
                "app":{"name":"hello","version":"1.0.0"},
                "shell":{},
                "routing_claim":{"host":"@default","path":"/"},
                "tenant_claims":{}
            }"#,
        );
        // Bad page: invalid HTTP method.
        write(
            tmp.path(),
            "pages/bad.page.json",
            r#"{"route":{"method":"FETCH","path":"/bad"},"template":{"path":"bad.page.html"}}"#,
        );
        write(tmp.path(), "pages/bad.page.html", "<div></div>");
        let args = deploy_args(Some(tmp.path().join("app.json")));
        let err = build_app_bundle(&args).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("FETCH") || msg.contains("InvalidRouteMethod"),
            "got: {msg}",
        );
    }

    #[tokio::test]
    async fn valid_pages_and_components_bundled() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "app.json",
            r#"{
                "schema_version":"0.1",
                "app":{"name":"hello","version":"1.0.0"},
                "shell":{},
                "routing_claim":{"host":"@default","path":"/"},
                "tenant_claims":{}
            }"#,
        );
        write(
            tmp.path(),
            "pages/landing.page.json",
            r#"{"name":"landing","route":{"path":"/"},"template":{"path":"landing.page.html"}}"#,
        );
        write(tmp.path(), "pages/landing.page.html", "<h1>hi</h1>");
        write(
            tmp.path(),
            "components/card.component.json",
            // PropsSchema is `#[serde(transparent)]` — the outer
            // `props` field is the BTreeMap directly.
            r#"{"name":"card","props":{}}"#,
        );
        write(tmp.path(), "components/card.component.html", "<div></div>");
        let args = deploy_args(Some(tmp.path().join("app.json")));
        let (bundle, _) = build_app_bundle(&args).await.expect("valid");
        let pages = bundle["pages"].as_array().unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0]["name"], "landing");
        let components = bundle["components"].as_array().unwrap();
        assert_eq!(components.len(), 1);
        assert_eq!(components[0]["name"], "card");
    }

    #[tokio::test]
    async fn page_invoking_unknown_component_fails_cross_validation() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "app.json",
            r#"{
                "schema_version":"0.1",
                "app":{"name":"hello","version":"1.0.0"},
                "shell":{},
                "routing_claim":{"host":"@default","path":"/"},
                "tenant_claims":{}
            }"#,
        );
        write(
            tmp.path(),
            "pages/landing.page.json",
            r#"{"name":"landing","route":{"path":"/"},"template":{"path":"landing.page.html"}}"#,
        );
        // Page invokes `mystery-card` which the components/ dir
        // doesn't define.
        write(
            tmp.path(),
            "pages/landing.page.html",
            r#"<div>{{ component("mystery-card") }}</div>"#,
        );
        write(
            tmp.path(),
            "components/card.component.json",
            r#"{"name":"card","props":{}}"#,
        );
        write(tmp.path(), "components/card.component.html", "<div></div>");
        let args = deploy_args(Some(tmp.path().join("app.json")));
        let err = build_app_bundle(&args).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("mystery-card") && msg.contains("unknown component"),
            "got: {msg}",
        );
    }

    #[tokio::test]
    async fn qualified_component_invocations_pass_cross_validation() {
        // `@forge/card` is a built-in cascade qualifier — the CLI
        // can't reach the runtime's built-in catalog so it accepts
        // any `@`-qualified name and trusts the runtime's resolver.
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "app.json",
            r#"{
                "schema_version":"0.1",
                "app":{"name":"hello","version":"1.0.0"},
                "shell":{},
                "routing_claim":{"host":"@default","path":"/"},
                "tenant_claims":{}
            }"#,
        );
        write(
            tmp.path(),
            "pages/landing.page.json",
            r#"{"name":"landing","route":{"path":"/"},"template":{"path":"landing.page.html"}}"#,
        );
        write(
            tmp.path(),
            "pages/landing.page.html",
            r#"<div>{{ component('@forge/card', {title: 'hi'}) }}</div>"#,
        );
        let args = deploy_args(Some(tmp.path().join("app.json")));
        let (bundle, _) = build_app_bundle(&args).await.expect("valid");
        assert_eq!(bundle["app_manifest"]["app"]["name"], "hello");
    }
}
