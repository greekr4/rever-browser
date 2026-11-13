//! Port of `src/main/mcp/tools/listeners.ts` — inspect_listeners: the event
//! listeners on an element and its ancestors, with where each handler lives.
//!
//! Reading listeners needs the Chrome DevTools Protocol (DOMDebugger
//! .getEventListeners on remote objects), which the `Browser` trait does not
//! offer and page JavaScript cannot do: after validating its arguments the tool
//! reports that as unsupported. The report built from the listeners found is
//! ported, so only the protocol round-trips are missing.

use rever_core::traffic::{ListFilter, TrafficStore};
use rever_formats::listeners::{
    LISTENER_LIMITATIONS, ListenerOwner, RawListener, SnippetOptions, normalize_listener,
    snippet_at,
};
use rever_formats::script_registry::ScriptRegistry;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry, unsupported};
use rever_tools_api::utils::{ToolResult, err};
use serde_json::{Value, json};

use crate::args::Args;

/// One listener as the protocol reports it, with what was read of its handler.
#[derive(Clone, Debug)]
pub struct FoundListener {
    pub raw: RawListener,
    /// `Function.prototype.toString` of the handler object, when it has one.
    pub handler_text: Option<String>,
    /// The whole script (Debugger.getScriptSource), the fallback for the above.
    pub script_source: Option<String>,
}

/// The element, then (optionally) every ancestor up through shadow hosts to
/// document and window — where React/Vue-style delegated handlers live.
#[derive(Clone, Debug)]
pub struct ChainNode {
    /// From `rever_formats::listeners::node_label`.
    pub label: String,
    pub listeners: Vec<FoundListener>,
}

fn handler_source(found: &FoundListener, max_chars: usize) -> String {
    let opts = SnippetOptions {
        max_chars: Some(max_chars),
        ..SnippetOptions::default()
    };
    fn non_empty(text: &Option<String>) -> Option<&str> {
        text.as_deref().filter(|t| !t.is_empty())
    }
    if let Some(text) = non_empty(&found.handler_text) {
        return snippet_at(text, 0, 0, opts);
    }
    match non_empty(&found.script_source) {
        Some(source) => snippet_at(source, found.raw.line_number, found.raw.column_number, opts),
        None => String::new(),
    }
}

/// The tool's JSON for the listeners found along `chain` (index = depth).
/// `types`, when not empty, keeps only those event types.
pub fn listeners_report(
    store: &TrafficStore,
    registry: &ScriptRegistry,
    wc_id: u64,
    session_id: Option<&str>,
    chain: &[ChainNode],
    types: Option<&[&str]>,
    max_source_chars: usize,
) -> Result<String, String> {
    let wanted = types.filter(|types| !types.is_empty());
    let scripts = store.list(&ListFilter {
        method_or_type: Some("Script".into()),
        ..ListFilter::default()
    });
    let mut listeners = Vec::new();
    for (depth, node) in chain.iter().enumerate() {
        let owner = ListenerOwner {
            depth: u32::try_from(depth).unwrap_or(u32::MAX),
            label: node.label.clone(),
        };
        for found in &node.listeners {
            let raw = &found.raw;
            if wanted.is_some_and(|wanted| !wanted.contains(&raw.r#type.as_str())) {
                continue;
            }
            let url = registry.script_url(wc_id, session_id, &raw.script_id);
            let script_request_id =
                url.and_then(|url| scripts.iter().find(|r| r.url == url).map(|r| &r.request_id));
            let mut entry = serde_json::to_value(normalize_listener(raw, &owner, url))
                .map_err(|e| e.to_string())?;
            if let Some(fields) = entry.as_object_mut() {
                // Ready-made resolve_source arguments (it takes 0-based positions).
                let resolve_source = script_request_id.map(|request_id| {
                    json!({ "requestId": request_id, "line": raw.line_number, "column": raw.column_number })
                });
                fields.insert("resolveSource".into(), json!(resolve_source));
                fields.insert(
                    "source".into(),
                    json!(handler_source(found, max_source_chars)),
                );
            }
            listeners.push(entry);
        }
    }
    let searched: Vec<&str> = chain.iter().map(|node| node.label.as_str()).collect();
    serde_json::to_string_pretty(&json!({
        "element": searched.first(),
        "searched": searched,
        "count": listeners.len(),
        "listeners": listeners,
        "limitations": LISTENER_LIMITATIONS,
    }))
    .map_err(|e| e.to_string())
}

pub struct InspectListeners;

impl Tool for InspectListeners {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "inspect_listeners",
            "List the event listeners on an element — type, flags, `url:line:col` of the handler, and its source. Use it to go button → handler → request before clicking. ancestors=true (default) also walks parents up to document/window, where React/Vue-style frameworks register their delegated handlers. Pass the `resolveSource` object of a listener to resolve_source for the original (source-mapped) location. Never invokes a handler.",
            json!({
                "type": "object",
                "properties": {
                    "ref": { "type": "string", "description": "Element ref from browser_snapshot (e.g. \"r12\")" },
                    "selector": { "type": "string", "description": "CSS selector (first match, main document)" },
                    "ancestors": { "type": "boolean", "description": "Also list listeners on ancestors, document and window (default true)" },
                    "types": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Only these event types, e.g. [\"click\",\"submit\"] — cuts window-level noise"
                    },
                    "maxSourceChars": { "type": "integer", "exclusiveMinimum": 0, "maximum": 10000, "description": "Per-handler source cap (default 1500)" }
                }
            }),
        )
    }

    fn call<'a>(&'a self, _: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { inspect_listeners(&args).unwrap_or_else(err) })
    }
}

