//! MCP tools for script and WASM analysis, execution traces, listeners and crypto tracing

use std::sync::MutexGuard;
use std::time::{SystemTime, UNIX_EPOCH};

use rever_core::traffic::TrafficStore;
use rever_tools_api::tool::{ToolContext, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde::Serialize;

mod args;
pub mod crypto_trace;
pub mod deob;
pub mod exec_trace;
mod js;
mod js_regex;
pub mod listeners;
pub mod scripts;
pub mod sourcemap;
pub mod wasm;
pub mod wasm_analysis;

/// Registers every tool of this crate, in the order the TypeScript server did.
pub fn register(registry: &mut ToolRegistry) {
    scripts::register(registry);
    deob::register(registry);
    sourcemap::register(registry);
    wasm::register(registry);
    exec_trace::register(registry);
    listeners::register(registry);
    crypto_trace::register(registry);
}

/// The traffic store. A panic elsewhere while it was locked does not make the
/// captured requests unreadable.
fn traffic(ctx: &ToolContext) -> MutexGuard<'_, TrafficStore> {
    ctx.traffic.lock().unwrap_or_else(|p| p.into_inner())
}

/// `ok(JSON.stringify(value, null, 2))`.
fn ok_pretty(value: &impl Serialize) -> ToolResult {
    match serde_json::to_string_pretty(value) {
        Ok(text) => ok(text),
        Err(e) => err(e.to_string()),
    }
}

/// `Date.now()`.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::Value;

    use super::*;

    #[test]
    fn 모든_도구를_ts_서버와_같은_순서로_등록한다() {
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        let names: Vec<String> = registry.defs().into_iter().map(|def| def.name).collect();
        assert_eq!(
            names,
            [
                "list_scripts",
                "grep_script",
                "grep_scripts",
                "extract_context",
                "detect_bundler",
                "deobfuscate_script",
                "deob_detect",
                "deob_auto",
                "resolve_source",
                "list_sources",
                "get_original_source",
                "list_wasm",
                "wasm_decompile",
                "wasm_info",
                "grep_wasm",
                "wasm_xref",
                "exec_trace",
                "inspect_listeners",
                "crypto_trace_start",
                "crypto_trace_list",
                "crypto_trace_stop",
            ]
        );
    }

    #[test]
    fn 스키마는_객체이고_required는_선언된_속성만_가리킨다() {
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        for def in registry.defs() {
            assert!(!def.description.is_empty(), "{}", def.name);
            let schema = &def.input_schema;
            assert_eq!(schema["type"], "object", "{}", def.name);
            let properties = schema["properties"].as_object().unwrap();
            let declared: HashSet<&str> = properties.keys().map(String::as_str).collect();
            for (key, property) in properties {
                assert!(property["type"].is_string(), "{}.{key}", def.name);
            }
            let required = schema.get("required").and_then(Value::as_array);
            for key in required.into_iter().flatten() {
                assert!(
                    declared.contains(key.as_str().unwrap()),
                    "{}: {key}",
                    def.name
                );
            }
        }
    }

    #[tokio::test]
    async fn 인자가_객체가_아니면_모든_도구가_던지지_않고_에러를_돌려준다() {
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        let ctx = ToolContext::for_tests();
        for def in registry.defs() {
            let tool = registry.find(&def.name).unwrap();
            // Tools without an input schema ignore their arguments.
            let takes_arguments = def.input_schema["properties"]
                .as_object()
                .is_some_and(|p| !p.is_empty());
            let result = tool.call(&ctx, serde_json::json!([1, 2])).await;
            if takes_arguments {
                assert!(result.is_error, "{}", def.name);
            }
        }
    }
}
