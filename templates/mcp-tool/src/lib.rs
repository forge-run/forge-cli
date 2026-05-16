//! MCP tool — a small calculator service surfaced via the MCP
//! adapter. forge-runtime auto-derives the MCP tool catalog from
//! the registered service's `OperationKind::Query` ops, so this
//! handler shows up as the tool `calc.add` in any MCP client
//! that connects to the workspace.
//!
//! What the customer learns from this template:
//! - How to expose multiple ops in one service.
//! - How input_schema / output_schema in `service.json` shape
//!   the MCP tool's parameter signature.
//! - How to return structured errors (the `Err` arm of the
//!   handler return type lands as a `{ "error": "<msg>" }`
//!   response that forge-runtime maps to a tool error).

use forge_sdk::define_op;
use serde_json::Value;

define_op! {
    fn handle(input: Value) -> Result<Value, String> {
        // Tiny dispatch on the `op` field so one WASM module
        // serves multiple ops. (Forge-runtime invokes
        // `forge_op` once per call regardless of which op the
        // caller targeted; the wrapper threads the op name
        // through the input by convention. Customers who only
        // have one op skip this match.)
        let op = input
            .get("__forge_op")
            .and_then(Value::as_str)
            .unwrap_or("add");
        match op {
            "add" => add(&input),
            "sub" => sub(&input),
            other => Err(format!("unknown op `{other}`")),
        }
    }
}

fn add(input: &Value) -> Result<Value, String> {
    let a = input.get("a").and_then(Value::as_f64).ok_or("missing `a`")?;
    let b = input.get("b").and_then(Value::as_f64).ok_or("missing `b`")?;
    Ok(serde_json::json!({"result": a + b}))
}

fn sub(input: &Value) -> Result<Value, String> {
    let a = input.get("a").and_then(Value::as_f64).ok_or("missing `a`")?;
    let b = input.get("b").and_then(Value::as_f64).ok_or("missing `b`")?;
    Ok(serde_json::json!({"result": a - b}))
}
