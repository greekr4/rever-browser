//! The panel crates' views and stores as this app holds them, and what they ask of
//! the app: storage, script evaluation in the page, the egress IP, the page's sockets.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui::{App, Context, Entity, Task, prelude::*};
use rever_core::traffic::TrafficStore;
use rever_formats::console_format::EvalReply;
use rever_ui_console::console_panel::{ConsolePanel, Evaluate};
use rever_ui_console::exceptions_panel::ExceptionsPanel;
use rever_ui_history::OpenUrl;
use rever_ui_history::bookmark_bar::BookmarkBar;
use rever_ui_history::bookmarks::BookmarksStore;
use rever_ui_history::history::HistoryStore;
use rever_ui_history::history_panel::HistoryPanel;
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::text_input::{Submitted, TextInput};
use rever_ui_kit::theme::ThemeMode;
use rever_ui_menus::ip_badge::{IpBadgeHost, IpResult};
use rever_ui_state::ip_visibility::IpVisibilityStore;
use rever_ui_apimap::api_map_panel::ApiMapPanel;
use rever_ui_repeater::repeater_panel::{RepeaterPanel, RepeaterSender};
use rever_ui_repeater::ws_repeater_panel::WsRepeaterPanel;
use rever_ui_repeater::{RepeaterRequestSpec, RepeaterResponse, RepeaterStore};
use rever_ui_state::{FileStorage, MemoryStorage, Storage};
use rever_ui_storage::cookies_panel::CookiesPanel;
use rever_ui_workflows::core::workflow_panel::WorkflowPanel;
use rever_ui_workflows::core::store::WorkflowsStore;
use rever_ui_workflows::host::{
    CliRunPayload, CliRunResult, McpToolInfo, OnProgress, PipeProgress, TrafficCall, WorkflowHost,
    WorkflowRunStep, WorkflowStepProgress,
};
use rever_ui_workflows::pipeline::resolve::ResolvedPipeNode;
use rever_ui_traffic::detail_drawer::{DetailDrawer, DetailDrawerEvent, WIDTH_STORAGE_KEY};
use rever_ui_traffic::store::TrafficState;
use rever_ui_traffic::traffic_list::{TrafficList, TrafficListEvent};
use rever_ui_websocket::{LiveSocket, SendOutcome, SendRequest, WebSocketPanel, WsHost};
use serde_json::json;
use servo::JSValue;

use crate::app::{Browser, data_dir};
use crate::bridge;
use crate::storage_host::StorageBridge;

/// The interface's own settings file in the shared data folder. The Electron app keeps
/// these keys in Chromium's Local Storage and has no file of this name.
const STORAGE_FILE: &str = "rust-ui-storage.json";

/// What a widget shows for something the page engine cannot give it yet.
const NOT_YET: &str = "not available yet";

fn open_storage(persist: bool) -> Rc<dyn Storage> {
    if !persist {
        return Rc::new(MemoryStorage::new());
    }
    let path = data_dir().join(STORAGE_FILE);
    match FileStorage::open(&path) {
        Ok(storage) => Rc::new(storage),
        Err(error) => {
            // The unreadable file is left as it is; this run keeps its settings in memory.
            eprintln!(
                "rever: {}: {error}; history, bookmarks and settings are not saved in this run",
                path.display()
            );
            Rc::new(MemoryStorage::new())
        }
    }
}

fn save(storage: &dyn Storage, key: &str, value: &str) {
    if let Err(error) = storage.set(key, value) {
        eprintln!("rever: {key} was not saved: {error}");
    }
}

/// The history crate's stores write through their own `Storage` trait; this hands them
/// the app's storage.
struct HistoryStorage(Rc<dyn Storage>);

impl rever_ui_history::storage::Storage for HistoryStorage {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key)
    }

    fn set(&mut self, key: &str, value: &str) {
        save(self.0.as_ref(), key, value);
    }
}

/// What outlives a change of language or theme.
#[derive(Clone)]
pub(crate) struct Stores {
    /// The capture store the network tap and the console fill.
    pub traffic: Arc<Mutex<TrafficStore>>,
    /// The traffic list's rows and selection, shared with the detail drawer.
    pub traffic_state: Entity<TrafficState>,
    pub history: Entity<HistoryStore>,
    pub bookmarks: Entity<BookmarksStore>,
    pub storage: Rc<dyn Storage>,
}

impl Stores {
    /// `persist` false keeps history, bookmarks and settings in memory only.
    pub fn new(traffic: Arc<Mutex<TrafficStore>>, persist: bool, cx: &mut App) -> Self {
        let storage = open_storage(persist);
        let boxed = || Box::new(HistoryStorage(storage.clone()));
        Self {
            traffic,
            traffic_state: cx.new(|_| TrafficState::default()),
            history: cx.new(|_| HistoryStore::new(boxed())),
            bookmarks: cx.new(|_| BookmarksStore::new(boxed())),
            storage: storage.clone(),
        }
    }
}

