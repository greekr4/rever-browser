//! Port of `src/main/workflow-executor.ts` — the deterministic workflow runner.
//!
//! Executes a fixed list of MCP tool calls in order, with no model in the loop.
//! Tools act on the page the agent's browser tools use. Variable substitution
//! ({{var}}) is resolved before steps arrive here, so this stays a plain
//! sequencer.
//!
//! What the TypeScript reached through module imports comes from the context
//! here: tools from the registry handle ([`crate::bridge`]), the page from
//! `ctx.browser`, and the agent of an AI step from [`AiAgentsSlot`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use regex::{Captures, Regex};
use rever_tools_api::tool::{BoxFuture, ToolContext};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::time::{Instant, sleep};

use crate::bridge::{ToolOutcome, call_mcp_tool};
use crate::js::{
    JS_SPACE, array_index, js_number, js_regex, js_string, js_trim, js_truthy, json_stringify,
    utf16_prefix,
};
use crate::lock;

fn empty_input() -> Value {
    json!({})
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStep {
    pub tool: String,
    #[serde(default = "empty_input")]
    pub input: Value,
    // Preconditions, both applied BEFORE the tool runs: wait until `waitFor`
    // matches an element, then pause a further `delay` ms. Pages that render
    // asynchronously need the selector wait; `delay` covers the cases where
    // there's nothing specific to wait on (animations, debounced handlers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_for: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_timeout: Option<f64>,
    // Store this step's result in the run's variable bag under this name, so
    // later steps can interpolate it as {{name}} (or {{name.path.to.field}}).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_as: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Waiting,
    Running,
    Done,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepProgress {
    pub index: usize,
    pub tool: String,
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<String>,
}

impl StepProgress {
    fn new(index: usize, step: &RunStep, status: StepStatus) -> Self {
        Self {
            index,
            tool: step.tool.clone(),
            status,
            output: None,
            error: None,
            waiting_for: None,
        }
    }

    fn failed(index: usize, step: &RunStep, error: String) -> Self {
        Self {
            error: Some(error),
            ..Self::new(index, step, StepStatus::Error)
        }
    }
}

type Run = Arc<AtomicBool>;

fn cancelled(run: &Run) -> bool {
    run.load(Ordering::SeqCst)
}

/// Only one workflow runs at a time; a second run supersedes the first.
#[derive(Default)]
pub struct WorkflowRuns {
    active: Mutex<Option<Run>>,
}

impl WorkflowRuns {
    pub fn cancel(&self) {
        if let Some(run) = lock(&self.active).as_ref() {
            run.store(true, Ordering::SeqCst);
        }
    }

    fn begin(&self) -> Run {
        self.cancel();
        let run = Run::default();
        *lock(&self.active) = Some(run.clone());
        run
    }

    fn end(&self, run: &Run) {
        let mut active = lock(&self.active);
        if active
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, run))
        {
            *active = None;
        }
    }
}

pub fn cancel_workflow(ctx: &ToolContext) {
    ctx.state::<WorkflowRuns>().cancel();
}

const DEFAULT_WAIT_TIMEOUT_MS: f64 = 10_000.0;
const POLL: Duration = Duration::from_millis(100);

/// A step's milliseconds as a time span; a negative or absurd number must not panic.
fn span(ms: f64) -> Duration {
    Duration::try_from_secs_f64(ms / 1000.0).unwrap_or(if ms > 0.0 {
        Duration::MAX
    } else {
        Duration::ZERO
    })
}

// Sleep in POLL slices so Stop takes effect during a long wait instead of
// after it.
async fn sleep_cancellable(ms: f64, run: &Run) {
    // `None` is a span too long to end: only Stop ends it.
    let until = Instant::now().checked_add(span(ms));
    loop {
        let left = match until {
            Some(until) => until.saturating_duration_since(Instant::now()),
            None => POLL,
        };
        if left.is_zero() || cancelled(run) {
            return;
        }
        sleep(POLL.min(left)).await;
    }
}

/// Apply a step's preconditions. Fails if `waitFor` never matches within the
/// timeout — the caller turns that into a normal step error, so a macro fails
/// loudly on a selector that never appears instead of acting on a half-built
/// page.
async fn apply_step_waits(
    ctx: &ToolContext,
    step: &RunStep,
    run: &Run,
    on_waiting: &mut (dyn FnMut(String) + Send),
) -> Result<(), String> {
    if let Some(selector) = step.wait_for.as_deref().filter(|s| !s.is_empty()) {
        let timeout = step.wait_timeout.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        on_waiting(format!("waiting for {selector}"));
        let deadline = Instant::now().checked_add(span(timeout));
        let expression = format!(
            "!!document.querySelector({})",
            json_stringify(&Value::from(selector), false)
        );
        loop {
            if cancelled(run) {
                return Ok(());
            }
            // An error is no page yet, or a page mid-navigation. Keep polling
            // until the deadline rather than failing on a transient error.
            let found = ctx.browser.evaluate(&expression).await;
            if found.is_ok_and(|found| js_truthy(&found)) {
                break;
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(format!(
                    "waitFor timed out after {}ms: no element matches {selector}",
                    js_number(timeout)
                ));
            }
            sleep(POLL).await;
        }
    }
    if let Some(delay) = step.delay.filter(|delay| *delay > 0.0) {
        on_waiting(format!("waiting {}ms", js_number(delay)));
        sleep_cancellable(delay, run).await;
    }
    Ok(())
}

// ── Runtime variables ───────────────────────────────────────────────────────
// User-supplied {{vars}} are substituted before steps arrive, but unknown
// placeholders are left intact — those are filled here from step results
// captured via `saveAs`, which is what makes an AI step's output usable
// downstream.

/// `None` is JavaScript's `undefined`: nothing by that name.
// shortcut: of the names a JavaScript value inherits, only an array's `length`
// is looked up; {{x.constructor}} and the like stay as written.
fn resolve_path<'a>(vars: &'a Map<String, Value>, path: &str) -> Option<Value> {
    let mut keys = path.split('.');
    let mut current: &'a Value = vars.get(keys.next()?)?;
    for key in keys {
        current = match current {
            Value::Object(map) => map.get(key)?,
            Value::Array(items) if key == "length" => return Some(json!(items.len())),
            Value::Array(items) => items.get(array_index(key)?)?,
            _ => return None,
        };
    }
    Some(current.clone())
}

