//! Port of `src/main/mcp/tools/debugger.ts` — breakpoints, stepping and evaluating
//! on a paused call frame.
//!
//! The bookkeeping the TypeScript kept in `chrome-cdp.ts` module variables (the
//! breakpoint list and the pause state) lives in [`DebugState`]. Everything that
//! has to reach the JavaScript debugger goes through [`DebugEngine`], whose
//! methods mirror the CDP `Debugger.*` commands the TypeScript sent.

use std::sync::{Arc, Mutex};

use rever_tools_api::tool::{BoxFuture, ToolDef, ToolRegistry, unsupported};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::args::{int, text};
use crate::{lock, no_args, pretty, schema};

/// One breakpoint as `bp_list` prints it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakpointEntry {
    /// The short id the agent uses with `bp_remove`.
    pub id: String,
    /// The engine's own id.
    pub breakpoint_id: String,
    pub url_glob: String,
    pub line: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

/// The parameters of CDP's `Debugger.paused`, as far as the tools read them.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PausedState {
    #[serde(default)]
    pub call_frames: Vec<PausedFrame>,
    #[serde(default)]
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PausedFrame {
    #[serde(default)]
    pub call_frame_id: String,
    #[serde(default)]
    pub function_name: String,
    /// `{ scriptId, lineNumber, columnNumber }`, printed as the engine sent it.
    #[serde(default)]
    pub location: Value,
    #[serde(default)]
    pub scope_chain: Vec<Scope>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Scope {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub object: ScopeObject,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct ScopeObject {
    pub description: Option<String>,
}

/// The parameters of `Debugger.setBreakpointByUrl`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BreakpointRequest {
    pub url_regex: String,
    pub line_number: u64,
    pub column_number: Option<u64>,
    /// Never empty: an empty condition is not sent.
    pub condition: Option<String>,
}

/// The result of `Debugger.setBreakpointByUrl`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SetBreakpoint {
    pub breakpoint_id: String,
    /// Where the breakpoint resolved, printed as the engine sent it.
    pub locations: Vec<Value>,
}

/// The result of `Debugger.evaluateOnCallFrame`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameEval {
    /// `result.value`; `None` when the result has none (JavaScript `undefined`,
    /// or a value that cannot be returned by value). `Some(Null)` is `null`.
    pub value: Option<Value>,
    /// `result.description`.
    pub description: Option<String>,
    /// `exceptionDetails.text` when the expression threw.
    pub exception: Option<String>,
}

/// What the debugger tools ask of the page's JavaScript engine. Every method has
/// a default that reports the operation as unsupported, so an engine implements
/// what it can. An `Err` is shown to the agent as the tool's error.
pub trait DebugEngine: Send + Sync {
    /// Whether there is a page to debug. When there is none the tools answer
    /// "no active browser target" without calling anything else.
    fn has_target(&self) -> bool {
        true
    }

    fn set_breakpoint<'a>(
        &'a self,
        _request: &'a BreakpointRequest,
    ) -> BoxFuture<'a, Result<SetBreakpoint, String>> {
        Box::pin(async { Err(unsupported("setting a breakpoint")) })
    }

    fn remove_breakpoint<'a>(
        &'a self,
        _breakpoint_id: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err(unsupported("removing a breakpoint")) })
    }

    fn resume(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err(unsupported("resuming from a breakpoint")) })
    }

    fn step_over(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err(unsupported("stepping over")) })
    }

    fn step_into(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err(unsupported("stepping into")) })
    }

    fn step_out(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err(unsupported("stepping out")) })
    }

    /// Evaluate with the result returned by value and without a preview.
    fn evaluate_on_call_frame<'a>(
        &'a self,
        _call_frame_id: &'a str,
        _expression: &'a str,
    ) -> BoxFuture<'a, Result<FrameEval, String>> {
        Box::pin(async { Err(unsupported("evaluating on a call frame")) })
    }
}

/// The engine in place until the app sets one: nothing is supported.
pub struct NoDebugEngine;

impl DebugEngine for NoDebugEngine {}

/// The debugger state shared by every call: `ctx.state::<DebugState>()`.
#[derive(Default)]
pub struct DebugState {
    breakpoints: Mutex<Vec<BreakpointEntry>>,
    paused: Mutex<Option<PausedState>>,
    engine: Mutex<Option<Arc<dyn DebugEngine>>>,
}

impl DebugState {
    pub fn breakpoints(&self) -> Vec<BreakpointEntry> {
        lock(&self.breakpoints).clone()
    }

    pub fn add_breakpoint(&self, entry: BreakpointEntry) {
        lock(&self.breakpoints).push(entry);
    }

    pub fn remove_breakpoint(&self, id: &str) -> Option<BreakpointEntry> {
        let mut breakpoints = lock(&self.breakpoints);
        let index = breakpoints.iter().position(|entry| entry.id == id)?;
        Some(breakpoints.remove(index))
    }

    pub fn paused(&self) -> Option<PausedState> {
        lock(&self.paused).clone()
    }

