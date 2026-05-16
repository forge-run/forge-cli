# forge-cli

Command-line client for the [Forge platform](https://app.forge.run).

Sign in via your browser, pick a tenant + workspace, then deploy schemas, push WASM workloads, tail logs, and manage tokens — all over HTTPS against your workspace's runtime.

## Install

```sh
curl -sSf https://raw.githubusercontent.com/forge-run/forge-cli/main/install.sh | sh
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

## Common commands

```sh
forge build                              # compile a Rust crate to WASM
forge deploy                             # upload service.json + WASM to the workspace
forge schema apply schema.json           # push declarative schema changes
forge logs --follow                      # tail recent requests
forge static upload ./dist               # ship static assets (CSS / JS / images)
forge tokens mint --tier user            # mint a service / user token for in-app use
```

See `forge <command> --help` for the full flag set.

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
