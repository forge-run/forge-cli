//! `forge login` — RFC 8628 device-flow authentication.
//!
//! Three-step flow:
//!
//! 1. POST `/api/v1/auth/device/authorize` to mint a grant.
//!    Workspace returns `(device_code, user_code, verification_uri,
//!    expires_in, interval)`.
//! 2. Print the verification URL + user code; tell the user to
//!    open the URL in a browser. The CLI never sees the password.
//! 3. Poll `/api/v1/auth/device/token` every `interval` seconds.
//!    On success the workspace returns the same `LoginOutput`
//!    shape that `/api/v1/auth/login` produces. Save the access
//!    token (and refresh token, when one comes back) into
//!    `~/.forge/config.toml` under the active profile.
//!
//! The login command does not consume the global `--token` flag —
//! login is what produces a token. It does honor `--base-url`,
//! `FORGE_BASE_URL`, and the per-profile `base_url` from
//! `~/.forge/config.toml`.

use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use crate::config::{resolve_for_login, save_profile_token};

/// Effective max wall-clock the CLI will block waiting for the
/// user to finish in the browser, regardless of what the server
/// says. Caps surprise — a buggy server that returns a huge
/// `expires_in` shouldn't tie up a terminal indefinitely.
const HARD_DEADLINE: Duration = Duration::from_secs(900);

/// Floor on the polling interval. Defends the workspace from a
/// runaway CLI hammering the token endpoint when the server's
/// hint is too aggressive (or zero).
const MIN_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Profile name to write the resulting token under. Defaults
    /// to the global `--profile`, then the config file's
    /// `default_profile`, then the literal `"default"`.
    /// Direct-mode only — federated mode (the default) keys off
    /// `[portal]` instead of named profiles.
    #[arg(long)]
    profile_override: Option<String>,

    /// Override the portal URL used for federated login. Falls
    /// back to the global `--portal-url` flag, `FORGE_PORTAL_URL`,
    /// and finally `https://app.forge.run`. Federated mode is the
    /// default; pass --base-url to opt into direct mode (the
    /// break-glass).
    #[arg(long)]
    portal_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthorizeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: i64,
    interval: i64,
}

#[derive(Debug, Deserialize)]
struct LoginOutput {
    access_token: String,
    #[serde(default)]
    access_expires_at: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
}

pub async fn run(
    args: LoginArgs,
    cli_base_url: Option<String>,
    cli_profile: Option<String>,
    cli_portal_url: Option<String>,
) -> Result<()> {
    // Federated mode is the default. Direct mode runs only when
    // the operator explicitly opts in with --base-url (the
    // break-glass for unreachable-portal scenarios). The common
    // case — `forge login` with no flags — delegates to
    // `forge sso login` against the saved or default portal URL.
    let portal_url = args.portal_url.clone().or(cli_portal_url);
    let want_direct_mode = cli_base_url.is_some() && portal_url.is_none();

    if !want_direct_mode {
        return crate::cmd::sso::run(
            crate::cmd::sso::SsoCmd::Login(crate::cmd::sso::LoginArgs {
                portal_url,
                no_browser: false,
            }),
            None,
        )
        .await;
    }

    // Direct mode (legacy / break-glass). The login flow has
    // its own profile handling because it can override where
    // the token lands without changing the global active-profile
    // semantics.
    //
    // Deprecation notice: direct-mode `forge login` remains
    // available as the break-glass when the portal is
    // unreachable, but every operator should be on `forge sso
    // login` for portal-mediated access. Print to stderr so
    // scripts that parse stdout keep working unchanged.
    eprintln!(
        "warning: direct-mode `forge login --base-url …` is deprecated. \
         The default `forge login` now delegates to `forge sso login` (portal-mediated \
         access). Pass --base-url only as a break-glass when the portal is unreachable."
    );
    let effective_profile = args.profile_override.clone().or(cli_profile.clone());
    let (base_url, profile) = resolve_for_login(cli_base_url, effective_profile)?;
    let base_url = base_url.trim_end_matches('/').to_string();

    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("forge-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("building HTTP client")?;

    // Step 1: kick off the grant.
    let auth = post_authorize(&client, &base_url).await?;

    // Step 2: tell the human what to do.
    eprintln!();
    eprintln!("Open {} in your browser.", auth.verification_uri);
    eprintln!("Enter this code: {}", auth.user_code);
    eprintln!();
    // Best-effort auto-open; the printed URL above is the
    // always-works fallback if it fails (headless / no browser).
    match webbrowser::open(&auth.verification_uri) {
        Ok(()) => eprintln!("(opened in your default browser)"),
        Err(e) => eprintln!("(couldn't open browser automatically — open manually: {e})"),
    }
    eprintln!("Waiting for approval...");

    // Step 3: poll until done, expired, or denied.
    let interval = Duration::from_secs(auth.interval.max(1) as u64).max(MIN_POLL_INTERVAL);
    let server_deadline = Duration::from_secs(auth.expires_in.max(1) as u64);
    let deadline = std::time::Instant::now() + server_deadline.min(HARD_DEADLINE);

    let outcome =
        poll_until_done(&client, &base_url, &auth.device_code, interval, deadline).await?;

    // Step 4: save token under the chosen profile.
    save_profile_token(
        &profile,
        &base_url,
        &outcome.access_token,
        outcome.refresh_token.as_deref(),
    )?;

    eprintln!();
    eprintln!("Logged in.");
    eprintln!("Profile:  {profile}");
    eprintln!("Workspace: {base_url}");
    if let Some(uid) = outcome.user_id {
        eprintln!("User:      {uid}");
    }
    if let Some(exp) = outcome.access_expires_at {
        eprintln!("Expires:   {exp}");
    }
    Ok(())
}

async fn post_authorize(client: &Client, base_url: &str) -> Result<AuthorizeResponse> {
    let url = format!("{base_url}/api/v1/auth/device/authorize");
    let resp = client
        .post(&url)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("device_authorize failed: {status}: {body}");
    }
    resp.json::<AuthorizeResponse>()
        .await
        .context("decode authorize response")
}

