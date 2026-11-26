//! The window's state: tabs, the page engine, the agent session, and what the panels share.
//! Everything here runs on the main thread; drawing lives in `shell`.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dpi::PhysicalSize;
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures::channel::oneshot;
use gpui::{
    App, AppContext, Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    PathPromptOptions, Pixels, PromptLevel, RenderImage, SharedString, TitlebarOptions, WeakEntity,
    Window, WindowBounds, WindowOptions, px, size,
};
use image::{Frame, RgbaImage};
use net::tap::NetworkTapEvent;
use rever_agent::claude_cli::ClaudeCliFinder;
use rever_agent::model_catalog::ModelCatalog;
use rever_chat_core::acp_permission::{ACP_PERMISSION_TIMEOUT_MS, ApproveOptions, PermissionStore};
use rever_chat_core::red_team_mode::RedTeamMode;
use rever_core::address::normalize_address;
use rever_core::paths::user_data_dir;
use rever_core::traffic::{
    self, ConsoleEntry, Headers, ListFilter, RequestPatch, TrafficStore, WsDirection, WsFrame,
};
use rever_mcp::McpServer;
use rever_tools_api::tool::{
    DialogAnswer, DialogRecord, PageInfo, TabInfo, ToolContext, ToolRegistry,
};
use rever_tools_debug::debugger::DebugState;
use rever_tools_debug::intercept::InterceptState;
use rever_tools_page::downloads::DownloadState;
use rever_tools_redteam::scan::RendererBridge;
use rever_ui_chat::chat_history_menu::{
    ChatHistoryEvent, ChatHistoryMenu, ChatHistoryMenuProps, HistoryRow,
};
use rever_ui_chat::model_catalog::ModelCatalogStore;
use rever_ui_chat::model_picker::{ModelPicker, ModelPickerEvent, ModelPickerProps};
use rever_ui_chat::permission_prompt::{
    PermissionPrompt, PermissionPromptEvent, PermissionPromptProps,
};
use rever_ui_history::history::NewHistoryEntry;
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::text_input::{Submitted, TextInput};
use rever_ui_kit::theme::ThemeMode;
use rever_ui_menus::app_menu::{AppMenu, AppMenuItem, AppMenuProps};
use rever_ui_menus::ip_badge::IpBadge;
use rever_ui_menus::ui::{Look, LookFn, fill};
use rever_ui_setup::browser_import_modal::BrowserImportModal;
use rever_ui_setup::host::SetupHost;
use rever_ui_setup::onboarding_modal::OnboardingModal;
use rever_ui_setup::profile_menu::ProfileMenu;
use rever_ui_setup::profiles::ProfilesStore;
use rever_ui_setup::proxy_button::ProxyButton;
use rever_ui_state::app_theme::{self, AppThemeStore};
use rever_ui_state::chat_collapsed::ChatCollapsedStore;
use rever_ui_state::chat_history::{ChatHistory, new_conversation_id};
use rever_ui_state::constants::AcpAgentId;

use crate::permissions::ChatRules;
use rever_ui_state::spaces::{self as space_rules, SavedSpaces};
use rever_ui_state::tabs::{
    STORE_KEY as TABS_KEY, Tab as SessionTab, parse_saved_tabs, serialize_tabs,
};
use serde_json::{Value, json};
use servo::{
    ColorPicker, ContextMenu, ContextMenuAction, EmbedderControl, JSValue, RgbColor, SelectElement,
    SimpleDialog,
};
use url::Url;

use crate::agent::{AgentEvent, ChatAgent};
use crate::bridge;
use crate::chat_agents::{self, PickerHost};
use crate::engine::{Engine, EngineEvent, TabId};
use crate::panels::{self, IpHost, Stores, Views};

pub(crate) const DEFAULT_URL: &str = "https://github.com/greekr4/rever-browser";
/// What the "+" button opens, as in TabBar.tsx.
const NEW_TAB_URL: &str = "https://www.google.com";

/// The bottom panels, grouped as in FloatingChips.tsx: related panels sit together.
pub(crate) const PANEL_GROUPS: &[&[&str]] = &[
    &["traffic", "apimap", "websocket"],
    &["console", "exceptions"],
    &["repeater", "wsrepeater", "storage"],
    &["history", "workflows"],
];

pub(crate) fn panel_title_key(panel: &str) -> &'static str {
    match panel {
        "traffic" => "panel.traffic",
        "apimap" => "panel.apimap",
        "websocket" => "panel.websocket",
        "console" => "panel.console",
        "exceptions" => "panel.exceptions",
        "repeater" => "panel.repeater",
        "wsrepeater" => "panel.wsrepeater",
        "storage" => "panel.storage",
        "history" => "panel.history",
        _ => "panel.workflows",
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// The folder the Electron app keeps its data in; both apps share it.
pub(crate) fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    user_data_dir(|name| std::env::var(name).ok(), std::env::consts::OS, &home)
}

/// Every ported tool over this window's capture store and page, served over MCP.
/// Every tool domain the app serves, in registration order.
fn tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::default();
    rever_tools_traffic::register(&mut registry);
    rever_tools_inspect::register(&mut registry);
    rever_tools_scripts::register(&mut registry);
    rever_tools_browser::register(&mut registry);
    rever_tools_page::register(&mut registry);
    rever_tools_debug::register(&mut registry);
    rever_tools_redteam::register(&mut registry);
    rever_tools_automation::register(&mut registry);
    rever_tools_automation::register_extras(&mut registry);
    rever_tools_http::register(&mut registry);
    rever_tools_probe::register(&mut registry);
    registry
}

/// A failure costs the agent its tools, not the browser: it is logged and the app goes on.
fn start_mcp(
    traffic: Arc<Mutex<TrafficStore>>,
    browser: Arc<bridge::AppBrowser>,
    debug_endpoint: crate::debug::Endpoint,
) -> (
    Option<McpServer>,
    Arc<InterceptState>,
    Arc<DownloadState>,
    Arc<rever_tools_api::tool::ToolContext>,
) {
    let registry = tool_registry();
    let data_dir = data_dir();
    // The server publishes its endpoint file here.
    let _ = std::fs::create_dir_all(&data_dir);
    let context = Arc::new(ToolContext::new(traffic, browser.clone(), data_dir));
    let context_for_app = context.clone();
    // The intercept and override tools decide requests through the page engine.
    // The breakpoint tools debug pages through Servo's own debugging server.
    let debug = context.state::<DebugState>();
    debug.set_engine(crate::debug::ServoDebugger::new(
        debug_endpoint,
        Arc::downgrade(&debug),
    ));
    let downloads = context.state::<DownloadState>();
    let intercept = context.state::<InterceptState>();
    let edits = Arc::new(crate::intercept::NetEdits::default());
    crate::intercept::install_editors(intercept.clone(), edits.clone());
    intercept.set_engine(Arc::new(crate::intercept::AppIntercept {
        browser: browser.clone(),
        edits,
        state: Arc::downgrade(&intercept),
    }));
    // redteam_start hands its request to the interface through the
    // RendererBridge; without this subscription every ask failed with
    // "no renderer window". The command answers on the main thread.
    {
        let (commands, wake) = browser.channels();
        context
            .state::<RendererBridge>()
            .subscribe(move |op, payload| {
                let (answer, answered) = oneshot::channel();
                let sent = commands
                    .unbounded_send(crate::bridge::Command::InterfaceAsk(
                        op.to_owned(),
                        payload,
                        answer,
                    ))
                    .is_ok();
                let _ = wake.unbounded_send(());
                Box::pin(async move {
                    if !sent {
                        return Err("the browser window is closed".to_owned());
                    }
                    answered
                        .await
                        .unwrap_or_else(|_| Err("the browser window is closed".to_owned()))
                })
            });
    }
    // Macros and workflow steps call the other tools by name through the registry;
    // without it saving or running one failed with "not connected to workflows yet".
    let registry = Arc::new(registry);
    context
        .state::<rever_tools_automation::bridge::ToolRegistryHandle>()
        .set(registry.clone());
    let server = rever_mcp::start(registry, context)
        .inspect_err(|error| eprintln!("[mcp] {error}"))
        .ok();
    // A workflow's AI step runs one of the chat's agents, with the app's tools.
    let endpoint = server
        .as_ref()
        .map(|server| (server.url.clone(), server.auth_header.clone()));
    let profile = server.as_ref().map(|server| server.tool_profile().clone());
    context_for_app
        .state::<rever_tools_automation::workflow_executor::AiAgentsSlot>()
        .set(Arc::new(crate::workflow_agents::WorkflowAgents::new(
            endpoint, profile,
        )));
    (server, intercept, downloads, context_for_app)
}

/// `/^https?:\/\//i`: the pages that can be bookmarked.
pub(crate) fn is_web_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Where a link in a chat message leads, if it is a page the app can open in a tab.
pub(crate) fn link_target(href: &str) -> Option<Url> {
    Url::parse(href)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
}

/// The composer's text after another panel hands it a draft: `prev ? prev + '\n' + text : text`
/// in ChatPanel.tsx.
fn draft_text(previous: &str, draft: &str) -> String {
    if previous.is_empty() {
        draft.to_owned()
    } else {
        format!("{previous}\n{draft}")
    }
}

/// Servo names the console levels after its enum ("warn"); the store holds the names
/// CDP gives them ("warning"), which the panels and the MCP tools filter by.
fn console_kind(level: &str) -> &str {
    match level {
        "warn" => "warning",
        other => other,
    }
}

/// The history store's tab id: "t1", "t2", … as in the Electron tabs store.
fn history_tab_id(tab: TabId) -> String {
    format!("t{}", tab.0)
}

/// System → Light → Dark → System, as `cycle` in app-theme.ts.
/// Where the tabs and the toolbar sit in the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    /// The tab strip across the top, the toolbar under it: Chrome's layout.
    #[default]
    TabsOnTop,
    /// The tabs as a list in a sidebar; the toolbar stays over the page.
    TabsOnSide,
    /// The tabs and the toolbar in the sidebar; the page takes the full height.
    Sidebar,
}

/// The storage key of the layout choice.
const LAYOUT_KEY: &str = "layout";

impl Layout {
    /// The next layout, in the order the menu item cycles through.
    fn next(self) -> Self {
        match self {
            Self::TabsOnTop => Self::TabsOnSide,
            Self::TabsOnSide => Self::Sidebar,
            Self::Sidebar => Self::TabsOnTop,
        }
    }

    /// The name the choice is stored under, and the suffix of its label key.
    pub fn name(self) -> &'static str {
        match self {
            Self::TabsOnTop => "top",
            Self::TabsOnSide => "side",
            Self::Sidebar => "sidebar",
        }
    }

    /// The layout of a stored name; an unknown or missing one is the default.
    fn from_name(name: Option<&str>) -> Self {
        [Self::TabsOnSide, Self::Sidebar]
            .into_iter()
            .find(|layout| name == Some(layout.name()))
            .unwrap_or_default()
    }

    /// Whether the tabs are a list in a sidebar.
    pub fn has_sidebar(self) -> bool {
        self != Self::TabsOnTop
    }
}

fn next_theme(mode: ThemeMode) -> ThemeMode {
    match mode {
        ThemeMode::System => ThemeMode::Light,
        ThemeMode::Light => ThemeMode::Dark,
        ThemeMode::Dark => ThemeMode::System,
    }
}

fn theme_from_stored(mode: app_theme::ThemeMode) -> ThemeMode {
    match mode {
        app_theme::ThemeMode::System => ThemeMode::System,
        app_theme::ThemeMode::Light => ThemeMode::Light,
        app_theme::ThemeMode::Dark => ThemeMode::Dark,
    }
}

fn theme_to_stored(mode: ThemeMode) -> app_theme::ThemeMode {
    match mode {
        ThemeMode::System => app_theme::ThemeMode::System,
        ThemeMode::Light => app_theme::ThemeMode::Light,
        ThemeMode::Dark => app_theme::ThemeMode::Dark,
    }
}

/// The `{mode}` of the theme item's tooltip and the suffix of its hint key.
fn theme_name(mode: ThemeMode) -> &'static str {
    match mode {
        ThemeMode::System => "system",
        ThemeMode::Light => "light",
        ThemeMode::Dark => "dark",
    }
}

/// Runs on Servo's network threads: record what the tap saw.
fn record(store: &Mutex<TrafficStore>, downloads: &DownloadState, event: NetworkTapEvent) {
    fn headers<'a>(pairs: impl Iterator<Item = (&'a str, &'a [u8])>) -> Headers {
        let mut map = Headers::new();
        for (name, value) in pairs {
            let value = String::from_utf8_lossy(value).into_owned();
            // Repeated headers are joined with a newline, as CDP does.
            map.entry(name.to_owned())
                .and_modify(|all| *all = format!("{all}\n{value}"))
                .or_insert(value);
        }
        map
    }
    let patch = match event {
        // The WebSocket variants are routed to record_ws by the tap callback.
        NetworkTapEvent::WebSocketOpened { .. } | NetworkTapEvent::WebSocketFrame { .. } => return,
        NetworkTapEvent::Request {
            id,
            url,
            method,
            headers: h,
            body,
            destination,
            webview_id,
        } => {
            let resource_type = traffic::resource_type_of(&destination);
            // A WS handshake fetch reports its URL with the http scheme; the
            // websocket tools (and the agent) expect ws/wss.
            let mut url = url;
            if resource_type == "WebSocket" {
                let scheme = match url.scheme() {
                    "https" => "wss",
                    _ => "ws",
                };
                url.as_mut_url().set_scheme(scheme).ok();
            }
            RequestPatch {
                request_id: id,
                url: Some(url.to_string()),
                method: Some(method.to_string()),
                resource_type: Some(resource_type.to_owned()),
                request_headers: Some(headers(h.iter().map(|(n, v)| (n.as_str(), v.as_bytes())))),
                request_post_data: (!body.is_empty())
                    .then(|| String::from_utf8_lossy(&body).into_owned()),
                // The tab that sent it (its WebView id as u64); None for a request
                // with no target webview.
                tab_id: webview_id,
                ..RequestPatch::default()
            }
        }
        NetworkTapEvent::Response {
            id,
            mut url,
            status,
            headers: h,
            body,
        } => {
            let response_headers = headers(h.iter().map(|(n, v)| (n.as_str(), v.as_bytes())));
            let mime = traffic::mime_of(&response_headers);
            // What this id was classified as decides two things below.
            let entry_type = store
                .lock()
                .unwrap()
                .get(&id)
                .map(|request| request.resource_type.clone());
            let is_navigation = entry_type.as_deref() == Some("Document");
            // A WS handshake answers with the http form of the socket URL.
            if entry_type.as_deref() == Some("WebSocket") {
                let scheme = match url.scheme() {
                    "https" => "wss",
                    _ => "ws",
                };
                url.as_mut_url().set_scheme(scheme).ok();
            }
            // A top-level navigation answered as an attachment is a download, not a
            // page: record it and save the file (a fetch of one is XHR, not a download).
            if status < 400 {
                let disposition = response_headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-disposition"))
                    .map(|(_, value)| value.clone());
                let is_attachment = disposition
                    .as_deref()
                    .is_some_and(|value| value.to_ascii_lowercase().contains("attachment"));
                if is_attachment && is_navigation {
                    let url = url.to_string();
                    let name = rever_tools_page::downloads::download_filename(
                        &disposition.unwrap_or_default(),
                        &url,
                    );
                    downloads.record(&url, &name, &mime, &body, now_ms());
                }
            }
            let (stored, base64) = traffic::encode_body(&body, &mime);
            RequestPatch {
                request_id: id,
                url: Some(url.to_string()),
                status: Some(status),
                completed_at: Some(now_ms()),
                encoded_data_length: Some(body.len() as u64),
                mime_type: Some(mime),
                response_headers: Some(response_headers),
                response_body: Some(stored),
                response_body_base64: Some(base64),
                ..RequestPatch::default()
            }
        }
    };
    store.lock().unwrap().upsert(patch, now_ms());
}

/// The WebSocket events the tap emits: a connection becomes a store entry the
/// websocket tools filter on (resource type "WebSocket", status 101); frames
/// append to that entry so get_ws_frames can read them back.
fn record_ws(store: &Mutex<TrafficStore>, event: NetworkTapEvent) {
    let (id, frame) = match event {
        NetworkTapEvent::WebSocketOpened {
            id,
            url,
            webview_id,
        } => {
            // Same scheme rewrite as the handshake Request: ws/wss, not http(s).
            let mut url = url;
            let scheme = match url.scheme() {
                "https" => "wss",
                _ => "ws",
            };
            url.as_mut_url().set_scheme(scheme).ok();
            let patch = RequestPatch {
                request_id: id,
                url: Some(url.to_string()),
                method: Some("GET".to_owned()),
                resource_type: Some("WebSocket".to_owned()),
                status: Some(101),
                // A websocket stays open: no completed_at, like CDP's 101 handshake.
                tab_id: webview_id,
                ..RequestPatch::default()
            };
            store.lock().unwrap().upsert(patch, now_ms());
            return;
        }
        NetworkTapEvent::WebSocketFrame {
            id,
            outgoing,
            binary,
            payload,
        } => (
            id,
            WsFrame {
                direction: if outgoing {
                    WsDirection::Sent
                } else {
                    WsDirection::Received
                },
                // 1 = text, 2 = binary, the WebSocket opcodes CDP reports.
                opcode: if binary { 2 } else { 1 },
                payload_data: String::from_utf8_lossy(&payload).into_owned(),
                timestamp: now_ms(),
                mask: Some(outgoing),
            },
        ),
        // Only the WebSocket variants belong here.
        _ => return,
    };
    store.lock().unwrap().append_ws_frame(&id, frame);
}