    /// The engine reports `Debugger.paused` (`Some`) and `Debugger.resumed` or a
    /// detached page (`None`) here.
    pub fn set_paused(&self, paused: Option<PausedState>) {
        *lock(&self.paused) = paused;
    }

    pub fn set_engine(&self, engine: Arc<dyn DebugEngine>) {
        *lock(&self.engine) = Some(engine);
    }

    pub fn engine(&self) -> Arc<dyn DebugEngine> {
        lock(&self.engine)
            .clone()
            .unwrap_or_else(|| Arc::new(NoDebugEngine))
    }

    fn target(&self) -> Result<Arc<dyn DebugEngine>, String> {
        let engine = self.engine();
        if !engine.has_target() {
            return Err("no active browser target".to_owned());
        }
        Ok(engine)
    }
}

/// A URL glob as the regex source CDP matches script URLs with: `*` is any run,
/// `?` any one character, everything else literal.
pub fn glob_to_regex(glob: &str) -> String {
    let mut out = String::with_capacity(glob.len());
    for ch in glob.chars() {
        match ch {
            '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            _ => out.push(ch),
        }
    }
    out
}

tool!(
    BpAdd,
    ToolDef::new(
        "bp_add",
        "[NOT FUNCTIONAL IN THIS BUILD] Execution never pauses under Electron's webContents.debugger — breakpoints/stepping are a no-op (even a literal `debugger;` will not halt). Prefer crypto_trace / console_eval instrumentation instead. Set a breakpoint at a URL glob + line number. The page will pause when execution reaches the location.",
        schema(
            json!({
                "urlGlob": {
                    "type": "string",
                    "description": "URL glob pattern, e.g. \"*/main.*.js\""
                },
                "line": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 9007199254740991i64,
                    "description": "0-indexed line number"
                },
                "column": {
                    "description": "0-indexed column number",
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 9007199254740991i64
                },
                "condition": {
                    "description": "Conditional expression (JS); only pause when truthy",
                    "type": "string"
                }
            }),
            &["urlGlob", "line"]
        )
    ),
    |ctx, args| {
        let state = ctx.state::<DebugState>();
        let engine = state.target()?;
        let url_glob = text(args, "urlGlob").unwrap_or_default();
        let unsigned = |key| int(args, key).and_then(|number| u64::try_from(number).ok());
        let line = unsigned("line").unwrap_or_default();
        let column = unsigned("column");
        let condition = text(args, "condition");
        let request = BreakpointRequest {
            url_regex: glob_to_regex(url_glob),
            line_number: line,
            column_number: column,
            condition: condition.filter(|c| !c.is_empty()).map(str::to_owned),
        };
        let set = engine.set_breakpoint(&request).await?;

        let id = uuid::Uuid::new_v4().to_string();
        state.add_breakpoint(BreakpointEntry {
            id: id.clone(),
            breakpoint_id: set.breakpoint_id.clone(),
            url_glob: url_glob.to_owned(),
            line,
            column,
            // Kept as given, even the empty one that was not sent.
            condition: condition.map(str::to_owned),
        });
        Ok(pretty(&json!({
            "id": id,
            "breakpointId": set.breakpoint_id,
            "locations": set.locations
        })))
    }
);

tool!(
    BpList,
    ToolDef::new("bp_list", "List all active breakpoints.", no_args()),
    |ctx, _args| { Ok(pretty(&ctx.state::<DebugState>().breakpoints())) }
);

tool!(
    BpRemove,
    ToolDef::new(
        "bp_remove",
        "Remove a breakpoint by its short ID (from bp_list).",
        schema(
            json!({
                "id": {
                    "type": "string",
                    "description": "Short breakpoint ID from bp_list"
                }
            }),
            &["id"]
        )
    ),
    |ctx, args| {
        let state = ctx.state::<DebugState>();
        let engine = state.target()?;
        let id = text(args, "id").unwrap_or_default();
        // Forgotten before the engine is asked, so a failed removal still drops it.
        let Some(breakpoint) = state.remove_breakpoint(id) else {
            return Err(format!("unknown breakpoint id: {id}"));
        };
        engine.remove_breakpoint(&breakpoint.breakpoint_id).await?;
        Ok(format!("breakpoint {id} removed"))
    }
);

tool!(
    BpStatus,
    ToolDef::new(
        "bp_status",
        "Return the current debugger pause state. If paused, shows the top call frames.",
        no_args()
    ),
    |ctx, _args| {
        let Some(paused) = ctx.state::<DebugState>().paused() else {
            return Ok(pretty(&json!({ "paused": false })));
        };
        let frames: Vec<Value> = paused
            .call_frames
            .iter()
            .take(5)
            .map(|frame| {
                let scope_keys: Vec<String> = frame
                    .scope_chain
                    .iter()
                    .map(|scope| {
                        let description = scope.object.description.as_deref().unwrap_or("?");
                        format!("{}: {description}", scope.kind)
                    })
                    .collect();
                let function_name = match frame.function_name.as_str() {
                    "" => "(anonymous)",
                    name => name,
                };
                json!({
                    "functionName": function_name,
                    "location": frame.location,
                    "scopeKeys": scope_keys
                })
            })
            .collect();
        Ok(pretty(&json!({
            "paused": true,
            "reason": paused.reason,
            "frames": frames
        })))
    }
);