/// The views that take language and theme when they are made.
pub(crate) struct Views {
    pub address: Entity<TextInput>,
    pub composer: Entity<TextInput>,
    /// The target-URL field of the Red Team dialog.
    pub redteam_target: Entity<TextInput>,
    pub traffic: Entity<TrafficList>,
    pub drawer: Entity<DetailDrawer>,
    pub console: Entity<ConsolePanel>,
    pub exceptions: Entity<ExceptionsPanel>,
    pub websocket: Entity<WebSocketPanel>,
    pub history: Entity<HistoryPanel>,
    pub bookmark_bar: Entity<BookmarkBar>,
    pub storage: Entity<CookiesPanel>,
    pub apimap: Entity<ApiMapPanel>,
    pub repeater: Entity<RepeaterPanel>,
    /// Kept so "send to repeater" can load a request the panel then syncs from.
    pub repeater_store: Entity<RepeaterStore>,
    pub wsrepeater: Entity<WsRepeaterPanel>,
    pub workflows: Entity<WorkflowPanel>,
}

impl Views {
    pub fn new(
        stores: &Stores,
        lang: Lang,
        theme_mode: ThemeMode,
        browser: Arc<dyn rever_tools_api::tool::Browser>,
        cx: &mut Context<Browser>,
    ) -> Self {
        let address = cx.new(|cx| TextInput::new(lang.t("toolbar.addressPlaceholder"), cx));
        cx.subscribe(&address, |this, _, event: &Submitted, cx| {
            this.navigate(&event.0, cx)
        })
        .detach();
        let composer = cx.new(|cx| TextInput::new(lang.t("chat.placeholder"), cx));
        cx.subscribe(&composer, |this, _, event: &Submitted, cx| {
            this.send_chat(&event.0, cx)
        })
        .detach();
        let redteam_target = cx.new(|cx| TextInput::new(lang.t("scan.bar.placeholder"), cx));
        cx.subscribe(&redteam_target, |this, _, _: &Submitted, cx| {
            this.start_redteam(cx)
        })
        .detach();

        let (store, state) = (stores.traffic.clone(), stores.traffic_state.clone());
        let traffic =
            cx.new(|cx| TrafficList::new(store.clone(), state.clone(), lang, theme_mode, cx));
        cx.subscribe(&traffic, |this, _, event: &TrafficListEvent, cx| {
            match event {
                TrafficListEvent::PushDraft(text) => this.push_draft(text, cx),
                TrafficListEvent::SendToRepeater(id) => this.load_repeater(id, cx),
            }
            cx.notify();
        })
        .detach();
        let saved_width = stores.storage.get(WIDTH_STORAGE_KEY);
        let drawer = cx.new(|cx| {
            let width = saved_width.as_deref();
            DetailDrawer::new(store.clone(), state, width, lang, theme_mode, cx)
        });
        cx.subscribe(
            &drawer,
            |this, _, event: &DetailDrawerEvent, cx| match event {
                DetailDrawerEvent::PushDraft(text) => this.push_draft(text, cx),
                DetailDrawerEvent::SendToRepeater(id) => this.load_repeater(id, cx),
                DetailDrawerEvent::Resized(width) => save(
                    this.stores.storage.as_ref(),
                    WIDTH_STORAGE_KEY,
                    &width.to_string(),
                ),
            },
        )
        .detach();

        let console = cx.new(|cx| ConsolePanel::new(store.clone(), lang, theme_mode, cx));
        cx.subscribe(&console, |this, _, event: &Evaluate, _| {
            this.evaluate_console(&event.0)
        })
        .detach();
        let exceptions = cx.new(|cx| ExceptionsPanel::new(store.clone(), lang, theme_mode, cx));
        let websocket =
            cx.new(|cx| WebSocketPanel::new(store, Arc::new(NoSockets), lang, theme_mode, cx));

        let history = cx.new(|cx| HistoryPanel::new(stores.history.clone(), lang, theme_mode, cx));
        cx.subscribe(&history, |this, _, event: &OpenUrl, cx| {
            this.open_url(&event.0, cx)
        })
        .detach();
        let bookmark_bar =
            cx.new(|cx| BookmarkBar::new(stores.bookmarks.clone(), lang, theme_mode, cx));
        cx.subscribe(&bookmark_bar, |this, _, event: &OpenUrl, cx| {
            this.open_url(&event.0, cx)
        })
        .detach();

        let storage = cx.new(|cx| {
            CookiesPanel::new(Rc::new(StorageBridge::new(browser.clone())), lang, theme_mode, cx)
        });
        let apimap =
            cx.new(|cx| ApiMapPanel::new(stores.traffic.clone(), lang, theme_mode, cx));
        let repeater_store = cx.new(|_| RepeaterStore::new());
        let repeater = {
            let sender = Arc::new(RepeaterBridge {
                browser: browser.clone(),
            });
            cx.new(|cx| RepeaterPanel::new(repeater_store.clone(), sender, lang, theme_mode, cx))
        };
        let wsrepeater = cx.new(|cx| {
            WsRepeaterPanel::new(stores.traffic.clone(), Arc::new(NoSockets), lang, theme_mode, cx)
        });
        let workflows = {
            let store = cx.new(|_| WorkflowsStore::new(stores.storage.clone()));
            let registry = Rc::new(rever_ui_workflows::default_registry());
            cx.new(|cx| WorkflowPanel::new(store, registry, Rc::new(NoWorkflows), lang, theme_mode, cx))
        };

        Self {
            address,
            composer,
            redteam_target,
            traffic,
            drawer,
            console,
            exceptions,
            websocket,
            history,
            bookmark_bar,
            storage,
            apimap,
            repeater,
            repeater_store,
            wsrepeater,
            workflows,
        }
    }
}

