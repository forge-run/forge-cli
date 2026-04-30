//! `forge logs` — tail the workspace's request log.
//!
//! Two modes:
//! - default: GET /api/v1/manage/logs?lines=N, print, exit.
//! - --follow: GET /api/v1/manage/logs?follow=true (SSE),
//!   print each event as it arrives until Ctrl-C.
//!
//! Admin-tier required either way.

use anyhow::Result;
use clap::Args;
use serde::Deserialize;

use crate::client::{ForgeClient, ForgeError};

#[derive(Debug, Args)]
pub struct LogsArgs {
    /// How many trailing lines to fetch (one-shot mode).
    /// Capped server-side at 1000. Ignored when --follow.
    #[arg(long, default_value_t = 100)]
    lines: usize,
    /// Print as JSON-lines instead of the formatted columnar
    /// view. Easier to pipe into jq / less.
    #[arg(long)]
    json: bool,
    /// Subscribe to the live request stream over SSE. Prints
    /// each new request as it lands; Ctrl-C to exit.
    #[arg(long)]
    follow: bool,
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
    if args.follow {
        return run_follow(args, client).await;
    }
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

/// SSE follow path. Opens a long-lived GET against
/// `/api/v1/manage/logs?follow=true`, reads chunks as they
/// arrive, parses `data: {...}` events and prints them in the
/// same shape as the one-shot path.
async fn run_follow(args: LogsArgs, client: &ForgeClient) -> Result<()> {
    let url = format!("{}/api/v1/manage/logs?follow=true", client.base_url());
    // Fresh client with NO request timeout — SSE streams are
    // long-lived and the default 30s timeout would terminate
    // them. We still rely on the OS / network for connection
    // health.
    let stream_client = reqwest::Client::builder()
        .user_agent(concat!("forge-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| anyhow::anyhow!("build streaming client: {e}"))?;
    let mut resp = stream_client
        .get(&url)
        .bearer_auth(client.token())
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("connect to {url}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(map_err(ForgeError::Http { status, body }));
    }

    // SSE parser: events are `data: <payload>\n\n`. We
    // accumulate bytes, scan for `\n\n` boundaries, parse the
    // `data:` line out of each event, and print one record per
    // event. Keep-alive comments (`: ...\n\n`) are ignored.
    eprintln!("following {} (Ctrl-C to exit)", url);
    let mut buf = String::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| anyhow::anyhow!("read SSE chunk: {e}"))?
    {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = buf.find("\n\n") {
            let event = buf[..end].to_string();
            buf.drain(..end + 2);
            if let Some(payload) = event.lines().find_map(|l| l.strip_prefix("data:")) {
                let payload = payload.trim();
                if payload.is_empty() {
                    continue;
                }
                if let Ok(rec) = serde_json::from_str::<LogRecord>(payload) {
                    print_record(&rec, args.json)?;
                }
            }
        }
    }
    Ok(())
}

fn print_record(r: &LogRecord, json: bool) -> Result<()> {
    if json {
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
    } else {
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
