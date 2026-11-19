//! Port of `src/renderer/src/workflows/core/agent-bridge.ts` — serves the macro and CLI MCP tools.
//!
//! Saved workflows live in the interface's store, so the tool side asks here. The
//! TypeScript reached three zustand stores as globals; [`BridgeHost`] is those stores
//! (workflows, tabs, scan) and the clock, as the application holds them.

use std::future::Future;

use rever_cli_spec::cli_spec::{CliData, CliEffect, parse_extract};
use serde_json::{Map, Value, json};

use super::store::WorkflowsStore;
use super::types::{Workflow, new_workflow_id};
use crate::cli::cli_editor::as_cli;
use crate::js::{js_string, js_trim, js_truthy, number_value, stringify_pretty};
use crate::macros::macro_editor::{MacroData, MacroStep, resolve_steps, step_id};

/// The scan store's sweep intensity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanIntensity {
    Readonly,
    Probe,
    Redteam,
}

/// How long `scan:launch` with autostart waits for the new tab's webview, and how often
/// it looks.
pub const ATTACH_TIMEOUT_MS: u64 = 10_000;
pub const ATTACH_POLL_MS: u64 = 200;

pub trait BridgeHost {
    /// The workflows store (`useWorkflowsStore.getState()`).
    fn with_workflows<R>(&self, f: impl FnOnce(&mut WorkflowsStore) -> R) -> R;

    /// `addTab(url, { activate })`: the new tab's id.
    fn add_tab(&self, url: &str, activate: bool) -> String;
    /// The ids of every open tab.
    fn tab_ids(&self) -> Vec<String>;
    fn close_tab(&self, id: &str);
    /// Whether the tab's webview is attached (`webContentsId != null`).
    fn tab_attached(&self, id: &str) -> bool;

    fn scan_set_target(&self, url: &str);
    fn scan_set_pages(&self, pages: f64);
    fn scan_set_intensity(&self, intensity: ScanIntensity);
    fn scan_open(&self);
    fn scan_ask_confirm(&self);
    /// Starts the scan without waiting for it: it runs for minutes.
    fn scan_start(&self);

    /// `Date.now()`.
    fn now_ms(&self) -> u64;
    fn sleep(&self, ms: u64) -> impl Future<Output = ()>;
}

/// `p[key] != null`.
fn present<'a>(p: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    p.get(key).filter(|value| !value.is_null())
}

/// `String(p[key])`.
fn text(p: &Map<String, Value>, key: &str) -> String {
    js_string(p.get(key))
}

/// Step shape the agent sends: tool + parsed JSON input.
fn to_macro_steps(steps: Option<&Value>) -> Result<Vec<MacroStep>, String> {
    let steps = match steps {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(steps)) => steps,
        Some(_) => return Err("steps.map is not a function".to_owned()),
    };
    steps
        .iter()
        .map(|step| {
            if step.is_null() {
                return Err("Cannot read properties of null (reading 'tool')".to_owned());
            }
            let tool = match step.get("tool") {
                None | Some(Value::Null) => String::new(),
                other => js_string(other),
            };
            let input = step.get("input").filter(|input| !input.is_null());
            let wait_for = step.get("waitFor");
            Ok(MacroStep {
                id: step_id(),
                tool,
                input: input.map_or_else(|| "{}".to_owned(), stringify_pretty),
                wait_for: js_truthy(wait_for).then(|| js_string(wait_for)),
                delay: step.get("delay").and_then(Value::as_f64),
                save_as: None,
            })
        })
        .collect()
}

/// Step shape the agent receives.
fn to_agent_steps(steps: &[MacroStep]) -> Vec<Value> {
    steps
        .iter()
        .map(|step| {
            let mut input = json!({});
            if !js_trim(&step.input).is_empty() {
                match serde_json::from_str(&step.input) {
                    Ok(parsed) => input = parsed,
                    // Keep the raw text visible to the agent so it can fix a broken step.
                    Err(_) => {
                        return json!({ "tool": step.tool, "input": { "__unparsed": step.input } });
                    }
                }
            }
            let mut out = Map::new();
            out.insert("tool".to_owned(), json!(step.tool));
            out.insert("input".to_owned(), input);
            if let Some(wait_for) = step.wait_for.as_ref().filter(|text| !text.is_empty()) {
                out.insert("waitFor".to_owned(), json!(wait_for));
            }
            if let Some(delay) = step.delay {
                out.insert("delay".to_owned(), number_value(delay));
            }
            Value::Object(out)
        })
        .collect()
}

fn find<'a>(
    store: &'a WorkflowsStore,
    kind: &str,
    label: &str,
    id: &str,
) -> Result<&'a Workflow, String> {
    store
        .workflows()
        .iter()
        .find(|workflow| workflow.kind == kind && workflow.id == id)
        .ok_or_else(|| format!("No {label} with id \"{id}\""))
}

fn find_macro<'a>(store: &'a WorkflowsStore, id: &str) -> Result<&'a Workflow, String> {
    find(store, "macro", "macro", id)
}

fn find_cli<'a>(store: &'a WorkflowsStore, id: &str) -> Result<&'a Workflow, String> {
    find(store, "cli", "CLI", id)
}

