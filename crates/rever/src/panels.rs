//! The panel crates' views and stores as this app holds them, and what they ask of
//! the app: storage, script evaluation in the page, the egress IP, the page's sockets.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui::{App, Context, Entity, Task, prelude::*};
use rever_core::traffic::TrafficStore;
use rever_formats::console_format::EvalReply;
use rever_ui_apimap::api_map_panel::ApiMapPanel;
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
use rever_ui_repeater::repeater_panel::{RepeaterPanel, RepeaterSender};
use rever_ui_repeater::text_area::TextArea;
use rever_ui_repeater::ws_repeater_panel::WsRepeaterPanel;
use rever_ui_repeater::{RepeaterRequestSpec, RepeaterResponse, RepeaterStore};
use rever_ui_state::ip_visibility::IpVisibilityStore;
use rever_ui_state::{FileStorage, MemoryStorage, Storage};
use rever_ui_storage::cookies_panel::CookiesPanel;
use rever_ui_traffic::detail_drawer::{DetailDrawer, DetailDrawerEvent, WIDTH_STORAGE_KEY};
use rever_ui_traffic::store::TrafficState;
use rever_ui_traffic::traffic_list::{TrafficList, TrafficListEvent};
use rever_ui_websocket::{LiveSocket, SendOutcome, SendRequest, WebSocketPanel, WsHost};
use rever_ui_workflows::core::store::WorkflowsStore;
use rever_ui_workflows::core::workflow_panel::WorkflowPanel;
use rever_ui_workflows::host::{
    CliRunPayload, CliRunResult, McpToolInfo, OnProgress, PipeProgress, TrafficCall, WorkflowHost,
    WorkflowRunStep, WorkflowStepProgress,
};
use rever_ui_workflows::pipeline::resolve::ResolvedPipeNode;
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
    pub composer: Entity<TextArea>,
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
    pub workflows_store: Entity<WorkflowsStore>,
}

impl Views {
    pub fn new(
        stores: &Stores,
        lang: Lang,
        theme_mode: ThemeMode,
        browser: Arc<dyn rever_tools_api::tool::Browser>,
        tools: Arc<rever_tools_api::tool::ToolContext>,
        cx: &mut Context<Browser>,
    ) -> Self {
        let address = cx.new(|cx| TextInput::new(lang.t("toolbar.addressPlaceholder"), cx));
        cx.subscribe(&address, |this, _, event: &Submitted, cx| {
            this.navigate(&event.0, cx)
        })
        .detach();
        // Several lines: Enter sends, Shift+Enter breaks the line.
        let composer = cx.new(|cx| TextArea::message(lang.t("chat.placeholder"), cx));
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
        let page_sockets = Arc::new(PageSockets {
            browser: browser.clone(),
        });
        let websocket =
            cx.new(|cx| WebSocketPanel::new(store, page_sockets.clone(), lang, theme_mode, cx));

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
            CookiesPanel::new(
                Rc::new(StorageBridge::new(browser.clone())),
                lang,
                theme_mode,
                cx,
            )
        });
        let apimap = cx.new(|cx| ApiMapPanel::new(stores.traffic.clone(), lang, theme_mode, cx));
        let repeater_store = cx.new(|_| RepeaterStore::new());
        let repeater = {
            let sender = Arc::new(RepeaterBridge {
                browser: browser.clone(),
            });
            cx.new(|cx| RepeaterPanel::new(repeater_store.clone(), sender, lang, theme_mode, cx))
        };
        let wsrepeater = cx.new(|cx| {
            WsRepeaterPanel::new(stores.traffic.clone(), page_sockets, lang, theme_mode, cx)
        });
        let workflows_store = cx.new(|_| {
            WorkflowsStore::new(Rc::new(WorkflowsFile {
                path: workflows_file(),
                earlier: stores.storage.clone(),
            }))
        });
        let workflows = {
            let store = workflows_store.clone();
            let registry = Rc::new(rever_ui_workflows::default_registry());
            let host = Rc::new(WorkflowTools { context: tools });
            cx.new(|cx| WorkflowPanel::new(store, registry, host, lang, theme_mode, cx))
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
            workflows_store,
        }
    }
}

