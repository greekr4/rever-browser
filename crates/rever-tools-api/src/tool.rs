//! The contract between MCP tools, the MCP server and the browser.
//!
//! A tool crate implements [`Tool`] for each tool and registers them in a
//! [`ToolRegistry`]; the server lists and calls them; the app implements
//! [`Browser`] over its page engine. Tool crates depend on this crate only, never
//! on the app or on each other.
//!
//! Naming a tool with `ToolDef::new("name", …)` is also how the port scoreboard
//! counts ported tools, so the name is always a string literal at that call.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use rever_core::traffic::TrafficStore;
use serde_json::Value;

use crate::utils::ToolResult;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a client sees in `tools/list`.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments object.
    pub input_schema: Value,
}

impl ToolDef {
    pub fn new(name: &str, description: &str, input_schema: Value) -> Self {
        Self {
            name: name.to_owned(),
            description: description.to_owned(),
            input_schema,
        }
    }
}

pub trait Tool: Send + Sync {
    fn def(&self) -> ToolDef;

    /// `args` is the arguments object as the client sent it; validating it is the
    /// tool's job. Failures are reported in the result (`utils::err`), not by panicking.
    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult>;
}

/// The error text every unsupported [`Browser`] operation returns, so tools and
/// tests can recognise it.
pub fn unsupported(operation: &str) -> String {
    format!("{operation} is not supported by this browser engine yet")
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageInfo {
    pub url: String,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    /// Epoch seconds; `None` for a session cookie.
    pub expires: Option<f64>,
    pub http_only: bool,
    pub secure: bool,
    pub same_site: Option<String>,
}

/// One synthetic input, in CSS pixels of the page viewport.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    MouseMove {
        x: f64,
        y: f64,
    },
    MouseDown {
        x: f64,
        y: f64,
    },
    MouseUp {
        x: f64,
        y: f64,
    },
    /// Positive `dy` scrolls the page down.
    Wheel {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
    /// A press or release of one button at a point: `button` is the DOM's number
    /// (0 left, 1 middle, 2 right). The engine counts clicks itself, so a double
    /// click is two press/release pairs in quick succession.
    MouseButton {
        x: f64,
        y: f64,
        down: bool,
        button: u8,
    },
    /// A W3C key value ("a", "Enter", "ArrowLeft"), pressed then released.
    Key {
        key: String,
    },
    /// A key pressed and released while modifiers are held: `Alt` 1, `Control` 2,
    /// `Meta` 4, `Shift` 8.
    KeyChord {
        key: String,
        modifiers: u8,
    },
    /// Text typed as key presses.
    Text {
        text: String,
    },
}

/// One open tab, as `Browser::tabs` lists them: the position in the list is its index.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabInfo {
    pub url: String,
    pub title: String,
    /// The tab the tools act on.
    pub active: bool,
    /// Stable per-tab id (the engine's WebView id as u64), matching the
    /// `tab_id` stamped on captured requests — the key `list_requests?tab=` /
    /// `list_endpoints?tab=` resolve a tab index to. 0 when unknown.
    pub id: u64,
}

/// A native `alert`/`confirm`/`prompt` the page opened, as `dialog_history` prints it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct DialogRecord {
    /// Epoch milliseconds.
    pub ts: u64,
    /// "alert", "confirm" or "prompt".
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
    pub url: String,
    /// Who closed it and how; none while it is still open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<DialogAnswer>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct DialogAnswer {
    /// "agent" or "user".
    pub by: String,
    pub accept: bool,
    /// Epoch milliseconds.
    pub at: u64,
}

