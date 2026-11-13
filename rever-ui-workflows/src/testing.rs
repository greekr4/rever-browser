//! What the view tests share: a host that records what the views ask of it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{App, Modifiers, Task, VisualTestContext};
use rever_cli_spec::cli_spec::CliData;
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::ThemeMode;

use crate::core::registry::EditorProps;
use crate::core::types::Workflow;
use crate::host::{
    CliRunPayload, CliRunResult, McpToolInfo, OnProgress, PipeProgress, TrafficCall, WorkflowHost,
    WorkflowRunStep, WorkflowStepProgress,
};
use crate::pipeline::resolve::ResolvedPipeNode;

/// Key bindings of both kinds of text field.
pub(crate) fn bind_keys(cx: &mut App) {
    rever_ui_kit::text_input::bind_keys(cx);
    crate::bind_keys(cx);
}

pub(crate) fn tool(name: &str, description: &str) -> McpToolInfo {
    McpToolInfo {
        name: name.into(),
        description: (!description.is_empty()).then(|| description.to_owned()),
        input_schema: serde_json::json!({ "type": "object" }),
    }
}

/// The working copies an editor reported through `on_change`, oldest first.
pub(crate) type Changes = Rc<RefCell<Vec<Workflow>>>;

/// What the panel hands an editor, with `on_change` recorded.
pub(crate) fn props(workflow: Workflow, host: Rc<FakeHost>) -> (EditorProps, Changes) {
    let changes = Changes::default();
    let props = EditorProps {
        workflow,
        on_change: Rc::new({
            let changes = changes.clone();
            move |workflow, _| changes.borrow_mut().push(workflow)
        }),
        host,
        lang: Lang::En,
        theme_mode: ThemeMode::Light,
    };
    (props, changes)
}

/// Whether an element with this debug selector has been painted. GPUI never forgets a
/// selector's bounds, so this says nothing once the element has been on screen: check
/// the view's state for something that should have gone away.
pub(crate) fn shown(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector).is_some()
}

/// Click the middle of the element with this debug selector.
pub(crate) fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let Some(bounds) = cx.debug_bounds(selector) else {
        panic!("no element {selector}");
    };
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

/// Click a text field and replace what it holds.
pub(crate) fn retype(cx: &mut VisualTestContext, selector: &'static str, text: &str) {
    click(cx, selector);
    cx.simulate_keystrokes("cmd-a backspace");
    if !text.is_empty() {
        cx.simulate_input(text);
    }
    cx.run_until_parked();
}

/// Answers at once with what the test put in, and keeps every request.
#[derive(Default)]
pub(crate) struct FakeHost {
    pub tools: Vec<McpToolInfo>,
    pub traffic: RefCell<Vec<TrafficCall>>,
    /// The progress a macro run reports, then returns.
    pub step_results: RefCell<Vec<WorkflowStepProgress>>,
    /// The progress a pipeline run reports.
    pub pipe_results: RefCell<Vec<PipeProgress>>,
    /// When set, a run fails with this instead.
    pub run_error: RefCell<Option<String>>,
    /// When set, a run reports its progress and then never finishes.
    pub hold: Cell<bool>,
    pub compiled: RefCell<Option<Result<CliData, String>>>,
    pub cli_result: RefCell<Option<Result<CliRunResult, String>>>,

    pub runs: RefCell<Vec<Vec<WorkflowRunStep>>>,
    pub pipeline_runs: RefCell<Vec<Vec<ResolvedPipeNode>>>,
    pub cancels: RefCell<usize>,
    pub compile_requests: RefCell<Vec<Vec<String>>>,
    pub cli_runs: RefCell<Vec<CliRunPayload>>,
}

impl WorkflowHost for FakeHost {
    fn list_tools(&self, _: &mut App) -> Task<Vec<McpToolInfo>> {
        Task::ready(self.tools.clone())
    }

    fn run(
        &self,
        steps: Vec<WorkflowRunStep>,
        on_progress: OnProgress<WorkflowStepProgress>,
        cx: &mut App,
    ) -> Task<Result<Vec<WorkflowStepProgress>, String>> {
        self.runs.borrow_mut().push(steps);
        if let Some(error) = self.run_error.borrow().clone() {
            return Task::ready(Err(error));
        }
        let results = self.step_results.borrow().clone();
        for progress in &results {
            on_progress(progress.clone(), cx);
        }
        if self.hold.get() {
            return cx.spawn(async |_| std::future::pending().await);
        }
        Task::ready(Ok(results))
    }

    fn run_pipeline(
        &self,
        nodes: Vec<ResolvedPipeNode>,
        on_progress: OnProgress<PipeProgress>,
        cx: &mut App,
    ) -> Task<Result<bool, String>> {
        self.pipeline_runs.borrow_mut().push(nodes);
        if let Some(error) = self.run_error.borrow().clone() {
            return Task::ready(Err(error));
        }
        for progress in self.pipe_results.borrow().iter() {
            on_progress(progress.clone(), cx);
        }
        if self.hold.get() {
            return cx.spawn(async |_| std::future::pending().await);
        }
        Task::ready(Ok(true))
    }

    fn cancel(&self, _: &mut App) {
        *self.cancels.borrow_mut() += 1;
    }

    fn compile_cli(&self, request_ids: Vec<String>, _: &mut App) -> Task<Result<CliData, String>> {
        self.compile_requests.borrow_mut().push(request_ids);
        Task::ready(
            self.compiled
                .borrow()
                .clone()
                .unwrap_or_else(|| Err("no compile result".to_owned())),
        )
    }

    fn run_cli(&self, payload: CliRunPayload, _: &mut App) -> Task<Result<CliRunResult, String>> {
        self.cli_runs.borrow_mut().push(payload);
        Task::ready(
            self.cli_result
                .borrow()
                .clone()
                .unwrap_or_else(|| Err("no run result".to_owned())),
        )
    }

    fn traffic(&self, _: &App) -> Vec<TrafficCall> {
        self.traffic.borrow().clone()
    }
}