/// A tab nobody showed or drove for this long is put to sleep, as Chrome's Memory
/// Saver does: its page is closed and opens again from its address when the tab is used.
const TAB_SLEEP_AFTER: Duration = Duration::from_secs(10 * 60);

/// In the sidebar the proxy and profile buttons sit at the window's left edge: their
/// popups open to the right there, and to the left under the toolbar.
fn place_popups(
    layout: Layout,
    proxy: &Entity<ProxyButton>,
    profiles: Option<&Entity<ProfileMenu>>,
    cx: &mut App,
) {
    let right = layout == Layout::Sidebar;
    proxy.update(cx, |button, cx| button.set_opens_right(right, cx));
    if let Some(menu) = profiles {
        menu.update(cx, |menu, cx| menu.set_opens_right(right, cx));
    }
}

/// Whether a tab left alone for `idle` goes to sleep.
/// Whether a tab left alone for `idle` goes to sleep. The shown tab never does, and
/// none does while the agent or a scan works: they drive tabs behind the shown one.
fn sleeps(shown: bool, asleep: bool, idle: Duration, working: bool, after: Duration) -> bool {
    !shown && !asleep && !working && idle >= after
}

/// How long after a tool drove a tab it still counts as the agent's.
const AGENT_RECENT: Duration = Duration::from_secs(5);

/// Whether an agent is at work in a tab: a scan worker owns it, the chat turn that
/// began at `turn` has driven it, or some tool drove it a moment ago.
fn agent_on_tab(acted: Option<Instant>, turn: Option<Instant>, worker: bool, now: Instant) -> bool {
    worker
        || acted.is_some_and(|at| {
            turn.is_some_and(|turn| at >= turn) || now.saturating_duration_since(at) < AGENT_RECENT
        })
}

/// `REVER_TAB_SLEEP_SECS` overrides the wait, to watch tabs fall asleep in a test.
fn tab_sleep_after() -> Duration {
    std::env::var("REVER_TAB_SLEEP_SECS")
        .ok()
        .and_then(|seconds| seconds.parse().ok())
        .map_or(TAB_SLEEP_AFTER, Duration::from_secs)
}

/// How long the grid view of the spaces takes to zoom open or shut.
pub(crate) const SPACES_ZOOM: Duration = Duration::from_millis(260);

/// A space: a group of tabs with a tab strip of its own. One space is shown at a
/// time; the grid view shows them all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpaceId(pub u32);

pub(crate) struct Space {
    pub id: SpaceId,
    pub name: String,
    /// The tab the space showed when it was last left, shown again on return.
    pub shown: Option<TabId>,
}

pub(crate) struct Tab {
    pub id: TabId,
    /// The space whose tab strip holds this tab.
    pub space: SpaceId,
    pub title: SharedString,
    pub url: String,
    /// The latest page frame, in the BGRA layout GPUI paints.
    pub frame: Option<Arc<RenderImage>>,
    /// The page's icon, in the same layout.
    pub favicon: Option<Arc<RenderImage>>,
    /// When the tab was last shown or driven by a tool.
    pub used: Instant,
    /// Asleep: the tab has no page in the engine until it is used again.
    pub asleep: bool,
    /// When a tool last drove this tab.
    pub agent_acted: Option<Instant>,
}

impl Tab {
    /// A tab whose page is loading: nothing painted yet.
    fn new(id: TabId, space: SpaceId, url: String, title: SharedString) -> Self {
        Self {
            id,
            space,
            title,
            url,
            frame: None,
            favicon: None,
            used: Instant::now(),
            asleep: false,
            agent_acted: None,
        }
    }
}

pub(crate) struct ChatLine {
    pub from_user: bool,
    pub text: String,
    /// A turn that failed: drawn as an error, not as the agent's Markdown.
    pub failed: bool,
    /// A tool the agent called: `text` is its name. `Some(true)` once it returned.
    pub tool: Option<bool>,
}

/// Whether a scrolled list shows its end: within a few lines of it counts, so a reader
/// who is at the newest line stays there as more arrives. `offset` is zero at the top
/// and negative below it, as GPUI reports it.
pub(crate) fn shows_end(offset: f32, max_offset: f32) -> bool {
    -offset >= max_offset - 40.0
}

/// The conversation as the history store keeps it: one message per line, shaped like
/// the Electron app's `UIMessage`s so the store's title (the first user text) works.
/// The empty reply line a turn starts with is not kept.
fn lines_to_messages(chat: &[ChatLine]) -> Vec<Value> {
    chat.iter()
        .filter(|line| line.from_user || line.tool.is_some() || !line.text.is_empty())
        .map(|line| {
            let role = if line.from_user { "user" } else { "assistant" };
            let part = match line.tool {
                Some(_) => json!({ "type": "dynamic-tool", "toolName": line.text, "state": "output-available" }),
                None => json!({ "type": "text", "text": line.text }),
            };
            json!({ "role": role, "parts": [part], "failed": line.failed })
        })
        .collect()
}

/// The lines of a saved conversation. A tool that was still running when it was saved
/// reads as finished: nothing is running after a restart.
fn messages_to_lines(messages: &[Value]) -> Vec<ChatLine> {
    messages
        .iter()
        .filter_map(|message| {
            let part = message["parts"].get(0)?;
            let tool = part["type"] == "dynamic-tool";
            let text = if tool {
                &part["toolName"]
            } else {
                &part["text"]
            };
            Some(ChatLine {
                from_user: message["role"] == "user",
                text: text.as_str()?.to_owned(),
                failed: message["failed"] == true,
                tool: tool.then_some(true),
            })
        })
        .collect()
}

/// How much of the earlier conversation a new session is told, in characters.
const EARLIER_LIMIT: usize = 8_000;

/// What a session that has not seen the conversation is sent: the earlier lines as a
/// transcript, then the new message. A session started after a restart, a reopened
/// conversation or a change of agent would otherwise answer as if nothing was said.
fn with_earlier(earlier: &[ChatLine], text: &str) -> String {
    let mut transcript = String::new();
    for line in earlier
        .iter()
        .filter(|line| line.tool.is_none() && !line.failed)
    {
        if line.text.is_empty() {
            continue;
        }
        let who = if line.from_user { "User" } else { "Assistant" };
        transcript.push_str(&format!("{who}: {}\n\n", line.text));
    }
    if transcript.is_empty() {
        return text.to_owned();
    }
    // The newest part is what matters when it does not all fit.
    let skip = transcript.chars().count().saturating_sub(EARLIER_LIMIT);
    let recent: String = transcript.chars().skip(skip).collect();
    format!(
        "[Earlier in this conversation, for context]\n{}[End of the earlier conversation]\n\n{text}",
        recent
    )
}

/// `mcp__rever-traffic__list_requests` is shown as `list_requests`.
fn tool_label(name: &str) -> &str {
    name.rsplit("__").next().unwrap_or(name)
}

/// One event of the agent's turn applied to the conversation. Text grows the reply
/// it belongs to; a tool call is a line of its own, so text after it starts a new reply.
fn apply_agent_event(chat: &mut Vec<ChatLine>, event: &AgentEvent) {
    let finish_tools = |chat: &mut Vec<ChatLine>| {
        for line in chat.iter_mut().filter(|line| line.tool == Some(false)) {
            line.tool = Some(true);
        }
    };
    match event {
        AgentEvent::Text(text) => {
            let reply = chat
                .last_mut()
                .filter(|line| !line.from_user && !line.failed && line.tool.is_none());
            match reply {
                Some(line) => line.text.push_str(text),
                None => chat.push(ChatLine {
                    from_user: false,
                    text: text.clone(),
                    failed: false,
                    tool: None,
                }),
            }
        }
        AgentEvent::ToolCall(name, _) => chat.push(ChatLine {
            from_user: false,
            text: tool_label(name).to_owned(),
            failed: false,
            tool: Some(false),
        }),
        AgentEvent::ToolResult(name, _) => {
            let label = tool_label(name);
            let call = chat
                .iter_mut()
                .rev()
                .find(|line| line.tool == Some(false) && line.text == label);
            if let Some(line) = call {
                line.tool = Some(true);
            }
        }
        AgentEvent::Done | AgentEvent::Failed(_) => finish_tools(chat),
        // Shown as a prompt over the window, not as a line of the conversation.
        AgentEvent::Permission(_) => {}
    }
}

/// The pages to open at start, with the titles their tabs show until they load, and
/// which of them is shown: the URL of the command line, else the tabs the last run
/// left open (as Chrome continues where you left off), else the home page.
fn start_pages(asked: Option<Url>, saved: Option<&str>) -> (Vec<(Url, String)>, usize) {
    let home = |url: Option<Url>| {
        let url = url.unwrap_or_else(|| Url::parse(DEFAULT_URL).expect("the home page is a URL"));
        (vec![(url, String::new())], 0)
    };
    let Some(saved) = parse_saved_tabs(saved).filter(|_| asked.is_none()) else {
        return home(asked);
    };
    // The saved index is untrusted; the cast saturates.
    let wanted = saved.active_index.max(0.0) as usize;
    let (mut pages, mut active) = (Vec::new(), 0);
    for (index, tab) in saved.tabs.into_iter().enumerate() {
        if let Ok(url) = Url::parse(&tab.url) {
            // A tab whose address no longer parses is skipped: the one before it is shown.
            if index <= wanted {
                active = pages.len();
            }
            pages.push((url, tab.title.unwrap_or_default()));
        }
    }
    if pages.is_empty() {
        home(None)
    } else {
        (pages, active)
    }
}

/// What the command line asked for.
pub(crate) struct Launch {
    /// The page to open; `None` continues where the last run left off.
    pub url: Option<Url>,
    pub open_panel: Option<&'static str>,
    pub self_check: Option<String>,
    /// A chat message to send shortly after start (port harness).
    pub say: Option<String>,
}

/// An open `prompt()` dialog awaiting the user: the message, a text input, and
/// the log entry's timestamp so the answer can be recorded on it.
pub(crate) struct PromptOverlay {
    pub dialog: SimpleDialog,
    pub input: Entity<TextInput>,
    pub message: String,
    ts: u64,
}

/// An open color picker: the picker and a `#rrggbb` text field.
pub(crate) struct ColorOverlay {
    pub picker: ColorPicker,
    pub input: Entity<TextInput>,
}

/// `RgbColor` → `#rrggbb`.
pub(crate) fn rgb_to_hex(c: RgbColor) -> String {
    format!("#{:02x}{:02x}{:02x}", c.red, c.green, c.blue)
}

/// `#rrggbb` / `rrggbb` → `RgbColor`; `None` if it isn't 6 hex digits.
pub(crate) fn hex_to_rgb(hex: &str) -> Option<RgbColor> {
    let hex = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
    if hex.len() != 6 {
        return None;
    }
    let byte = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).ok();
    Some(RgbColor {
        red: byte(0..2)?,
        green: byte(2..4)?,
        blue: byte(4..6)?,
    })
}

pub(crate) struct Browser {
    pub engine: Engine,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub chat: Vec<ChatLine>,
    pub open_panel: Option<&'static str>,
    /// Where the page was laid out last, in window coordinates. Written during paint.
    pub page_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The capture store, the stores of the history views and the settings storage.
    pub stores: Stores,
    /// The engine bridge, kept so the storage panel's host can reach it (and so
    /// `rebuild_views` can hand it to a fresh `Views`).
    storage_browser: Arc<bridge::AppBrowser>,
    /// The text fields and panels; made again when the language or the theme changes.
    pub views: Views,
    pub menu: Entity<AppMenu>,
    pub ip_badge: Entity<IpBadge>,
    /// Set by the network tap's threads when the capture store changed.
    traffic_changed: Arc<AtomicBool>,
    /// When the workflows file last changed, as far as the Workflows panel has seen.
    workflows_modified: Option<std::time::SystemTime>,
    /// Answers to what the console panel ran in the page, collected until the next pump.
    eval_results: Rc<RefCell<Vec<Result<JSValue, String>>>>,
    /// The Claude Code CLI, started with the first chat message.
    agent: Option<Box<dyn ChatAgent>>,
    pub agent_busy: bool,
    /// How many init scripts tools have added; the count is the next script's id.
    /// Wakes `pump` from other threads.
    wake: UnboundedSender<()>,
    /// Requests from MCP tools, answered in `pump`.
    tool_commands: UnboundedReceiver<bridge::Command>,
    /// The sending end, for the app's own commands (the scan's tab opens).
    tool_tx: UnboundedSender<bridge::Command>,
    /// The scan asked for its tabs to be closed; the wake loop does it.
    close_scan_tabs: bool,
    /// Serves the tools to the agent CLI and to outside clients (mcp-endpoint.json);
    /// `None` when it could not start. Dropping it stops the server.
    mcp: Option<McpServer>,
    /// The tools' shared context — the scan clears this site's findings through it.
    tool_context: Arc<ToolContext>,
    pub page_focus: FocusHandle,
    pub lang: Lang,
    pub theme_mode: ThemeMode,
    pub layout: Layout,
    app_theme: AppThemeStore,
    /// The active tab's frame as RGBA, kept only for --self-check.
    last_rgba: Option<RgbaImage>,
    self_check: Option<String>,
    /// The agent's answer for the next page dialog: accept, and a prompt's text.
    dialog_answer: Option<(bool, Option<String>)>,
    /// The native dialogs the pages opened, oldest first, for dialog_history.
    dialog_log: Vec<DialogRecord>,
    /// Off: dialogs wait for the user even while the agent acts.
    dialog_auto_dismiss: bool,
    /// Files the agent hands to the next file picker.
    upload_paths: Vec<PathBuf>,
    /// When a tool last drove the page: a dialog opened right after is the agent's.
    tool_acted: Option<Instant>,
    /// The once-a-second repaint for the agent's marks is running.
    agent_marks_watched: bool,
    /// Height of the open bottom chip panel; the user drags its top edge.
    pub panel_height: f32,
    /// While the panel's top edge is dragged: (pointer y at start, height at start).
    pub panel_drag: Option<(f32, f32)>,
    /// The Red Team scan dialog (a movable popup) is open.
    pub redteam_open: bool,
    /// Chosen scan intensity: "readonly" | "probe" | "redteam".
    pub redteam_intensity: &'static str,
    /// Max same-site pages the scan opens.
    pub redteam_pages: u32,
    /// Top-left of the dialog in window pixels.
    pub redteam_pos: (f32, f32),
    /// While the header is dragged: the (pointer - pos) offset held constant.
    pub redteam_drag: Option<(f32, f32)>,
    /// A scan turn is streaming into the dialog (not the chat panel).
    pub redteam_running: bool,
    /// The activity log section of the dialog is expanded.
    pub redteam_log_open: bool,
    /// The activity log's scroll position, kept at the newest line while running.
    pub redteam_log_scroll: gpui::ScrollHandle,
    /// The scan report's own window, when one is open.
    pub redteam_report_win: Option<gpui::WindowHandle<crate::shell::report::ReportWindow>>,
    /// A scan just started: open the spaces grid so its worker spaces show live.
    pub scan_spaces_due: bool,
    /// The model the scan's CLI sessions run on (None = the CLI's own default).
    pub redteam_model: Option<(String, String)>,
    /// The scan dialog's model chip.
    pub scan_model_picker: Entity<ModelPicker>,
    /// The running (or just-finished) scan orchestrator, when there is one.
    pub scan: Option<crate::scan::ScanRun>,
    /// URLs of the tabs the scan opened; closed when it finishes.
    pub scan_tabs: Vec<String>,
    /// Dialog size; the bottom-right corner drags it.
    pub redteam_w: f32,
    pub redteam_h: f32,
    /// While the corner is dragged: (pointer x, pointer y, width, height) at start.
    pub redteam_size_drag: Option<(f32, f32, f32, f32)>,
    /// The `<select>` whose option list is open over the page.
    pub select: Option<SelectElement>,
    /// An open `prompt()` dialog: the page can't type into the system prompt, so
    /// it gets a text-input overlay instead.
    pub prompt: Option<PromptOverlay>,
    /// An open right-click context menu, drawn over the page at its position.
    pub context_menu: Option<ContextMenu>,
    /// An open `<input type=color>` picker: a hex field over the page.
    pub color_picker: Option<ColorOverlay>,
    /// Where the debugger tools find Servo's debugging server.
    debug_endpoint: crate::debug::Endpoint,
    /// Mobile mode: the page is a phone-wide column in the middle of its area.
    pub mobile: bool,
    /// Requests a modify rule holds until the agent decides them.
    held_requests: crate::intercept::Held,
    /// The text the input method is still composing in the page (한 before it
    /// becomes 한글); `None` when nothing is being composed.
    pub composing: Option<String>,
    /// Addresses of closed tabs, newest last, for Cmd+Shift+T.
    closed_tabs: Vec<String>,
    /// The open tabs as last written for the next start.
    saved_session: String,
    /// The spaces the tabs are grouped in; never empty.
    pub spaces: Vec<Space>,
    /// The space whose tabs the tab strip shows.
    pub space: SpaceId,
    next_space: u32,
    saved_spaces: String,
    /// The grid view is open: every space as a tile, in place of the page.
    pub spaces_open: bool,
    pub spaces_focus: FocusHandle,
    /// The grid view zooming open or shut: when it began, the space whose picture
    /// moves between the page and its tile, and whether the view is opening.
    pub spaces_zoom: Option<(Instant, SpaceId, bool)>,
    /// A picture of each space's page for the grid view, taken while it is open.
    pub space_shots: Vec<(SpaceId, Arc<RenderImage>)>,
    space_shots_at: Instant,
    /// What the process holds, for the development build's overlay.
    pub memory: crate::memory::MemoryWatch,
    /// The link a wheel-button press landed on; its release opens it in a new tab.
    pub wheel_link: Option<Url>,
    /// Page frames shown in the last full second, and the count of the running one.
    pub fps: u32,
    frames: u32,
    /// Servo's latest memory report, for the card a hovered tab shows.
    pub page_memory: crate::engine::MemoryReport,
    page_memory_asked: Option<Instant>,
    /// The agent's tool calls that wait for the user's yes or no.
    pub permissions: Rc<RefCell<PermissionStore<ChatRules>>>,
    pub permission_prompt: Entity<PermissionPrompt>,
    /// The first-run guide: which agent the chat talks to.
    pub onboarding: Entity<OnboardingModal>,
    /// The offer to bring Chrome's logins over, once after the guide and from the menu.
    pub browser_import: Entity<BrowserImportModal>,
    /// The profiles, each a window of its own. Only the app's first window lists them.
    pub profile_menu: Option<Entity<ProfileMenu>>,
    /// Opens a window that goes through a proxy; in such a window, changes its proxy.
    pub proxy_button: Entity<ProxyButton>,
    /// What a profile or proxy window is called; `None` in the app's first window.
    pub window_label: Option<SharedString>,
    /// Keeps the conversation at its newest line while a reply streams.
    pub chat_scroll: gpui::ScrollHandle,
    /// The model chip of the composer: every agent's models in one list.
    pub model_picker: Entity<ModelPicker>,
    /// The model lists the picker shows; the CLIs' own answers are put in here.
    model_store: Rc<RefCell<ModelCatalogStore>>,
    /// The agent the conversation runs on.
    chat_agent: AcpAgentId,
    /// The chosen model's id and name; `None` leaves the agent's default.
    chat_model: Option<(String, String)>,
    /// When the running turn started, for the "working" line.
    pub turn_started: Option<Instant>,
    /// The saved conversations, and the "history" chip that lists them.
    chat_history: ChatHistory,
    pub history_menu: Entity<ChatHistoryMenu>,
    /// The id the conversation on screen is saved under.
    conversation_id: String,
    chat_collapsed_store: ChatCollapsedStore,
    /// The chat panel is folded away; the toolbar button brings it back.
    pub chat_collapsed: bool,
    /// The chat panel covers the page.
    pub chat_expanded: bool,
    /// The panel's width as the user dragged it; the window can ask for less.
    pub chat_width: f32,
    /// While the panel's left edge is dragged: (pointer x at start, width at start).
    pub chat_drag: Option<(f32, f32)>,
    /// The width the panel was last drawn at, and while it slides to a new one:
    /// when the slide started and the width it started from.
    pub chat_drawn: f32,
    pub chat_slide: Option<(Instant, f32)>,
    /// The bottom panel's slide open or shut: when it began and the height it began at.
    pub panel_slide: Option<(Instant, f32)>,
    /// The height the bottom panel was last drawn at.
    pub panel_drawn: f32,
    /// The panel last open: it is still the one drawn while it slides shut.
    pub panel_last: Option<&'static str>,
}