/// The file the macro and CLI tools keep the workflows in.
pub fn workflows_file() -> std::path::PathBuf {
    crate::app::data_dir().join(rever_tools_automation::store::WORKFLOWS_FILE)
}

/// The Workflows panel's storage: the tools' file, so the workflow the agent saves is
/// the one the panel lists. Both sides write the same JSON.
struct WorkflowsFile {
    path: std::path::PathBuf,
    /// Where the panel kept the list before it shared the file.
    earlier: Rc<dyn rever_ui_state::Storage>,
}

impl rever_ui_state::Storage for WorkflowsFile {
    fn get(&self, key: &str) -> Option<String> {
        std::fs::read_to_string(&self.path)
            .ok()
            .or_else(|| self.earlier.get(key))
    }

    fn set(&self, _: &str, value: &str) -> std::io::Result<()> {
        // Written beside the file and moved over it: a tool never reads half a list.
        // Not the tools' own temporary name, so the two never write the same file.
        let temporary = self.path.with_extension("json.panel.tmp");
        std::fs::write(&temporary, value)?;
        std::fs::rename(&temporary, &self.path)
    }

    fn remove(&self, _: &str) -> std::io::Result<()> {
        std::fs::remove_file(&self.path)
    }
}

/// The panel and the workflow executor each port the same TypeScript types: their
/// JSON form carries a value from one to the other.
fn recast<T: serde::Serialize, U: serde::de::DeserializeOwned>(value: &T) -> Result<U, String> {
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .map_err(|error| error.to_string())
}

/// Run `work` on a thread with a tokio runtime of its own, and hand back its result.
/// The executor waits on tokio timers, and its tools are answered by this window's
/// thread, which must stay free meanwhile.
fn on_executor_thread<T: Send + 'static>(
    work: impl FnOnce(&tokio::runtime::Runtime) -> T + Send + 'static,
) -> futures::channel::oneshot::Receiver<Result<T, String>> {
    let (done, finished) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let result = runtime
            .map(|runtime| work(&runtime))
            .map_err(|error| error.to_string());
        let _ = done.send(result);
    });
    finished
}

const RUN_STOPPED: &str = "the run stopped";

/// The workflows panel's host: macros, pipelines and CLI commands run through the
/// tools and the executor the agent's `run_macro` and `cli_run` use.
struct WorkflowTools {
    context: Arc<rever_tools_api::tool::ToolContext>,
}

impl WorkflowHost for WorkflowTools {
    fn list_tools(&self, _: &mut App) -> Task<Vec<McpToolInfo>> {
        use rever_tools_automation::bridge::{ToolRegistryHandle, list_mcp_tools};
        let allowed = list_mcp_tools(&self.context).unwrap_or_default();
        let registry = self.context.state::<ToolRegistryHandle>().get();
        let tools = registry
            .map(|registry| registry.defs())
            .unwrap_or_default()
            .into_iter()
            .filter(|def| allowed.contains(&def.name))
            .map(|def| McpToolInfo {
                name: def.name,
                description: Some(def.description),
                input_schema: def.input_schema,
            })
            .collect();
        Task::ready(tools)
    }

    fn run(
        &self,
        steps: Vec<WorkflowRunStep>,
        on_progress: OnProgress<WorkflowStepProgress>,
        cx: &mut App,
    ) -> Task<Result<Vec<WorkflowStepProgress>, String>> {
        use futures::StreamExt;
        use rever_tools_automation::workflow_executor::{
            RunOptions, RunStep, StepProgress, run_workflow,
        };
        let steps: Vec<RunStep> = match recast(&steps) {
            Ok(steps) => steps,
            Err(error) => return Task::ready(Err(error)),
        };
        let context = self.context.clone();
        let (progress, mut seen) = futures::channel::mpsc::unbounded::<StepProgress>();
        let finished = on_executor_thread(move |runtime| {
            let mut report = |step| {
                let _ = progress.unbounded_send(step);
            };
            // An AI step's agent starts where the chat's does.
            let cwd = crate::app::data_dir().join("agent-scratch");
            let options = RunOptions {
                cwd: Some(cwd.to_string_lossy().into_owned()),
            };
            runtime.block_on(run_workflow(&context, &steps, &mut report, &options))
        });
        cx.spawn(async move |cx| {
            while let Some(step) = seen.next().await {
                if let Ok(step) = recast(&step) {
                    let _ = cx.update(|cx| on_progress(step, cx));
                }
            }
            let steps = finished.await.map_err(|_| RUN_STOPPED.to_owned())??;
            steps.iter().map(recast).collect()
        })
    }

