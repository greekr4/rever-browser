//! Outbound request primitives for tools: `raw_http` (send a raw HTTP/1.1
//! request over a socket) and, later, `grpc_call`. Both are transport, not
//! payloads: `raw_http` is gated to loopback / lab hosts by the fence.

use rever_tools_api::tool::ToolRegistry;
use serde_json::{Value, json};

pub mod grpc_call;
pub mod raw_http;

/// Registers every tool of this crate, in the order `buildMcpServer` does.
pub fn register(registry: &mut ToolRegistry) {
    raw_http::register(registry);
    grpc_call::register(registry);
}

/// The JSON Schema the MCP SDK derives from a zod shape.
pub(crate) fn schema(properties: Value, required: &[&str]) -> Value {
    let mut schema = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": properties,
    });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

/// `JSON.stringify(value, null, 2)`.
pub(crate) fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}
