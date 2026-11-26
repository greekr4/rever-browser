//! Port of `src/main/mcp/tools/exec_trace.ts` — exec_trace: record which
//! JavaScript functions run during an action, via V8 precise coverage.
//!
//! The recording itself needs the Chrome DevTools Protocol's Profiler domain,
//! which the `Browser` trait does not offer: `start` (and a `stop` that would
//! have to sample) report that as unsupported. Argument validation, the
//! per-tab state and the report built from a coverage sample are ported.

use std::collections::HashMap;
use std::sync::Mutex;

use rever_core::traffic::{ListFilter, TrafficStore};
use rever_formats::exec_trace::{
    ScriptCoverage, ScriptInfo, StackFrame, TraceInput, TraceRequest, executed_functions,
    is_traceable_script, summarize_trace,
};
use rever_formats::script_registry::ScriptRegistry;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry, unsupported};
use rever_tools_api::utils::{ResultStore, ToolResult, err, ok};
use serde_json::{Map, Value, json};

use crate::args::Args;
use crate::now_ms;

/// A recording in progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trace {
    /// Epoch milliseconds.
    pub started_at: u64,
    pub limit: usize,
    pub url_filter: Option<String>,
    pub include_anonymous: bool,
}

/// The TypeScript keeps one trace per tab, by webContents id; the `Browser`
/// trait addresses a single tab, so one slot of each is all there is. A
/// finished auto-stop parks its result until the next `stop` so it isn't lost.
#[derive(Default)]
pub struct ExecTraceState {
    pub trace: Mutex<Option<Trace>>,
    pub auto_stopped: Mutex<Option<String>>,
}

/// The second half of the TypeScript's `finish`: turns the coverage sample
/// taken at `stopped_at` into the tool's JSON. `script_source` is what
/// Debugger.getScriptSource returns for a scriptId.
#[allow(clippy::too_many_arguments)]
pub fn trace_report(
    store: &TrafficStore,
    registry: &ScriptRegistry,
    wc_id: u64,
    trace: &Trace,
    coverage: Vec<ScriptCoverage>,
    script_source: &dyn Fn(&str) -> Option<String>,
    stopped_at: u64,
    auto: bool,
) -> Result<String, String> {
    let url_of = |c: &ScriptCoverage| {
        registry
            .script_url(wc_id, None, &c.script_id)
            .unwrap_or(&c.url)
            .to_owned()
    };
    let coverage: Vec<ScriptCoverage> = coverage
        .into_iter()
        .filter(|c| {
            let url = url_of(c);
            is_traceable_script(&url, trace.include_anonymous)
                && trace
                    .url_filter
                    .as_deref()
                    .is_none_or(|filter| url.contains(filter))
        })
        .collect();

    // Source text only for scripts that actually ran something — needed to turn
    // offsets into line:col and to place request call sites inside functions.
    let script_bodies = store.list(&ListFilter {
        method_or_type: Some("Script".into()),
        ..ListFilter::default()
    });
    let mut scripts = HashMap::new();
    for c in &coverage {
        if executed_functions(c).is_empty() {
            continue;
        }
        let url = url_of(c);
        let request_id = script_bodies
            .iter()
            .find(|r| !url.is_empty() && r.url == url)
            .map(|r| r.request_id.clone());
        scripts.insert(
            c.script_id.clone(),
            ScriptInfo {
                url,
                source: script_source(&c.script_id),
                request_id,
            },
        );
    }

    let requests: Vec<TraceRequest> = store
        .list(&ListFilter {
            since: Some(trace.started_at),
            ..ListFilter::default()
        })
        .into_iter()
        .filter(|r| r.started_at <= stopped_at && r.resource_type != "Script")
        .map(|r| TraceRequest {
            request_id: r.request_id.clone(),
            method: r.method.clone(),
            url: r.url.clone(),
            status: r.status,
            initiator_stack: r.initiator_stack.as_ref().map(|stack| {
                stack
                    .iter()
                    .map(|frame| StackFrame {
                        function_name: frame.function_name.clone(),
                        url: frame.url.clone(),
                        line_number: frame.line_number,
                        column_number: frame.column_number,
                    })
                    .collect()
            }),
        })
        .collect();

    let summary = summarize_trace(TraceInput {
        coverage: &coverage,
        scripts: &scripts,
        requests: &requests,
        limit: trace.limit,
    });
    let mut out = Map::new();
    out.insert(
        "windowMs".into(),
        json!(stopped_at.saturating_sub(trace.started_at)),
    );
    if auto {
        out.insert("note".into(), json!("auto-stopped at the time limit"));
    }
    if let Value::Object(fields) = serde_json::to_value(&summary).map_err(|e| e.to_string())? {
        out.extend(fields);
    }
    serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
}

