//! `forge branch` — ephemeral preview environments.
//!
//! A preview is a **fresh, empty workspace running YOUR code with SEED data** —
//! NOT a copy of production. `forge branch new` provisions a throwaway workspace
//! (its own freshly-minted encryption key, empty substrate — a few MB, not a
//! multi-GB clone), then deploys your working-tree code to it exactly like
//! `forge ship` does: schema is created, the repo's declarative seed fixtures
//! (`domains/*/seeds/*.json` → `config_data`) are projected, and the app is
//! deployed. So you get a running, realistic copy of your app to test changes
//! against — with test data, and zero production rows or PII.
//!
//! Why fresh + seeded instead of a production clone: cloning prod meant copying
//! its whole substrate + per-workspace content store (which for a real workspace
//! is huge), and "promoting" meant swapping those bytes back over production — a
//! destructive, high-blast-radius operation. The preview here shares NOTHING
//! with production, so its entire lifecycle (create / test / discard) can never
//! affect prod, and "promote" is just a normal deploy of your code.
//!
//! CLI surface (this file):
//!
//! - `forge branch new <name> --source <ws>` — create a throwaway workspace and
//!   deploy your current working tree + seed fixtures onto it. No prod data.
//! - `forge branch list [--source <ws>]` — list active previews.
//! - `forge branch test <id>` — smoke-check the preview serves (wakes it +
//!   health-checks); records the result.
//! - `forge branch promote <id> --yes` — deploy this preview's code to its
//!   SOURCE workspace (a normal converge — ships code, never touches data).
//! - `forge branch discard <id> --yes` — delete the throwaway workspace + its
//!   key + substrate.
//!
//! All commands route through forge-cp's `/admin/branches` endpoints
//! (`forge-control-plane/src/admin_branches.rs`).
//!
//! Auth: direct-to-CP, mirrors `forge domain` — needs `FORGE_CP_URL` +
//! `FORGE_ADMIN_TOKEN` env vars.
//!
//! # Status
//!
//! Create + discard are LIVE (cp v0.11.50): `new` provisions a fresh empty
//! workspace with its own DEK; `discard` tears it down; neither touches prod.
//! The working-tree code deploy inside `new` (the `forge ship` step) and
//! `promote`-as-code-deploy are the in-flight wiring — until they land, `new`
//! yields an empty preview and `promote` still routes to the legacy CP path.
//! The CLI prints the CP's JSON response verbatim.

use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Debug, Subcommand)]
pub enum BranchCmd {
    /// Create a fresh, empty preview workspace and deploy your working-tree
    /// code + seed fixtures onto it. No production data is copied — the preview
    /// runs your code against test data (the repo's `domains/*/seeds/*.json`).
    New(NewArgs),

    /// List active preview environments.
    List(ListArgs),

    /// Smoke-check a preview: wake it and confirm it serves.
    Test(TestArgs),

    /// Deploy this preview's code to its SOURCE workspace. Ships code via a
    /// normal converge — it does NOT copy or overwrite the source's data.
    Promote(PromoteArgs),

    /// Delete a preview — its throwaway workspace, encryption key, and substrate.
    Discard(DiscardArgs),
}

#[derive(Debug, Args)]
pub struct NewArgs {
    /// Human label for the branch. Surfaces in URLs and
    /// list output.
    pub name: String,

    /// Source workspace id. Required (no implicit active-
    /// workspace lookup yet — once `forge ws use` integrates
    /// with branches this becomes optional).
    #[arg(long)]
    pub source: String,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Filter to branches of a specific source workspace.
    #[arg(long)]
    pub source: Option<String>,
}

#[derive(Debug, Args)]
pub struct TestArgs {
    pub branch_id: String,
}

