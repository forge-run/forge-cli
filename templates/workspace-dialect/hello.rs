//! The `{{domain}}::hello` starter op.

use forge::schema::inputs::{{domain}}::{HelloRequest, HelloResponse};
use forge::{op, OpContext};

/// Greets the caller by the name they sent.
#[op]
#[must_use]
pub fn hello(_ctx: &OpContext, req: &HelloRequest) -> HelloResponse {
    HelloResponse {
        message: format!("hello, {}, from {{domain}}", req.name),
    }
}