impl Browser {
    pub fn new(launch: Launch, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Launch {
            url,
            open_panel,
            self_check,
            say,
        } = launch;
        let favicons = data_dir().join("favicons");
        let _ = std::fs::create_dir_all(&favicons);
        rever_ui_history::history_panel::set_favicon_dir(favicons);
        let lang = Lang::detect();
        let (wake, mut woken) = futures::channel::mpsc::unbounded::<()>();
        let agent_wake = wake.clone();
        let (tool_tx, tool_commands) = futures::channel::mpsc::unbounded();
        let tool_tx_app = tool_tx.clone();
        let tool_browser = Arc::new(bridge::AppBrowser::new(tool_tx, wake.clone()));
        let traffic = Arc::new(Mutex::new(TrafficStore::default()));
        let debug_endpoint = crate::debug::Endpoint::default();
        let (mcp, intercept, downloads, tool_context) = start_mcp(
            traffic.clone(),
            tool_browser.clone(),
            debug_endpoint.clone(),
        );
        let traffic_changed = Arc::new(AtomicBool::new(false));
        {
            let (store, wake, changed) = (traffic.clone(), wake.clone(), traffic_changed.clone());
            let downloads = downloads.clone();
            net::tap::set_network_tap(move |event| {
                match &event {
                    NetworkTapEvent::WebSocketOpened { .. }
                    | NetworkTapEvent::WebSocketFrame { .. } => {
                        record_ws(&store, event);
                    }
                    NetworkTapEvent::Request { .. } | NetworkTapEvent::Response { .. } => {
                        record(&store, &downloads, event);
                    }
                }
                changed.store(true, Ordering::Relaxed);
                let _ = wake.unbounded_send(());
            });
        }
        let mut engine = Engine::new(
            move || {
                let _ = wake.unbounded_send(());
            },
            window.scale_factor(),
            data_dir().join("servo-profile"),
        );
        let held_requests = crate::intercept::Held::default();
        let note = {
            let (store, changed) = (traffic.clone(), traffic_changed.clone());
            move |patch| {
                store.lock().unwrap().upsert(patch, now_ms());
                changed.store(true, Ordering::Relaxed);
            }
        };
        engine.on_resource_load(crate::intercept::on_load(
            intercept,
            held_requests.clone(),
            note,
        ));
        // Tabs left alone go to sleep; looked at twice within the wait, so none
        // oversleeps it by much.
        cx.spawn_in(window, async move |this, cx| {
            loop {
                let pause = (tab_sleep_after() / 2).min(Duration::from_secs(30));
                cx.background_executor().timer(pause).await;
                let looked =
                    this.update_in(cx, |this, window, cx| this.sleep_idle_tabs(window, cx));
                if looked.is_err() {
                    break;
                }
                crate::memory::release_unused();
            }
        })
        .detach();
        // A development build watches its own memory; the shell draws it over the page.
        if cfg!(debug_assertions) {
            cx.spawn(async move |this, cx| {
                while let Some(bytes) = crate::memory::footprint() {
                    let recorded = this.update(cx, |this, cx| {
                        this.memory.record(bytes);
                        this.fps = std::mem::take(&mut this.frames);
                        cx.notify();
                    });
                    if recorded.is_err() {
                        break;
                    }
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                }
            })
            .detach();
        }
        // A --self-check run is a test: it must not add its pages to the user's history.
        let stores = Stores::new(traffic.clone(), self_check.is_none(), cx);
        let (pages, active) = start_pages(url, stores.storage.get(TABS_KEY).as_deref());
        // Only the shown tab loads; the others stay asleep until they are used, so a
        // long list of tabs costs one page at start.
        // The tabs come back in the spaces they were in.
        let saved_spaces = space_rules::parse_saved_spaces(
            stores.storage.get(space_rules::STORE_KEY).as_deref(),
            pages.len(),
        );
        let spaces: Vec<Space> = (1u32..)
            .zip(saved_spaces.names)
            .map(|(id, name)| Space {
                id: SpaceId(id),
                name,
                shown: None,
            })
            .collect();
        let next_space = spaces.len() as u32 + 1;
        let mut tabs = Vec::new();
        for (index, (url, title)) in pages.into_iter().enumerate() {
            let space = spaces[saved_spaces.of[index]].id;
            let mut tab = Tab::new(engine.reserve(), space, url.to_string(), title.into());
            if index == active {
                engine
                    .open_as(tab.id, url, PhysicalSize::new(800, 600))
                    .expect("the page engine could not start");
            } else {
                tab.asleep = true;
            }
            tabs.push(tab);
        }
        let url = tabs[active].url.clone();
        // The space shown is the one of the tab shown.
        let space = tabs[active].space;

        // Servo and the tap wake this task from their threads; the work happens here, on the main thread.
        cx.spawn_in(window, async move |this, cx| {
            while woken.next().await.is_some() {
                while woken.try_recv().is_ok() {}
                if this
                    .update_in(cx, |this, window, cx| this.pump(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        if let Some(dir) = self_check.clone() {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(8)).await;
                let _ = this.update(cx, |this, cx| {
                    this.write_self_check(&dir);
                    cx.quit();
                });
            })
            .detach();
        }

        if let Some(text) = say {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let _ = this.update(cx, |this, cx| this.send_chat(&text, cx));
            })
            .detach();
        }

        let app_theme = AppThemeStore::load(stores.storage.as_ref());
        // REVER_THEME wins over the saved choice, for previews and captures.
        let theme_mode = if std::env::var("REVER_THEME").is_ok_and(|mode| !mode.is_empty()) {
            ThemeMode::from_env()
        } else {
            theme_from_stored(app_theme.mode())
        };
        // REVER_LAYOUT wins over the saved choice too.
        let layout = Layout::from_name(
            std::env::var("REVER_LAYOUT")
                .ok()
                .or_else(|| stores.storage.get(LAYOUT_KEY))
                .as_deref(),
        );
        let views = Views::new(
            &stores,
            lang,
            theme_mode,
            tool_browser.clone(),
            tool_context.clone(),
            cx,
        );
        views
            .address
            .update(cx, |input, cx| input.set_text(url, cx));

        let this = cx.weak_entity();
        // The menus read language and theme on every render, so a switch shows at once.
        let look: LookFn = {
            let this = this.clone();
            Rc::new(move |cx| {
                this.read_with(cx, |this, _| Look {
                    theme_mode: this.theme_mode,
                    lang: this.lang,
                })
                .unwrap_or(Look { theme_mode, lang })
            })
        };
        // shortcut: the approvals are not written to the audit log yet; connect
        // `ApprovalAuditStore` when the log's panel is ported.
        let permissions = Rc::new(RefCell::new(PermissionStore::new(ChatRules, |_| {})));
        let permission_prompt = cx.new(|cx| {
            let permissions = permissions.clone();
            let props = move |_: &App| {
                let store = permissions.borrow();
                PermissionPromptProps {
                    current: store
                        .current_permission()
                        .map(|waiting| (waiting.id(), waiting.request().clone())),
                    queue_len: store.permission_queue().len(),
                }
            };
            PermissionPrompt::new(props, look.clone(), cx)
        });
        cx.subscribe(
            &permission_prompt,
            |this, _, event: &PermissionPromptEvent, cx| this.answer_permission(event, cx),
        )
        .detach();
        let menu = cx.new(|_| {
            let this = this.clone();
            AppMenu::new(move |cx| menu_props(&this, cx), look.clone())
        });
        let setup: Rc<dyn SetupHost> = Rc::new(crate::setup_host::SetupBridge::new(
            stores.storage.clone(),
            self_check.is_some(),
            tool_browser.clone(),
            this.clone(),
        ));
        let onboarding = cx.new(|cx| OnboardingModal::new(setup.clone(), look.clone(), cx));
        let own_window = crate::profile_windows::this_window();
        let window_proxy = own_window.as_ref().and_then(|own| own.proxy.clone());
        let window_label = own_window.map(|own| SharedString::from(own.label));
        // A profile window is one profile: the list of them stays in the first window.
        let profile_menu = (window_label.is_none() && self_check.is_none()).then(|| {
            let store = cx.new(|_| ProfilesStore::new(setup.clone()));
            cx.new(|cx| ProfileMenu::new(store, setup.clone(), look.clone(), cx))
        });
        let proxy_button = {
            let this = this.clone();
            let tab = move |cx: &App| {
                this.read_with(cx, |this, _| {
                    let tab = &this.tabs[this.active];
                    rever_ui_state::tabs::Tab {
                        id: tab.id.0.to_string(),
                        url: tab.url.clone(),
                        title: tab.title.to_string(),
                        web_contents_id: None,
                        initial_url: String::new(),
                        // The window's proxy is every tab's; "isolated" is what lets
                        // the button change it in place.
                        partition: window_proxy.as_ref().map(|_| "window".to_owned()),
                        proxy: window_proxy.clone(),
                        profile_id: None,
                    }
                })
                .ok()
            };
            let button =
                cx.new(|cx| ProxyButton::new(setup.clone(), tab, |_| {}, look.clone(), cx));
            button.update(cx, |button, _| button.set_http_only(true));
            button
        };
        place_popups(layout, &proxy_button, profile_menu.as_ref(), cx);
        if window_label.is_none() && self_check.is_none() {
            // What a crashed incognito or proxy window left on the disk.
            crate::profile_windows::sweep_ephemeral(&data_dir().join("profiles"));
        } else {
            // Logins brought from another browser for this profile, left by the
            // window that made it.
            let cookies = crate::profile_windows::take_cookies(&data_dir());
            let page = tool_browser.clone();
            cx.background_spawn(async move {
                crate::storage_host::store_cookies(page.as_ref(), &cookies).await;
            })
            .detach();
        }
        let browser_import = {
            // A self-check run offers nothing: its storage starts empty every time.
            let storage: Rc<dyn rever_ui_state::Storage> = if self_check.is_some()
                || crate::profile_windows::this_window().is_some()
            {
                let offered = rever_ui_state::storage::MemoryStorage::new();
                let _ = rever_ui_state::Storage::set(&offered, "rev:browser-import-offered", "1");
                Rc::new(offered)
            } else {
                stores.storage.clone()
            };
            cx.new(|_| BrowserImportModal::new(setup, storage, look.clone()))
        };
        let ip_badge = {
            let host = Rc::new(IpHost::new(stores.storage.clone()));
            cx.new(|cx| IpBadge::new(host, look.clone(), cx))
        };
        // The conversation the app was closed with comes back, and the rest is one
        // click away in the history menu.
        let chat_history = ChatHistory::load(stores.storage.as_ref());
        let (conversation_id, chat) = match chat_history.conversations().first() {
            // A self-check run starts from nothing, whatever was saved.
            Some(latest) if self_check.is_none() => {
                (latest.id.clone(), messages_to_lines(&latest.messages))
            }
            _ => (fresh_conversation_id(), Vec::new()),
        };
        let chat_collapsed_store = ChatCollapsedStore::load(stores.storage.as_ref());
        let history_menu = cx.new(|_| {
            let this = this.clone();
            ChatHistoryMenu::new(move |cx| history_props(&this, cx), look.clone())
        });
        cx.subscribe(&history_menu, |this, menu, event: &ChatHistoryEvent, cx| {
            match event {
                ChatHistoryEvent::Open(id) => this.open_conversation(id, cx),
                ChatHistoryEvent::Remove(id) => this.remove_conversation(id, cx),
            }
            menu.update(cx, |menu, cx| menu.set_open(false, cx));
        })
        .detach();
        let chat_agent = stores
            .storage
            .get(AGENT_STORAGE_KEY)
            .and_then(|id| chat_agents::agent_by_id(&id))
            .unwrap_or(AcpAgentId::ClaudeCode);
        let chat_model = stores
            .storage
            .get(MODEL_STORAGE_KEY)
            .filter(|id| !id.is_empty())
            .map(|id| {
                let name = stores
                    .storage
                    .get(MODEL_NAME_STORAGE_KEY)
                    .unwrap_or_else(|| id.clone());
                (id, name)
            });
        // The lists on disk first, so the picker is full at once.
        let catalog = ModelCatalog::new(&data_dir());
        let model_store = Rc::new(RefCell::new(ModelCatalogStore::new()));
        for agent in LIVE_MODEL_AGENTS {
            model_store
                .borrow_mut()
                .put(agent, catalog.cached(agent).as_ref());
        }
        let model_picker = cx.new(|cx| {
            let this = this.clone();
            let host = Rc::new(PickerHost {
                models: model_store.clone(),
            });
            ModelPicker::new(move |cx| picker_props(&this, cx), host, look.clone(), cx)
        });
        cx.subscribe(&model_picker, |this, _, event: &ModelPickerEvent, cx| {
            if let ModelPickerEvent::Selected {
                agent_id,
                model_id,
                name,
                ..
            } = event
            {
                this.choose_model(*agent_id, model_id, name, cx);
            }
        })
        .detach();
        // The Red Team dialog's own model chip; the scan's CLI sessions run on it.
        let scan_model_picker = cx.new(|cx| {
            let this = this.clone();
            let host = Rc::new(PickerHost {
                models: model_store.clone(),
            });
            ModelPicker::new(
                move |cx| scan_picker_props(&this, cx),
                host,
                look.clone(),
                cx,
            )
        });
        cx.subscribe(
            &scan_model_picker,
            |this, _, event: &ModelPickerEvent, cx| {
                if let ModelPickerEvent::Selected {
                    agent_id,
                    model_id,
                    name,
                    ..
                } = event
                {
                    // Scan workers are Claude CLI sessions; other agents' models do not apply.
                    if *agent_id == AcpAgentId::ClaudeCode {
                        this.redteam_model = Some((model_id.to_owned(), name.to_owned()));
                        cx.notify();
                    }
                }
            },
        )
        .detach();
        // The CLIs themselves know their models; asked once per start, off the main thread.
        if self_check.is_none() {
            cx.spawn(async move |this, cx| {
                let fresh = cx
                    .background_executor()
                    .spawn(async move {
                        let (cli, version) = (ClaudeCliFinder::new(), env!("CARGO_PKG_VERSION"));
                        LIVE_MODEL_AGENTS.map(|agent| {
                            (
                                agent,
                                catalog.refresh(agent, &cli, version, || now_ms() as i64),
                            )
                        })
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    for (agent, list) in &fresh {
                        this.model_store.borrow_mut().put(agent, list.as_ref());
                    }
                    this.model_picker.update(cx, |_, cx| cx.notify());
                });
            })
            .detach();
        }
        // The bot-check floating button is hidden in the browser, so it is not built.
        Self {
            engine,
            tabs,
            active,
            saved_session: String::new(),
            spaces,
            space,
            next_space,
            saved_spaces: String::new(),
            spaces_open: false,
            spaces_focus: cx.focus_handle(),
            spaces_zoom: None,
            space_shots: Vec::new(),
            space_shots_at: Instant::now(),
            memory: Default::default(),
            wheel_link: None,
            fps: 0,
            frames: 0,
            page_memory: Vec::new(),
            page_memory_asked: None,
            permissions,
            permission_prompt,
            onboarding,
            browser_import,
            profile_menu,
            proxy_button,
            window_label,
            chat,
            open_panel,
            page_bounds: Rc::new(Cell::new(Bounds::default())),
            stores,
            storage_browser: tool_browser,
            views,
            menu,
            ip_badge,
            traffic_changed,
            workflows_modified: None,
            eval_results: Rc::default(),
            agent: None,
            agent_busy: false,
            wake: agent_wake,
            tool_commands,
            tool_tx: tool_tx_app,
            close_scan_tabs: false,
            mcp,
            tool_context,
            page_focus: cx.focus_handle(),
            lang,
            theme_mode,
            layout,
            app_theme,
            last_rgba: None,
            self_check,
            dialog_answer: None,
            dialog_log: Vec::new(),
            dialog_auto_dismiss: true,
            upload_paths: Vec::new(),
            tool_acted: None,
            agent_marks_watched: false,
            panel_height: 320.0,
            panel_drag: None,
            redteam_open: false,
            redteam_intensity: "readonly",
            redteam_pages: 8,
            redteam_pos: (0.0, 0.0),
            redteam_drag: None,
            redteam_running: false,
            redteam_log_open: true,
            redteam_log_scroll: gpui::ScrollHandle::new(),
            redteam_report_win: None,
            scan_spaces_due: false,
            scan: None,
            scan_tabs: Vec::new(),
            redteam_w: 720.0,
            redteam_h: 560.0,
            redteam_size_drag: None,
            select: None,
            prompt: None,
            context_menu: None,
            color_picker: None,
            mobile: false,
            debug_endpoint,
            held_requests,
            composing: None,
            closed_tabs: Vec::new(),
            chat_scroll: gpui::ScrollHandle::new(),
            model_picker,
            scan_model_picker,
            redteam_model: None,
            model_store,
            chat_agent,
            chat_model,
            turn_started: None,
            chat_history,
            history_menu,
            conversation_id,
            chat_collapsed: chat_collapsed_store.collapsed(),
            chat_collapsed_store,
            chat_expanded: false,
            chat_width: crate::shell::CHAT_WIDTH,
            chat_drag: None,
            chat_drawn: 0.0,
            chat_slide: None,
            panel_slide: None,
            panel_drawn: 0.0,
            panel_last: None,
        }
    }