// `\w` is ASCII-only in JavaScript, and its `\s` is not quite Rust's.
static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\{{\{{{JS_SPACE}*([A-Za-z0-9_.-]+){JS_SPACE}*\}}\}}"
    ))
    .expect("built-in pattern compiles")
});
static WHOLE_PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^\{{\{{{JS_SPACE}*([A-Za-z0-9_.-]+){JS_SPACE}*\}}\}}$"
    ))
    .expect("built-in pattern compiles")
});

fn stringify_var(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => json_stringify(other, false),
    }
}

pub fn substitute(value: &Value, vars: &Map<String, Value>) -> Value {
    match value {
        Value::String(text) => {
            let path = |found: &Captures| found.get(1).map_or("", |m| m.as_str()).to_owned();
            // A string that is exactly one placeholder resolves to the raw value, so
            // {"limit": "{{n}}"} yields a number rather than the string "5".
            if let Some(whole) = WHOLE_PLACEHOLDER.captures(text) {
                return resolve_path(vars, &path(&whole)).unwrap_or_else(|| value.clone());
            }
            let replaced = PLACEHOLDER.replace_all(text, |found: &Captures| {
                match resolve_path(vars, &path(found)) {
                    Some(resolved) => stringify_var(&resolved),
                    // Leave unknown placeholders visible instead of blanking them — a
                    // typo should surface in the tool error, not silently become "".
                    None => found.get(0).map_or("", |m| m.as_str()).to_owned(),
                }
            });
            Value::String(replaced.into_owned())
        }
        Value::Array(items) => items.iter().map(|item| substitute(item, vars)).collect(),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| (key.clone(), substitute(item, vars)))
                .collect(),
        ),
        other => other.clone(),
    }
}

// ── AI step ─────────────────────────────────────────────────────────────────
// A macro step can call the agent instead of an MCP tool. The session is
// spawned on first use and torn down when the run ends, so a macro never
// disturbs (or depends on) the chat panel's conversation.

pub const AI_TOOL: &str = "__ai_prompt";

/// The agent to spawn for an AI step, as the agent router takes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AgentDef {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
}

/// What an AI step needs of the app: the agent router's session calls and the
/// settings' key check. The app plugs its own in with
/// `ctx.state::<AiAgentsSlot>().set(…)`; until then an AI step fails with a
/// clear message and every other step works.
pub trait AiAgents: Send + Sync {
    /// Whether an API key is configured for a direct provider (`anthropic`,
    /// `openai`, `grok`, `glm`).
    fn has_api_key(&self, _provider: &str) -> bool {
        false
    }

    /// Starts a session and returns its id.
    fn spawn_session<'a>(
        &'a self,
        agent: &'a AgentDef,
        cwd: &'a str,
    ) -> BoxFuture<'a, Result<String, String>>;

    /// Runs one prompt to the end. `on_update` receives every session
    /// notification as the JSON the agent protocol carries.
    fn prompt_session<'a>(
        &'a self,
        session_id: &'a str,
        prompt: &'a str,
        on_update: &'a mut (dyn FnMut(&Value) + Send),
    ) -> BoxFuture<'a, Result<(), String>>;

    fn kill_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<(), String>>;
}

struct NoAgents;

impl AiAgents for NoAgents {
    fn spawn_session<'a>(
        &'a self,
        _agent: &'a AgentDef,
        _cwd: &'a str,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async {
            Err(format!(
                "{AI_TOOL} is not available: no agent is connected to workflows yet"
            ))
        })
    }

    fn prompt_session<'a>(
        &'a self,
        _session_id: &'a str,
        _prompt: &'a str,
        _on_update: &'a mut (dyn FnMut(&Value) + Send),
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("no agent session".to_owned()) })
    }

    fn kill_session<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// The agents AI steps run on.
pub struct AiAgentsSlot(Mutex<Arc<dyn AiAgents>>);

impl Default for AiAgentsSlot {
    fn default() -> Self {
        Self(Mutex::new(Arc::new(NoAgents)))
    }
}

impl AiAgentsSlot {
    pub fn set(&self, agents: Arc<dyn AiAgents>) {
        *lock(&self.0) = agents;
    }

    pub fn get(&self) -> Arc<dyn AiAgents> {
        lock(&self.0).clone()
    }
}

struct AiSession {
    id: String,
    agent_id: String,
}

fn pick_agent_id(requested: Option<&str>, agents: &dyn AiAgents) -> String {
    if let Some(requested) = requested.filter(|requested| !requested.is_empty()) {
        return requested.to_owned();
    }
    // Prefer a direct API provider when a key is configured — no child process
    // to spawn, so the first AI step doesn't pay several seconds of startup.
    let direct = ["anthropic", "openai", "grok", "glm"];
    let keyed = direct
        .into_iter()
        .find(|provider| agents.has_api_key(provider));
    keyed.unwrap_or("claude-code").to_owned()
}

fn agent_def_for(agent_id: &str) -> AgentDef {
    let (id, command) = match agent_id {
        "anthropic" | "openai" | "grok" | "glm" => (agent_id, ""),
        "codex" => ("codex", "codex-acp"),
        _ => ("claude-code", "claude-agent-acp"),
    };
    AgentDef {
        id: id.to_owned(),
        command: command.to_owned(),
        args: Vec::new(),
    }
}

static FENCED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"```(?i-u:json)?{JS_SPACE}*((?s:.*?))```"))
        .expect("built-in pattern compiles")
});

pub fn strip_fences(text: &str) -> &str {
    let fenced = FENCED.captures(text).and_then(|found| found.get(1));
    js_trim(fenced.map_or(text, |inner| inner.as_str()))
}