fn inspect_listeners(args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("inspect_listeners", args)?;
    let element_ref = args.opt_str("ref")?.filter(|r| !r.is_empty());
    let selector = args.opt_str("selector")?.filter(|s| !s.is_empty());
    // Checked like the TypeScript's schema does; with no listeners to read,
    // nothing uses the values yet (defaults: ancestors true, 1,500 characters).
    args.opt_bool("ancestors")?;
    args.opt_str_array("types")?;
    args.opt_int("maxSourceChars", 1, Some(10_000))?;
    if element_ref.is_some() == selector.is_some() {
        return Err("pass exactly one of ref or selector".into());
    }
    Err(unsupported("DOMDebugger.getEventListeners"))
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(InspectListeners);
}

#[cfg(test)]
mod tests {
    use rever_core::traffic::RequestPatch;
    use rever_tools_api::utils::Content;

    use super::*;

    async fn call(args: Value) -> (bool, String) {
        let ctx = ToolContext::for_tests();
        let result = InspectListeners.call(&ctx, args).await;
        let text = match result.content.first() {
            Some(Content::Text { text }) => text.clone(),
            None => String::new(),
        };
        (result.is_error, text)
    }

    #[tokio::test]
    async fn ref와_selector_중_정확히_하나를_요구한다() {
        let expected = (true, "pass exactly one of ref or selector".to_owned());
        assert_eq!(call(json!({})).await, expected);
        assert_eq!(
            call(json!({ "ref": "r1", "selector": "#buy" })).await,
            expected
        );
        // An empty string counts as not given, as it is falsy in JavaScript.
        assert_eq!(call(json!({ "ref": "", "selector": "" })).await, expected);
    }

    #[tokio::test]
    async fn 대상을_정하면_리스너_조회를_지원하지_않는다고_알린다() {
        let expected = (true, unsupported("DOMDebugger.getEventListeners"));
        assert_eq!(call(json!({ "ref": "r12" })).await, expected);
        assert_eq!(
            call(json!({ "selector": "#buy", "ref": "" })).await,
            expected
        );
        assert_eq!(
            call(json!({ "selector": "#buy", "ancestors": false, "types": ["click"], "maxSourceChars": 10000 }))
                .await,
            expected
        );
    }

    #[tokio::test]
    async fn 인자를_스키마대로_검사한다() {
        for args in [
            json!({ "ref": 12 }),
            json!({ "selector": ["#a"] }),
            json!({ "ref": "r1", "ancestors": "true" }),
            json!({ "ref": "r1", "types": "click" }),
            json!({ "ref": "r1", "types": ["click", 3] }),
            json!({ "ref": "r1", "maxSourceChars": 0 }),
            json!({ "ref": "r1", "maxSourceChars": 10001 }),
        ] {
            let (is_error, text) = call(args.clone()).await;
            assert!(is_error, "{args}");
            assert!(
                text.starts_with(
                    "Input validation error: Invalid arguments for tool inspect_listeners: "
                ),
                "{text}"
            );
        }
    }

    // ── the report built from the listeners found ───────────────────────────

    fn listener(kind: &str, script_id: &str, line: i64, column: i64) -> FoundListener {
        FoundListener {
            raw: RawListener {
                r#type: kind.into(),
                use_capture: false,
                passive: false,
                once: false,
                script_id: script_id.into(),
                line_number: line,
                column_number: column,
            },
            handler_text: None,
            script_source: None,
        }
    }

