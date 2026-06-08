//! `forge schema apply` — push a schema definition to the workspace.
//!
//! Reads a JSON schema file from disk and POSTs it to the
//! workspace's `/api/v1/manage/schema/apply` endpoint with the
//! customer's bearer token (saved by `forge login`). The server
//! validates the bearer, requires `role == "admin"` on the user,
//! then dispatches to forge-storage's `apply_schema` with a
//! synthesized System identity.
//!
//! Schema file shape (matches `ForgeStorage::apply_schema`):
//!
//! ```json
//! {
//!   "name": "users",
//!   "archetype": "Base",
//!   "columns": [
//!     { "name": "email", "type": "string", "required": true, "unique": true },
//!     { "name": "password_hash", "type": "string", "required": true },
//!     { "name": "role", "type": "string", "required": true }
//!   ]
//! }
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use crate::client::{ForgeClient, ForgeError};

#[derive(Debug, Subcommand)]
pub enum SchemaCmd {
    /// Apply a schema definition to the workspace.
    Apply(ApplyArgs),
}

#[derive(Debug, Args)]
pub struct ApplyArgs {
    /// Path to the schema JSON file.
    #[arg(long)]
    file: PathBuf,

    /// Allow a DESTRUCTIVE change — one that drops relationships, row-security
    /// policies, or columns relative to the table's current schema. Off by
    /// default: the server refuses such a transition (it's almost always an
    /// accidental partial/minimal schema, the class of bug that silently wiped
    /// relationships + RLS). Set this ONLY for an intentional removal.
    #[arg(long)]
    allow_destructive: bool,

    /// Print the response as JSON instead of human-friendly text.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Serialize)]
struct ApplyRequest {
    schema: serde_json::Value,
    allow_destructive: bool,
}

#[derive(Debug, Deserialize)]
struct ApplyResponse {
    #[serde(default)]
    table_id: Option<u64>,
    #[serde(default)]
    version: Option<u64>,
}

pub async fn run(cmd: SchemaCmd, client: &ForgeClient) -> Result<()> {
    match cmd {
        SchemaCmd::Apply(args) => apply(args, client).await,
    }
}

async fn apply(args: ApplyArgs, client: &ForgeClient) -> Result<()> {
    let bytes = std::fs::read(&args.file)
        .with_context(|| format!("reading schema file {}", args.file.display()))?;
    let schema: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing schema file {} as JSON", args.file.display()))?;
    let table = schema
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let req = ApplyRequest {
        schema,
        allow_destructive: args.allow_destructive,
    };
    let resp: ApplyResponse = client
        .post_json("/api/v1/manage/schema/apply", &req)
        .await
        .map_err(map_err)?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "table_id": resp.table_id,
                "version": resp.version,
            }))?,
        );
    } else {
        let label = table.as_deref().unwrap_or("(unnamed)");
        eprintln!("schema applied: {label}");
        if let Some(id) = resp.table_id {
            eprintln!("table_id: {id}");
        }
        if let Some(v) = resp.version {
            eprintln!("version:  {v}");
        }
    }
    Ok(())
}

fn map_err(e: ForgeError) -> anyhow::Error {
    match e {
        ForgeError::Http { status, body: _ } if status.as_u16() == 401 => {
            anyhow::anyhow!(
                "401 Unauthorized — your token isn't valid for this workspace. \
                 Run `forge login` to refresh it.",
            )
        }
        ForgeError::Http { status, body: _ } if status.as_u16() == 403 => {
            anyhow::anyhow!(
                "403 Forbidden — schema apply requires role=admin on the calling user. \
                 Either bootstrap an admin user (operator side) or update your role claim.",
            )
        }
        ForgeError::Http { status, body } => {
            anyhow::anyhow!("{status}: {body}")
        }
        other => anyhow::anyhow!(other),
    }
}
