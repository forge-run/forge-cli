# forge-cli

Command-line client for the [Forge platform](https://app.forge.run).

Sign in via your browser, pick a tenant + workspace, then deploy schemas, push WASM workloads, tail logs, and manage tokens — all over HTTPS against your workspace's runtime.

## Install

```sh
curl -sSfL https://install.forge.run | sh
```

This drops a `forge` binary into `~/.local/bin` (override with `FORGE_INSTALL_DIR=/usr/local/bin`). Add that directory to your `PATH` if it isn't already.

Pre-built binaries cover macOS (Intel + Apple Silicon) and Linux (x86_64 + aarch64). For other targets, build from source:

```sh
cargo install --git https://github.com/forge-run/forge-cli --locked
```

## Quickstart

```sh
forge login                              # opens browser → device-flow auth
forge tenant list                        # tenants you belong to
forge tenant use <tenant-id>             # pick one
forge ws list                            # workspaces in that tenant
forge ws use <workspace-id>              # pick one
forge whoami                             # confirm you're authenticated
```

After `ws use`, every other command (`schema apply`, `deploy`, `logs`, `static upload`, `tokens`) targets the active workspace. The CLI mints a workspace-scoped bearer on demand and caches it; you generally won't need to think about tokens.

## Command reference

| Command | Purpose |
|---|---|
| `forge login` | Browser-based device flow against the portal. Default action runs `forge sso login`. Pass `--base-url` for the (deprecated) direct-workspace flow. |
| `forge logout` | Revoke the saved bearer + clear portal session and per-workspace cache. |
| `forge whoami` | Print the identity of the currently active session. |
| `forge sso login` | Run the portal-side device flow. Equivalent to `forge login` with federated defaults. |
| `forge sso connect <ws-id>` | Mint a federated bearer for a specific workspace and cache it. Pass `--raw` to print just the bearer. |
| `forge tenant list` | List tenants the portal user belongs to. |
| `forge tenant use <tenant-id>` | Set the active tenant. |
| `forge ws list` | List workspaces visible to the active portal session, scoped to the active tenant. |
| `forge ws use <workspace-id>` | Set the active workspace. Subsequent commands mint a per-workspace bearer on demand. |
| `forge new --template <name>` | Scaffold a new workload from a built-in template (`echo`, `mcp-tool`, `subscription-publisher`). |
| `forge build` | Compile a Rust crate to a WASM Component for deploy. |
| `forge deploy` | Upload `service.json` + the compiled WASM module to the active workspace. |
| `forge schema apply <file>` | Push declarative schema changes to the active workspace. |
| `forge logs` | Tail recent request log entries. `--follow` streams new entries via SSE. |
| `forge static upload <path>` | Upload static assets (CSS / JS / images) to the workspace's `/static/` mount. |
| `forge tokens mint --tier <user\|service\|admin>` | Mint a token for in-app use. |
| `forge tokens list` | List active tokens for the active workspace. |
| `forge tokens revoke <hash>` | Revoke a token by its hash prefix. |
| `forge update` | Self-update from GitHub releases. `--check` reports availability without applying; `--force` re-installs the current version. |

See `forge <command> --help` for the full flag set on any subcommand.

## Global flags + env vars

| Flag | Env | Purpose |
|---|---|---|
| `--base-url` | `FORGE_BASE_URL` | Override the workspace URL (direct-mode / break-glass). |
| `--token` | `FORGE_TOKEN` | Override the bearer. Never echoed; never logged. |
| `--profile` | — | Named profile from `~/.forge/config.toml` (direct mode). |
| `--portal-url` | `FORGE_PORTAL_URL` | Override the portal URL (default `https://app.forge.run`). |

## Self-update

```sh
forge update --check                     # report if a newer version is available
forge update                             # download + atomic-swap in place
```

`forge update` reads from this repo's GitHub releases and verifies sha256 before swapping the binary. Run periodically — the platform's wire formats evolve and an out-of-date CLI may start failing in confusing ways.

## Configuration

State lives at `~/.forge/config.toml` (mode `0600`):

```toml
config_version = 2

[portal]
base_url = "https://app.forge.run"
bearer   = "fr_u_…"

[current]
tenant_id    = "…"
workspace_id = "ws-…"

[cache.workspaces."ws-…"]
bearer      = "fr_f_…"     # per-workspace federated bearer
expires_at  = "…"
api_url     = "https://ws-….forge.run"
```

`forge logout` clears the portal session and the workspace bearer cache.

## License

Dual-licensed under [MIT](./LICENSE-MIT) or [Apache 2.0](./LICENSE-APACHE) at your option.

## Contributing

Issues and PRs welcome. The CLI is a thin HTTP client over the platform's stable wire protocol — most contributions are command UX improvements, new subcommands that wrap existing endpoints, or platform-target additions to the release matrix.
