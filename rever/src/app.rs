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
use gpui::{
    App, AppContext, Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    PathPromptOptions, Pixels, PromptLevel, RenderImage, SharedString, WeakEntity, Window,
};
use image::{Frame, RgbaImage};
use net::tap::NetworkTapEvent;
use rever_core::address::normalize_address;
use rever_core::paths::user_data_dir;
use rever_core::traffic::{self, ConsoleEntry, Headers, ListFilter, RequestPatch, TrafficStore};
use rever_mcp::McpServer;
use rever_tools_api::tool::{
    DialogAnswer, DialogRecord, PageInfo, TabInfo, ToolContext, ToolRegistry,
};
use rever_tools_debug::debugger::DebugState;
use rever_tools_debug::intercept::InterceptState;
use rever_tools_page::downloads::DownloadState;
use rever_ui_history::history::NewHistoryEntry;
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::ThemeMode;
use rever_ui_menus::app_menu::{AppMenu, AppMenuItem, AppMenuProps};
use rever_ui_menus::bot_check_button::BotCheckButton;
use rever_ui_menus::ip_badge::IpBadge;
use rever_ui_menus::ui::{Look, LookFn, fill};
use rever_ui_state::app_theme::{self, AppThemeStore};
use servo::{EmbedderControl, JSValue, SelectElement, SimpleDialog};
use url::Url;

use crate::agent::{AgentEvent, AgentSession};
use crate::bridge;
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