    fn run_pipeline(
        &self,
        nodes: Vec<ResolvedPipeNode>,
        on_progress: OnProgress<PipeProgress>,
        cx: &mut App,
    ) -> Task<Result<bool, String>> {
        use futures::StreamExt;
        use rever_tools_automation::workflow_executor as executor;
        let nodes: Vec<executor::ResolvedPipeNode> = match recast(&nodes) {
            Ok(nodes) => nodes,
            Err(error) => return Task::ready(Err(error)),
        };
        let context = self.context.clone();
        let (progress, mut seen) = futures::channel::mpsc::unbounded::<executor::PipeProgress>();
        let finished = on_executor_thread(move |runtime| {
            let mut report = |node| {
                let _ = progress.unbounded_send(node);
            };
            runtime.block_on(executor::run_pipeline(&context, &nodes, &mut report));
        });
        cx.spawn(async move |cx| {
            while let Some(node) = seen.next().await {
                if let Ok(node) = recast(&node) {
                    let _ = cx.update(|cx| on_progress(node, cx));
                }
            }
            finished.await.map_err(|_| RUN_STOPPED.to_owned())??;
            Ok(true)
        })
    }

    fn cancel(&self, _: &mut App) {
        rever_tools_automation::workflow_executor::cancel_workflow(&self.context);
    }

    fn compile_cli(
        &self,
        request_ids: Vec<String>,
        _: &mut App,
    ) -> Task<Result<rever_cli_spec::cli_spec::CliData, String>> {
        use rever_tools_automation::cli_exec::compile_captured;
        Task::ready(compile_captured(
            &self.context,
            &request_ids,
            &Default::default(),
        ))
    }

    fn run_cli(&self, payload: CliRunPayload, cx: &mut App) -> Task<Result<CliRunResult, String>> {
        use rever_tools_automation::cli_exec::{CliRun, execute_cli};
        let context = self.context.clone();
        let finished = on_executor_thread(move |runtime| {
            let run = CliRun {
                base_url: &payload.base_url,
                writes: payload.writes,
                command: &payload.command,
                inputs: &payload.inputs,
            };
            runtime.block_on(execute_cli(&context, run))
        });
        cx.spawn(async move |_| {
            let result = finished.await.map_err(|_| RUN_STOPPED.to_owned())??;
            recast(&result)
        })
    }

