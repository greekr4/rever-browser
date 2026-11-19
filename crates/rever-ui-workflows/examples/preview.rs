//! Opens this crate's views alone with fixture data. See rever_ui_kit::preview.
//!
//! The panel fills the window as it fills the app's slide-up panel, with one saved
//! workflow of each kind. The line at the bottom shows the last prompt a template
//! asked the host to put in the chat input.
//!   REVER_PREVIEW_EDITOR=macro|pipeline|cli|template|list|missing  the editor that is open (default macro;
//!                                                                  list: the workflow list, missing: a kind with no editor)
//!   REVER_PREVIEW_RUN=1                                            press Run in the open editor (macro, pipeline, cli)
//!   REVER_PREVIEW_DELETE=1                                         with list: the delete confirmation is open
//!   REVER_PREVIEW_EMPTY=1                                          start with no saved workflows
//!   REVER_THEME, REVER_LANG                                        as in the app

use std::rc::Rc;

use gpui::{App, Context, Entity, SharedString, Task, Window, div, prelude::*, px};
use rever_cli_spec::cli_spec::{CapturedCall, CliData, CompileOptions, compile_cli};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};
use rever_ui_state::{MemoryStorage, Storage};
use rever_ui_workflows::cli::cli_editor::CliEditor;
use rever_ui_workflows::core::store::STORAGE_KEY;
use rever_ui_workflows::host::{
    BranchTaken, CliRunPayload, CliRunResult, McpToolInfo, OnProgress, PipeProgress, PipeStatus,
    StepStatus, TrafficCall, WorkflowRunStep, WorkflowStepProgress,
};
use rever_ui_workflows::macros::macro_editor::MacroEditor;
use rever_ui_workflows::pipeline::pipeline_editor::PipelineEditor;
use rever_ui_workflows::pipeline::resolve::ResolvedPipeNode;
use rever_ui_workflows::{
    UseInChat, WorkflowHost, WorkflowPanel, WorkflowsStore, default_registry,
};
use serde_json::{Value, json};

/// (request id, method, URL, resource type)
const TRAFFIC: &[(&str, &str, &str, &str)] = &[
    ("1.1", "GET", "https://shop.example.com/", "Document"),
    (
        "1.2",
        "GET",
        "https://shop.example.com/assets/app.77e0c4.js",
        "Script",
    ),
    (
        "1.3",
        "GET",
        "https://shop.example.com/api/products?category=keyboard&page=2",
        "Fetch",
    ),
    ("1.4", "GET", "https://shop.example.com/api/cart", "XHR"),
    (
        "1.5",
        "POST",
        "https://shop.example.com/api/cart/items",
        "Fetch",
    ),
    (
        "1.6",
        "GET",
        "https://shop.example.com/api/search?q=기계식+키보드&sort=price",
        "XHR",
    ),
];

fn tools() -> Vec<McpToolInfo> {
    let tool = |name: &str, description: &str, schema: Value| McpToolInfo {
        name: name.to_owned(),
        description: Some(description.to_owned()),
        input_schema: schema,
    };
    vec![
        tool(
            "browser_navigate",
            "Navigate the active tab to a URL and wait for the page to load.",
            json!({
                "type": "object",
                "properties": { "url": { "type": "string", "description": "Absolute URL" } },
                "required": ["url"]
            }),
        ),
        tool(
            "browser_click",
            "Click an element by its snapshot ref.",
            json!({ "type": "object", "properties": { "ref": { "type": "string" } }, "required": ["ref"] }),
        ),
        tool(
            "browser_type",
            "Type text into an input by its snapshot ref.",
            json!({
                "type": "object",
                "properties": { "ref": { "type": "string" }, "text": { "type": "string" } }
            }),
        ),
        tool(
            "list_requests",
            "List captured requests, newest first.",
            json!({ "type": "object", "properties": { "limit": { "type": "number" } } }),
        ),
    ]
}

