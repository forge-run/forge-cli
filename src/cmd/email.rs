//! `forge email *` — workspace email/notifications admin over the
//! manage API (Tier 1 roadmap item 3, Phase 0).
//!
//! - `forge email identity set|show`     → the workspace sending identity
//! - `forge email template set|list|rm`  → registered minijinja templates
//! - `forge email suppress add|list|rm`  → the do-not-send list
//! - `forge email deliveries`            → recent queue rows (status view)
//! - `forge email test <to> <template>`  → enqueue a real send via a
//!   one-off template payload (verification helper)
//!
//! All admin-tier. The byo_relay password is never an argument here —
//! it lives in `_secrets` (`forge secrets set <name>`) and the identity
//! references it by name.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde_json::Value;

use crate::client::ForgeClient;

#[derive(Debug, Subcommand)]
pub enum EmailCmd {
    /// Workspace sending identity.
    #[command(subcommand)]
    Identity(IdentityCmd),
    /// Registered email templates (minijinja).
    #[command(subcommand)]
    Template(TemplateCmd),
    /// Suppression list (do-not-send).
    #[command(subcommand)]
    Suppress(SuppressCmd),
    /// Recent deliveries with status (no bodies).
    Deliveries(JsonArgs),
}

#[derive(Debug, Subcommand)]
pub enum IdentityCmd {
    /// Set (create or replace) the workspace sending identity.
    Set(IdentitySetArgs),
    /// Show the current identity.
    Show(JsonArgs),
}

#[derive(Debug, Subcommand)]
pub enum TemplateCmd {
    /// Register or update a template. Bodies come from files.
    Set(TemplateSetArgs),
    /// List registered templates (names + versions).
    List(JsonArgs),
    /// Remove a template.
    Rm(NameArgs),
}

#[derive(Debug, Subcommand)]
pub enum SuppressCmd {
    /// Add an address to the suppression list.
    Add(SuppressAddArgs),
    /// List suppressed addresses.
    List(JsonArgs),
    /// Remove an address from the suppression list.
    Rm(AddressArgs),
}

#[derive(Debug, Args)]
pub struct JsonArgs {
    /// Print the response as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub struct NameArgs {
    name: String,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub struct AddressArgs {
    address: String,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub struct IdentitySetArgs {
    /// Sending mode: byo_relay | platform_shared | custom_domain.
    /// Phase 0 delivers via byo_relay only.
    #[arg(long, default_value = "byo_relay")]
    mode: String,
    /// From header address (e.g. noreply@yourdomain.com).
    #[arg(long)]
    from: String,
    /// From display name.
    #[arg(long)]
    from_name: Option<String>,
    /// byo_relay: SMTP smarthost hostname.
    #[arg(long)]
    relay_host: Option<String>,
    /// byo_relay: submission port (465 implicit TLS / 587 STARTTLS).
    #[arg(long)]
    relay_port: Option<i64>,
    /// byo_relay: SMTP AUTH username.
    #[arg(long)]
    relay_username: Option<String>,
    /// byo_relay: _secrets name holding the SMTP AUTH password
    /// (set it first: `forge secrets set <name>`).
    #[arg(long)]
    relay_secret: Option<String>,
    /// active | disabled.
    #[arg(long, default_value = "active")]
    status: String,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub struct TemplateSetArgs {
    /// Template name — 1-96 chars of [a-z0-9_-]; ops reference it
    /// verbatim in send_email calls.
    name: String,
    /// Subject template (inline minijinja).
    #[arg(long)]
    subject: String,
    /// Plain-text body template file.
    #[arg(long)]
    text_file: std::path::PathBuf,
    /// Optional HTML body template file.
    #[arg(long)]
    html_file: Option<std::path::PathBuf>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub struct SuppressAddArgs {
    address: String,
    /// hard_bounce | complaint | manual.
    #[arg(long, default_value = "manual")]
    reason: String,
    #[arg(long)]
    json: bool,
}

pub async fn run(cmd: EmailCmd, client: &ForgeClient) -> Result<()> {
    match cmd {
        EmailCmd::Identity(IdentityCmd::Set(a)) => identity_set(a, client).await,
        EmailCmd::Identity(IdentityCmd::Show(a)) => {
            get_and_print(client, "/api/v1/manage/email/identity", a.json).await
        }
        EmailCmd::Template(TemplateCmd::Set(a)) => template_set(a, client).await,
        EmailCmd::Template(TemplateCmd::List(a)) => {
            get_and_print(client, "/api/v1/manage/email/templates", a.json).await
        }
        EmailCmd::Template(TemplateCmd::Rm(a)) => {
            post_and_print(
                client,
                "/api/v1/manage/email/templates/rm",
                serde_json::json!({ "name": a.name }),
                a.json,
            )
            .await
        }
        EmailCmd::Suppress(SuppressCmd::Add(a)) => {
            post_and_print(
                client,
                "/api/v1/manage/email/suppressions",
                serde_json::json!({ "address": a.address, "reason": a.reason }),
                a.json,
            )
            .await
        }
        EmailCmd::Suppress(SuppressCmd::List(a)) => {
            get_and_print(client, "/api/v1/manage/email/suppressions", a.json).await
        }
        EmailCmd::Suppress(SuppressCmd::Rm(a)) => {
            post_and_print(
                client,
                "/api/v1/manage/email/suppressions/rm",
                serde_json::json!({ "address": a.address }),
                a.json,
            )
            .await
        }
        EmailCmd::Deliveries(a) => {
            get_and_print(client, "/api/v1/manage/email/deliveries", a.json).await
        }
    }
}

async fn identity_set(a: IdentitySetArgs, client: &ForgeClient) -> Result<()> {
    if a.mode == "byo_relay"
        && (a.relay_host.is_none() || a.relay_port.is_none() || a.relay_secret.is_none())
    {
        bail!("byo_relay needs --relay-host, --relay-port and --relay-secret (a _secrets name)");
    }
    let body = serde_json::json!({
        "sending_mode": a.mode,
        "from_address": a.from,
        "from_name": a.from_name,
        "relay_host": a.relay_host,
        "relay_port": a.relay_port,
        "relay_username": a.relay_username,
        "relay_secret_name": a.relay_secret,
        "status": a.status,
    });
    post_and_print(client, "/api/v1/manage/email/identity", body, a.json).await
}

async fn template_set(a: TemplateSetArgs, client: &ForgeClient) -> Result<()> {
    let body_text = std::fs::read_to_string(&a.text_file)
        .with_context(|| format!("reading {}", a.text_file.display()))?;
    let body_html = a
        .html_file
        .as_ref()
        .map(|p| std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display())))
        .transpose()?;
    let body = serde_json::json!({
        "name": a.name,
        "subject": a.subject,
        "body_text": body_text,
        "body_html": body_html,
    });
    post_and_print(client, "/api/v1/manage/email/templates/set", body, a.json).await
}

async fn get_and_print(client: &ForgeClient, path: &str, _json: bool) -> Result<()> {
    let resp: Value = client.get_json(path).await.map_err(anyhow::Error::from)?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}

async fn post_and_print(client: &ForgeClient, path: &str, body: Value, _json: bool) -> Result<()> {
    let resp: Value = client
        .post_json(path, &body)
        .await
        .map_err(anyhow::Error::from)?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}