/// The step's input fields are: `prompt`; `json`, to force a JSON-only answer
/// and parse it before storing; `schema`, a shape hint appended to the prompt,
/// e.g. '{ "title": string }'; and `agent`, to override the agent ('anthropic'
/// | 'openai' | 'grok' | an ACP binary id).
///
/// Returns the text to show and the value to save.
async fn run_ai_step(
    agents: &dyn AiAgents,
    raw: &Value,
    session: &mut Option<AiSession>,
    cwd: &str,
) -> Result<(String, Value), String> {
    let prompt = js_trim(raw.get("prompt").and_then(Value::as_str).unwrap_or(""));
    if prompt.is_empty() {
        return Err(format!("{AI_TOOL} requires a \"prompt\""));
    }

    let agent_id = pick_agent_id(raw.get("agent").and_then(Value::as_str), agents);
    if session
        .as_ref()
        .is_none_or(|open| open.agent_id != agent_id)
    {
        if let Some(open) = session.take() {
            let _ = agents.kill_session(&open.id).await;
        }
        let id = agents.spawn_session(&agent_def_for(&agent_id), cwd).await?;
        // Kept as soon as it exists, so the end of the run closes it even when
        // the prompt below fails.
        *session = Some(AiSession { id, agent_id });
    }
    let Some(open) = session.as_ref() else {
        return Err("no agent session".to_owned());
    };

    let mut full = prompt.to_owned();
    if raw.get("json").is_some_and(js_truthy) {
        match raw.get("schema").filter(|schema| js_truthy(schema)) {
            Some(schema) => full.push_str(&format!(
                "\n\nRespond with ONLY valid JSON matching this shape, no markdown fences and no prose:\n{}",
                js_string(schema)
            )),
            None => full.push_str(
                "\n\nRespond with ONLY valid JSON. No markdown fences, no prose, no explanation.",
            ),
        }
    }

    let mut text = String::new();
    let mut on_update = |notification: &Value| {
        let update = &notification["update"];
        if update["sessionUpdate"] == "agent_message_chunk" && update["content"]["type"] == "text" {
            text.push_str(update["content"]["text"].as_str().unwrap_or(""));
        }
    };
    agents
        .prompt_session(&open.id, &full, &mut on_update)
        .await?;
    let text = js_trim(&text);
    if text.is_empty() {
        return Err("agent returned no text".to_owned());
    }

    if !raw.get("json").is_some_and(js_truthy) {
        return Ok((text.to_owned(), json!(text)));
    }
    let cleaned = strip_fences(text);
    match serde_json::from_str::<Value>(cleaned) {
        Ok(value) => Ok((cleaned.to_owned(), value)),
        Err(_) => Err(format!(
            "agent did not return valid JSON. Got: {}",
            utf16_prefix(cleaned, 200)
        )),
    }
}

#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    /// Where an AI step's agent starts. Defaults to the process's directory.
    pub cwd: Option<String>,
}