    pub fn active_id(&self) -> TabId {
        self.tabs[self.active].id
    }

    /// Load a captured request into the repeater panel's store and open the
    /// panel (the "↻R" / "send to repeater" action).
    pub fn load_repeater(&mut self, request_id: &str, cx: &mut Context<Self>) {
        let stored = self.stores.traffic.lock().unwrap().get(request_id).cloned();
        self.views.repeater_store.update(cx, |store, cx| {
            store.load_from_traffic(request_id, stored.as_ref());
            cx.notify();
        });
        self.set_open_panel(Some("repeater"), cx);
    }

    /// The tab a targeted command names by index, or the active tab when the
    /// index is absent or out of range.
    /// A tab a tool reaches behind the shown one is woken and counts as used.
    fn resolve_tab(&mut self, target: Option<usize>) -> TabId {
        let index = target
            .filter(|index| *index < self.tabs.len())
            .unwrap_or(self.active);
        self.wake_tab(index);
        self.tabs[index].id
    }

    /// Whether an agent is at work in `tab`: the badge on its tab and its space.
    pub(crate) fn agent_on(&self, tab: &Tab) -> bool {
        let turn = self.turn_started.filter(|_| self.agent_busy);
        let worker = self
            .scan
            .as_ref()
            .is_some_and(|scan| scan.worker_on(&tab.url));
        agent_on_tab(tab.agent_acted, turn, worker, Instant::now())
    }

    /// Stop the agent at work in a space's tabs: its scan workers, or the chat turn.
    /// A client outside the app (the CLI) cannot be stopped from here; its mark goes.
    pub fn stop_agent_in(&mut self, space: SpaceId, cx: &mut Context<Self>) {
        let mut chat = false;
        for index in 0..self.tabs.len() {
            if self.tabs[index].space != space || !self.agent_on(&self.tabs[index]) {
                continue;
            }
            let url = self.tabs[index].url.clone();
            let worker = self
                .scan
                .as_mut()
                .is_some_and(|scan| scan.stop_worker(&url));
            chat |= !worker;
            self.tabs[index].agent_acted = None;
        }
        if chat {
            self.stop_chat(cx);
        }
        cx.notify();
    }

    /// Repaint once a second while a tab carries the agent's mark, so the mark
    /// goes when the agent has left.
    fn watch_agent_marks(&mut self, cx: &mut Context<Self>) {
        if self.agent_marks_watched {
            return;
        }
        self.agent_marks_watched = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let marked = this.update(cx, |this, cx| {
                    cx.notify();
                    this.agent_marks_watched = this.tabs.iter().any(|tab| this.agent_on(tab));
                    this.agent_marks_watched
                });
                if !matches!(marked, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Open a sleeping tab's page again, and count the tab as used now.
    fn wake_tab(&mut self, index: usize) {
        let size = self
            .engine
            .size(self.active_id())
            .unwrap_or(PhysicalSize::new(800, 600));
        let tab = &mut self.tabs[index];
        tab.used = Instant::now();
        if !tab.asleep {
            return;
        }
        // An address that does not open leaves the tab asleep, to try again next time.
        if let Ok(url) = Url::parse(&tab.url)
            && self.engine.open_as(tab.id, url, size).is_ok()
        {
            tab.asleep = false;
        }
    }

    /// Close the pages of the tabs nobody used for a while; the tabs stay in the strip.
    fn sleep_idle_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let working = self.agent_busy || self.scan.is_some();
        let after = tab_sleep_after();
        let active = self.active;
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            if !sleeps(
                index == active,
                tab.asleep,
                tab.used.elapsed(),
                working,
                after,
            ) {
                continue;
            }
            self.engine.close(tab.id);
            tab.asleep = true;
            // Its last picture goes too: a page's worth of pixels per tab.
            if let Some(frame) = tab.frame.take() {
                let _ = window.drop_image(frame);
            }
            cx.notify();
        }
    }

    /// A tab is hovered: measure what the pages hold, at most once in two seconds.
    pub(crate) fn measure_page_memory(&mut self) {
        let fresh = Duration::from_secs(2);
        if self.page_memory_asked.is_none_or(|at| at.elapsed() > fresh) {
            self.page_memory_asked = Some(Instant::now());
            self.engine.measure_memory();
        }
    }