/// What tools may ask of the page they are driving. It addresses the tab the agent
/// is working in. Every method has a default that reports the operation as
/// unsupported, so an engine implements what it can and tools degrade with a clear
/// message instead of failing to build.
pub trait Browser: Send + Sync {
    fn page(&self) -> BoxFuture<'_, Result<PageInfo, String>> {
        Box::pin(async { Err(unsupported("page info")) })
    }

    /// Start loading `url`; resolves once the navigation has been handed to the engine.
    fn navigate<'a>(&'a self, _url: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err(unsupported("navigate")) })
    }

    fn back(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err(unsupported("history back")) })
    }

    fn forward(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err(unsupported("history forward")) })
    }

    fn reload(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err(unsupported("reload")) })
    }

    /// Evaluate JavaScript in the page and return its value as JSON. A thrown
    /// exception is an `Err` carrying its message.
    fn evaluate<'a>(&'a self, _expression: &'a str) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async { Err(unsupported("evaluate")) })
    }

    /// The visible page as PNG bytes.
    fn screenshot(&self) -> BoxFuture<'_, Result<Vec<u8>, String>> {
        Box::pin(async { Err(unsupported("screenshot")) })
    }

    fn input(&self, _input: Input) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err(unsupported("input")) })
    }

    /// Viewport size in CSS pixels.
    fn viewport(&self) -> BoxFuture<'_, Result<(u32, u32), String>> {
        Box::pin(async { Err(unsupported("viewport size")) })
    }

    /// Cookies that would be sent to `url`.
    fn cookies<'a>(&'a self, _url: &'a str) -> BoxFuture<'a, Result<Vec<Cookie>, String>> {
        Box::pin(async { Err(unsupported("reading cookies")) })
    }

    fn set_cookie<'a>(
        &'a self,
        _url: &'a str,
        _cookie: Cookie,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err(unsupported("setting cookies")) })
    }

    fn delete_cookie<'a>(
        &'a self,
        _url: &'a str,
        _name: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err(unsupported("deleting cookies")) })
    }

    /// Every open tab, in tab-strip order.
    fn tabs(&self) -> BoxFuture<'_, Result<Vec<TabInfo>, String>> {
        Box::pin(async { Err(unsupported("listing tabs")) })
    }

    /// Point the tools (and the window) at the tab with this index of [`Browser::tabs`].
    fn select_tab(&self, _index: usize) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err(unsupported("selecting a tab")) })
    }

    /// A handle whose page actions (navigate / evaluate / screenshot / page)
    /// target the tab at `index` instead of the active one, without switching
    /// the user's tab. `None` when the backend can't address tabs by index.
    fn for_tab(&self, _index: usize) -> Option<Arc<dyn Browser>> {
        None
    }

    /// Load `url` in a new tab without selecting it; resolves to the new tab's index.
    fn open_tab<'a>(&'a self, _url: &'a str) -> BoxFuture<'a, Result<usize, String>> {
        Box::pin(async { Err(unsupported("opening a background tab")) })
    }

    /// Choose the answer of the next `alert`/`confirm`/`prompt` the page opens:
    /// OK or Cancel, and for an accepted prompt the text it returns. One-shot.
    fn arm_dialog_answer(
        &self,
        _accept: bool,
        _prompt_text: Option<String>,
    ) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err(unsupported("answering native dialogs")) })
    }

    /// Whether this engine enforces the capture-not-send fence: while the fence is
    /// held, a request that would change server state is answered here and never
    /// sent. A crawl that clicks everything must not run without it.
    fn holds_fenced_requests(&self) -> bool {
        false
    }

    /// Show the page as a phone would (a phone's user agent and width) or as a
    /// desktop again, and reload it.
    fn set_viewport_mode(&self, _mobile: bool) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err(unsupported("switching between desktop and mobile mode")) })
    }

    /// Replace the system clipboard's text.
    fn write_clipboard<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err(unsupported("writing the clipboard")) })
    }

    /// Whether dialogs that open while the agent acts are answered for it, and
    /// the last `limit` native dialogs, oldest first.
    fn dialog_history(
        &self,
        _limit: usize,
    ) -> BoxFuture<'_, Result<(bool, Vec<DialogRecord>), String>> {
        Box::pin(async { Err(unsupported("native dialog handling")) })
    }

    fn clear_dialog_history(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err(unsupported("native dialog handling")) })
    }

    /// With `false`, every dialog waits for the user, also while the agent acts.
    fn set_dialog_auto_dismiss(&self, _enabled: bool) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err(unsupported("native dialog handling")) })
    }

    /// Hand these files to the next file picker the page opens; an empty list
    /// takes back files that no picker asked for.
    fn arm_file_paths(&self, _paths: Vec<String>) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err(unsupported("attaching files to a file input")) })
    }

    /// Run `source` in every new document before its own scripts. Returns an id for
    /// [`Browser::remove_init_script`].
    fn add_init_script<'a>(&'a self, _source: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async { Err(unsupported("init scripts")) })
    }

    fn remove_init_script<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err(unsupported("init scripts")) })
    }
}

