//! Minimal forge-runtime workload — echoes its JSON input
//! back to the caller. The simplest valid workload shape;
//! useful as a smoke target for `forge build` + `forge deploy`.
//!
//! v0.2 shape: implement the `Guest` trait's single `handle`
//! function and register it with `export!`. The runtime invokes
//! `op.handle` for every call (HTTP / WS / Subscription / MCP);
//! input + output cross the boundary as JSON strings.
//!
//! Build:
//!   forge build
//! Deploy:
//!   forge deploy --manifest service.json --wasm \
//!       target/wasm32-wasip1/release/forge_template_echo.wasm
//! Invoke (after deploy + workspace URL):
//!   curl -X POST -H "Authorization: Bearer $TOKEN" \
//!        -H "Content-Type: application/json" \
//!        -d '{"hello":"world"}' \
//!        https://<workspace>.forge.run/api/domains/echo/echo/v1/ping

use forge_sdk_v2::{Guest, OpError, OpInput, OpOutput};
use serde_json::Value;

struct Echo;

impl Guest for Echo {
    fn handle(input: OpInput) -> Result<OpOutput, OpError> {
        // Input arrives as a JSON string; parse it so we can echo
        // the structured value back rather than the raw text.
        let parsed: Value = serde_json::from_str(&input.json)
            .map_err(|e| OpError::BadRequest(format!("invalid json: {e}")))?;

        // Echo the input verbatim under an `echoed` key, plus a
        // marker so callers can tell this came from the SDK
        // template (vs. a custom handler that might also call
        // itself echo).
        let out = serde_json::json!({
            "echoed": parsed,
            "via": "forge-sdk-v2 echo template",
        });
        Ok(OpOutput {
            json: out.to_string(),
        })
    }
}

forge_sdk_v2::export!(Echo with_types_in forge_sdk_v2);
