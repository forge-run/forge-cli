//! `forge tokens *` — wraps the workspace's `_platform::auth::*` ops.
//!
//! v0 ships `mint` only; `list` and `revoke` follow once the surface
//! shape is shaken down against a real workspace. The wire is:
//!
//! - `POST /api/v1/platform/auth/mint`  → `MintRequest` → `MintResponse`
//! - `GET  /api/v1/platform/auth/tokens` → `ListResponse`
//! - `POST /api/v1/platform/auth/revoke` → `RevokeRequest` → `RevokeResponse`
//!
//! Every op is System-tier, so the bearer token must be admin-tier
//! (the bootstrap admin token, or anything minted with `tier=admin`).

use anyhow::Result;
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use crate::client::{ForgeClient, ForgeError};

#[derive(Debug, Subcommand)]
pub enum TokensCmd {
    /// Mint a new opaque bearer token. The raw token is printed once
    /// to stdout — capture it in your `.env` immediately; the platform
    /// only stores its hash.
    Mint(MintArgs),
}

#[derive(Debug, Args)]
pub struct MintArgs {
    /// Token tier: `user`, `service`, or `admin`. The runtime enforces
    /// the per-tier capability set; admin can mint other admins,
    /// service is for non-interactive callers, user for end users.
    #[arg(long)]
    tier: String,

    /// Subject the token represents — typically a user id, service
    /// name, or workspace identity. Stored alongside the hash for
    /// audit / list output.
    #[arg(long)]
    subject: String,

    /// Optional ISO-8601 expiry. If unset, the token never expires
    /// (you must explicitly `revoke` to invalidate).
    #[arg(long)]
    expires_at: Option<String>,

    /// Custom claims as `key=value` pairs. Repeatable. Values are
    /// parsed as JSON (so `--claim age=42` is numeric, `--claim
    /// name=alice` is string-after-fallback). Forwarded into the
    /// minted token's claim bag.
    #[arg(long = "claim", value_name = "KEY=VALUE")]
    claims: Vec<String>,

    /// Print the response as JSON instead of human-friendly text.
    /// Useful for shell scripting: `forge tokens mint ... --json | jq -r .raw_token`.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Serialize)]
struct MintRequest {
    tier: String,
    subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<String>,
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    claims: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct MintResponse {
    /// Raw token string — return-once. Operator captures into env.
    raw_token: String,
    /// Hex-encoded hash of the token. Stored server-side; operators
    /// use this to revoke without ever holding the raw value again.
    token_hash_hex: String,
    /// Server-side echo of the subject for confirmation.
    subject: String,
    /// Server-side echo of the tier.
    tier: String,
    /// Optional ISO-8601 expiry, when one was set.
    #[serde(default)]
    expires_at: Option<String>,
}

pub async fn run(cmd: TokensCmd, client: &ForgeClient) -> Result<()> {
    match cmd {
        TokensCmd::Mint(args) => mint(args, client).await,
    }
}

async fn mint(args: MintArgs, client: &ForgeClient) -> Result<()> {
    let claims = parse_claims(&args.claims)?;
    let req = MintRequest {
        tier: args.tier,
        subject: args.subject,
        expires_at: args.expires_at,
        claims,
    };
    let resp: MintResponse = client
        .post_json("/api/v1/platform/auth/mint", &req)
        .await
        .map_err(map_err)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "raw_token": resp.raw_token,
            "token_hash_hex": resp.token_hash_hex,
            "subject": resp.subject,
            "tier": resp.tier,
            "expires_at": resp.expires_at,
        }))?);
    } else {
        // Raw token first on stdout so `$(forge tokens mint ...)`
        // captures cleanly. Everything else goes to stderr.
        println!("{}", resp.raw_token);
        eprintln!("token_hash_hex: {}", resp.token_hash_hex);
        eprintln!("subject:        {}", resp.subject);
        eprintln!("tier:           {}", resp.tier);
        if let Some(exp) = resp.expires_at {
            eprintln!("expires_at:     {exp}");
        }
        eprintln!();
        eprintln!("The raw token is shown only here. Capture it now.");
    }
    Ok(())
}

/// Parse repeated `--claim KEY=VALUE` flags into a JSON object. Values
/// are parsed as JSON when valid (so `42`, `true`, `"x"`, `[1,2]` all
/// keep their natural types) and fall back to a JSON string when not.
fn parse_claims(raw: &[String]) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut out = serde_json::Map::new();
    for entry in raw {
        let (key, value) = entry
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--claim must be KEY=VALUE; got {entry:?}"))?;
        let parsed = serde_json::from_str(value)
            .unwrap_or_else(|_| serde_json::Value::String(value.to_string()));
        out.insert(key.to_string(), parsed);
    }
    Ok(out)
}

/// Translate transport / HTTP errors into anyhow with a hint about
/// the most common causes. Avoids printing `400: {...raw json...}`
/// at the customer when we can do better.
fn map_err(e: ForgeError) -> anyhow::Error {
    match e {
        ForgeError::Http { status, body: _ } if status.as_u16() == 401 => {
            anyhow::anyhow!(
                "401 Unauthorized — the token isn't valid for this workspace. \
                 Check FORGE_TOKEN / --token / your config profile."
            )
        }
        ForgeError::Http { status, body } if status.as_u16() == 403 => {
            anyhow::anyhow!(
                "403 Forbidden — the token isn't admin-tier. Token mint requires \
                 the bootstrap admin token (or one minted with `--tier admin`). Body: {body}"
            )
        }
        ForgeError::Http { status, body } => {
            anyhow::anyhow!("{status}: {body}")
        }
        other => anyhow::anyhow!(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_claims_keeps_natural_types() {
        let claims = parse_claims(&[
            "age=42".into(),
            "active=true".into(),
            "name=alice".into(),
            "tags=[\"a\",\"b\"]".into(),
        ])
        .unwrap();
        assert_eq!(claims["age"], serde_json::json!(42));
        assert_eq!(claims["active"], serde_json::json!(true));
        assert_eq!(claims["name"], serde_json::json!("alice"));
        assert_eq!(claims["tags"], serde_json::json!(["a", "b"]));
    }

    #[test]
    fn parse_claims_falls_back_to_string_on_invalid_json() {
        let claims = parse_claims(&["greeting=hello world".into()]).unwrap();
        // "hello world" is not a valid JSON literal; falls back to string.
        assert_eq!(claims["greeting"], serde_json::json!("hello world"));
    }

    #[test]
    fn parse_claims_rejects_missing_equals() {
        assert!(parse_claims(&["broken".into()]).is_err());
    }
}
