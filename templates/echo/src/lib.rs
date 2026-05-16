//! Minimal forge-runtime workload — echoes its JSON input
//! back to the caller. The simplest valid workload shape;
//! useful as a smoke target for `forge build` + `forge deploy`.
//!
//! Build:
//!   forge build
//! Deploy:
//!   forge deploy --manifest service.json --wasm \
//!       target/wasm32-wasip2/release/{{crate_name}}.wasm
//! Invoke (after deploy + workspace URL):
//!   curl -X POST -H "Authorization: Bearer $TOKEN" \
//!        -H "Content-Type: application/json" \
//!        -d '{"hello":"world"}' \
//!        https://<workspace>.forge.run/api/v1/services/echo/ping

use forge_sdk::define_op;
use serde_json::Value;

define_op! {
    fn handle(input: Value) -> Result<Value, String> {
        // Echo the input verbatim under an `echoed` key, plus a
        // marker so callers can tell this came from the SDK
        // template (vs. a custom handler that might also call
        // itself echo).
        Ok(serde_json::json!({
            "echoed": input,
            "via": "forge-sdk echo template",
        }))
    }
}