    fn pump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active_id();
        let mut frame_ready = false;
        let mut console_changed = false;
        let mut opened = false;
        let mut tabs_changed = false;
        // Tabs behind the active one that built a frame.
        let mut behind = Vec::new();
        let mut closed = Vec::new();
        for event in self.engine.pump() {
            match event {
                EngineEvent::FrameReady(tab) if tab == active => frame_ready = true,
                EngineEvent::FrameReady(tab) => {
                    // Once a pump however many frames it built.
                    if !behind.contains(&tab) {
                        behind.push(tab);
                    }
                }
                EngineEvent::Closed(id) => closed.push(id),
                EngineEvent::Opened(id) => {
                    self.tabs
                        .push(Tab::new(id, self.space, String::new(), "".into()));
                    opened = true;
                }
                EngineEvent::Title(tab, title) => {
                    let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab) else {
                        continue;
                    };
                    // As onTitle in WebviewTab.tsx: an empty title is not a title for the history.
                    if !title.is_empty() {
                        let (tab_id, url) = (history_tab_id(tab.id), tab.url.clone());
                        self.stores.history.update(cx, |history, cx| {
                            history.update_title(&tab_id, &url, &title);
                            cx.notify();
                        });
                    }
                    tab.title = title.into();
                    tabs_changed = true;
                }
                EngineEvent::Favicon(tab, mut icon) => {
                    let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab) else {
                        continue;
                    };
                    // Kept on disk too: the bookmark bar and the history show it from there.
                    let file = rever_ui_history::history_panel::favicon_host(&tab.url)
                        .and_then(|host| rever_ui_history::history_panel::favicon_file(&host));
                    if let Some(file) = file {
                        let _ = icon.save(file);
                        self.views.bookmark_bar.update(cx, |_, cx| cx.notify());
                    }
                    // GPUI paints BGRA.
                    for pixel in icon.as_chunks_mut::<4>().0 {
                        pixel.swap(0, 2);
                    }
                    let icon = Arc::new(RenderImage::new(vec![Frame::new(icon)]));
                    if let Some(old) = tab.favicon.replace(icon) {
                        let _ = window.drop_image(old);
                    }
                }
                EngineEvent::Url(tab, url) => {
                    // Do not overwrite what the user is typing.
                    let address = &self.views.address;
                    if tab == active && !address.focus_handle(cx).is_focused(window) {
                        address.update(cx, |input, cx| input.set_text(url.clone(), cx));
                    }
                    let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab) else {
                        continue;
                    };
                    tab.url = url.clone();
                    tabs_changed = true;
                    let entry = NewHistoryEntry {
                        tab_id: history_tab_id(tab.id),
                        url,
                        title: String::new(),
                    };
                    self.stores.history.update(cx, |history, cx| {
                        history.push(entry);
                        cx.notify();
                    });
                }
                EngineEvent::Memory(report) => {
                    self.page_memory = report;
                    cx.notify();
                }
                EngineEvent::Console { level, message } => {
                    self.stores
                        .traffic
                        .lock()
                        .unwrap()
                        .append_console(ConsoleEntry {
                            ts: now_ms(),
                            kind: console_kind(&level).to_owned(),
                            text: message,
                            ..ConsoleEntry::default()
                        });
                    console_changed = true;
                }
            }
        }
        if frame_ready {
            self.capture(window);
        }
        let painted = frame_ready || !behind.is_empty();
        for tab in behind {
            self.engine.settle(tab);
        }
        // The grid view's pictures follow their pages, twice a second at most.
        if self.spaces_open && painted && self.space_shots_at.elapsed() > Duration::from_millis(500)
        {
            self.capture_spaces(window);
        }
        if tabs_changed {
            self.save_session();
        }
        // The debugger tools connect to Servo's debugging server once it is up.
        if self.debug_endpoint.lock().unwrap().is_none() {
            *self.debug_endpoint.lock().unwrap() = self.engine.devtools();
            if std::env::var_os("REVER_DEVTOOLS_DEBUG").is_some() {
                eprintln!("[devtools] {:?}", self.engine.devtools());
            }
        }
        for (_, control) in self.engine.take_controls() {
            self.answer_control(control, window, cx);
        }
        for id in closed {
            // The last tab stays: `close_tab` never closes it.
            if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                self.close_tab(index, window, cx);
            }
        }
        if opened {
            // As a browser does for a link that opens a window: show the new tab.
            self.select_tab(self.tabs.len() - 1, window, cx);
        }
        for result in self.eval_results.take() {
            let reply = result.and_then(|value| panels::eval_reply(&value));
            self.views
                .console
                .update(cx, |console, cx| console.finish_eval(reply, cx));
        }
        if console_changed {
            self.views.console.update(cx, |panel, cx| panel.refresh(cx));
            self.views
                .exceptions
                .update(cx, |panel, cx| panel.refresh(cx));
        }
        if self.traffic_changed.swap(false, Ordering::Relaxed) {
            // The CLI editor lists the captured requests.
            self.views.workflows.update(cx, |_, cx| cx.notify());
            self.views.traffic.update(cx, |panel, cx| panel.refresh(cx));
            self.views
                .websocket
                .update(cx, |panel, cx| panel.refresh(cx));
            self.views.apimap.update(cx, |panel, cx| panel.refresh(cx));
        }
        // The agent's macro tools write the workflows file from the MCP server's
        // thread: the panel reads it again when the file has changed.
        let modified = std::fs::metadata(panels::workflows_file())
            .and_then(|file| file.modified())
            .ok();
        if modified != self.workflows_modified {
            self.workflows_modified = modified;
            self.views.workflows_store.update(cx, |store, cx| {
                store.reload();
                cx.notify();
            });
        }
        if self.scan_spaces_due && !self.spaces_open {
            self.scan_spaces_due = false;
            self.toggle_spaces(window, cx);
            // A notify from this async context does not by itself schedule a
            // frame: without this, the opened grid never paints. request_
            // animation_frame would panic here (it is paint-phase only).
            window.refresh();
        }
        self.pump_agent(cx);
        self.pump_scan(cx);
        if self.close_scan_tabs {
            self.close_scan_tabs = false;
            let urls = std::mem::take(&mut self.scan_tabs);
            let mut index = self.tabs.len();
            while index > 0 {
                index -= 1;
                if self.tabs.len() > 1 && urls.contains(&self.tabs[index].url) {
                    self.close_tab(index, window, cx);
                }
            }
        }
        self.run_tool_commands(window, cx);
        cx.notify();
    }

    /// Paint the active tab and keep the result for the next render.
    fn capture(&mut self, window: &mut Window) {
        let Some(mut image) = self.engine.frame(self.active_id()) else {
            return;
        };
        if self.self_check.is_some() {
            self.last_rgba = Some(image.clone());
        }
        // GPUI paints BGRA.
        for pixel in image.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        self.frames += 1;
        let frame = Arc::new(RenderImage::new(vec![Frame::new(image)]));
        if let Some(old) = self.tabs[self.active].frame.replace(frame) {
            let _ = window.drop_image(old);
        }
    }

    /// Make the active page as large as the area the interface gave it.
    pub fn fit_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let bounds = page_area(self.page_bounds.get(), self.mobile);
        let scale = window.scale_factor();
        let device = |length: Pixels| (f32::from(length) * scale).round().max(1.0) as u32;
        let size = PhysicalSize::new(device(bounds.size.width), device(bounds.size.height));
        if self.engine.size(self.active_id()) != Some(size) {
            self.engine.resize(self.active_id(), size);
            self.pump(window, cx);
        }
    }

    pub fn navigate(&mut self, input: &str, cx: &mut Context<Self>) {
        let Ok(url) = Url::parse(&normalize_address(input)) else {
            return;
        };
        self.engine.load(self.active_id(), url);
        cx.notify();
    }

    /// A panel or a menu asked for this page: show it in the address bar and load it in
    /// the active tab, as the navigation request effect in App.tsx does.
    pub fn open_url(&mut self, url: &str, cx: &mut Context<Self>) {
        self.views
            .address
            .update(cx, |input, cx| input.set_text(url.to_owned(), cx));
        self.navigate(url, cx);
    }

    pub fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Ok(url) = Url::parse(NEW_TAB_URL) {
            self.open_tab(url, window, cx);
        }
    }

    /// Open `url` in a new tab and show it.
    pub fn open_tab(&mut self, url: Url, window: &mut Window, cx: &mut Context<Self>) {
        let size = self
            .engine
            .size(self.active_id())
            .unwrap_or(PhysicalSize::new(800, 600));
        let address = url.to_string();
        let Ok(id) = self.engine.open(url, size) else {
            return;
        };
        self.tabs.push(Tab::new(id, self.space, address, "".into()));
        self.select_tab(self.tabs.len() - 1, window, cx);
    }

    /// Open `url` in a new tab behind the shown one, as a wheel-button click on a link does.
    pub fn open_background_tab(&mut self, url: Url, cx: &mut Context<Self>) {
        let size = self
            .engine
            .size(self.active_id())
            .unwrap_or(PhysicalSize::new(800, 600));
        let address = url.to_string();
        let Ok(id) = self.engine.open(url, size) else {
            return;
        };
        self.tabs.push(Tab::new(id, self.space, address, "".into()));
        self.save_session();
        cx.notify();
    }

    pub fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        // The tab being left is idle from now on. When it was just closed there is
        // none: `active` still names its place, which may be past the end.
        if let Some(left) = self.tabs.get_mut(self.active) {
            left.used = Instant::now();
        }
        self.active = index;
        // The tab's space becomes the one shown, and remembers this tab.
        let (space, shown) = (self.tabs[index].space, self.tabs[index].id);
        self.space = space;
        if let Some(entry) = self.spaces.iter_mut().find(|entry| entry.id == space) {
            entry.shown = Some(shown);
        }
        self.wake_tab(index);
        let url = self.tabs[index].url.clone();
        self.views
            .address
            .update(cx, |input, cx| input.set_text(url, cx));
        self.fit_page(window, cx);
        self.capture(window);
        self.save_session();
        cx.notify();
    }

    /// Remember the open tabs for the next start. Written only when they changed.
    fn save_session(&mut self) {
        let tabs: Vec<SessionTab> = self
            .tabs
            .iter()
            .map(|tab| SessionTab {
                id: tab.id.0.to_string(),
                url: tab.url.clone(),
                title: tab.title.to_string(),
                web_contents_id: None,
                initial_url: String::new(),
                proxy: None,
                partition: None,
                profile_id: None,
            })
            .collect();
        let saved = serialize_tabs(&tabs, &self.active_id().0.to_string());
        if saved != self.saved_session {
            if let Err(error) = self.stores.storage.set(TABS_KEY, &saved) {
                eprintln!("rever: the open tabs were not saved: {error}");
            }
            self.saved_session = saved;
        }
        // The spaces, for the same tabs: the ones the line above saved have an address.
        let place = |space: SpaceId| {
            self.spaces
                .iter()
                .position(|entry| entry.id == space)
                .unwrap_or(0)
        };
        let spaces = space_rules::serialize_spaces(&SavedSpaces {
            names: self.spaces.iter().map(|space| space.name.clone()).collect(),
            of: self
                .tabs
                .iter()
                .filter(|tab| !tab.url.is_empty())
                .map(|tab| place(tab.space))
                .collect(),
            current: place(self.space),
        });
        if spaces != self.saved_spaces {
            if let Err(error) = self.stores.storage.set(space_rules::STORE_KEY, &spaces) {
                eprintln!("rever: the spaces were not saved: {error}");
            }
            self.saved_spaces = spaces;
        }
    }

    pub fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        // Never close the last tab, as in the tabs store.
        if self.tabs.len() == 1 || index >= self.tabs.len() {
            return;
        }
        let Some(neighbour) = space_rules::neighbour_after_close(&self.tab_spaces(), index) else {
            // The only tab of its space: the space closes with it.
            return self.close_space(self.tabs[index].space, window, cx);
        };
        self.drop_tab(index, window);
        // Closing the shown tab shows its neighbour in the space; closing another one
        // keeps the shown tab. Positions after `index` moved down by one.
        let keep = if index == self.active {
            neighbour
        } else {
            self.active
        };
        let next = if keep > index { keep - 1 } else { keep };
        self.select_tab(next, window, cx);
    }

    /// Take the tab at `index` out of the list and end its page.
    fn drop_tab(&mut self, index: usize, window: &mut Window) {
        let tab = self.tabs.remove(index);
        self.engine.close(tab.id);
        if is_web_url(&tab.url) {
            self.closed_tabs.push(tab.url.clone());
        }
        for image in [tab.frame, tab.favicon].into_iter().flatten() {
            let _ = window.drop_image(image);
        }
    }

    /// Each tab's space, in tab order.
    fn tab_spaces(&self) -> Vec<SpaceId> {
        self.tabs.iter().map(|tab| tab.space).collect()
    }

    /// The positions of the shown space's tabs: what the tab strip holds.
    pub fn space_tabs(&self) -> Vec<usize> {
        space_rules::tabs_of(&self.tab_spaces(), self.space)
    }

    /// Show `space`: the tab it showed last, else its first one.
    pub fn enter_space(&mut self, space: SpaceId, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self
            .spaces
            .iter()
            .find(|entry| entry.id == space)
            .and_then(|entry| entry.shown);
        let index = self
            .tabs
            .iter()
            .position(|tab| Some(tab.id) == shown)
            .or_else(|| self.tabs.iter().position(|tab| tab.space == space));
        if let Some(index) = index {
            self.select_tab(index, window, cx);
        }
    }

    /// Start a space with one new tab and show it.
    pub fn new_space(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let taken: Vec<String> = self.spaces.iter().map(|space| space.name.clone()).collect();
        let id = SpaceId(self.next_space);
        self.next_space += 1;
        self.spaces.push(Space {
            id,
            name: space_rules::new_space_name(&taken),
            shown: None,
        });
        // The new tab lands in the space that is shown.
        self.space = id;
        self.new_tab(window, cx);
    }

    /// Close a space with all its tabs. The last space stays, as the last tab does.
    pub fn close_space(&mut self, space: SpaceId, window: &mut Window, cx: &mut Context<Self>) {
        if self.spaces.len() <= 1 {
            return;
        }
        let shown = self.tabs.get(self.active).map(|tab| tab.id);
        for index in (0..self.tabs.len()).rev() {
            if self.tabs[index].space == space {
                self.drop_tab(index, window);
            }
        }
        self.spaces.retain(|entry| entry.id != space);
        if let Some(at) = self.space_shots.iter().position(|(of, _)| *of == space) {
            let _ = window.drop_image(self.space_shots.remove(at).1);
        }
        // The shown tab stays shown when it survived; else the first space takes over.
        match self.tabs.iter().position(|tab| Some(tab.id) == shown) {
            Some(index) => self.select_tab(index, window, cx),
            None => self.enter_space(self.spaces[0].id, window, cx),
        }
    }

    /// Open the grid view of the spaces, or go back from it to the space that is shown.
    pub fn toggle_spaces(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.spaces_open {
            return self.leave_spaces(self.space, window, cx);
        }
        self.spaces_open = true;
        window.focus(&self.spaces_focus);
        self.capture_spaces(window);
        // The page shrinks into its tile.
        self.spaces_zoom = Some((Instant::now(), self.space, true));
        cx.notify();
    }

    /// Go from the grid view into `space`: its tile grows to fill the window, then the
    /// space itself is shown.
    pub fn leave_spaces(&mut self, space: SpaceId, window: &mut Window, cx: &mut Context<Self>) {
        // Already on the way out.
        if matches!(self.spaces_zoom, Some((_, _, false))) {
            return;
        }
        self.spaces_zoom = Some((Instant::now(), space, false));
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SPACES_ZOOM).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.close_spaces(window);
                this.enter_space(space, window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Take the grid view away at once.
    pub fn close_spaces(&mut self, window: &mut Window) {
        self.spaces_open = false;
        self.spaces_zoom = None;
        // The pictures are a page's worth of pixels each: gone with the view.
        for (_, shot) in self.space_shots.drain(..) {
            let _ = window.drop_image(shot);
        }
        window.focus(&self.page_focus);
    }

    /// Take a picture of each space's page for the grid view. A sleeping page has
    /// none: its tile shows the page's title instead.
    fn capture_spaces(&mut self, window: &mut Window) {
        self.space_shots_at = Instant::now();
        for space in &self.spaces {
            let tab = self
                .tabs
                .iter()
                .find(|tab| Some(tab.id) == space.shown)
                .or_else(|| self.tabs.iter().find(|tab| tab.space == space.id));
            let Some(mut image) = tab
                .filter(|tab| !tab.asleep)
                .and_then(|tab| self.engine.frame(tab.id))
            else {
                continue;
            };
            // GPUI paints BGRA.
            for pixel in image.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            let shot = Arc::new(RenderImage::new(vec![Frame::new(image)]));
            match self.space_shots.iter_mut().find(|(of, _)| *of == space.id) {
                Some((_, old)) => {
                    let _ = window.drop_image(std::mem::replace(old, shot));
                }
                None => self.space_shots.push((space.id, shot)),
            }
        }
    }

    /// The picker's choice: remembered, and used from the next message on. A model of
    /// the same agent goes to the running session, which keeps the conversation;
    /// another agent starts its own session, which has not seen it.
    fn choose_model(
        &mut self,
        agent: AcpAgentId,
        model_id: &str,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        let same_agent = self.chat_agent == agent;
        self.chat_agent = agent;
        self.chat_model = Some((model_id.to_owned(), name.to_owned()));
        for (key, value) in [
            (AGENT_STORAGE_KEY, agent.as_str()),
            (MODEL_STORAGE_KEY, model_id),
            (MODEL_NAME_STORAGE_KEY, name),
        ] {
            if let Err(error) = self.stores.storage.set(key, value) {
                eprintln!("rever: {key} was not saved: {error}");
            }
        }
        // A session that cannot take the model is replaced at the next turn.
        let kept = same_agent
            && self
                .agent
                .as_mut()
                .is_none_or(|agent| agent.set_model(model_id).is_ok());
        if !kept {
            self.forget_agent();
        }
        cx.notify();
    }

    /// The first-run pick: the agent the chat talks to from now on.
    pub(crate) fn choose_agent(&mut self, agent: AcpAgentId, cx: &mut Context<Self>) {
        if self.chat_agent != agent {
            self.chat_agent = agent;
            // The other agent's model means nothing to this one.
            self.chat_model = None;
            for key in [MODEL_STORAGE_KEY, MODEL_NAME_STORAGE_KEY] {
                let _ = self.stores.storage.remove(key);
            }
            self.forget_agent();
        }
        if let Err(error) = self.stores.storage.set(AGENT_STORAGE_KEY, agent.as_str()) {
            eprintln!("rever: {AGENT_STORAGE_KEY} was not saved: {error}");
        }
        cx.notify();
    }

    /// A new tab from a menu, which has no window at hand: opened through the tools'
    /// command, then shown.
    pub(crate) fn open_tab_from_menu(&mut self, url: &str, cx: &mut Context<Self>) {
        let (answer, opened) = oneshot::channel();
        let _ = self
            .tool_tx
            .unbounded_send(bridge::Command::OpenTab(url.to_owned(), answer));
        let _ = self.wake.unbounded_send(());
        let (commands, wake) = (self.tool_tx.clone(), self.wake.clone());
        cx.spawn(async move |_, _| {
            if let Ok(Ok(index)) = opened.await {
                let (answer, _) = oneshot::channel();
                let _ = commands.unbounded_send(bridge::Command::SelectTab(index, answer));
                let _ = wake.unbounded_send(());
            }
        })
        .detach();
    }

    /// The first-run guide closed: the import offer, which waited for it, looks again.
    pub(crate) fn setup_changed(&mut self, cx: &mut Context<Self>) {
        self.browser_import.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// The menu's "Import from your browser".
    pub fn open_browser_import(&mut self, cx: &mut Context<Self>) {
        self.browser_import.update(cx, |modal, cx| modal.open(cx));
    }

    /// Cmd+Shift+T: open the most recently closed tab again.
    pub fn reopen_closed_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(url) = self.closed_tabs.pop().and_then(|url| Url::parse(&url).ok()) {
            self.open_tab(url, window, cx);
        }
    }

    pub fn toggle_panel(&mut self, panel: &'static str, cx: &mut Context<Self>) {
        let next = (self.open_panel != Some(panel)).then_some(panel);
        self.set_open_panel(next, cx);
    }

    /// Open, switch or close the bottom panel. Opening and closing slide; switching
    /// from one panel to another does not.
    pub fn set_open_panel(&mut self, panel: Option<&'static str>, cx: &mut Context<Self>) {
        if self.open_panel.is_some() != panel.is_some() {
            self.panel_slide = Some((Instant::now(), self.panel_drawn));
        }
        self.open_panel = panel;
        cx.notify();
    }

    /// The star in the address bar: bookmark the active page, or remove its bookmark.
    pub fn toggle_bookmark(&mut self, cx: &mut Context<Self>) {
        let tab = &self.tabs[self.active];
        if !is_web_url(&tab.url) {
            return;
        }
        let (url, title) = (tab.url.clone(), tab.title.to_string());
        self.stores.bookmarks.update(cx, |bookmarks, cx| {
            if bookmarks.bookmarks().iter().any(|mark| mark.url == url) {
                bookmarks.remove_by_url(&url);
            } else {
                bookmarks.add(&url, &title);
            }
            cx.notify();
        });
        cx.notify();
    }

    /// How many entries the panel holds, for its chip and its header.
    pub fn panel_count(&self, panel: &str, cx: &App) -> usize {
        let store = self.stores.traffic.lock().unwrap();
        let list = self.stores.traffic_state.read(cx);
        match panel {
            "traffic" => list.order.len(),
            "apimap" => list.endpoints.len(),
            "console" => store.console_count(),
            "exceptions" => store.exception_count(),
            "websocket" => store.web_socket_count(),
            "history" => self.stores.history.read(cx).entries().len(),
            _ => 0,
        }
    }

    fn cycle_layout(&mut self, cx: &mut Context<Self>) {
        self.layout = self.layout.next();
        if let Err(error) = self.stores.storage.set(LAYOUT_KEY, self.layout.name()) {
            eprintln!("rever: the layout choice was not saved: {error}");
        }
        place_popups(
            self.layout,
            &self.proxy_button,
            self.profile_menu.as_ref(),
            cx,
        );
        cx.notify();
    }

    fn cycle_theme(&mut self, cx: &mut Context<Self>) {
        self.theme_mode = next_theme(self.theme_mode);
        let stored = theme_to_stored(self.theme_mode);
        if let Err(error) = self
            .app_theme
            .set_mode(self.stores.storage.as_ref(), stored)
        {
            eprintln!("rever: the theme choice was not saved: {error}");
        }
        self.rebuild_views(cx);
    }

    fn toggle_lang(&mut self, cx: &mut Context<Self>) {
        self.lang = match self.lang {
            Lang::En => Lang::Ko,
            Lang::Ko => Lang::En,
        };
        self.rebuild_views(cx);
    }

    /// The panels take language and theme when they are made, so a change makes them
    /// again; what was typed in the address bar and the composer is carried over.
    fn rebuild_views(&mut self, cx: &mut Context<Self>) {
        let address = self.views.address.read(cx).text().to_owned();
        let draft = self.views.composer.read(cx).text().to_owned();
        self.views = Views::new(
            &self.stores,
            self.lang,
            self.theme_mode,
            self.storage_browser.clone(),
            self.tool_context.clone(),
            cx,
        );
        self.views
            .address
            .update(cx, |input, cx| input.set_text(address, cx));
        self.views
            .composer
            .update(cx, |input, cx| input.set_text(draft, cx));
        cx.notify();
    }

    /// Another panel hands text to the chat composer ("Ask about", "Send to chat").
    pub fn push_draft(&mut self, draft: &str, cx: &mut Context<Self>) {
        self.views.composer.update(cx, |input, cx| {
            let text = draft_text(input.text(), draft);
            input.set_text(text, cx);
        });
    }

    /// Run what was typed in the console panel in the active tab; `pump` hands the
    /// answer back to the panel.
    pub fn evaluate_console(&mut self, expression: &str) {
        let results = self.eval_results.clone();
        self.engine
            .evaluate(self.active_id(), expression.to_owned(), move |result| {
                results.borrow_mut().push(result)
            });
        // A closed tab answers at once: make sure a pump follows to deliver it.
        let _ = self.wake.unbounded_send(());
    }

    pub fn send_chat(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = text.trim();
        // One turn at a time, as the Electron composer does while a turn is streaming.
        if text.is_empty() || self.agent_busy {
            return;
        }
        self.chat.push(ChatLine {
            from_user: true,
            text: text.to_owned(),
            failed: false,
            tool: None,
        });
        self.views
            .composer
            .update(cx, |input, cx| input.set_text("", cx));
        // Sending is a move to the newest line, wherever the reader was.
        self.chat_scroll.scroll_to_bottom();
        // A session that has not seen the conversation gets it with the message.
        let earlier = &self.chat[..self.chat.len() - 1];
        let prompt = match self.agent {
            Some(_) => text.to_owned(),
            None => with_earlier(earlier, text),
        };
        if let Err(error) = self.start_turn(&prompt) {
            self.forget_agent();
            self.turn_failed(&error);
        }
        self.save_conversation();
        cx.notify();
    }

    fn turn_failed(&mut self, error: &str) {
        self.chat.push(ChatLine {
            from_user: false,
            text: format!("⚠ {}: {error}", self.lang.t("chat.turnFailed")),
            failed: true,
            tool: None,
        });
    }

    /// Start the CLI if needed and hand it the prompt.
    fn start_turn(&mut self, text: &str) -> Result<(), String> {
        self.start_agent_turn(text)?;
        // The empty assistant line the streamed reply fills in.
        self.chat.push(ChatLine {
            from_user: false,
            text: String::new(),
            failed: false,
            tool: None,
        });
        Ok(())
    }

    /// Spawn the agent if needed and send `text` as a turn, without touching the
    /// chat log — the Red Team dialog uses this so the scan streams into itself.
    fn start_agent_turn(&mut self, text: &str) -> Result<(), String> {
        if self.redteam_running {
            return Err("a red-team scan is running — stop it first".to_owned());
        }
        if self.agent.is_none() {
            // The same sandbox folder the Electron app gives its agents.
            let scratch = data_dir().join("agent-scratch");
            let wake = self.wake.clone();
            let mcp = self
                .mcp
                .as_ref()
                .map(|mcp| (mcp.url.as_str(), mcp.auth_header.as_str()));
            let model = self.chat_model.as_ref().map(|(id, _)| id.as_str());
            let profile = self.mcp.as_ref().map(|mcp| mcp.tool_profile().clone());
            let wake = move || {
                let _ = wake.unbounded_send(());
            };
            self.agent = Some(chat_agents::spawn_agent(
                self.chat_agent,
                model,
                &scratch,
                mcp,
                profile,
                wake,
            )?);
        }
        let agent = self.agent.as_mut().ok_or("no agent session")?;
        agent.send(text)?;
        self.agent_busy = true;
        self.turn_started = Some(Instant::now());
        Ok(())
    }

    /// Answer what MCP tools asked of the page since the last pump.
    /// A page opened a dialog or a picker. The agent's armed answer wins; a dialog
    /// that opens while the agent drives the page is accepted so its tools do not
    /// hang on it; anything else is the user's and gets the system's own window.
    fn answer_control(
        &mut self,
        control: EmbedderControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // shortcut: "the agent is acting" is "a tool ran within the last 3 seconds";
        // track tool calls from start to end if that guess proves wrong.
        let agent_acting = self
            .tool_acted
            .is_some_and(|at| at.elapsed() < Duration::from_secs(3));
        match control {
            EmbedderControl::SimpleDialog(dialog) => {
                let kind = match dialog {
                    SimpleDialog::Alert(_) => "alert",
                    SimpleDialog::Confirm(_) => "confirm",
                    SimpleDialog::Prompt(_) => "prompt",
                };
                // shortcut: the history keeps the last 200; raise it with the tool's limit.
                if self.dialog_log.len() >= 200 {
                    self.dialog_log.remove(0);
                }
                self.dialog_log.push(DialogRecord {
                    ts: now_ms(),
                    kind: kind.to_owned(),
                    message: dialog.message().to_owned(),
                    url: self.tabs[self.active].url.clone(),
                    answer: None,
                });
                let opened = self.dialog_log.len() - 1;
                let answered = |by: &str, accept| {
                    Some(DialogAnswer {
                        by: by.to_owned(),
                        accept,
                        at: now_ms(),
                    })
                };
                let armed = self.dialog_answer.take();
                if armed.is_some() || (agent_acting && self.dialog_auto_dismiss) {
                    let (accept, text) = armed.unwrap_or((true, None));
                    self.dialog_log[opened].answer = answered("agent", accept);
                    return answer_dialog(dialog, accept, text.as_deref());
                }
                // prompt() needs a text field the system dialog doesn't give: open
                // a GPUI input overlay instead (alert/confirm keep the native buttons).
                if let SimpleDialog::Prompt(prompt) = &dialog {
                    let initial = prompt.current_value().to_owned();
                    let input = cx.new(|cx| TextInput::new("", cx));
                    input.update(cx, |input, cx| input.set_text(initial, cx));
                    cx.subscribe(&input, |this, _, _: &Submitted, cx| {
                        this.confirm_prompt(true, cx)
                    })
                    .detach();
                    self.prompt = Some(PromptOverlay {
                        message: self.dialog_log[opened].message.clone(),
                        ts: self.dialog_log[opened].ts,
                        dialog,
                        input,
                    });
                    cx.notify();
                    return;
                }
                let buttons: &[&str] = match dialog {
                    SimpleDialog::Alert(_) => &["OK"],
                    _ => &["OK", "Cancel"],
                };
                // shortcut: the system prompt has no text field, so the user's
                // prompt() answers its default value; give it a text input when needed.
                let chosen = window.prompt(PromptLevel::Info, dialog.message(), None, buttons, cx);
                let ts = self.dialog_log[opened].ts;
                cx.spawn(async move |this, cx| {
                    let accept = chosen.await == Ok(0);
                    answer_dialog(dialog, accept, None);
                    let _ = this.update(cx, |this, _| {
                        // Found again by its time: the log may have been cleared or trimmed.
                        if let Some(record) = this.dialog_log.iter_mut().rev().find(|r| r.ts == ts)
                        {
                            record.answer = answered("user", accept);
                        }
                    });
                })
                .detach();
            }
            EmbedderControl::FilePicker(mut picker) => {
                if !self.upload_paths.is_empty() {
                    picker.select(&std::mem::take(&mut self.upload_paths));
                    return picker.submit();
                }
                if agent_acting {
                    return picker.dismiss();
                }
                let chosen = cx.prompt_for_paths(PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple: picker.allow_select_multiple(),
                    prompt: None,
                });
                cx.spawn(async move |_, _| match chosen.await {
                    Ok(Ok(Some(paths))) => {
                        picker.select(&paths);
                        picker.submit();
                    }
                    _ => picker.dismiss(),
                })
                .detach();
            }
            // The agent picks options with browser_select_option; a list its click
            // opened would only sit over the page.
            EmbedderControl::SelectElement(select) if !agent_acting => {
                self.select = Some(select);
                cx.notify();
            }
            // The agent doesn't right-click; a menu would only sit over the page.
            EmbedderControl::ContextMenu(menu) if !agent_acting => {
                self.context_menu = Some(menu);
                cx.notify();
            }
            EmbedderControl::ColorPicker(picker) if !agent_acting => {
                let hex = rgb_to_hex(picker.current_color().unwrap_or(RgbColor {
                    red: 0,
                    green: 0,
                    blue: 0,
                }));
                let input = cx.new(|cx| TextInput::new("#rrggbb", cx));
                input.update(cx, |input, cx| input.set_text(hex, cx));
                cx.subscribe(&input, |this, _, _: &Submitted, cx| {
                    this.submit_color(true, cx)
                })
                .detach();
                self.color_picker = Some(ColorOverlay { picker, input });
                cx.notify();
            }
            // Dropped, which dismisses it: the input method has no interface yet.
            _ => {}
        }
    }

    /// Close the prompt overlay: confirm with the typed text, or dismiss it.
    pub fn confirm_prompt(&mut self, accept: bool, cx: &mut Context<Self>) {
        let Some(overlay) = self.prompt.take() else {
            return;
        };
        let text = overlay.input.read(cx).text().to_owned();
        answer_dialog(overlay.dialog, accept, accept.then_some(text.as_str()));
        if let Some(record) = self
            .dialog_log
            .iter_mut()
            .rev()
            .find(|r| r.ts == overlay.ts)
        {
            record.answer = Some(DialogAnswer {
                by: "user".to_owned(),
                accept,
                at: now_ms(),
            });
        }
        cx.notify();
    }

    /// Close the color picker: on accept, set the typed `#rrggbb` (if it parses)
    /// before submitting; on cancel, submit the color unchanged.
    pub fn submit_color(&mut self, accept: bool, cx: &mut Context<Self>) {
        let Some(mut overlay) = self.color_picker.take() else {
            return;
        };
        if accept && let Some(color) = hex_to_rgb(overlay.input.read(cx).text()) {
            overlay.picker.select(Some(color));
        }
        overlay.picker.submit();
        cx.notify();
    }

    /// Answer a RendererBridge request (only "scan:launch" is defined): arm the
    /// Red Team dialog with the payload and open it; an autostart (loopback)
    /// target begins the scan at once. Runs on the main thread.
    fn answer_interface_ask(
        &mut self,
        op: &str,
        payload: serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Result<serde_json::Value, String> {
        if op != "scan:launch" {
            return Err(format!("the interface does not answer \"{op}\""));
        }
        let url = payload
            .get("url")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let pages = payload
            .get("pages")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(20)
            .clamp(1, 200) as u32;
        let intensity = payload
            .get("intensity")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("readonly");
        // Keep the dialog's vocabulary ("redteam"); start maps it to the
        // orchestrator's "full".
        let intensity = match intensity {
            "probe" => "probe",
            "full" | "redteam" => "redteam",
            _ => "readonly",
        };
        let autostart = payload
            .get("autostart")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if url.is_empty() {
            return Err("scan:launch without a url".to_owned());
        }
        if self.redteam_pos == (0.0, 0.0) {
            self.redteam_pos = (120.0, 90.0);
        }
        self.views
            .redteam_target
            .update(cx, |input, cx| input.set_text(url, cx));
        self.views
            .redteam_pages
            .update(cx, |input, cx| input.set_text(pages.to_string(), cx));
        self.redteam_intensity = intensity;
        self.redteam_pages = pages;
        self.redteam_open = true;
        let phase = if autostart {
            if self.agent_busy {
                return Err("an agent turn is already running — close it first".to_owned());
            }
            self.start_redteam(cx);
            "running"
        } else {
            "confirm"
        };
        cx.notify();
        Ok(serde_json::json!({ "phase": phase }))
    }

    /// Open the Red Team dialog, prefilling the target with the active page url.
    pub fn open_redteam(&mut self, cx: &mut Context<Self>) {
        let url = self.tabs[self.active].url.clone();
        self.views
            .redteam_target
            .update(cx, |input, cx| input.set_text(url, cx));
        if self.redteam_pos == (0.0, 0.0) {
            self.redteam_pos = (120.0, 90.0);
        }
        self.redteam_open = true;
        cx.notify();
    }

    pub fn close_redteam(&mut self, cx: &mut Context<Self>) {
        self.redteam_open = false;
        cx.notify();
    }

    pub fn set_redteam_intensity(&mut self, level: &'static str, cx: &mut Context<Self>) {
        self.redteam_intensity = level;
        cx.notify();
    }

    /// Move the dialog so the pointer keeps its grab offset within the header.
    pub fn on_redteam_drag(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        if let Some((dx, dy)) = self.redteam_drag {
            self.redteam_pos = (x - dx, y - dy);
            cx.notify();
        }
    }

    /// Resize the dialog while its bottom-right corner is dragged.
    pub fn on_redteam_resize(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        if let Some((sx, sy, sw, sh)) = self.redteam_size_drag {
            self.redteam_w = (sw + (x - sx)).clamp(360.0, 1100.0);
            self.redteam_h = (sh + (y - sy)).clamp(220.0, 900.0);
            cx.notify();
        }
    }

    /// Run the configured scan: the ported scan-store orchestrator (auth →
    /// discovery → per-tab workers → probes → report), streaming into the
    /// dialog. Worker turns are their own CLI sessions.
    pub fn start_redteam(&mut self, cx: &mut Context<Self>) {
        let target = self.views.redteam_target.read(cx).text().trim().to_owned();
        // A finished scan stays on screen (its report); Start replaces it.
        if target.is_empty() || self.agent_busy || self.redteam_running {
            return;
        }
        let pages = self
            .views
            .redteam_pages
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(self.redteam_pages)
            .clamp(1, 200);
        self.redteam_pages = pages;
        // Fresh-scan guarantee: findings persist per site, which cannot tell two
        // apps on the same origin apart — drop this site's old findings first.
        // (The TS also cleared the target host's cookies; the Rust auth bootstrap
        // registers a throwaway account anyway, so v1 leaves cookies alone.)
        let cleared =
            rever_tools_redteam::findings::clear_findings_for_site(&self.tool_context, &target);
        if cleared > 0 {
            eprintln!("[scan] dropped {cleared} old finding(s) for this site");
        }
        crate::agent::REDTEAM_SCAN.store(true, Ordering::Relaxed);
        // The scan's worker spaces open in the grid view as they arrive; the
        // wake brings the pump, which opens it where a window is at hand.
        self.scan_spaces_due = true;
        let _ = self.wake.unbounded_send(());
        self.scan_tabs.clear();
        self.scan = Some(crate::scan::ScanRun::new(
            &target,
            pages,
            // "redteam" is the dialog's name for the orchestrator's "full".
            crate::scan::orchestrator_intensity(self.redteam_intensity),
        ));
        self.redteam_running = true;
        self.redteam_log_open = true;
        // A 1s tick so the stats row's elapsed clock moves between scan events.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let running = this.update(cx, |this, cx| {
                    let running = this.redteam_running;
                    cx.notify();
                    running
                });
                if running.is_err() || !running.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
        self.pump_scan(cx);
        cx.notify();
    }

    /// Show the finished scan's report in its own window, refreshing (and
    /// focusing) the window when one is already open.
    pub fn open_report_window(&mut self, cx: &mut Context<Self>) {
        let Some(scan) = self.scan.as_ref() else {
            return;
        };
        if scan.report.is_empty() {
            return;
        }
        let target = scan.target().to_owned();
        let findings = scan.findings;
        let report = scan.report.clone();
        if let Some(handle) = self.redteam_report_win {
            let refreshed = handle.update(cx, |view, window, cx| {
                view.set(&target, findings, &report);
                window.activate_window();
                cx.notify();
            });
            if refreshed.is_ok() {
                return;
            }
        }
        let lang = self.lang;
        let mode = self.theme_mode;
        let bounds = Bounds::centered(None, size(px(760.0), px(880.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some(lang.t("scan.report.title").into()),
                        appears_transparent: false,
                        traffic_light_position: None,
                    }),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|_| {
                        crate::shell::report::ReportWindow::new(
                            &target, findings, &report, lang, mode,
                        )
                    })
                },
            )
            .expect("could not open the report window");
        self.redteam_report_win = Some(window);
        cx.notify();
    }

    /// Ask the running scan to stop; its next pump jumps to the report.
    pub fn stop_redteam(&mut self, cx: &mut Context<Self>) {
        if let Some(scan) = self.scan.as_mut() {
            scan.stop();
        }
        self.pump_scan(cx);
    }

    /// Advance the scan one tick: spawn what the phases need, execute the
    /// actions it returns (open/close its tabs), and finish it cleanly.
    fn pump_scan(&mut self, cx: &mut Context<Self>) {
        let Some(mut scan) = self.scan.take() else {
            return;
        };
        let scratch = data_dir().join("agent-scratch");
        let mcp = self
            .mcp
            .as_ref()
            .map(|mcp| (mcp.url.clone(), mcp.auth_header.clone()));
        // The scan's own model choice, when there is one.
        let model = self.redteam_model.as_ref().map(|(id, _)| id.clone());
        let wake = self.wake.clone();
        let mut spawn = move |prompt: &str| {
            let _ = prompt;
            let wake = wake.clone();
            let mcp = mcp
                .as_ref()
                .map(|(url, auth)| (url.as_str(), auth.as_str()));
            crate::agent::AgentSession::spawn(&scratch, mcp, model.as_deref(), move || {
                let _ = wake.unbounded_send(());
            })
        };
        // The URLs in the store, for the discovery scope follow-up.
        let captured: Vec<String> = self
            .stores
            .traffic
            .lock()
            .unwrap()
            .list(&ListFilter {
                limit: Some(500),
                ..ListFilter::default()
            })
            .into_iter()
            .map(|request| request.url.clone())
            .collect();
        for action in scan.pump(&mut spawn, &captured) {
            match action {
                crate::scan::ScanAction::OpenTab(url) => {
                    // A background tab, opened through the same command the
                    // MCP tools use; the answer (its index) is not needed.
                    let (answer, _) = oneshot::channel();
                    let _ = self
                        .tool_tx
                        .unbounded_send(bridge::Command::OpenTab(url.clone(), answer));
                    let _ = self.wake.unbounded_send(());
                    self.scan_tabs.push(url);
                }
                // Closing needs the window (image teardown); the wake loop
                // picks the flag up right after this pump.
                crate::scan::ScanAction::CloseScanTabs => self.close_scan_tabs = true,
            }
        }
        let was_running = self.redteam_running;
        self.redteam_running = scan.running();
        // Follow the activity log's newest line while the scan streams.
        if self.redteam_running {
            self.redteam_log_scroll.scroll_to_bottom();
        }
        // A finished scan lands its report in the report window — after the
        // scan is back in place (open_report_window reads self.scan).
        let finished_with_report = was_running && !scan.running() && !scan.report.is_empty();
        if !scan.running() {
            crate::agent::REDTEAM_SCAN.store(false, Ordering::Relaxed);
        }
        self.scan = Some(scan);
        if finished_with_report {
            // Deferred: opening mid-pump left the platform window half-drawn
            // (black, off-screen); opening on a later turn works.
            let browser = cx.entity();
            cx.defer(move |cx| browser.update(cx, |this, cx| this.open_report_window(cx)));
        }
        cx.notify();
    }

    /// Write the conversation on screen to the history store.
    fn save_conversation(&mut self) {
        // A self-check run is the port harness's, not a conversation to keep.
        if self.self_check.is_some() {
            return;
        }
        let messages = lines_to_messages(&self.chat);
        let saved = self.chat_history.save(
            self.stores.storage.as_ref(),
            &self.conversation_id,
            self.chat_agent,
            messages,
            now_ms(),
        );
        if let Err(error) = saved {
            eprintln!("rever: the conversation was not saved: {error}");
        }
    }

    /// Start an empty conversation; the one on screen stays in the history.
    pub fn new_conversation(&mut self, cx: &mut Context<Self>) {
        if self.agent_busy {
            return;
        }
        self.chat.clear();
        self.forget_agent();
        self.conversation_id = fresh_conversation_id();
        cx.notify();
    }

    /// Show a saved conversation. Its agent session is gone, so the next message
    /// starts one and tells it what was said (`with_earlier`).
    fn open_conversation(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.agent_busy {
            return;
        }
        let Some(saved) = self
            .chat_history
            .conversations()
            .iter()
            .find(|c| c.id == id)
        else {
            return;
        };
        self.chat = messages_to_lines(&saved.messages);
        self.conversation_id = saved.id.clone();
        self.forget_agent();
        self.chat_scroll.scroll_to_bottom();
        cx.notify();
    }

    fn remove_conversation(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Err(error) = self.chat_history.remove(self.stores.storage.as_ref(), id) {
            eprintln!("rever: the conversation was not removed: {error}");
        }
        // Removing the one on screen leaves an empty conversation.
        if id == self.conversation_id && !self.agent_busy {
            self.new_conversation(cx);
        }
        cx.notify();
    }

    /// The name of the agent the conversation runs on, for the chat panel's tab.
    pub fn chat_agent_name(&self) -> &'static str {
        let agent = rever_ui_state::constants::ACP_AGENTS
            .iter()
            .find(|agent| agent.id == self.chat_agent);
        agent.map_or("Chat", |agent| agent.name)
    }

    /// Fold the chat panel away, or bring it back at its own width.
    pub fn toggle_chat(&mut self, cx: &mut Context<Self>) {
        self.chat_collapsed = !self.chat_collapsed;
        self.chat_expanded = false;
        self.chat_slide = Some((Instant::now(), self.chat_drawn));
        let storage = self.stores.storage.as_ref();
        if let Err(error) = self
            .chat_collapsed_store
            .set_collapsed(storage, self.chat_collapsed)
        {
            eprintln!("rever: the chat panel's state was not saved: {error}");
        }
        cx.notify();
    }

    /// Let the chat panel cover the page, or give the page its room back.
    pub fn toggle_chat_expanded(&mut self, cx: &mut Context<Self>) {
        self.chat_expanded = !self.chat_expanded;
        self.chat_slide = Some((Instant::now(), self.chat_drawn));
        if std::mem::take(&mut self.chat_collapsed) {
            let _ = self
                .chat_collapsed_store
                .set_collapsed(self.stores.storage.as_ref(), false);
        }
        cx.notify();
    }

    /// Resize the chat panel while its left edge is dragged (dragging left, so a
    /// smaller x, makes it wider). The window's width limits it when it is drawn.
    pub fn on_chat_drag(&mut self, x: f32, cx: &mut Context<Self>) {
        if let Some((start_x, start_width)) = self.chat_drag {
            self.chat_width = (start_width + (start_x - x)).max(crate::shell::CHAT_MIN_WIDTH);
            cx.notify();
        }
    }

    /// Resize the open panel while its top edge is dragged (dragging up, so a
    /// smaller y, makes it taller).
    pub fn on_panel_drag(&mut self, y: f32, cx: &mut Context<Self>) {
        if let Some((start_y, start_height)) = self.panel_drag {
            self.panel_height = (start_height + (start_y - y)).clamp(160.0, 900.0);
            cx.notify();
        }
    }

    /// Close the context menu: run the action the user picked, or dismiss it.
    pub fn choose_context(&mut self, action: Option<ContextMenuAction>, cx: &mut Context<Self>) {
        let Some(menu) = self.context_menu.take() else {
            return;
        };
        match action {
            Some(action) => menu.select(action),
            None => menu.dismiss(),
        }
        cx.notify();
    }

    /// Close the open `<select>` list: with the option the user picked, or with
    /// none, which leaves the page's choice as it was.
    pub fn choose_option(&mut self, id: Option<usize>, cx: &mut Context<Self>) {
        let Some(mut select) = self.select.take() else {
            return;
        };
        match id {
            // `<select multiple>`: a click toggles the option and keeps the list
            // open; the outside press (id None) submits the accumulated set.
            Some(id) if select.allow_select_multiple() => {
                let mut chosen = select.selected_options();
                match chosen.iter().position(|&c| c == id) {
                    Some(at) => {
                        chosen.remove(at);
                    }
                    None => chosen.push(id),
                }
                select.select(chosen);
                self.select = Some(select);
            }
            Some(id) => {
                select.select(vec![id]);
                select.submit();
            }
            // Single select: an outside press leaves the page's choice as it was.
            // Multiple: it confirms what was toggled.
            None if select.allow_select_multiple() => select.submit(),
            None => {}
        }
        cx.notify();
    }

    fn run_tool_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use bridge::Command;
        let scale = f64::from(window.scale_factor());
        while let Ok(command) = self.tool_commands.try_recv() {
            // The app's own panels send commands too; only an agent's leave a mark.
            let command = match command {
                Command::ByAgent(command) => {
                    self.tool_acted = Some(Instant::now());
                    let driven = match &*command {
                        Command::Navigate(target, ..)
                        | Command::Evaluate(target, ..)
                        | Command::EvaluateIn(target, ..)
                        | Command::Screenshot(target, ..) => Some(*target),
                        Command::Back(_)
                        | Command::Forward(_)
                        | Command::Reload(_)
                        | Command::Input(..) => Some(None),
                        _ => None,
                    };
                    if let Some(target) = driven {
                        let index = target
                            .filter(|index| *index < self.tabs.len())
                            .unwrap_or(self.active);
                        self.tabs[index].agent_acted = Some(Instant::now());
                        self.watch_agent_marks(cx);
                    }
                    *command
                }
                command => command,
            };
            let tab = self.active_id();
            match command {
                Command::Page(target, answer) => {
                    let index = target.unwrap_or(self.active).min(self.tabs.len() - 1);
                    let page = &self.tabs[index];
                    let _ = answer.send(Ok(PageInfo {
                        url: page.url.clone(),
                        title: page.title.to_string(),
                    }));
                }
                Command::Navigate(target, url, answer) => {
                    let tab = self.resolve_tab(target);
                    let loaded = Url::parse(&normalize_address(&url))
                        .map(|url| self.engine.load(tab, url))
                        .map_err(|error| format!("invalid URL: {error}"));
                    let _ = answer.send(loaded);
                }
                Command::Back(answer) => {
                    self.engine.back(tab);
                    let _ = answer.send(Ok(()));
                }
                Command::Forward(answer) => {
                    self.engine.forward(tab);
                    let _ = answer.send(Ok(()));
                }
                Command::Reload(answer) => {
                    self.engine.reload(tab);
                    let _ = answer.send(Ok(()));
                }
                Command::Evaluate(target, script, answer) => {
                    let tab = self.resolve_tab(target);
                    self.engine.evaluate(tab, script, move |result| {
                        let _ = answer.send(result.map(|value| bridge::js_to_json(&value)));
                    })
                }
                Command::Screenshot(target, answer) => {
                    let tab = self.resolve_tab(target);
                    let png = self
                        .engine
                        .frame(tab)
                        .ok_or_else(|| "the page has not been painted yet".to_owned())
                        .and_then(|image| {
                            let mut bytes = Vec::new();
                            image
                                .write_to(
                                    &mut std::io::Cursor::new(&mut bytes),
                                    image::ImageFormat::Png,
                                )
                                .map(|_| bytes)
                                .map_err(|error| error.to_string())
                        });
                    let _ = answer.send(png);
                }
                Command::Input(input, answer) => {
                    for event in bridge::input_events(&input, scale) {
                        self.engine.input(tab, event);
                    }
                    let _ = answer.send(Ok(()));
                }
                Command::InterfaceAsk(op, payload, answer) => {
                    let _ = answer.send(self.answer_interface_ask(&op, payload, cx));
                }
                Command::Viewport(answer) => {
                    let size = self.engine.size(tab).map(|size| {
                        (
                            (f64::from(size.width) / scale).round() as u32,
                            (f64::from(size.height) / scale).round() as u32,
                        )
                    });
                    let _ = answer.send(size.ok_or_else(|| "the tab is closed".to_owned()));
                }
                Command::Tabs(answer) => {
                    let tabs = self.tabs.iter().enumerate().map(|(index, tab)| TabInfo {
                        url: tab.url.clone(),
                        title: tab.title.to_string(),
                        active: index == self.active,
                        id: self.engine.webview_u64(tab.id).unwrap_or(0),
                    });
                    let _ = answer.send(Ok(tabs.collect()));
                }
                Command::SelectTab(index, answer) => {
                    let selected = if index < self.tabs.len() {
                        self.select_tab(index, window, cx);
                        Ok(())
                    } else {
                        Err("could not select that tab".to_owned())
                    };
                    let _ = answer.send(selected);
                }
                Command::OpenTab(url, answer) => {
                    let size = self.engine.size(tab).unwrap_or(PhysicalSize::new(800, 600));
                    let opened = Url::parse(&normalize_address(&url))
                        .map_err(|error| format!("invalid URL: {error}"))
                        .and_then(|url| {
                            let address = url.to_string();
                            let id = self.engine.open(url, size)?;
                            // A scan worker's tab gets a space of its own (the
                            // ego-Lite shape: one live tile per worker in the
                            // grid, which dies with the tab when the scan ends).
                            let space = if self.redteam_running {
                                let id = SpaceId(self.next_space);
                                self.next_space += 1;
                                self.spaces.push(Space {
                                    id,
                                    name: crate::scan::short_loc(&address),
                                    shown: None,
                                });
                                id
                            } else {
                                self.space
                            };
                            // Not selected: the agent reads it by index while it loads.
                            self.tabs.push(Tab::new(id, space, address, "".into()));
                            Ok(self.tabs.len() - 1)
                        });
                    let _ = answer.send(opened);
                    cx.notify();
                }
                Command::Cookies(url, answer) => {
                    let cookies = Url::parse(&url)
                        .map_err(|error| format!("invalid URL: {error}"))
                        .map(|url| {
                            let cookies = self.engine.cookies(url);
                            cookies.iter().map(bridge::from_engine_cookie).collect()
                        });
                    let _ = answer.send(cookies);
                }
                Command::SetCookie(url, cookie, answer) => {
                    let stored = Url::parse(&url)
                        .map_err(|error| format!("invalid URL: {error}"))
                        .map(|url| {
                            self.engine
                                .set_cookie(url, bridge::to_engine_cookie(&cookie))
                        });
                    let _ = answer.send(stored);
                }
                Command::DialogHistory(limit, answer) => {
                    let from = self.dialog_log.len().saturating_sub(limit);
                    let recent = self.dialog_log[from..].to_vec();
                    let _ = answer.send(Ok((self.dialog_auto_dismiss, recent)));
                }
                Command::ClearDialogs(answer) => {
                    self.dialog_log.clear();
                    let _ = answer.send(Ok(()));
                }
                Command::SetDialogAutoDismiss(enabled, answer) => {
                    self.dialog_auto_dismiss = enabled;
                    let _ = answer.send(Ok(()));
                }
                Command::ArmDialog(accept, text, answer) => {
                    self.dialog_answer = Some((accept, text));
                    let _ = answer.send(Ok(()));
                }
                Command::ArmFiles(paths, answer) => {
                    self.upload_paths = paths.into_iter().map(PathBuf::from).collect();
                    let _ = answer.send(Ok(()));
                }
                Command::AddInitScript(source, answer) => {
                    let added = self.engine.add_init_script(tab, source);
                    let _ = answer.send(added.ok_or_else(|| "the tab is closed".to_owned()));
                }
                Command::RemoveInitScript(id, answer) => {
                    let removed = self.engine.remove_init_script(&id);
                    let _ = answer.send(
                        removed
                            .then_some(())
                            .ok_or_else(|| format!("no init script with id {id}")),
                    );
                }
                Command::Decide(request_id, decision, answer) => {
                    let held = self.held_requests.borrow_mut().remove(&request_id);
                    let decided = held
                        .map(|load| crate::intercept::answer(load, decision))
                        .ok_or_else(|| format!("no paused request {request_id}"));
                    let _ = answer.send(decided);
                }
                Command::SetViewport(mobile, answer) => {
                    self.mobile = mobile;
                    self.engine.set_mobile_user_agent(mobile);
                    self.fit_page(window, cx);
                    // The page has to ask again to be served the other layout.
                    self.engine.reload(tab);
                    cx.notify();
                    let _ = answer.send(Ok(()));
                }
                Command::WriteClipboard(text, answer) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    let _ = answer.send(Ok(()));
                }
                Command::EvaluateIn(target, frame, script, answer) => {
                    let tab = self.resolve_tab(target);
                    self.engine.evaluate_in(tab, frame, script, move |result| {
                        let _ = answer.send(result);
                    })
                }
                Command::FrameOf(target, parent, element, answer) => {
                    let tab = self.resolve_tab(target);
                    self.engine.frame_of(tab, parent, element, move |result| {
                        let _ = answer.send(result);
                    })
                }
                // Unwrapped above; a mark is never nested.
                Command::ByAgent(_) => {}
            }
        }
    }

    /// Apply what the agent said since the last pump.
    /// The user's answer to the tool call the prompt shows.
    fn answer_permission(&mut self, event: &PermissionPromptEvent, cx: &mut Context<Self>) {
        let grant = |grant_session: &bool| ApproveOptions {
            grant_session: *grant_session,
        };
        let mut store = self.permissions.borrow_mut();
        match event {
            PermissionPromptEvent::Respond {
                option_id,
                grant_session,
            } => store.respond_to_permission(option_id, grant(grant_session)),
            PermissionPromptEvent::Approve { grant_session } => {
                store.approve_current_permission(grant(grant_session))
            }
            PermissionPromptEvent::Reject => store.reject_current_permission(),
        }
        drop(store);
        self.permission_prompt.update(cx, |_, cx| cx.notify());
    }

    /// Stop the turn in progress. The agent's session ends with it; the next message
    /// starts a new one, which is handed the conversation so far.
    pub fn stop_chat(&mut self, cx: &mut Context<Self>) {
        if !self.agent_busy {
            return;
        }
        self.forget_agent();
        self.agent_busy = false;
        // Closes the tool rows still shown as running.
        apply_agent_event(&mut self.chat, &AgentEvent::Done);
        self.save_conversation();
        cx.notify();
    }

    /// The composer's chip: with auto-approve off, every tool call asks.
    pub fn toggle_auto_approve(&mut self, cx: &mut Context<Self>) {
        let mut store = self.permissions.borrow_mut();
        let on = !store.acp_auto_approve();
        store.set_acp_auto_approve(on);
        drop(store);
        cx.notify();
    }

    /// End the agent's session; what it still waited to be allowed is refused.
    fn forget_agent(&mut self) {
        self.agent = None;
        let mut store = self.permissions.borrow_mut();
        while store.current_permission().is_some() {
            store.reject_current_permission();
        }
    }

    fn pump_agent(&mut self, cx: &mut Context<Self>) {
        let Some(agent) = self.agent.as_mut() else {
            return;
        };
        // The lock: new lines pull the conversation down only for a reader who is at its
        // end. Someone who scrolled up to read keeps their place.
        let follow = shows_end(
            self.chat_scroll.offset().y.into(),
            self.chat_scroll.max_offset().height.into(),
        );
        let mut turn_ended = false;
        for event in agent.drain() {
            // A scan streams into its own dialog, not into the conversation.
            if !self.redteam_running {
                apply_agent_event(&mut self.chat, &event);
                if follow {
                    self.chat_scroll.scroll_to_bottom();
                }
            }
            match event {
                AgentEvent::Text(_) | AgentEvent::ToolCall(..) | AgentEvent::ToolResult(..) => {}
                AgentEvent::Permission(ask) => {
                    let waiting = self.permissions.borrow_mut().request_permission_from_user(
                        ask.request,
                        &RedTeamMode::new(),
                        ask.answer,
                    );
                    self.permission_prompt.update(cx, |_, cx| cx.notify());
                    // Unanswered for a minute, it is refused: the agent is not left hanging.
                    if let Some(id) = waiting {
                        cx.spawn(async move |this, cx| {
                            let wait = Duration::from_millis(ACP_PERMISSION_TIMEOUT_MS);
                            cx.background_executor().timer(wait).await;
                            let _ = this.update(cx, |this, cx| {
                                this.permissions.borrow_mut().expire(id);
                                this.permission_prompt.update(cx, |_, cx| cx.notify());
                            });
                        })
                        .detach();
                    }
                }
                AgentEvent::Done => {
                    self.agent_busy = false;
                    turn_ended = true;
                }
                AgentEvent::Failed(error) => {
                    // A failure outside a turn (the CLI exiting after a finished one) is not shown.
                    if self.agent_busy {
                        self.turn_failed(&error);
                        turn_ended = true;
                    }
                    self.agent_busy = false;
                    self.forget_agent();
                    break;
                }
            }
        }
        // The reply is kept once it is whole, not at every streamed piece.
        if turn_ended && !self.redteam_running {
            self.save_conversation();
        }
    }

    fn write_self_check(&self, dir: &str) {
        let _ = std::fs::create_dir_all(dir);
        let frame = self.last_rgba.as_ref();
        if let Some(frame) = frame {
            let _ = frame.save(format!("{dir}/frame.png"));
        }
        let store = self.stores.traffic.lock().unwrap();
        let requests: Vec<_> = store.list(&ListFilter {
            limit: Some(500),
            ..Default::default()
        });
        let tab = &self.tabs[self.active];
        let state = serde_json::json!({
            "title": tab.title.to_string(),
            "url": tab.url,
            "tabs": self.tabs.len(),
            "frame": frame.map(|f| [f.width(), f.height()]),
            // A blank frame has one color; a rendered page has several.
            "frameColors": frame.map(|f| {
                f.pixels().step_by(16).map(|p| p.0).collect::<std::collections::HashSet<_>>().len()
            }),
            "requests": requests,
            "mcpUrl": self.mcp.as_ref().map(|mcp| mcp.url.as_str()),
            "chat": self.chat.iter().filter(|line| line.tool.is_none()).map(|line| serde_json::json!({ "fromUser": line.from_user, "text": line.text })).collect::<Vec<_>>(),
        });
        let _ = std::fs::write(format!("{dir}/state.json"), state.to_string());
    }
}

