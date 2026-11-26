//! The control server: it exposes a tool registry over a Unix socket, guards
//! it with a token, and publishes the socket in `mcp-endpoint.json`.
//!
//! The tools come ready in a [`ToolRegistry`]; this crate adds the two the
//! catalog needs (`tool_result_more`, `tool_profile_set`) and answers calls.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rever_scope::scope::scope_off_warning;
use rever_tools_api::tool::{Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{Content, ToolResult, err};
use serde::Serialize;
use serde_json::Value;
use tokio::runtime::Handle;

use rever_control::paging::ToolResultMore;
use rever_control::tool_profile::ToolProfileSet;
use rever_control::tool_profile_state::ToolProfileState;
use rever_control::write_private_file;

const ENDPOINT_FILE: &str = "mcp-endpoint.json";

/// `Date.now()`.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

// ── What the page did around a tool call ────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DialogAnswer {
    pub accept: bool,
    /// Epoch milliseconds.
    pub at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogRecord {
    /// `alert`, `confirm`, `prompt` or `beforeunload`.
    pub kind: String,
    pub message: String,
    /// Who closed it and how; unset while it is still open.
    pub answer: Option<DialogAnswer>,
}

/// The page-side facts and effects the TypeScript took from `chrome-cdp`,
/// `downloads`, `ai-events` and `cdp-eval`. The app supplies them once its
/// engine can; every method defaults to "nothing happened".
pub trait PageEvents: Send + Sync {
    /// A dialog on screen waiting for the user, if any.
    fn open_dialog(&self) -> Option<DialogRecord> {
        None
    }

    /// The last `limit` dialogs, oldest first.
    fn dialog_history(&self, _limit: usize) -> Vec<DialogRecord> {
        Vec::new()
    }

    /// File names of the downloads started at or after `since` (epoch milliseconds).
    fn downloads_started_since(&self, _since: i64) -> Vec<String> {
        Vec::new()
    }

    /// A tool call starts (`true`) or ends (`false`). Dialogs that open in
    /// between are answered for the agent; calls overlap, so count them.
    fn agent_busy(&self, _busy: bool) {}

    /// A non-browser tool starts: `label` is its name with spaces, `detail` its
    /// first arguments. Drives the activity log and the on-page "analyzing…" HUD.
    fn activity_started(&self, _label: &str, _detail: Option<&str>) {}

    /// The tool started above finished, or `failed`.
    fn activity_finished(&self, _failed: bool) {}
}

/// No page facts: the default until the app supplies its own.
pub struct NoPageEvents;

impl PageEvents for NoPageEvents {}

tokio::task_local! {
    /// Set for the task a tool call runs in.
    static TOOL_CALL: ();
}

/// Run `call` as a tool call: what it asks of the app counts as the agent's.
pub async fn as_tool_call<T>(call: impl Future<Output = T>) -> T {
    TOOL_CALL.scope((), call).await
}

/// Whether the running task is a tool call an MCP client made, not the app's own work.
pub fn in_tool_call() -> bool {
    TOOL_CALL.try_with(|_| ()).is_ok()
}

/// Marks the agent busy for as long as it lives, also when the call panics.
struct AgentBusy(Arc<dyn PageEvents>);

impl AgentBusy {
    fn enter(page: Arc<dyn PageEvents>) -> Self {
        page.agent_busy(true);
        Self(page)
    }
}

impl Drop for AgentBusy {
    fn drop(&mut self) {
        self.0.agent_busy(false);
    }
}

/// Tell the agent about dialogs answered, downloads started and an unscoped attack tool — all invisible otherwise.
fn with_page_notes(
    mut res: ToolResult,
    since: i64,
    tool_name: &str,
    page: &dyn PageEvents,
) -> ToolResult {
    let mut lines: Vec<String> = scope_off_warning(tool_name).into_iter().collect();
    for dialog in page.dialog_history(100) {
        let Some(answer) = dialog.answer.filter(|answer| answer.at >= since) else {
            continue;
        };
        lines.push(format!(
            "[page {}] \"{}\" → auto-answered {}",
            dialog.kind,
            dialog.message,
            if answer.accept { "OK" } else { "Cancel" }
        ));
    }
    for filename in page.downloads_started_since(since) {
        lines.push(format!(
            "[download started] \"{filename}\" → call wait_for_download for the saved file"
        ));
    }
    if !lines.is_empty() {
        res.content.push(Content::Text {
            text: lines.join("\n"),
        });
    }
    res
}

/// JavaScript's `Number.prototype.toString`: plain digits from 1e-6 up to 1e21,
/// exponent form outside that.
fn js_number(x: f64) -> String {
    if x == 0.0 {
        return "0".to_owned(); // -0 prints as "0" too
    }
    if (1e-6..1e21).contains(&x.abs()) {
        return format!("{x}");
    }
    let text = format!("{x:e}");
    match text.split_once('e') {
        Some((mantissa, exponent)) if !exponent.starts_with('-') => {
            format!("{mantissa}e+{exponent}")
        }
        _ => text,
    }
}

/// `text.slice(0, units)` counted in UTF-16 units. A character that straddles
/// the cut is dropped whole: Rust cannot hold the lone half JavaScript keeps.
fn slice_utf16(text: &str, units: usize) -> &str {
    let mut seen = 0;
    for (byte, ch) in text.char_indices() {
        seen += ch.len_utf16();
        if seen > units {
            return text.get(..byte).unwrap_or(text);
        }
    }
    text
}

fn summarize_tool_args(schema: &Value, input: &Value) -> Option<String> {
    let Value::Object(input) = input else {
        return None;
    };
    // The TypeScript handler was handed zod's parse of the arguments, not the
    // client's object: the schema's keys in the schema's order, defaults filled
    // in, unknown keys dropped.
    let entries: Vec<(&String, &Value)> = match schema.get("properties") {
        Some(Value::Object(properties)) => properties
            .iter()
            .filter_map(|(key, property)| {
                Some((key, input.get(key).or_else(|| property.get("default"))?))
            })
            .collect(),
        _ => input.iter().collect(),
    };
    let mut parts: Vec<String> = Vec::new();
    for (key, value) in entries {
        let text = match value {
            Value::Null | Value::Array(_) | Value::Object(_) => continue,
            Value::Bool(flag) => flag.to_string(),
            Value::Number(number) => number
                .as_f64()
                .map_or_else(|| number.to_string(), js_number),
            Value::String(text) => text.clone(),
        };
        parts.push(format!("{key}: {}", slice_utf16(&text, 40)));
        if parts.len() >= 2 {
            break;
        }
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

// ── One MCP session ─────────────────────────────────────────────────────────

/// What every session of one server shares.
struct Shared {
    registry: Arc<ToolRegistry>,
    context: Arc<ToolContext>,
    page: Arc<dyn PageEvents>,
    profile: Arc<ToolProfileState>,
    version: String,
    runtime: Handle,
}

impl Shared {
    /// The registry's tools, then this crate's own two unless the app already
    /// registered them (which is how it fixes their place in the list).
    fn defs(&self) -> Vec<ToolDef> {
        let mut defs = self.registry.defs();
        for own in [ToolResultMore.def(), ToolProfileSet.def()] {
            if !defs.iter().any(|def| def.name == own.name) {
                defs.push(own);
            }
        }
        defs
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.registry.find(name).or_else(|| match name {
            "tool_result_more" => Some(Arc::new(ToolResultMore) as Arc<dyn Tool>),
            "tool_profile_set" => Some(Arc::new(ToolProfileSet) as Arc<dyn Tool>),
            _ => None,
        })
    }

    /// Wrap EVERY non-browser tool call in a lightweight activity event. Browser
    /// tools already emit their own visual actions (cursor/box); the
    /// static-analysis tools (scripts, crypto, wasm, probes, …) otherwise run
    /// invisibly, which makes the UI look frozen. This surfaces them in the AI
    /// activity overlay so the user always sees what the agent is doing.
    async fn call_with_activity(&self, tool: Arc<dyn Tool>, name: &str, args: Value) -> ToolResult {
        let non_browser = !name.starts_with("browser_");
        if non_browser {
            let detail = summarize_tool_args(&tool.def().input_schema, &args);
            let pretty = name.replace('_', " ");
            // Renderer activity log + an on-page "analyzing…" HUD so backend/static
            // analysis (scripts, crypto, wasm, probes) isn't invisible.
            self.page.activity_started(&pretty, detail.as_deref());
        }
        // An open dialog blocks the page, so any page command would hang until the user answers.
        if let Some(open) = self.page.open_dialog() {
            return err(format!(
                "A page {}() is open on screen and waits for the user: \"{}\". Ask the user to answer it, then retry.",
                open.kind, open.message
            ));
        }
        let since = now_ms();
        let context = self.context.clone();
        let page = self.page.clone();
        // Its own task, so a tool that panics fails one call, not the session.
        let call = self.runtime.spawn(async move {
            let _busy = AgentBusy::enter(page);
            as_tool_call(tool.call(&context, args)).await
        });
        match call.await {
            Ok(res) => {
                // The SDK refused bad arguments before the wrapped handler ran,
                // so that refusal never carried notes: nothing "ran".
                let res = if refused_arguments(&res, name) {
                    res
                } else {
                    with_page_notes(res, since, name, self.page.as_ref())
                };
                if non_browser {
                    self.page.activity_finished(false);
                }
                res
            }
            Err(failure) => {
                if non_browser {
                    self.page.activity_finished(true);
                }
                err(panic_message(failure))
            }
        }
    }
}

/// Whether `res` is a tool's refusal of its arguments. Validating is the tool's
/// job here, and every tool crate words the refusal as the SDK did.
fn refused_arguments(res: &ToolResult, tool_name: &str) -> bool {
    let prefix = format!(
        "MCP error -32602: Input validation error: Invalid arguments for tool {tool_name}: "
    );
    res.is_error
        && matches!(res.content.first(), Some(Content::Text { text }) if text.starts_with(&prefix))
}

/// The text of a thrown error, as the SDK put it in the tool result.
fn panic_message(failure: tokio::task::JoinError) -> String {
    let Ok(payload) = failure.try_into_panic() else {
        return "tool call was cancelled".to_owned();
    };
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "tool call panicked".to_owned()
    }
}

/// The live registered-tool bit of one tool in one session.
// ── The control socket ──────────────────────────────────────────────────────

/// Hands one socket call to the same `Shared` the MCP sessions use, so the two
/// transports cannot drift while both exist.
fn socket_handler(shared: Arc<Shared>) -> rever_control::socket::Handler {
    Arc::new(move |call: rever_control::socket::Call| {
        let name = call.tool;
        let args = call.args;
        // Not a tool: the CLI's catalog, so `revb list` and `revb describe`
        // need no second protocol.
        if name == "__catalog" {
            let tools: Vec<Value> = shared
                .defs()
                .into_iter()
                .map(|def| {
                    serde_json::json!({
                        "name": def.name,
                        "description": def.description,
                        "inputSchema": def.input_schema,
                    })
                })
                .collect();
            return rever_control::socket::Reply::result(
                serde_json::json!({ "version": shared.version, "tools": tools }),
            );
        }
        let Some(tool) = shared.find(&name) else {
            return rever_control::socket::Reply::error(format!("tool not found: {name}"));
        };
        let runtime = shared.runtime.clone();
        let inner = shared.clone();
        // A socket thread is not a runtime thread, so blocking here is safe.
        let result =
            runtime.block_on(async move { inner.call_with_activity(tool, &name, args).await });
        match serde_json::to_value(result) {
            Ok(value) => rever_control::socket::Reply::result(value),
            Err(e) => rever_control::socket::Reply::error(e.to_string()),
        }
    })
}

// ── Start and stop ──────────────────────────────────────────────────────────

/// What the app can supply beyond the tools and their context.
pub struct ServerOptions {
    /// Reported as the server version (the app's version).
    pub version: String,
    pub page: Arc<dyn PageEvents>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            page: Arc::new(NoPageEvents),
        }
    }
}