fn now_ms() -> u64 {
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
/// A failure costs the agent its tools, not the browser: it is logged and the app goes on.
fn start_mcp(
    traffic: Arc<Mutex<TrafficStore>>,
    browser: Arc<bridge::AppBrowser>,
    debug_endpoint: crate::debug::Endpoint,
) -> (Option<McpServer>, Arc<InterceptState>, Arc<DownloadState>) {
    let mut registry = ToolRegistry::default();
    rever_tools_traffic::register(&mut registry);
    rever_tools_inspect::register(&mut registry);
    rever_tools_scripts::register(&mut registry);
    rever_tools_browser::register(&mut registry);
    rever_tools_page::register(&mut registry);
    rever_tools_debug::register(&mut registry);
    rever_tools_redteam::register(&mut registry);
    rever_tools_automation::register(&mut registry);
    let data_dir = data_dir();
    // The server publishes its endpoint file here.
    let _ = std::fs::create_dir_all(&data_dir);
    let context = Arc::new(ToolContext::new(traffic, browser.clone(), data_dir));
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
        browser,
        edits,
        state: Arc::downgrade(&intercept),
    }));
    let server = rever_mcp::start(Arc::new(registry), context)
        .inspect_err(|error| eprintln!("[mcp] {error}"))
        .ok();
    (server, intercept, downloads)
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
/// in ChatPanel.tsx. The composer is a single-line field, so line breaks become spaces,
/// as they do when text is pasted into it.
fn draft_text(previous: &str, draft: &str) -> String {
    let joined = if previous.is_empty() {
        draft.to_owned()
    } else {
        format!("{previous}\n{draft}")
    };
    joined.replace('\n', " ")
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
        NetworkTapEvent::Request {
            id,
            url,
            method,
            headers: h,
            body,
            destination,
        } => RequestPatch {
            request_id: id,
            url: Some(url.to_string()),
            method: Some(method.to_string()),
            resource_type: Some(traffic::resource_type_of(&destination).to_owned()),
            request_headers: Some(headers(h.iter().map(|(n, v)| (n.as_str(), v.as_bytes())))),
            request_post_data: (!body.is_empty())
                .then(|| String::from_utf8_lossy(&body).into_owned()),
            ..RequestPatch::default()
        },
        NetworkTapEvent::Response {
            id,
            url,
            status,
            headers: h,
            body,
        } => {
            let response_headers = headers(h.iter().map(|(n, v)| (n.as_str(), v.as_bytes())));
            let mime = traffic::mime_of(&response_headers);
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
                let is_navigation = store
                    .lock()
                    .unwrap()
                    .get(&id)
                    .is_some_and(|request| request.resource_type == "Document");
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

pub(crate) struct Tab {
    pub id: TabId,
    pub title: SharedString,
    pub url: String,
    /// The latest page frame, in the BGRA layout GPUI paints.
    pub frame: Option<Arc<RenderImage>>,
}

pub(crate) struct ChatLine {
    pub from_user: bool,
    pub text: String,
    /// A turn that failed: drawn as an error, not as the agent's Markdown.
    pub failed: bool,
}

/// What the command line asked for.
pub(crate) struct Launch {
    pub url: Url,
    pub open_panel: Option<&'static str>,
    pub self_check: Option<String>,
    /// A chat message to send shortly after start (port harness).
    pub say: Option<String>,
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
    /// The text fields and panels; made again when the language or the theme changes.
    pub views: Views,
    pub menu: Entity<AppMenu>,
    pub ip_badge: Entity<IpBadge>,
    pub bot_check: Entity<BotCheckButton>,
    /// Set by the network tap's threads when the capture store changed.
    traffic_changed: Arc<AtomicBool>,
    /// Answers to what the console panel ran in the page, collected until the next pump.
    eval_results: Rc<RefCell<Vec<Result<JSValue, String>>>>,
    /// The Claude Code CLI, started with the first chat message.
    agent: Option<AgentSession>,
    pub agent_busy: bool,
    /// How many init scripts tools have added; the count is the next script's id.
    /// Wakes `pump` from other threads.
    wake: UnboundedSender<()>,
    /// Requests from MCP tools, answered in `pump`.
    tool_commands: UnboundedReceiver<bridge::Command>,
    /// Serves the tools to the agent CLI and to outside clients (mcp-endpoint.json);
    /// `None` when it could not start. Dropping it stops the server.
    mcp: Option<McpServer>,
    pub page_focus: FocusHandle,
    pub lang: Lang,
    pub theme_mode: ThemeMode,
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
    /// The `<select>` whose option list is open over the page.
    pub select: Option<SelectElement>,
    /// Where the debugger tools find Servo's debugging server.
    debug_endpoint: crate::debug::Endpoint,
    /// Mobile mode: the page is a phone-wide column in the middle of its area.
    pub mobile: bool,
    /// Requests a modify rule holds until the agent decides them.
    held_requests: crate::intercept::Held,
    /// The text the input method is still composing in the page (한 before it
    /// becomes 한글); `None` when nothing is being composed.
    pub composing: Option<String>,
}

impl Browser {
    pub fn new(launch: Launch, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Launch {
            url,
            open_panel,
            self_check,
            say,
        } = launch;
        let lang = Lang::detect();
        let (wake, mut woken) = futures::channel::mpsc::unbounded::<()>();
        let agent_wake = wake.clone();
        let (tool_tx, tool_commands) = futures::channel::mpsc::unbounded();
        let tool_browser = Arc::new(bridge::AppBrowser::new(tool_tx, wake.clone()));
        let traffic = Arc::new(Mutex::new(TrafficStore::default()));
        let debug_endpoint = crate::debug::Endpoint::default();
        let (mcp, intercept, downloads) =
            start_mcp(traffic.clone(), tool_browser, debug_endpoint.clone());
        let traffic_changed = Arc::new(AtomicBool::new(false));
        {
            let (store, wake, changed) = (traffic.clone(), wake.clone(), traffic_changed.clone());
            let downloads = downloads.clone();
            net::tap::set_network_tap(move |event| {
                record(&store, &downloads, event);
                changed.store(true, Ordering::Relaxed);
                let _ = wake.unbounded_send(());
            });
        }
        let mut engine = Engine::new(
            move || {
                let _ = wake.unbounded_send(());
            },
            window.scale_factor(),
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
        let first = engine
            .open(url.clone(), PhysicalSize::new(800, 600))
            .expect("the page engine could not start");

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

        // A --self-check run is a test: it must not add its pages to the user's history.
        let stores = Stores::new(traffic, self_check.is_none(), cx);
        let app_theme = AppThemeStore::load(stores.storage.as_ref());
        // REVER_THEME wins over the saved choice, for previews and captures.
        let theme_mode = if std::env::var("REVER_THEME").is_ok_and(|mode| !mode.is_empty()) {
            ThemeMode::from_env()
        } else {
            theme_from_stored(app_theme.mode())
        };
        let views = Views::new(&stores, lang, theme_mode, cx);
        views
            .address
            .update(cx, |input, cx| input.set_text(url.to_string(), cx));

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
        let menu = cx.new(|_| {
            let this = this.clone();
            AppMenu::new(move |cx| menu_props(&this, cx), look.clone())
        });
        let ip_badge = {
            let host = Rc::new(IpHost::new(stores.storage.clone()));
            cx.new(|cx| IpBadge::new(host, look.clone(), cx))
        };
        let bot_check = cx.new(|_| {
            BotCheckButton::new(
                move |url, _, cx| {
                    let _ = this.update(cx, |this, cx| this.open_url(url, cx));
                },
                look,
            )
        });

        Self {
            engine,
            tabs: vec![Tab {
                id: first,
                title: "".into(),
                url: url.to_string(),
                frame: None,
            }],
            active: 0,
            chat: Vec::new(),
            open_panel,
            page_bounds: Rc::new(Cell::new(Bounds::default())),
            stores,
            views,
            menu,
            ip_badge,
            bot_check,
            traffic_changed,
            eval_results: Rc::default(),
            agent: None,
            agent_busy: false,
            wake: agent_wake,
            tool_commands,
            mcp,
            page_focus: cx.focus_handle(),
            lang,
            theme_mode,
            app_theme,
            last_rgba: None,
            self_check,
            dialog_answer: None,
            dialog_log: Vec::new(),
            dialog_auto_dismiss: true,
            upload_paths: Vec::new(),
            tool_acted: None,
            select: None,
            mobile: false,
            debug_endpoint,
            held_requests,
            composing: None,
        }
    }

    pub fn active_id(&self) -> TabId {
        self.tabs[self.active].id
    }

    fn pump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active_id();
        let mut frame_ready = false;
        let mut console_changed = false;
        let mut opened = false;
        let mut closed = Vec::new();
        for event in self.engine.pump() {
            match event {
                EngineEvent::FrameReady(tab) => frame_ready |= tab == active,
                EngineEvent::Closed(id) => closed.push(id),
                EngineEvent::Opened(id) => {
                    self.tabs.push(Tab {
                        id,
                        title: "".into(),
                        url: String::new(),
                        frame: None,
                    });
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
            self.views.traffic.update(cx, |panel, cx| panel.refresh(cx));
            self.views
                .websocket
                .update(cx, |panel, cx| panel.refresh(cx));
        }
        self.pump_agent();
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
        for pixel in image.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
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
        self.tabs.push(Tab {
            id,
            title: "".into(),
            url: address,
            frame: None,
        });
        self.select_tab(self.tabs.len() - 1, window, cx);
    }

    pub fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        let url = self.tabs[index].url.clone();
        self.views
            .address
            .update(cx, |input, cx| input.set_text(url, cx));
        self.fit_page(window, cx);
        self.capture(window);
        cx.notify();
    }

    pub fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        // Never close the last tab, as in the tabs store.
        if self.tabs.len() == 1 || index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        self.engine.close(tab.id);
        if let Some(frame) = tab.frame {
            let _ = window.drop_image(frame);
        }
        let next = if index < self.active {
            self.active - 1
        } else {
            self.active.min(self.tabs.len() - 1)
        };
        self.select_tab(next, window, cx);
    }

    pub fn toggle_panel(&mut self, panel: &'static str, cx: &mut Context<Self>) {
        self.open_panel = if self.open_panel == Some(panel) {
            None
        } else {
            Some(panel)
        };
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
        self.views = Views::new(&self.stores, self.lang, self.theme_mode, cx);
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
        });
        self.views
            .composer
            .update(cx, |input, cx| input.set_text("", cx));
        if let Err(error) = self.start_turn(text) {
            self.agent = None;
            self.turn_failed(&error);
        }
        cx.notify();
    }

    fn turn_failed(&mut self, error: &str) {
        self.chat.push(ChatLine {
            from_user: false,
            text: format!("⚠ {}: {error}", self.lang.t("chat.turnFailed")),
            failed: true,
        });
    }

    /// Start the CLI if needed and hand it the prompt.
    fn start_turn(&mut self, text: &str) -> Result<(), String> {
        if self.agent.is_none() {
            // The same sandbox folder the Electron app gives its agents.
            let scratch = data_dir().join("agent-scratch");
            let wake = self.wake.clone();
            let mcp = self
                .mcp
                .as_ref()
                .map(|mcp| (mcp.url.as_str(), mcp.auth_header.as_str()));
            self.agent = Some(AgentSession::spawn(&scratch, mcp, move || {
                let _ = wake.unbounded_send(());
            })?);
        }
        let agent = self.agent.as_mut().ok_or("no agent session")?;
        agent
            .send(text)
            .map_err(|error| format!("Claude Code CLI is not accepting input: {error}"))?;
        self.agent_busy = true;
        self.chat.push(ChatLine {
            from_user: false,
            text: String::new(),
            failed: false,
        });
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
            // Dropped, which dismisses them: color pickers, context menus and the
            // input method have no interface here yet.
            _ => {}
        }
    }

    /// Close the open `<select>` list: with the option the user picked, or with
    /// none, which leaves the page's choice as it was.
    pub fn choose_option(&mut self, id: Option<usize>, cx: &mut Context<Self>) {
        let Some(mut select) = self.select.take() else {
            return;
        };
        if let Some(id) = id {
            select.select(vec![id]);
            select.submit();
        }
        cx.notify();
    }

    fn run_tool_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use bridge::Command;
        let scale = f64::from(window.scale_factor());
        while let Ok(command) = self.tool_commands.try_recv() {
            self.tool_acted = Some(Instant::now());
            let tab = self.active_id();
            match command {
                Command::Page(answer) => {
                    let active = &self.tabs[self.active];
                    let _ = answer.send(Ok(PageInfo {
                        url: active.url.clone(),
                        title: active.title.to_string(),
                    }));
                }
                Command::Navigate(url, answer) => {
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
                Command::Evaluate(script, answer) => {
                    self.engine.evaluate(tab, script, move |result| {
                        let _ = answer.send(result.map(|value| bridge::js_to_json(&value)));
                    })
                }
                Command::Screenshot(answer) => {
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
                            // Not selected: the agent reads it by index while it loads.
                            self.tabs.push(Tab {
                                id,
                                title: "".into(),
                                url: address,
                                frame: None,
                            });
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
            }
        }
    }

    /// Apply what the agent said since the last pump.
    fn pump_agent(&mut self) {
        let Some(agent) = self.agent.as_mut() else {
            return;
        };
        for event in agent.drain() {
            match event {
                AgentEvent::Text(text) => {
                    if let Some(line) = self.chat.last_mut().filter(|line| !line.from_user) {
                        line.text.push_str(&text);
                    }
                }
                AgentEvent::Done => self.agent_busy = false,
                AgentEvent::Failed(error) => {
                    // A failure outside a turn (the CLI exiting after a finished one) is not shown.
                    if self.agent_busy {
                        self.turn_failed(&error);
                    }
                    self.agent_busy = false;
                    self.agent = None;
                    return;
                }
            }
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
            "chat": self.chat.iter().map(|line| serde_json::json!({ "fromUser": line.from_user, "text": line.text })).collect::<Vec<_>>(),
        });
        let _ = std::fs::write(format!("{dir}/state.json"), state.to_string());
    }
}

/// The "⋮" menu, as App.tsx fills it. Items whose feature the Rust app does not have yet
/// (page theme, notifications, viewport, view source) are listed disabled.
fn menu_props(browser: &WeakEntity<Browser>, cx: &App) -> AppMenuProps {
    let (theme_mode, lang) = browser
        .read_with(cx, |this, _| (this.theme_mode, this.lang))
        .unwrap_or((ThemeMode::Light, Lang::En));
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
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn 초안은_입력란_뒤에_붙고_줄바꿈은_공백이_된다() {
        assert_eq!(
            draft_text("", "Analyze this:\n\nQuestion: "),
            "Analyze this:  Question: "
        );
        assert_eq!(draft_text("already typed", "more"), "already typed more");
    }

    #[test]
    fn 서보의_warn은_cdp의_warning으로_기록한다() {
        assert_eq!(console_kind("warn"), "warning");
        assert_eq!(console_kind("error"), "error");
        assert_eq!(console_kind("log"), "log");
    }

    #[test]
    fn 히스토리의_탭_아이디는_t와_번호다() {
        assert_eq!(history_tab_id(TabId(3)), "t3");
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