/// The "⋮" menu, as App.tsx fills it. Items whose feature the Rust app does not have yet
/// (page theme, notifications, viewport, view source) are listed disabled.
/// The agents whose own CLIs report a model list, by the catalog's ids.
const LIVE_MODEL_AGENTS: [&str; 2] = ["claude-code", "codex"];
pub(crate) const AGENT_STORAGE_KEY: &str = "rever.chat.agent";
const MODEL_STORAGE_KEY: &str = "rever.chat.model";
const MODEL_NAME_STORAGE_KEY: &str = "rever.chat.modelName";

/// An id for a conversation that starts now.
fn fresh_conversation_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    // The id's random part only tells apart conversations started in one millisecond.
    new_conversation_id(now_ms(), f64::from(nanos) / 1e9)
}

fn history_props(browser: &WeakEntity<Browser>, cx: &App) -> ChatHistoryMenuProps {
    browser
        .read_with(cx, |this, _| ChatHistoryMenuProps {
            conversations: this
                .chat_history
                .conversations()
                .iter()
                .map(HistoryRow::from)
                .collect(),
            current_id: Some(this.conversation_id.clone()),
            // A conversation is not swapped out under a running turn.
            disabled: this.agent_busy,
            column: None,
        })
        .unwrap_or_default()
}