fn cli_summary(workflow: &Workflow, data: &CliData) -> Value {
    let commands: Vec<Value> = data
        .commands
        .iter()
        .map(|command| {
            let mut row = json!({
                "name": command.name,
                "method": command.method,
                "path": command.path,
                "effect": command.effect,
            });
            if let (Some(extract), Some(row)) = (&command.extract, row.as_object_mut()) {
                let names: Vec<&str> = extract.fields.iter().map(|f| f.name.as_str()).collect();
                row.insert("extract".to_owned(), json!(names.join(", ")));
            }
            row
        })
        .collect();
    json!({
        "id": workflow.id,
        "name": workflow.name,
        "site": data.site,
        "baseUrl": data.base_url,
        "writes": data.writes,
        "commands": commands,
    })
}

fn summary(workflow: &Workflow, data: &MacroData) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("id".to_owned(), json!(workflow.id));
    out.insert("name".to_owned(), json!(workflow.name));
    out.insert(
        "description".to_owned(),
        json!(workflow.description.as_deref().unwrap_or_default()),
    );
    out.insert("stepCount".to_owned(), json!(data.steps.len()));
    out.insert("updatedAt".to_owned(), json!(workflow.updated_at));
    out
}

fn detail(workflow: &Workflow) -> Value {
    let data = MacroData::from_value(&workflow.data);
    let mut out = summary(workflow, &data);
    out.insert(
        "steps".to_owned(),
        Value::Array(to_agent_steps(&data.steps)),
    );
    out.insert("vars".to_owned(), json!(data.vars));
    Value::Object(out)
}

fn cli_value(data: &CliData) -> Value {
    serde_json::to_value(data).unwrap_or_default()
}

/// A failed write surfaces as the request's error, as a throwing `setItem` did.
fn save(store: &mut WorkflowsStore, workflow: Workflow, now_ms: u64) -> Result<(), String> {
    store
        .upsert_at(workflow, now_ms)
        .map_err(|error| error.to_string())
}