fn captured() -> Vec<CapturedCall> {
    TRAFFIC
        .iter()
        .map(|(id, method, url, kind)| CapturedCall {
            request_id: id.to_string(),
            method: method.to_string(),
            url: url.to_string(),
            resource_type: Some(kind.to_string()),
            status: Some(200),
            mime_type: Some("application/json".to_owned()),
            request_post_data: (*method == "POST")
                .then(|| r#"{"sku":"KB-87-BLK","qty":1}"#.to_owned()),
            ..Default::default()
        })
        .collect()
}

fn compiled(request_ids: &[String]) -> Result<CliData, String> {
    let calls: Vec<CapturedCall> = captured()
        .into_iter()
        .filter(|call| request_ids.contains(&call.request_id))
        .collect();
    compile_cli(&calls, &CompileOptions::default())
}

/// Answers every request at once from fixtures.
struct FixtureHost;

impl WorkflowHost for FixtureHost {
    fn list_tools(&self, _: &mut App) -> Task<Vec<McpToolInfo>> {
        Task::ready(tools())
    }

    fn run(
        &self,
        steps: Vec<WorkflowRunStep>,
        on_progress: OnProgress<WorkflowStepProgress>,
        cx: &mut App,
    ) -> Task<Result<Vec<WorkflowStepProgress>, String>> {
        // The first step succeeds, the second is left waiting, the third fails.
        let results: Vec<WorkflowStepProgress> = steps
            .iter()
            .enumerate()
            .map(|(index, step)| {
                let (status, output, error, waiting_for) = match index {
                    0 => (
                        StepStatus::Done,
                        Some("Navigated to https://shop.example.com/login (200)".to_owned()),
                        None,
                        None,
                    ),
                    1 => (
                        StepStatus::Waiting,
                        None,
                        None,
                        step.wait_for.as_ref().map(|s| format!("waiting for {s}")),
                    ),
                    _ => (
                        StepStatus::Error,
                        None,
                        Some("No element matches ref \"r12\"".to_owned()),
                        None,
                    ),
                };
                WorkflowStepProgress {
                    index,
                    tool: step.tool.clone(),
                    status,
                    output,
                    error,
                    waiting_for,
                }
            })
            .collect();
        for progress in &results {
            on_progress(progress.clone(), cx);
        }
        Task::ready(Ok(results))
    }

    fn run_pipeline(
        &self,
        nodes: Vec<ResolvedPipeNode>,
        on_progress: OnProgress<PipeProgress>,
        cx: &mut App,
    ) -> Task<Result<bool, String>> {
        for node in &nodes {
            let progress = match node {
                ResolvedPipeNode::Tool { id, tool, .. } => PipeProgress {
                    node_id: id.clone(),
                    tool: Some(tool.clone()),
                    status: PipeStatus::Done,
                    output: Some("{ \"status\": 200, \"title\": \"Sign in\" }".to_owned()),
                    error: None,
                    taken: None,
                },
                ResolvedPipeNode::If { id, .. } => PipeProgress {
                    node_id: id.clone(),
                    tool: None,
                    status: PipeStatus::Branch,
                    output: None,
                    error: None,
                    taken: Some(BranchTaken::Then),
                },
            };
            on_progress(progress, cx);
        }
        Task::ready(Ok(true))
    }

    fn cancel(&self, _: &mut App) {}

    fn compile_cli(&self, request_ids: Vec<String>, _: &mut App) -> Task<Result<CliData, String>> {
        Task::ready(compiled(&request_ids))
    }

    fn run_cli(&self, _: CliRunPayload, _: &mut App) -> Task<Result<CliRunResult, String>> {
        Task::ready(Ok(CliRunResult {
            ok: true,
            status: Some(200),
            body: Some(
                r#"{"items":[{"sku":"KB-87-BLK","name":"기계식 키보드 87키","price":98000},{"sku":"CAP-PBT-01","name":"PBT 키캡 세트","price":15000}],"page":2,"total":41}"#
                    .to_owned(),
            ),
            ..Default::default()
        }))
    }

    fn traffic(&self, _: &App) -> Vec<TrafficCall> {
        TRAFFIC
            .iter()
            .map(|(id, method, url, kind)| TrafficCall {
                request_id: id.to_string(),
                method: method.to_string(),
                url: url.to_string(),
                resource_type: kind.to_string(),
            })
            .collect()
    }
}

/// One workflow of each kind, in the JSON the Electron app writes, so the preview also
/// shows that hydration reads it.
fn fixture_storage(empty: bool) -> Rc<dyn Storage> {
    let storage = MemoryStorage::new();
    if empty {
        return Rc::new(storage);
    }
    let cli = compiled(&["1.3".to_owned(), "1.6".to_owned(), "1.5".to_owned()])
        .ok()
        .and_then(|data| serde_json::to_value(data).ok())
        .unwrap_or_default();
    let now = 1_763_000_000_000u64;
    let workflows = json!([
        {
            "id": "wmacro1",
            "kind": "macro",
            "name": "Log in to the shop",
            "data": {
                "steps": [
                    {
                        "id": "s1",
                        "tool": "browser_navigate",
                        "input": "{ \"url\": \"https://{{host}}/login\" }"
                    },
                    {
                        "id": "s2",
                        "tool": "browser_type",
                        "input": "{\n  \"ref\": \"r4\",\n  \"text\": \"{{user}}\"\n}",
                        "waitFor": "#login-form",
                        "delay": 300
                    },
                    {
                        "id": "s3",
                        "tool": "__ai_prompt",
                        "input": "{ \"prompt\": \"Summarise the page title\", \"json\": true }",
                        "saveAs": "page"
                    }
                ],
                "vars": "{ \"host\": \"shop.example.com\", \"user\": \"admin\" }"
            },
            "createdAt": now, "updatedAt": now
        },
        {
            "id": "wpipe1",
            "kind": "pipeline",
            "name": "Open the cart, sign in first when asked",
            "data": {
                "nodes": [
                    {
                        "id": "n1", "type": "tool", "tool": "browser_navigate",
                        "input": "{ \"url\": \"https://{{host}}/cart\" }"
                    },
                    {
                        "id": "n2", "type": "if",
                        "cond": { "on": "output", "op": "contains", "value": "Sign in" },
                        "then": [
                            { "id": "n3", "type": "tool", "tool": "browser_click", "input": "{ \"ref\": \"r2\" }" }
                        ],
                        "else": []
                    }
                ],
                "vars": "{ \"host\": \"shop.example.com\" }"
            },
            "createdAt": now, "updatedAt": now
        },
        {
            "id": "wcli1",
            "kind": "cli",
            "name": "shop.example.com",
            "data": cli,
            "createdAt": now, "updatedAt": now
        },
        {
            "id": "wtpl1",
            "kind": "template",
            "name": "Explain the auth flow — a name long enough to be cut off in a narrow panel",
            "data": {
                "body": "Look at the captured traffic for this site.\n\n1. List the endpoints involved in signing in.\n2. Say which request sets the session cookie.\n3. Point out any token that has to be computed in JavaScript."
            },
            "createdAt": now, "updatedAt": now
        }
    ]);
    let _ = storage.set(
        STORAGE_KEY,
        &json!({ "state": { "workflows": workflows }, "version": 0 }).to_string(),
    );
    Rc::new(storage)
}

struct Preview {
    panel: Entity<WorkflowPanel>,
    theme_mode: ThemeMode,
    last_prompt: Option<SharedString>,
}

impl Preview {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        rever_ui_workflows::bind_keys(cx);
        let env = |name: &str| std::env::var(name).unwrap_or_default();
        let (lang, theme_mode) = (Lang::detect(), ThemeMode::from_env());
        let store =
            cx.new(|_| WorkflowsStore::new(fixture_storage(env("REVER_PREVIEW_EMPTY") == "1")));
        let panel = cx.new(|cx| {
            WorkflowPanel::new(
                store.clone(),
                Rc::new(default_registry()),
                Rc::new(FixtureHost),
                lang,
                theme_mode,
                cx,
            )
        });
        cx.subscribe(&panel, |preview, _, event: &UseInChat, cx| {
            preview.last_prompt = Some(event.0.clone().into());
            cx.notify();
        })
        .detach();

        let editor = env("REVER_PREVIEW_EDITOR");
        let run = env("REVER_PREVIEW_RUN") == "1";
        panel.update(cx, |panel, cx| match editor.as_str() {
            "list" => {
                if env("REVER_PREVIEW_DELETE") == "1" {
                    panel.ask_delete("wtpl1", window, cx);
                }
            }
            "missing" => {
                // A workflow whose kind no module registers any more.
                let _ = store.update(cx, |store, _| {
                    store.upsert(rever_ui_workflows::Workflow {
                        id: "wold1".into(),
                        kind: "recorder".into(),
                        name: "Recorded session".into(),
                        ..Default::default()
                    })
                });
                panel.edit("wold1", window, cx);
            }
            "pipeline" => panel.edit("wpipe1", window, cx),
            "cli" => panel.edit("wcli1", window, cx),
            "template" => panel.edit("wtpl1", window, cx),
            _ => panel.edit("wmacro1", window, cx),
        });
        if run && let Some(view) = panel.read(cx).editor_view() {
            if let Ok(editor) = view.clone().downcast::<MacroEditor>() {
                // The tools arrive on the next turn of the loop; Run needs none of them.
                editor.update(cx, |editor, cx| editor.run(cx));
            } else if let Ok(editor) = view.clone().downcast::<PipelineEditor>() {
                editor.update(cx, |editor, cx| editor.run(cx));
            } else if let Ok(editor) = view.clone().downcast::<CliEditor>() {
                editor.update(cx, |editor, cx| {
                    editor.export(cx);
                    editor.run(cx);
                });
            }
        }
        Self {
            panel,
            theme_mode,
            last_prompt: None,
        }
    }
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::resolve(self.theme_mode, window.appearance());
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .text_size(px(13.0))
            .font_family(".SystemUIFont")
            .child(div().flex_1().min_h_0().child(self.panel.clone()))
            .child(
                div()
                    .flex_none()
                    .px(px(10.0))
                    .pb(px(6.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_dim)
                    .child(match &self.last_prompt {
                        Some(prompt) => format!("UseInChat → {prompt}"),
                        None => "UseInChat → (press Use on a template)".to_string(),
                    }),
            )
    }
}

fn main() {
    rever_ui_kit::preview::run(720.0, 680.0, |window, cx| {
        cx.new(|cx| Preview::new(window, cx))
    });
}
