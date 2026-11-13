//! Port of `src/main/mcp/server.ts` — the MCP server: it exposes a tool registry
//! over streamable HTTP on a random localhost port, guards it with a bearer
//! token, and publishes the address in `mcp-endpoint.json`.
//!
//! The TypeScript built one SDK `McpServer` per session and registered every
//! tool on it. Here the tools come ready in a [`ToolRegistry`]; a session keeps
//! only the enabled bit of each tool, which the catalog mode (`X-Rever-Catalog`)
//! and the tool profile decide.

use std::borrow::Cow;
use std::future::poll_fn;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::{Body, HttpBody};
use axum::extract::State;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header};
use rever_scope::scope::scope_off_warning;
use rever_tool_profile::tool_profile::{
    CatalogKind, ToolListServer, ToolProfile, ToolToggle, apply_follow_catalog,
    catalog_disables_tools, catalog_from_header,
};
use rever_tools_api::tool::{Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{Content, ToolResult, err};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CompleteRequestParams, CompleteResult,
    CustomRequest, CustomResult, ErrorCode, Implementation, InitializeRequestParams,
    InitializeResult, JsonObject, ListPromptsResult, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
    ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{
    SessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{ErrorData as McpError, Peer, RoleServer, ServerHandler};
use serde::Serialize;
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use url::Url;

use crate::tool_profile_state::{FollowServer, ToolProfileState};
use crate::tools::paging::ToolResultMore;
use crate::tools::tool_profile::ToolProfileSet;
use crate::write_private_file;

const READ_BODY_LIMIT: usize = 16 * 1024 * 1024; // 16MB

const ENDPOINT_FILE: &str = "mcp-endpoint.json";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

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
            tool.call(&context, args).await
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
struct ToolBit {
    enabled: bool,
}

impl ToolToggle for ToolBit {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn enable(&mut self) {
        self.enabled = true;
    }

    fn disable(&mut self) {
        self.enabled = false;
    }
}

/// The part of a session the profile state holds on to while it follows.
struct SessionLink {
    /// Fixed by the `initialize` request's `X-Rever-Catalog` header.
    catalog: OnceLock<CatalogKind>,
    /// Every tool registered for this session, in list order.
    handles: Mutex<Vec<(String, ToolBit)>>,
    peer: Mutex<Option<Peer<RoleServer>>>,
    runtime: Handle,
}

impl SessionLink {
    /// `None`: no such tool in this session.
    fn enabled(&self, name: &str) -> Option<bool> {
        lock(&self.handles)
            .iter()
            .find(|(known, _)| known == name)
            .map(|(_, handle)| handle.enabled)
    }
}

impl ToolListServer for SessionLink {
    fn is_connected(&self) -> bool {
        lock(&self.peer)
            .as_ref()
            .is_some_and(|peer| !peer.is_transport_closed())
    }

    fn send_tool_list_changed(&self) {
        let Some(peer) = lock(&self.peer).clone() else {
            return;
        };
        // The profile can change from the app's own thread, outside the runtime.
        self.runtime.spawn(async move {
            let _ = peer.notify_tool_list_changed().await;
        });
    }

    // The handles here only flip a bit and never notify on their own, so there
    // is nothing to mute while they are toggled.
    fn set_tool_list_changed_muted(&self, _muted: bool) {}
}

impl FollowServer for SessionLink {
    fn apply_follow_catalog(&self, profile: ToolProfile) {
        let mut handles = lock(&self.handles);
        apply_follow_catalog(
            self,
            handles
                .iter_mut()
                .map(|(name, handle)| (name.as_str(), handle)),
            profile,
        );
    }
}

/// The handler rmcp runs for one session (`buildMcpServer` in the TypeScript).
struct Session {
    shared: Arc<Shared>,
    link: Arc<SessionLink>,
}

impl Session {
    fn new(shared: Arc<Shared>) -> Self {
        let link = Arc::new(SessionLink {
            catalog: OnceLock::new(),
            handles: Mutex::default(),
            peer: Mutex::default(),
            runtime: shared.runtime.clone(),
        });
        Self { shared, link }
    }

    fn follow_server(&self) -> Arc<dyn FollowServer> {
        self.link.clone()
    }

    /// Register the session's tools. Until this runs the session has none.
    fn open(&self, catalog: CatalogKind) -> bool {
        if self.link.catalog.set(catalog).is_err() {
            return false;
        }
        *lock(&self.link.handles) = self
            .shared
            .defs()
            .into_iter()
            // A full catalog is for callers that drive any tool; they have no profile to switch.
            .filter(|def| catalog != CatalogKind::Full || def.name != "tool_profile_set")
            .map(|def| (def.name, ToolBit { enabled: true }))
            .collect();
        // Follow disables before the peer is stored, so the first tools/list is
        // already filtered and no list_changed goes out. full and direct keep
        // every tool enabled.
        if catalog_disables_tools(catalog) {
            self.shared
                .profile
                .register_follow_server(self.follow_server());
            self.link.apply_follow_catalog(self.shared.profile.get());
        }
        true
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shared
            .profile
            .unregister_follow_server(&self.follow_server());
    }
}

/// `req.headers[name]` in Node: a repeated header arrives joined with ", ".
fn node_header(headers: &HeaderMap, name: &str) -> Option<String> {
    let values: Vec<Cow<'_, str>> = headers
        .get_all(name)
        .iter()
        .map(|value| String::from_utf8_lossy(value.as_bytes()))
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

fn catalog_of(headers: &HeaderMap) -> CatalogKind {
    catalog_from_header(node_header(headers, "x-rever-catalog").as_deref())
}

fn to_rmcp_tool(def: ToolDef) -> rmcp::model::Tool {
    let schema = match def.input_schema {
        Value::Object(schema) => schema,
        // The SDK's stand-in for a tool without an object schema.
        _ => JsonObject::from_iter([
            ("type".to_owned(), Value::from("object")),
            ("properties".to_owned(), Value::Object(JsonObject::new())),
        ]),
    };
    rmcp::model::Tool::new(def.name, def.description, schema)
}

fn to_call_tool_result(res: ToolResult) -> CallToolResult {
    let mut out = CallToolResult::default();
    // Both sides serialize a content block in the MCP wire shape, so going
    // through it keeps this in step with any block kind added later.
    out.content = res
        .content
        .iter()
        .filter_map(|block| serde_json::to_value(block).ok())
        .filter_map(|block| serde_json::from_value(block).ok())
        .collect();
    // A successful result has no `isError` key at all, not `isError: false`.
    out.is_error = res.is_error.then_some(true);
    out
}

/// The SDK's answer to a request it had no handler for.
fn method_not_found() -> McpError {
    McpError::new(ErrorCode::METHOD_NOT_FOUND, "Method not found", None)
}

impl ServerHandler for Session {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new(
            "rever-traffic",
            self.shared.version.clone(),
        ))
    }

    // Every session starts with `initialize`, as with the TypeScript SDK; the
    // sessionless 2026-07-28 revision is not offered.
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::known_up_to(&ProtocolVersion::V_2025_11_25))
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        let result = self.negotiate_initialize(&request)?;
        let catalog = context
            .extensions
            .get::<Parts>()
            .map_or(CatalogKind::Follow, |parts| catalog_of(&parts.headers));
        if !self.open(catalog) {
            return Err(McpError::invalid_request(
                "Invalid Request: Server already initialized",
                None,
            ));
        }
        context.peer.set_peer_info(request);
        *lock(&self.link.peer) = Some(context.peer);
        Ok(result)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = self
            .shared
            .defs()
            .into_iter()
            .filter(|def| self.link.enabled(&def.name) == Some(true))
            .map(to_rmcp_tool)
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.as_ref();
        let args = request.arguments.map_or(Value::Null, Value::Object);
        // The SDK reported both refusals inside the tool result, not as a JSON-RPC error.
        let res = match (self.link.enabled(name), self.shared.find(name)) {
            (Some(true), Some(tool)) => self.shared.call_with_activity(tool, name, args).await,
            (Some(false), _) => err(format!("MCP error -32602: Tool {name} disabled")),
            _ => err(format!("MCP error -32602: Tool {name} not found")),
        };
        Ok(to_call_tool_result(res).into())
    }

    // The TypeScript server had handlers for tools only and refused the rest;
    // rmcp's defaults would answer these with empty results.
    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Err(method_not_found())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Err(method_not_found())
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Err(method_not_found())
    }

    async fn complete(
        &self,
        _request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, McpError> {
        Err(method_not_found())
    }

    async fn on_custom_request(
        &self,
        _request: CustomRequest,
        _context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, McpError> {
        Err(method_not_found())
    }
}

// ── HTTP ────────────────────────────────────────────────────────────────────

/// What the HTTP handler needs for every request.
struct Gate {
    // Any local process can reach 127.0.0.1, and the Origin check below only
    // stops browsers. The token (published with the URL, mode 0600) is what
    // keeps a stray local process out of the user's authenticated sessions.
    token: String,
    bound_port: u16,
    sessions: Arc<LocalSessionManager>,
    mcp: StreamableHttpService<Session, LocalSessionManager>,
}

fn plain(status: StatusCode, text: &'static str) -> Response<Body> {
    let mut response = Response::new(Body::from(text));
    *response.status_mut() = status;
    response
}

/// Constant-time check of `Authorization: Bearer <token>`.
fn is_authorized(header: Option<&[u8]>, token: &str) -> bool {
    let Some(header) = header else {
        return false;
    };
    let expected = format!("Bearer {token}");
    header.len() == expected.len()
        && header
            .iter()
            .zip(expected.bytes())
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0
}

/// DNS-rebinding check of the Origin header. The agent's own requests carry no
/// Origin, so a missing one passes.
fn is_origin_allowed(origin: Option<&str>, bound_port: u16) -> bool {
    let Some(origin) = origin.filter(|origin| !origin.is_empty()) else {
        return true; // the agent or a same-process request
    };
    let Ok(url) = Url::parse(origin) else {
        return false;
    };
    let default_port = if origin.starts_with("https") { 443 } else { 80 };
    matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
        && url.port().unwrap_or(default_port) == bound_port
}

/// The SDK's `isInitializeRequest`, down to the fields a client must send; the
/// nested shape of `capabilities` is left to the protocol layer.
fn is_initialize_request(body: &Value) -> bool {
    let params = &body["params"];
    body["method"] == "initialize"
        && params["protocolVersion"].is_string()
        && params["capabilities"].is_object()
        && params["clientInfo"]["name"].is_string()
        && params["clientInfo"]["version"].is_string()
}

async fn read_body(mut body: Body) -> Result<Vec<u8>, StatusCode> {
    let mut raw = Vec::new();
    while let Some(frame) = poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
        let Ok(data) = frame.map_err(|_| StatusCode::BAD_REQUEST)?.into_data() else {
            continue;
        };
        if raw.len() + data.len() > READ_BODY_LIMIT {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        raw.extend_from_slice(&data);
    }
    Ok(raw)
}

/// A request body as `JSON.parse(buffer.toString('utf8'))` took it, in a form
/// serde_json takes too: invalid UTF-8 becomes U+FFFD (as in Node), and so does
/// an escaped lone surrogate, which a JavaScript string held and a Rust one cannot.
fn js_tolerant_json(raw: Vec<u8>) -> Vec<u8> {
    let mut raw = match String::from_utf8_lossy(&raw) {
        Cow::Borrowed(_) => raw,
        Cow::Owned(text) => text.into_bytes(),
    };
    // The surrogate a `\uXXXX` escape at `at` names, if it names one.
    let surrogate = |raw: &[u8], at: usize| -> Option<u16> {
        let hex = raw.get(at..at + 6)?.strip_prefix(b"\\u")?;
        let code = u16::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
        (0xD800..=0xDFFF).contains(&code).then_some(code)
    };
    let mut at = 0;
    while let Some(&byte) = raw.get(at) {
        if byte != b'\\' {
            at += 1;
            continue;
        }
        match surrogate(&raw, at) {
            // A high half followed by its low half is one character: leave it.
            Some(0xD800..=0xDBFF) if matches!(surrogate(&raw, at + 6), Some(0xDC00..=0xDFFF)) => {
                at += 12;
            }
            Some(_) => {
                if let Some(hex) = raw.get_mut(at + 2..at + 6) {
                    hex.copy_from_slice(b"fffd");
                }
                at += 6;
            }
            // Any other escape: step over the escaped byte, so the `u` after an
            // escaped backslash is not read as the start of an escape.
            None => at += 2,
        }
    }
    raw
}

async fn handle(State(gate): State<Arc<Gate>>, request: Request<Body>) -> Response<Body> {
    // The whole request target, so a query string is not `/mcp` either.
    if request.uri().path_and_query().map(|target| target.as_str()) != Some("/mcp") {
        return plain(StatusCode::NOT_FOUND, "not found");
    }

    let authorization = request.headers().get(header::AUTHORIZATION);
    if !is_authorized(authorization.map(HeaderValue::as_bytes), &gate.token) {
        let mut response = plain(
            StatusCode::UNAUTHORIZED,
            "unauthorized: missing or invalid bearer token",
        );
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }

    let method = request.method().clone();
    // On POST, an Origin header must name a trusted origin.
    if method == Method::POST {
        let origin = node_header(request.headers(), "origin");
        if !is_origin_allowed(origin.as_deref(), gate.bound_port) {
            return plain(StatusCode::FORBIDDEN, "forbidden: untrusted origin");
        }
    }

    let has_session = match node_header(request.headers(), "mcp-session-id") {
        Some(id) if !id.is_empty() => gate.sessions.has_session(&id.into()).await.unwrap_or(false),
        _ => false,
    };

    if method == Method::POST {
        let (mut parts, body) = request.into_parts();
        let raw = match read_body(body).await {
            Ok(raw) => raw,
            Err(StatusCode::PAYLOAD_TOO_LARGE) => {
                return plain(StatusCode::PAYLOAD_TOO_LARGE, "request body too large");
            }
            Err(_) => return plain(StatusCode::BAD_REQUEST, "bad request"),
        };
        let raw = js_tolerant_json(raw);
        let body: Option<Value> = if raw.is_empty() {
            None
        } else {
            match serde_json::from_slice(&raw) {
                Ok(body) => Some(body),
                Err(_) => return plain(StatusCode::BAD_REQUEST, "bad request"),
            }
        };

        if !has_session {
            if !body.as_ref().is_some_and(is_initialize_request) {
                return plain(StatusCode::BAD_REQUEST, "missing or invalid session");
            }
            // An initialize under a stale session id opens a new session, as it
            // did in the TypeScript; rmcp would answer the stale id with 404.
            parts.headers.remove("mcp-session-id");
        }
        let request = Request::from_parts(parts, Body::from(raw));
        return gate.mcp.handle(request).await.map(Body::new);
    }

    if method == Method::GET || method == Method::DELETE {
        if !has_session {
            return plain(StatusCode::BAD_REQUEST, "no session");
        }
        return gate.mcp.handle(request).await.map(Body::new);
    }

    plain(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
}

// ── Endpoint file ───────────────────────────────────────────────────────────

/// Where the live endpoint is published. The port is chosen by the OS, so any
/// out-of-process client (an external agent, a script, a test harness) has no
/// way to find the server without this.
pub fn endpoint_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join(ENDPOINT_FILE)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Endpoint<'a> {
    url: &'a str,
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

fn write_endpoint_file(file: &Path, url: &str, token: &str) {
    let endpoint = Endpoint {
        url,
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

// ── Start and stop ──────────────────────────────────────────────────────────

/// What the app can supply beyond the tools and their context.
pub struct ServerOptions {
    /// Reported as the server version in `initialize` (the app's version).
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

/// A running server. Dropping it stops the server; [`McpServer::close`] also
/// removes the endpoint file and waits for the server thread.
pub struct McpServer {
    pub url: String,
    /// Bearer token every request must carry — see `auth_header`.
    pub token: String,
    /// Ready-made `Authorization` header value for in-process and child clients.
    pub auth_header: String,
    endpoint_file: PathBuf,
    profile: Arc<ToolProfileState>,
    // Dropping the sender is the stop signal.
    stop: oneshot::Sender<()>,
    thread: JoinHandle<()>,
}

impl McpServer {
    /// The tool profile this server's follow sessions track.
    pub fn tool_profile(&self) -> &Arc<ToolProfileState> {
        &self.profile
    }

    pub fn close(self) {
        let _ = std::fs::remove_file(&self.endpoint_file);
        drop(self.stop);
        let _ = self.thread.join();
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
    let bind_error = |e: std::io::Error| format!("failed to bind MCP server: {e}");
    let token = random_token()?;
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(bind_error)?;
    listener.set_nonblocking(true).map_err(bind_error)?;
    let bound_port = listener.local_addr().map_err(bind_error)?.port();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("rever-mcp")
        .enable_all()
        .build()
        .map_err(|e| format!("failed to start the MCP server runtime: {e}"))?;
    let listener = {
        let _in_runtime = runtime.enter();
        tokio::net::TcpListener::from_std(listener).map_err(bind_error)?
    };

    let profile = ToolProfileState::of(&context);
    let endpoint_file = endpoint_file_path(&context.data_dir);
    let shared = Arc::new(Shared {
        registry,
        context,
        page: options.page,
        profile: profile.clone(),
        version: options.version,
        runtime: runtime.handle().clone(),
    });

    let mut sessions = LocalSessionManager::default();
    // A session lives until the client deletes it, however long the agent
    // idles between turns; rmcp would drop it after five quiet minutes.
    sessions.session_config.keep_alive = None;
    // No priming events: nothing here can replay a stream, and their empty
    // `data:` line is not JSON to the clients that read every data line.
    sessions.session_config.sse_retry = None;
    let sessions = Arc::new(sessions);
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts([
            format!("127.0.0.1:{bound_port}"),
            format!("localhost:{bound_port}"),
        ])
        .with_sse_retry(None)
        .with_max_request_body_bytes(READ_BODY_LIMIT);
    let mcp = StreamableHttpService::new(
        move || Ok(Session::new(shared.clone())),
        sessions.clone(),
        config,
    );
    let app = Router::new().fallback(handle).with_state(Arc::new(Gate {
        token: token.clone(),
        bound_port,
        sessions,
        mcp,
    }));

    let (stop, stopped) = oneshot::channel::<()>();
    let thread = std::thread::Builder::new()
        .name("rever-mcp".to_owned())
        .spawn(move || {
            runtime.block_on(async move {
                tokio::spawn(async move {
                    if let Err(e) = axum::serve(listener, app).await {
                        eprintln!("[mcp] server stopped: {e}");
                    }
                });
                // Resolves when the McpServer is closed or dropped.
                let _ = stopped.await;
            });
            // Open event streams and running tools must not hold the stop back.
            runtime.shutdown_background();
        })
        .map_err(|e| format!("failed to start the MCP server thread: {e}"))?;

    let url = format!("http://127.0.0.1:{bound_port}/mcp");
    eprintln!("[mcp] listening on {url}");
    write_endpoint_file(&endpoint_file, &url, &token);

    Ok(McpServer {
        url,
        auth_header: format!("Bearer {token}"),
        token,
        endpoint_file,
        profile,
        stop,
        thread,
    })
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    use std::time::Duration;

    use reqwest::blocking::{Client as Http, RequestBuilder, Response as HttpResponse};
    use rever_tools_api::tool::{BoxFuture, NoBrowser};
    use rever_tools_api::utils::{ResultStore, ok};
    use serde_json::json;

    use super::*;
    use crate::tool_profile_state::ProfileSource;
    use crate::tool_profile_state::tests::TempDir;

    // ── Stand-in tools ──────────────────────────────────────────────────────

    /// Answers with its own name and the arguments it was handed.
    struct Echo(&'static str);

    impl Tool for Echo {
        fn def(&self) -> ToolDef {
            ToolDef::new(
                self.0,
                "echoes its arguments",
                json!({ "type": "object", "properties": { "q": { "type": "string" } } }),
            )
        }

        fn call<'a>(&'a self, _: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
            Box::pin(async move { ok(format!("{}:{args}", self.0)) })
        }
    }

    /// Answers with 30,000 characters, parked for paging.
    struct Big;

    impl Tool for Big {
        fn def(&self) -> ToolDef {
            ToolDef::new("get_request", "a long result", Value::Null)
        }

        fn call<'a>(&'a self, ctx: &'a ToolContext, _: Value) -> BoxFuture<'a, ToolResult> {
            Box::pin(async move {
                let store = ctx.state::<Mutex<ResultStore>>();
                lock(&store).ok_budgeted("0123456789".repeat(3_000), None)
            })
        }
    }

    struct Boom;

    impl Tool for Boom {
        fn def(&self) -> ToolDef {
            ToolDef::new("boom", "always panics", json!({ "type": "object" }))
        }

        fn call<'a>(&'a self, _: &'a ToolContext, _: Value) -> BoxFuture<'a, ToolResult> {
            Box::pin(async move { panic!("tool blew up") })
        }
    }

    /// In registration order: core, core (browser), reverse-only, probe-only,
    /// hidden, unknown to the profile table, core, unknown.
    const REGISTERED: [&str; 8] = [
        "list_requests",
        "browser_snapshot",
        "grep_scripts",
        "intruder_run",
        "bp_add",
        "site_special",
        "get_request",
        "boom",
    ];

    fn registry() -> Arc<ToolRegistry> {
        let mut registry = ToolRegistry::default();
        for name in &REGISTERED[..6] {
            registry.register(Echo(name));
        }
        registry.register(Big);
        registry.register(Boom);
        Arc::new(registry)
    }

    // ── Page stand-in ───────────────────────────────────────────────────────

    #[derive(Default)]
    struct Page {
        open: Mutex<Option<DialogRecord>>,
        history: Mutex<Vec<DialogRecord>>,
        downloads: Mutex<Vec<String>>,
        /// Every `since` the server asked about downloads with.
        asked_since: Mutex<Vec<i64>>,
        events: Mutex<Vec<String>>,
    }

    impl Page {
        fn log(&self, event: String) {
            lock(&self.events).push(event);
        }

        fn events(&self) -> Vec<String> {
            lock(&self.events).clone()
        }
    }

    impl PageEvents for Page {
        fn open_dialog(&self) -> Option<DialogRecord> {
            lock(&self.open).clone()
        }

        fn dialog_history(&self, limit: usize) -> Vec<DialogRecord> {
            self.log(format!("history {limit}"));
            lock(&self.history).clone()
        }

        fn downloads_started_since(&self, since: i64) -> Vec<String> {
            lock(&self.asked_since).push(since);
            lock(&self.downloads).clone()
        }

        fn agent_busy(&self, busy: bool) {
            self.log(format!("busy {busy}"));
        }

        fn activity_started(&self, label: &str, detail: Option<&str>) {
            self.log(format!("start {label} | {}", detail.unwrap_or("-")));
        }

        fn activity_finished(&self, failed: bool) {
            self.log(format!("finish failed={failed}"));
        }
    }

    fn dialog(kind: &str, message: &str, answer: Option<(bool, i64)>) -> DialogRecord {
        DialogRecord {
            kind: kind.to_owned(),
            message: message.to_owned(),
            answer: answer.map(|(accept, at)| DialogAnswer { accept, at }),
        }
    }

    // ── Server and client ───────────────────────────────────────────────────

    struct Fixture {
        dir: TempDir,
        server: McpServer,
    }

    fn serve_with(page: Arc<dyn PageEvents>) -> Fixture {
        let dir = TempDir::new("server");
        let context = ToolContext::new(Arc::default(), Arc::new(NoBrowser), dir.0.clone());
        let options = ServerOptions {
            version: "9.8.7".to_owned(),
            page,
        };
        let server = start_with(registry(), Arc::new(context), options).unwrap();
        Fixture { dir, server }
    }

    fn serve() -> Fixture {
        serve_with(Arc::new(NoPageEvents))
    }

    const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#;

    /// The JSON-RPC message in a streamable-HTTP reply: SSE `data:` line or plain JSON.
    fn message_of(body: &str) -> Value {
        let line = body
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .unwrap_or(body);
        serde_json::from_str(line).unwrap_or_else(|e| panic!("not a JSON-RPC reply ({e}): {body}"))
    }

    /// A minimal MCP client over real HTTP, like scripts/mcp-call.py.
    struct Client {
        http: Http,
        url: String,
        auth: String,
        catalog: Option<&'static str>,
        sid: Option<String>,
        next_id: u64,
    }

    impl Client {
        fn new(server: &McpServer, catalog: Option<&'static str>) -> Self {
            Self {
                http: Http::new(),
                url: server.url.clone(),
                auth: server.auth_header.clone(),
                catalog,
                sid: None,
                next_id: 0,
            }
        }

        /// A client that has finished the initialize handshake.
        fn connect(server: &McpServer, catalog: Option<&'static str>) -> Self {
            let mut client = Self::new(server, catalog);
            let reply = client.initialize();
            assert!(reply["result"]["serverInfo"].is_object(), "{reply}");
            client
        }

        fn initialize(&mut self) -> Value {
            let reply = self.rpc(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "1" }
                }),
            );
            let done =
                self.post(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
            assert_eq!(done.status(), 202);
            reply
        }

        fn request(&self, method: reqwest::Method) -> RequestBuilder {
            let mut request = self
                .http
                .request(method, &self.url)
                .header("Authorization", &self.auth)
                .header("Accept", "application/json, text/event-stream");
            if let Some(catalog) = self.catalog {
                request = request.header("X-Rever-Catalog", catalog);
            }
            if let Some(sid) = &self.sid {
                request = request.header("mcp-session-id", sid);
            }
            request
        }

        fn post(&mut self, payload: &Value) -> HttpResponse {
            let response = self
                .request(reqwest::Method::POST)
                .header("Content-Type", "application/json")
                .body(payload.to_string())
                .send()
                .unwrap();
            if self.sid.is_none() {
                self.sid = response
                    .headers()
                    .get("mcp-session-id")
                    .map(|id| id.to_str().unwrap().to_owned());
            }
            response
        }

        fn rpc(&mut self, method: &str, params: Value) -> Value {
            self.next_id += 1;
            let response = self.post(&json!({
                "jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params
            }));
            assert_eq!(response.status(), 200);
            message_of(&response.text().unwrap())
        }

        fn tools(&mut self) -> Vec<String> {
            let reply = self.rpc("tools/list", json!({}));
            reply["result"]["tools"]
                .as_array()
                .unwrap_or_else(|| panic!("no tools in {reply}"))
                .iter()
                .map(|tool| tool["name"].as_str().unwrap().to_owned())
                .collect()
        }

        /// The `result` of a tools/call.
        fn call(&mut self, name: &str, args: Value) -> Value {
            let reply = self.rpc("tools/call", json!({ "name": name, "arguments": args }));
            assert!(reply["result"].is_object(), "{reply}");
            reply["result"].clone()
        }
    }

    fn text_result(text: &str) -> Value {
        json!({ "content": [{ "type": "text", "text": text }] })
    }

    fn error_result(text: &str) -> Value {
        json!({ "content": [{ "type": "text", "text": text }], "isError": true })
    }

    fn status_and_text(response: HttpResponse) -> (u16, String) {
        (response.status().as_u16(), response.text().unwrap())
    }

    // ── Handshake ───────────────────────────────────────────────────────────

    #[test]
    fn initialize는_서버_정보와_세션_id를_돌려준다() {
        let fixture = serve();
        let mut client = Client::new(&fixture.server, None);
        let reply = client.initialize();
        assert_eq!(
            reply,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": { "tools": { "listChanged": true } },
                    "serverInfo": { "name": "rever-traffic", "version": "9.8.7" }
                }
            })
        );
        // A session id in the SDK's shape: a random UUID.
        let sid = client.sid.clone().unwrap();
        assert_eq!(sid.len(), 36);
        assert_eq!(sid.matches('-').count(), 4);
    }

    #[test]
    fn 클라이언트가_요청한_프로토콜_버전에_맞춘다() {
        let fixture = serve();
        for (asked, answered) in [
            // What scripts/mcp-call.py asks for.
            ("2024-11-05", "2024-11-05"),
            ("2025-11-25", "2025-11-25"),
            // The sessionless revision is not offered: the newest with a handshake answers.
            ("2026-07-28", "2025-11-25"),
            ("1999-01-01", "2025-11-25"),
        ] {
            let mut client = Client::new(&fixture.server, None);
            let reply = client.rpc(
                "initialize",
                json!({
                    "protocolVersion": asked,
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "1" }
                }),
            );
            assert_eq!(reply["result"]["protocolVersion"], answered, "{asked}");
        }
    }

    #[test]
    fn 주소와_인증_헤더를_돌려준다() {
        let fixture = serve();
        let server = &fixture.server;
        let port: u16 = server
            .url
            .strip_prefix("http://127.0.0.1:")
            .and_then(|rest| rest.strip_suffix("/mcp"))
            .and_then(|port| port.parse().ok())
            .unwrap();
        assert_ne!(port, 0);
        // randomBytes(24) as hex.
        assert_eq!(server.token.len(), 48);
        assert!(
            server
                .token
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_eq!(server.auth_header, format!("Bearer {}", server.token));
    }

    // ── Catalog modes and the tool list ─────────────────────────────────────

    #[test]
    fn full_카탈로그는_등록한_도구를_모두_등록_순서로_보여준다() {
        let fixture = serve();
        let mut client = Client::connect(&fixture.server, Some("full"));
        let mut expected: Vec<&str> = REGISTERED.to_vec();
        // This crate's paging tool comes after the registry's; the profile
        // switch is not registered for a full session at all.
        expected.push("tool_result_more");
        assert_eq!(client.tools(), expected);

        let reply = client.rpc("tools/list", json!({}));
        let tools = reply["result"]["tools"].as_array().unwrap();
        assert_eq!(
            tools[0],
            json!({
                "name": "list_requests",
                "description": "echoes its arguments",
                "inputSchema": { "type": "object", "properties": { "q": { "type": "string" } } }
            })
        );
        // A tool without an object schema is listed with the empty one.
        assert_eq!(tools[6]["name"], "get_request");
        assert_eq!(
            tools[6]["inputSchema"],
            json!({ "type": "object", "properties": {} })
        );
        assert_eq!(tools[8]["inputSchema"], ToolResultMore.def().input_schema);
        // Only follow sessions read the profile, so a full session leaves no
        // tool-profile.json behind.
        assert!(!fixture.dir.0.join("tool-profile.json").exists());
    }

    #[test]
    fn 헤더가_없으면_현재_프로파일이_광고하는_도구만_보여준다() {
        let fixture = serve();
        let mut client = Client::connect(&fixture.server, None);
        // browse: no reverse-only, probe-only, hidden or unknown tool.
        assert_eq!(
            client.tools(),
            [
                "list_requests",
                "browser_snapshot",
                "get_request",
                "tool_result_more",
                "tool_profile_set"
            ]
        );
        assert_eq!(
            std::fs::read_to_string(fixture.dir.0.join("tool-profile.json")).unwrap(),
            "{\n  \"version\": 1,\n  \"profile\": \"browse\"\n}"
        );
    }

    #[test]
    fn 저장된_프로파일로_follow_세션을_시작한다() {
        let dir = TempDir::new("server");
        std::fs::write(
            dir.0.join("tool-profile.json"),
            r#"{"version":1,"profile":"reverse"}"#,
        )
        .unwrap();
        let context = ToolContext::new(Arc::default(), Arc::new(NoBrowser), dir.0.clone());
        let server = start(registry(), Arc::new(context)).unwrap();
        let mut client = Client::connect(&server, None);
        assert_eq!(
            client.tools(),
            [
                "list_requests",
                "browser_snapshot",
                "grep_scripts",
                "get_request",
                "tool_result_more",
                "tool_profile_set"
            ]
        );
    }

    #[test]
    fn direct_카탈로그는_모든_도구와_프로파일_전환_도구를_보여준다() {
        let fixture = serve();
        let mut client = Client::connect(&fixture.server, Some("direct"));
        let mut expected: Vec<&str> = REGISTERED.to_vec();
        expected.extend(["tool_result_more", "tool_profile_set"]);
        assert_eq!(client.tools(), expected);
    }

    #[test]
    fn 카탈로그_헤더는_정확히_full이나_direct일_때만_인정한다() {
        let fixture = serve();
        for header in ["Full", "fully", "all", ""] {
            let mut client = Client::connect(&fixture.server, Some(header));
            assert_eq!(client.tools().len(), 5, "{header:?}");
        }

        // A repeated header reaches Node joined, so it is neither value.
        let mut headers = HeaderMap::new();
        assert_eq!(catalog_of(&headers), CatalogKind::Follow);
        headers.append("x-rever-catalog", HeaderValue::from_static("full"));
        assert_eq!(catalog_of(&headers), CatalogKind::Full);
        headers.append("x-rever-catalog", HeaderValue::from_static("full"));
        assert_eq!(catalog_of(&headers), CatalogKind::Follow);
        assert_eq!(
            node_header(&headers, "x-rever-catalog").as_deref(),
            Some("full, full")
        );
    }

    #[test]
    fn 카탈로그는_initialize_때_정해지고_뒤의_헤더로_바뀌지_않는다() {
        let fixture = serve();
        let mut client = Client::connect(&fixture.server, None);
        client.catalog = Some("full");
        assert_eq!(client.tools().len(), 5);
        assert_eq!(
            client.call("grep_scripts", json!({})),
            error_result("MCP error -32602: Tool grep_scripts disabled")
        );
    }

    // ── Tool calls ──────────────────────────────────────────────────────────

    #[test]
    fn 도구를_호출하면_결과를_is_error_키_없이_돌려준다() {
        let fixture = serve();
        let mut client = Client::connect(&fixture.server, Some("full"));
        assert_eq!(
            client.call("list_requests", json!({ "q": "가", "n": 2 })),
            text_result("list_requests:{\"q\":\"가\",\"n\":2}")
        );
    }

    #[test]
    fn 인자를_생략한_호출은_도구에_null을_넘긴다() {
        let fixture = serve();
        let mut client = Client::connect(&fixture.server, Some("full"));
        let reply = client.rpc("tools/call", json!({ "name": "list_requests" }));
        assert_eq!(reply["result"], text_result("list_requests:null"));
    }

    #[test]
    fn 없는_도구와_꺼진_도구는_결과_안에서_거절한다() {
        let fixture = serve();
        let mut follow = Client::connect(&fixture.server, None);
        assert_eq!(
            follow.call("nope", json!({})),
            error_result("MCP error -32602: Tool nope not found")
        );
        for disabled in ["grep_scripts", "intruder_run", "bp_add", "site_special"] {
            assert_eq!(
                follow.call(disabled, json!({})),
                error_result(&format!("MCP error -32602: Tool {disabled} disabled"))
            );
        }
        // A core tool still runs in the same session.
        assert_eq!(
            follow.call("list_requests", json!({})),
            text_result("list_requests:{}")
        );

        // A full session never registered the profile switch.
        let mut full = Client::connect(&fixture.server, Some("full"));
        assert_eq!(
            full.call("tool_profile_set", json!({})),
            error_result("MCP error -32602: Tool tool_profile_set not found")
        );
    }

    #[test]
    fn 패닉한_도구는_오류_결과가_되고_세션은_계속_쓴다() {
        let page = Arc::new(Page::default());
        let fixture = serve_with(page.clone());
        let mut client = Client::connect(&fixture.server, Some("full"));
        assert_eq!(client.call("boom", json!({})), error_result("tool blew up"));
        assert_eq!(
            page.events(),
            [
                "start boom | -",
                "busy true",
                "busy false",
                "finish failed=true"
            ]
        );
        assert_eq!(
            client.call("list_requests", json!({})),
            text_result("list_requests:{}")
        );
    }

    #[test]
    fn 잘린_결과는_tool_result_more로_이어_읽는다() {
        let fixture = serve();
        // The result is parked once per server, so another session can page it.
        let mut first = Client::connect(&fixture.server, None);
        let head = first.call("get_request", json!({}));
        let head = head["content"][0]["text"].as_str().unwrap();
        assert!(head.starts_with("0123456789"));
        assert!(head.ends_with(
            "\n\n[truncated: showing 12000 of 30000 chars. Call tool_result_more with resultId=\"res_1\", offset=12000 for the rest.]"
        ));

        let mut second = Client::connect(&fixture.server, Some("full"));
        let next = second.call(
            "tool_result_more",
            json!({ "resultId": "res_1", "offset": 12_000 }),
        );
        assert_eq!(
            next,
            text_result(&format!(
                "{}\n\n[more: call tool_result_more resultId=\"res_1\", offset=24000 (6000 chars left)]",
                "0123456789".repeat(1_200)
            ))
        );
        let last = second.call(
            "tool_result_more",
            json!({ "resultId": "res_1", "offset": 24_000, "limit": 60_000 }),
        );
        assert_eq!(
            last,
            text_result(&format!(
                "{}\n\n[end of result: 30000 chars total]",
                "0123456789".repeat(600)
            ))
        );
        let refused = second.call("tool_result_more", json!({ "resultId": "res_1" }));
        assert_eq!(refused["isError"], true);
        assert!(
            refused["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("MCP error -32602: Input validation error: Invalid arguments for tool tool_result_more: [")
        );
    }

    #[test]
    fn 핸들러가_없는_메서드는_method_not_found로_거절한다() {
        let fixture = serve();
        let mut client = Client::connect(&fixture.server, Some("full"));
        // What the TypeScript server answered to each of these.
        let refused = json!({ "code": -32601, "message": "Method not found" });
        let complete = json!({
            "ref": { "type": "ref/prompt", "name": "p" },
            "argument": { "name": "a", "value": "b" }
        });
        for (method, params) in [
            ("resources/list", json!({})),
            ("resources/templates/list", json!({})),
            ("prompts/list", json!({})),
            ("completion/complete", complete),
            ("foo/bar", json!({})),
        ] {
            let reply = client.rpc(method, params);
            assert_eq!(reply["error"], refused, "{method}: {reply}");
        }
        assert_eq!(client.rpc("ping", json!({}))["result"], json!({}));
    }

    #[test]
    fn 자바스크립트가_받던_깨진_문자는_대체_문자로_바꿔_받는다() {
        let fixed = |raw: &[u8]| String::from_utf8(js_tolerant_json(raw.to_vec())).unwrap();
        assert_eq!(fixed(b""), "");
        assert_eq!(
            fixed(br#"{"q":"plain \n \u00e9"}"#),
            r#"{"q":"plain \n \u00e9"}"#
        );
        // Invalid UTF-8, as Buffer.toString('utf8') decodes it.
        assert_eq!(fixed(b"{\"q\":\"a\xffb\"}"), "{\"q\":\"a\u{fffd}b\"}");
        // A whole pair is one character and stays.
        assert_eq!(fixed(br#""\ud83d\ude00""#), r#""\ud83d\ude00""#);
        assert_eq!(fixed(br#""\uD83D\uDE00""#), r#""\uD83D\uDE00""#);
        // Lone halves: high, low, high before a non-surrogate, two highs, high at the end.
        assert_eq!(fixed(br#""a\ud800b""#), r#""a\ufffdb""#);
        assert_eq!(fixed(br#""\uDE00""#), r#""\ufffd""#);
        assert_eq!(fixed(br#""\ud800\u0041""#), r#""\ufffd\u0041""#);
        assert_eq!(fixed(br#""\ud800\ud83d\ude00""#), r#""\ufffd\ud83d\ude00""#);
        assert_eq!(fixed(br#""\ud800"#), r#""\ufffd"#);
        // An escaped backslash before "ud800" is text, not an escape.
        assert_eq!(fixed(br#""\\ud800""#), r#""\\ud800""#);
        assert_eq!(fixed(br#""\\\ud800""#), r#""\\\ufffd""#);
        // Cut short or not hex: left for the parser to refuse.
        assert_eq!(fixed(br#""\ud80""#), r#""\ud80""#);
        assert_eq!(fixed(br#""\ud8zz""#), r#""\ud8zz""#);
        assert_eq!(fixed(b"\\"), "\\");

        // Over HTTP: the TypeScript server ran the tool for both of these.
        let fixture = serve();
        let client = Client::connect(&fixture.server, Some("full"));
        let call = |arguments: &[u8]| {
            let mut body = br#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"list_requests","arguments":"#.to_vec();
            body.extend_from_slice(arguments);
            body.extend_from_slice(b"}}");
            let response = client
                .request(reqwest::Method::POST)
                .header("Content-Type", "application/json")
                .body(body)
                .send()
                .unwrap();
            assert_eq!(response.status(), 200);
            message_of(&response.text().unwrap())["result"].clone()
        };
        assert_eq!(
            call(br#"{"q":"a\ud800b"}"#),
            text_result("list_requests:{\"q\":\"a\u{fffd}b\"}")
        );
        assert_eq!(
            call(b"{\"q\":\"a\xffb\"}"),
            text_result("list_requests:{\"q\":\"a\u{fffd}b\"}")
        );
    }

    // ── Tool profile over HTTP ──────────────────────────────────────────────

    #[test]
    fn 프로파일을_바꾸면_모든_follow_세션의_목록이_바뀐다() {
        let fixture = serve();
        let mut agent = Client::connect(&fixture.server, None);
        let mut other = Client::connect(&fixture.server, None);
        let mut full = Client::connect(&fixture.server, Some("full"));
        let mut direct = Client::connect(&fixture.server, Some("direct"));
        assert!(!agent.tools().contains(&"grep_scripts".to_owned()));

        let read = agent.call("tool_profile_set", json!({}));
        assert!(
            read["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("profile=browse advertised=")
        );

        let switched = agent.call("tool_profile_set", json!({ "profile": "reverse" }));
        assert!(
            switched["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("profile=reverse advertised=")
        );
        assert!(switched.get("isError").is_none());

        let reverse = [
            "list_requests",
            "browser_snapshot",
            "grep_scripts",
            "get_request",
            "tool_result_more",
            "tool_profile_set",
        ];
        assert_eq!(agent.tools(), reverse);
        assert_eq!(other.tools(), reverse);
        assert_eq!(
            other.call("grep_scripts", json!({})),
            text_result("grep_scripts:{}")
        );
        // full and direct were complete before and stay so.
        assert_eq!(full.tools().len(), 9);
        assert_eq!(direct.tools().len(), 10);

        assert_eq!(fixture.server.tool_profile().get(), ToolProfile::Reverse);
        assert_eq!(
            std::fs::read_to_string(fixture.dir.0.join("tool-profile.json")).unwrap(),
            "{\n  \"version\": 1,\n  \"profile\": \"reverse\"\n}"
        );

        // A direct session carries the switch too, and it moves the same profile.
        direct.call("tool_profile_set", json!({ "profile": "browse" }));
        assert!(!agent.tools().contains(&"grep_scripts".to_owned()));
    }

    #[test]
    fn 에이전트는_probe를_못_고르지만_사용자가_고르면_follow_세션에_반영된다() {
        let fixture = serve();
        let mut agent = Client::connect(&fixture.server, None);
        assert_eq!(
            agent.call("tool_profile_set", json!({ "profile": "probe" })),
            error_result(
                "probe is user-selected. Ask the user to set Tools → Access. Profile was not changed."
            )
        );
        assert!(!agent.tools().contains(&"intruder_run".to_owned()));

        // The app's Tools → Access menu.
        fixture
            .server
            .tool_profile()
            .set(ToolProfile::Probe, ProfileSource::User)
            .unwrap();
        assert!(agent.tools().contains(&"intruder_run".to_owned()));
        assert!(!agent.tools().contains(&"grep_scripts".to_owned()));
    }

    #[test]
    fn 프로파일이_바뀌면_follow_세션에_list_changed를_보낸다() {
        let fixture = serve();
        let mut agent = Client::connect(&fixture.server, None);

        // The session's standalone event stream, where notifications arrive.
        let stream = Http::builder()
            .timeout(None)
            .build()
            .unwrap()
            .get(&agent.url)
            .header("Authorization", &agent.auth)
            .header("Accept", "text/event-stream")
            .header("mcp-session-id", agent.sid.as_deref().unwrap())
            .send()
            .unwrap();
        assert_eq!(stream.status(), 200);
        let (lines, received) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                if lines.send(line).is_err() {
                    break;
                }
            }
        });
        let next_data = |wait: Duration| -> Option<String> {
            let deadline = std::time::Instant::now() + wait;
            loop {
                let left = deadline.checked_duration_since(std::time::Instant::now())?;
                let line = received.recv_timeout(left).ok()?;
                if let Some(data) = line.strip_prefix("data: ") {
                    return Some(data.to_owned());
                }
            }
        };

        // Same profile: no bit flips, so nothing is sent.
        fixture
            .server
            .tool_profile()
            .set(ToolProfile::Browse, ProfileSource::User)
            .unwrap();
        // From the app's own thread, outside the server's runtime.
        fixture
            .server
            .tool_profile()
            .set(ToolProfile::Reverse, ProfileSource::User)
            .unwrap();
        let first = next_data(Duration::from_secs(10)).expect("a list_changed notification");
        assert_eq!(
            serde_json::from_str::<Value>(&first).unwrap(),
            json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" })
        );
        // One notification for the one change, not one per toggled tool.
        assert_eq!(next_data(Duration::from_millis(300)), None);

        agent.call("tool_profile_set", json!({ "profile": "browse" }));
        assert!(next_data(Duration::from_secs(10)).is_some());
    }

    #[test]
    fn 끝난_세션은_프로파일을_따르지_않는다() {
        let fixture = serve();
        let mut gone = Client::connect(&fixture.server, None);
        let _full = Client::connect(&fixture.server, Some("full"));
        let _direct = Client::connect(&fixture.server, Some("direct"));
        // Only the follow session is registered with the profile.
        let profile = fixture.server.tool_profile();
        assert_eq!(profile.follow_server_count(), 1);

        let closed = gone.request(reqwest::Method::DELETE).send().unwrap();
        assert!(closed.status().is_success());
        // The handler is dropped with the session, a moment after the reply.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while profile.follow_server_count() > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(profile.follow_server_count(), 0);

        // Changing the profile with no follow session left must not fail.
        let change = fixture
            .server
            .tool_profile()
            .set(ToolProfile::Reverse, ProfileSource::User);
        assert!(change.is_ok());

        let (status, text) = status_and_text(
            gone.post(&json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/list" })),
        );
        assert_eq!((status, text.as_str()), (400, "missing or invalid session"));
    }

    // ── Page notes and activity ─────────────────────────────────────────────

    #[test]
    fn 브라우저가_아닌_도구만_활동을_알린다() {
        let page = Arc::new(Page::default());
        let fixture = serve_with(page.clone());
        let mut client = Client::connect(&fixture.server, Some("full"));

        // `limit` and `third` are not in the tool's schema: zod dropped them
        // before the handler, so they are not in the summary either.
        client.call(
            "list_requests",
            json!({ "limit": 5, "q": "login", "skip": null, "third": true }),
        );
        assert_eq!(
            page.events(),
            [
                "start list requests | q: login",
                "busy true",
                "busy false",
                "history 100",
                "finish failed=false"
            ]
        );

        lock(&page.events).clear();
        client.call("browser_snapshot", json!({}));
        assert_eq!(page.events(), ["busy true", "busy false", "history 100"]);

        // This crate's own tools go through the same wrapper.
        lock(&page.events).clear();
        client.call(
            "tool_result_more",
            json!({ "offset": 0, "resultId": "res_9" }),
        );
        assert_eq!(
            page.events().first().map(String::as_str),
            Some("start tool result more | resultId: res_9 · offset: 0")
        );
    }

    #[test]
    fn 열린_대화상자가_있으면_도구를_실행하지_않고_알린다() {
        let page = Arc::new(Page::default());
        *lock(&page.open) = Some(dialog("confirm", "Delete \"a\"?", None));
        let fixture = serve_with(page.clone());
        let mut client = Client::connect(&fixture.server, Some("full"));
        assert_eq!(
            client.call("list_requests", json!({})),
            error_result(
                "A page confirm() is open on screen and waits for the user: \"Delete \"a\"?\". Ask the user to answer it, then retry."
            )
        );
        // The tool never ran, and the activity it announced is left open, as in the TypeScript.
        assert_eq!(page.events(), ["start list requests | -"]);

        *lock(&page.open) = None;
        assert_eq!(
            client.call("list_requests", json!({})),
            text_result("list_requests:{}")
        );
    }

    #[test]
    fn 호출_중에_답한_대화상자와_시작된_다운로드를_결과에_덧붙인다() {
        let page = Arc::new(Page::default());
        *lock(&page.history) = vec![
            // Answered long before this call: not mentioned.
            dialog("alert", "old", Some((true, 0))),
            dialog("alert", "saved", Some((true, i64::MAX))),
            // Still open: not mentioned.
            dialog("prompt", "name?", None),
            dialog("confirm", "leave?", Some((false, i64::MAX))),
        ];
        *lock(&page.downloads) = vec!["report.csv".to_owned()];
        let fixture = serve_with(page.clone());
        let mut client = Client::connect(&fixture.server, Some("full"));
        assert_eq!(
            client.call("browser_snapshot", json!({})),
            json!({
                "content": [
                    { "type": "text", "text": "browser_snapshot:{}" },
                    {
                        "type": "text",
                        "text": "[page alert] \"saved\" → auto-answered OK\n[page confirm] \"leave?\" → auto-answered Cancel\n[download started] \"report.csv\" → call wait_for_download for the saved file"
                    }
                ]
            })
        );
        // `since` is the moment the call started, in epoch milliseconds.
        let asked = lock(&page.asked_since).clone();
        assert_eq!(asked.len(), 1);
        assert!((now_ms() - asked[0]).abs() < 60_000);
        // Refusals made before the tool runs carry no notes.
        assert_eq!(
            client.call("nope", json!({})),
            error_result("MCP error -32602: Tool nope not found")
        );
    }

    #[test]
    fn 인자를_거절한_결과에는_아무것도_덧붙이지_않는다() {
        let page = Arc::new(Page::default());
        *lock(&page.downloads) = vec!["report.csv".to_owned()];
        let fixture = serve_with(page.clone());
        let mut client = Client::connect(&fixture.server, Some("full"));
        // Refused for its arguments: in the TypeScript the handler, and with it
        // the notes, was never reached.
        let refused = client.call("tool_result_more", json!({}));
        assert_eq!(refused["isError"], true);
        assert_eq!(refused["content"].as_array().map(Vec::len), Some(1));
        assert!(lock(&page.asked_since).is_empty());

        // Any other error is the handler's own, and carries the notes.
        let unknown = client.call("tool_result_more", json!({ "resultId": "x", "offset": 0 }));
        assert_eq!(unknown["isError"], true);
        assert_eq!(
            unknown["content"][1]["text"],
            "[download started] \"report.csv\" → call wait_for_download for the saved file"
        );

        // Only the tool's own refusal counts, worded for that tool.
        let worded = |tool: &str| {
            err(format!(
                "MCP error -32602: Input validation error: Invalid arguments for tool {tool}: []"
            ))
        };
        assert!(refused_arguments(&worded("intruder_run"), "intruder_run"));
        assert!(!refused_arguments(&worded("intruder"), "intruder_run"));
        assert!(!refused_arguments(
            &err("MCP error -32602: other"),
            "intruder_run"
        ));
        let mut passed = worded("intruder_run");
        passed.is_error = false;
        assert!(!refused_arguments(&passed, "intruder_run"));
        passed.content.clear();
        passed.is_error = true;
        assert!(!refused_arguments(&passed, "intruder_run"));
    }

    #[test]
    fn scope가_꺼진_공격_도구_결과에는_경고를_덧붙인다() {
        // No test in this crate sets a scope, so the process-wide scope is off.
        let fixture = serve();
        let mut client = Client::connect(&fixture.server, Some("full"));
        assert_eq!(
            client.call("intruder_run", json!({})),
            json!({
                "content": [
                    { "type": "text", "text": "intruder_run:{}" },
                    {
                        "type": "text",
                        "text": "[scope off] intruder_run ran with no target scope — any host is allowed. Pin the authorized target with scope_set before further probing."
                    }
                ]
            })
        );
        // Not an attack tool: nothing is added.
        assert_eq!(
            client.call("grep_scripts", json!({})),
            text_result("grep_scripts:{}")
        );
    }

    #[test]
    fn 덧붙일_것이_없으면_결과를_그대로_둔다() {
        let page = Page::default();
        let res = with_page_notes(ok("done"), 0, "list_requests", &page);
        assert_eq!(res, ok("done"));

        // An error result gets its notes too.
        *lock(&page.downloads) = vec!["a\"b.zip".to_owned()];
        let res = with_page_notes(err("failed"), now_ms(), "list_requests", &page);
        assert!(res.is_error);
        assert_eq!(
            res.content,
            [
                Content::Text {
                    text: "failed".to_owned()
                },
                Content::Text {
                    text: "[download started] \"a\"b.zip\" → call wait_for_download for the saved file"
                        .to_owned()
                }
            ]
        );
    }

    #[test]
    fn 대화상자는_since_이후에_답한_것만_적는다() {
        let page = Page::default();
        *lock(&page.history) = vec![
            dialog("alert", "before", Some((true, 999))),
            dialog("beforeunload", "at", Some((true, 1_000))),
            dialog("alert", "after", Some((false, 1_001))),
        ];
        let res = with_page_notes(ok("x"), 1_000, "list_requests", &page);
        assert_eq!(
            res.content.get(1),
            Some(&Content::Text {
                text: "[page beforeunload] \"at\" → auto-answered OK\n[page alert] \"after\" → auto-answered Cancel"
                    .to_owned()
            })
        );
    }

    #[test]
    fn 활동_요약은_앞의_두_인자를_40자까지_적는다() {
        // No schema to order by: the arguments as sent.
        let summarize_tool_args = |input: &Value| summarize_tool_args(&Value::Null, input);
        assert_eq!(summarize_tool_args(&Value::Null), None);
        assert_eq!(summarize_tool_args(&json!("text")), None);
        assert_eq!(summarize_tool_args(&json!({})), None);
        // null, arrays and objects are skipped, not counted.
        assert_eq!(
            summarize_tool_args(&json!({ "a": null, "b": [1], "c": { "d": 1 } })),
            None
        );
        assert_eq!(
            summarize_tool_args(&json!({ "url": "https://a.test", "full": true, "n": 3 })),
            Some("url: https://a.test · full: true".to_owned())
        );
        assert_eq!(
            summarize_tool_args(&json!({ "skip": [], "id": 12, "ratio": 0.5 })),
            Some("id: 12 · ratio: 0.5".to_owned())
        );
        // An empty string still counts as a part.
        assert_eq!(
            summarize_tool_args(&json!({ "q": "" })),
            Some("q: ".to_owned())
        );

        let long = "a".repeat(41);
        assert_eq!(
            summarize_tool_args(&json!({ "q": long })),
            Some(format!("q: {}", "a".repeat(40)))
        );
        // 40 UTF-16 units: 20 emoji fit, and a 21st cut in half is dropped whole.
        let emoji = "😀".repeat(21);
        assert_eq!(
            summarize_tool_args(&json!({ "q": emoji })),
            Some(format!("q: {}", "😀".repeat(20)))
        );
        let odd = format!("a{}", "😀".repeat(20));
        assert_eq!(
            summarize_tool_args(&json!({ "q": odd })),
            Some(format!("q: a{}", "😀".repeat(19)))
        );
    }

    #[test]
    fn 활동_요약은_스키마_순서로_적고_기본값을_채우고_모르는_키는_뺀다() {
        // registerTool('echo', { inputSchema: { q: z.string().optional(),
        // n: z.number().default(7), z: z.boolean().optional() } }) as the SDK lists it.
        let schema = json!({
            "type": "object",
            "properties": {
                "q": { "type": "string" },
                "n": { "default": 7, "type": "number" },
                "z": { "type": "boolean" }
            }
        });
        // The handler received {"q":"x","n":7,"z":true} for these arguments (bun).
        assert_eq!(
            summarize_tool_args(&schema, &json!({ "z": true, "extra": 1, "q": "x" })),
            Some("q: x · n: 7".to_owned())
        );
        assert_eq!(
            summarize_tool_args(&schema, &json!({ "z": false })),
            Some("n: 7 · z: false".to_owned())
        );
        assert_eq!(
            summarize_tool_args(&schema, &json!({ "n": 1, "extra": "kept out" })),
            Some("n: 1".to_owned())
        );
        assert_eq!(summarize_tool_args(&schema, &json!([1])), None);
        // A schema of no keys parses every call to {}.
        let empty = json!({ "type": "object", "properties": {} });
        assert_eq!(summarize_tool_args(&empty, &json!({ "q": "x" })), None);
        // A schema without a key list leaves the arguments as sent.
        assert_eq!(
            summarize_tool_args(&json!({ "type": "object" }), &json!({ "b": 1, "a": 2 })),
            Some("b: 1 · a: 2".to_owned())
        );
    }

    #[test]
    fn 활동_요약의_숫자는_자바스크립트처럼_적는다() {
        let args: Value =
            serde_json::from_str(r#"{"a":1.0,"b":1e21,"c":-0.0,"d":1e-7,"e":123456789012}"#)
                .unwrap();
        let Value::Object(args) = args else {
            panic!("not an object")
        };
        let text = |key: &str| {
            summarize_tool_args(&Value::Null, &json!({ key: args[key] }))
                .unwrap()
                .split_once(": ")
                .unwrap()
                .1
                .to_owned()
        };
        // What String(n) prints in node.
        assert_eq!(text("a"), "1");
        assert_eq!(text("b"), "1e+21");
        assert_eq!(text("c"), "0");
        assert_eq!(text("d"), "1e-7");
        assert_eq!(text("e"), "123456789012");
    }

    // ── The HTTP gate ───────────────────────────────────────────────────────

    #[test]
    fn 토큰이_없거나_틀리면_401로_거절한다() {
        let fixture = serve();
        let http = Http::new();
        let post = || {
            http.post(&fixture.server.url)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .body(INITIALIZE)
        };
        let wrong = format!("Bearer {}", "0".repeat(48));
        let lowercase = format!("bearer {}", fixture.server.token);
        let longer = format!("{}0", fixture.server.auth_header);
        let attempts = [
            post(),
            post().header("Authorization", wrong),
            post().header("Authorization", &fixture.server.token),
            post().header("Authorization", lowercase),
            post().header("Authorization", longer),
            http.get(&fixture.server.url),
        ];
        for attempt in attempts {
            let response = attempt.send().unwrap();
            assert_eq!(response.status(), 401);
            assert_eq!(
                response
                    .headers()
                    .get("WWW-Authenticate")
                    .map(|v| v.to_str().unwrap()),
                Some("Bearer")
            );
            assert_eq!(
                response.text().unwrap(),
                "unauthorized: missing or invalid bearer token"
            );
        }

        let accepted = post()
            .header("Authorization", &fixture.server.auth_header)
            .send()
            .unwrap();
        assert_eq!(accepted.status(), 200);
    }

    #[test]
    fn 토큰_비교는_전체가_같을_때만_통과한다() {
        assert!(is_authorized(Some(b"Bearer abc"), "abc"));
        assert!(!is_authorized(None, "abc"));
        assert!(!is_authorized(Some(b""), "abc"));
        assert!(!is_authorized(Some(b"Bearer abd"), "abc"));
        assert!(!is_authorized(Some(b"Bearer ab"), "abc"));
        assert!(!is_authorized(Some(b"Bearer abcd"), "abc"));
        assert!(!is_authorized(Some(b"Basic  abc"), "abc"));
        // An empty token would still need the scheme and the space.
        assert!(!is_authorized(Some(b"Bearer"), ""));
        assert!(is_authorized(Some(b"Bearer "), ""));
    }

    #[test]
    fn mcp가_아닌_경로는_토큰보다_먼저_404로_거절한다() {
        let fixture = serve();
        let http = Http::new();
        let base = fixture.server.url.trim_end_matches("/mcp").to_owned();
        for path in ["/", "/mcp/", "/mcp?x=1", "/MCP", "/other"] {
            for auth in [None, Some(&fixture.server.auth_header)] {
                let mut request = http.post(format!("{base}{path}")).body(INITIALIZE);
                if let Some(auth) = auth {
                    request = request.header("Authorization", auth);
                }
                let (status, text) = status_and_text(request.send().unwrap());
                assert_eq!((status, text.as_str()), (404, "not found"), "{path}");
            }
        }
    }

    #[test]
    fn post의_origin은_같은_포트의_로컬_주소만_받는다() {
        let fixture = serve();
        let port = fixture
            .server
            .url
            .trim_start_matches("http://127.0.0.1:")
            .trim_end_matches("/mcp")
            .to_owned();
        let send = |origin: &str| {
            let client = Client::new(&fixture.server, None);
            client
                .request(reqwest::Method::POST)
                .header("Content-Type", "application/json")
                .header("Origin", origin)
                .body(INITIALIZE)
                .send()
                .unwrap()
        };
        for origin in [
            "https://evil.example",
            "http://localhost",
            "http://127.0.0.1:1",
            "null",
            "not a url",
        ] {
            let (status, text) = status_and_text(send(origin));
            assert_eq!(
                (status, text.as_str()),
                (403, "forbidden: untrusted origin"),
                "{origin}"
            );
        }
        for origin in [
            format!("http://localhost:{port}"),
            format!("http://127.0.0.1:{port}"),
        ] {
            assert_eq!(send(&origin).status(), 200, "{origin}");
        }

        // Only POST is checked: the event stream of a session opens under any Origin.
        let client = Client::connect(&fixture.server, None);
        let stream = client
            .request(reqwest::Method::GET)
            .header("Origin", "https://evil.example")
            .send()
            .unwrap();
        assert_eq!(stream.status(), 200);
    }

    #[test]
    fn origin_검사는_url의_호스트와_포트를_본다() {
        assert!(is_origin_allowed(None, 5000));
        assert!(is_origin_allowed(Some(""), 5000));
        assert!(is_origin_allowed(Some("http://localhost:5000"), 5000));
        assert!(is_origin_allowed(Some("http://127.0.0.1:5000"), 5000));
        // The scheme is not checked, only host and port.
        assert!(is_origin_allowed(Some("https://localhost:5000"), 5000));
        assert!(is_origin_allowed(Some("HTTP://LOCALHOST:5000/x"), 5000));
        // No port in the origin: the scheme's default stands in.
        assert!(is_origin_allowed(Some("http://localhost"), 80));
        assert!(is_origin_allowed(Some("https://127.0.0.1"), 443));
        assert!(!is_origin_allowed(Some("https://localhost"), 80));
        // An explicit default port is dropped by URL parsing, as in JavaScript.
        assert!(is_origin_allowed(Some("http://localhost:80"), 80));
        // "HTTPS…" does not start with "https", so 80 is assumed: the TypeScript quirk.
        assert!(is_origin_allowed(Some("HTTPS://localhost"), 80));

        assert!(!is_origin_allowed(Some("http://localhost:5001"), 5000));
        assert!(!is_origin_allowed(
            Some("http://localhost.evil.example:5000"),
            5000
        ));
        assert!(!is_origin_allowed(Some("http://[::1]:5000"), 5000));
        assert!(!is_origin_allowed(Some("http://0.0.0.0:5000"), 5000));
        assert!(!is_origin_allowed(Some("localhost:5000"), 5000));
        assert!(!is_origin_allowed(Some("null"), 5000));
        assert!(!is_origin_allowed(Some("http://"), 5000));
    }

    #[test]
    fn host_헤더가_서버_주소가_아니면_거절한다() {
        let fixture = serve();
        let client = Client::new(&fixture.server, None);
        let response = client
            .request(reqwest::Method::POST)
            .header("Content-Type", "application/json")
            .header("Host", "evil.example")
            .body(INITIALIZE)
            .send()
            .unwrap();
        assert_eq!(response.status(), 403);
    }

    #[test]
    fn 세션_없는_요청은_400으로_거절한다() {
        let fixture = serve();
        let client = Client::new(&fixture.server, None);
        let post = |body: &str| {
            client
                .request(reqwest::Method::POST)
                .header("Content-Type", "application/json")
                .body(body.to_owned())
        };
        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

        let cases = [
            (post(list), 400, "missing or invalid session"),
            (
                post(list).header("mcp-session-id", "no-such-session"),
                400,
                "missing or invalid session",
            ),
            // No body at all is not an initialize request.
            (post(""), 400, "missing or invalid session"),
            // A batch is not one either.
            (
                post(&format!("[{INITIALIZE}]")),
                400,
                "missing or invalid session",
            ),
            // initialize without clientInfo.
            (
                post(
                    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}}"#,
                ),
                400,
                "missing or invalid session",
            ),
            (post("{not json"), 400, "bad request"),
            (client.request(reqwest::Method::GET), 400, "no session"),
            (
                client
                    .request(reqwest::Method::GET)
                    .header("mcp-session-id", "no-such-session"),
                400,
                "no session",
            ),
            (client.request(reqwest::Method::DELETE), 400, "no session"),
            (
                client.request(reqwest::Method::PUT).body(INITIALIZE),
                405,
                "method not allowed",
            ),
            (
                client.request(reqwest::Method::PATCH),
                405,
                "method not allowed",
            ),
        ];
        for (request, status, text) in cases {
            let (got_status, got_text) = status_and_text(request.send().unwrap());
            assert_eq!((got_status, got_text.as_str()), (status, text));
        }
    }

    #[test]
    fn initialize_요청인지는_필수_필드로_판단한다() {
        let valid: Value = serde_json::from_str(INITIALIZE).unwrap();
        assert!(is_initialize_request(&valid));
        // jsonrpc and id are not part of the SDK's check.
        assert!(is_initialize_request(&json!({
            "method": "initialize",
            "params": {
                "protocolVersion": "x", "capabilities": {},
                "clientInfo": { "name": "n", "version": "v" }
            }
        })));

        let mut other = valid.clone();
        other["method"] = json!("tools/list");
        assert!(!is_initialize_request(&other));
        for missing in ["protocolVersion", "capabilities", "clientInfo"] {
            let mut body = valid.clone();
            body["params"].as_object_mut().unwrap().remove(missing);
            assert!(!is_initialize_request(&body), "{missing}");
        }
        let mut body = valid.clone();
        body["params"]["protocolVersion"] = json!(20250618);
        assert!(!is_initialize_request(&body));
        let mut body = valid.clone();
        body["params"]["clientInfo"] = json!({ "name": "n" });
        assert!(!is_initialize_request(&body));
        for not_a_request in [json!(null), json!([valid]), json!("initialize"), json!({})] {
            assert!(!is_initialize_request(&not_a_request));
        }
    }

    #[test]
    fn 모르는_세션_id로_initialize하면_새_세션을_연다() {
        let fixture = serve();
        let mut client = Client::new(&fixture.server, Some("full"));
        client.sid = Some("stale-session".to_owned());
        let response = client.post(&serde_json::from_str(INITIALIZE).unwrap());
        assert_eq!(response.status(), 200);
        let sid = response
            .headers()
            .get("mcp-session-id")
            .map(|id| id.to_str().unwrap().to_owned())
            .unwrap();
        assert_ne!(sid, "stale-session");

        client.sid = Some(sid);
        assert_eq!(client.tools().len(), 9);
    }

    #[test]
    fn 세션마다_id가_다르고_delete하면_그_세션만_끝난다() {
        let fixture = serve();
        let mut first = Client::connect(&fixture.server, Some("full"));
        let mut second = Client::connect(&fixture.server, Some("full"));
        assert_ne!(first.sid, second.sid);

        let closed = first.request(reqwest::Method::DELETE).send().unwrap();
        assert!(closed.status().is_success());
        let (status, text) = status_and_text(
            first.post(&json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/list" })),
        );
        assert_eq!((status, text.as_str()), (400, "missing or invalid session"));
        let (status, text) = status_and_text(first.request(reqwest::Method::GET).send().unwrap());
        assert_eq!((status, text.as_str()), (400, "no session"));

        assert_eq!(second.tools().len(), 9);
    }

    #[test]
    fn 본문은_2메가바이트를_넘어도_받는다() {
        let fixture = serve();
        let mut client = Client::connect(&fixture.server, Some("full"));
        let big = "x".repeat(3 * 1024 * 1024);
        let res = client.call("list_requests", json!({ "q": big }));
        let text = res["content"][0]["text"].as_str().unwrap();
        assert_eq!(text.len(), "list_requests:{\"q\":\"\"}".len() + big.len());
    }

    #[test]
    fn 본문은_16메가바이트까지만_읽는다() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let read = |size: usize| runtime.block_on(read_body(Body::from(vec![b'x'; size])));
        assert_eq!(read(0).map(|raw| raw.len()), Ok(0));
        assert_eq!(
            read(READ_BODY_LIMIT).map(|raw| raw.len()),
            Ok(READ_BODY_LIMIT)
        );
        assert_eq!(
            read(READ_BODY_LIMIT + 1),
            Err(StatusCode::PAYLOAD_TOO_LARGE)
        );
        assert_eq!(READ_BODY_LIMIT, 16_777_216);
    }

    // ── Endpoint file and shutdown ──────────────────────────────────────────

    #[test]
    fn 엔드포인트_파일에_주소와_토큰과_pid를_적는다() {
        let fixture = serve();
        let file = endpoint_file_path(&fixture.dir.0);
        assert_eq!(file, fixture.dir.0.join("mcp-endpoint.json"));
        let text = std::fs::read_to_string(&file).unwrap();
        let endpoint: Value = serde_json::from_str(&text).unwrap();
        let started_at = endpoint["startedAt"].as_str().unwrap().to_owned();
        // The exact bytes JSON.stringify(…, null, 2) writes, keys in this order.
        assert_eq!(
            text,
            format!(
                "{{\n  \"url\": \"{}\",\n  \"token\": \"{}\",\n  \"pid\": {},\n  \"startedAt\": \"{started_at}\"\n}}",
                fixture.server.url,
                fixture.server.token,
                std::process::id()
            )
        );
        // 2026-11-13T01:02:03.456Z
        let shape: String = started_at
            .chars()
            .map(|c| if c.is_ascii_digit() { '0' } else { c })
            .collect();
        assert_eq!(shape, "0000-00-00T00:00:00.000Z");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn 이미_있던_엔드포인트_파일도_소유자_전용으로_좁힌다() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("server");
        let file = dir.0.join("mcp-endpoint.json");
        std::fs::write(
            &file,
            "stale and much longer than what replaces it".repeat(20),
        )
        .unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_endpoint_file(&file, "http://127.0.0.1:1/mcp", "t");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let endpoint: Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(endpoint["url"], "http://127.0.0.1:1/mcp");
        assert_eq!(endpoint["token"], "t");
    }

    #[test]
    fn 데이터_폴더에_쓸_수_없어도_서버는_뜬다() {
        let dir = TempDir::new("server");
        let missing = dir.0.join("missing");
        let context = ToolContext::new(Arc::default(), Arc::new(NoBrowser), missing.clone());
        let server = start(registry(), Arc::new(context)).unwrap();
        assert!(!missing.exists());
        let mut client = Client::connect(&server, None);
        assert_eq!(client.tools().len(), 5);
        server.close();
    }

    #[test]
    fn 닫으면_엔드포인트_파일을_지우고_연결을_받지_않는다() {
        let Fixture { dir, server } = serve();
        let mut client = Client::connect(&server, Some("full"));
        assert_eq!(client.tools().len(), 9);
        let file = endpoint_file_path(&dir.0);
        assert!(file.exists());

        server.close();
        assert!(!file.exists());
        let after = Http::new()
            .post(&client.url)
            .header("Authorization", &client.auth)
            .body(INITIALIZE)
            .send();
        assert!(after.is_err());
    }

    #[test]
    fn 서버_둘은_서로_다른_포트와_토큰을_쓴다() {
        let first = serve();
        let second = serve();
        assert_ne!(first.server.url, second.server.url);
        assert_ne!(first.server.token, second.server.token);

        // One server's token does not open the other.
        let crossed = Http::new()
            .post(&first.server.url)
            .header("Authorization", &second.server.auth_header)
            .body(INITIALIZE)
            .send()
            .unwrap();
        assert_eq!(crossed.status(), 401);
    }

    #[test]
    fn 시각은_to_iso_string과_같은_모양으로_적는다() {
        // Expected values are what new Date(ms).toISOString() prints in node.
        assert_eq!(to_iso_string(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(to_iso_string(1_794_533_829_191), "2026-11-13T01:37:09.191Z");
        assert_eq!(to_iso_string(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(to_iso_string(1_709_251_199_999), "2024-02-29T23:59:59.999Z");
        assert_eq!(to_iso_string(4_102_444_800_000), "2100-01-01T00:00:00.000Z");
        assert_eq!(to_iso_string(-1), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn 토큰은_매번_다른_48자리_16진수다() {
        let first = random_token().unwrap();
        let second = random_token().unwrap();
        assert_eq!(first.len(), 48);
        assert!(
            first
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        );
        assert_ne!(first, second);
    }
}
