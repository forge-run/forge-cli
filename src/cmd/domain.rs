//! `forge domain` — Phase E.3 admin CLI for tenant domain
//! claims. Thin wrapper over forge-cp's `/admin/tenants/:id/domains`
//! routes.
//!
//! Auth: direct-to-CP with operator credentials. Two env vars
//! required:
//! - `FORGE_CP_URL` — base URL of forge-cp (e.g.,
//!   `https://cp.internal.forge.run`).
//! - `FORGE_ADMIN_TOKEN` — bearer the CP accepts on
//!   `/admin/*`. Operator-issued; not a customer-facing token.
//!
//! Tenant id is sourced from the current selection
//! (`config::read_current_selection`). Pass `--tenant-id` to
//! override.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Debug, Subcommand)]
pub enum DomainCmd {
    /// Claim a hostname for the active tenant. Begins the ACME
    /// validation flow; returns DNS TXT record instructions.
    Add(AddArgs),
    /// List all domains claimed by the active tenant.
    List(ListArgs),
    /// Show ACME validation status for a hostname.
    Status(StatusArgs),
    /// Update the claim policy on an existing domain (Strict / Open).
    Policy(PolicyArgs),
    /// Trigger ACME validation polling — call after provisioning
    /// the DNS TXT record. On success, the cert is issued and
    /// served by the edge on the next routing-table poll.
    Validate(ValidateArgs),
}

#[derive(Debug, clap::Args)]
pub struct AddArgs {
    /// Hostname to claim, e.g. `shop.acme.com` or wildcard `*.acme.com`.
    pub hostname: String,
    /// Claim policy: `strict` (per-row admin click-through; safer
    /// default) or `open` (SaaS-style arbitrary subdomain claims).
    #[arg(long, default_value = "strict")]
    pub policy: String,
    /// Override the active-tenant selection.
    #[arg(long)]
    pub tenant_id: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct ListArgs {
    #[arg(long)]
    pub tenant_id: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct StatusArgs {
    pub hostname: String,
    #[arg(long)]
    pub tenant_id: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct PolicyArgs {
    pub hostname: String,
    /// `strict` or `open`.
    pub policy: String,
    #[arg(long)]
    pub tenant_id: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct ValidateArgs {
    pub hostname: String,
    #[arg(long)]
    pub tenant_id: Option<String>,
}

pub async fn run(cmd: DomainCmd) -> Result<()> {
    match cmd {
        DomainCmd::Add(args) => add(args).await,
        DomainCmd::List(args) => list(args).await,
        DomainCmd::Status(args) => status(args).await,
        DomainCmd::Policy(args) => policy(args).await,
        DomainCmd::Validate(args) => validate(args).await,
    }
}

// ── HTTP plumbing ────────────────────────────────────────────────

struct CpClient {
    client: reqwest::Client,
    base_url: String,
    bearer: String,
}

impl CpClient {
    fn from_env() -> Result<Self> {
        let base_url = std::env::var("FORGE_CP_URL").context(
            "FORGE_CP_URL not set — point at the control-plane base URL \
             (e.g., https://cp.internal.forge.run). Required for `forge domain`.",
        )?;
        let bearer = std::env::var("FORGE_ADMIN_TOKEN").context(
            "FORGE_ADMIN_TOKEN not set — pass the operator bearer the CP \
             accepts on /admin/*. Required for `forge domain`.",
        )?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("build http client")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            bearer,
        })
    }
}

fn resolve_tenant_id(override_id: Option<String>) -> Result<String> {
    if let Some(id) = override_id {
        return Ok(id);
    }
    let (tenant, _) = config::read_current_selection()?;
    tenant.ok_or_else(|| {
        anyhow::anyhow!("no active tenant — run `forge tenant use <tenant-id>` or pass --tenant-id")
    })
}

// ── Domain response shape (mirrors forge-cp's TenantDomainResponse) ─

#[derive(Debug, Serialize, Deserialize)]
struct DomainResponse {
    pub tenant_id: String,
    pub hostname: String,
    #[serde(default)]
    pub validated_at: Option<String>,
    pub cert_status: String,
    pub claim_policy: String,
    #[serde(default)]
    pub dns_challenge_state: Option<serde_json::Value>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
struct ClaimDomainRequest {
    hostname: String,
    claim_policy: String,
}

// ── Subcommand impls ─────────────────────────────────────────────

async fn add(args: AddArgs) -> Result<()> {
    let cp = CpClient::from_env()?;
    let tenant_id = resolve_tenant_id(args.tenant_id)?;
    let policy = normalize_policy(&args.policy)?;
    let url = format!("{}/admin/tenants/{tenant_id}/domains", cp.base_url);
    let resp = cp
        .client
        .post(&url)
        .bearer_auth(&cp.bearer)
        .json(&ClaimDomainRequest {
            hostname: args.hostname.clone(),
            claim_policy: policy,
        })
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        // Phase E.3 (gap-close) — special-case 409 with an actionable
        // pointer. The CP rejects same-tenant + cross-tenant
        // duplicates under the same status code; the message can't
        // disambiguate so we suggest `forge domain list` (covers
        // the same-tenant case) and contact ops (cross-tenant).
        if status.as_u16() == 409 {
            bail!(
                "hostname `{}` is already claimed.\n\
                 Run `forge domain list` to check your tenant's domains. \
                 If you don't own it, the claim belongs to another tenant — \
                 contact platform ops to release or transfer it.\n\
                 (forge-cp said: {body})",
                args.hostname,
            );
        }
        bail!("forge-cp /admin/tenants/{tenant_id}/domains {status}: {body}");
    }
    let domain: DomainResponse = serde_json::from_str(&body).context("decode domain response")?;
    println!("Domain claimed:");
    println!("  hostname: {}", domain.hostname);
    println!("  cert_status: {}", domain.cert_status);
    println!("  claim_policy: {}", domain.claim_policy);
    if let Some(challenge) = &domain.dns_challenge_state {
        println!();
        println!("Next step: provision this DNS TXT record so ACME can validate:");
        if let Some(name) = challenge.get("dns_record_name").and_then(|v| v.as_str()) {
            println!("  TXT name:  {name}");
        }
        if let Some(value) = challenge.get("dns_record_value").and_then(|v| v.as_str()) {
            println!("  TXT value: {value}");
        }
        if let Some(expires) = challenge.get("expires_at").and_then(|v| v.as_str()) {
            println!("  expires:   {expires}");
        }
        println!();
        println!(
            "Then run `forge domain validate {}` to finalize the cert.",
            domain.hostname
        );
    }
    Ok(())
}

async fn list(args: ListArgs) -> Result<()> {
    let cp = CpClient::from_env()?;
    let tenant_id = resolve_tenant_id(args.tenant_id)?;
    let url = format!("{}/admin/tenants/{tenant_id}/domains", cp.base_url);
    let resp = cp
        .client
        .get(&url)
        .bearer_auth(&cp.bearer)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        let s = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("forge-cp {s}: {body}");
    }
    let rows: Vec<DomainResponse> = resp.json().await.context("decode domain list")?;
    if rows.is_empty() {
        eprintln!("(no domains claimed)");
        return Ok(());
    }
    println!("HOSTNAME                              STATUS        POLICY    VALIDATED");
    for r in &rows {
        let validated = r.validated_at.as_deref().unwrap_or("—");
        println!(
            " {:<37} {:<13} {:<9} {}",
            r.hostname, r.cert_status, r.claim_policy, validated,
        );
    }
    Ok(())
}

async fn status(args: StatusArgs) -> Result<()> {
    let cp = CpClient::from_env()?;
    let tenant_id = resolve_tenant_id(args.tenant_id)?;
    let url = format!(
        "{}/admin/tenants/{tenant_id}/domains/{}/status",
        cp.base_url, args.hostname,
    );
    let resp = cp
        .client
        .get(&url)
        .bearer_auth(&cp.bearer)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        let s = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("forge-cp {s}: {body}");
    }
    let domain: DomainResponse = resp.json().await.context("decode domain status")?;
    println!("hostname:     {}", domain.hostname);
    println!("cert_status:  {}", domain.cert_status);
    println!("claim_policy: {}", domain.claim_policy);
    if let Some(v) = &domain.validated_at {
        println!("validated_at: {v}");
    }
    if let Some(c) = &domain.dns_challenge_state {
        println!("challenge:    {}", serde_json::to_string_pretty(c)?);
    }
    Ok(())
}

