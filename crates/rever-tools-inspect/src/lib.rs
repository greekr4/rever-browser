//! MCP tools that decode and inspect: protobuf, tokens, GraphQL, auth, anti-bot, security headers, crypto

use std::sync::MutexGuard;

use rever_core::traffic::TrafficStore;
use rever_tools_api::tool::{ToolContext, ToolRegistry};

mod args;
mod js;

pub mod antibot;
pub mod auth;
pub mod crypto;
pub mod decode;
pub mod graphql;
pub mod headers;
pub mod protobuf;
pub mod security_inspect;

/// Registers every tool of this crate, in the order `src/main/mcp/server.ts` does.
pub fn register(registry: &mut ToolRegistry) {
    auth::register(registry);
    decode::register(registry);
    headers::register(registry);
    crypto::register(registry);
    security_inspect::register(registry);
    antibot::register(registry);
    graphql::register(registry);
    protobuf::register(registry);
}

/// The traffic store. A poisoned lock is still read: the store holds plain
/// data, and one panicked tool call must not break every later one.
fn traffic(ctx: &ToolContext) -> MutexGuard<'_, TrafficStore> {
    ctx.traffic
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the tests of every module share.
#[cfg(test)]
pub(crate) mod testing {
    use rever_core::traffic::{Headers, RequestPatch};
    use rever_tools_api::tool::{Tool, ToolContext};
    use rever_tools_api::utils::Content;
    use serde_json::Value;

    /// Calls a tool and returns whether it failed, with its text.
    pub(crate) async fn run(tool: &dyn Tool, ctx: &ToolContext, args: Value) -> (bool, String) {
        let result = tool.call(ctx, args).await;
        let text = match result.content.first() {
            Some(Content::Text { text }) => text.clone(),
            None => String::new(),
        };
        (result.is_error, text)
    }

    pub(crate) fn request(id: &str, url: &str) -> RequestPatch {
        RequestPatch {
            request_id: id.into(),
            url: Some(url.into()),
            method: Some("GET".into()),
            ..RequestPatch::default()
        }
    }

    pub(crate) fn store(ctx: &ToolContext, patch: RequestPatch) {
        let started_at = patch.started_at.unwrap_or(0);
        super::traffic(ctx).upsert(patch, started_at);
    }

    pub(crate) fn headers(pairs: &[(&str, &str)]) -> Headers {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 모든_도구를_서버와_같은_순서로_등록한다() {
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        let names: Vec<String> = registry.defs().into_iter().map(|def| def.name).collect();
        assert_eq!(
            names,
            [
                "auth_dump",
                "export_python_client",
                "export_client",
                "decode_token",
                "header_preset_list",
                "header_preset_save",
                "header_preset_apply",
                "header_preset_disable",
                "crypto_chain",
                "hash_iter",
                "hmac_compute",
                "magic_hash_lookup",
                "security_inspect",
                "detect_antibot_vm",
                "graphql_introspect",
                "protobuf_decode",
            ]
        );
    }

    #[test]
    fn 모든_입력_스키마는_객체이고_required는_정의된_속성만_가리킨다() {
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        for def in registry.defs() {
            let schema = &def.input_schema;
            assert_eq!(schema["type"], "object", "{}", def.name);
            assert!(!def.description.is_empty(), "{}", def.name);
            let properties = schema["properties"].as_object();
            assert!(properties.is_some(), "{}", def.name);
            for name in schema["required"].as_array().into_iter().flatten() {
                let known = name
                    .as_str()
                    .is_some_and(|name| properties.is_some_and(|p| p.contains_key(name)));
                assert!(known, "{}: {name}", def.name);
            }
        }
    }
}
