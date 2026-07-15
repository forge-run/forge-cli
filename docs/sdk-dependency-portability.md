# SDK dependency portability — decision note

Status: **scoped, not executed** (a product/release decision).
Scope: how `forge new` scaffolds reference `forge-sdk-v2` so a project builds
off the maintainer's box.

## The problem

`forge new` templates pin the SDK with a path dependency:

```toml
forge-sdk-v2 = { path = "../.." }   # in-repo template placeholder
```

which `forge new` rewrites to an absolute local path
(`/Users/rory/Documents/forge-sdk-v2`). That only resolves on the box the CLI
was built on. Every other user gets a `Cargo.toml` pointing at a directory that
does not exist — the hard blocker for the golden path off-box.

There are **two** blockers, not one:

1. **`forge-sdk-v2` is `publish = false`.** It is not on crates.io or any
   registry, so no `forge-sdk-v2 = "0.2"` version dependency resolves.
2. **`forge-sdk-v2` is not self-contained.** Its `src/lib.rs` does:

   ```rust
   wit_bindgen::generate!({ path: "../forge-runtime/wit", world: "forge-op" });
   ```

   It reads `../forge-runtime/wit/forge.wit` at compile time. Even a `git` or
   registry dependency would fail to build unless a sibling `forge-runtime`
   checkout exists — which a customer will never have.

So **making the SDK self-contained (vendoring the WIT into the crate) is a
prerequisite for every distribution option below.**

## What each fix entails

### A. Vendor the WIT, then publish to crates.io — recommended target
- Copy `forge-runtime/wit/` into `forge-sdk-v2/wit/`; change the macro to
  `path: "wit"`. Add a sync check (CI diff of the vendored WIT vs.
  `forge-runtime/wit`) so it can't drift.
- Set `publish = true`, own the `forge-sdk-v2` name on crates.io, version it,
  and cut releases in lockstep with the runtime's WIT/wasmtime bumps.
- Templates become `forge-sdk-v2 = "0.2"`; `forge new` drops the path
  substitution entirely.
- Cost: public crate ownership + a release process tied to runtime WIT changes.
  Best long-term DX (one line, no auth, offline-cacheable).

### B. Vendor the WIT, then a private/alternate registry
- Same self-containment work as (A). Publish to a private cargo registry.
- Consumers add a `[registries]` entry + auth token to `.cargo/config.toml`;
  `forge new` would scaffold that config too.
- Cost: registry hosting + per-user auth setup. Use only if the SDK must stay
  closed-source.

### C. Vendor the WIT, then a git tag dependency — recommended interim
- Same self-containment work as (A). Keep `publish = false`.
- Templates become
  `forge-sdk-v2 = { git = "https://github.com/forge-run/forge-sdk-v2.git", tag = "v0.2.0" }`.
- Cost: each build fetches from GitHub (needs network + repo read access), no
  offline crate cache on first build. Zero registry/ownership overhead — the
  fastest path to "builds off-box".

### D. Vendor the SDK source into each scaffold
- `forge new` copies the whole SDK (WIT included) into every project.
- No external dependency at all, but every project carries a copy that never
  updates, and the scaffold balloons. Rejected except as a last resort.

## Recommendation

1. **Make `forge-sdk-v2` self-contained first** (vendor `forge-runtime/wit` into
   the crate + a CI drift-check). This unblocks everything and is the real fix
   the absolute-path hack is standing in for.
2. **Ship the git-tag dependency (C) as the interim** the moment the WIT is
   vendored — smallest release surface, and it makes off-box builds work.
3. **Move to a crates.io version dependency (A)** when the SDK API is stable
   enough to commit to public releases. At that point `forge new` drops
   `resolve_sdk_dep` and the templates hardcode `forge-sdk-v2 = "0.2"`.

## What the CLI does today (interim, in `src/cmd/new.rs`)

`resolve_sdk_dep()` resolves the dependency line with this precedence, and never
writes a silent wrong path:

1. `FORGE_SDK_PATH` env var — explicit override for any checkout layout.
2. A sibling `forge-sdk-v2` next to the `forge-cli` build dir (the canonical dev
   layout), resolved from `CARGO_MANIFEST_DIR` at compile time.
3. Otherwise a visible `/path/to/forge-sdk-v2` placeholder, and `forge new`
   prints a warning telling the user to fix it.

This removes the hardcoded personal absolute path and gives off-box users a
documented resolution step, but it is a stopgap: the durable fix is (1) + (2/3)
above.