pub struct ExecTrace;

impl Tool for ExecTrace {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "exec_trace",
            "[NOT SUPPORTED IN THIS BUILD] Recording needs V8 precise coverage, which the engine does not expose: start and stop both answer that it is not supported. Record which JS functions run during an action, via V8 precise coverage. action=start → perform the action (browser_click, or ask the user to click) → action=stop returns every function called in that window (url:line:col, call count), the requests made, and onRequestStack marking functions a request's initiator stack passed through — the fastest way to find request-signing code in a large bundle. Feed a function's resolveSource to resolve_source. The page runs slower while recording; always stop (auto-stops after maxSeconds).",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["start", "stop", "status"] },
                    "maxSeconds": { "type": "integer", "exclusiveMinimum": 0, "maximum": 600, "description": "start: auto-stop after this many seconds (default 120)" },
                    "limit": { "type": "integer", "exclusiveMinimum": 0, "maximum": 2000, "description": "start: max functions returned (default 150)" },
                    "urlFilter": { "type": "string", "description": "start: only scripts whose URL contains this" },
                    "includeAnonymous": { "type": "boolean", "description": "start: also include URL-less scripts (eval / new Function) — default false, they are mostly rever's own helpers" }
                },
                "required": ["action"]
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { exec_trace(ctx, &args).unwrap_or_else(err) })
    }
}

fn exec_trace(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("exec_trace", args)?;
    let action = args.enumeration("action", &["start", "stop", "status"])?;
    // Checked like the TypeScript's schema does; with no Profiler to start,
    // nothing reads the values yet (defaults: 120 s, 150 functions, false).
    args.opt_int("maxSeconds", 1, Some(600))?;
    args.opt_int("limit", 1, Some(2_000))?;
    args.opt_str("urlFilter")?;
    args.opt_bool("includeAnonymous")?;

    let state = ctx.state::<ExecTraceState>();
    let mut trace = state.trace.lock().unwrap_or_else(|p| p.into_inner());
    let mut auto_stopped = state.auto_stopped.lock().unwrap_or_else(|p| p.into_inner());

    if action == "status" {
        let elapsed = trace
            .as_ref()
            .map(|t| now_ms().saturating_sub(t.started_at));
        return Ok(ok(json!({
            "running": trace.is_some(),
            "elapsedMs": elapsed,
            "autoStoppedResultWaiting": auto_stopped.is_some(),
        })
        .to_string()));
    }

    if action == "stop" {
        if let Some(parked) = auto_stopped.take() {
            let results = ctx.state::<Mutex<ResultStore>>();
            let mut results = results.lock().unwrap_or_else(|p| p.into_inner());
            return Ok(results.ok_budgeted(parked, None));
        }
        // As in the TypeScript's finish(), the trace is over once stop is asked
        // for, whether or not its sample could be taken.
        if trace.take().is_none() {
            return Err("no exec_trace running — call exec_trace action=start first".into());
        }
        return Err(unsupported("Profiler.takePreciseCoverage"));
    }

    if trace.is_some() {
        return Err("exec_trace already running on this tab — call action=stop first".into());
    }
    *auto_stopped = None;
    Err(unsupported("Profiler.startPreciseCoverage"))
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ExecTrace);
}

