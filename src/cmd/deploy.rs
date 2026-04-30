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
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::{ForgeClient, ForgeError};

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

    /// Print the response as JSON instead of human-friendly text.
    #[arg(long)]
    json: bool,
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
    services: Vec<DeployedService>,
    registry_version: u64,
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
        wasm_modules.push(serde_json::json!({
            "service_namespace": entry.service_namespace,
            "service_name": entry.service_name,
            "wasm_bytes": bytes,
        }));
    }

    let payload = serde_json::json!({
        "services": manifest.services,
        "wasm_modules": wasm_modules,
    });

    let resp: DeployResponse = client
        .post_json("/api/v1/manage/wasm/deploy", &payload)
        .await
        .map_err(map_err)?;

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