pub async fn run_workflow(
    ctx: &ToolContext,
    steps: &[RunStep],
    on_progress: &mut (dyn FnMut(StepProgress) + Send),
    opts: &RunOptions,
) -> Vec<StepProgress> {
    // Supersede any in-flight run.
    let runs = ctx.state::<WorkflowRuns>();
    let run = runs.begin();
    let agents = ctx.state::<AiAgentsSlot>().get();

    let mut results = Vec::new();
    let mut vars: Map<String, Value> = Map::new();
    let mut ai_session: Option<AiSession> = None;
    for (index, step) in steps.iter().enumerate() {
        if cancelled(&run) {
            break;
        }
        let mut on_waiting = |reason: String| {
            on_progress(StepProgress {
                waiting_for: Some(reason),
                ..StepProgress::new(index, step, StepStatus::Waiting)
            });
        };
        let waited = apply_step_waits(ctx, step, &run, &mut on_waiting).await;
        if waited.is_ok() && cancelled(&run) {
            break;
        }
        let outcome = match waited {
            Err(error) => Err(error),
            Ok(()) => {
                on_progress(StepProgress::new(index, step, StepStatus::Running));
                // Fill {{placeholders}} left over from the first pass with values
                // captured by earlier steps' saveAs.
                let input = substitute(&step.input, &vars);
                if step.tool == AI_TOOL {
                    let cwd = opts.cwd.clone().unwrap_or_else(|| {
                        std::env::current_dir()
                            .map(|dir| dir.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    });
                    run_ai_step(agents.as_ref(), &input, &mut ai_session, &cwd)
                        .await
                        .map(|(text, value)| (text, false, value))
                } else {
                    call_mcp_tool(ctx, &step.tool, input)
                        .await
                        .map(|ToolOutcome { text, is_error }| (text.clone(), is_error, json!(text)))
                }
            }
        };
        let result = match outcome {
            Ok((text, false, saved)) => {
                if let Some(name) = step.save_as.as_deref().filter(|name| !name.is_empty()) {
                    vars.insert(name.to_owned(), saved);
                }
                StepProgress {
                    output: Some(text),
                    ..StepProgress::new(index, step, StepStatus::Done)
                }
            }
            Ok((text, true, _)) | Err(text) => StepProgress::failed(index, step, text),
        };
        on_progress(result.clone());
        // Stop the sequence on the first failing step.
        let failed = result.status == StepStatus::Error;
        results.push(result);
        if failed {
            break;
        }
    }
    if let Some(session) = ai_session {
        let _ = agents.kill_session(&session.id).await;
    }
    runs.end(&run);
    results
}

// ── Pipeline (branch-aware) execution ───────────────────────────────────────
// The pipeline module compiles its node graph to this resolved tree (inputs
// already parsed + {{var}} substituted) and we walk it, branching on the last
// tool result. Conditions let a pipeline handle errors instead of hard-stopping.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CondOn {
    Output,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CondOp {
    Contains,
    Equals,
    Matches,
    Always,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipeCond {
    pub on: CondOn,
    pub op: CondOp,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ResolvedPipeNode {
    Tool {
        id: String,
        tool: String,
        #[serde(default = "empty_input")]
        input: Value,
    },
    If {
        id: String,
        cond: PipeCond,
        then: Vec<ResolvedPipeNode>,
        #[serde(rename = "else")]
        otherwise: Vec<ResolvedPipeNode>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PipeStatus {
    Running,
    Done,
    Error,
    Branch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Taken {
    Then,
    Else,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipeProgress {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub status: PipeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken: Option<Taken>,
}

fn eval_cond(cond: &PipeCond, last: Option<&ToolOutcome>) -> bool {
    let target = match (cond.on, last) {
        (CondOn::Error, Some(last)) if !last.is_error => "",
        (_, Some(last)) => last.text.as_str(),
        (_, None) => "",
    };
    match cond.op {
        CondOp::Always => true,
        CondOp::Contains => target.contains(&cond.value),
        CondOp::Equals => js_trim(target) == cond.value,
        // A pattern that does not compile is false, not a failure.
        CondOp::Matches => js_regex(&cond.value).is_some_and(|pattern| pattern.is_match(target)),
    }
}

pub async fn run_pipeline(
    ctx: &ToolContext,
    nodes: &[ResolvedPipeNode],
    on_progress: &mut (dyn FnMut(PipeProgress) + Send),
) {
    let runs = ctx.state::<WorkflowRuns>();
    let run = runs.begin();

    let mut last: Option<ToolOutcome> = None;
    // The lists being walked, innermost last: a branch is walked to its end
    // before the list that holds its `if` goes on.
    let mut walking = vec![nodes.iter()];
    while let Some(list) = walking.last_mut() {
        if cancelled(&run) {
            break;
        }
        let Some(node) = list.next() else {
            walking.pop();
            continue;
        };
        match node {
            ResolvedPipeNode::Tool { id, tool, input } => {
                let progress = |status: PipeStatus| PipeProgress {
                    node_id: id.clone(),
                    tool: Some(tool.clone()),
                    status,
                    output: None,
                    error: None,
                    taken: None,
                };
                on_progress(progress(PipeStatus::Running));
                let outcome = match call_mcp_tool(ctx, tool, input.clone()).await {
                    Ok(outcome) => outcome,
                    Err(text) => ToolOutcome {
                        text,
                        is_error: true,
                    },
                };
                on_progress(if outcome.is_error {
                    PipeProgress {
                        error: Some(outcome.text.clone()),
                        ..progress(PipeStatus::Error)
                    }
                } else {
                    PipeProgress {
                        output: Some(outcome.text.clone()),
                        ..progress(PipeStatus::Done)
                    }
                });
                last = Some(outcome);
            }
            ResolvedPipeNode::If {
                id,
                cond,
                then,
                otherwise,
            } => {
                let taken = if eval_cond(cond, last.as_ref()) {
                    Taken::Then
                } else {
                    Taken::Else
                };
                on_progress(PipeProgress {
                    node_id: id.clone(),
                    tool: None,
                    status: PipeStatus::Branch,
                    output: None,
                    error: None,
                    taken: Some(taken),
                });
                walking.push(if taken == Taken::Then {
                    then.iter()
                } else {
                    otherwise.iter()
                });
            }
        }
    }
    runs.end(&run);
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::test_support::{Echo, FakeBrowser, TestContext, context, context_on, registry_with};

    fn vars(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    // --- ported from workflow-executor.test.ts ---

    #[test]
    fn 빈_문자열이_아니라_원본_플레이스홀더를_남긴다_미해결_변수() {
        assert_eq!(
            substitute(&json!({ "q": "hello {{missing}}" }), &vars(json!({}))),
            json!({ "q": "hello {{missing}}" })
        );
    }

    #[test]
    fn 문자열_전체가_플레이스홀더면_값의_타입을_보존한다() {
        assert_eq!(
            substitute(&json!({ "limit": "{{n}}" }), &vars(json!({ "n": 5 }))),
            json!({ "limit": 5 })
        );
        assert_eq!(
            substitute(
                &json!({ "item": "{{obj}}" }),
                &vars(json!({ "obj": { "a": 1 } }))
            ),
            json!({ "item": { "a": 1 } })
        );
    }

    #[test]
    fn 문자열_중간에_있으면_문자열로_보간한다() {
        assert_eq!(
            substitute(
                &json!({ "q": "search: {{term}}!" }),
                &vars(json!({ "term": "shoes" }))
            ),
            json!({ "q": "search: shoes!" })
        );
    }

    #[test]
    fn 점_표기로_중첩_필드를_읽는다() {
        let vars = vars(json!({ "ai": { "title": "Acme", "tags": ["a", "b"] } }));
        assert_eq!(
            substitute(&json!({ "t": "{{ai.title}}" }), &vars),
            json!({ "t": "Acme" })
        );
        assert_eq!(
            substitute(&json!({ "t": "{{ai.tags.1}}" }), &vars),
            json!({ "t": "b" })
        );
    }

    #[test]
    fn 객체를_문자열에_보간하면_json으로_직렬화한다() {
        assert_eq!(
            substitute(
                &json!({ "body": "payload={{obj}}" }),
                &vars(json!({ "obj": { "a": 1 } }))
            ),
            json!({ "body": "payload={\"a\":1}" })
        );
    }

    #[test]
    fn 배열과_중첩_객체를_재귀적으로_처리한다() {
        let out = substitute(
            &json!({ "list": ["{{a}}", { "deep": "{{a}}" }] }),
            &vars(json!({ "a": "x" })),
        );
        assert_eq!(out, json!({ "list": ["x", { "deep": "x" }] }));
    }

    #[test]
    fn 문자열이_아닌_값은_그대로_둔다() {
        assert_eq!(
            substitute(&json!({ "n": 1, "b": true, "z": null }), &vars(json!({}))),
            json!({ "n": 1, "b": true, "z": null })
        );
    }

    #[test]
    fn json_펜스를_벗겨낸다() {
        assert_eq!(strip_fences("```json\n{\"a\":1}\n```"), "{\"a\":1}");
    }

    #[test]
    fn 언어_표기_없는_펜스도_벗겨낸다() {
        assert_eq!(strip_fences("```\n{\"a\":1}\n```"), "{\"a\":1}");
    }

    #[test]
    fn 펜스가_없으면_트림만_한다() {
        assert_eq!(strip_fences("  {\"a\":1}  "), "{\"a\":1}");
    }

    // --- not covered by the TypeScript tests ---

    #[test]
    fn 플레이스홀더의_경계_사례는_자바스크립트와_같다() {
        // Expected values are what substitute() returns under bun.
        let vars = vars(json!({ "n": null, "f": 5.0, "list": ["a", "b"], "s": "text", "a-b": 1 }));
        let run = |text: &str| substitute(&json!(text), &vars);
        assert_eq!(run("{{n}}"), Value::Null);
        assert_eq!(run("x{{n}}"), json!("xnull"));
        assert_eq!(run("x{{f}}"), json!("x5"));
        assert_eq!(run("{{ list.length }}"), json!(2));
        assert_eq!(run("{{list.01}}"), json!("{{list.01}}"));
        assert_eq!(run("{{list.5}}"), json!("{{list.5}}"));
        assert_eq!(run("{{s.length}}"), json!("{{s.length}}"));
        assert_eq!(run("{{n.deep}}"), json!("{{n.deep}}"));
        assert_eq!(run("{{a-b}}/{{list}}"), json!("1/[\"a\",\"b\"]"));
        // \w is ASCII-only, and a line break after the placeholder is not "whole".
        assert_eq!(run("{{변수}}"), json!("{{변수}}"));
        assert_eq!(run("{{f}}\n"), json!("5\n"));
        assert_eq!(run("{{\u{a0}f\u{feff}}}"), json!(5.0));
    }

    #[test]
    fn 펜스는_대소문자를_가리지_않고_앞뒤_글은_버린다() {
        assert_eq!(strip_fences("Here:\n```JSON\n[1]\n```\nDone"), "[1]");
        assert_eq!(strip_fences("```json{\"a\":1}```"), "{\"a\":1}");
        assert_eq!(strip_fences("``` not closed"), "``` not closed");
    }

    fn step(tool: &str, input: Value) -> RunStep {
        RunStep {
            tool: tool.to_owned(),
            input,
            wait_for: None,
            delay: None,
            wait_timeout: None,
            save_as: None,
        }
    }

    fn with_tools() -> TestContext {
        let ctx = context();
        registry_with(&ctx, |registry| registry.register(Echo("echo")));
        ctx
    }

    async fn run(ctx: &ToolContext, steps: &[RunStep]) -> (Vec<StepProgress>, Vec<StepProgress>) {
        let mut seen = Vec::new();
        let results = run_workflow(ctx, steps, &mut |p| seen.push(p), &RunOptions::default()).await;
        (results, seen)
    }

    #[tokio::test]
    async fn 스텝을_순서대로_실행하고_진행_상황을_알린다() {
        let ctx = with_tools();
        let steps = [
            step("echo", json!({ "a": 1 })),
            step("echo", json!({ "b": 2 })),
        ];
        let (results, seen) = run(&ctx, &steps).await;
        assert_eq!(
            serde_json::to_value(&results).unwrap(),
            json!([
                { "index": 0, "tool": "echo", "status": "done", "output": "echo:{\"a\":1}" },
                { "index": 1, "tool": "echo", "status": "done", "output": "echo:{\"b\":2}" },
            ])
        );
        let statuses: Vec<(usize, StepStatus)> = seen.iter().map(|p| (p.index, p.status)).collect();
        assert_eq!(
            statuses,
            [
                (0, StepStatus::Running),
                (0, StepStatus::Done),
                (1, StepStatus::Running),
                (1, StepStatus::Done)
            ]
        );
        // Key order of a serialized progress, as the TypeScript object has it.
        assert_eq!(
            serde_json::to_string(&results[0]).unwrap(),
            r#"{"index":0,"tool":"echo","status":"done","output":"echo:{\"a\":1}"}"#
        );
    }

    #[tokio::test]
    async fn 첫_실패에서_멈추고_에러_결과를_남긴다() {
        let ctx = with_tools();
        let steps = [
            step("echo", json!({})),
            step("echo", json!({ "fail": true })),
            step("echo", json!({ "never": 1 })),
        ];
        let (results, _) = run(&ctx, &steps).await;
        assert_eq!(results.len(), 2);
        assert_eq!(
            serde_json::to_value(&results[1]).unwrap(),
            json!({ "index": 1, "tool": "echo", "status": "error", "error": "echo:{\"fail\":true}" })
        );
    }

    #[tokio::test]
    async fn save_as로_저장한_결과를_다음_스텝이_쓴다() {
        let ctx = with_tools();
        let mut first = step("echo", json!({ "q": 1 }));
        first.save_as = Some("r".to_owned());
        // A failed step saves nothing, and an unknown name stays as written.
        let steps = [
            first,
            step(
                "echo",
                json!({ "whole": "{{r}}", "in": "<{{r}}>", "no": "{{x}}" }),
            ),
        ];
        let (results, _) = run(&ctx, &steps).await;
        let expected =
            json!({ "whole": "echo:{\"q\":1}", "in": "<echo:{\"q\":1}>", "no": "{{x}}" });
        assert_eq!(results[1].output, Some(format!("echo:{expected}")));
    }

    #[tokio::test]
    async fn 모르는_도구와_연결_안_된_레지스트리는_스텝_에러가_된다() {
        let ctx = with_tools();
        let (results, _) = run(&ctx, &[step("nope", json!({}))]).await;
        assert_eq!(
            results[0].error.as_deref(),
            Some("MCP error -32602: Tool nope not found")
        );

        let bare = context();
        let (results, seen) = run(&bare, &[step("echo", json!({})), step("echo", json!({}))]).await;
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].error.as_deref(),
            Some("the tool registry is not connected to workflows yet")
        );
        assert_eq!(seen.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for는_요소가_나타날_때까지_기다린다() {
        // Not there, then an error mid-navigation, then there.
        let browser = FakeBrowser::new(Ok(json!(true)))
            .then(Ok(json!(false)))
            .then(Err("navigating".to_owned()));
        let ctx = context_on(browser.clone());
        registry_with(&ctx, |registry| registry.register(Echo("echo")));
        let mut waiting = step("echo", json!({}));
        waiting.wait_for = Some("#go \"x\"".to_owned());
        let started = Instant::now();
        let (results, seen) = run(&ctx, &[waiting]).await;
        assert_eq!(results[0].status, StepStatus::Done);
        assert_eq!(started.elapsed(), Duration::from_millis(200));
        assert_eq!(seen[0].status, StepStatus::Waiting);
        assert_eq!(
            seen[0].waiting_for.as_deref(),
            Some("waiting for #go \"x\"")
        );
        assert_eq!(
            serde_json::to_string(&seen[0]).unwrap(),
            r##"{"index":0,"tool":"echo","status":"waiting","waitingFor":"waiting for #go \"x\""}"##
        );
        let evaluated = lock(&browser.evaluated);
        assert_eq!(evaluated.len(), 3);
        assert_eq!(evaluated[0], r##"!!document.querySelector("#go \"x\"")"##);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for가_시간_안에_안_나타나면_스텝이_실패한다() {
        let ctx = context_on(FakeBrowser::new(Ok(json!(false))));
        registry_with(&ctx, |registry| registry.register(Echo("echo")));
        let mut waiting = step("echo", json!({}));
        waiting.wait_for = Some(".late".to_owned());
        let started = Instant::now();
        let (results, _) = run(&ctx, &[waiting.clone(), step("echo", json!({}))]).await;
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].error.as_deref(),
            Some("waitFor timed out after 10000ms: no element matches .late")
        );
        assert_eq!(started.elapsed(), Duration::from_millis(10_000));

        // The step's own timeout, printed the way JavaScript prints the number.
        waiting.wait_timeout = Some(250.5);
        let (results, _) = run(&ctx, &[waiting.clone()]).await;
        assert_eq!(
            results[0].error.as_deref(),
            Some("waitFor timed out after 250.5ms: no element matches .late")
        );
        // A timeout of zero or less still looks once.
        waiting.wait_timeout = Some(-5.0);
        let (results, _) = run(&ctx, &[waiting]).await;
        assert_eq!(
            results[0].error.as_deref(),
            Some("waitFor timed out after -5ms: no element matches .late")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn delay만큼_쉬고_나서_실행한다() {
        let ctx = with_tools();
        let mut slow = step("echo", json!({}));
        slow.delay = Some(350.0);
        let mut none = step("echo", json!({}));
        none.delay = Some(0.0);
        let started = Instant::now();
        let (results, seen) = run(&ctx, &[slow, none]).await;
        assert_eq!(results.len(), 2);
        assert_eq!(started.elapsed(), Duration::from_millis(350));
        assert_eq!(seen[0].waiting_for.as_deref(), Some("waiting 350ms"));
        // A delay of zero is no wait at all.
        assert_eq!(
            seen.iter()
                .filter(|p| p.status == StepStatus::Waiting)
                .count(),
            1
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 취소하면_기다리는_중에도_멈춘다() {
        let ctx = with_tools();
        let mut slow = step("echo", json!({}));
        slow.delay = Some(60_000.0);
        let steps = [slow, step("echo", json!({}))];
        let started = Instant::now();
        let stop = async {
            sleep(Duration::from_millis(250)).await;
            cancel_workflow(&ctx);
        };
        let ((results, seen), ()) = tokio::join!(run(&ctx, &steps), stop);
        assert!(results.is_empty());
        assert_eq!(seen.len(), 1);
        assert!(started.elapsed() < Duration::from_millis(500));
        // Nothing is left to cancel once the run has ended.
        assert!(lock(&ctx.state::<WorkflowRuns>().active).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn 새_실행은_진행_중인_실행을_밀어낸다() {
        let ctx = with_tools();
        let mut slow = step("echo", json!({ "first": true }));
        slow.delay = Some(1_000.0);
        let first = [slow, step("echo", json!({}))];
        let second = [step("echo", json!({ "second": true }))];
        let later = async {
            sleep(Duration::from_millis(100)).await;
            run(&ctx, &second).await
        };
        let ((first_results, _), (second_results, _)) = tokio::join!(run(&ctx, &first), later);
        assert!(first_results.is_empty());
        assert_eq!(second_results.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn 터무니없는_대기_시간에도_패닉하지_않는다() {
        let ctx = context_on(FakeBrowser::new(Ok(json!(false))));
        registry_with(&ctx, |registry| registry.register(Echo("echo")));
        let mut forever = step("echo", json!({}));
        forever.delay = Some(f64::MAX);
        forever.wait_for = Some(".never".to_owned());
        forever.wait_timeout = Some(1e300);
        let stop = async {
            sleep(Duration::from_millis(500)).await;
            cancel_workflow(&ctx);
        };
        let steps = [forever];
        let ((results, _), ()) = tokio::join!(run(&ctx, &steps), stop);
        assert!(results.is_empty());
        assert_eq!(span(f64::NAN), Duration::ZERO);
        assert_eq!(span(-1.0), Duration::ZERO);
    }

    #[test]
    fn 스텝은_camel_case_json에서_읽고_입력이_없으면_빈_객체다() {
        let step: RunStep = serde_json::from_value(json!({
            "tool": "t", "waitFor": "#a", "delay": 5, "waitTimeout": 100, "saveAs": "r"
        }))
        .unwrap();
        assert_eq!(step.input, json!({}));
        assert_eq!(step.wait_for.as_deref(), Some("#a"));
        assert_eq!(step.delay, Some(5.0));
        assert_eq!(step.wait_timeout, Some(100.0));
        assert_eq!(step.save_as.as_deref(), Some("r"));
    }

    // --- AI steps ---

    #[derive(Default)]
    struct FakeAgents {
        keys: Vec<&'static str>,
        replies: Mutex<VecDeque<Result<Vec<Value>, String>>>,
        log: Mutex<Vec<String>>,
    }

    fn chunk(text: &str) -> Value {
        json!({ "update": { "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } } })
    }

    impl FakeAgents {
        fn replying(replies: Vec<Result<Vec<Value>, String>>) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies.into()),
                ..Self::default()
            })
        }

        fn log(&self) -> Vec<String> {
            lock(&self.log).clone()
        }
    }

    impl AiAgents for FakeAgents {
        fn has_api_key(&self, provider: &str) -> bool {
            self.keys.contains(&provider)
        }

        fn spawn_session<'a>(
            &'a self,
            agent: &'a AgentDef,
            cwd: &'a str,
        ) -> BoxFuture<'a, Result<String, String>> {
            Box::pin(async move {
                let mut log = lock(&self.log);
                let id = format!("s{}", log.iter().filter(|l| l.starts_with("spawn")).count());
                log.push(format!(
                    "spawn {id} {} [{}] in {cwd}",
                    agent.id, agent.command
                ));
                Ok(id)
            })
        }

        fn prompt_session<'a>(
            &'a self,
            session_id: &'a str,
            prompt: &'a str,
            on_update: &'a mut (dyn FnMut(&Value) + Send),
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                lock(&self.log).push(format!("prompt {session_id} {prompt}"));
                let reply = lock(&self.replies).pop_front().unwrap_or(Ok(Vec::new()));
                for notification in reply? {
                    on_update(&notification);
                }
                Ok(())
            })
        }

        fn kill_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                lock(&self.log).push(format!("kill {session_id}"));
                Err("already gone".to_owned())
            })
        }
    }

    async fn run_ai(agents: Arc<FakeAgents>, steps: &[RunStep]) -> Vec<StepProgress> {
        let ctx = with_tools();
        ctx.state::<AiAgentsSlot>().set(agents);
        let opts = RunOptions {
            cwd: Some("/work".to_owned()),
        };
        run_workflow(&ctx, steps, &mut |_| {}, &opts).await
    }

    #[tokio::test]
    async fn ai_스텝은_에이전트의_답을_모아_결과와_변수로_쓴다() {
        let other = json!({ "update": { "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "hm" } } });
        let image = json!({ "update": { "sessionUpdate": "agent_message_chunk", "content": { "type": "image" } } });
        let agents = FakeAgents::replying(vec![Ok(vec![
            chunk("  Hello"),
            other,
            image,
            chunk(" there \n"),
        ])]);
        let mut ask = step(AI_TOOL, json!({ "prompt": "  say hi  " }));
        ask.save_as = Some("ai".to_owned());
        let steps = [ask, step("echo", json!({ "got": "{{ai}}" }))];
        let results = run_ai(agents.clone(), &steps).await;
        assert_eq!(results[0].output.as_deref(), Some("Hello there"));
        assert_eq!(
            results[1].output.as_deref(),
            Some(r#"echo:{"got":"Hello there"}"#)
        );
        // No key configured: the Claude Code agent, closed when the run ends
        // (a failing close is ignored).
        assert_eq!(
            agents.log(),
            [
                "spawn s0 claude-code [claude-agent-acp] in /work",
                "prompt s0 say hi",
                "kill s0"
            ]
        );
    }

    #[tokio::test]
    async fn ai_스텝의_json_답은_펜스를_벗겨_값으로_저장한다() {
        let agents = FakeAgents::replying(vec![
            Ok(vec![chunk("```json\n{\"title\":\"Acme\",\"n\":2}\n```")]),
            Ok(vec![chunk("[1]")]),
        ]);
        let mut first = step(
            AI_TOOL,
            json!({ "prompt": "p", "json": true, "schema": "{ \"title\": string }" }),
        );
        first.save_as = Some("ai".to_owned());
        let second = step(AI_TOOL, json!({ "prompt": "q", "json": 1 }));
        let last = step("echo", json!({ "t": "{{ai.title}}", "n": "{{ai.n}}" }));
        let results = run_ai(agents.clone(), &[first, second, last]).await;
        assert_eq!(
            results[0].output.as_deref(),
            Some("{\"title\":\"Acme\",\"n\":2}")
        );
        assert_eq!(
            results[2].output.as_deref(),
            Some(r#"echo:{"t":"Acme","n":2}"#)
        );
        // One session serves both steps.
        assert_eq!(
            agents.log(),
            [
                "spawn s0 claude-code [claude-agent-acp] in /work",
                "prompt s0 p\n\nRespond with ONLY valid JSON matching this shape, no markdown fences and no prose:\n{ \"title\": string }",
                "prompt s0 q\n\nRespond with ONLY valid JSON. No markdown fences, no prose, no explanation.",
                "kill s0"
            ]
        );
    }

    #[tokio::test]
    async fn ai_스텝의_실패는_스텝_에러가_되고_세션은_닫힌다() {
        let not_json = "가".repeat(250);
        let cases: Vec<(Value, Result<Vec<Value>, String>, String)> = vec![
            (
                json!({ "prompt": "  " }),
                Ok(vec![]),
                "__ai_prompt requires a \"prompt\"".to_owned(),
            ),
            (
                json!({ "prompt": 5 }),
                Ok(vec![]),
                "__ai_prompt requires a \"prompt\"".to_owned(),
            ),
            (
                json!({ "prompt": "p" }),
                Ok(vec![chunk(" \n")]),
                "agent returned no text".to_owned(),
            ),
            (
                json!({ "prompt": "p" }),
                Err("agent crashed".to_owned()),
                "agent crashed".to_owned(),
            ),
            (
                json!({ "prompt": "p", "json": true }),
                Ok(vec![chunk(&not_json)]),
                // The echo of a bad answer is cut at 200 UTF-16 units.
                format!("agent did not return valid JSON. Got: {}", "가".repeat(200)),
            ),
        ];
        for (input, reply, expected) in cases {
            let agents = FakeAgents::replying(vec![reply]);
            let results = run_ai(
                agents.clone(),
                &[step(AI_TOOL, input.clone()), step("echo", json!({}))],
            )
            .await;
            assert_eq!(results.len(), 1, "{input}");
            assert_eq!(results[0].error, Some(expected), "{input}");
            let log = agents.log();
            let spawned = log.iter().any(|line| line.starts_with("spawn"));
            assert_eq!(
                log.last().is_some_and(|line| line == "kill s0"),
                spawned,
                "{input}"
            );
        }
    }

    #[tokio::test]
    async fn ai_스텝의_에이전트는_요청과_설정된_키로_고른다() {
        let keyed = Arc::new(FakeAgents {
            keys: vec!["grok", "openai"],
            ..FakeAgents::default()
        });
        assert_eq!(pick_agent_id(None, keyed.as_ref()), "openai");
        assert_eq!(pick_agent_id(Some(""), keyed.as_ref()), "openai");
        assert_eq!(pick_agent_id(Some("codex"), keyed.as_ref()), "codex");
        assert_eq!(pick_agent_id(None, &NoAgents), "claude-code");
        let def = |id: &str| {
            let def = agent_def_for(id);
            (def.id, def.command)
        };
        assert_eq!(def("glm"), ("glm".to_owned(), String::new()));
        assert_eq!(def("codex"), ("codex".to_owned(), "codex-acp".to_owned()));
        assert_eq!(
            def("anything"),
            ("claude-code".to_owned(), "claude-agent-acp".to_owned())
        );

        // Changing the agent between steps closes the old session first.
        let agents = FakeAgents::replying(vec![Ok(vec![chunk("a")]), Ok(vec![chunk("b")])]);
        let steps = [
            step(AI_TOOL, json!({ "prompt": "one", "agent": "codex" })),
            step(AI_TOOL, json!({ "prompt": "two", "agent": "anthropic" })),
        ];
        run_ai(agents.clone(), &steps).await;
        assert_eq!(
            agents.log(),
            [
                "spawn s0 codex [codex-acp] in /work",
                "prompt s0 one",
                "kill s0",
                "spawn s1 anthropic [] in /work",
                "prompt s1 two",
                "kill s1"
            ]
        );
    }

    #[tokio::test]
    async fn 에이전트가_연결되지_않았으면_ai_스텝만_실패한다() {
        let ctx = with_tools();
        let steps = [
            step("echo", json!({})),
            step(AI_TOOL, json!({ "prompt": "p" })),
        ];
        let (results, _) = run(&ctx, &steps).await;
        assert_eq!(results[0].status, StepStatus::Done);
        assert_eq!(
            results[1].error.as_deref(),
            Some("__ai_prompt is not available: no agent is connected to workflows yet")
        );
    }

    // --- pipelines ---

    fn outcome(text: &str, is_error: bool) -> ToolOutcome {
        ToolOutcome {
            text: text.to_owned(),
            is_error,
        }
    }

    fn cond(on: CondOn, op: CondOp, value: &str) -> PipeCond {
        PipeCond {
            on,
            op,
            value: value.to_owned(),
        }
    }

    #[test]
    fn 조건은_마지막_결과의_출력이나_에러를_본다() {
        let good = outcome(" all fine \n", false);
        let bad = outcome("HTTP 500", true);
        let check =
            |on, op, value: &str, last: Option<&ToolOutcome>| eval_cond(&cond(on, op, value), last);
        assert!(check(CondOn::Output, CondOp::Contains, "fine", Some(&good)));
        assert!(!check(
            CondOn::Output,
            CondOp::Contains,
            "Fine",
            Some(&good)
        ));
        assert!(check(
            CondOn::Output,
            CondOp::Equals,
            "all fine",
            Some(&good)
        ));
        assert!(!check(CondOn::Output, CondOp::Equals, "all", Some(&good)));
        assert!(check(
            CondOn::Output,
            CondOp::Matches,
            r"^\s*all\s+f",
            Some(&good)
        ));
        // An invalid pattern is false, not a failure.
        assert!(!check(CondOn::Output, CondOp::Matches, "(", Some(&good)));
        // `on: error` reads the text only when the last call failed...
        assert!(check(CondOn::Error, CondOp::Contains, "500", Some(&bad)));
        assert!(!check(CondOn::Error, CondOp::Contains, "fine", Some(&good)));
        assert!(check(CondOn::Error, CondOp::Equals, "", Some(&good)));
        // ...while `on: output` reads it either way.
        assert!(check(CondOn::Output, CondOp::Contains, "500", Some(&bad)));
        // Before any tool ran there is only the empty string.
        assert!(check(CondOn::Output, CondOp::Contains, "", None));
        assert!(!check(CondOn::Error, CondOp::Contains, "x", None));
        assert!(check(CondOn::Error, CondOp::Always, "x", None));
    }

    #[test]
    fn matches_조건은_자바스크립트_정규식으로_읽는다() {
        // Expected values are what evalCond answers under bun.
        let check = |value: &str, text: &str| {
            eval_cond(
                &cond(CondOn::Output, CondOp::Matches, value),
                Some(&outcome(text, false)),
            )
        };
        // \w and \b know ASCII only, so Hangul is not a word character.
        assert!(!check(r"^\w+$", "성공"));
        assert!(check(r"\bok\b", "결과ok입니다"));
        assert!(!check(r"^\d+$", "٣٤"));
        // `.` does not cross a carriage return.
        assert!(!check("a.b", "a\rb"));
        // A brace that is no quantifier is a literal, not a broken pattern.
        assert!(check(r#"^\{"ok":\s*true}$"#, r#"{"ok": true}"#));
        assert!(check("items{", "items{}"));
        // Not JavaScript syntax: false, as when `new RegExp` throws.
        assert!(!check("(?i)ok", "OK"));
        // An empty pattern matches anything, also before any tool ran.
        assert!(check("", "x"));
        assert!(eval_cond(&cond(CondOn::Error, CondOp::Matches, "^$"), None));
    }

    fn pipeline(value: Value) -> Vec<ResolvedPipeNode> {
        serde_json::from_value(value).unwrap()
    }

    async fn run_pipe(ctx: &ToolContext, nodes: &[ResolvedPipeNode]) -> Vec<Value> {
        let mut seen = Vec::new();
        run_pipeline(ctx, nodes, &mut |p| {
            seen.push(serde_json::to_value(p).unwrap())
        })
        .await;
        seen
    }

    #[tokio::test]
    async fn 파이프라인은_마지막_결과에_따라_가지를_고른다() {
        let ctx = with_tools();
        let nodes = pipeline(json!([
            { "id": "a", "type": "tool", "tool": "echo", "input": { "fail": true } },
            { "id": "b", "type": "if", "cond": { "on": "error", "op": "contains", "value": "fail" },
              "then": [
                { "id": "c", "type": "tool", "tool": "echo", "input": { "recover": 1 } },
                { "id": "d", "type": "if", "cond": { "on": "output", "op": "equals", "value": "nope" },
                  "then": [{ "id": "x", "type": "tool", "tool": "echo" }],
                  "else": [{ "id": "e", "type": "tool", "tool": "missing", "input": {} }] }
              ],
              "else": [{ "id": "y", "type": "tool", "tool": "echo", "input": {} }] },
            { "id": "f", "type": "tool", "tool": "echo" }
        ]));
        let seen = run_pipe(&ctx, &nodes).await;
        assert_eq!(
            seen,
            [
                json!({ "nodeId": "a", "tool": "echo", "status": "running" }),
                json!({ "nodeId": "a", "tool": "echo", "status": "error", "error": "echo:{\"fail\":true}" }),
                json!({ "nodeId": "b", "status": "branch", "taken": "then" }),
                json!({ "nodeId": "c", "tool": "echo", "status": "running" }),
                json!({ "nodeId": "c", "tool": "echo", "status": "done", "output": "echo:{\"recover\":1}" }),
                json!({ "nodeId": "d", "status": "branch", "taken": "else" }),
                json!({ "nodeId": "e", "tool": "missing", "status": "running" }),
                json!({ "nodeId": "e", "tool": "missing", "status": "error", "error": "MCP error -32602: Tool missing not found" }),
                // A failing tool does not stop a pipeline; the list goes on.
                json!({ "nodeId": "f", "tool": "echo", "status": "running" }),
                json!({ "nodeId": "f", "tool": "echo", "status": "done", "output": "echo:{}" }),
            ]
        );
        assert!(lock(&ctx.state::<WorkflowRuns>().active).is_none());
    }

    #[tokio::test]
    async fn 파이프라인의_호출_실패도_에러_결과로_가지에_쓰인다() {
        // No registry connected: the call itself fails.
        let ctx = context();
        let nodes = pipeline(json!([
            { "id": "a", "type": "tool", "tool": "echo", "input": {} },
            { "id": "b", "type": "if", "cond": { "on": "error", "op": "matches", "value": "not connected" },
              "then": [], "else": [] }
        ]));
        let seen = run_pipe(&ctx, &nodes).await;
        assert_eq!(seen[1]["status"], "error");
        assert_eq!(
            seen[1]["error"],
            "the tool registry is not connected to workflows yet"
        );
        assert_eq!(
            seen[2],
            json!({ "nodeId": "b", "status": "branch", "taken": "then" })
        );
    }

    #[tokio::test]
    async fn 취소된_파이프라인은_다음_노드를_실행하지_않는다() {
        let ctx = with_tools();
        let nodes = pipeline(json!([
            { "id": "a", "type": "tool", "tool": "echo", "input": {} },
            { "id": "b", "type": "tool", "tool": "echo", "input": {} }
        ]));
        let mut seen = Vec::new();
        let state = ctx.state::<WorkflowRuns>();
        let mut on_progress = |p: PipeProgress| {
            // Stop arrives while the first node is running.
            state.cancel();
            seen.push(p.node_id);
        };
        run_pipeline(&ctx, &nodes, &mut on_progress).await;
        assert_eq!(seen, ["a", "a"]);
    }
}