#[cfg(test)]
mod tests {
    use rever_core::traffic::{CallFrame, RequestPatch};
    use rever_tools_api::utils::Content;

    use super::*;

    async fn call(ctx: &ToolContext, args: Value) -> (bool, String) {
        let result = ExecTrace.call(ctx, args).await;
        let text = match result.content.first() {
            Some(Content::Text { text }) => text.clone(),
            None => String::new(),
        };
        (result.is_error, text)
    }

    fn running(started_at: u64) -> Trace {
        Trace {
            started_at,
            limit: 150,
            url_filter: None,
            include_anonymous: false,
        }
    }

    #[tokio::test]
    async fn 기록_중이_아니면_status는_모두_비어_있다() {
        let ctx = ToolContext::for_tests();
        assert_eq!(
            call(&ctx, json!({ "action": "status" })).await,
            (
                false,
                r#"{"running":false,"elapsedMs":null,"autoStoppedResultWaiting":false}"#.to_owned()
            )
        );
    }

    #[tokio::test]
    async fn status는_기록_중인_시간과_기다리는_결과를_알린다() {
        let ctx = ToolContext::for_tests();
        let state = ctx.state::<ExecTraceState>();
        *state.trace.lock().unwrap() = Some(running(now_ms() - 1_500));
        *state.auto_stopped.lock().unwrap() = Some("{}".into());
        let (is_error, text) = call(&ctx, json!({ "action": "status" })).await;
        assert!(!is_error);
        let status: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(status["running"], true);
        assert_eq!(status["autoStoppedResultWaiting"], true);
        let elapsed = status["elapsedMs"].as_u64().unwrap();
        assert!((1_500..60_000).contains(&elapsed), "{elapsed}");
    }

