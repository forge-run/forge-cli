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
//! - How to return errors (the `Err` arm maps to an `op-error`
//!   that forge-runtime translates into a tool/HTTP error).
//!
//! v0.2 shape: one `Guest::handle` entry point per module,
//! registered with `export!`. Input + output cross the boundary
//! as JSON strings.

use forge_sdk_v2::{Guest, OpError, OpInput, OpOutput};
use serde_json::Value;

struct Calc;

impl Guest for Calc {
    fn handle(input: OpInput) -> Result<OpOutput, OpError> {
        // Parse the JSON string the runtime hands us.
        let value: Value = serde_json::from_str(&input.json)
            .map_err(|e| OpError::BadRequest(format!("invalid json: {e}")))?;

        // Tiny dispatch on the `__forge_op` field so one WASM
        // module serves multiple ops. forge-runtime invokes
        // `op.handle` once per call regardless of which op the
        // caller targeted; the dispatcher threads the op name
        // through the input by convention. Single-op modules skip
        // this match.
        let op = value
            .get("__forge_op")
            .and_then(Value::as_str)
            .unwrap_or("add");
        let result = match op {
            "add" => add(&value),
            "sub" => sub(&value),
            other => Err(OpError::BadRequest(format!("unknown op `{other}`"))),
        }?;
        Ok(OpOutput {
            json: result.to_string(),
        })
    }
}

fn add(input: &Value) -> Result<Value, OpError> {
    let a = number(input, "a")?;
    let b = number(input, "b")?;
    Ok(serde_json::json!({"result": a + b}))
}

fn sub(input: &Value) -> Result<Value, OpError> {
    let a = number(input, "a")?;
    let b = number(input, "b")?;
    Ok(serde_json::json!({"result": a - b}))
}

fn number(input: &Value, key: &str) -> Result<f64, OpError> {
    input
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| OpError::BadRequest(format!("missing `{key}`")))
}

forge_sdk_v2::export!(Calc with_types_in forge_sdk_v2);