async fn poll_until_done(
    client: &Client,
    base_url: &str,
    device_code: &str,
    initial_interval: Duration,
    deadline: std::time::Instant,
) -> Result<LoginOutput> {
    let url = format!("{base_url}/api/v1/auth/device/token");
    let mut interval = initial_interval;

    loop {
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("login timed out — restart `forge login` to get a fresh code");
        }

        // Sleep first; the user just got the code, they need a
        // moment to switch tabs. `interval` is capped on the lower
        // end so a misconfigured server can't force us into a busy
        // loop.
        tokio::time::sleep(interval).await;

        let body = serde_json::json!({ "device_code": device_code });
        let resp = client.post(&url).json(&body).send().await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                // Transient network errors don't kill the flow —
                // we keep polling until the deadline.
                eprintln!("(transient: {e}; retrying)");
                continue;
            }
        };
        let status = resp.status();
        let body_bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("(transient body read: {e}; retrying)");
                continue;
            }
        };

        if status.is_success() {
            return serde_json::from_slice::<LoginOutput>(&body_bytes)
                .context("decode login output");
        }

        // RFC 8628 errors come back as 400 with
        // `{ error: "...", error_description: "..." }`.
        let err: PollError = serde_json::from_slice(&body_bytes).unwrap_or(PollError::default());

        match err.error.as_deref() {
            Some("authorization_pending") => {
                // Keep polling at the same interval.
            }
            Some("slow_down") => {
                // Server is asking us to back off. Bump by 5s.
                interval += Duration::from_secs(5);
            }
            Some("expired_token") => {
                anyhow::bail!("device code expired before approval — restart `forge login`",);
            }
            Some("access_denied") => {
                anyhow::bail!(
                    "{}",
                    err.error_description
                        .unwrap_or_else(|| "access denied".to_string()),
                );
            }
            Some(other) => {
                anyhow::bail!("unexpected error from /device/token: {other} ({status})",);
            }
            None if status == StatusCode::BAD_REQUEST => {
                // Some non-RFC body — surface as a generic error
                // so the user can see what came back.
                let body_str = String::from_utf8_lossy(&body_bytes);
                anyhow::bail!("device_token returned 400: {body_str}");
            }
            None => {
                anyhow::bail!(
                    "device_token returned {status}: {}",
                    String::from_utf8_lossy(&body_bytes),
                );
            }
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct PollError {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Tiny stub-server harness for the polling loop. Returns
    /// `pending` for the first N polls, then `approved` with a
    /// canned LoginOutput. Verifies the CLI saves the right
    /// token without spinning up a real forge-runtime.
    async fn stub_server(approve_after: usize) -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let counter = counter_clone.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let body = if req.contains("/device/authorize") {
                        r#"{"device_code":"DEV","user_code":"BCDF-GHJK","verification_uri":"https://example.test/device","expires_in":600,"interval":1}"#
                            .to_string()
                    } else if req.contains("/device/token") {
                        let n = counter.fetch_add(1, Ordering::Relaxed);
                        if n < approve_after {
                            // RFC 8628 pending response (400).
                            let payload = r#"{"error":"authorization_pending"}"#;
                            return write_response(
                                &mut sock,
                                "400 Bad Request",
                                "application/json",
                                payload,
                            )
                            .await;
                        } else {
                            r#"{"access_token":"FAKE-ACCESS","access_expires_at":"2099-01-01T00:00:00Z","refresh_token":"FAKE-REFRESH","user_id":"alice"}"#
                                .to_string()
                        }
                    } else {
                        return write_response(
                            &mut sock,
                            "404 Not Found",
                            "text/plain",
                            "not found",
                        )
                        .await;
                    };
                    let _ = write_response(&mut sock, "200 OK", "application/json", &body).await;

                    async fn write_response(
                        sock: &mut tokio::net::TcpStream,
                        status: &str,
                        ct: &str,
                        body: &str,
                    ) {
                        let resp = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len(),
                        );
                        let _ = sock.write_all(resp.as_bytes()).await;
                        let _ = sock.shutdown().await;
                    }
                });
            }
        });

        (format!("http://{addr}"), counter)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn polling_loop_succeeds_after_pending() {
        let (base, counter) = stub_server(2).await;
        let client = Client::new();

        // Authorize first to get the device code shape.
        let auth = post_authorize(&client, &base).await.unwrap();
        assert_eq!(auth.user_code, "BCDF-GHJK");
        assert_eq!(auth.device_code, "DEV");

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let out = poll_until_done(
            &client,
            &base,
            &auth.device_code,
            Duration::from_millis(50),
            deadline,
        )
        .await
        .unwrap();
        assert_eq!(out.access_token, "FAKE-ACCESS");
        assert_eq!(out.refresh_token.as_deref(), Some("FAKE-REFRESH"));
        // We should have polled at least 3 times (2 pendings + 1 approved).
        assert!(counter.load(Ordering::Relaxed) >= 3);
    }
}
