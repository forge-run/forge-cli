use forge::{OpContext, Value};
use serde::Serialize;

#[derive(Serialize)]
pub struct Greeting {
    message: String,
}

pub fn hello(ctx: &OpContext, input: &Value) -> Greeting {
    let name: String = input.get_str("name", "world");
    Greeting {
        message: format!("hello, {}, from {{domain}}", name),
    }
}