#[derive(Debug, Args)]
pub struct PromoteArgs {
    pub branch_id: String,
    /// Confirm deploying this preview's code to the live source workspace
    /// (skips the interactive prompt). Ships code, not data.
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
pub struct DiscardArgs {
    pub branch_id: String,
    #[arg(long)]
    yes: bool,
}

pub async fn run(cmd: BranchCmd) -> Result<()> {
    let cp = CpClient::from_env()?;
    match cmd {
        BranchCmd::New(a) => new(&cp, a).await,
        BranchCmd::List(a) => list(&cp, a).await,
        BranchCmd::Test(a) => test(&cp, a).await,
        BranchCmd::Promote(a) => promote(&cp, a).await,
        BranchCmd::Discard(a) => discard(&cp, a).await,
    }
}

// ── HTTP plumbing (mirrors `forge domain`) ───────────────────

struct CpClient {
    client: reqwest::Client,
    base_url: String,
    bearer: String,
}

impl CpClient {
    fn from_env() -> Result<Self> {
        let base_url = std::env::var("FORGE_CP_URL").context(
            "FORGE_CP_URL not set — point at the control-plane base URL \
             (e.g., https://cp.internal.forge.run). Required for `forge branch`.",
        )?;
        let bearer = std::env::var("FORGE_ADMIN_TOKEN").context(
            "FORGE_ADMIN_TOKEN not set — pass the operator bearer the CP \
             accepts on /admin/*. Required for `forge branch`.",
        )?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("build http client")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            bearer,
        })
    }
}

// ── Wire shapes (mirror forge-control-plane::admin_branches) ─

#[derive(Debug, Serialize)]
struct CreateBranchRequest {
    source_workspace_id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // `created_at` parses but isn't surfaced in the
// table view today.
struct BranchWire {
    id: String,
    source_workspace_id: String,
    name: String,
    state: String,
    created_at: String,
    #[serde(default)]
    last_test_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListBranchesResponse {
    branches: Vec<BranchWire>,
}

// ── Subcommand impls ────────────────────────────────────────

async fn new(cp: &CpClient, args: NewArgs) -> Result<()> {
    let url = format!("{}/admin/branches", cp.base_url);
    let resp = cp
        .client
        .post(&url)
        .bearer_auth(&cp.bearer)
        .json(&CreateBranchRequest {
            source_workspace_id: args.source.clone(),
            name: args.name.clone(),
        })
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    print_response(resp).await
}

async fn list(cp: &CpClient, args: ListArgs) -> Result<()> {
    let mut url = format!("{}/admin/branches", cp.base_url);
    if let Some(src) = &args.source {
        url.push_str(&format!("?source={src}"));
    }
    let resp = cp
        .client
        .get(&url)
        .bearer_auth(&cp.bearer)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        // Non-success: surface the CP's message verbatim.
        anyhow::bail!("{status}: {body}");
    }
    let parsed: ListBranchesResponse = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(_) => {
            // Body didn't parse as the expected shape — print it raw.
            println!("{body}");
            return Ok(());
        }
    };
    if parsed.branches.is_empty() {
        println!("no branches");
        return Ok(());
    }
    println!(
        "{:<24}  {:<12}  {:<10}  {:<24}  NAME",
        "ID", "STATE", "TEST", "SOURCE",
    );
    for b in &parsed.branches {
        println!(
            "{:<24}  {:<12}  {:<10}  {:<24}  {}",
            b.id,
            b.state,
            b.last_test_status.as_deref().unwrap_or("-"),
            b.source_workspace_id,
            b.name,
        );
    }
    Ok(())
}

async fn test(cp: &CpClient, args: TestArgs) -> Result<()> {
    let url = format!("{}/admin/branches/{}/test", cp.base_url, args.branch_id);
    let resp = cp
        .client
        .post(&url)
        .bearer_auth(&cp.bearer)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    print_response(resp).await
}

async fn promote(cp: &CpClient, args: PromoteArgs) -> Result<()> {
    if !args.yes {
        // Promote deploys this preview's code to the LIVE source workspace (a
        // normal converge — ships code, not data). Require explicit confirmation.
        anyhow::bail!(
            "promote deploys this preview's code to the live source workspace. \
             Pass --yes to confirm."
        );
    }
    let url = format!("{}/admin/branches/{}/promote", cp.base_url, args.branch_id);
    let resp = cp
        .client
        .post(&url)
        .bearer_auth(&cp.bearer)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    print_response(resp).await
}

async fn discard(cp: &CpClient, args: DiscardArgs) -> Result<()> {
    if !args.yes {
        anyhow::bail!("discard deletes the preview's throwaway workspace. Pass --yes to confirm.");
    }
    let url = format!("{}/admin/branches/{}", cp.base_url, args.branch_id);
    let resp = cp
        .client
        .delete(&url)
        .bearer_auth(&cp.bearer)
        .send()
        .await
        .with_context(|| format!("DELETE {url}"))?;
    print_response(resp).await
}

async fn print_response(resp: reqwest::Response) -> Result<()> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status.is_success() {
        println!("{body}");
        Ok(())
    } else {
        // Surface the CP's error string verbatim.
        anyhow::bail!("{status}: {body}");
    }
}
