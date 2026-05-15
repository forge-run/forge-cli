//! `forge sso` — federated workspace access via the portal.
//!
//! Implements the operator-driven half of ADR 0021:
//!
//!     forge sso connect <workspace_id>
//!
//! The CLI hits the portal's `/api/v1/auth/sso/issue` with
//! `mint_tier="federated"`, follows the returned `target_url` to the
//! target workspace's `/api/v1/auth/sso/consume`, and prints the
//! resulting `fr_f_*` access token + expiry. The operator can then
//! attach the bearer to subsequent `forge --token … --base-url …`
//! calls.
//!
//! This is the explicit, operator-visible form of the dance. The
//! transparent 401-retry interceptor (which would do this
//! automatically inside `client::ForgeClient`) is the next step —
//! see the comment block at the bottom of this file. Shipping the
//! explicit command first means operators can verify the federated
//! path works end-to-end without depending on the implicit retry
//! logic.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Debug, Subcommand)]
pub enum SsoCmd {
    /// Mint a federated bearer for a target workspace via the
    /// portal's SSO mint/consume path. Requires that the active
    /// profile already holds a portal bearer (from `forge login`
    /// against `app.forge.run`).
    Connect(ConnectArgs),
}

#[derive(Debug, clap::Args)]
pub struct ConnectArgs {
    /// Target workspace id (e.g. `ws-30bb36de`).
    pub workspace_id: String,
    /// Portal base URL. Defaults to the active profile's
    /// `base_url` from `~/.forge/config.toml` (set by `forge login`
    /// against the portal).
    #[arg(long, env = "FORGE_PORTAL_URL")]
    pub portal_url: Option<String>,
    /// Target workspace base URL. Defaults to
    /// `https://<workspace_id>.forge.run` — overridable for local
    /// dev where the workspace isn't behind the public edge.
    #[arg(long)]
    pub workspace_url: Option<String>,
    /// Emit just the bearer on stdout (suitable for shell-script
    /// capture: `BEARER=$(forge sso connect ws-… --raw)`). Without
    /// this flag, prints a human-readable banner with the bearer +
    /// expiry.
    #[arg(long, default_value_t = false)]
    pub raw: bool,
}

pub async fn run(
    cmd: SsoCmd,
    cli_base_url: Option<String>,
    cli_token: Option<String>,
    cli_profile: Option<String>,
) -> Result<()> {
    match cmd {
        SsoCmd::Connect(args) => connect(args, cli_base_url, cli_token, cli_profile).await,
    }
}

#[derive(Debug, Serialize)]
struct IssueBody<'a> {
    target_ws: &'a str,
    mint_tier: &'a str,
}

#[derive(Debug, Deserialize)]
struct IssueResponse {
    #[allow(dead_code)] // diagnostic; we follow target_url directly
    token: String,
    target_url: String,
}

#[derive(Debug, Deserialize)]
struct ConsumeResponse {
    access_token: String,
    access_expires_at: String,
}

async fn connect(
    args: ConnectArgs,
    cli_base_url: Option<String>,
    cli_token: Option<String>,
    cli_profile: Option<String>,
) -> Result<()> {
    // The portal session lives under the active profile (whatever
    // the operator's `forge login` wrote). Re-use the existing
    // config resolver so the precedence order (flag > env > profile)
    // stays consistent across the CLI.
    let portal_url = args.portal_url.or(cli_base_url.clone());
    let cfg = config::resolve(portal_url, cli_token, cli_profile)
        .context("resolving portal session for SSO mint")?;
    let portal_base = cfg.base_url.trim_end_matches('/').to_string();
    let portal_bearer = cfg.token;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("build http client")?;

    // 1. Mint via portal. `mint_tier=federated` makes the consume
    //    side route to the federated branch (no _users row, no
    //    cookie, JSON response).
    let issue_url = format!("{portal_base}/api/v1/auth/sso/issue");
    let issue_resp = client
        .post(&issue_url)
        .bearer_auth(&portal_bearer)
        // Match the portal's CSRF gate — same-origin Origin header
        // is what the SSO issue handler checks (auth_routes::csrf_check).
        .header("origin", &portal_base)
        .json(&IssueBody {
            target_ws: &args.workspace_id,
            mint_tier: "federated",
        })
        .send()
        .await
        .with_context(|| format!("POST {issue_url}"))?;
    if !issue_resp.status().is_success() {
        let status = issue_resp.status();
        let body = issue_resp.text().await.unwrap_or_default();
        bail!("portal SSO issue returned {status}: {body}");
    }
    let issued: IssueResponse = issue_resp
        .json()
        .await
        .context("decode portal SSO issue response")?;

    // 2. The target_url the portal returns points at
    //    `https://<workspace>.forge.run/api/v1/auth/sso/consume?token=…`.
    //    For local-dev / private-network setups the operator can
    //    override the host portion via --workspace-url; we keep
    //    the query string verbatim either way.
    let target_url = if let Some(override_host) = args.workspace_url.as_deref() {
        rewrite_host(&issued.target_url, override_host.trim_end_matches('/'))
            .context("rewrite target_url host")?
    } else {
        issued.target_url.clone()
    };

    // 3. Consume on the target. The federated branch returns JSON
    //    (not a 302 redirect), so we follow with a plain GET and
    //    decode the body.
    let consume_resp = client
        .get(&target_url)
        .send()
        .await
        .with_context(|| format!("GET {target_url}"))?;
    let status = consume_resp.status();
    if status == StatusCode::FOUND || status == StatusCode::SEE_OTHER {
        bail!(
            "target workspace returned a redirect from /sso/consume — \
             this usually means the target's runtime_config.federated_mint_enabled \
             is false (so the portal accepted the federated mint request but \
             the workspace is configured to reject it). Set federated_mint_enabled=true \
             on the target's envelope and retry."
        );
    }
    if !status.is_success() {
        let body = consume_resp.text().await.unwrap_or_default();
        bail!("target SSO consume returned {status}: {body}");
    }
    let consumed: ConsumeResponse = consume_resp
        .json()
        .await
        .context("decode federated consume response")?;

    if args.raw {
        // Stdout: just the bearer. Stderr stays human-friendly.
        println!("{}", consumed.access_token);
        eprintln!(
            "federated bearer minted for {} (expires {})",
            args.workspace_id, consumed.access_expires_at,
        );
    } else {
        println!("Federated bearer for {}:", args.workspace_id);
        println!("  Bearer:     {}", consumed.access_token);
        println!("  Expires at: {}", consumed.access_expires_at);
        println!(
            "\nUsage (env):\n  FORGE_TOKEN={} forge --base-url {} <cmd>",
            consumed.access_token,
            args.workspace_url
                .as_deref()
                .unwrap_or(&format!("https://{}.forge.run", args.workspace_id)),
        );
    }
    Ok(())
}