/// The chosen entry of an agent's model list, and what the chip reads for it.
fn model_names(
    this: &Browser,
    agent: AcpAgentId,
    chosen: Option<&(String, String)>,
) -> (Option<String>, Option<String>) {
    let chosen = chosen.map(|(_, name)| name.clone());
    let models = this.model_store.borrow().agent_models(agent);
    let chip = rever_ui_chat::model_picker::chip_model_name(chosen.as_deref(), &models);
    (chosen, chip)
}

fn picker_props(browser: &WeakEntity<Browser>, cx: &App) -> ModelPickerProps {
    let (agent_id, (model_name, chip_name), busy) = browser
        .read_with(cx, |this, _| {
            let names = model_names(this, this.chat_agent, this.chat_model.as_ref());
            (this.chat_agent, names, this.agent_busy)
        })
        .unwrap_or((AcpAgentId::ClaudeCode, (None, None), false));
    ModelPickerProps {
        agent_id,
        model_name,
        chip_name,
        // One turn at a time: the model changes between turns.
        disabled: busy,
        new_tab: false,
        column: None,
    }
}

/// The scan dialog's model chip: the scan's own choice over the CLI's models.
fn scan_picker_props(browser: &WeakEntity<Browser>, cx: &App) -> ModelPickerProps {
    let ((model_name, chip_name), running) = browser
        .read_with(cx, |this, _| {
            let names = model_names(this, AcpAgentId::ClaudeCode, this.redteam_model.as_ref());
            (names, this.redteam_running)
        })
        .unwrap_or(((None, None), false));
    ModelPickerProps {
        agent_id: AcpAgentId::ClaudeCode,
        model_name,
        chip_name,
        // A running scan already spawned its sessions on a model.
        disabled: running,
        new_tab: false,
        column: None,
    }
}

