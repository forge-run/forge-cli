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

After `ws use`, every other command (`logs`, `secrets`, `tokens`, …) targets the active workspace. The CLI mints a workspace-scoped bearer on demand and caches it; you generally won't need to think about tokens.

## Deploy is GitOps

Your git repo is the desired state. One command deploys and waits:

```sh
forge ship        # wasm-build → wasm-upload → git push → wait for the converge
```

Under the hood that's three steps, which you can also run individually:

1. **`forge wasm-build`** — compile the workspace graph to WebAssembly Components, stamp their content hashes into `forge.lock`, and persist the Components to `.forge/artifacts/`.
2. **`forge wasm-upload`** — stage those Components to the workspace's content store, so the manifest the server converges resolves every module by hash.
3. **`forge push`** (or a plain `git push`) — the server records the source tree as desired state and converges the running workspace (schema, seeds, services, pages). `forge push` then **blocks until it's actually live** and decodes the failures; a bare `git push` only records desired state and returns.

`forge dev` runs this loop for you on every save during local development. There is no imperative deploy step: the old `forge schema apply` / pre-GitOps `forge ship` commands were removed, and `forge deploy` / `forge static upload` remain only as hidden primitives `forge dev` shells out to. Scaffold a GitOps repo with `forge init`; watch a deploy converge at `GET /api/v1/manage/reconcile/status`.

> **Skipping `wasm-upload` is the classic silent failure.** If you `git push` without staging the Components first, the pushed manifest references bytes the content store doesn't hold, and the converge silently never applies (`in_sync` stays false, `live_hash` unmoved, no error). `forge ship` bundles the steps so this can't happen, and `forge push` reports exactly this state instead of returning success.

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
| `forge wasm-build` | Compile the workspace graph to WASM Components, stamp `forge.lock`, and persist the Components to `.forge/artifacts/`. Run before commit. (Aliased as `forge build`.) |
| `forge wasm-upload` | Stage the built Components (`.forge/artifacts/`) to the workspace's content store so a `git push` converge resolves each module by hash. Run after `wasm-build`, before pushing. |
| `forge ship` | The whole deploy in one: `wasm-build` → `wasm-upload` → `git push` → wait for the converge. `--no-build`, `--no-wait`, `--timeout <secs>`. |
| `forge push` | Push to the forge-git remote and block until converged, decoding the silent failure modes. `--no-wait` records without waiting; non-zero exit on stuck/error/timeout (CI-friendly). |
| `forge logs` | Tail recent request log entries. `--follow` streams new entries via SSE. |
| `forge tokens mint --tier <user\|service\|admin>` | Mint a token for in-app use. |
| `forge tokens list` | List active tokens for the active workspace. |
| `forge tokens revoke <hash>` | Revoke a token by its hash prefix. |
| `forge update` | Self-update from GitHub releases. `--check` reports availability without applying; `--force` re-installs the current version. |
| `forge dev` | Watch the project tree and re-run `forge wasm-build` + `forge deploy` (+ `forge static upload`) on every save. Inner-loop driver for the agent + human authoring path. |
| `forge pages` | List `pages/**/*.page.json` artifacts in a routing table — auth tier, capabilities, rendering shape, sibling-file flags. `--json` for machine-readable output. |
| `forge components` | List `components/**/*.component.json` artifacts with prop counts (total + required), slot counts, behavior triggers, sibling-file flags. `--json` supported. |
| `forge brand` | Print app-local branding overrides from `app.json` plus the emitted `:root { --brand-*: …; }` CSS-variable block. `--css-only` for piping into a stylesheet; `--json` for tooling. |
| `forge branch new <name>` | Fork the active workspace's substrate snapshot into a new ephemeral workspace. Pass `--source <ws-id>` to choose the source. |
| `forge branch list` | List ephemeral branches under the active tenant. `--source <ws-id>` filters by source workspace. |
| `forge branch test <id>` | Run the standard build + test pipeline against an ephemeral branch. Surfaces a job-id for log polling. |
| `forge branch promote <id> --yes` | Promote a branch's substrate back to its source workspace (destructive — requires `--yes`). |
| `forge branch discard <id> --yes` | Destroy a branch and free its storage snapshot (destructive — requires `--yes`). |
| `forge domain {add\|list\|status\|policy\|validate}` | Claim a hostname for the active tenant, poll ACME validation, update claim policy (Strict / Open). Operator path; requires `FORGE_CP_URL` + `FORGE_ADMIN_TOKEN`. |

See `forge <command> --help` for the full flag set on any subcommand.

### `forge dev` — inner-loop driver

```sh
forge dev                                    # default: watches cwd, 500ms debounce
forge dev --project-dir my-app/              # override project root
forge dev --debounce-ms 200                  # tighter debounce for fast typing
forge dev --skip-initial                     # don't run an immediate deploy on startup
```

Watches `pages/`, `components/`, `src/`, `templates/`, `static/`, `app.json`, `service.json`. On change it shells out to the same `forge` binary it's running under (`current_exe()`), so child invocations match the parent CLI version. Ctrl-C exits cleanly.

The static-upload step is best-effort: if the upload fails after a successful deploy, the watcher prints a warning and keeps running (the deploy itself already landed; assets become reachable on next upload).

### `forge pages` / `forge components` / `forge brand`

These are local-only inspection commands — they parse the on-disk substrate artifacts via `forge_schema::validate_*` and print a table. No runtime roundtrip; no `forge login` needed.

```sh
forge pages                                  # default: ./pages
forge pages --project-dir ../shop-template/
forge pages --json | jq '.[] | select(.auth == "admin")'

forge components --json | jq '.[].name'

forge brand                                  # human-readable + CSS block
forge brand --css-only > brand.css           # pipe-friendly
forge brand --json
```

Use these during migration from v1-style portals to the v0.10 substrate to confirm the manifest parses, every page route resolves, every prop schema validates, and the resolved brand cascade matches expectations before deploying.

### `forge branch` — ephemeral workspace branches

Direct-to-CP admin operations. Authentication mirrors `forge domain`:

```sh
export FORGE_CP_URL=https://cp.internal.forge.run
export FORGE_ADMIN_TOKEN=…
```

Then:

```sh
forge branch new feature-x --source ws-abc123    # fork ws-abc123 → new ephemeral workspace
forge branch list                                # all branches under the active tenant
forge branch list --source ws-abc123             # filtered to one source
forge branch test br-…                           # run build + tests against the branch
forge branch promote br-… --yes                  # merge branch substrate → source (destructive)
forge branch discard br-… --yes                  # destroy the branch snapshot
```

Branches are the agent CI/CD primitive: fork, run experimental changes, promote-or-discard. CP handlers return `501 not_implemented` until the substrate-snapshot work lands; the CLI surfaces the response verbatim so the substrate-pending state is visible.

### `forge domain` — tenant domain claims

```sh
forge domain add shop.acme.example                                # claim + start ACME
forge domain add '*.acme.example' --policy open                   # wildcard, open subdomain claims
forge domain list                                                 # validated + pending
forge domain status shop.acme.example                             # poll ACME state
forge domain validate shop.acme.example                           # nudge validation (after DNS TXT)
forge domain policy shop.acme.example open                        # flip claim policy
```

Same `FORGE_CP_URL` + `FORGE_ADMIN_TOKEN` env vars as `forge branch`.

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
