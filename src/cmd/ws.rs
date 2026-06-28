//! `forge ws` — list + select workspaces in the operator's
//! portal session. Scoped to the active tenant if set. Setting
//! the active workspace clears the previous federated-bearer
//! cache so the client's 401-retry mints a fresh one on first
//! use against the new target.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde::Deserialize;

use crate::config;

#[derive(Debug, Subcommand)]
pub enum WsCmd {
    /// List workspaces visible to the active portal session.
    /// Filters by the active tenant when one is set via
    /// `forge tenant use`.
    List,
    /// Set the active workspace. Writes
    /// `[current].workspace_id` to `~/.forge/config.toml`.
    /// Subsequent CLI commands target this workspace and let
    /// the 401-retry interceptor mint a federated bearer on
    /// demand.
    Use(UseArgs),
}

#[derive(Debug, clap::Args)]
pub struct UseArgs {
    pub workspace_id: String,
}

pub async fn run(cmd: WsCmd) -> Result<()> {
    match cmd {
        WsCmd::List => list().await,
        WsCmd::Use(args) => use_(args),
    }
}

/// cp-admin returns one row per workspace with the canonical
/// envelope nested under `envelope` and the assignment fields at
/// the top level. We don't decode the whole envelope — just the
/// shape we need to render the list. `workspace_id` lives inside
/// the envelope (and `tenant_id` too); we pull them up at the row
/// level via a custom `Deserialize` impl backed by serde_json on
/// the lazy fields.
#[derive(Debug, Deserialize)]
struct CpWorkspaceRow {
    #[serde(default)]
    assigned_node_id: Option<String>,
    envelope: EnvelopeBrief,
}

#[derive(Debug, Deserialize)]
struct EnvelopeBrief {
    workspace_id: String,
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    frozen: Option<bool>,
    #[serde(default)]
    generation: Option<u64>,
}

async fn list() -> Result<()> {
    let portal = config::read_portal_session()?.ok_or_else(crate::cmd::sso::no_portal_hint)?;
    let (current_tenant, current_ws) = config::read_current_selection()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("build http client")?;
    let url = format!(
        "{}/api/v1/cp-admin/workspaces",
        portal.base_url.trim_end_matches('/'),
    );
    let resp = client
        .get(&url)
        .bearer_auth(&portal.bearer)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("portal /cp-admin/workspaces {status}: {body}");
    }
    let mut rows: Vec<CpWorkspaceRow> = resp.json().await.context("decode workspaces list")?;
    // Tenant filter is client-side: cp-admin returns every
    // workspace the operator's admin bearer can see; we narrow to
    // the active tenant so the listing matches what the operator
    // expects after `forge tenant use`.
    if let Some(tid) = current_tenant.as_deref() {
        // Keep rows whose envelope tenant_id matches, OR whose
        // tenant_id is null — legacy envelopes that predate the
        // wire-crate's `ExecutionEnvelope.tenant_id` field round-
        // trip with null here even if the workspace IS attached
        // to a tenant via the portal's `workspaces` table. Showing
        // them lets the operator find their workspaces; the
        // unfiltered case (no `forge tenant use`) already returns
        // everything anyway, so this isn't an information leak.
        rows.retain(|r| r.envelope.tenant_id.as_deref().is_none_or(|t| t == tid));
    }
    if rows.is_empty() {
        if current_tenant.is_some() {
            eprintln!("(no workspaces in active tenant)");
        } else {
            eprintln!("(no workspaces visible to this portal session)");
        }
        return Ok(());
    }
    println!(
        "WORKSPACE_ID     NODE                         TENANT                     FROZEN  GEN"
    );
    for r in &rows {
        let active = current_ws.as_deref() == Some(r.envelope.workspace_id.as_str());
        let marker = if active { "*" } else { " " };
        let frozen = r
            .envelope
            .frozen
            .map(|b| if b { "yes" } else { "-" })
            .unwrap_or("-");
        let generation = r
            .envelope
            .generation
            .map(|g| g.to_string())
            .unwrap_or_else(|| "-".into());
        let node = r.assigned_node_id.as_deref().unwrap_or("(unassigned)");
        // `(tenant=?)` flags the legacy-envelope case where the
        // CP build is older than the wire-crate's tenant_id field
        // — operator should re-set tenant on the envelope (or
        // upgrade the CP) so the filter behaves predictably.
        let tenant = r.envelope.tenant_id.as_deref().unwrap_or("(tenant=?)");
        println!(
            "{marker}{ws:<15} {node:<28} {tenant:<26} {frozen:<7} {gen}",
            ws = r.envelope.workspace_id,
            gen = generation,
        );
    }
    if let Some(w) = current_ws.as_deref() {
        println!("\n* active workspace ({w})");
    } else {
        println!("\n(no active workspace; run `forge ws use <id>`)");
    }
    Ok(())
}

fn use_(args: UseArgs) -> Result<()> {
    config::set_current_workspace(&args.workspace_id)?;
    eprintln!(
        "active workspace: {} — subsequent commands will mint federated bearers on demand",
        args.workspace_id,
    );
    Ok(())
}
