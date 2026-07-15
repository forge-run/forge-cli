# {{workspace_name}}

A git-native Forge **workspace** — the unit `forge ship` builds and pushes.
Unlike a single-service crate, this is the GitOps golden path: `main` is the
desired state, and `forge ship` converges your workspace to match it.

## Layout

| Path | What |
|------|------|
| `workspace.json` | workspace identity + host→app routing (empty until you add an app) |
| `Cargo.toml` | the cargo virtual workspace (members) |
| `domains/{{domain}}/` | one bounded-context wasm module, op `{{domain}}::hello` |

## Ship it

```bash
forge login
forge ws use <workspace-id>
git remote add forge https://git.forge.run/<workspace-id>/{{workspace_name}}
forge ship
```

`forge ship` runs `wasm-build → wasm-upload → git push`, then waits for the
converge. It fails closed if the converge doesn't go live.

## Build locally

```bash
forge wasm-build
```

Compiles the workspace graph, stamps `forge.lock`, and writes the Components to
`.forge/artifacts/` for `forge wasm-upload` to stage.

## What's next

- Add an **app** (`apps/<name>/app.json` + `pages/`) and route a host to it in
  `workspace.json` `hosts`, then bind this op from a page with
  `@op:{{domain}}::hello`.
- Add more ops to `domains/{{domain}}/service.json` + a match arm in
  `services/lib.rs`.
- Add more domains as `domains/<name>/` workspace members.
