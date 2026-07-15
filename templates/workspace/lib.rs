//! {{domain}} domain — a minimal headless wasm workload.
//!
//! A bounded-context Component. The substrate re-invokes it with
//! `op_name = <op>` for each `@op:{{domain}}::<op>` page-data binding; the op
//! returns raw JSON and the runtime lifts it into `props.data.<binding>`.
//!
//! v0.2 shape: implement the `Guest` trait's `handle` and register it with
//! `export!`; dispatch on `op_context().op_name`. Gated on `wasm32` so host
//! `cargo test`/`check` don't link the Component-Model symbols.

#[cfg(target_arch = "wasm32")]
mod component_export {
    use forge_sdk_v2::{Guest, OpError, OpInput, OpOutput};

    struct Handler;

    impl Guest for Handler {
        fn handle(_input: OpInput) -> Result<OpOutput, OpError> {
            let ctx = forge_sdk_v2::op_context()
                .map_err(|e| OpError::Internal(format!("op_context unavailable: {e}")))?;
            let json = match ctx.op_name.as_str() {
                "hello" => serde_json::json!({
                    "message": "hello from {{domain}}",
                    "via": "forge-sdk-v2 workspace template",
                })
                .to_string(),
                other => {
                    return Err(OpError::BadRequest(format!(
                        "{{domain}} domain: unknown op `{other}`"
                    )))
                }
            };
            Ok(OpOutput { json })
        }
    }

    forge_sdk_v2::export!(Handler with_types_in forge_sdk_v2);
}