/// Every op that only touches the workflows store.
fn store_op(
    store: &mut WorkflowsStore,
    op: &str,
    p: &Map<String, Value>,
    now: u64,
) -> Result<Value, String> {
    match op {
        "macro:list" => Ok(store
            .workflows()
            .iter()
            .filter(|workflow| workflow.kind == "macro")
            .map(|workflow| {
                Value::Object(summary(workflow, &MacroData::from_value(&workflow.data)))
            })
            .collect()),

        "macro:get" => Ok(detail(find_macro(store, &text(p, "id"))?)),

        "macro:create" => {
            let workflow = Workflow {
                id: new_workflow_id(),
                kind: "macro".to_owned(),
                name: present(p, "name")
                    .map_or_else(|| "Untitled macro".to_owned(), |name| js_string(Some(name))),
                description: js_truthy(p.get("description")).then(|| text(p, "description")),
                data: MacroData {
                    steps: to_macro_steps(p.get("steps"))?,
                    vars: if js_truthy(p.get("vars")) {
                        text(p, "vars")
                    } else {
                        String::new()
                    },
                }
                .to_value(),
                created_at: now,
                updated_at: now,
            };
            save(store, workflow.clone(), now)?;
            Ok(detail(&workflow))
        }

        "macro:update" => {
            let workflow = find_macro(store, &text(p, "id"))?;
            let data = MacroData::from_value(&workflow.data);
            let next = Workflow {
                name: present(p, "name")
                    .map_or_else(|| workflow.name.clone(), |name| js_string(Some(name))),
                description: present(p, "description")
                    .map(|description| js_string(Some(description)))
                    .or_else(|| workflow.description.clone()),
                data: MacroData {
                    steps: match present(p, "steps") {
                        Some(steps) => to_macro_steps(Some(steps))?,
                        None => data.steps,
                    },
                    vars: present(p, "vars").map_or(data.vars, |vars| js_string(Some(vars))),
                }
                .to_value(),
                ..workflow.clone()
            };
            save(store, next.clone(), now)?;
            Ok(detail(&next))
        }

        "macro:remove-steps" => {
            let workflow = find_macro(store, &text(p, "id"))?;
            let mut data = MacroData::from_value(&workflow.data);
            let indexes: Vec<f64> = match p.get("indexes") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(items)) => items.iter().filter_map(Value::as_f64).collect(),
                // A string is iterable too, but its characters equal no index.
                Some(Value::String(_)) => Vec::new(),
                Some(other) => return Err(format!("{} is not iterable", js_string(Some(other)))),
            };
            let mut position = 0.0;
            data.steps.retain(|_| {
                let keep = !indexes.contains(&position);
                position += 1.0;
                keep
            });
            let next = Workflow {
                data: data.to_value(),
                ..workflow.clone()
            };
            save(store, next.clone(), now)?;
            Ok(detail(&next))
        }

        // run_macro asks us to resolve rather than resolving on the tool side, so
        // {{var}} substitution has exactly one implementation.
        "macro:resolve" => {
            let workflow = find_macro(store, &text(p, "id"))?;
            let data = MacroData::from_value(&workflow.data);
            let vars = present(p, "vars").map_or(data.vars, |vars| js_string(Some(vars)));
            let steps = resolve_steps(&data.steps, &vars)?;
            if steps.is_empty() {
                return Err(format!("Macro \"{}\" has no steps", workflow.name));
            }
            Ok(json!({ "name": workflow.name, "steps": steps }))
        }

        "macro:delete" => {
            let id = text(p, "id");
            find_macro(store, &id)?;
            store.remove(&id).map_err(|error| error.to_string())?;
            Ok(json!({ "deleted": true }))
        }

        "cli:list" => Ok(store
            .workflows()
            .iter()
            .filter(|workflow| workflow.kind == "cli")
            .map(|workflow| cli_summary(workflow, &as_cli(&workflow.data)))
            .collect()),

        "cli:get" => {
            let workflow = find_cli(store, &text(p, "id"))?;
            Ok(json!({
                "id": workflow.id,
                "name": workflow.name,
                "data": cli_value(&as_cli(&workflow.data)),
            }))
        }

        "cli:save" => {
            let incoming = as_cli(p.get("data").unwrap_or(&Value::Null));
            if incoming.commands.is_empty() {
                return Err("CLI has no commands".to_owned());
            }
            let id = match p.get("id") {
                Some(Value::String(id)) if !id.is_empty() => id.clone(),
                _ => new_workflow_id(),
            };
            let existing = store
                .workflows()
                .iter()
                .find(|workflow| workflow.kind == "cli" && workflow.id == id);
            let data = CliData {
                writes: false,
                ..incoming
            };
            let workflow = Workflow {
                name: present(p, "name")
                    .map(|name| js_string(Some(name)))
                    .or_else(|| existing.map(|existing| existing.name.clone()))
                    .unwrap_or_else(|| data.site.clone()),
                id,
                kind: "cli".to_owned(),
                description: None,
                data: cli_value(&data),
                created_at: existing.map_or(now, |existing| existing.created_at),
                updated_at: now,
            };
            save(store, workflow.clone(), now)?;
            Ok(cli_summary(&workflow, &data))
        }

        "cli:set-writes" => {
            let workflow = find_cli(store, &text(p, "id"))?;
            let data = CliData {
                writes: p.get("writes") == Some(&Value::Bool(true)),
                ..as_cli(&workflow.data)
            };
            let next = Workflow {
                data: cli_value(&data),
                ..workflow.clone()
            };
            save(store, next.clone(), now)?;
            Ok(cli_summary(&next, &data))
        }

        "cli:set-extract" => {
            let workflow = find_cli(store, &text(p, "id"))?;
            let mut data = as_cli(&workflow.data);
            let name = present(p, "command").map(|name| js_string(Some(name)));
            let name = name.unwrap_or_default();
            let extract = parse_extract(p.get("extract").unwrap_or(&Value::Null))?
                .ok_or("extract is required")?;
            if !data.commands.iter().any(|command| command.name == name) {
                return Err(format!("unknown command: {name}"));
            }
            for command in &mut data.commands {
                if command.name == name {
                    command.extract = Some(extract.clone());
                }
            }
            let next = Workflow {
                data: cli_value(&data),
                ..workflow.clone()
            };
            save(store, next.clone(), now)?;
            Ok(cli_summary(&next, &data))
        }

        "cli:effect" => {
            let data = as_cli(&find_cli(store, &text(p, "id"))?.data);
            let name = text(p, "command");
            let effect = data
                .commands
                .iter()
                .find(|command| command.name == name)
                .map_or(CliEffect::Unknown, |command| command.effect);
            Ok(json!(effect))
        }

        _ => Err(format!("Unknown bridge op \"{op}\"")),
    }
}