/// The workflows panel's host. The panel renders (list/edit workflows from the
/// registry), but running them + the CLI compiler need the agent/MCP wiring that
/// isn't ported into the GPUI shell yet, so those return "not yet".
struct NoWorkflows;

impl WorkflowHost for NoWorkflows {
    fn list_tools(&self, _: &mut App) -> Task<Vec<McpToolInfo>> {
        Task::ready(Vec::new())
    }
    fn run(
        &self,
        _: Vec<WorkflowRunStep>,
        _: OnProgress<WorkflowStepProgress>,
        _: &mut App,
    ) -> Task<Result<Vec<WorkflowStepProgress>, String>> {
        Task::ready(Err(NOT_YET.to_owned()))
    }
    fn run_pipeline(
        &self,
        _: Vec<ResolvedPipeNode>,
        _: OnProgress<PipeProgress>,
        _: &mut App,
    ) -> Task<Result<bool, String>> {
        Task::ready(Err(NOT_YET.to_owned()))
    }
    fn cancel(&self, _: &mut App) {}
    fn compile_cli(
        &self,
        _: Vec<String>,
        _: &mut App,
    ) -> Task<Result<rever_cli_spec::cli_spec::CliData, String>> {
        Task::ready(Err(NOT_YET.to_owned()))
    }
    fn run_cli(&self, _: CliRunPayload, _: &mut App) -> Task<Result<CliRunResult, String>> {
        Task::ready(Err(NOT_YET.to_owned()))
    }
    fn traffic(&self, _: &App) -> Vec<TrafficCall> {
        Vec::new()
    }
}

/// The repeater panel's sender: replays a request through the active tab's
/// `fetch` (the same engine path as the `repeater_send` tool), so it carries the
/// page's cookies/TLS. `send_raw` runs on a background thread, so blocking on
/// the bridge future is fine.
struct RepeaterBridge {
    browser: Arc<dyn rever_tools_api::tool::Browser>,
}

impl RepeaterSender for RepeaterBridge {
    fn send_raw(&self, spec: RepeaterRequestSpec) -> Result<RepeaterResponse, String> {
        let request = rever_tools_http::RequestSpec {
            url: spec.url,
            method: spec.method,
            headers: spec.headers.entries().iter().cloned().collect(),
            body: spec.body,
        };
        let expression = rever_tools_http::repeater::build_eval_expression(&request);
        let value = futures::executor::block_on(self.browser.evaluate(&expression))?;
        serde_json::from_value(value).map_err(|error| error.to_string())
    }
}

/// The WebSocket panel's page side. The engine reports neither the page's sockets nor
/// their frames yet, so there is nothing to list and nothing to send through.
struct NoSockets;

impl WsHost for NoSockets {
    fn sockets(&self) -> Result<Vec<LiveSocket>, String> {
        Err(NOT_YET.to_owned())
    }

    fn send(&self, _: SendRequest) -> SendOutcome {
        SendOutcome {
            ok: false,
            error: Some(NOT_YET.to_owned()),
            reconnected: false,
        }
    }
}

/// The IP badge's host: the egress IP lookup needs the tab's network session, which
/// the engine does not expose yet; hiding the value is saved like the Electron setting.
pub(crate) struct IpHost {
    storage: Rc<dyn Storage>,
    visibility: RefCell<IpVisibilityStore>,
}

impl IpHost {
    pub fn new(storage: Rc<dyn Storage>) -> Self {
        let visibility = RefCell::new(IpVisibilityStore::load(storage.as_ref()));
        Self {
            storage,
            visibility,
        }
    }
}

