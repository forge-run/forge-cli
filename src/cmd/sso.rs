//! `forge sso` — federated workspace access via the portal.
//!
//! Implements the operator-visible half of ADR 0021. Two paths:
//!
//!     forge sso login                 — device flow against the portal
//!     forge sso connect <workspace>   — mint a federated bearer for a workspace
//!
//! `login` writes `[portal]` to `~/.forge/config.toml`; `connect`
//! reads it, drives `/api/v1/auth/sso/issue` (mint_tier=federated)
//! and follows through to the target workspace's
//! `/api/v1/auth/sso/consume`. The resulting `fr_f_*` bearer +
//! expiry is printed for explicit operator use; the same dance
//! also fires automatically inside `client::ForgeClient` on a 401
//! when an active workspace is selected.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Debug, Subcommand)]
pub enum SsoCmd {
    /// Run the portal-side device flow and save the resulting
    /// portal session to `[portal]` in `~/.forge/config.toml`.
    /// Operators run this once per laptop; all subsequent
    /// federated mints reuse the saved bearer.
    Login(LoginArgs),
    /// Mint a federated bearer for a target workspace via the
    /// portal's SSO mint/consume path. Requires `[portal]` to be
    /// configured (i.e. `forge sso login` was already run).
    Connect(ConnectArgs),
}

#[derive(Debug, clap::Args)]
pub struct LoginArgs {
    /// Portal base URL. Falls back to the global `--portal-url`
    /// flag, `FORGE_PORTAL_URL`, and finally
    /// `https://app.forge.run`. Subcommand-local override exists
    /// so an operator can briefly point `forge sso login` at a
    /// portal fixture (e.g. for testing) without exporting an
    /// env var that affects other shells.
    #[arg(long)]
    pub portal_url: Option<String>,

    /// Don't auto-open the device-flow verification URL in the
    /// operator's default browser. Useful on headless boxes / CI
    /// runners where opening a browser would either fail or
    /// surprise. The URL + user_code are always printed; this
    /// flag just skips the `open(url)` attempt.
    #[arg(long, default_value_t = false)]
    pub no_browser: bool,
}

#[derive(Debug, clap::Args)]
pub struct ConnectArgs {
    /// Target workspace id (e.g. `ws-30bb36de`).
    pub workspace_id: String,
    /// Target workspace base URL. Defaults to
    /// `https://<workspace_id>.forge.run` — overridable for local
    /// dev where the workspace isn't behind the public edge.
    #[arg(long)]
    pub workspace_url: Option<String>,
    /// Emit just the bearer on stdout (suitable for shell-script
    /// capture: `BEARER=$(forge sso connect ws-… --raw)`).
    #[arg(long, default_value_t = false)]
    pub raw: bool,
}

pub async fn run(cmd: SsoCmd, cli_portal_url: Option<String>) -> Result<()> {
    match cmd {
        SsoCmd::Login(args) => login(args, cli_portal_url).await,
        SsoCmd::Connect(args) => connect(args, cli_portal_url).await,
    }
}

#[derive(Debug, Serialize)]
struct IssueBody<'a> {
    target_ws: &'a str,
    mint_tier: &'a str,
}

#[derive(Debug, Deserialize)]
struct IssueResponse {
    target_url: String,
}

#[derive(Debug, Deserialize)]
struct ConsumeResponse {
    access_token: String,
    access_expires_at: String,
}

async fn login(args: LoginArgs, cli_portal_url: Option<String>) -> Result<()> {
    let portal_url = args
        .portal_url
        .or(cli_portal_url)
        .unwrap_or_else(|| "https://app.forge.run".to_string());
    let portal_base = portal_url.trim_end_matches('/');
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("build http client")?;

    // Mint a device grant. The runtime's device-flow endpoints are
    // /api/v1/auth/device/{authorize,token}; same shape every
    // workspace serves, but we hit the portal specifically.
    #[derive(Deserialize)]
    struct AuthorizeResp {
        device_code: String,
        user_code: String,
        verification_uri: String,
        expires_in: u64,
        interval: u64,
    }
    let authorize_url = format!("{portal_base}/api/v1/auth/device/authorize");
    let resp = client
        .post(&authorize_url)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .with_context(|| format!("POST {authorize_url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("device authorize {status}: {body}");
    }
    let grant: AuthorizeResp = resp.json().await.context("decode authorize response")?;
    eprintln!(
        "\nOpen this URL in your browser and enter the code:\n  {}\n  code: {}\n",
        grant.verification_uri, grant.user_code,
    );
    if !args.no_browser {
        // Best-effort. `webbrowser::open` returns Err on headless
        // boxes / no DISPLAY / no registered handler; the printed
        // URL+code above is the always-works fallback either way,
        // so a failure here is a soft warning, not a hard error.
        match webbrowser::open(&grant.verification_uri) {
            Ok(()) => eprintln!("(opened in your default browser)"),
            Err(e) => eprintln!(
                "(couldn't open browser automatically — open the URL manually: {e})",
            ),
        }
    }
    eprintln!(
        "Waiting for approval (expires in {}s, polling every {}s)…",
        grant.expires_in, grant.interval,
    );

    let token_url = format!("{portal_base}/api/v1/auth/device/token");
    let interval = Duration::from_secs(grant.interval.max(1));
    let deadline = std::time::Instant::now() + Duration::from_secs(grant.expires_in);
    #[derive(Deserialize)]
    struct TokenResp {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        user_id: Option<String>,
    }
    let poll_body = serde_json::json!({"device_code": grant.device_code});
    loop {
        tokio::time::sleep(interval).await;
        if std::time::Instant::now() > deadline {
            bail!("device flow timed out without approval");
        }
        let poll = client
            .post(&token_url)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&poll_body)?)
            .send()
            .await
            .with_context(|| format!("POST {token_url}"))?;
        let status = poll.status();
        if status.is_success() {
            let token: TokenResp = poll.json().await.context("decode token response")?;
            config::save_portal_session(
                portal_base,
                &token.access_token,
                token.refresh_token.as_deref(),
                token.user_id.as_deref(),
            )?;
            eprintln!("portal session saved to ~/.forge/config.toml");
            return Ok(());
        }
        if status == StatusCode::BAD_REQUEST {
            // RFC 8628 — "authorization_pending" / "slow_down" /
            // "expired_token" / "access_denied". The runtime
            // emits these as 400s with a JSON body containing
            // {error}. authorization_pending = keep polling;
            // slow_down = back off; the rest are terminal.
            let body = poll.text().await.unwrap_or_default();
            #[derive(Deserialize)]
            struct DeviceErr {
                error: String,
            }
            if let Ok(e) = serde_json::from_str::<DeviceErr>(&body) {
                match e.error.as_str() {
                    "authorization_pending" => continue,
                    "slow_down" => {
                        // Back off another interval-worth before
                        // the next try. The forge-runtime device
                        // handler emits this when polling is too
                        // aggressive; honour it.
                        tokio::time::sleep(interval).await;
                        continue;
                    }
                    other => bail!("device flow rejected: {other}"),
                }
            }
            bail!("device flow returned 400 with unrecognised body: {body}");
        }
        let body = poll.text().await.unwrap_or_default();
        bail!("device flow returned {status}: {body}");
    }
}