/// Answer one request of the tool side. `Err` is the message the TypeScript threw.
pub async fn handle_agent_request<H: BridgeHost>(
    host: &H,
    op: &str,
    payload: &Value,
) -> Result<Value, String> {
    let empty = Map::new();
    let p = payload.as_object().unwrap_or(&empty);

    match op {
        "tab:open" => {
            // Scan opens same-site pages in background tabs; don't steal focus.
            host.add_tab(&text(p, "url"), false);
            Ok(json!({ "ok": true }))
        }

        "scan:launch" => {
            // redteam_start: arm the popup and stop at confirm — the deterministic
            // pipeline (and any active probing) only runs on the user's click.
            // EXCEPTION: autostart (gated to loopback by the MCP tool) runs the whole
            // pipeline without a click, so the agent can self-drive local benchmarks.
            let intensity = match text(p, "intensity").as_str() {
                "probe" => ScanIntensity::Probe,
                "redteam" => ScanIntensity::Redteam,
                _ => ScanIntensity::Readonly,
            };
            let url = text(p, "url");
            host.scan_set_target(&url);
            if let Some(Value::Number(pages)) = p.get("pages") {
                host.scan_set_pages(pages.as_f64().unwrap_or(f64::NAN));
            }
            host.scan_set_intensity(intensity);
            host.scan_open();
            if js_truthy(p.get("autostart")) {
                // The UI flow assumes the user already has the target open in the
                // active tab; autostart has no such tab, so open one and wait for its
                // webview to attach before discovery drives browser_navigate —
                // otherwise every turn fails "no active browser target" and the worker
                // pool respawns forever.
                let tab_id = host.add_tab(&url, true);
                // Clean slate: a prior (or crashed) self-driven run leaves its tabs
                // behind — stale /login + /search tabs pile up, and a worker that
                // lists tabs can't tell which is "the" target, stalling the scan. Close
                // everything except the fresh target tab. (Self-driven only; a manual
                // UI scan keeps the user's tabs.)
                for id in host.tab_ids() {
                    if id != tab_id {
                        host.close_tab(&id);
                    }
                }
                let attach_start = host.now_ms();
                while host.now_ms().saturating_sub(attach_start) < ATTACH_TIMEOUT_MS {
                    if host.tab_attached(&tab_id) {
                        break;
                    }
                    host.sleep(ATTACH_POLL_MS).await;
                }
                // Non-blocking: the scan runs for minutes; return now, caller polls.
                host.scan_start();
                return Ok(json!({ "ok": true, "phase": "running" }));
            }
            host.scan_ask_confirm();
            Ok(json!({ "ok": true, "phase": "confirm" }))
        }

        _ => {
            let now = host.now_ms();
            host.with_workflows(|store| store_op(store, op, p, now))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::pin::pin;
    use std::rc::Rc;
    use std::task::{Context, Poll, Waker};

    use rever_ui_state::MemoryStorage;

    use super::*;

    /// The fakes never wait on anything outside, so polling in a loop finishes.
    fn block_on<F: Future>(future: F) -> F::Output {
        let mut future = pin!(future);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
                return output;
            }
        }
    }

    struct FakeHost {
        store: RefCell<WorkflowsStore>,
        calls: RefCell<Vec<String>>,
        tabs: RefCell<Vec<String>>,
        /// The clock value at which the new tab's webview attaches.
        attach_at: Cell<Option<u64>>,
        now: Cell<u64>,
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                store: RefCell::new(WorkflowsStore::new(Rc::new(MemoryStorage::new()))),
                calls: RefCell::default(),
                tabs: RefCell::new(vec!["t1".into(), "t2".into()]),
                attach_at: Cell::new(None),
                now: Cell::new(1_000),
            }
        }

        fn log(&self, call: impl Into<String>) {
            self.calls.borrow_mut().push(call.into());
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn request(&self, op: &str, payload: Value) -> Result<Value, String> {
            block_on(handle_agent_request(self, op, &payload))
        }

        fn stored(&self, id: &str) -> Workflow {
            self.store
                .borrow()
                .workflows()
                .iter()
                .find(|workflow| workflow.id == id)
                .cloned()
                .unwrap()
        }
    }

    impl BridgeHost for FakeHost {
        fn with_workflows<R>(&self, f: impl FnOnce(&mut WorkflowsStore) -> R) -> R {
            f(&mut self.store.borrow_mut())
        }
        fn add_tab(&self, url: &str, activate: bool) -> String {
            self.log(format!("add_tab {url} {activate}"));
            self.tabs.borrow_mut().push("new".into());
            "new".into()
        }
        fn tab_ids(&self) -> Vec<String> {
            self.tabs.borrow().clone()
        }
        fn close_tab(&self, id: &str) {
            self.log(format!("close_tab {id}"));
            self.tabs.borrow_mut().retain(|tab| tab != id);
        }
        fn tab_attached(&self, _: &str) -> bool {
            self.attach_at.get().is_some_and(|at| self.now.get() >= at)
        }
        fn scan_set_target(&self, url: &str) {
            self.log(format!("target {url}"));
        }
        fn scan_set_pages(&self, pages: f64) {
            self.log(format!("pages {pages}"));
        }
        fn scan_set_intensity(&self, intensity: ScanIntensity) {
            self.log(format!("intensity {intensity:?}"));
        }
        fn scan_open(&self) {
            self.log("open");
        }
        fn scan_ask_confirm(&self) {
            self.log("ask_confirm");
        }
        fn scan_start(&self) {
            self.log(format!("start at {}", self.now.get()));
        }
        fn now_ms(&self) -> u64 {
            self.now.get()
        }
        async fn sleep(&self, ms: u64) {
            self.now.set(self.now.get() + ms);
        }
    }

    fn create_macro(host: &FakeHost, payload: Value) -> String {
        let created = host.request("macro:create", payload).unwrap();
        created["id"].as_str().unwrap().to_owned()
    }

    fn command(name: &str, effect: &str) -> Value {
        json!({
            "name": name,
            "desc": "",
            "method": "GET",
            "path": "/api/items",
            "effect": effect,
            "evidenceRequestIds": ["1.1"],
            "params": {},
            "query": { "q": { "kind": "string", "required": false } },
            "sessionHeaders": [],
            "responsePointers": [],
        })
    }

    fn cli_data(commands: Vec<Value>) -> Value {
        json!({
            "site": "shop",
            "title": "Shop",
            "baseUrl": "https://shop.test",
            "commands": commands,
            "writes": true,
        })
    }

    // ── tab:open, scan:launch ───────────────────────────────────────────────

    #[test]
    fn 탭_열기는_배경_탭으로_연다() {
        let host = FakeHost::new();
        assert_eq!(
            host.request("tab:open", json!({ "url": "https://a.test/" })),
            Ok(json!({ "ok": true }))
        );
        assert_eq!(host.calls(), ["add_tab https://a.test/ false"]);
    }

    #[test]
    fn url이_없으면_undefined라는_글자를_연다() {
        let host = FakeHost::new();
        host.request("tab:open", Value::Null).unwrap();
        assert_eq!(host.calls(), ["add_tab undefined false"]);
    }

    #[test]
    fn 스캔_실행은_확인_단계에서_멈춘다() {
        let host = FakeHost::new();
        assert_eq!(
            host.request(
                "scan:launch",
                json!({ "url": "https://a.test/", "pages": 5, "intensity": "probe" })
            ),
            Ok(json!({ "ok": true, "phase": "confirm" }))
        );
        assert_eq!(
            host.calls(),
            [
                "target https://a.test/",
                "pages 5",
                "intensity Probe",
                "open",
                "ask_confirm"
            ]
        );
    }

    #[test]
    fn 모르는_강도는_읽기_전용이고_숫자가_아닌_페이지_수는_넘기지_않는다() {
        let host = FakeHost::new();
        host.request(
            "scan:launch",
            json!({ "url": "u", "pages": "5", "intensity": "nuke" }),
        )
        .unwrap();
        assert_eq!(
            host.calls(),
            ["target u", "intensity Readonly", "open", "ask_confirm"]
        );

        let host = FakeHost::new();
        host.request("scan:launch", json!({ "url": "u", "intensity": "redteam" }))
            .unwrap();
        assert!(host.calls().contains(&"intensity Redteam".to_owned()));
    }

    #[test]
    fn 자동_시작은_새_탭만_남기고_웹뷰가_붙으면_스캔을_시작한다() {
        let host = FakeHost::new();
        // 두 번 기다린 뒤(400ms) 붙는다.
        host.attach_at.set(Some(1_400));
        assert_eq!(
            host.request(
                "scan:launch",
                json!({ "url": "http://127.0.0.1/", "autostart": true })
            ),
            Ok(json!({ "ok": true, "phase": "running" }))
        );
        assert_eq!(
            host.calls(),
            [
                "target http://127.0.0.1/",
                "intensity Readonly",
                "open",
                "add_tab http://127.0.0.1/ true",
                "close_tab t1",
                "close_tab t2",
                "start at 1400"
            ]
        );
        assert_eq!(*host.tabs.borrow(), ["new"]);
    }

    #[test]
    fn 웹뷰가_붙지_않아도_십_초_뒤에는_스캔을_시작한다() {
        let host = FakeHost::new();
        host.request("scan:launch", json!({ "url": "u", "autostart": 1 }))
            .unwrap();
        assert_eq!(host.calls().last().unwrap(), "start at 11000");
        // 확인 단계는 거치지 않는다.
        assert!(!host.calls().contains(&"ask_confirm".to_owned()));
    }

    #[test]
    fn 자동_시작이_거짓_값이면_확인_단계로_간다() {
        for autostart in [json!(false), json!(0), json!(""), json!(null)] {
            let host = FakeHost::new();
            let reply = host
                .request("scan:launch", json!({ "url": "u", "autostart": autostart }))
                .unwrap();
            assert_eq!(reply["phase"], "confirm");
        }
    }

    // ── macro:* ─────────────────────────────────────────────────────────────

    #[test]
    fn 매크로를_만들면_저장하고_상세를_돌려준다() {
        let host = FakeHost::new();
        let created = host
            .request(
                "macro:create",
                json!({
                    "name": "Login",
                    "description": "signs in",
                    "steps": [
                        { "tool": "browser_navigate", "input": { "url": "https://a.test" } },
                        { "tool": "browser_click", "waitFor": "#go", "delay": 200 }
                    ],
                    "vars": "{ \"user\": \"admin\" }"
                }),
            )
            .unwrap();
        let id = created["id"].as_str().unwrap();
        assert!(id.starts_with('w'));
        assert_eq!(
            created,
            json!({
                "id": id,
                "name": "Login",
                "description": "signs in",
                "stepCount": 2,
                "updatedAt": 1000,
                "steps": [
                    { "tool": "browser_navigate", "input": { "url": "https://a.test" } },
                    { "tool": "browser_click", "input": {}, "waitFor": "#go", "delay": 200 }
                ],
                "vars": "{ \"user\": \"admin\" }"
            })
        );

        let stored = host.stored(id);
        assert_eq!(stored.kind, "macro");
        assert_eq!(stored.created_at, 1000);
        let data = MacroData::from_value(&stored.data);
        // 입력은 두 칸 들여쓴 JSON 글자로 저장된다.
        assert_eq!(data.steps[0].input, "{\n  \"url\": \"https://a.test\"\n}");
        assert_eq!(data.steps[1].input, "{}");
        assert!(data.steps[0].id.starts_with('s'));
        assert_eq!(data.steps[1].wait_for.as_deref(), Some("#go"));
        assert_eq!(data.steps[1].delay, Some(200.0));
    }

    #[test]
    fn 이름과_설명과_단계가_없어도_매크로를_만든다() {
        let host = FakeHost::new();
        let created = host.request("macro:create", json!({})).unwrap();
        assert_eq!(created["name"], "Untitled macro");
        assert_eq!(created["description"], "");
        assert_eq!(created["stepCount"], 0);
        assert_eq!(created["vars"], "");
        let stored = host.stored(created["id"].as_str().unwrap());
        assert_eq!(stored.description, None);
    }

    #[test]
    fn 단계_목록이_배열이_아니면_오류다() {
        let host = FakeHost::new();
        assert_eq!(
            host.request("macro:create", json!({ "steps": "click" })),
            Err("steps.map is not a function".to_owned())
        );
        assert_eq!(
            host.request("macro:create", json!({ "steps": [null] })),
            Err("Cannot read properties of null (reading 'tool')".to_owned())
        );
        assert!(host.store.borrow().workflows().is_empty());
    }

    #[test]
    fn 매크로_목록은_매크로만_요약한다() {
        let host = FakeHost::new();
        let id = create_macro(&host, json!({ "name": "A", "steps": [{ "tool": "t" }] }));
        host.request(
            "cli:save",
            json!({ "data": cli_data(vec![command("list", "read")]) }),
        )
        .unwrap();
        assert_eq!(
            host.request("macro:list", json!({})),
            Ok(json!([{
                "id": id,
                "name": "A",
                "description": "",
                "stepCount": 1,
                "updatedAt": 1000
            }]))
        );
    }

    #[test]
    fn 없는_매크로를_찾으면_오류다() {
        let host = FakeHost::new();
        for op in [
            "macro:get",
            "macro:update",
            "macro:remove-steps",
            "macro:resolve",
            "macro:delete",
        ] {
            assert_eq!(
                host.request(op, json!({ "id": "nope" })),
                Err("No macro with id \"nope\"".to_owned()),
                "{op}"
            );
        }
        // id가 없으면 undefined라는 글자로 찾는다.
        assert_eq!(
            host.request("macro:get", json!({})),
            Err("No macro with id \"undefined\"".to_owned())
        );
    }

    #[test]
    fn 깨진_입력은_원문_그대로_에이전트에게_보여준다() {
        let host = FakeHost::new();
        let id = create_macro(&host, json!({ "steps": [{ "tool": "t" }] }));
        host.with_workflows(|store| {
            let mut workflow = store.workflows()[0].clone();
            let mut data = MacroData::from_value(&workflow.data);
            data.steps[0].input = "{ broken".into();
            data.steps[0].wait_for = Some("#x".into());
            workflow.data = data.to_value();
            store.upsert_at(workflow, 1).unwrap();
        });
        let got = host.request("macro:get", json!({ "id": id })).unwrap();
        // 기다림 조건 없이 도구와 원문만 돌려준다.
        assert_eq!(
            got["steps"],
            json!([{ "tool": "t", "input": { "__unparsed": "{ broken" } }])
        );
    }

    #[test]
    fn 매크로_수정은_준_필드만_바꾼다() {
        let host = FakeHost::new();
        let id = create_macro(
            &host,
            json!({ "name": "A", "description": "d", "steps": [{ "tool": "t" }], "vars": "{}" }),
        );
        host.now.set(2_000);
        let updated = host
            .request("macro:update", json!({ "id": id, "name": "B" }))
            .unwrap();
        assert_eq!(updated["name"], "B");
        assert_eq!(updated["description"], "d");
        assert_eq!(updated["stepCount"], 1);
        assert_eq!(updated["vars"], "{}");
        // 돌려주는 요약의 수정 시각은 고치기 전 값이고, 저장된 것은 지금이다.
        assert_eq!(updated["updatedAt"], 1000);
        assert_eq!(host.stored(&id).updated_at, 2000);
        assert_eq!(host.stored(&id).created_at, 1000);

        let updated = host
            .request(
                "macro:update",
                json!({ "id": id, "description": "", "steps": [], "vars": "" }),
            )
            .unwrap();
        assert_eq!(updated["name"], "B");
        assert_eq!(updated["description"], "");
        assert_eq!(updated["stepCount"], 0);
        assert_eq!(updated["vars"], "");
    }

    #[test]
    fn 단계를_위치로_지운다() {
        let host = FakeHost::new();
        let id = create_macro(
            &host,
            json!({ "steps": [{ "tool": "a" }, { "tool": "b" }, { "tool": "c" }, { "tool": "d" }] }),
        );
        let after = host
            .request(
                "macro:remove-steps",
                json!({ "id": id, "indexes": [0, 2, 9, "1"] }),
            )
            .unwrap();
        // 글자 "1"은 숫자 1이 아니다.
        assert_eq!(
            after["steps"],
            json!([{ "tool": "b", "input": {} }, { "tool": "d", "input": {} }])
        );
        // 위치가 없으면 그대로다.
        let same = host
            .request("macro:remove-steps", json!({ "id": id }))
            .unwrap();
        assert_eq!(same["stepCount"], 2);
    }

    #[test]
    fn 매크로를_풀어_실행할_단계로_돌려준다() {
        let host = FakeHost::new();
        let id = create_macro(
            &host,
            json!({
                "name": "Open",
                "steps": [{ "tool": "browser_navigate", "input": { "url": "https://{{host}}/" } }],
                "vars": "{ \"host\": \"a.test\" }"
            }),
        );
        assert_eq!(
            host.request("macro:resolve", json!({ "id": id })),
            Ok(json!({
                "name": "Open",
                "steps": [{ "tool": "browser_navigate", "input": { "url": "https://a.test/" } }]
            }))
        );
        // 요청이 준 변수가 저장된 변수를 대신한다.
        let resolved = host
            .request(
                "macro:resolve",
                json!({ "id": id, "vars": "{ \"host\": \"b.test\" }" }),
            )
            .unwrap();
        assert_eq!(resolved["steps"][0]["input"]["url"], "https://b.test/");
        assert_eq!(
            host.request("macro:resolve", json!({ "id": id, "vars": "{ nope" })),
            Err("Variables are not valid JSON".to_owned())
        );
    }

    #[test]
    fn 단계가_없는_매크로는_풀_수_없다() {
        let host = FakeHost::new();
        let id = create_macro(&host, json!({ "name": "Empty" }));
        assert_eq!(
            host.request("macro:resolve", json!({ "id": id })),
            Err("Macro \"Empty\" has no steps".to_owned())
        );
    }

    #[test]
    fn 매크로를_지운다() {
        let host = FakeHost::new();
        let id = create_macro(&host, json!({}));
        assert_eq!(
            host.request("macro:delete", json!({ "id": id })),
            Ok(json!({ "deleted": true }))
        );
        assert!(host.store.borrow().workflows().is_empty());
    }

    // ── cli:* ───────────────────────────────────────────────────────────────

    #[test]
    fn cli를_저장하면_쓰기를_끈_채로_요약을_돌려준다() {
        let host = FakeHost::new();
        let saved = host
            .request(
                "cli:save",
                json!({ "data": cli_data(vec![command("list", "read"), command("buy", "write")]) }),
            )
            .unwrap();
        let id = saved["id"].as_str().unwrap();
        assert_eq!(
            saved,
            json!({
                "id": id,
                // 이름이 없으면 사이트 이름을 쓴다.
                "name": "shop",
                "site": "shop",
                "baseUrl": "https://shop.test",
                "writes": false,
                "commands": [
                    { "name": "list", "method": "GET", "path": "/api/items", "effect": "read" },
                    { "name": "buy", "method": "GET", "path": "/api/items", "effect": "write" }
                ]
            })
        );
        let stored = host.stored(id);
        assert_eq!(stored.kind, "cli");
        assert_eq!(stored.data["writes"], false);
        assert_eq!(stored.data["baseUrl"], "https://shop.test");
        assert_eq!(host.request("cli:list", json!({})), Ok(json!([saved])));
    }

    #[test]
    fn 명령이_없는_cli는_저장하지_않는다() {
        let host = FakeHost::new();
        for payload in [
            json!({}),
            json!({ "data": cli_data(vec![]) }),
            // 명령 모양이 아닌 항목은 명령이 아니다.
            json!({ "data": cli_data(vec![json!({ "name": "x" })]) }),
        ] {
            assert_eq!(
                host.request("cli:save", payload),
                Err("CLI has no commands".to_owned())
            );
        }
    }

    #[test]
    fn 같은_id로_저장하면_이름과_만든_시각을_지킨다() {
        let host = FakeHost::new();
        let saved = host
            .request(
                "cli:save",
                json!({ "name": "My shop", "data": cli_data(vec![command("list", "read")]) }),
            )
            .unwrap();
        let id = saved["id"].as_str().unwrap().to_owned();
        host.now.set(5_000);
        let again = host
            .request(
                "cli:save",
                json!({ "id": id, "data": cli_data(vec![command("search", "read")]) }),
            )
            .unwrap();
        assert_eq!(again["id"], id.as_str());
        assert_eq!(again["name"], "My shop");
        assert_eq!(again["commands"][0]["name"], "search");
        assert_eq!(host.stored(&id).created_at, 1000);
        assert_eq!(host.stored(&id).updated_at, 5000);
        assert_eq!(host.store.borrow().workflows().len(), 1);
    }

    #[test]
    fn cli를_id로_가져온다() {
        let host = FakeHost::new();
        let saved = host
            .request(
                "cli:save",
                json!({ "data": cli_data(vec![command("list", "read")]) }),
            )
            .unwrap();
        let got = host
            .request("cli:get", json!({ "id": saved["id"] }))
            .unwrap();
        assert_eq!(got["name"], "shop");
        assert_eq!(got["data"]["site"], "shop");
        assert_eq!(
            got["data"]["commands"][0]["evidenceRequestIds"],
            json!(["1.1"])
        );
        assert_eq!(
            host.request("cli:get", json!({ "id": "nope" })),
            Err("No CLI with id \"nope\"".to_owned())
        );
    }

    #[test]
    fn 쓰기_허용은_true일_때만_켠다() {
        let host = FakeHost::new();
        let saved = host
            .request(
                "cli:save",
                json!({ "data": cli_data(vec![command("list", "read")]) }),
            )
            .unwrap();
        let id = saved["id"].clone();
        let on = host
            .request("cli:set-writes", json!({ "id": id, "writes": true }))
            .unwrap();
        assert_eq!(on["writes"], true);
        for writes in [json!("true"), json!(1), json!(null)] {
            let off = host
                .request("cli:set-writes", json!({ "id": id, "writes": writes }))
                .unwrap();
            assert_eq!(off["writes"], false);
        }
    }

    #[test]
    fn 추출_규칙을_명령에_붙인다() {
        let host = FakeHost::new();
        let saved = host
            .request(
                "cli:save",
                json!({ "data": cli_data(vec![command("list", "read")]) }),
            )
            .unwrap();
        let id = saved["id"].clone();
        let extract = json!({
            "selector": ".item",
            "fields": [{ "name": "title" }, { "name": "link", "selector": "a", "attr": "href" }]
        });
        let after = host
            .request(
                "cli:set-extract",
                json!({ "id": id, "command": "list", "extract": extract }),
            )
            .unwrap();
        assert_eq!(after["commands"][0]["extract"], "title, link");
        let stored = host.stored(id.as_str().unwrap());
        assert_eq!(stored.data["commands"][0]["extract"]["limit"], 5);

        assert_eq!(
            host.request("cli:set-extract", json!({ "id": id, "command": "list" })),
            Err("extract is required".to_owned())
        );
        assert_eq!(
            host.request(
                "cli:set-extract",
                json!({ "id": id, "command": "list", "extract": "x" })
            ),
            Err("extract must be an object".to_owned())
        );
        assert_eq!(
            host.request(
                "cli:set-extract",
                json!({ "id": id, "command": "nope", "extract": extract })
            ),
            Err("unknown command: nope".to_owned())
        );
        assert_eq!(
            host.request("cli:set-extract", json!({ "id": id, "extract": extract })),
            Err("unknown command: ".to_owned())
        );
    }

    #[test]
    fn 명령의_효과를_알려주고_모르는_명령은_unknown이다() {
        let host = FakeHost::new();
        let saved = host
            .request(
                "cli:save",
                json!({ "data": cli_data(vec![command("list", "read"), command("buy", "write")]) }),
            )
            .unwrap();
        let id = saved["id"].clone();
        let effect = |name: &str| {
            host.request("cli:effect", json!({ "id": id, "command": name }))
                .unwrap()
        };
        assert_eq!(effect("list"), "read");
        assert_eq!(effect("buy"), "write");
        assert_eq!(effect("nope"), "unknown");
        assert_eq!(
            host.request("cli:effect", json!({ "id": "nope", "command": "list" })),
            Err("No CLI with id \"nope\"".to_owned())
        );
    }

    /// Each line is one request the TypeScript `handleAgentRequest` answered under bun,
    /// with `Date.now()` pinned to 1000: label, op, payload, the placeholder the new
    /// workflow's id is written as, and the answer (or `ERR` and the thrown message).
    /// The requests build on each other, so they are replayed in order.
    #[test]
    fn 타입스크립트가_답한_요청을_순서대로_똑같이_답한다() {
        let host = FakeHost::new();
        // (placeholder, the id this run gave)
        let mut ids: Vec<(String, String)> = Vec::new();
        let mut replayed = 0;
        for line in include_str!("agent_bridge_replay.tsv").lines() {
            let columns: Vec<&str> = line.split('\t').collect();
            let [label, op, payload, name_as, expected] = columns[..] else {
                panic!("malformed line: {line}");
            };
            let mut payload = payload.to_owned();
            for (placeholder, actual) in &ids {
                payload = payload.replace(&format!("\"{placeholder}\""), &format!("\"{actual}\""));
            }
            let answer = host.request(op, serde_json::from_str(&payload).unwrap());
            if let (false, Ok(answer)) = (name_as.is_empty(), &answer) {
                ids.push((
                    name_as.to_owned(),
                    answer["id"].as_str().unwrap().to_owned(),
                ));
            }
            let mut got = match answer {
                Ok(answer) => answer.to_string(),
                Err(error) => format!("ERR {error}"),
            };
            for (placeholder, actual) in &ids {
                got = got.replace(actual, placeholder);
            }
            // Through the same serializer, so `1e+21` and `1e21` compare equal while
            // the order of keys still counts.
            let expected = match expected.strip_prefix("ERR ") {
                Some(_) => expected.to_owned(),
                None => serde_json::from_str::<Value>(expected).unwrap().to_string(),
            };
            assert_eq!(got, expected, "{label}");
            replayed += 1;
        }
        assert_eq!(replayed, 42);
    }

    #[test]
    fn 모르는_요청은_오류다() {
        let host = FakeHost::new();
        assert_eq!(
            host.request("macro:explode", json!({})),
            Err("Unknown bridge op \"macro:explode\"".to_owned())
        );
    }

    #[test]
    fn 객체가_아닌_요청_본문은_빈_객체로_본다() {
        let host = FakeHost::new();
        assert_eq!(host.request("macro:list", json!("text")), Ok(json!([])));
        assert_eq!(host.request("macro:list", json!([1, 2])), Ok(json!([])));
    }
}
