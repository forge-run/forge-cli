//! HTTP client for the workspace's `forge-runtime`.
//!
//! Owns a single `reqwest::Client` configured with a sensible timeout
//! and bearer auth. Every CLI command reaches the runtime through one
//! of `get` / `post` / `post_json` here so the auth + URL shape lives
//! in exactly one place.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::{Client, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::config::ResolvedConfig;

#[derive(Clone)]
pub struct ForgeClient {
    inner: Client,
    base_url: String,
    token: Arc<String>,
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
}

impl ForgeClient {
    pub fn new(cfg: ResolvedConfig) -> Result<Self> {
        let inner = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("forge-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            inner,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            token: Arc::new(cfg.token),
        })
    }

    /// `POST <base>/<path>` with a JSON body, decoded as `R`.
    pub async fn post_json<B: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<R, ForgeError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .inner
            .post(&url)
            .bearer_auth(self.token.as_str())
            .json(body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ForgeError::Http { status, body });
        }
        let body_bytes = resp.bytes().await?;
        let parsed: R = serde_json::from_slice(&body_bytes)?;
        Ok(parsed)
    }

    /// `GET <base>/<path>`, decoded as `R`.
    #[allow(dead_code)] // wired in once `tokens list` lands
    pub async fn get_json<R: DeserializeOwned>(&self, path: &str) -> Result<R, ForgeError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .inner
            .get(&url)
            .bearer_auth(self.token.as_str())
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ForgeError::Http { status, body });
        }
        let body_bytes = resp.bytes().await?;
        let parsed: R = serde_json::from_slice(&body_bytes)?;
        Ok(parsed)
    }
}
