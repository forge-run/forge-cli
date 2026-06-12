//! `forge secrets *` — workspace secrets over the manage API
//! (Tier 1 roadmap item 1, Phase 0).
//!
//! - `POST /api/v1/manage/secrets/set` → create / rotate one secret
//! - `GET  /api/v1/manage/secrets`     → names + lifecycle metadata
//! - `POST /api/v1/manage/secrets/rm`  → soft-retire a name
//!
//! All three require an admin-tier bearer. The VALUE is read from
//! stdin or `--value-file`, never from argv (argv leaks via shell
//! history and `ps`), and is never echoed back — `set` confirms with
//! name + version only. The server stores AEAD ciphertext; `list`
//! structurally cannot return values.

use std::io::Read;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use crate::client::ForgeClient;

#[derive(Debug, Subcommand)]
pub enum SecretsCmd {
    /// Set (create or rotate) a secret. Reads the value from stdin
    /// (`forge secrets set stripe_key < key.txt`, or piped) or from
    /// `--value-file`. Trailing newline is stripped.
    Set(SetArgs),
    /// List secret names with version + lifecycle stamps. Values are
    /// never returned.
    List(ListArgs),
    /// Retire a secret. The name reads as not-found to ops from now
    /// on; the audit trail is preserved, and a later `set` of the
    /// same name un-retires it at a bumped version.
    Rm(RmArgs),
}

#[derive(Debug, Args)]
pub struct SetArgs {
    /// Secret name — 1-96 chars of [a-z0-9_-]. Referenced verbatim
    /// from op manifests (`secrets: ["stripe_key"]`).
    name: String,

    /// Read the value from this file instead of stdin.
    #[arg(long)]
    value_file: Option<std::path::PathBuf>,

    /// Print the response as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Print the response as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub struct RmArgs {
    /// Secret name to retire.
    name: String,

    /// Print the response as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Serialize)]
struct SetRequest {
    name: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct SetResponse {
    name: String,
    version: i64,
    #[serde(default)]
    rotated: bool,
}

#[derive(Debug, Deserialize)]
struct ListResponse {
    secrets: Vec<ListEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ListEntry {
    name: String,
    version: i64,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    rotated_at: Option<String>,
    #[serde(default)]
    retired: bool,
}

#[derive(Debug, Deserialize)]
struct RmResponse {
    name: String,
    #[serde(default)]
    retired: bool,
}

pub async fn run(cmd: SecretsCmd, client: &ForgeClient) -> Result<()> {
    match cmd {
        SecretsCmd::Set(args) => set(args, client).await,
        SecretsCmd::List(args) => list(args, client).await,
        SecretsCmd::Rm(args) => rm(args, client).await,
    }
}

async fn set(args: SetArgs, client: &ForgeClient) -> Result<()> {
    let mut value = match &args.value_file {
        Some(path) => {
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
        }
        None => {
            // Refuse an interactive TTY: a value typed at a visible
            // prompt lands in terminal scrollback. Pipe it instead.
            if atty_stdin() {
                bail!(
                    "stdin is a TTY — pipe the value in (`forge secrets set {} < value.txt`) \
                     or use --value-file",
                    args.name
                );
            }
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("reading value from stdin")?;
            buf
        }
    };
    // One trailing newline is an artifact of `echo`/files, not the value.
    if value.ends_with('\n') {
        value.pop();
        if value.ends_with('\r') {
            value.pop();
        }
    }
    if value.is_empty() {
        bail!("secret value is empty");
    }

    let req = SetRequest {
        name: args.name,
        value,
    };
    let resp: SetResponse = client
        .post_json("/api/v1/manage/secrets/set", &req)
        .await
        .map_err(anyhow::Error::from)?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": resp.name, "version": resp.version, "rotated": resp.rotated,
            }))?
        );
    } else {
        eprintln!(
            "{} `{}` at version {} — value stored encrypted; it is never shown again.",
            if resp.rotated { "rotated" } else { "created" },
            resp.name,
            resp.version,
        );
        eprintln!(
            "ops read it by declaring `secrets: [\"{}\"]` in their manifest.",
            resp.name
        );
    }
    Ok(())
}

async fn list(args: ListArgs, client: &ForgeClient) -> Result<()> {
    let resp: ListResponse = client
        .get_json("/api/v1/manage/secrets")
        .await
        .map_err(anyhow::Error::from)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&resp.secrets)?);
        return Ok(());
    }
    if resp.secrets.is_empty() {
        eprintln!("no secrets set in this workspace");
        return Ok(());
    }
    println!(
        "{:<32} {:>7}  {:<20}  {}",
        "NAME", "VERSION", "ROTATED", "STATUS"
    );
    for s in &resp.secrets {
        println!(
            "{:<32} {:>7}  {:<20}  {}",
            s.name,
            s.version,
            s.rotated_at.as_deref().unwrap_or("—"),
            if s.retired { "retired" } else { "active" },
        );
    }
    Ok(())
}

async fn rm(args: RmArgs, client: &ForgeClient) -> Result<()> {
    let req = serde_json::json!({ "name": args.name });
    let resp: RmResponse = client
        .post_json("/api/v1/manage/secrets/rm", &req)
        .await
        .map_err(anyhow::Error::from)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": resp.name, "retired": resp.retired,
            }))?
        );
    } else {
        eprintln!(
            "retired `{}` — ops now read it as not-found; `set` re-activates it.",
            resp.name
        );
    }
    Ok(())
}

fn atty_stdin() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}
