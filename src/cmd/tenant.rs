//! `forge tenant` — list + select tenants in the operator's
//! portal session. The portal owns the unified tenant model; this
//! subcommand exposes it programmatically so operators can pick a
//! tenant without clicking through the web UI.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde::Deserialize;

use crate::config;

#[derive(Debug, Subcommand)]
pub enum TenantCmd {
    /// List tenants the active portal user belongs to.
    List,
    /// Set the active tenant. Writes `[current].tenant_id` to
    /// `~/.forge/config.toml`. Subsequent `forge ws list` calls
    /// scope to this tenant.
    Use(UseArgs),
}

#[derive(Debug, clap::Args)]
pub struct UseArgs {
    pub tenant_id: String,
}

pub async fn run(cmd: TenantCmd) -> Result<()> {
    match cmd {
        TenantCmd::List => list().await,
        TenantCmd::Use(args) => use_(args),
    }
}

#[derive(Debug, Deserialize)]
struct TenantRow {
    id: String,
    name: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    tier: Option<String>,
}

async fn list() -> Result<()> {
    let portal = config::read_portal_session()?.ok_or_else(crate::cmd::sso::no_portal_hint)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("build http client")?;
    let url = format!(
        "{}/api/v1/data/tenants",
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
        bail!("portal /data/tenants {status}: {body}");
    }
    let rows: Vec<TenantRow> = resp.json().await.context("decode tenants list")?;
    let (current_tenant, _) = config::read_current_selection()?;
    if rows.is_empty() {
        eprintln!("(no tenants — your portal user has no tenant_memberships rows)");
        return Ok(());
    }
    println!("TENANT_ID                                KIND       TIER         NAME");
    for r in &rows {
        let active = current_tenant.as_deref() == Some(&r.id);
        let marker = if active { "*" } else { " " };
        let kind = r.kind.as_deref().unwrap_or("-");
        let tier = r.tier.as_deref().unwrap_or("-");
        println!(
            "{marker}{id:<39} {kind:<10} {tier:<12} {name}",
            id = r.id,
            name = r.name,
        );
    }
    if let Some(t) = current_tenant.as_deref() {
        println!("\n* active tenant ({t})");
    } else {
        println!("\n(no active tenant; run `forge tenant use <id>`)");
    }
    Ok(())
}

fn use_(args: UseArgs) -> Result<()> {
    // We don't pre-verify membership here — the portal will reject
    // any /api/v1/cp-admin/workspaces request for a tenant the user
    // isn't a member of, and the federated mint path also re-checks
    // tenant_memberships server-side. Saving locally without a
    // server round-trip keeps `forge tenant use` fast + offline.
    config::set_current_tenant(&args.tenant_id)?;
    eprintln!("active tenant: {}", args.tenant_id);
    Ok(())
}