async fn connect(args: ConnectArgs, cli_portal_url: Option<String>) -> Result<()> {
    let mut portal = config::read_portal_session()?.ok_or_else(no_portal_hint)?;
    // CLI override for ad-hoc portal fixtures. Doesn't touch the
    // saved config; the next invocation falls back to the on-disk
    // base_url unless the operator re-passes --portal-url.
    if let Some(override_url) = cli_portal_url {
        portal.base_url = override_url.trim_end_matches('/').to_string();
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("build http client")?;

    // 1. Mint via portal. mint_tier=federated routes the consume
    //    side to the federated branch (no _users row, no cookie,
    //    JSON response).
    let portal_base = portal.base_url.trim_end_matches('/');
    let issue_url = format!("{portal_base}/api/v1/auth/sso/issue");
    let issue_resp = client
        .post(&issue_url)
        .bearer_auth(&portal.bearer)
        // Same-origin Origin satisfies the issuer's CSRF gate
        // (auth_routes::csrf_check).
        .header("origin", portal_base)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&IssueBody {
            target_ws: &args.workspace_id,
            mint_tier: "federated",
        })?)
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

    // 2. Follow the target URL. --workspace-url overrides the host
    //    portion for local-dev / private-network deploys.
    let target_url = if let Some(override_host) = args.workspace_url.as_deref() {
        rewrite_host(&issued.target_url, override_host.trim_end_matches('/'))
            .context("rewrite target_url host")?
    } else {
        issued.target_url.clone()
    };

    let consume_resp = client
        .get(&target_url)
        .send()
        .await
        .with_context(|| format!("GET {target_url}"))?;
    let status = consume_resp.status();
    if status == StatusCode::FOUND || status == StatusCode::SEE_OTHER {
        bail!(
            "target returned {status} from /sso/consume — usually means \
             federated_mint_enabled=false in the target's envelope. Flip it true and retry."
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

    // 3. Cache the resulting bearer for future invocations so the
    //    client's 401-retry path can find it without going back to
    //    the portal. Derive api_url from the issued target_url so
    //    private-network overrides survive.
    let api_url = if let Some(path_idx) = target_url.find("/api/v1/auth/sso/consume") {
        target_url[..path_idx].trim_end_matches('/').to_string()
    } else {
        format!("https://{}.forge.run", args.workspace_id)
    };
    config::cache_workspace_bearer(
        &args.workspace_id,
        &api_url,
        &consumed.access_token,
        &consumed.access_expires_at,
    )?;

    if args.raw {
        println!("{}", consumed.access_token);
        eprintln!(
            "federated bearer minted for {} (expires {})",
            args.workspace_id, consumed.access_expires_at,
        );
    } else {
        println!("Federated bearer for {}:", args.workspace_id);
        println!("  Bearer:     {}", consumed.access_token);
        println!("  Expires at: {}", consumed.access_expires_at);
        println!("  API URL:    {api_url}");
        println!("\nCached to ~/.forge/config.toml — subsequent commands will reuse it.");
    }
    Ok(())
}

/// Error message for "no `[portal]` session in
/// ~/.forge/config.toml" — split out so the federated-only
/// subcommands (`forge tenant`, `forge ws`, `forge sso connect`)
/// surface the same hint. When the user is on an old (direct-mode-
/// only) config, the message includes a concrete migration step.
pub(crate) fn no_portal_hint() -> anyhow::Error {
    let legacy = config::is_legacy_config().unwrap_or(false);
    if legacy {
        anyhow::anyhow!(
            "no `[portal]` section in ~/.forge/config.toml — your config predates the federated \
             tier (ADR 0021). Existing `[profile.*]` entries are preserved; run \
             `forge sso login --portal-url https://app.forge.run` to add a portal session \
             alongside them.",
        )
    } else {
        anyhow::anyhow!(
            "no portal session — run `forge sso login --portal-url https://app.forge.run` first \
             (or `forge login --portal-url …`).",
        )
    }
}

fn rewrite_host(absolute_url: &str, new_base: &str) -> Result<String> {
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
