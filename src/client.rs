//! HTTP client for the workspace's `forge-runtime`.
//!
//! Owns a single `reqwest::Client` configured with a sensible timeout
//! and bearer auth. Every CLI command reaches the runtime through one
//! of `get_json` / `post_json` / `post_with_optional_header` here so
//! the auth + URL shape lives in exactly one place.
//!
//! **Transparent re-auth on 401.** When a portal session is configured
//! (`config::ResolvedConfig.portal`) and an active workspace is
//! selected (`config::ResolvedConfig.workspace_id`), the client
//! transparently re-mints a federated bearer on 401 and retries once:
//!
//!   ┌─ portal /api/v1/auth/sso/issue  (mint_tier=federated)
//!   │
//!   ├─ target /api/v1/auth/sso/consume   → fresh fr_f_*
//!   │
//!   └─ cache the bearer + retry the original request
//!
//! The cached bearer + the active workspace's `api_url` are written
//! to `~/.forge/config.toml`'s `[cache.workspaces.<id>]` section so a
//! later CLI invocation reuses it without going back to the portal.
//! On the second 401 in a row (re-minted bearer also rejected), the
//! error propagates — usually means the portal/workspace
//! configuration drifted (kill switch flipped, target's
//! trusted_sso_issuers reordered, etc.) and the operator needs to
//! see it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::{Client, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::config::{self, PortalSession, ResolvedConfig};

#[derive(Clone)]
pub struct ForgeClient {
    inner: Client,
    /// Wrapped in `Arc<Mutex<…>>` so the 401-retry interceptor can
    /// swap the live bearer + base URL in place when it re-mints.
    /// Reads on the hot path take the mutex briefly to copy a
    /// String/url out — no awaiting under lock, no contention in
    /// practice (CLI is sequential).
    state: Arc<Mutex<ClientState>>,
}

#[derive(Debug)]
struct ClientState {
    base_url: String,
    token: String,
    portal: Option<PortalSession>,
    workspace_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    /// Network / TLS / connect failure. Not the runtime's fault — try
    /// again, check VPN, check the URL.
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
    /// Runtime returned a 4xx/5xx with a body. Body is preserved for
    /// the caller to surface.
    #[error("{status}: {body}")]
    Http { status: StatusCode, body: String },
    /// Response body wasn't valid JSON in the expected shape.
    #[error("decode response: {0}")]
    Decode(#[from] serde_json::Error),
    /// 401-retry was attempted but the re-minted bearer was also
    /// rejected. The inner error from the second attempt is
    /// surfaced as-is.
    #[error("federated re-mint succeeded but the retried request also failed: {inner}")]
    RetryFailed { inner: Box<ForgeError> },
    /// 401-retry was attempted but the federated mint itself
    /// failed (portal rejected, target's kill switch off, etc.).
    /// The inner mint error is preserved.
    #[error("federated re-mint failed: {0}")]
    MintFailed(String),
}

impl ForgeClient {
    /// Workspace base URL with no trailing slash. Exposed so
    /// long-lived streaming consumers (e.g. `forge logs --follow`)
    /// can build their own no-timeout client without going
    /// through this client's request-level helpers.
    ///
    /// Reads through the mutex; the value reflects the most recent
    /// 401-retry's resolved cache entry (which may have a different
    /// `api_url` than the one initially configured if the cache
    /// changed mid-session). Streaming callers should re-read this
    /// per stream start, not stash it across long-lived connections.
    pub fn base_url(&self) -> String {
        self.state.lock().unwrap().base_url.clone()
    }

    /// Bearer token for the workspace. Exposed for the same
    /// streaming-consumer reason as [`base_url`]. Never log this.
    pub fn token(&self) -> String {
        self.state.lock().unwrap().token.clone()
    }