async fn policy(args: PolicyArgs) -> Result<()> {
    let cp = CpClient::from_env()?;
    let tenant_id = resolve_tenant_id(args.tenant_id)?;
    let policy = normalize_policy(&args.policy)?;
    let url = format!(
        "{}/admin/tenants/{tenant_id}/domains/{}/policy",
        cp.base_url, args.hostname,
    );
    let resp = cp
        .client
        .put(&url)
        .bearer_auth(&cp.bearer)
        .json(&serde_json::json!({"claim_policy": policy}))
        .send()
        .await
        .with_context(|| format!("PUT {url}"))?;
    if !resp.status().is_success() {
        let s = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("forge-cp /policy {s}: {body}");
    }
    let domain: DomainResponse = resp.json().await.context("decode policy response")?;
    println!("Policy updated:");
    println!("  hostname:     {}", domain.hostname);
    println!("  claim_policy: {}", domain.claim_policy);
    Ok(())
}

async fn validate(args: ValidateArgs) -> Result<()> {
    let cp = CpClient::from_env()?;
    let tenant_id = resolve_tenant_id(args.tenant_id)?;
    let url = format!(
        "{}/admin/tenants/{tenant_id}/domains/{}/validate",
        cp.base_url, args.hostname,
    );
    let resp = cp
        .client
        .post(&url)
        .bearer_auth(&cp.bearer)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        let s = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("forge-cp /validate {s}: {body}");
    }
    let domain: DomainResponse = resp.json().await.context("decode validate response")?;
    println!("Validation complete:");
    println!("  hostname:     {}", domain.hostname);
    println!("  cert_status:  {}", domain.cert_status);
    if let Some(v) = &domain.validated_at {
        println!("  validated_at: {v}");
    }
    Ok(())
}

fn normalize_policy(s: &str) -> Result<String> {
    match s.to_ascii_lowercase().as_str() {
        "strict" => Ok("Strict".into()),
        "open" => Ok("Open".into()),
        other => bail!("unknown claim policy `{other}` — use `strict` or `open`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_policy_handles_case_insensitively() {
        assert_eq!(normalize_policy("strict").unwrap(), "Strict");
        assert_eq!(normalize_policy("STRICT").unwrap(), "Strict");
        assert_eq!(normalize_policy("Open").unwrap(), "Open");
        assert!(normalize_policy("unknown").is_err());
    }
}