/// A running server. Dropping it closes the socket and removes the endpoint file.
pub struct McpServer {
    /// The socket's path. Named `url` because the app passes it around as one.
    pub url: String,
    pub token: String,
    /// Unused by the CLI; kept for the app's call sites.
    pub auth_header: String,
    endpoint_file: PathBuf,
    profile: Arc<ToolProfileState>,
    /// Kept alive, not read: dropping it stops the accept loop.
    #[allow(dead_code)]
    control: rever_control::socket::ControlServer,
    /// Kept alive, not read: dropping it kills the tool tasks.
    #[allow(dead_code)]
    runtime: tokio::runtime::Runtime,
}

impl McpServer {
    /// The tool profile this server tracks.
    pub fn tool_profile(&self) -> &Arc<ToolProfileState> {
        &self.profile
    }

    pub fn close(self) {
        let _ = std::fs::remove_file(&self.endpoint_file);
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.endpoint_file);
    }
}

pub fn start(registry: Arc<ToolRegistry>, context: Arc<ToolContext>) -> Result<McpServer, String> {
    start_with(registry, context, ServerOptions::default())
}

pub fn start_with(
    registry: Arc<ToolRegistry>,
    context: Arc<ToolContext>,
    options: ServerOptions,
) -> Result<McpServer, String> {
    let token = random_token()?;
    let profile = ToolProfileState::of(&context);
    let endpoint_file = endpoint_file_path(&context.data_dir);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("rever-tools")
        .enable_all()
        .build()
        .map_err(|e| format!("failed to start the tool runtime: {e}"))?;

    let shared = Arc::new(Shared {
        registry,
        context: context.clone(),
        page: options.page,
        profile: profile.clone(),
        version: options.version,
        runtime: runtime.handle().clone(),
    });

    let socket_path = context.data_dir.join("control.sock");
    let control = rever_control::socket::serve(&socket_path, &token, socket_handler(shared))
        .map_err(|e| format!("failed to open the control socket: {e}"))?;

    let url = socket_path.to_string_lossy().into_owned();
    eprintln!("[control] listening on {url}");
    write_endpoint_file(&endpoint_file, &url, Some(&socket_path), &token);

    Ok(McpServer {
        url,
        auth_header: format!("Bearer {token}"),
        token,
        endpoint_file,
        profile,
        control,
        runtime,
    })
}

