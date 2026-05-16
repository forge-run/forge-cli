# {{crate_name}}

A tiny calculator service exposed through forge-runtime's MCP
adapter. Two query ops — `add` and `sub` — that any MCP client
can call as tools.

## Build

```bash
forge build
```

## Deploy

```bash
forge deploy --manifest service.json --wasm target/wasm32-wasip2/release/{{crate_name}}.wasm
```

## Invoke from an MCP client

forge-runtime's MCP adapter auto-derives the tool catalog from
your registered service's query ops. Connect any MCP client
(Claude Desktop, the `mcp` CLI, etc.) to the workspace:

```
mcp client https://<workspace>.forge.run/mcp \
  --auth "Bearer $FORGE_TOKEN"
```

You'll see two tools: `tools.calc.add` and `tools.calc.sub`. The
input/output schemas in `service.json` become the tool's
parameter signature.

## Or invoke via plain HTTP

```bash
curl -X POST \
  -H "Authorization: Bearer $FORGE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"a": 2, "b": 3}' \
  https://<workspace>.forge.run/api/v1/services/tools/calc/add
# => {"result": 5}
```

## What's in the source

- `src/lib.rs` dispatches on `__forge_op` so one WASM module
  serves both `add` and `sub`. Single-op modules skip the match.
- The service definition in `service.json` declares both ops with
  their JSON Schemas; forge-runtime uses these for input
  validation and MCP tool emission.

## Adding a new op

1. Add an arm to the `match` in `src/lib.rs` (e.g. `mul`).
2. Add an entry to `operations` in `service.json` with the new
   op's name + schemas.
3. `forge build && forge deploy ...`. Re-deploy is idempotent.
