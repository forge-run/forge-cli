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
//! v0 ships only `tokens mint`. The rest of the surface (`schema`,
//! `ops`, `deploy`, `logs`, `new`, `build`) is stubbed and prints
//! "not implemented" so the command shape is visible immediately.

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
    /// Manage opaque-token authentication for the workspace.
    #[command(subcommand)]
    Tokens(cmd::tokens::TokensCmd),

    /// Apply schema definitions to the workspace.
    #[command(hide = true)]
    Schema,

    /// Apply declarative operations to the workspace.
    #[command(hide = true)]
    Ops,

    /// Build a Rust crate to a workspace-deployable WASM module.
    #[command(hide = true)]
    Build,

    /// Upload built artifacts and apply schema/ops in one transaction.
    #[command(hide = true)]
    Deploy,

    /// Tail recent logs from the workspace.
    #[command(hide = true)]
    Logs,

    /// Scaffold a new forge project in the current directory.
    #[command(hide = true)]
    New,
}

#[tokio::main]
async fn main() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    fmt().with_env_filter(filter).with_target(false).init();

    let cli = Cli::parse();
    let cfg = config::resolve(cli.base_url, cli.token, cli.profile)?;
    let client = client::ForgeClient::new(cfg)?;

    match cli.cmd {
        Cmd::Tokens(t) => cmd::tokens::run(t, &client).await,
        Cmd::Schema | Cmd::Ops | Cmd::Build | Cmd::Deploy | Cmd::Logs | Cmd::New => {
            anyhow::bail!("not implemented yet — only `forge tokens` is wired in v0")
        }
    }
}
