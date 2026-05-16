//! `forge branch` — ephemeral workspace branches.
//!
//! Branchable workspaces + test-running = the agent CI/CD
//! primitive (see [[project_ephemeral_branches]] in memory).
//! Closes the third-party cliff gap by giving every customer
//! a way to snapshot their workspace state, run an experimental
//! migration / deploy / data-correction, observe results, then
//! promote-or-discard.
//!
//! CLI surface (this file):
//!
//! - `forge branch new <name>` — fork the active workspace's
//!   substrate snapshot into a new ephemeral workspace.
//! - `forge branch list` — list active branches.
//! - `forge branch test <id>` — run the standard build + test
//!   pipeline against a branch.
//! - `forge branch promote <id>` — merge a branch's substrate
//!   back to the source workspace.
//! - `forge branch discard <id>` — destroy a branch.
//!
//! All commands route through forge-cp's `/admin/branches`
//! endpoints (`forge-control-plane/src/admin_branches.rs`).
//!
//! Auth: direct-to-CP, mirrors `forge domain` — needs
//! `FORGE_CP_URL` + `FORGE_ADMIN_TOKEN` env vars.
//!
//! # Status
//!
//! Wire path live; CP handlers return 501 today. The CLI
//! prints the CP's response verbatim so the operator can see
//! the substrate-pending state for each verb.

use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Debug, Subcommand)]
pub enum BranchCmd {
    /// Fork the active workspace's substrate into a new
    /// ephemeral workspace. The fork is byte-identical at fork
    /// time; subsequent writes diverge.
    New(NewArgs),

    /// List ephemeral branches under the active tenant.
    List(ListArgs),

    /// Run the standard build + test pipeline against a branch.
    Test(TestArgs),

    /// Promote a branch's substrate back to its source workspace.
    /// Equivalent to merging a PR.
    Promote(PromoteArgs),

    /// Destroy a branch. Frees the underlying storage snapshot.
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
    /// Confirm the destructive merge without an interactive prompt.
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
        // 501 / non-success path — surface raw so the operator
        // sees the "substrate pending" message verbatim.
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
        // Promote is destructive (replaces source workspace
        // substrate). Require explicit confirmation.
        anyhow::bail!(
            "promote replaces the source workspace's substrate. Pass --yes to confirm."
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
        anyhow::bail!("discard destroys the branch snapshot. Pass --yes to confirm.");
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
        // 501 surfaces here verbatim — useful while the
        // substrate work lands. Operator sees the raw error
        // string from forge-cp's stub handlers.
        anyhow::bail!("{status}: {body}");
    }
}
