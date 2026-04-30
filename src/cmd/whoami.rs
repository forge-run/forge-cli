//! `forge whoami` — print the identity of the current bearer.
//!
//! Calls `GET /api/v1/auth/me` with the saved bearer; the runtime
//! validates against storage and returns the resolved Identity.

use anyhow::Result;
use clap::Args;
use serde::Deserialize;

use crate::client::{ForgeClient, ForgeError};

#[derive(Debug, Args)]
pub struct WhoamiArgs {
    /// Print the response as JSON instead of human-friendly text.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Deserialize)]
struct MeResponse {
    subject: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    roles: Vec<String>,
    tier: serde_json::Value,
    workspace: String,
}

pub async fn run(args: WhoamiArgs, client: &ForgeClient) -> Result<()> {
    let resp: MeResponse = client.get_json("/api/v1/auth/me").await.map_err(map_err)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "subject": resp.subject,
                "role": resp.role,
                "roles": resp.roles,
                "tier": resp.tier,
                "workspace": resp.workspace,
            }))?,
        );
    } else {
        println!("subject:   {}", resp.subject);
        println!("workspace: {}", resp.workspace);
        if let Some(r) = &resp.role {
            println!("role:      {r}");
        }
        if !resp.roles.is_empty() {
            println!("roles:     {}", resp.roles.join(", "));
        }
        println!("tier:      {}", resp.tier);
    }
    Ok(())
}

fn map_err(e: ForgeError) -> anyhow::Error {
    match e {
        ForgeError::Http { status, body: _ } if status.as_u16() == 401 => {
            anyhow::anyhow!(
                "401 Unauthorized — the saved token isn't valid for this workspace. \
                 Run `forge login` to get a fresh one.",
            )
        }
        ForgeError::Http { status, body } => anyhow::anyhow!("{status}: {body}"),
        other => anyhow::anyhow!(other),
    }
}