fn menu_props(browser: &WeakEntity<Browser>, cx: &App) -> AppMenuProps {
    let (theme_mode, lang, layout) = browser
        .read_with(cx, |this, _| (this.theme_mode, this.lang, this.layout))
        .unwrap_or((ThemeMode::Light, Lang::En, Layout::default()));
    let t = |key| lang.t(key);
    let mode = theme_name(theme_mode);
    let act = |action: fn(&mut Browser, &mut Context<Browser>)| {
        let browser = browser.clone();
        move |_: &mut Window, cx: &mut App| {
            let _ = browser.update(cx, action);
        }
    };
    let not_yet = |key| AppMenuItem::new(t(key), |_, _| {}).disabled(true);
    AppMenuProps {
        title: t("menu.title").into(),
        footer: Some(format!("Rever Browser v{}", env!("CARGO_PKG_VERSION")).into()),
        items: vec![
            AppMenuItem::new(t("menu.theme"), act(Browser::cycle_theme))
                .hint(t(match theme_mode {
                    ThemeMode::System => "menu.theme.system",
                    ThemeMode::Light => "menu.theme.light",
                    ThemeMode::Dark => "menu.theme.dark",
                }))
                .title(fill(t("toolbar.theme"), &[("mode", mode)])),
            AppMenuItem::new(t("menu.layout"), act(Browser::cycle_layout)).hint(t(match layout {
                Layout::TabsOnTop => "menu.layout.top",
                Layout::TabsOnSide => "menu.layout.side",
                Layout::Sidebar => "menu.layout.sidebar",
            })),
            not_yet("menu.pageTheme"),
            AppMenuItem::new(t("lang.label"), act(Browser::toggle_lang)).hint(match lang {
                Lang::En => "English",
                Lang::Ko => "한국어",
            }),
            not_yet("menu.notifications"),
            not_yet("menu.viewport")
                .hint(t("toolbar.viewport.desktop"))
                .sep(),
            not_yet("menu.viewSource")
                .title(t("toolbar.viewSource"))
                .sep(),
            AppMenuItem::new(t("import.title"), act(Browser::open_browser_import)),
        ],
    }
}

fn answer_dialog(dialog: SimpleDialog, accept: bool, text: Option<&str>) {
    match dialog {
        SimpleDialog::Prompt(mut prompt) if accept => {
            if let Some(text) = text {
                prompt.set_current_value(text);
            }
            prompt.confirm();
        }
        dialog if accept => dialog.confirm(),
        dialog => dialog.dismiss(),
    }
}

/// Where the page is drawn inside the area the interface gave it: all of it, or
/// in mobile mode a phone-wide column in the middle.
///
/// shortcut: mobile mode keeps the window's height and pixel density, where the
/// TypeScript emulated 390x844 at 3x; add those when a site needs the exact device.
pub fn page_area(bounds: Bounds<Pixels>, mobile: bool) -> Bounds<Pixels> {
    if !mobile {
        return bounds;
    }
    let width = bounds.size.width.min(gpui::px(crate::engine::MOBILE_WIDTH));
    let mut area = bounds;
    area.origin.x += (bounds.size.width - width) / 2.0;
    area.size.width = width;
    area
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG: Duration = Duration::from_secs(3600);

    #[test]
    fn 도구_설명문에_일렉트론이라는_말이_없다() {
        // The Electron app is gone, so a description that explains itself by
        // Electron's behaviour can only mislead the agent reading it.
        let defs = tool_registry().defs();
        assert!(defs.len() > 150, "only {} tools registered", defs.len());
        for def in defs {
            assert!(!def.description.contains("Electron"), "{}", def.name);
            assert!(!def.description.contains("webContents"), "{}", def.name);
        }
    }

    #[test]
    fn 오래_쓰지_않은_뒤_탭은_잠든다() {
        assert!(sleeps(false, false, LONG, false, TAB_SLEEP_AFTER));
    }

    #[test]
    fn 보고_있는_탭은_오래_두어도_잠들지_않는다() {
        assert!(!sleeps(true, false, LONG, false, TAB_SLEEP_AFTER));
    }

    #[test]
    fn 방금_쓴_탭은_잠들지_않는다() {
        let idle = TAB_SLEEP_AFTER - Duration::from_secs(1);
        assert!(!sleeps(false, false, idle, false, TAB_SLEEP_AFTER));
    }

    #[test]
    fn 에이전트나_스캔이_일하는_동안에는_아무_탭도_잠들지_않는다() {
        assert!(!sleeps(false, false, LONG, true, TAB_SLEEP_AFTER));
    }

    #[test]
    fn 방금_도구가_건드린_탭은_에이전트_작업_중이다() {
        let now = Instant::now();
        assert!(agent_on_tab(Some(now), None, false, now));
    }

    #[test]
    fn 건드린_지_오래됐거나_건드린_적_없는_탭은_작업_중이_아니다() {
        let now = Instant::now();
        assert!(!agent_on_tab(None, None, false, now));
        assert!(!agent_on_tab(Some(now), None, false, now + AGENT_RECENT));
    }

    #[test]
    fn 대화_턴이_도는_동안은_이번_턴에_건드린_탭이_계속_작업_중이다() {
        let turn = Instant::now();
        let touched = turn + Duration::from_secs(1);
        let later = touched + Duration::from_secs(600);
        assert!(agent_on_tab(Some(touched), Some(turn), false, later));
        // 지난 턴에 건드린 탭은 이번 턴의 것이 아니다.
        assert!(!agent_on_tab(Some(turn), Some(touched), false, later));
    }

    #[test]
    fn 스캔_워커가_맡은_탭은_건드리지_않아도_작업_중이다() {
        assert!(agent_on_tab(None, None, true, Instant::now()));
    }

    #[test]
    fn 이미_잠든_탭은_다시_재우지_않는다() {
        assert!(!sleeps(false, true, LONG, false, TAB_SLEEP_AFTER));
    }

    #[test]
    fn 시작할_때는_주소_인자가_먼저고_없으면_지난번_탭을_다시_연다() {
        let saved = r#"{"tabs":[{"url":"https://a.test/","title":"A"},{"url":"not a url"},{"url":"https://c.test/x","title":"C"}],"activeIndex":2}"#;
        let urls = |pages: &[(Url, String)]| -> Vec<String> {
            pages.iter().map(|(url, _)| url.to_string()).collect()
        };
        // 지난번 탭: 읽을 수 없는 주소는 빼고, 보던 탭을 그대로 고른다.
        let (pages, active) = start_pages(None, Some(saved));
        assert_eq!(urls(&pages), ["https://a.test/", "https://c.test/x"]);
        assert_eq!((active, pages[1].1.as_str()), (1, "C"));
        // 주소 인자가 있으면 그것만 연다.
        let asked = Url::parse("https://asked.test/").unwrap();
        let (pages, active) = start_pages(Some(asked), Some(saved));
        assert_eq!(
            (urls(&pages), active),
            (vec!["https://asked.test/".to_owned()], 0)
        );
        // 저장된 것이 없거나 쓸 수 없으면 홈 페이지다.
        for nothing in [None, Some("{}"), Some(r#"{"tabs":[{"url":"??"}]}"#)] {
            let (pages, active) = start_pages(None, nothing);
            assert_eq!((urls(&pages), active), (vec![DEFAULT_URL.to_owned()], 0));
        }
        // 보던 탭 번호가 범위를 넘으면 마지막 탭이다.
        let far =
            r#"{"tabs":[{"url":"https://a.test/"},{"url":"https://b.test/"}],"activeIndex":9}"#;
        assert_eq!(start_pages(None, Some(far)).1, 1);
    }

    #[test]
    fn 웹소켓_이벤트는_연결을_항목으로_프레임을_붙여_기록한다() {
        let store = Mutex::new(TrafficStore::default());
        record_ws(
            &store,
            NetworkTapEvent::WebSocketOpened {
                id: "ws-1".to_owned(),
                url: "ws://127.0.0.1:8779/ws".parse().unwrap(),
                webview_id: Some(7),
            },
        );
        let entry = {
            let store = store.lock().unwrap();
            let list = store.list(&ListFilter::default());
            (list.len(), list[0].resource_type.clone())
        };
        assert_eq!(entry, (1, "WebSocket".to_owned()));

        record_ws(
            &store,
            NetworkTapEvent::WebSocketFrame {
                id: "ws-1".to_owned(),
                outgoing: true,
                binary: false,
                payload: b"hello".to_vec(),
            },
        );
        record_ws(
            &store,
            NetworkTapEvent::WebSocketFrame {
                id: "ws-1".to_owned(),
                outgoing: false,
                binary: true,
                payload: vec![1, 2],
            },
        );
        let frames = store
            .lock()
            .unwrap()
            .ws_frames("ws-1", None)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(frames.len(), 2);
        assert_eq!(
            (
                frames[0].direction,
                frames[0].opcode,
                frames[0].payload_data.as_str()
            ),
            (WsDirection::Sent, 1, "hello")
        );
        assert_eq!(
            (frames[1].direction, frames[1].opcode),
            (WsDirection::Received, 2)
        );
    }

    #[test]
    fn 레이아웃은_위_옆_사이드바_순서로_돌고_저장값과_오간다() {
        assert_eq!(Layout::TabsOnTop.next(), Layout::TabsOnSide);
        assert_eq!(Layout::TabsOnSide.next(), Layout::Sidebar);
        assert_eq!(Layout::Sidebar.next(), Layout::TabsOnTop);
        for layout in [Layout::TabsOnTop, Layout::TabsOnSide, Layout::Sidebar] {
            assert_eq!(Layout::from_name(Some(layout.name())), layout);
        }
    }

    #[test]
    fn 저장된_레이아웃이_없거나_모르는_값이면_탭이_위에_있다() {
        assert_eq!(Layout::from_name(None), Layout::TabsOnTop);
        assert_eq!(Layout::from_name(Some("floating")), Layout::TabsOnTop);
    }

    #[test]
    fn 탭이_위에_있을_때만_사이드바가_없다() {
        assert!(!Layout::TabsOnTop.has_sidebar());
        assert!(Layout::TabsOnSide.has_sidebar());
        assert!(Layout::Sidebar.has_sidebar());
    }

    #[test]
    fn 테마는_시스템_라이트_다크_순서로_돌고_저장값과_오간다() {
        assert_eq!(next_theme(ThemeMode::System), ThemeMode::Light);
        assert_eq!(next_theme(ThemeMode::Light), ThemeMode::Dark);
        assert_eq!(next_theme(ThemeMode::Dark), ThemeMode::System);
        for mode in [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark] {
            assert_eq!(theme_from_stored(theme_to_stored(mode)), mode);
        }
        assert_eq!(theme_name(ThemeMode::System), "system");
    }

    #[test]
    fn http와_https_주소만_북마크할_수_있다() {
        assert!(is_web_url("https://example.com/"));
        assert!(is_web_url("HTTP://EXAMPLE.COM"));
        assert!(!is_web_url("about:blank"));
        assert!(!is_web_url("view-source:https://example.com/"));
        assert!(!is_web_url(""));
    }

    #[test]
    fn 채팅_링크는_http와_https_주소만_탭으로_연다() {
        assert_eq!(
            link_target("https://example.com/a?b=1").map(String::from),
            Some("https://example.com/a?b=1".to_owned())
        );
        assert!(link_target("http://127.0.0.1:8777/").is_some());
        assert!(link_target("/relative").is_none());
        assert!(link_target("javascript:alert(1)").is_none());
        assert!(link_target("file:///etc/passwd").is_none());
        assert!(link_target("mailto:dev@example.com").is_none());
    }

    #[test]
    fn 빈_입력란에는_초안이_줄바꿈까지_그대로_들어간다() {
        assert_eq!(
            draft_text("", "Analyze this:\n\nQuestion: "),
            "Analyze this:\n\nQuestion: "
        );
    }

    #[test]
    fn 쓰던_글이_있으면_초안은_다음_줄에_붙는다() {
        assert_eq!(draft_text("already typed", "more"), "already typed\nmore");
    }

    #[test]
    fn 서보의_warn은_cdp의_warning으로_기록한다() {
        assert_eq!(console_kind("warn"), "warning");
        assert_eq!(console_kind("error"), "error");
        assert_eq!(console_kind("log"), "log");
    }

    #[test]
    fn 도구_호출은_대화에_한_줄씩_남고_뒤의_글은_새_답으로_시작한다() {
        let mut chat = vec![ChatLine {
            from_user: false,
            text: String::new(),
            failed: false,
            tool: None,
        }];
        let call = |name: &str| AgentEvent::ToolCall(name.into(), serde_json::Value::Null);
        apply_agent_event(&mut chat, &AgentEvent::Text("확인".into()));
        apply_agent_event(&mut chat, &AgentEvent::Text("합니다".into()));
        apply_agent_event(&mut chat, &call("mcp__rever-traffic__list_requests"));
        apply_agent_event(&mut chat, &AgentEvent::Text("결과".into()));
        let lines: Vec<(&str, Option<bool>)> = chat
            .iter()
            .map(|line| (line.text.as_str(), line.tool))
            .collect();
        assert_eq!(
            lines,
            [
                ("확인합니다", None),
                ("list_requests", Some(false)),
                ("결과", None)
            ]
        );
        // 결과가 오면 그 도구만 끝난 것으로 바뀐다.
        apply_agent_event(&mut chat, &call("browser_click"));
        apply_agent_event(
            &mut chat,
            &AgentEvent::ToolResult("mcp__rever-traffic__list_requests".into(), String::new()),
        );
        assert_eq!(chat[1].tool, Some(true));
        assert_eq!(chat[3].tool, Some(false));
        // 턴이 끝나면 남은 도구도 모두 끝난 것으로 본다.
        apply_agent_event(&mut chat, &AgentEvent::Done);
        assert_eq!(chat[3].tool, Some(true));
    }

    #[test]
    fn 대화는_저장했다가_그대로_다시_읽힌다() {
        let line = |from_user, text: &str, failed, tool| ChatLine {
            from_user,
            text: text.into(),
            failed,
            tool,
        };
        let chat = vec![
            line(true, "요청 수 알려줘", false, None),
            line(false, "", false, None),
            line(false, "list_requests", false, Some(false)),
            line(false, "**1건**입니다", false, None),
            line(false, "⚠ 턴 실패: 끊김", true, None),
        ];
        let messages = lines_to_messages(&chat);
        // 빈 답 줄은 저장하지 않고, 첫 사용자 글이 제목이 될 수 있는 모양이다.
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["parts"][0]["text"], "요청 수 알려줘");

        let restored = messages_to_lines(&messages);
        let seen: Vec<(bool, &str, bool, Option<bool>)> = restored
            .iter()
            .map(|line| (line.from_user, line.text.as_str(), line.failed, line.tool))
            .collect();
        assert_eq!(
            seen,
            [
                (true, "요청 수 알려줘", false, None),
                // 돌던 도구는 다시 켠 뒤에는 끝난 것으로 보인다.
                (false, "list_requests", false, Some(true)),
                (false, "**1건**입니다", false, None),
                (false, "⚠ 턴 실패: 끊김", true, None),
            ]
        );
    }

    #[test]
    fn 새_세션에는_앞_대화를_붙여서_보낸다() {
        let line = |from_user, text: &str, tool| ChatLine {
            from_user,
            text: text.into(),
            failed: false,
            tool,
        };
        // 앞 대화가 없으면 보낸 글 그대로다.
        assert_eq!(with_earlier(&[], "안녕"), "안녕");
        let earlier = [
            line(true, "내 이름은 태균", None),
            line(false, "list_requests", Some(true)),
            line(false, "반갑습니다", None),
        ];
        let prompt = with_earlier(&earlier, "내 이름이 뭐지?");
        assert!(prompt.contains("User: 내 이름은 태균"));
        assert!(prompt.contains("Assistant: 반갑습니다"));
        assert!(!prompt.contains("list_requests"), "도구 줄은 넣지 않는다");
        assert!(prompt.ends_with("내 이름이 뭐지?"));

        // 긴 대화는 최근 부분만 들어간다.
        let long = [
            line(true, &"가".repeat(20_000), None),
            line(false, "마지막 답", None),
        ];
        let prompt = with_earlier(&long, "질문");
        assert!(prompt.chars().count() < EARLIER_LIMIT + 200);
        assert!(prompt.contains("마지막 답"));
    }

    #[test]
    fn 끝에서_몇_줄_안이면_끝을_보고_있는_것이다() {
        // 내용 높이가 화면보다 600 크다: 맨 아래의 offset은 -600.
        assert!(shows_end(-600.0, 600.0));
        assert!(shows_end(-570.0, 600.0));
        assert!(
            !shows_end(-300.0, 600.0),
            "위로 올려 읽는 중이면 따라가지 않는다"
        );
        // 스크롤할 것이 없는 짧은 대화는 늘 끝이다.
        assert!(shows_end(0.0, 0.0));
    }

    #[test]
    fn 히스토리의_탭_아이디는_t와_번호다() {
        assert_eq!(history_tab_id(TabId(3)), "t3");
    }
}