pub fn endpoint_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join(ENDPOINT_FILE)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Endpoint<'a> {
    url: &'a str,
    /// The CLI's way in. Absent on a platform without Unix sockets.
    #[serde(skip_serializing_if = "Option::is_none")]
    socket: Option<String>,
    token: &'a str,
    pid: u32,
    started_at: String,
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// `new Date(ms).toISOString()` for the four-digit years a clock reports.
fn to_iso_string(ms: i64) -> String {
    let (year, month, day) = civil_from_days(ms.div_euclid(86_400_000));
    let ms_of_day = ms.rem_euclid(86_400_000);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        ms_of_day / 3_600_000,
        ms_of_day / 60_000 % 60,
        ms_of_day / 1_000 % 60,
        ms_of_day % 1_000
    )
}

fn write_endpoint_file(file: &Path, url: &str, socket: Option<&Path>, token: &str) {
    let endpoint = Endpoint {
        url,
        socket: socket.map(|p| p.to_string_lossy().into_owned()),
        token,
        pid: std::process::id(),
        started_at: to_iso_string(now_ms()),
    };
    let written = serde_json::to_string_pretty(&endpoint)
        .map_err(std::io::Error::other)
        .and_then(|text| write_private_file(file, &text));
    if let Err(e) = written {
        eprintln!("[mcp] could not publish endpoint file: {e}");
    }
}

/// `randomBytes(24).toString('hex')`.
fn random_token() -> Result<String, String> {
    let mut bytes = [0_u8; 24];
    getrandom::fill(&mut bytes).map_err(|e| format!("no random bytes for the MCP token: {e}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