/// A browser that supports nothing: for tools that never touch the page, and tests.
pub struct NoBrowser;

impl Browser for NoBrowser {}

/// Everything a tool call can reach.
pub struct ToolContext {
    pub traffic: Arc<Mutex<TrafficStore>>,
    pub browser: Arc<dyn Browser>,
    /// The app's data folder (settings, saved commands, findings).
    pub data_dir: PathBuf,
    state: Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl ToolContext {
    pub fn new(
        traffic: Arc<Mutex<TrafficStore>>,
        browser: Arc<dyn Browser>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            traffic,
            browser,
            data_dir,
            state: Mutex::default(),
        }
    }

    /// A context with an empty traffic store and no browser, for tests.
    pub fn for_tests() -> Self {
        Self::new(Arc::default(), Arc::new(NoBrowser), std::env::temp_dir())
    }

    /// State shared by every call, one value per type, created on first use. A tool
    /// crate keeps what the TypeScript kept in module-level variables here (its own
    /// struct with interior mutability), without this crate knowing about it.
    pub fn state<T: Default + Send + Sync + 'static>(&self) -> Arc<T> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = state
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Arc::new(T::default()));
        // The map is keyed by the type, so the downcast cannot fail.
        entry
            .clone()
            .downcast::<T>()
            .unwrap_or_else(|_| Arc::new(T::default()))
    }
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Add a tool. A later tool with the same name replaces the earlier one.
    pub fn register(&mut self, tool: impl Tool + 'static) {
        let name = tool.def().name;
        self.tools.retain(|existing| existing.def().name != name);
        self.tools.push(Arc::new(tool));
    }

    pub fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .iter()
            .find(|tool| tool.def().name == name)
            .cloned()
    }

    /// Definitions in registration order.
    pub fn defs(&self) -> Vec<ToolDef> {
        self.tools.iter().map(|tool| tool.def()).collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};

    use serde_json::json;

    use super::*;
    use crate::utils::ok;

    /// Run a future that never waits on anything, without an async runtime.
    fn now<T>(mut future: BoxFuture<'_, T>) -> T {
        match future.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the future waited on something"),
        }
    }

    struct Echo(&'static str);

    impl Tool for Echo {
        fn def(&self) -> ToolDef {
            ToolDef::new(self.0, "echoes its arguments", json!({ "type": "object" }))
        }

        fn call<'a>(&'a self, _: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
            Box::pin(async move { ok(format!("{}:{args}", self.0)) })
        }
    }

    #[test]
    fn 등록한_도구를_이름으로_찾아_호출한다() {
        let mut registry = ToolRegistry::default();
        registry.register(Echo("first"));
        registry.register(Echo("second"));
        let names: Vec<String> = registry.defs().into_iter().map(|def| def.name).collect();
        assert_eq!(names, ["first", "second"]);

        let context = ToolContext::for_tests();
        let tool = registry.find("second").unwrap();
        assert_eq!(
            now(tool.call(&context, json!({ "a": 1 }))),
            ok("second:{\"a\":1}")
        );
        assert!(registry.find("missing").is_none());
    }

    #[test]
    fn 같은_이름으로_다시_등록하면_나중_것이_남는다() {
        let mut registry = ToolRegistry::default();
        registry.register(Echo("same"));
        registry.register(Echo("same"));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn 상태는_타입마다_하나이고_호출_사이에_유지된다() {
        #[derive(Default)]
        struct Counter(AtomicUsize);
        #[derive(Default)]
        struct Other(AtomicUsize);

        let context = ToolContext::for_tests();
        context.state::<Counter>().0.fetch_add(2, Ordering::SeqCst);
        context.state::<Counter>().0.fetch_add(3, Ordering::SeqCst);
        assert_eq!(context.state::<Counter>().0.load(Ordering::SeqCst), 5);
        assert_eq!(context.state::<Other>().0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn 엔진이_구현하지_않은_동작은_지원하지_않는다고_알린다() {
        let error = now(NoBrowser.evaluate("1 + 1")).unwrap_err();
        assert_eq!(error, unsupported("evaluate"));
        assert!(now(NoBrowser.cookies("https://a.test/")).is_err());
    }
}
