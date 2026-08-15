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
//! Deploy is GitOps: your repo is the desired state, and `git push`
//! hands the source to the server, which compiles and converges it.
//! The old imperative deploy commands (`schema apply`, `ship`) have
//! been removed; `deploy` and `static upload` are retained only as
//! hidden primitives that `forge dev` uses for its inner loop.

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::{EnvFilter, fmt};

mod client;
mod cmd;
mod config;
mod contract_lint;

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

    /// Portal base URL. Falls back to env / config / the default
    /// `https://app.forge.run`. Used by `forge sso login` plus the
    /// transparent 401-retry path when the saved `[portal].base_url`
    /// should be overridden for a one-shot invocation (e.g. local
    /// dev against a portal fixture).
    #[arg(long, global = true, env = "FORGE_PORTAL_URL")]
    portal_url: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Authenticate this CLI via browser-based device flow.
    /// Federated mode is the default — `forge login` with no
    /// flags delegates to `forge sso login` against
    /// `https://app.forge.run`. Pass `--base-url <url>` to opt
    /// into direct mode (the deprecated break-glass for when the
    /// portal is unreachable).
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

    /// Manage workspace secrets (set / list / rm). Values are
    /// encrypted server-side; ops read them via manifest-declared
    /// `secrets: [...]` grants.
    #[command(subcommand)]
    Secrets(cmd::secrets::SecretsCmd),
    /// Workspace email/notifications admin (identity, templates,
    /// suppressions, deliveries).
    #[command(subcommand)]
    Email(cmd::email::EmailCmd),

    /// Generate a typed client SDK from the workspace's registry.
    /// `forge sdk generate` writes a self-contained TypeScript client
    /// package (content-addressed to the registry version) to `--out`.
    #[command(subcommand)]
    Sdk(cmd::sdk::SdkCmd),

    /// Compile the workspace graph to WebAssembly Components, stamp their
    /// blake3 hashes into `forge.lock`, and persist the Components to
    /// `.forge/artifacts/` for `forge wasm-upload` to stage. (Was `forge build`.)
    #[command(name = "wasm-build", alias = "build")]
    WasmBuild(cmd::build::BuildArgs),

    /// Stage the built WebAssembly Components (from `.forge/artifacts/`) to the
    /// workspace's content store so a `git push` (GitOps) converge resolves each
    /// module by hash. Run after `forge wasm-build`, before pushing.
    #[command(name = "wasm-upload")]
    WasmUpload(cmd::wasm_upload::WasmUploadArgs),

    /// Deploy in one command: wasm-build → wasm-upload → git push → wait for
    /// the converge. The complete GitOps deploy; guarantees the step ordering.
    Ship(cmd::ship::ShipArgs),

    /// Push to the forge-git remote and block until the converge finishes,
    /// decoding the silent failure modes (unstaged components, stuck, refused).
    /// A raw `git push` only records desired state; this waits for it to be live.
    Push(cmd::push::PushArgs),

    /// Fetch the workspace's live deploy state from its forge-git remote (the
    /// mirror of `push`). Its history is server-derived and usually diverges
    /// from local commits, so `--ff` is opt-in.
    Pull(cmd::pull::PullArgs),

    /// Internal: imperative upload of a service manifest + compiled WASM
    /// module(s). Superseded by `git push` (GitOps) as the deploy path;
    /// kept hidden as the primitive `forge dev` shells out to for its
    /// inner loop.
    #[command(hide = true)]
    Deploy(cmd::deploy::DeployArgs),

    /// Tail recent request log entries from the workspace.
    Logs(cmd::logs::LogsArgs),

    /// Scaffold a new forge workload from a starter template.
    New(cmd::new::NewArgs),

    /// Scaffold a new GitOps-native workspace repo (app.json + config dirs +
    /// git init). Push its `main` to deploy: `main` is the desired state.
    Init(cmd::init::InitArgs),

    /// Internal: imperative static-asset upload. Superseded by `git push`
    /// (the server bundles static assets on converge); kept hidden as the
    /// primitive `forge dev` shells out to for its inner loop.
    #[command(hide = true)]
    #[command(subcommand)]
    Static(cmd::static_cmd::StaticCmd),

    /// Federated workspace access via the portal.
    /// `forge sso login` runs portal device flow;
    /// `forge sso connect <workspace_id>` mints a federated
    /// bearer for a specific workspace (also fires automatically
    /// inside other commands via the transparent re-auth path
    /// when `forge ws use` set an active workspace).
    #[command(subcommand)]
    Sso(cmd::sso::SsoCmd),

    /// Self-update from the GitHub release pipeline. Downloads
    /// the latest release for this platform, verifies sha256,
    /// atomic-swaps the running binary in place. Pass `--check`
    /// to query without applying.
    Update(cmd::update::UpdateArgs),

    /// List and select tenants visible to the active portal session.
    #[command(subcommand)]
    Tenant(cmd::tenant::TenantCmd),

    /// Phase E.3 — tenant domain claims + ACME validation flow.
    /// Operator-only (direct-to-CP via FORGE_CP_URL + FORGE_ADMIN_TOKEN
    /// env vars). Portal-proxied customer-facing flow lands in Phase F.
    #[command(subcommand)]
    Domain(cmd::domain::DomainCmd),

    /// Phase G.1 — watch the project tree and redeploy on change.
    /// Foreground process; runs `forge build` + `forge deploy`
    /// (+ `forge static upload`) when a `.page.json`, `.component.css`,
    /// template, or `.rs` file is saved. Ctrl-C to exit.
    Dev(cmd::dev::DevArgs),

    /// Phase G.2 — list the project's pages with their routes,
    /// auth tier, capabilities, and rendering tier. Reads
    /// `pages/*.page.json` locally; no runtime roundtrip.
    Pages(cmd::pages::PagesArgs),

    /// Phase G.2 — list the project's app-scope components with
    /// prop counts, slot counts, and behavior triggers. Reads
    /// `components/*.component.json` locally.
    Components(cmd::components::ComponentsArgs),

    /// Phase G.2 — print the app-local branding overrides from
    /// `app.json` plus the CSS-variable emission the runtime will
    /// inline at render time.
    Brand(cmd::brand::BrandArgs),

    /// Ephemeral preview environments. Spin up a throwaway workspace running
    /// your working-tree code with SEED data (no production data), test against
    /// it, then promote (deploy the code to prod) or discard.
    #[command(subcommand)]
    Branch(cmd::branch::BranchCmd),

    /// List + select workspaces visible to the active portal
    /// session, scoped to the active tenant. Setting a workspace
    /// here lets every other CLI command (`logs`, `secrets`, …)
    /// auto-mint a federated bearer against it on first 401.
    #[command(subcommand)]
    Ws(cmd::ws::WsCmd),
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
        Cmd::Login(args) => cmd::login::run(args, cli.base_url, cli.profile, cli.portal_url).await,
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
        Cmd::Secrets(s) => {
            let cfg = config::resolve(cli.base_url, cli.token, cli.profile)?;
            let client = client::ForgeClient::new(cfg)?;
            cmd::secrets::run(s, &client).await
        }
        Cmd::Email(e) => {
            let cfg = config::resolve(cli.base_url, cli.token, cli.profile)?;
            let client = client::ForgeClient::new(cfg)?;
            cmd::email::run(e, &client).await
        }
        Cmd::Sdk(s) => {
            let cfg = config::resolve(cli.base_url, cli.token, cli.profile)?;
            let client = client::ForgeClient::new(cfg)?;
            cmd::sdk::run(s, &client).await
        }
        Cmd::WasmBuild(b) => {
            // `wasm-build` doesn't talk to the network — it shells out
            // to cargo. Skip the config + client construction.
            cmd::build::run(b).await
        }
        Cmd::WasmUpload(u) => {
            let cfg = config::resolve(cli.base_url, cli.token, cli.profile)?;
            let client = client::ForgeClient::new(cfg)?;
            cmd::wasm_upload::run(u, &client).await
        }
        Cmd::Ship(s) => {
            let cfg = config::resolve(cli.base_url, cli.token, cli.profile)?;
            let client = client::ForgeClient::new(cfg)?;
            cmd::ship::run(s, &client).await
        }
        Cmd::Push(p) => {
            let cfg = config::resolve(cli.base_url, cli.token, cli.profile)?;
            let client = client::ForgeClient::new(cfg)?;
            cmd::push::run(p, &client).await
        }
        Cmd::Pull(p) => {
            let cfg = config::resolve(cli.base_url, cli.token, cli.profile)?;
            let client = client::ForgeClient::new(cfg)?;
            cmd::pull::run(p, &client).await
        }
        Cmd::Deploy(d) => {
            let cfg = config::resolve(cli.base_url, cli.token, cli.profile)?;
            let client = client::ForgeClient::new(cfg)?;
            cmd::deploy::run(d, &client).await
        }
        Cmd::New(args) => cmd::new::run(args).await,
        Cmd::Init(args) => cmd::init::run(args).await,
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
        Cmd::Sso(s) => cmd::sso::run(s, cli.portal_url).await,
        Cmd::Update(args) => cmd::update::run(args).await,
        Cmd::Tenant(t) => cmd::tenant::run(t).await,
        Cmd::Domain(d) => cmd::domain::run(d).await,
        Cmd::Dev(args) => cmd::dev::run(args).await,
        Cmd::Pages(args) => cmd::pages::run(args).await,
        Cmd::Components(args) => cmd::components::run(args).await,
        Cmd::Brand(args) => cmd::brand::run(args).await,
        Cmd::Branch(b) => cmd::branch::run(b).await,
        Cmd::Ws(w) => cmd::ws::run(w).await,
    }
}
