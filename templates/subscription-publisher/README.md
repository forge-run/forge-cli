# {{crate_name}}

Subscription op that publishes events to active subscribers when
triggered. Demonstrates forge-sdk's `publish_event` API.

## Build

```bash
forge build
```

## Deploy

```bash
forge deploy --manifest service.json --wasm target/wasm32-wasip2/release/{{crate_name}}.wasm
```

## Subscribe (one terminal)

forge-runtime exposes Subscription ops via Server-Sent Events:

```bash
curl -N \
  -H "Authorization: Bearer $FORGE_TOKEN" \
  -H "Accept: text/event-stream" \
  https://<workspace>.forge.run/api/v1/services/events/pings/trigger
```

Leave that running.

## Trigger (another terminal)

```bash
curl -X POST \
  -H "Authorization: Bearer $FORGE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"reason": "manual test"}' \
  https://<workspace>.forge.run/api/v1/services/events/pings/trigger
```

The subscriber's stream prints the published payload.

## Anatomy

- The handler in `src/lib.rs` builds a payload, calls
  `publish_event(&payload)`, and returns `{"published": true}`.
- `service.json` declares the op as `kind: "subscription"` AND
  grants the `subscription:publish` permission. Without that
  permission, `publish_event` returns `PermissionDenied` and the
  handler surfaces an error.
- For directed publish (one specific subscriber instead of fan-
  out), use `forge_sdk::publish_event_to(subscription_id, &payload)`.
