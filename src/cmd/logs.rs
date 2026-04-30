//! `forge logs` — tail the workspace's request log.
//!
//! Hits `GET /api/v1/manage/logs?lines=N` on the active
//! workspace and prints each line. Admin-tier required.
//!
//! v1 ships poll mode only — no SSE follow. Customers
//! iterating on a workload can re-run the command, or wrap it
//! in a shell loop. SSE follow lands when there's demand.

use anyhow::Result;
use clap::Args;
use serde::Deserialize;

use crate::client::{ForgeClient, ForgeError};

#[derive(Debug, Args)]
pub struct LogsArgs {
    /// How many trailing lines to fetch. Capped server-side at
    /// 1000.
    #[arg(long, default_value_t = 100)]
    lines: usize,
    /// Print as JSON-lines instead of the formatted columnar
    /// view. Easier to pipe into jq / less.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Deserialize)]
struct LogRecord {
    ts: String,
    method: String,
    path: String,
    status: u16,
    dur_ms: u64,
}

pub async fn run(args: LogsArgs, client: &ForgeClient) -> Result<()> {
    let path = format!("/api/v1/manage/logs?lines={}", args.lines);
    let records: Vec<LogRecord> = client.get_json(&path).await.map_err(map_err)?;
    if records.is_empty() {
        eprintln!("no log entries (workload hasn't served any requests yet)");
        return Ok(());
    }
    if args.json {
        for r in &records {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "ts": r.ts,
                    "method": r.method,
                    "path": r.path,
                    "status": r.status,
                    "dur_ms": r.dur_ms,
                }))?
            );
        }
    } else {
        // Columnar tail. Width-pad the method (max 7 for HTTP)
        // and the status so things line up; truncate the path
        // mid-line if it's silly long.
        for r in &records {
            let path_display = if r.path.len() > 80 {
                format!("{}…", &r.path[..79])
            } else {
                r.path.clone()
            };
            println!(
                "{ts}  {status:>3}  {method:<7} {path}  {dur}ms",
                ts = r.ts,
                status = r.status,
                method = r.method,
                path = path_display,
                dur = r.dur_ms,
            );
        }
    }
    Ok(())
}

fn map_err(e: ForgeError) -> anyhow::Error {
    match e {
        ForgeError::Http { status, body: _ } if status.as_u16() == 401 => {
            anyhow::anyhow!(
                "401 Unauthorized — the saved token isn't valid. \
                 Run `forge login` to get a fresh one.",
            )
        }
        ForgeError::Http { status, body: _ } if status.as_u16() == 403 => {
            anyhow::anyhow!(
                "403 Forbidden — `forge logs` requires role=admin on the calling user. \
                 Ask the operator to elevate, or run as the bootstrap admin user.",
            )
        }
        ForgeError::Http { status, body: _ } if status.as_u16() == 503 => {
            anyhow::anyhow!(
                "503 — the workspace's runtime started without a request log. \
                 Ask the operator to ensure FORGE_DATA_DIR is set when launching forge-runtime.",
            )
        }
        ForgeError::Http { status, body } => anyhow::anyhow!("{status}: {body}"),
        other => anyhow::anyhow!(other),
    }
}