tool!(
    BpResume,
    ToolDef::new(
        "bp_resume",
        "[NOT FUNCTIONAL IN THIS BUILD] Execution never pauses under Electron's webContents.debugger, so there is never a pause to resume from. Prefer crypto_trace / console_eval instrumentation instead. Resume execution after a breakpoint pause.",
        no_args()
    ),
    |ctx, _args| {
        ctx.state::<DebugState>().target()?.resume().await?;
        Ok("resumed".to_owned())
    }
);

tool!(
    BpStepOver,
    ToolDef::new(
        "bp_step_over",
        "[NOT FUNCTIONAL IN THIS BUILD] Execution never pauses under Electron's webContents.debugger — stepping is a no-op (even a literal `debugger;` will not halt). Prefer crypto_trace / console_eval instrumentation instead. Step over the current line (must be paused at a breakpoint).",
        no_args()
    ),
    |ctx, _args| {
        ctx.state::<DebugState>().target()?.step_over().await?;
        Ok("stepped over".to_owned())
    }
);

tool!(
    BpStepInto,
    ToolDef::new(
        "bp_step_into",
        "[NOT FUNCTIONAL IN THIS BUILD] Execution never pauses under Electron's webContents.debugger — stepping is a no-op (even a literal `debugger;` will not halt). Prefer crypto_trace / console_eval instrumentation instead. Step into the next function call (must be paused at a breakpoint).",
        no_args()
    ),
    |ctx, _args| {
        ctx.state::<DebugState>().target()?.step_into().await?;
        Ok("stepped into".to_owned())
    }
);

tool!(
    BpStepOut,
    ToolDef::new(
        "bp_step_out",
        "[NOT FUNCTIONAL IN THIS BUILD] Execution never pauses under Electron's webContents.debugger — stepping is a no-op (even a literal `debugger;` will not halt). Prefer crypto_trace / console_eval instrumentation instead. Step out of the current function (must be paused at a breakpoint).",
        no_args()
    ),
    |ctx, _args| {
        ctx.state::<DebugState>().target()?.step_out().await?;
        Ok("stepped out".to_owned())
    }
);

tool!(
    BpEvalInFrame,
    ToolDef::new(
        "bp_eval_in_frame",
        "[NOT FUNCTIONAL IN THIS BUILD] Execution never pauses under Electron's webContents.debugger, so there is never a paused frame to evaluate in (even a literal `debugger;` will not halt). Prefer crypto_trace / console_eval instrumentation instead. Evaluate a JavaScript expression in a specific call frame while paused at a breakpoint.",
        schema(
            json!({
                "frameIndex": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 9007199254740991i64,
                    "description": "Index into the paused call frames (0 = top frame)"
                },
                "expression": {
                    "type": "string",
                    "description": "JavaScript expression to evaluate in that frame"
                }
            }),
            &["frameIndex", "expression"]
        )
    ),
    |ctx, args| {
        let state = ctx.state::<DebugState>();
        let engine = state.target()?;
        let Some(paused) = state.paused() else {
            return Err("debugger is not paused — set a breakpoint first".to_owned());
        };
        let frame_index = int(args, "frameIndex").unwrap_or_default();
        let frame = usize::try_from(frame_index)
            .ok()
            .and_then(|index| paused.call_frames.get(index));
        let Some(frame) = frame else {
            return Err(format!(
                "no frame at index {frame_index} ({} frames total)",
                paused.call_frames.len()
            ));
        };
        let expression = text(args, "expression").unwrap_or_default();
        let result = engine
            .evaluate_on_call_frame(&frame.call_frame_id, expression)
            .await?;
        if let Some(exception) = result.exception {
            return Err(exception);
        }
        Ok(match (result.value, result.description) {
            (Some(value), _) => pretty(&value),
            (None, Some(description)) => pretty(&description),
            // The TypeScript answered with no text at all here
            // (JSON.stringify(undefined)); say what the value was instead.
            (None, None) => "undefined".to_owned(),
        })
    }
);

pub fn register(registry: &mut ToolRegistry) {
    registry.register(BpAdd);
    registry.register(BpList);
    registry.register(BpRemove);
    registry.register(BpStatus);
    registry.register(BpResume);
    registry.register(BpStepOver);
    registry.register(BpStepInto);
    registry.register(BpStepOut);
    registry.register(BpEvalInFrame);
}

#[cfg(test)]
mod tests {
    use rever_tools_api::tool::{Tool, ToolContext};

    use super::*;
    use crate::test_support::{call, call_json};