    #[tokio::test]
    async fn 시작하지_않고_stop하면_에러다() {
        let ctx = ToolContext::for_tests();
        assert_eq!(
            call(&ctx, json!({ "action": "stop" })).await,
            (
                true,
                "no exec_trace running — call exec_trace action=start first".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn start는_profiler가_없어_지원하지_않는다고_알린다() {
        let ctx = ToolContext::for_tests();
        let state = ctx.state::<ExecTraceState>();
        *state.auto_stopped.lock().unwrap() = Some("old".into());
        assert_eq!(
            call(&ctx, json!({ "action": "start", "maxSeconds": 5 })).await,
            (true, unsupported("Profiler.startPreciseCoverage"))
        );
        // Starting drops a result parked by an earlier auto-stop.
        assert_eq!(*state.auto_stopped.lock().unwrap(), None);
        assert_eq!(*state.trace.lock().unwrap(), None);
    }

    #[tokio::test]
    async fn 이미_기록_중이면_start를_거절한다() {
        let ctx = ToolContext::for_tests();
        let state = ctx.state::<ExecTraceState>();
        *state.trace.lock().unwrap() = Some(running(1));
        assert_eq!(
            call(&ctx, json!({ "action": "start" })).await,
            (
                true,
                "exec_trace already running on this tab — call action=stop first".to_owned()
            )
        );
        assert!(state.trace.lock().unwrap().is_some());
    }

    #[tokio::test]
    async fn stop은_자동_종료된_결과를_한_번_돌려준다() {
        let ctx = ToolContext::for_tests();
        let state = ctx.state::<ExecTraceState>();
        *state.auto_stopped.lock().unwrap() = Some("{\n  \"windowMs\": 5\n}".into());
        assert_eq!(
            call(&ctx, json!({ "action": "stop" })).await,
            (false, "{\n  \"windowMs\": 5\n}".to_owned())
        );
        assert!(call(&ctx, json!({ "action": "stop" })).await.0);
    }

    #[tokio::test]
    async fn 큰_결과는_예산에_맞춰_자르고_이어_읽을_수_있게_둔다() {
        let ctx = ToolContext::for_tests();
        let state = ctx.state::<ExecTraceState>();
        *state.auto_stopped.lock().unwrap() = Some("x".repeat(12_001));
        let (is_error, text) = call(&ctx, json!({ "action": "stop" })).await;
        assert!(!is_error);
        assert!(
            text.ends_with(
                "[truncated: showing 12000 of 12001 chars. Call tool_result_more with resultId=\"res_1\", offset=12000 for the rest.]"
            ),
            "{}",
            text.get(12_000..).unwrap_or("")
        );
        let results = ctx.state::<Mutex<ResultStore>>();
        let rest = results
            .lock()
            .unwrap()
            .get_result_slice("res_1", 12_000, None);
        assert_eq!(rest.map(|page| page.slice), Some("x".to_owned()));
    }

    #[tokio::test]
    async fn 기록_중에_stop하면_기록을_끝내고_지원하지_않는다고_알린다() {
        let ctx = ToolContext::for_tests();
        let state = ctx.state::<ExecTraceState>();
        *state.trace.lock().unwrap() = Some(running(1));
        assert_eq!(
            call(&ctx, json!({ "action": "stop" })).await,
            (true, unsupported("Profiler.takePreciseCoverage"))
        );
        assert_eq!(*state.trace.lock().unwrap(), None);
    }

    #[tokio::test]
    async fn 인자를_스키마대로_검사한다() {
        let ctx = ToolContext::for_tests();
        for args in [
            json!({}),
            json!({ "action": "pause" }),
            json!({ "action": "status", "maxSeconds": 0 }),
            json!({ "action": "status", "maxSeconds": 601 }),
            json!({ "action": "status", "limit": 2001 }),
            json!({ "action": "status", "limit": 1.5 }),
            json!({ "action": "status", "urlFilter": 1 }),
            json!({ "action": "status", "includeAnonymous": "yes" }),
        ] {
            let (is_error, text) = call(&ctx, args.clone()).await;
            assert!(is_error, "{args}");
            assert!(
                text.starts_with("Input validation error: Invalid arguments for tool exec_trace: "),
                "{text}"
            );
        }
        let ok_args = json!({ "action": "status", "maxSeconds": 600, "limit": 2000, "urlFilter": "app", "includeAnonymous": true });
        assert!(!call(&ctx, ok_args).await.0);
    }

    // ── the report built from a coverage sample ─────────────────────────────

    fn coverage(script_id: &str, url: &str, functions: Value) -> ScriptCoverage {
        serde_json::from_value(json!({ "scriptId": script_id, "url": url, "functions": functions }))
            .unwrap()
    }

    fn function(name: &str, start: u64, end: u64, count: u64) -> Value {
        json!({
            "functionName": name,
            "isBlockCoverage": true,
            "ranges": [{ "startOffset": start, "endOffset": end, "count": count }]
        })
    }

    fn request(id: &str, url: &str, resource_type: &str) -> RequestPatch {
        RequestPatch {
            request_id: id.into(),
            url: Some(url.into()),
            method: Some("GET".into()),
            resource_type: Some(resource_type.into()),
            ..RequestPatch::default()
        }
    }

    const SOURCE: &str =
        "function sign(a) {\n  return fetch('/api?sig=' + a)\n}\nfunction idle() {}";

    fn sample() -> (TrafficStore, Vec<ScriptCoverage>) {
        let mut store = TrafficStore::default();
        // Before the window, the bundle itself, the signed request, and one after the stop.
        store.upsert(request("early", "http://h/early", "XHR"), 50);
        store.upsert(
            RequestPatch {
                response_body: Some(SOURCE.into()),
                ..request("bundle", "http://h/app.js", "Script")
            },
            120,
        );
        store.upsert(
            RequestPatch {
                status: Some(200),
                initiator_stack: Some(vec![CallFrame {
                    function_name: "sign".into(),
                    url: "http://h/app.js".into(),
                    line_number: 1,
                    column_number: 9,
                }]),
                ..request("api", "http://h/api?sig=1", "Fetch")
            },
            150,
        );
        store.upsert(request("late", "http://h/late", "XHR"), 900);
        let coverage = vec![
            coverage(
                "7",
                "http://h/app.js",
                json!([
                    function("", 0, 72, 1),
                    function("sign", 0, 52, 3),
                    function("idle", 53, 71, 0)
                ]),
            ),
            coverage(
                "8",
                "chrome-extension://x/y.js",
                json!([function("ext", 5, 9, 1)]),
            ),
            coverage("9", "", json!([function("evaled", 5, 9, 2)])),
            coverage(
                "10",
                "http://h/vendor.js",
                json!([function("vendor", 5, 9, 1)]),
            ),
        ];
        (store, coverage)
    }

    fn report(trace: &Trace, auto: bool) -> Value {
        let (store, coverage) = sample();
        let text = trace_report(
            &store,
            &ScriptRegistry::new(),
            1,
            trace,
            coverage,
            &|script_id| (script_id == "7").then(|| SOURCE.to_owned()),
            400,
            auto,
        )
        .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn 창_안의_함수와_요청을_묶어_보고한다() {
        let out = report(&running(100), false);
        let keys: Vec<&str> = out
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "windowMs",
                "totalFunctions",
                "omittedFunctions",
                "scripts",
                "requests",
                "limitations"
            ]
        );
        assert_eq!(out["windowMs"], 300);
        // The extension script and the URL-less one are hidden; idle() never ran.
        assert_eq!(out["totalFunctions"], 2);
        let urls: Vec<&str> = out["scripts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["url"].as_str().unwrap())
            .collect();
        assert_eq!(urls, ["http://h/app.js", "http://h/vendor.js"]);
        let sign = &out["scripts"][0]["functions"][0];
        assert_eq!(sign["name"], "sign");
        assert_eq!(sign["location"], "http://h/app.js:1:1");
        assert_eq!(sign["calls"], 3);
        // The request's initiator stack passes through sign(), and the bundle is
        // a captured script, so its position can go straight to resolve_source.
        assert_eq!(sign["onRequestStack"], json!(["api"]));
        assert_eq!(
            sign["resolveSource"],
            json!({ "requestId": "bundle", "line": 0, "column": 0 })
        );
        // Only requests inside the window, and never the scripts themselves.
        assert_eq!(
            out["requests"],
            json!([{ "requestId": "api", "method": "GET", "url": "http://h/api?sig=1", "status": 200 }])
        );
    }

    #[test]
    fn 자동_종료는_note를_windowms_다음에_넣는다() {
        let out = report(&running(100), true);
        let keys: Vec<&str> = out
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys.get(..2), Some(&["windowMs", "note"][..]));
        assert_eq!(out["note"], "auto-stopped at the time limit");
    }

    #[test]
    fn url_필터와_익명_스크립트_포함과_개수_제한을_따른다() {
        let filtered = report(
            &Trace {
                url_filter: Some("vendor".into()),
                ..running(100)
            },
            false,
        );
        assert_eq!(filtered["totalFunctions"], 1);
        assert_eq!(filtered["scripts"][0]["url"], "http://h/vendor.js");

        let anonymous = report(
            &Trace {
                include_anonymous: true,
                ..running(100)
            },
            false,
        );
        assert_eq!(anonymous["totalFunctions"], 3);

        let limited = report(
            &Trace {
                limit: 1,
                ..running(100)
            },
            false,
        );
        assert_eq!(limited["totalFunctions"], 2);
        assert_eq!(limited["omittedFunctions"], 1);
    }

    #[test]
    fn 스크립트_url은_레지스트리의_것을_먼저_쓴다() {
        let (store, _) = sample();
        let mut registry = ScriptRegistry::new();
        registry.record_script(1, None, "7", "http://h/renamed.js");
        let text = trace_report(
            &store,
            &registry,
            1,
            &running(100),
            vec![coverage("7", "", json!([function("sign", 5, 9, 1)]))],
            &|_| None,
            400,
            false,
        )
        .unwrap();
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["scripts"][0]["url"], "http://h/renamed.js");
        // No source and no captured body for that URL: nothing to resolve.
        assert_eq!(
            out["scripts"][0]["functions"][0]["resolveSource"],
            Value::Null
        );
    }
}