impl IpBadgeHost for IpHost {
    fn current_ip(&self, _: &mut App) -> Task<Result<IpResult, String>> {
        Task::ready(Err(NOT_YET.to_owned()))
    }

    fn hidden(&self, _: &App) -> bool {
        self.visibility.borrow().hidden()
    }

    fn toggle_hidden(&self, _: &mut App) {
        if let Err(error) = self.visibility.borrow_mut().toggle(self.storage.as_ref()) {
            eprintln!("rever: the IP badge setting was not saved: {error}");
        }
    }
}

/// A Servo script result in the shape of a CDP `Runtime.evaluate` reply, which is what
/// the console panel formats. Servo sends no object preview: objects and arrays print
/// as their JSON text.
pub(crate) fn eval_reply(value: &JSValue) -> Result<EvalReply, String> {
    let plain = bridge::js_to_json(value);
    let result = match value {
        JSValue::Undefined => json!({ "type": "undefined" }),
        JSValue::Null => json!({ "type": "object", "subtype": "null", "value": null }),
        JSValue::Boolean(_) => json!({ "type": "boolean", "value": plain }),
        // JSON has no NaN or Infinity; CDP names them in `unserializableValue`.
        JSValue::Number(n) if !n.is_finite() => {
            let name = if n.is_nan() {
                "NaN"
            } else if *n > 0.0 {
                "Infinity"
            } else {
                "-Infinity"
            };
            json!({ "type": "number", "unserializableValue": name })
        }
        JSValue::Number(_) => json!({ "type": "number", "value": plain }),
        JSValue::String(_) => json!({ "type": "string", "value": plain }),
        _ => json!({ "type": "object", "description": plain.to_string() }),
    };
    serde_json::from_value(json!({ "result": result })).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use rever_formats::console_format::format_eval_result;
    use rever_ui_history::storage::Storage as _;

    use super::*;

    fn printed(value: JSValue) -> String {
        let output = format_eval_result(&eval_reply(&value).unwrap());
        assert!(!output.is_error);
        output.text
    }

    #[test]
    fn 스크립트_결과는_콘솔에_크롬처럼_찍힌다() {
        assert_eq!(printed(JSValue::Undefined), "undefined");
        assert_eq!(printed(JSValue::Null), "null");
        assert_eq!(printed(JSValue::Boolean(true)), "true");
        assert_eq!(printed(JSValue::Number(42.0)), "42");
        assert_eq!(printed(JSValue::Number(1.5)), "1.5");
        assert_eq!(printed(JSValue::String("zz".into())), "\"zz\"");
    }

    #[test]
    fn 유한하지_않은_수는_이름으로_찍힌다() {
        assert_eq!(printed(JSValue::Number(f64::NAN)), "NaN");
        assert_eq!(printed(JSValue::Number(f64::INFINITY)), "Infinity");
        assert_eq!(printed(JSValue::Number(f64::NEG_INFINITY)), "-Infinity");
    }

    #[test]
    fn 객체와_배열은_json_문자열로_찍힌다() {
        let list = JSValue::Array(vec![JSValue::Number(1.0), JSValue::String("a".into())]);
        assert_eq!(printed(list), "[1,\"a\"]");
        let object = JSValue::Object([("k".to_owned(), JSValue::Boolean(false))].into());
        assert_eq!(printed(object), "{\"k\":false}");
    }

    #[test]
    fn 열린_소켓과_ip_조회는_아직_없다고_답한다() {
        assert_eq!(NoSockets.sockets(), Err(NOT_YET.to_owned()));
        let sent = NoSockets.send(SendRequest {
            url: "wss://example.com/".into(),
            payload: "ping".into(),
            as_binary: false,
            reconnect: false,
        });
        assert!(!sent.ok);
        assert_eq!(sent.error.as_deref(), Some(NOT_YET));
    }

    #[test]
    fn 히스토리_저장소는_앱의_저장소에_쓰고_읽는다() {
        let shared: Rc<dyn Storage> = Rc::new(MemoryStorage::new());
        let mut adapter = HistoryStorage(shared.clone());
        assert_eq!(adapter.get("rev:history"), None);
        adapter.set("rev:history", "{}");
        assert_eq!(shared.get("rev:history").as_deref(), Some("{}"));
        assert_eq!(adapter.get("rev:history").as_deref(), Some("{}"));
    }

    #[test]
    fn 저장하지_않는_실행은_파일을_열지_않는다() {
        // 메모리 저장소는 쓴 값을 돌려주고, 데이터 폴더에는 아무것도 남기지 않는다.
        let storage = open_storage(false);
        save(storage.as_ref(), "k", "v");
        assert_eq!(storage.get("k").as_deref(), Some("v"));
    }
}
