//! Active injection / fuzzing probes: send crafted variants of a captured
//! request and report what the server did. Ported from `src/main/mcp/tools/
//! *_probe.ts` and `crlf.ts`.
//!
//! Every probe sends through [`rever_tools_http::send`], so the
//! capture-not-send fence and the session scope are enforced in one place — a
//! probe never opens its own socket.

use rever_tools_api::tool::ToolRegistry;
use serde_json::{Value, json};

mod args;
mod cmd;
mod common;
mod crlf;
mod nosqli;
mod path_write;
mod payload;
mod pickle;
mod sqli;
mod ssti;

/// Registers every tool of this crate, in the order `buildMcpServer` does.
pub fn register(registry: &mut ToolRegistry) {
    registry.register(crlf::CrlfTest);
    registry.register(sqli::SqliProbe);
    registry.register(nosqli::NosqliProbe);
    registry.register(ssti::SstiProbe);
    registry.register(cmd::CmdProbe);
    registry.register(payload::PayloadProbe);
    registry.register(path_write::PathWriteProbe);
    registry.register(pickle::PickleGadget);
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