    /// Records what the tools ask for and answers from its fields.
    #[derive(Default)]
    struct FakeEngine {
        calls: Mutex<Vec<String>>,
        detached: bool,
        /// Every operation fails with this message.
        fail: Option<&'static str>,
        eval: FrameEval,
    }

    impl FakeEngine {
        fn record(&self, call: String) -> Result<(), String> {
            lock(&self.calls).push(call);
            self.fail.map_or(Ok(()), |message| Err(message.to_owned()))
        }

        fn calls(&self) -> Vec<String> {
            lock(&self.calls).clone()
        }
    }

    impl DebugEngine for FakeEngine {
        fn has_target(&self) -> bool {
            !self.detached
        }

        fn set_breakpoint<'a>(
            &'a self,
            request: &'a BreakpointRequest,
        ) -> BoxFuture<'a, Result<SetBreakpoint, String>> {
            Box::pin(async move {
                self.record(format!("set:{request:?}"))?;
                Ok(SetBreakpoint {
                    breakpoint_id: "1:10:0:.*/main\\.js".to_owned(),
                    locations: vec![
                        json!({ "scriptId": "42", "lineNumber": 10, "columnNumber": 4 }),
                    ],
                })
            })
        }

        fn remove_breakpoint<'a>(
            &'a self,
            breakpoint_id: &'a str,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move { self.record(format!("remove:{breakpoint_id}")) })
        }

        fn resume(&self) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async { self.record("resume".to_owned()) })
        }

        fn step_over(&self) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async { self.record("step_over".to_owned()) })
        }

        fn step_into(&self) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async { self.record("step_into".to_owned()) })
        }

        fn step_out(&self) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async { self.record("step_out".to_owned()) })
        }

        fn evaluate_on_call_frame<'a>(
            &'a self,
            call_frame_id: &'a str,
            expression: &'a str,
        ) -> BoxFuture<'a, Result<FrameEval, String>> {
            Box::pin(async move {
                self.record(format!("eval:{call_frame_id}:{expression}"))?;
                Ok(self.eval.clone())
            })
        }
    }

    fn context(engine: FakeEngine) -> (Arc<FakeEngine>, ToolContext) {
        let engine = Arc::new(engine);
        let context = ToolContext::for_tests();
        context.state::<DebugState>().set_engine(engine.clone());
        (engine, context)
    }

    fn entry(id: &str) -> BreakpointEntry {
        BreakpointEntry {
            id: id.to_owned(),
            breakpoint_id: format!("engine-{id}"),
            url_glob: "*/app.js".to_owned(),
            line: 3,
            column: None,
            condition: None,
        }
    }

    /// A pause with `count` frames, as CDP's `Debugger.paused` describes it.
    fn paused(count: usize) -> PausedState {
        let frames: Vec<Value> = (0..count)
            .map(|index| {
                json!({
                    "callFrameId": format!("frame-{index}"),
                    "functionName": if index == 0 { "sign".to_owned() } else { String::new() },
                    "location": { "scriptId": "7", "lineNumber": index, "columnNumber": 2 },
                    "scopeChain": [
                        { "type": "local", "object": { "objectId": "o1", "description": "Object" } },
                        { "type": "global", "object": { "objectId": "o2" } }
                    ],
                    "this": { "type": "undefined" }
                })
            })
            .collect();
        serde_json::from_value(json!({
            "callFrames": frames,
            "reason": "other",
            "hitBreakpoints": ["1:10:0"]
        }))
        .expect("Debugger.paused parameters")
    }

    #[test]
    fn 글롭은_자바스크립트와_같은_정규식_소스로_바뀐다() {
        // Expected values are what globToRegex returns in bun.
        for (glob, regex) in [
            ("*/main.*.js", r".*/main\..*\.js"),
            ("a?b", "a.b"),
            (r".+^${}()|[]\", r"\.\+\^\$\{\}\(\)\|\[\]\\"),
            ("https://a.test/x?y=1&z=*", r"https://a\.test/x.y=1&z=.*"),
            ("", ""),
            ("한*글", "한.*글"),
            ("**", ".*.*"),
            ("a-b/c,d", "a-b/c,d"),
        ] {
            assert_eq!(glob_to_regex(glob), regex, "{glob}");
        }
    }

    #[tokio::test]
    async fn bp_add는_엔진에_정규식과_위치를_넘기고_목록에_기록한다() {
        let (engine, context) = context(FakeEngine::default());
        let args = json!({ "urlGlob": "*/main.js", "line": 10, "column": 4, "condition": "x > 1" });
        let text = call(&BpAdd, &context, args).await.unwrap();

        assert_eq!(
            engine.calls(),
            [
                r#"set:BreakpointRequest { url_regex: ".*/main\\.js", line_number: 10, column_number: Some(4), condition: Some("x > 1") }"#
            ]
        );
        let listed = context.state::<DebugState>().breakpoints();
        assert_eq!(listed.len(), 1);
        let id = &listed[0].id;
        // randomUUID: 36 characters, version 4.
        assert_eq!(id.len(), 36);
        assert_eq!(id.chars().nth(14), Some('4'));
        assert_eq!(
            listed[0],
            BreakpointEntry {
                id: id.clone(),
                breakpoint_id: "1:10:0:.*/main\\.js".to_owned(),
                url_glob: "*/main.js".to_owned(),
                line: 10,
                column: Some(4),
                condition: Some("x > 1".to_owned()),
            }
        );
        assert_eq!(
            text,
            format!(
                "{{\n  \"id\": \"{id}\",\n  \"breakpointId\": \"1:10:0:.*/main\\\\.js\",\n  \"locations\": [\n    {{\n      \"scriptId\": \"42\",\n      \"lineNumber\": 10,\n      \"columnNumber\": 4\n    }}\n  ]\n}}"
            )
        );
    }

    #[tokio::test]
    async fn bp_add는_열과_빈_조건을_엔진에_보내지_않지만_빈_조건은_기록에_남긴다() {
        let (engine, context) = context(FakeEngine::default());
        let args = json!({ "urlGlob": "a.js", "line": 0, "condition": "" });
        call(&BpAdd, &context, args).await.unwrap();
        assert_eq!(
            engine.calls(),
            [
                r#"set:BreakpointRequest { url_regex: "a\\.js", line_number: 0, column_number: None, condition: None }"#
            ]
        );
        let listed = context.state::<DebugState>().breakpoints();
        assert_eq!(listed[0].column, None);
        assert_eq!(listed[0].condition.as_deref(), Some(""));

        // Column 0 is a real column, not an absent one.
        let args = json!({ "urlGlob": "a.js", "line": 2.0, "column": 0 });
        call(&BpAdd, &context, args).await.unwrap();
        assert!(engine.calls()[1].contains("line_number: 2, column_number: Some(0)"));
    }

    #[tokio::test]
    async fn bp_add는_엔진이_실패하면_아무것도_기록하지_않는다() {
        let (_, context) = context(FakeEngine {
            fail: Some("Debugger agent is not enabled"),
            ..FakeEngine::default()
        });
        let args = json!({ "urlGlob": "a.js", "line": 1 });
        assert_eq!(
            call(&BpAdd, &context, args).await,
            Err("Debugger agent is not enabled".to_owned())
        );
        assert!(context.state::<DebugState>().breakpoints().is_empty());
    }

    #[tokio::test]
    async fn bp_add는_인자가_틀리면_엔진을_부르지_않는다() {
        let (engine, context) = context(FakeEngine::default());
        let error = call(&BpAdd, &context, json!({ "urlGlob": "a.js", "line": -1 }))
            .await
            .unwrap_err();
        assert!(error.starts_with(
            "MCP error -32602: Input validation error: Invalid arguments for tool bp_add: ["
        ));
        assert!(engine.calls().is_empty());
    }

    #[tokio::test]
    async fn bp_list는_없는_필드를_빼고_추가한_순서대로_보여준다() {
        let context = ToolContext::for_tests();
        assert_eq!(call(&BpList, &context, json!({})).await.unwrap(), "[]");

        let state = context.state::<DebugState>();
        state.add_breakpoint(entry("a"));
        state.add_breakpoint(BreakpointEntry {
            column: Some(0),
            condition: Some("n === 2".to_owned()),
            ..entry("b")
        });
        assert_eq!(
            call(&BpList, &context, Value::Null).await.unwrap(),
            "[\n  {\n    \"id\": \"a\",\n    \"breakpointId\": \"engine-a\",\n    \"urlGlob\": \"*/app.js\",\n    \"line\": 3\n  },\n  {\n    \"id\": \"b\",\n    \"breakpointId\": \"engine-b\",\n    \"urlGlob\": \"*/app.js\",\n    \"line\": 3,\n    \"column\": 0,\n    \"condition\": \"n === 2\"\n  }\n]"
        );
    }

    #[tokio::test]
    async fn bp_remove는_엔진의_id로_지우고_목록에서_뺀다() {
        let (engine, context) = context(FakeEngine::default());
        let state = context.state::<DebugState>();
        state.add_breakpoint(entry("a"));
        state.add_breakpoint(entry("b"));

        assert_eq!(
            call(&BpRemove, &context, json!({ "id": "a" })).await,
            Ok("breakpoint a removed".to_owned())
        );
        assert_eq!(engine.calls(), ["remove:engine-a"]);
        assert_eq!(state.breakpoints(), [entry("b")]);

        assert_eq!(
            call(&BpRemove, &context, json!({ "id": "a" })).await,
            Err("unknown breakpoint id: a".to_owned())
        );
        assert_eq!(engine.calls().len(), 1);
    }

    #[tokio::test]
    async fn bp_remove는_엔진이_실패해도_목록에서는_이미_뺀다() {
        let (_, context) = context(FakeEngine {
            fail: Some("No breakpoint with given id"),
            ..FakeEngine::default()
        });
        let state = context.state::<DebugState>();
        state.add_breakpoint(entry("a"));
        assert_eq!(
            call(&BpRemove, &context, json!({ "id": "a" })).await,
            Err("No breakpoint with given id".to_owned())
        );
        assert!(state.breakpoints().is_empty());
    }

    #[tokio::test]
    async fn 대상_페이지가_없으면_상태를_건드리기_전에_알린다() {
        let (engine, context) = context(FakeEngine {
            detached: true,
            ..FakeEngine::default()
        });
        let state = context.state::<DebugState>();
        state.add_breakpoint(entry("a"));
        let no_target = Err("no active browser target".to_owned());

        let add = json!({ "urlGlob": "a.js", "line": 1 });
        assert_eq!(call(&BpAdd, &context, add).await, no_target);
        // Checked before the id: the breakpoint stays, known or not.
        assert_eq!(
            call(&BpRemove, &context, json!({ "id": "a" })).await,
            no_target
        );
        assert_eq!(
            call(&BpRemove, &context, json!({ "id": "zz" })).await,
            no_target
        );
        assert_eq!(state.breakpoints(), [entry("a")]);
        assert_eq!(call(&BpResume, &context, json!({})).await, no_target);
        assert_eq!(call(&BpStepOver, &context, json!({})).await, no_target);
        assert_eq!(call(&BpStepInto, &context, json!({})).await, no_target);
        assert_eq!(call(&BpStepOut, &context, json!({})).await, no_target);
        // Checked before the pause state.
        let eval = json!({ "frameIndex": 0, "expression": "x" });
        assert_eq!(call(&BpEvalInFrame, &context, eval).await, no_target);
        assert!(engine.calls().is_empty());

        // Listing and status never need the page.
        assert!(call(&BpList, &context, json!({})).await.is_ok());
        assert!(call(&BpStatus, &context, json!({})).await.is_ok());
    }

    #[tokio::test]
    async fn bp_status는_멈추지_않았으면_paused_false만_돌려준다() {
        let context = ToolContext::for_tests();
        assert_eq!(
            call(&BpStatus, &context, json!({})).await.unwrap(),
            "{\n  \"paused\": false\n}"
        );
    }

    #[tokio::test]
    async fn bp_status는_위에서_다섯_프레임만_요약한다() {
        let context = ToolContext::for_tests();
        let state = context.state::<DebugState>();
        state.set_paused(Some(paused(7)));
        let status = call_json(&BpStatus, &context, json!({})).await;
        assert_eq!(status["paused"], json!(true));
        assert_eq!(status["reason"], json!("other"));
        let frames = status["frames"].as_array().unwrap();
        assert_eq!(frames.len(), 5);
        assert_eq!(
            frames[0],
            json!({
                "functionName": "sign",
                "location": { "scriptId": "7", "lineNumber": 0, "columnNumber": 2 },
                // A scope object without a description prints "?".
                "scopeKeys": ["local: Object", "global: ?"]
            })
        );
        // A frame without a name is anonymous.
        assert_eq!(frames[4]["functionName"], json!("(anonymous)"));
        assert_eq!(frames[4]["location"]["lineNumber"], json!(4));

        // Resumed: back to not paused.
        state.set_paused(None);
        assert_eq!(
            call_json(&BpStatus, &context, json!({})).await,
            json!({ "paused": false })
        );
    }

    #[tokio::test]
    async fn bp_status의_출력은_키_순서까지_타입스크립트와_같다() {
        let context = ToolContext::for_tests();
        context.state::<DebugState>().set_paused(Some(paused(1)));
        assert_eq!(
            call(&BpStatus, &context, json!({})).await.unwrap(),
            "{\n  \"paused\": true,\n  \"reason\": \"other\",\n  \"frames\": [\n    {\n      \"functionName\": \"sign\",\n      \"location\": {\n        \"scriptId\": \"7\",\n        \"lineNumber\": 0,\n        \"columnNumber\": 2\n      },\n      \"scopeKeys\": [\n        \"local: Object\",\n        \"global: ?\"\n      ]\n    }\n  ]\n}"
        );
    }

    #[tokio::test]
    async fn bp_status는_빈_설명은_그대로_두고_없는_설명만_물음표로_바꾼다() {
        // `??` only replaces a missing description: the TypeScript printed
        // "local: " for an empty one (bun).
        let context = ToolContext::for_tests();
        let paused: PausedState = serde_json::from_value(json!({
            "reason": "other",
            "callFrames": [{
                "callFrameId": "f0",
                "functionName": "",
                "location": { "scriptId": "7", "lineNumber": 0, "columnNumber": 2 },
                "scopeChain": [
                    { "type": "local", "object": { "description": "" } },
                    { "type": "global", "object": {} }
                ]
            }]
        }))
        .unwrap();
        context.state::<DebugState>().set_paused(Some(paused));
        assert_eq!(
            call(&BpStatus, &context, json!({})).await.unwrap(),
            "{\n  \"paused\": true,\n  \"reason\": \"other\",\n  \"frames\": [\n    {\n      \"functionName\": \"(anonymous)\",\n      \"location\": {\n        \"scriptId\": \"7\",\n        \"lineNumber\": 0,\n        \"columnNumber\": 2\n      },\n      \"scopeKeys\": [\n        \"local: \",\n        \"global: ?\"\n      ]\n    }\n  ]\n}"
        );
    }

    #[tokio::test]
    async fn 엔진이_실수로_돌려준_수도_자바스크립트처럼_출력한다() {
        // An engine may hand every number over as a double; the TypeScript printed
        // 1000 and 1e+21 for these (bun), never 1000.0 or 1e21.
        struct Doubles;
        impl DebugEngine for Doubles {
            fn set_breakpoint<'a>(
                &'a self,
                _request: &'a BreakpointRequest,
            ) -> BoxFuture<'a, Result<SetBreakpoint, String>> {
                Box::pin(async {
                    Ok(SetBreakpoint {
                        breakpoint_id: "bp1".to_owned(),
                        locations: vec![
                            json!({ "scriptId": "1", "lineNumber": 1000.0, "columnNumber": 2.0 }),
                        ],
                    })
                })
            }

            fn evaluate_on_call_frame<'a>(
                &'a self,
                _call_frame_id: &'a str,
                _expression: &'a str,
            ) -> BoxFuture<'a, Result<FrameEval, String>> {
                Box::pin(async {
                    Ok(FrameEval {
                        value: Some(json!(1e21)),
                        ..FrameEval::default()
                    })
                })
            }
        }
        let context = ToolContext::for_tests();
        let state = context.state::<DebugState>();
        state.set_engine(Arc::new(Doubles));
        let added = call(&BpAdd, &context, json!({ "urlGlob": "a.js", "line": 1e3 }))
            .await
            .unwrap();
        assert!(
            added.ends_with(
                "\"locations\": [\n    {\n      \"scriptId\": \"1\",\n      \"lineNumber\": 1000,\n      \"columnNumber\": 2\n    }\n  ]\n}"
            ),
            "{added}"
        );
        state.set_paused(Some(paused(1)));
        let args = json!({ "frameIndex": 0, "expression": "x" });
        assert_eq!(
            call(&BpEvalInFrame, &context, args).await,
            Ok("1e+21".to_owned())
        );
    }

    #[test]
    fn 멈춤_상태는_빠진_필드가_있는_cdp_이벤트도_읽는다() {
        let state: PausedState =
            serde_json::from_value(json!({ "callFrames": [{ "scopeChain": [{}] }] })).unwrap();
        assert_eq!(state.reason, "");
        assert_eq!(state.call_frames.len(), 1);
        assert_eq!(state.call_frames[0].function_name, "");
        assert_eq!(state.call_frames[0].location, Value::Null);
        assert_eq!(state.call_frames[0].scope_chain[0].object.description, None);
        // A null description counts as none, like `??` in the TypeScript.
        let scope: Scope =
            serde_json::from_value(json!({ "type": "block", "object": { "description": null } }))
                .unwrap();
        assert_eq!(scope.kind, "block");
        assert_eq!(scope.object.description, None);
    }

    #[tokio::test]
    async fn 재개와_스텝은_엔진을_부르고_정해진_문구를_돌려준다() {
        let (engine, context) = context(FakeEngine::default());
        assert_eq!(
            call(&BpResume, &context, json!({})).await,
            Ok("resumed".to_owned())
        );
        assert_eq!(
            call(&BpStepOver, &context, json!({})).await,
            Ok("stepped over".to_owned())
        );
        assert_eq!(
            call(&BpStepInto, &context, json!({})).await,
            Ok("stepped into".to_owned())
        );
        assert_eq!(
            call(&BpStepOut, &context, Value::Null).await,
            Ok("stepped out".to_owned())
        );
        assert_eq!(
            engine.calls(),
            ["resume", "step_over", "step_into", "step_out"]
        );
    }

    #[tokio::test]
    async fn 재개와_스텝은_엔진의_에러_문구를_그대로_돌려준다() {
        let (_, context) = context(FakeEngine {
            fail: Some("Can only perform operation while paused."),
            ..FakeEngine::default()
        });
        let tools: [&dyn Tool; 4] = [&BpResume, &BpStepOver, &BpStepInto, &BpStepOut];
        for tool in tools {
            assert_eq!(
                call(tool, &context, json!({})).await,
                Err("Can only perform operation while paused.".to_owned())
            );
        }
    }

    #[tokio::test]
    async fn bp_eval_in_frame은_멈춘_프레임의_id로_평가한_값을_출력한다() {
        let (engine, context) = context(FakeEngine {
            eval: FrameEval {
                value: Some(json!({ "key": "abc", "n": [1, 2] })),
                description: Some("Object".to_owned()),
                exception: None,
            },
            ..FakeEngine::default()
        });
        context.state::<DebugState>().set_paused(Some(paused(3)));
        let args = json!({ "frameIndex": 2, "expression": "({key, n})" });
        assert_eq!(
            call(&BpEvalInFrame, &context, args).await.unwrap(),
            "{\n  \"key\": \"abc\",\n  \"n\": [\n    1,\n    2\n  ]\n}"
        );
        assert_eq!(engine.calls(), ["eval:frame-2:({key, n})"]);
    }

    #[tokio::test]
    async fn bp_eval_in_frame은_값이_없으면_설명을_null_값은_null을_출력한다() {
        async fn eval(result: FrameEval) -> Result<String, String> {
            let (_, context) = context(FakeEngine {
                eval: result,
                ..FakeEngine::default()
            });
            context.state::<DebugState>().set_paused(Some(paused(1)));
            let args = json!({ "frameIndex": 0, "expression": "x" });
            call(&BpEvalInFrame, &context, args).await
        }

        let described = FrameEval {
            description: Some("function sign(a) { … }".to_owned()),
            ..FrameEval::default()
        };
        assert_eq!(
            eval(described).await,
            Ok("\"function sign(a) { … }\"".to_owned())
        );
        // null is a value: it wins over the description.
        let null = FrameEval {
            value: Some(Value::Null),
            description: Some("null".to_owned()),
            exception: None,
        };
        assert_eq!(eval(null).await, Ok("null".to_owned()));
        let string = FrameEval {
            value: Some(json!("a\"b")),
            ..FrameEval::default()
        };
        assert_eq!(eval(string).await, Ok("\"a\\\"b\"".to_owned()));
        assert_eq!(eval(FrameEval::default()).await, Ok("undefined".to_owned()));
        // An exception wins over whatever value came with it.
        let threw = FrameEval {
            value: Some(json!(1)),
            description: None,
            exception: Some("Uncaught ReferenceError: x is not defined".to_owned()),
        };
        assert_eq!(
            eval(threw).await,
            Err("Uncaught ReferenceError: x is not defined".to_owned())
        );
    }

    #[tokio::test]
    async fn bp_eval_in_frame은_멈추지_않았거나_프레임이_없으면_엔진을_부르지_않는다() {
        let (engine, context) = context(FakeEngine::default());
        let args = json!({ "frameIndex": 0, "expression": "x" });
        assert_eq!(
            call(&BpEvalInFrame, &context, args).await,
            Err("debugger is not paused — set a breakpoint first".to_owned())
        );

        context.state::<DebugState>().set_paused(Some(paused(2)));
        let args = json!({ "frameIndex": 2, "expression": "x" });
        assert_eq!(
            call(&BpEvalInFrame, &context, args).await,
            Err("no frame at index 2 (2 frames total)".to_owned())
        );
        let args = json!({ "frameIndex": 9007199254740991i64, "expression": "x" });
        assert_eq!(
            call(&BpEvalInFrame, &context, args).await,
            Err("no frame at index 9007199254740991 (2 frames total)".to_owned())
        );
        assert!(engine.calls().is_empty());
    }

    #[tokio::test]
    async fn 엔진을_꽂지_않으면_엔진이_필요한_동작은_지원하지_않는다고_알린다() {
        let context = ToolContext::for_tests();
        let state = context.state::<DebugState>();
        let not_yet = |what: &str| Err(unsupported(what));

        let add = json!({ "urlGlob": "a.js", "line": 1 });
        assert_eq!(
            call(&BpAdd, &context, add).await,
            Err("setting a breakpoint is not supported by this browser engine yet".to_owned())
        );
        assert!(state.breakpoints().is_empty());

        state.add_breakpoint(entry("a"));
        assert_eq!(
            call(&BpRemove, &context, json!({ "id": "a" })).await,
            not_yet("removing a breakpoint")
        );
        assert_eq!(
            call(&BpRemove, &context, json!({ "id": "a" })).await,
            Err("unknown breakpoint id: a".to_owned())
        );
        assert_eq!(
            call(&BpResume, &context, json!({})).await,
            not_yet("resuming from a breakpoint")
        );
        assert_eq!(
            call(&BpStepOver, &context, json!({})).await,
            not_yet("stepping over")
        );
        assert_eq!(
            call(&BpStepInto, &context, json!({})).await,
            not_yet("stepping into")
        );
        assert_eq!(
            call(&BpStepOut, &context, json!({})).await,
            not_yet("stepping out")
        );

        state.set_paused(Some(paused(1)));
        let eval = json!({ "frameIndex": 0, "expression": "x" });
        assert_eq!(
            call(&BpEvalInFrame, &context, eval).await,
            not_yet("evaluating on a call frame")
        );
    }

    #[test]
    fn 상태는_호출_사이에_유지되고_없는_id를_지우면_none이다() {
        let context = ToolContext::for_tests();
        context.state::<DebugState>().add_breakpoint(entry("a"));
        let state = context.state::<DebugState>();
        assert_eq!(state.remove_breakpoint("missing"), None);
        assert_eq!(state.remove_breakpoint("a"), Some(entry("a")));
        assert_eq!(state.remove_breakpoint("a"), None);
        assert_eq!(state.paused(), None);
    }
}
