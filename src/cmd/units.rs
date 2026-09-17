//! `forge units` — the owner's shared units, published by content hash
//! (shared-code SC-10/SC-11, D-110).
//!
//! Operator-only today, like `forge domain`: direct to the control plane
//! via `FORGE_CP_URL` + `FORGE_ADMIN_TOKEN`, under the owner `--owner` names
//! or the active tenant selection. Three verbs:
//!
//! - `publish <file>` — POST the exact bytes; the control plane answers the
//!   address, and the same bytes twice answer the same address.
//! - `list` — the owner's units.
//! - `pull` — fetch every unit `forge.units.json` pins into the local store
//!   `forge check` reads (`crate::pins`), verifying each by digest.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use forge_platform_wire::{
    PublishSharedUnitRequest, SharedUnitRecord, SharedUnitSummary, UNIT_PINS_PATH,
    validate_unit_file_name,
};

use crate::cmd::domain::{CpClient, resolve_tenant_id};
use crate::pins;

#[derive(Debug, Subcommand)]
pub enum UnitsCmd {
    Publish(PublishArgs),
    List(OwnerArgs),
    Pull(PullArgs),
}

#[derive(Debug, clap::Args)]
pub struct PublishArgs {
    /// The unit's source file; its name is the name the unit is published under.
    pub file: PathBuf,
    #[arg(long)]
    pub owner: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct OwnerArgs {
    #[arg(long)]
    pub owner: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct PullArgs {
    #[arg(long)]
    pub owner: Option<String>,
    /// The workspace root holding `forge.units.json`; the current directory by default.
    #[arg(long)]
    pub manifest_dir: Option<PathBuf>,
}

pub async fn run(cmd: UnitsCmd) -> Result<()> {
    match cmd {
        UnitsCmd::Publish(args) => publish(args).await,
        UnitsCmd::List(args) => list(args).await,
        UnitsCmd::Pull(args) => pull(args).await,
    }
}

async fn publish(args: PublishArgs) -> Result<()> {
    let cp = CpClient::from_env()?;
    let owner = resolve_tenant_id(args.owner)?;
    let file_name = args
        .file
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    validate_unit_file_name(&file_name).map_err(|e| anyhow::anyhow!("{file_name}: {e}"))?;
    let source = std::fs::read_to_string(&args.file)
        .with_context(|| format!("read {}", args.file.display()))?;
    let local = pins::unit_address(source.as_bytes());
    let url = format!("{}/admin/tenants/{owner}/units", cp.base_url);
    let resp = cp
        .client
        .post(&url)
        .bearer_auth(&cp.bearer)
        .json(&PublishSharedUnitRequest { file_name, source })
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("forge-cp {status}: {body}");
    }
    let record: SharedUnitRecord = resp.json().await.context("decode the published unit")?;
    if record.address != local {
        bail!(
            "the control plane answered {} for bytes this machine hashes to {local}; the two \
             digests must agree before a pin can be trusted",
            record.address
        );
    }
    let verb = if status == reqwest::StatusCode::CREATED {
        "published"
    } else {
        "already published"
    };
    println!("{verb} {} as {}", record.file_name, record.address);
    Ok(())
}

async fn list(args: OwnerArgs) -> Result<()> {
    let cp = CpClient::from_env()?;
    let owner = resolve_tenant_id(args.owner)?;
    let url = format!("{}/admin/tenants/{owner}/units", cp.base_url);
    let resp = cp
        .client
        .get(&url)
        .bearer_auth(&cp.bearer)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        let s = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("forge-cp {s}: {body}");
    }
    let rows: Vec<SharedUnitSummary> = resp.json().await.context("decode the unit list")?;
    if rows.is_empty() {
        eprintln!("(no units published under {owner})");
        return Ok(());
    }
    println!("FILE                          BYTES    ADDRESS");
    for r in &rows {
        println!(" {:<29} {:<8} {}", r.file_name, r.source_bytes, r.address);
    }
    Ok(())
}

async fn pull(args: PullArgs) -> Result<()> {
    let root = args
        .manifest_dir
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let Some(pinned) = pins::read(&root).map_err(|e| anyhow::anyhow!(e))? else {
        bail!(
            "no {UNIT_PINS_PATH} at {} — nothing is pinned",
            root.display()
        );
    };
    let cp = CpClient::from_env()?;
    let owner = resolve_tenant_id(args.owner)?;
    let store = pins::store_dir(&root);
    for (name, address) in &pinned.units {
        let url = format!("{}/admin/tenants/{owner}/units/{address}", cp.base_url);
        let resp = cp
            .client
            .get(&url)
            .bearer_auth(&cp.bearer)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            let s = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("`{name}` pinned at {address}: forge-cp {s}: {body}");
        }
        let record: SharedUnitRecord = resp.json().await.context("decode the unit")?;
        let actual = pins::unit_address(record.source.as_bytes());
        if &actual != address {
            bail!(
                "`{name}`: the control plane answered bytes hashing to {actual} for {address}; \
                 refusing to store them"
            );
        }
        let dir = store.join(address.as_str());
        std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
        // One address is one file: clear anything else under it first.
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
        let path = dir.join(&record.file_name);
        std::fs::write(&path, record.source.as_bytes())
            .with_context(|| format!("write {}", path.display()))?;
        println!("pulled `{name}` ({}) at {address}", record.file_name);
    }
    Ok(())
}