    fn fixture() -> (TrafficStore, ScriptRegistry, Vec<ChainNode>) {
        let mut store = TrafficStore::default();
        store.upsert(
            RequestPatch {
                request_id: "bundle".into(),
                url: Some("http://h/app.js".into()),
                resource_type: Some("Script".into()),
                ..RequestPatch::default()
            },
            1,
        );
        let mut registry = ScriptRegistry::new();
        registry.record_script(1, None, "7", "http://h/app.js");
        registry.record_script(1, Some("frame"), "7", "http://h/frame.js");
        let chain = vec![
            ChainNode {
                label: "button#buy.btn".into(),
                listeners: vec![FoundListener {
                    handler_text: Some("function onBuy(e) { pay(e) }".into()),
                    script_source: Some("ignored when the handler has text".into()),
                    ..listener("click", "7", 0, 20)
                }],
            },
            ChainNode {
                label: "document".into(),
                listeners: vec![
                    FoundListener {
                        script_source: Some("first line\nsecond line".into()),
                        ..listener("keydown", "99", 1, 0)
                    },
                    listener("click", "99", 4, 2),
                ],
            },
            ChainNode {
                label: "window".into(),
                listeners: vec![],
            },
        ];
        (store, registry, chain)
    }

    #[test]
    fn 리스너를_위치와_소스와_함께_보고한다() {
        let (store, registry, chain) = fixture();
        let text = listeners_report(&store, &registry, 1, None, &chain, None, 1_500).unwrap();
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["element"], "button#buy.btn");
        assert_eq!(
            out["searched"],
            json!(["button#buy.btn", "document", "window"])
        );
        assert_eq!(out["count"], 3);
        assert_eq!(out["limitations"], json!(LISTENER_LIMITATIONS));
        // The handler's own text wins; its script is a captured request, so the
        // 0-based position is offered to resolve_source.
        assert_eq!(
            out["listeners"][0],
            json!({
                "type": "click",
                "on": "self (button#buy.btn)",
                "capture": false,
                "passive": false,
                "once": false,
                "location": "http://h/app.js:1:21",
                "url": "http://h/app.js",
                "line": 1,
                "column": 21,
                "resolveSource": { "requestId": "bundle", "line": 0, "column": 20 },
                "source": "function onBuy(e) { pay(e) }"
            })
        );
        // An unknown script: no URL, nothing to resolve, source cut from the script text.
        let keydown = &out["listeners"][1];
        assert_eq!(keydown["on"], "ancestor +1 (document)");
        assert_eq!(keydown["location"], "script#99:2:1");
        assert_eq!(keydown["url"], Value::Null);
        assert_eq!(keydown["resolveSource"], Value::Null);
        assert_eq!(keydown["source"], "second line");
        assert_eq!(out["listeners"][2]["source"], "");
        let keys: Vec<&str> = out
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["element", "searched", "count", "listeners", "limitations"]
        );
    }

    #[test]
    fn 이벤트_종류로_거르고_소스_길이를_제한한다() {
        let (store, registry, chain) = fixture();
        let text =
            listeners_report(&store, &registry, 1, None, &chain, Some(&["click"]), 10).unwrap();
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["count"], 2);
        assert_eq!(out["listeners"][0]["source"], "function o…");
        assert_eq!(out["listeners"][1]["on"], "ancestor +1 (document)");
        // An empty list filters nothing.
        let text = listeners_report(&store, &registry, 1, None, &chain, Some(&[]), 10).unwrap();
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["count"], 3);
    }

    #[test]
    fn 스크립트_url은_세션별로_찾고_요소가_없으면_null이다() {
        let (store, registry, chain) = fixture();
        let text =
            listeners_report(&store, &registry, 1, Some("frame"), &chain, None, 1_500).unwrap();
        let out: Value = serde_json::from_str(&text).unwrap();
        // frame.js is known to the registry but was never captured.
        assert_eq!(out["listeners"][0]["url"], "http://h/frame.js");
        assert_eq!(out["listeners"][0]["resolveSource"], Value::Null);

        let text = listeners_report(&store, &registry, 1, None, &[], None, 1_500).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({
                "element": null,
                "searched": [],
                "count": 0,
                "listeners": [],
                "limitations": LISTENER_LIMITATIONS
            })
        );
    }
}