    pub fn new(cfg: ResolvedConfig) -> Result<Self> {
        let inner = Client::builder()
            // A generous ceiling, not a target: most calls return in
            // milliseconds. A whole-workspace deploy (many domain/capability
            // wasm services + a large app bundle) is applied + validated
            // server-side in one request and can legitimately take tens of
            // seconds; the old 30s ceiling aborted the client mid-apply (the
            // server still committed, but the CLI reported a transport
            // timeout). 300s covers a cold multi-module workspace deploy.
            .timeout(Duration::from_secs(300))
            .user_agent(concat!("forge-cli/", env!("CARGO_PKG_VERSION")))
            // The federated consume path returns a JSON body; the
            // legacy consume returns a 302. We follow neither
            // automatically — both branches are handled explicitly
            // by the 401-retry interceptor below.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            inner,
            state: Arc::new(Mutex::new(ClientState {
                base_url: cfg.base_url.trim_end_matches('/').to_string(),
                token: cfg.token,
                portal: cfg.portal,
                workspace_id: cfg.workspace_id,
            })),
        })
    }

    /// `POST <base>/<path>` with a JSON body, decoded as `R`.
    pub async fn post_json<B: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<R, ForgeError> {
        let body_bytes = serde_json::to_vec(body)?;
        let bytes = self
            .do_request_with_retry(Method::Post, path, Some(&body_bytes), &[])
            .await?;
        let parsed: R = serde_json::from_slice(&bytes)?;
        Ok(parsed)
    }

    /// `POST <base>/<path>` with an optional header. No JSON body.
    /// Used by `forge logout` to send `X-Forge-Refresh: <token>`
    /// when the saved profile has a refresh token.
    pub async fn post_with_optional_header(
        &self,
        path: &str,
        header_name: &str,
        header_value: Option<&str>,
    ) -> Result<(), ForgeError> {
        let extra: Vec<(String, String)> = match header_value {
            Some(v) => vec![(header_name.to_string(), v.to_string())],
            None => vec![],
        };
        let _ = self
            .do_request_with_retry(Method::Post, path, None, &extra)
            .await?;
        Ok(())
    }

    /// `GET <base>/<path>`, decoded as `R`.
    pub async fn get_json<R: DeserializeOwned>(&self, path: &str) -> Result<R, ForgeError> {
        let bytes = self
            .do_request_with_retry(Method::Get, path, None, &[])
            .await?;
        let parsed: R = serde_json::from_slice(&bytes)?;
        Ok(parsed)
    }

    /// `HEAD <base>/<path>`; returns the HTTP status WITHOUT treating a
    /// non-2xx (e.g. 404) as an error, so a caller can branch on presence.
    /// Used by the W3 artifact-upload stage: 200 → already stored (dedup),
    /// 404 → needs a PUT, 503 → store unavailable. Transparently re-auths
    /// once on a 401 like every other request.
    pub async fn head_status(&self, path: &str) -> Result<StatusCode, ForgeError> {
        let resp = self
            .send_with_retry(Method::Head, path, None, "application/json", &[])
            .await?;
        Ok(resp.status())
    }

    /// `PUT <base>/<path>` with raw bytes under an explicit content type
    /// (content-addressed artifact upload — NOT JSON). Returns the response
    /// body on success, `ForgeError::Http` on 4xx/5xx.
    pub async fn put_bytes(
        &self,
        path: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<Vec<u8>, ForgeError> {
        let resp = self
            .send_with_retry(Method::Put, path, Some(bytes), content_type, &[])
            .await?;
        finish(resp).await
    }

    /// Single-shot request with the current bearer. Returns the
    /// raw response body on success, ForgeError::Http on
    /// 4xx/5xx. Used internally by the retry interceptor; doesn't
    /// itself retry.
    async fn do_request_once(
        &self,
        method: Method,
        path: &str,
        body_bytes: Option<&[u8]>,
        content_type: &str,
        extra_headers: &[(String, String)],
    ) -> Result<reqwest::Response, ForgeError> {
        let (base, token) = {
            let s = self.state.lock().unwrap();
            (s.base_url.clone(), s.token.clone())
        };
        let url = format!("{base}{path}");
        let mut req = match method {
            Method::Get => self.inner.get(&url),
            Method::Post => self.inner.post(&url),
            Method::Put => self.inner.put(&url),
            Method::Head => self.inner.head(&url),
        };
        if !token.is_empty() {
            req = req.bearer_auth(&token);
        }
        if let Some(b) = body_bytes {
            req = req.header("content-type", content_type).body(b.to_vec());
        }
        for (n, v) in extra_headers {
            req = req.header(n.as_str(), v.as_str());
        }
        Ok(req.send().await?)
    }

    /// Core 401-retry interceptor: sends the request, and on a 401
    /// transparently re-mints a federated bearer (when a portal session +
    /// active workspace are configured) and retries once. Returns the raw
    /// `reqwest::Response` — JSON callers go through [`do_request_with_retry`]
    /// (which adds `finish`), the artifact HEAD reads the status directly.
    async fn send_with_retry(
        &self,
        method: Method,
        path: &str,
        body_bytes: Option<&[u8]>,
        content_type: &str,
        extra_headers: &[(String, String)],
    ) -> Result<reqwest::Response, ForgeError> {
        let resp = self
            .do_request_once(method, path, body_bytes, content_type, extra_headers)
            .await?;
        if resp.status() != StatusCode::UNAUTHORIZED {
            return Ok(resp);
        }
        // 401 — try the federated re-mint if the portal session is
        // available + an active workspace is set. Without both we
        // fall through to surfacing the 401 to the caller.
        let (portal, workspace_id) = {
            let s = self.state.lock().unwrap();
            (s.portal.clone(), s.workspace_id.clone())
        };
        let Some(portal) = portal else {
            return Err(consume_to_http_err(resp).await);
        };
        let Some(workspace_id) = workspace_id else {
            return Err(consume_to_http_err(resp).await);
        };
        // Drop the 401 response body so the connection returns to
        // the pool — we're about to make several more requests.
        drop(resp);
        match self.remint_federated(&portal, &workspace_id).await {
            Ok(()) => {}
            Err(e) => return Err(ForgeError::MintFailed(e)),
        }
        // Retry once.
        let resp = self
            .do_request_once(method, path, body_bytes, content_type, extra_headers)
            .await?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            return Err(ForgeError::RetryFailed {
                inner: Box::new(consume_to_http_err(resp).await),
            });
        }
        Ok(resp)
    }

    /// Request with the 401-retry interceptor wrapped around it.
    /// Returns the response body bytes; callers decode as needed.
    async fn do_request_with_retry(
        &self,
        method: Method,
        path: &str,
        body_bytes: Option<&[u8]>,
        extra_headers: &[(String, String)],
    ) -> Result<Vec<u8>, ForgeError> {
        let resp = self
            .send_with_retry(method, path, body_bytes, "application/json", extra_headers)
            .await?;
        finish(resp).await
    }

    /// Drive the federated mint→consume dance against the portal
    /// and target workspace, swap the resulting bearer into the
    /// client state, and write through to the on-disk cache.
    async fn remint_federated(
        &self,
        portal: &PortalSession,
        workspace_id: &str,
    ) -> Result<(), String> {
        let portal_base = portal.base_url.trim_end_matches('/');
        // 1. Issue.
        let issue_url = format!("{portal_base}/api/v1/auth/sso/issue");
        let issue_resp = self
            .inner
            .post(&issue_url)
            .bearer_auth(&portal.bearer)
            .header("origin", portal_base)
            .header("content-type", "application/json")
            .body(
                serde_json::to_vec(&serde_json::json!({
                    "target_ws": workspace_id,
                    "mint_tier": "federated",
                }))
                .map_err(|e| format!("encode issue body: {e}"))?,
            )
            .send()
            .await
            .map_err(|e| format!("POST {issue_url}: {e}"))?;
        if !issue_resp.status().is_success() {
            let status = issue_resp.status();
            let body = issue_resp.text().await.unwrap_or_default();
            return Err(format!("portal /sso/issue {status}: {body}"));
        }
        let issued: IssueResponse = issue_resp
            .json()
            .await
            .map_err(|e| format!("decode issue response: {e}"))?;

        // 2. Consume. Federated branch returns a JSON body; the
        //    legacy branch returns a 302 redirect. We disabled the
        //    redirect-follow policy in the constructor so a
        //    legacy-branch response shows up here as a 302 we can
        //    detect and surface with a clear error.
        let consume_resp = self
            .inner
            .get(&issued.target_url)
            .send()
            .await
            .map_err(|e| format!("GET {}: {}", issued.target_url, e))?;
        let status = consume_resp.status();
        if status == StatusCode::FOUND || status == StatusCode::SEE_OTHER {
            return Err(format!(
                "target returned {status} from /sso/consume — usually means \
                 federated_mint_enabled=false in the target's envelope. \
                 Flip it true and retry.",
            ));
        }
        if !status.is_success() {
            let body = consume_resp.text().await.unwrap_or_default();
            return Err(format!("/sso/consume {status}: {body}"));
        }
        let consumed: ConsumeResponse = consume_resp
            .json()
            .await
            .map_err(|e| format!("decode consume response: {e}"))?;

        // 3. Update in-memory state + on-disk cache. Cache entry's
        //    api_url is derived from the issued target_url so a
        //    custom edge / private deploy keeps working.
        let api_url = derive_api_url_from_target(&issued.target_url, workspace_id);
        {
            let mut s = self.state.lock().unwrap();
            s.base_url = api_url.clone();
            s.token = consumed.access_token.clone();
        }
        config::cache_workspace_bearer(
            workspace_id,
            &api_url,
            &consumed.access_token,
            &consumed.access_expires_at,
        )
        .map_err(|e| format!("write cache: {e:#}"))?;
        Ok(())
    }
}

