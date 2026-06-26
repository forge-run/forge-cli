//! Subscription op that publishes events when triggered. Shows
//! customers how to use forge-sdk-v2's `publish_event` to push a
//! payload to active subscribers.
//!
//! Subscription ops in forge-runtime are bidirectional: the op
//! invocation arrives from a client (HTTP/WS/MCP), the handler
//! decides what to publish back. A subscriber on the same op
//! receives whatever this handler emits via `publish_event`.
//!
//! What the customer learns from this template:
//! - The shape of a Subscription op handler.
//! - How `forge_sdk_v2::publish_event` returns
//!   `Result<(), String>` — the customer handles the failure with
//!   normal `?`/`match` flow.
//! - The required `subscription:publish` permission (declared in
//!   `service.json`) — without it, `publish_event` returns an
//!   error.
//!
//! v0.2 shape: one `Guest::handle` entry point, registered with
//! `export!`. Input + output cross the boundary as JSON strings.

use forge_sdk_v2::{Guest, OpError, OpInput, OpOutput, publish_event};
use serde_json::Value;

struct Publisher;

impl Guest for Publisher {
    fn handle(input: OpInput) -> Result<OpOutput, OpError> {
        // Parse the trigger input the runtime hands us.
        let trigger: Value = serde_json::from_str(&input.json)
            .map_err(|e| OpError::BadRequest(format!("invalid json: {e}")))?;

        // Construct a payload from the trigger input + a marker.
        // Subscribers receive this verbatim.
        let payload = serde_json::json!({
            "trigger": trigger,
            "kind": "ping",
            "from": "subscription-publisher template",
        });

        // Push to all subscribers of this op. The most common
        // failure is a missing `subscription:publish` permission
        // in the op's `permissions` list.
        publish_event(&payload)
            .map_err(|e| OpError::Internal(format!("publish_event failed: {e}")))?;

        let out = serde_json::json!({"published": true});
        Ok(OpOutput {
            json: out.to_string(),
        })
    }
}

forge_sdk_v2::export!(Publisher with_types_in forge_sdk_v2);