    fn traffic(&self, _: &App) -> Vec<TrafficCall> {
        use rever_core::traffic::{ListFilter, MAX_ENTRIES};
        let store = self
            .context
            .traffic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let filter = ListFilter {
            limit: Some(MAX_ENTRIES),
            ..ListFilter::default()
        };
        // The store lists newest first; the editor wants capture order.
        store
            .list(&filter)
            .into_iter()
            .rev()
            .map(|row| TrafficCall {
                request_id: row.request_id.clone(),
                method: row.method.clone(),
                url: row.url.clone(),
                resource_type: row.resource_type.clone(),
            })
            .collect()
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

/// The WebSocket panel's page side, over the engine bridge: sockets come from
/// the capture wrapper (`sockets_expression` installs it into the current
/// document when missing), sends run on the page's own socket. Both calls
/// block — the panel runs them off the main thread, as its host doc states.
struct PageSockets {
    browser: Arc<dyn rever_tools_api::tool::Browser>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WrapperEntry {
    url: String,
    ready_state: u8,
}

impl WsHost for PageSockets {
    fn sockets(&self) -> Result<Vec<LiveSocket>, String> {
        let value = futures::executor::block_on(
            self.browser
                .evaluate(&rever_tools_traffic::websocket::sockets_expression()),
        )?;
        let json = value
            .as_str()
            .ok_or_else(|| "the page did not answer the socket list".to_owned())?;
        let entries: Vec<WrapperEntry> = serde_json::from_str(json)
            .map_err(|error| format!("unreadable socket list: {error}"))?;
        Ok(entries
            .into_iter()
            .map(|entry| LiveSocket {
                url: entry.url,
                ready_state: entry.ready_state,
                frame: "page".to_owned(),
            })
            .collect())
    }

    fn send(&self, request: SendRequest) -> SendOutcome {
        let expression = rever_tools_traffic::websocket::send_by_url_expression(
            &request.url,
            &request.payload,
            request.as_binary,
            request.reconnect,
        );
        let answer = futures::executor::block_on(self.browser.evaluate(&expression));
        match answer {
            Ok(serde_json::Value::String(word)) if word == "ok" => SendOutcome {
                ok: true,
                ..SendOutcome::default()
            },
            Ok(serde_json::Value::String(word)) if word == "reopened" => SendOutcome {
                ok: true,
                reconnected: true,
                ..SendOutcome::default()
            },
            Ok(serde_json::Value::String(word)) => SendOutcome {
                ok: false,
                error: Some(word),
                ..SendOutcome::default()
            },
            Ok(other) => SendOutcome {
                ok: false,
                error: Some(other.to_string()),
                ..SendOutcome::default()
            },
            Err(error) => SendOutcome {
                ok: false,
                error: Some(error),
                ..SendOutcome::default()
            },
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

    /// A browser whose evaluate always answers one canned JSON value.
    struct SocketsBrowser(serde_json::Value);

    impl rever_tools_api::tool::Browser for SocketsBrowser {
        fn evaluate<'a>(
            &'a self,
            _: &'a str,
        ) -> rever_tools_api::tool::BoxFuture<'a, Result<serde_json::Value, String>> {
            let answer = self.0.clone();
            Box::pin(async move { Ok(answer) })
        }
    }

    #[test]
    fn 페이지_소켓_호스트는_래퍼_목록을_파싱하고_전송_답을_매핑한다() {
        let host = PageSockets {
            browser: Arc::new(SocketsBrowser(serde_json::Value::String(
                r#"[{"id":0,"url":"ws://a.test/ws","readyState":1,"openedAt":1,"protocols":null}]"#
                    .into(),
            ))),
        };
        let sockets = host.sockets().unwrap();
        assert_eq!(
            (
                sockets[0].url.as_str(),
                sockets[0].ready_state,
                sockets[0].frame.as_str()
            ),
            ("ws://a.test/ws", 1, "page")
        );

        let ok = PageSockets {
            browser: Arc::new(SocketsBrowser(serde_json::Value::String("ok".into()))),
        };
        assert!(
            ok.send(SendRequest {
                url: "ws://a.test/ws".into(),
                payload: "p".into(),
                as_binary: false,
                reconnect: false
            })
            .ok
        );
        let reopened = PageSockets {
            browser: Arc::new(SocketsBrowser(serde_json::Value::String("reopened".into()))),
        };
        assert!(
            reopened
                .send(SendRequest {
                    url: "ws://a.test/ws".into(),
                    payload: "p".into(),
                    as_binary: false,
                    reconnect: true
                })
                .reconnected
        );
        let failed = PageSockets {
            browser: Arc::new(SocketsBrowser(serde_json::Value::String(
                "not-open:3".into(),
            ))),
        };
        let outcome = failed.send(SendRequest {
            url: "ws://a.test/ws".into(),
            payload: "p".into(),
            as_binary: false,
            reconnect: false,
        });
        assert!(!outcome.ok && outcome.error.as_deref() == Some("not-open:3"));
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