#[derive(Copy, Clone)]
enum Method {
    Get,
    Post,
    Put,
    Head,
}

#[derive(Debug, serde::Deserialize)]
struct IssueResponse {
    target_url: String,
}

#[derive(Debug, serde::Deserialize)]
struct ConsumeResponse {
    access_token: String,
    access_expires_at: String,
}

async fn finish(resp: reqwest::Response) -> Result<Vec<u8>, ForgeError> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(ForgeError::Http { status, body });
    }
    let bytes = resp.bytes().await?;
    Ok(bytes.to_vec())
}

async fn consume_to_http_err(resp: reqwest::Response) -> ForgeError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    ForgeError::Http { status, body }
}

/// Pull the base URL from the issued consume URL so a custom-host
/// edge / private deploy survives. The portal emits absolute URLs
/// like `https://ws-XXXX.forge.run/api/v1/auth/sso/consume?token=…`;
/// we strip the path back to the scheme+host. Falls back to the
/// conventional `https://<wid>.forge.run` if the URL is malformed.
fn derive_api_url_from_target(target_url: &str, workspace_id: &str) -> String {
    if let Some(path_idx) = target_url.find("/api/v1/auth/sso/consume") {
        target_url[..path_idx].trim_end_matches('/').to_string()
    } else {
        format!("https://{workspace_id}.forge.run")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_api_url_strips_path() {
        let url = "https://ws-XXXXXXXX.forge.run/api/v1/auth/sso/consume?token=abc.def";
        assert_eq!(
            derive_api_url_from_target(url, "ws-XXXXXXXX"),
            "https://ws-XXXXXXXX.forge.run"
        );
    }

    #[test]
    fn derive_api_url_handles_private_host() {
        let url = "http://127.0.0.1:40001/api/v1/auth/sso/consume?token=…";
        assert_eq!(
            derive_api_url_from_target(url, "ws-XXXXXXXX"),
            "http://127.0.0.1:40001"
        );
    }

    #[test]
    fn derive_api_url_falls_back_on_malformed_target() {
        assert_eq!(
            derive_api_url_from_target("not-a-url", "ws-XXXXXXXX"),
            "https://ws-XXXXXXXX.forge.run"
        );
    }
}
