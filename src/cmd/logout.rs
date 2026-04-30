//! `forge logout` — revoke the saved bearer + refresh token and
//! clear the local profile.
//!
//! Calls the workspace's `POST /api/v1/auth/logout` with the
//! saved access token (and refresh token via X-Forge-Refresh
//! header so both get revoked server-side). Whether or not the
//! server call succeeds, the local config entry is removed.
//! Storage's logout is idempotent — a token that's already
//! revoked is a no-op — so re-running logout from a different
//! machine doesn't error.

use anyhow::{Context, Result};
use clap::Args;

use crate::config::{ResolvedConfig, clear_profile_token, read_profile_token};

#[derive(Debug, Args)]
pub struct LogoutArgs {
    /// Don't call the workspace; just clear the local profile.
    /// Useful when the workspace is unreachable but you want a
    /// clean slate.
    #[arg(long)]
    local_only: bool,
}

pub async fn run(
    args: LogoutArgs,
    cli_base_url: Option<String>,
    cli_token: Option<String>,
    cli_profile: Option<String>,
) -> Result<()> {
    let profile = cli_profile.clone().unwrap_or_else(|| "default".to_string());
    let saved = read_profile_token(&profile)?;

    let (base_url, access_token, refresh_token) = match saved {
        Some(s) => (
            cli_base_url.unwrap_or(s.base_url),
            cli_token.unwrap_or(s.access_token),
            s.refresh_token,
        ),
        None => {
            // Nothing to revoke locally; fall back to whatever the
            // CLI flags provide. If the user has nothing at all,
            // we treat it as a no-op success — there's no token to
            // revoke and no profile to clear.
            let base = match cli_base_url {
                Some(b) => b,
                None => {
                    eprintln!("Already logged out (no saved profile, no --base-url).");
                    return Ok(());
                }
            };
            let token = match cli_token {
                Some(t) => t,
                None => {
                    eprintln!("Already logged out (no saved token).");
                    return Ok(());
                }
            };
            (base, token, None)
        }
    };

    if !args.local_only {
        // Best-effort revocation. Network errors don't block the
        // local clear; the worst case is the server token sits
        // around until its TTL expires.
        let cfg = ResolvedConfig {
            base_url: base_url.clone(),
            token: access_token.clone(),
        };
        let client = crate::client::ForgeClient::new(cfg)?;
        match call_logout(&client, refresh_token.as_deref()).await {
            Ok(()) => eprintln!("server-side tokens revoked"),
            Err(e) => eprintln!("warning: server-side revocation failed: {e:#}"),
        }
    }

    clear_profile_token(&profile).context("clearing profile from ~/.forge/config.toml")?;
    eprintln!("logged out (profile: {profile})");
    Ok(())
}

async fn call_logout(
    client: &crate::client::ForgeClient,
    refresh_token: Option<&str>,
) -> Result<()> {
    // logout returns 200 + `{}`; we don't care about the body.
    // Pass the refresh token as X-Forge-Refresh so storage revokes
    // both halves of the pair, not just the access half.
    client
        .post_with_optional_header("/api/v1/auth/logout", "x-forge-refresh", refresh_token)
        .await
        .context("POST /api/v1/auth/logout")?;
    Ok(())
}
