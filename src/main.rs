//! `forge` — customer-facing CLI for the forge platform.
//!
//! Every command speaks to a single workspace's running `forge-runtime`
//! over HTTPS, authenticated by a Bearer token. There is no central CLI
//! server; the workspace's runtime *is* the API endpoint. The runtime's
//! `_platform::*` operation namespace is the wire surface.
//!
//! Configuration source order (later overrides earlier):
//! 1. `~/.forge/config.toml` — default base URL + token, plus a list of
//!    named workspace profiles.
//! 2. `./.forge.toml` — per-project override (committed alongside
//!    schema/ops/wasm).
//! 3. Environment: `FORGE_BASE_URL`, `FORGE_TOKEN`.
//! 4. Command-line flags: `--base-url`, `--token`.
//!
//! v0 ships `login` (RFC 8628 device flow → saves token to config)
//! and `tokens mint` (System-tier; today only callable when the CLI
//! holds an admin-tier token, which is provisioning-shaped, not
//! customer-shaped). Other commands (`schema`, `ops`, `deploy`,
//! `logs`, `new`, `build`) are stubbed and print "not implemented"
//! so the command shape is visible immediately.

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::{EnvFilter, fmt};

mod client;
mod cmd;
mod config;

#[derive(Debug, Parser)]
#[command(name = "forge", version, about, long_about = None)]
struct Cli {
    /// Base URL of the workspace's forge-runtime (e.g.
    /// `https://alpha.forge.run`). Falls back to env / config.
    #[arg(long, global = true, env = "FORGE_BASE_URL")]
    base_url: Option<String>,

    /// Bearer token (admin-tier for `_platform::*` operations). Falls
    /// back to env / config. Never echoed; never logged.
    #[arg(long, global = true, env = "FORGE_TOKEN", hide_env_values = true)]
    token: Option<String>,

    /// Named profile from `~/.forge/config.toml`. Lets one developer
    /// switch between workspaces without re-typing URLs and tokens.
    #[arg(long, global = true)]
    profile: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Authenticate this CLI to a workspace via browser-based
    /// device flow (RFC 8628). Saves the resulting token under
    /// the active profile in `~/.forge/config.toml`.
    Login(cmd::login::LoginArgs),

    /// Revoke the saved bearer + refresh token and clear the
    /// active profile from `~/.forge/config.toml`.
    Logout(cmd::logout::LogoutArgs),

    /// Print the identity of the saved bearer (subject, role,
    /// workspace).
    Whoami(cmd::whoami::WhoamiArgs),

    /// Manage opaque-token authentication for the workspace.
    #[command(subcommand)]
    Tokens(cmd::tokens::TokensCmd),

    /// Apply schema definitions to the workspace.
    #[command(subcommand)]
    Schema(cmd::schema::SchemaCmd),

    /// Apply declarative operations to the workspace.
    #[command(hide = true)]
    Ops,

    /// Build a Rust crate to a workspace-deployable WASM module.
    Build(cmd::build::BuildArgs),

    /// Upload a service manifest + compiled WASM module(s) to the
    /// workspace.
    Deploy(cmd::deploy::DeployArgs),

    /// Tail recent request log entries from the workspace.
    Logs(cmd::logs::LogsArgs),

    /// Scaffold a new forge workload from a starter template.
    New(cmd::new::NewArgs),

    /// Upload static assets (CSS, JS, images) to the workspace's
    /// `<data_dir>/static/` directory. Each file becomes
    /// reachable at `https://<workspace>/static/<path>`. Replaces
    /// the v2.0 manual `scp` step. ADR 0017.
    #[command(subcommand)]
    Static(cmd::static_cmd::StaticCmd),

    /// ADR 0021 — federated workspace access via the portal.
    /// `forge sso connect <workspace_id>` drives the SSO
    /// mint→consume dance and emits the resulting `fr_f_*`
    /// bearer. Requires that the active profile already holds
    /// a portal bearer (from `forge login` against app.forge.run).
    #[command(subcommand)]
    Sso(cmd::sso::SsoCmd),
}

#[tokio::main]
async fn main() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    fmt().with_env_filter(filter).with_target(false).init();

    let cli = Cli::parse();

    // `login` resolves base_url + profile *without* a token (login
    // is what produces the token). Every other command needs both
    // and goes through the standard config::resolve path.
    match cli.cmd {
        Cmd::Login(args) => cmd::login::run(args, cli.base_url, cli.profile).await,
        Cmd::Logout(args) => cmd::logout::run(args, cli.base_url, cli.token, cli.profile).await,
        Cmd::Whoami(args) => {
            let cfg = config::resolve(cli.base_url, cli.token, cli.profile)?;
            let client = client::ForgeClient::new(cfg)?;
            cmd::whoami::run(args, &client).await
        }
        Cmd::Tokens(t) => {
            let cfg = config::resolve(cli.base_url, cli.token, cli.profile)?;
            let client = client::ForgeClient::new(cfg)?;
            cmd::tokens::run(t, &client).await
        }
        Cmd::Schema(s) => {
            let cfg = config::resolve(cli.base_url, cli.token, cli.profile)?;
            let client = client::ForgeClient::new(cfg)?;
            cmd::schema::run(s, &client).await
        }
        Cmd::Build(b) => {
            // `build` doesn't talk to the network — it shells out
            // to cargo. Skip the config + client construction.
            cmd::build::run(b).await
        }
        Cmd::Deploy(d) => {
            let cfg = config::resolve(cli.base_url, cli.token, cli.profile)?;
            let client = client::ForgeClient::new(cfg)?;
            cmd::deploy::run(d, &client).await
        }
        Cmd::New(args) => cmd::new::run(args).await,
        Cmd::Static(s) => {
            let cfg = config::resolve(cli.base_url, cli.token, cli.profile)?;
            let client = client::ForgeClient::new(cfg)?;
            cmd::static_cmd::run(s, &client).await
        }
        Cmd::Logs(args) => {
            let cfg = config::resolve(cli.base_url, cli.token, cli.profile)?;
            let client = client::ForgeClient::new(cfg)?;
            cmd::logs::run(args, &client).await
        }
        Cmd::Sso(s) => cmd::sso::run(s, cli.base_url, cli.token, cli.profile).await,
        Cmd::Ops => {
            anyhow::bail!(
                "not implemented yet — login/logout/whoami/tokens/schema/build/deploy/new/logs are wired",
            )
        }
    }
}