/// Replace the scheme + host of an absolute URL with `new_base`.
/// Used by --workspace-url to redirect the consume hop at a
/// non-public-edge endpoint (local dev, private-network probe).
/// Preserves the path + query verbatim.
fn rewrite_host(absolute_url: &str, new_base: &str) -> Result<String> {
    // Parse out the path+query suffix. The portal always emits an
    // absolute URL with `/api/v1/auth/sso/consume?token=…`, so we
    // find that prefix and join with new_base.
    let path_idx = absolute_url
        .find("/api/v1/auth/sso/consume")
        .ok_or_else(|| anyhow::anyhow!("target_url does not contain `/api/v1/auth/sso/consume`"))?;
    Ok(format!("{}{}", new_base, &absolute_url[path_idx..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_host_preserves_path_and_query() {
        let original = "https://ws-30bb36de.forge.run/api/v1/auth/sso/consume?token=abc.def";
        let rewritten = rewrite_host(original, "http://10.0.0.3:40001").unwrap();
        assert_eq!(
            rewritten,
            "http://10.0.0.3:40001/api/v1/auth/sso/consume?token=abc.def"
        );
    }

    #[test]
    fn rewrite_host_rejects_url_without_consume_path() {
        assert!(rewrite_host("https://example.com/", "http://10.0.0.3").is_err());
    }
}

// ── Follow-ups (intentionally not in this commit) ──────────────────
//
// 1. Transparent 401-retry interceptor inside `client::ForgeClient`.
//    On a 401 against the workspace, look up the active workspace
//    in `~/.forge/config.toml`'s `[current]` section, run the same
//    mint→consume dance against the configured `[portal]`, cache
//    the resulting `fr_f_*` under `[cache.workspaces.<id>]`, retry
//    the original request once. Second 401 propagates. Bumps the
//    config shape to:
//
//        [portal]
//        base_url   = "https://app.forge.run"
//        bearer     = "fr_u_…"
//        refresh_token = "fr_s_…"
//        email      = "ops@forge.run"
//
//        [current]
//        tenant_id    = "0b8971ba-…"
//        workspace_id = "ws-30bb36de"
//
//        [cache.workspaces."ws-30bb36de"]
//        bearer      = "fr_f_…"
//        expires_at  = "2026-05-16T08:00:00Z"
//        api_url     = "https://ws-30bb36de.forge.run"
//
// 2. `forge tenant list` / `forge tenant use <id>` — calls portal
//    `/api/v1/data/tenants` (the portal user's tenant_memberships
//    drive visibility) and writes `[current].tenant_id` on `use`.
//
// 3. `forge ws list` / `forge ws use <id>` — calls portal
//    `/api/v1/cp-admin/workspaces` filtered to the current tenant
//    and writes `[current].workspace_id` on `use`.
//
// 4. Old-config detection — when the new client encounters a
//    profile without `[portal]`, print a clear "your config is
//    pre-federated; please re-run `forge login`" and exit
//    non-zero. Do NOT auto-rewrite — multi-profile configs would
//    lose state.
