//! Subscription op that publishes events when triggered. Shows
//! customers how to use forge-sdk's `publish_event` to push a
//! payload to active subscribers.
//!
//! Subscription ops in forge-runtime are bidirectional: the op
//! invocation arrives from a client (HTTP/WS/MCP), the handler
//! decides what to publish back. A subscriber on the same op
//! receives whatever this handler emits via `publish_event`.
//!
//! What the customer learns from this template:
//! - The shape of a Subscription op handler.
//! - How `forge_sdk::publish_event` returns a typed
//!   `Result<(), PublishError>` — the customer handles
//!   `PermissionDenied` / `NoContext` / etc. with normal `?` flow.
//! - The required `subscription:publish` permission (declared in
//!   `service.json`) — without it, `publish_event` returns
//!   `PermissionDenied`.

use forge_sdk::{define_op, publish_event};
use serde_json::Value;

define_op! {
    fn handle(input: Value) -> Result<Value, String> {
        // Construct a payload from the trigger input + a marker.
        // Subscribers receive this verbatim.
        let payload = serde_json::json!({
            "trigger": input,
            "kind": "ping",
            "from": "subscription-publisher template",
        });

        // Push to all subscribers of this op. Errors are typed —
        // most commonly PermissionDenied if the op's
        // `permissions` list doesn't include `subscription:publish`.
        match publish_event(&payload) {
            Ok(()) => Ok(serde_json::json!({"published": true})),
            Err(e) => Err(format!("publish_event failed: {e}")),
        }
    }
}
