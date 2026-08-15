# {{crate_name}}

Minimal forge-runtime workload — echoes its JSON input back to the
caller. The simplest valid workload shape, useful as a smoke
target for `forge build` + `forge deploy`.

## Build

```bash
forge build
```

Produces `target/wasm32-wasip1/release/{{crate_name}}.wasm`. The
artifact is small (~30KB stripped) because the template enables
`lto + strip + opt-level=z` in `Cargo.toml`.

## Deploy

```bash
forge deploy --manifest service.json --wasm target/wasm32-wasip1/release/{{crate_name}}.wasm
```

The manifest declares one service `echo::echo` with one query op
`ping`. After deploy, forge-runtime exposes it at
`POST /api/domains/echo/echo/v1/ping` — the service declares
`api_version: 1`, so it routes under
`/api/domains/{domain}/{service}/v{major}/{op}` (ADR-0031). A service
that declares no `api_version` keeps the legacy
`/api/v1/services/{domain}/{service}/{op}` path.

## Invoke

```bash
curl -X POST \
  -H "Authorization: Bearer $FORGE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"hello":"world"}' \
  https://<workspace>.forge.run/api/domains/echo/echo/v1/ping
```

Response:

```json
{
  "echoed": {"hello": "world"},
  "via": "forge-sdk-v2 echo template"
}
```

## What's next

- Edit `src/lib.rs` to change the handler.
- Run `forge build && forge deploy ...` again — re-deploy is
  idempotent on `(domain, name)`; the service id is preserved,
  the wasm hash updates.
- Try the `mcp-tool` or `subscription-publisher` templates for
  workloads that talk to the MCP adapter or publish events.
