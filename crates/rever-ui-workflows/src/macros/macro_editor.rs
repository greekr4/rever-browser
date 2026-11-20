//! Port of `src/renderer/src/workflows/macro/MacroEditor.tsx` — the macro's data, its `{{var}}` resolution, and the editor that doubles as the runner.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::LazyLock;

use gpui::{
    AnyView, App, ClickEvent, Context, Entity, StyledText, TextRun, Window, div, font, prelude::*,
    px,
};
use regex::Regex;
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::text_input::TextInput;
use rever_ui_kit::theme::ThemeMode;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::registry::{EditorProps, OnChange};
use crate::core::types::{Workflow, new_id, now_ms};
use crate::host::{McpToolInfo, StepStatus, WorkflowHost, WorkflowRunStep, WorkflowStepProgress};
use crate::js::{js_number, js_trim, number_value, stringify_pretty, utf16_prefix};
use crate::pipeline::resolve::{LookedUp, PLACEHOLDER, lookup, parse_vars};
use crate::text_area::TextArea;
use crate::ui::{
    ButtonKind, MONO, SANS, SelectOption, Tokens, area_box, button, field_label, html_text,
    input_box, new_area, new_input, select, text, tooltip,
};

/// Pseudo-tool: runs a prompt through the agent instead of an MCP tool. Handled
/// by the workflow executor, so it never appears in `list_tools()`.
pub const AI_TOOL: &str = "__ai_prompt";

fn js_delay<S: serde::Serializer>(delay: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error> {
    delay.map(number_value).serialize(serializer)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MacroStep {
    pub id: String,
    pub tool: String,
    /// JSON object literal (as text) for the tool arguments. May contain `{{var}}`.
    pub input: String,
    /// Wait until this CSS selector matches before running the step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_for: Option<String>,
    /// Extra pause (ms) before running the step, applied after `wait_for`.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "js_delay")]
    pub delay: Option<f64>,
    /// Store this step's result under this name for later `{{name}}` references.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save_as: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MacroData {
    pub steps: Vec<MacroStep>,
    /// JSON object (as text) supplying values for `{{var}}` placeholders.
    pub vars: String,
}

impl MacroData {
    /// The `data` of a macro workflow. The TypeScript casts without looking; here a
    /// step that is not a step is skipped and anything else missing is empty.
    pub fn from_value(data: &Value) -> Self {
        let steps = data
            .get("steps")
            .and_then(Value::as_array)
            .map(|steps| {
                steps
                    .iter()
                    .filter_map(|step| MacroStep::deserialize(step).ok())
                    .collect()
            })
            .unwrap_or_default();
        let vars = data.get("vars").and_then(Value::as_str).unwrap_or_default();
        Self {
            steps,
            vars: vars.to_owned(),
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

pub(crate) fn step_id() -> String {
    new_id('s', now_ms(), 4)
}

/// Substitute `{{var}}` then parse each step's input into a runnable form.
/// Public so the list's Run button (macros/mod.rs) replays a macro the same
/// way the editor does.
pub fn resolve_steps(steps: &[MacroStep], vars_text: &str) -> Result<Vec<WorkflowRunStep>, String> {
    let vars = parse_vars(vars_text)?;
    let mut out = Vec::with_capacity(steps.len());
    for step in steps {
        if step.tool.is_empty() {
            return Err("A step has no tool selected".to_owned());
        }
        let mut failure = None;
        // Placeholders with no matching variable are left in place: they may refer
        // to a value an earlier step captures at run time via saveAs, which only
        // the executor can resolve.
        let substituted = PLACEHOLDER.replace_all(&step.input, |found: &regex::Captures| {
            let whole = found.get(0).map_or("", |whole| whole.as_str());
            let key = found.get(1).map_or("", |key| key.as_str());
            let value = match lookup(&vars, key) {
                Ok(value) => value.and_then(LookedUp::text),
                Err(error) => {
                    failure.get_or_insert(error);
                    None
                }
            };
            let Some(value) = value else {
                return whole.to_owned();
            };
            // Escape so an injected value can't break out of the JSON string.
            let json = serde_json::to_string(&value).unwrap_or_default();
            json.get(1..json.len().saturating_sub(1))
                .unwrap_or_default()
                .to_owned()
        });
        if let Some(error) = failure {
            return Err(error);
        }
        let input = if js_trim(&substituted).is_empty() {
            Value::Object(Default::default())
        } else {
            serde_json::from_str(&substituted)
                .map_err(|_| format!("Step \"{}\" input is not valid JSON", step.tool))?
        };
        let trimmed = |text: &Option<String>| {
            text.as_deref()
                .map(js_trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        out.push(WorkflowRunStep {
            tool: step.tool.clone(),
            input,
            wait_for: trimmed(&step.wait_for),
            delay: step.delay.filter(|delay| *delay > 0.0),
            save_as: trimmed(&step.save_as),
        });
    }
    Ok(out)
}

// Inline English in the component (not dictionary keys), kept as it is there.
const ADD_STEP: &str = "+ Add step";
const LOADING_TOOLS: &str = "Loading tools…";
const NO_STEPS: &str = "No steps yet.";
const SELECT_TOOL: &str = "Select a tool…";
const AI_HINT: (&str, &str, &str) = (
    "Sends a prompt to the agent and captures its answer. Set ",
    "json: true",
    " to force a parseable JSON answer, then reference fields as {{name.field}} in later steps.",
);
const AI_PLACEHOLDER: &str =
    r#"{ "prompt": "Summarise the page title", "json": true, "schema": "{ \"title\": string }" }"#;
const TOOL_PLACEHOLDER: &str = r#"{ "url": "https://..." }"#;
const INPUT_SCHEMA: &str = "input schema";
const VARS_LABEL: &str = "Variables (JSON, optional) — reference as {{name}} in step inputs";
const VARS_PLACEHOLDER: &str = r#"{ "user": "admin" }"#;
const RUN_MACRO: &str = "▶ Run macro";
const RUN_HINT: &str = "Runs on the active tab. Stops at the first failing step.";
const NO_STEPS_TO_RUN: &str = "No steps to run";

/// The line under a step while and after it runs.
pub fn progress_line(progress: &WorkflowStepProgress) -> String {
    match progress.status {
        StepStatus::Waiting => format!(
            "◐ {}",
            progress.waiting_for.as_deref().unwrap_or("waiting…")
        ),
        StepStatus::Running => "● running…".to_owned(),
        StepStatus::Done => {
            let output = utf16_prefix(progress.output.as_deref().unwrap_or_default(), 200);
            format!("✓ {}", if output.is_empty() { "done" } else { output })
        }
        StepStatus::Error => format!("✕ {}", progress.error.as_deref().unwrap_or("failed")),
    }
}

/// HTML's "valid floating-point number", the only text `<input type="number">` hands
/// the page; for anything else its value is "" (no delay).
static NUMBER_INPUT: LazyLock<Regex> = LazyLock::new(|| {
    // The pattern is a constant; it cannot fail to compile.
    #[allow(clippy::expect_used)]
    Regex::new(r"^-?(?:[0-9]+(?:\.[0-9]+)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?$")
        .expect("number input pattern")
});

/// The value of the delay field: empty is no delay, as is text a number input rejects
/// (Rust alone would also take "+5", "5." and " 5").
fn parse_delay(text: &str) -> Option<f64> {
    if !NUMBER_INPUT.is_match(text) {
        return None;
    }
    text.parse::<f64>().ok().filter(|delay| delay.is_finite())
}

fn input_placeholder(tool: &str) -> &'static str {
    if tool == AI_TOOL {
        AI_PLACEHOLDER
    } else {
        TOOL_PLACEHOLDER
    }
}

/// The text fields of one step, kept by step id so they survive reordering.
struct StepFields {
    wait_for: Entity<TextInput>,
    delay: Entity<TextInput>,
    save_as: Entity<TextInput>,
    input: Entity<TextArea>,
}

pub struct MacroEditor {
    workflow: Workflow,
    data: MacroData,
    on_change: OnChange,
    host: Rc<dyn WorkflowHost>,
    lang: Lang,
    theme_mode: ThemeMode,
    name: Entity<TextInput>,
    vars: Entity<TextArea>,
    fields: HashMap<String, StepFields>,
    tools: Vec<McpToolInfo>,
    running: bool,
    progress: HashMap<usize, WorkflowStepProgress>,
    run_error: Option<String>,
    /// The step whose tool list is open.
    open_select: Option<String>,
    /// Steps whose "input schema" is unfolded.
    open_schema: HashSet<String>,
}

impl MacroEditor {
    pub fn new(props: EditorProps, cx: &mut Context<Self>) -> Self {
        let data = MacroData::from_value(&props.workflow.data);
        let name = new_input(
            &props.workflow.name,
            props.lang.t("macro.goalPlaceholder"),
            cx,
        );
        let vars = new_area(&data.vars, VARS_PLACEHOLDER, 6.0, cx);
        cx.observe(&name, |editor, name, cx| {
            let name = name.read(cx).text().to_owned();
            if editor.workflow.name != name {
                editor.workflow.name = name;
                editor.changed(cx);
            }
        })
        .detach();
        cx.observe(&vars, |editor, vars, cx| {
            let vars = vars.read(cx).text().to_owned();
            if editor.data.vars != vars {
                editor.data.vars = vars;
                editor.commit(cx);
            }
        })
        .detach();

        let tools = props.host.list_tools(cx);
        cx.spawn(async move |editor, cx| {
            let tools = tools.await;
            let _ = editor.update(cx, |editor, cx| {
                editor.tools = tools;
                cx.notify();
            });
        })
        .detach();

        let mut editor = Self {
            workflow: props.workflow,
            data,
            on_change: props.on_change,
            host: props.host,
            lang: props.lang,
            theme_mode: props.theme_mode,
            name,
            vars,
            fields: HashMap::new(),
            tools: Vec::new(),
            running: false,
            progress: HashMap::new(),
            run_error: None,
            open_select: None,
            open_schema: HashSet::new(),
        };
        editor.sync_fields(cx);
        editor
    }

    pub fn data(&self) -> &MacroData {
        &self.data
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        (self.on_change)(self.workflow.clone(), cx);
        cx.notify();
    }

    /// The steps or the variables changed: write them into the working copy.
    fn commit(&mut self, cx: &mut Context<Self>) {
        self.workflow.data = self.data.to_value();
        self.changed(cx);
    }

    fn update_step(
        &mut self,
        id: &str,
        patch: impl FnOnce(&mut MacroStep) -> bool,
        cx: &mut Context<Self>,
    ) {
        let changed = self
            .data
            .steps
            .iter_mut()
            .find(|step| step.id == id)
            .is_some_and(patch);
        if changed {
            self.commit(cx);
        }
    }

    /// One set of fields per step: made for a new step, dropped with a removed one.
    fn sync_fields(&mut self, cx: &mut Context<Self>) {
        let lang = self.lang;
        self.fields
            .retain(|id, _| self.data.steps.iter().any(|step| &step.id == id));
        for step in &self.data.steps {
            if self.fields.contains_key(&step.id) {
                continue;
            }
            let wait_for = new_input(
                step.wait_for.as_deref().unwrap_or_default(),
                lang.t("macro.waitForPlaceholder"),
                cx,
            );
            let delay = new_input(
                &step.delay.map(js_number).unwrap_or_default(),
                lang.t("macro.delayPlaceholder"),
                cx,
            );
            let save_as = new_input(
                step.save_as.as_deref().unwrap_or_default(),
                lang.t("macro.saveAsPlaceholder"),
                cx,
            );
            let input = new_area(&step.input, input_placeholder(&step.tool), 6.0, cx);

            let id = step.id.clone();
            cx.observe(&wait_for, move |editor, field, cx| {
                let text = field.read(cx).text().to_owned();
                editor.update_step(
                    &id,
                    |step| {
                        let changed = step.wait_for.as_deref().unwrap_or_default() != text;
                        if changed {
                            step.wait_for = Some(text);
                        }
                        changed
                    },
                    cx,
                );
            })
            .detach();
            let id = step.id.clone();
            cx.observe(&delay, move |editor, field, cx| {
                let delay = parse_delay(field.read(cx).text());
                editor.update_step(
                    &id,
                    |step| {
                        let changed = step.delay != delay;
                        step.delay = delay;
                        changed
                    },
                    cx,
                );
            })
            .detach();
            let id = step.id.clone();
            cx.observe(&save_as, move |editor, field, cx| {
                let text = field.read(cx).text().to_owned();
                editor.update_step(
                    &id,
                    |step| {
                        let changed = step.save_as.as_deref().unwrap_or_default() != text;
                        if changed {
                            step.save_as = Some(text);
                        }
                        changed
                    },
                    cx,
                );
            })
            .detach();
            let id = step.id.clone();
            cx.observe(&input, move |editor, field, cx| {
                let text = field.read(cx).text().to_owned();
                editor.update_step(
                    &id,
                    |step| {
                        let changed = step.input != text;
                        step.input = text;
                        changed
                    },
                    cx,
                );
            })
            .detach();

            self.fields.insert(
                step.id.clone(),
                StepFields {
                    wait_for,
                    delay,
                    save_as,
                    input,
                },
            );
        }
    }

    pub fn add_step(&mut self, cx: &mut Context<Self>) {
        self.data.steps.push(MacroStep {
            id: step_id(),
            tool: self
                .tools
                .first()
                .map(|tool| tool.name.clone())
                .unwrap_or_default(),
            input: "{}".to_owned(),
            ..Default::default()
        });
        self.sync_fields(cx);
        self.commit(cx);
    }

    pub fn remove_step(&mut self, id: &str, cx: &mut Context<Self>) {
        self.data.steps.retain(|step| step.id != id);
        self.sync_fields(cx);
        self.commit(cx);
    }

    /// Swap the step at `index` with its neighbour above (-1) or below (+1).
    pub fn move_step(&mut self, index: usize, dir: isize, cx: &mut Context<Self>) {
        let Some(other) = index.checked_add_signed(dir) else {
            return;
        };
        if index >= self.data.steps.len() || other >= self.data.steps.len() {
            return;
        }
        self.data.steps.swap(index, other);
        self.commit(cx);
    }

    fn set_tool(&mut self, id: &str, tool: &str, cx: &mut Context<Self>) {
        if let Some(fields) = self.fields.get(id) {
            fields.input.update(cx, |input, cx| {
                input.set_placeholder(input_placeholder(tool), cx)
            });
        }
        self.update_step(
            id,
            |step| {
                let changed = step.tool != tool;
                step.tool = tool.to_owned();
                changed
            },
            cx,
        );
    }

    pub fn run(&mut self, cx: &mut Context<Self>) {
        let steps = match resolve_steps(&self.data.steps, &self.data.vars) {
            Ok(steps) if steps.is_empty() => Err(NO_STEPS_TO_RUN.to_owned()),
            other => other,
        };
        let steps = match steps {
            Ok(steps) => steps,
            Err(error) => {
                self.run_error = Some(error);
                cx.notify();
                return;
            }
        };
        self.run_error = None;
        self.progress.clear();
        self.running = true;
        cx.notify();
        let editor = cx.weak_entity();
        let run = self.host.run(
            steps,
            Box::new(move |progress, cx| {
                // Deferred: a host may report the first steps before `run` returns,
                // while this editor is still being updated.
                let editor = editor.clone();
                cx.defer(move |cx| {
                    let _ = editor.update(cx, |editor, cx| {
                        editor.progress.insert(progress.index, progress);
                        cx.notify();
                    });
                });
            }),
            cx,
        );
        cx.spawn(async move |editor, cx| {
            let outcome = run.await;
            let _ = editor.update(cx, |editor, cx| {
                if let Err(error) = outcome {
                    editor.run_error = Some(error);
                }
                editor.running = false;
                cx.notify();
            });
        })
        .detach();
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.host.cancel(cx);
    }

    fn render_step(
        &self,
        index: usize,
        step: &MacroStep,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let lang = self.lang;
        let dim = t.theme.text_dim;
        let info = self.tools.iter().find(|tool| tool.name == step.tool);
        let last = index + 1 == self.data.steps.len();

        let mut options = Vec::with_capacity(self.tools.len() + 2);
        if step.tool.is_empty() {
            options.push(SelectOption::new("", SELECT_TOOL));
        }
        options.push(SelectOption::new(
            AI_TOOL,
            format!("{AI_TOOL} — ask the AI"),
        ));
        options.extend(
            self.tools
                .iter()
                .map(|tool| SelectOption::plain(tool.name.clone())),
        );
        let (toggle_id, pick_id) = (step.id.clone(), step.id.clone());
        let tool_select = select(
            format!("macro-tool-{index}"),
            &step.tool,
            options,
            self.open_select.as_deref() == Some(step.id.as_str()),
            26.0,
            t,
            cx,
            move |editor: &mut Self, open, cx| {
                editor.open_select = open.then(|| toggle_id.clone());
                cx.notify();
            },
            move |editor: &mut Self, tool, cx| editor.set_tool(&pick_id, tool, cx),
        );

        let remove_id = step.id.clone();
        let head = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .w(px(18.0))
                    .flex_none()
                    .text_color(dim)
                    .child((index + 1).to_string()),
            )
            .child(tool_select.flex_1())
            .child(
                button(("macro-up", index), "↑", ButtonKind::Plain, index > 0, t)
                    .tooltip(tooltip(lang.t("common.moveUp"), t))
                    .when(index > 0, |up| {
                        up.on_click(cx.listener(move |editor, _: &ClickEvent, _, cx| {
                            editor.move_step(index, -1, cx)
                        }))
                    }),
            )
            .child(
                button(("macro-down", index), "↓", ButtonKind::Plain, !last, t)
                    .tooltip(tooltip(lang.t("common.moveDown"), t))
                    .when(!last, |down| {
                        down.on_click(cx.listener(move |editor, _: &ClickEvent, _, cx| {
                            editor.move_step(index, 1, cx)
                        }))
                    }),
            )
            .child(
                button(("macro-remove", index), "✕", ButtonKind::Plain, true, t)
                    .tooltip(tooltip(lang.t("macro.removeStep"), t))
                    .on_click(cx.listener(move |editor, _: &ClickEvent, _, cx| {
                        editor.remove_step(&remove_id, cx)
                    })),
            );

        // font-size 11, line-height 1.35
        let hint = || {
            div()
                .text_size(px(11.0))
                .line_height(px(15.0))
                .text_color(dim)
        };
        let description = if step.tool == AI_TOOL {
            let (before, code, after) = AI_HINT;
            let run = |len: usize, family: &'static str| TextRun {
                len,
                font: font(family),
                color: dim.into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            Some(hint().child(
                StyledText::new(format!("{before}{code}{after}")).with_runs(vec![
                    run(before.len(), SANS),
                    // <code>
                    run(code.len(), MONO),
                    run(after.len(), SANS),
                ]),
            ))
        } else {
            info.and_then(|info| info.description.as_deref())
                .filter(|description| !description.is_empty())
                .map(|description| hint().child(html_text(description)))
        };

        let fields = self.fields.get(&step.id);
        // Preconditions + result capture. Both waits run before the step: waitFor
        // first, then delay.
        let waits = fields.map(|fields| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.0))
                .child(
                    input_box(&fields.wait_for, 24.0, 6.0, t)
                        .id(("macro-wait-for", index))
                        .debug_selector(move || format!("macro-wait-for-{index}"))
                        .flex_grow()
                        .flex_shrink()
                        .flex_basis(px(150.0))
                        .min_w(px(110.0))
                        .font_family(MONO)
                        .text_size(px(11.0))
                        .tooltip(tooltip(lang.t("macro.waitForTitle"), t)),
                )
                .child(
                    input_box(&fields.delay, 24.0, 6.0, t)
                        .id(("macro-delay", index))
                        .debug_selector(move || format!("macro-delay-{index}"))
                        .w(px(84.0))
                        .flex_none()
                        .text_size(px(11.0))
                        .tooltip(tooltip(lang.t("macro.delayTitle"), t)),
                )
                .child(
                    input_box(&fields.save_as, 24.0, 6.0, t)
                        .id(("macro-save-as", index))
                        .debug_selector(move || format!("macro-save-as-{index}"))
                        .w(px(100.0))
                        .flex_none()
                        .font_family(MONO)
                        .text_size(px(11.0))
                        .tooltip(tooltip(lang.t("macro.saveAsTitle"), t)),
                )
        });
        let input = fields.map(|fields| {
            area_box(&fields.input, 3, 11.0, 6.0, t.theme.surface_2, t)
                .debug_selector(move || format!("macro-input-{index}"))
        });

        let schema = info.map(|info| {
            let open = self.open_schema.contains(&step.id);
            let id = step.id.clone();
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .id(("macro-schema", index))
                        .debug_selector(move || format!("macro-schema-{index}"))
                        .flex()
                        .gap(px(4.0))
                        .text_size(px(11.0))
                        .line_height(px(13.0))
                        .text_color(dim)
                        .cursor_pointer()
                        .on_click(cx.listener(move |editor, _: &ClickEvent, _, cx| {
                            if !editor.open_schema.remove(&id) {
                                editor.open_schema.insert(id.clone());
                            }
                            cx.notify();
                        }))
                        // The disclosure triangle of <summary>.
                        .child(div().font_family(MONO).text_size(px(8.0)).child(if open {
                            "▼"
                        } else {
                            "▶"
                        }))
                        .child(INPUT_SCHEMA),
                )
                .when(open, |details| {
                    details.child(
                        div()
                            .id(("macro-schema-body", index))
                            .mt(px(4.0))
                            .p(px(6.0))
                            .max_h(px(140.0))
                            .overflow_scroll()
                            .bg(t.code_bg)
                            .rounded(px(4.0))
                            .font_family(MONO)
                            .text_size(px(10.5))
                            .line_height(px(13.0))
                            .text_color(t.theme.text)
                            .whitespace_nowrap()
                            .child(stringify_pretty(&info.input_schema)),
                    )
                })
        });

        let progress = self.progress.get(&index).map(|progress| {
            let color = match progress.status {
                StepStatus::Waiting => dim,
                StepStatus::Running => t.theme.accent,
                StepStatus::Error => t.error,
                StepStatus::Done => t.chip_ok_text,
            };
            text(11.0)
                .debug_selector(move || format!("macro-progress-{index}"))
                .text_color(color)
                .child(html_text(&progress_line(progress)))
        });

        div()
            .id(("macro-step", index))
            .debug_selector(move || format!("macro-step-{index}"))
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.0))
            .p(px(8.0))
            .border_1()
            .border_color(t.theme.border)
            .rounded(px(6.0))
            .bg(t.surface)
            .child(head)
            .children(description)
            .children(waits)
            .children(input)
            .children(schema)
            .children(progress)
    }
}

/// The registry's editor factory.
pub fn open(props: EditorProps, _: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| MacroEditor::new(props, cx)).into()
}

impl Render for MacroEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::resolve(self.theme_mode, window);
        let dim = t.theme.text_dim;
        let has_tools = !self.tools.is_empty();
        let has_steps = !self.data.steps.is_empty();

        let steps_head = div()
            .flex()
            .items_center()
            .child(div().text_color(dim).child(self.lang.t("macro.steps")))
            .child(
                button("macro-add-step", ADD_STEP, ButtonKind::Plain, has_tools, &t)
                    .ml_auto()
                    .when(has_tools, |add| {
                        add.on_click(
                            cx.listener(|editor, _: &ClickEvent, _, cx| editor.add_step(cx)),
                        )
                    }),
            );
        let steps: Vec<_> = self
            .data
            .steps
            .iter()
            .enumerate()
            .map(|(index, step)| self.render_step(index, step, &t, cx).into_any_element())
            .collect();
        let steps_section = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.0))
            .child(steps_head)
            .when(!has_steps, |section| {
                section.child(div().text_color(dim).italic().child(if has_tools {
                    NO_STEPS
                } else {
                    LOADING_TOOLS
                }))
            })
            .children(steps);

        let run_button = if self.running {
            button(
                "macro-stop",
                self.lang.t("common.stop"),
                ButtonKind::Plain,
                true,
                &t,
            )
            .on_click(cx.listener(|editor, _: &ClickEvent, _, cx| editor.cancel(cx)))
        } else {
            button("macro-run", RUN_MACRO, ButtonKind::Primary, has_steps, &t)
                .when(has_steps, |run| {
                    run.on_click(cx.listener(|editor, _: &ClickEvent, _, cx| editor.run(cx)))
                })
        };

        text(12.0)
            .size_full()
            .flex()
            .flex_col()
            .font_family(SANS)
            .text_color(t.theme.text)
            .child(
                div()
                    .id("macro-editor")
                    .flex_shrink()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(field_label(&t).child("Name").child(
                        input_box(&self.name, 28.0, 8.0, &t).debug_selector(|| "macro-name".into()),
                    ))
                    .child(steps_section)
                    .child(
                        field_label(&t).child(VARS_LABEL).child(
                            area_box(&self.vars, 2, 11.0, 6.0, t.theme.surface_2, &t)
                                .debug_selector(|| "macro-vars".into()),
                        ),
                    )
                    .when_some(self.run_error.clone(), |column, error| {
                        column.child(
                            text(12.0)
                                .flex_none()
                                .debug_selector(|| "macro-run-error".into())
                                .text_color(t.error)
                                .child(html_text(&error)),
                        )
                    }),
            )
            // Pinned so Run stays reachable no matter how long the step list gets:
            // `position: sticky; bottom: 0` in the component, a sibling of the scrolling
            // part here. The 12px above it is the column's gap plus its own margin.
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.0))
                    .mt(px(12.0))
                    .py(px(8.0))
                    .bg(t.theme.bg_bar)
                    .border_t_1()
                    .border_color(t.theme.border)
                    .child(run_button)
                    .child(
                        text(11.0)
                            .flex_1()
                            .min_w_0()
                            .text_color(dim)
                            .child(RUN_HINT),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn step(tool: &str, input: &str) -> MacroStep {
        MacroStep {
            id: "s1".into(),
            tool: tool.into(),
            input: input.into(),
            ..Default::default()
        }
    }

    fn run_step(tool: &str, input: Value) -> WorkflowRunStep {
        WorkflowRunStep {
            tool: tool.into(),
            input,
            ..Default::default()
        }
    }

    #[test]
    fn 단계_입력을_json으로_읽는다() {
        assert_eq!(
            resolve_steps(
                &[step("browser_navigate", r#"{ "url": "https://a.test" }"#)],
                ""
            ),
            Ok(vec![run_step(
                "browser_navigate",
                json!({ "url": "https://a.test" })
            )])
        );
    }

    #[test]
    fn 빈_입력은_빈_객체다() {
        for input in ["", "  \n", "\u{feff}"] {
            assert_eq!(
                resolve_steps(&[step("t", input)], ""),
                Ok(vec![run_step("t", json!({}))]),
                "{input:?}"
            );
        }
    }

    #[test]
    fn 변수를_입력에_끼워_넣는다() {
        let steps = [step(
            "t",
            r#"{ "user": "{{user}}", "n": "{{ n }}", "ok": "{{ok}}" }"#,
        )];
        assert_eq!(
            resolve_steps(&steps, r#"{ "user": "admin", "n": 3, "ok": false }"#),
            Ok(vec![run_step(
                "t",
                json!({ "user": "admin", "n": "3", "ok": "false" })
            )])
        );
    }

    #[test]
    fn 값이_json_문자열을_깨지_못하게_이스케이프한다() {
        let steps = [step("t", r#"{ "q": "{{q}}" }"#)];
        assert_eq!(
            resolve_steps(&steps, r#"{ "q": "a\"b\\c\nd" }"#),
            Ok(vec![run_step("t", json!({ "q": "a\"b\\c\nd" }))])
        );
    }

    #[test]
    fn 없는_변수와_null_변수의_자리표시자는_그대로_둔다() {
        // 앞 단계가 saveAs로 남기는 값일 수 있어, 실행기가 풀도록 남긴다.
        let steps = [step(
            "t",
            r#"{ "a": "{{saved.title}}", "b": "{{ nothing }}" }"#,
        )];
        assert_eq!(
            resolve_steps(&steps, r#"{ "nothing": null }"#),
            Ok(vec![run_step(
                "t",
                json!({ "a": "{{saved.title}}", "b": "{{ nothing }}" })
            )])
        );
    }

    #[test]
    fn 변수가_json이_아니면_오류다() {
        assert_eq!(
            resolve_steps(&[step("t", "{}")], "{ user: 1 }"),
            Err("Variables are not valid JSON".to_owned())
        );
    }

    #[test]
    fn 도구를_고르지_않은_단계는_오류다() {
        assert_eq!(
            resolve_steps(&[step("a", "{}"), step("", "{}")], ""),
            Err("A step has no tool selected".to_owned())
        );
    }

    #[test]
    fn 입력이_json이_아니면_도구_이름과_함께_오류다() {
        assert_eq!(
            resolve_steps(&[step("browser_click", "{ ref: 1 }")], ""),
            Err("Step \"browser_click\" input is not valid JSON".to_owned())
        );
    }

    #[test]
    fn 기다림과_지연과_저장_이름은_값이_있을_때만_넣는다() {
        let full = MacroStep {
            wait_for: Some("  #go  ".into()),
            delay: Some(250.0),
            save_as: Some(" title ".into()),
            ..step("t", "{}")
        };
        assert_eq!(
            resolve_steps(&[full], ""),
            Ok(vec![WorkflowRunStep {
                tool: "t".into(),
                input: json!({}),
                wait_for: Some("#go".into()),
                delay: Some(250.0),
                save_as: Some("title".into()),
            }])
        );
        let blank = MacroStep {
            wait_for: Some("   ".into()),
            delay: Some(0.0),
            save_as: Some("".into()),
            ..step("t", "{}")
        };
        assert_eq!(
            resolve_steps(&[blank], ""),
            Ok(vec![run_step("t", json!({}))])
        );
        let negative = MacroStep {
            delay: Some(-5.0),
            ..step("t", "{}")
        };
        assert_eq!(
            resolve_steps(&[negative], ""),
            Ok(vec![run_step("t", json!({}))])
        );
    }

    #[test]
    fn 지연_칸은_숫자_입력이_받는_글자만_숫자로_읽는다() {
        assert_eq!(parse_delay(""), None);
        assert_eq!(parse_delay("300"), Some(300.0));
        assert_eq!(parse_delay("0"), Some(0.0));
        assert_eq!(parse_delay("1.5"), Some(1.5));
        assert_eq!(parse_delay(".5"), Some(0.5));
        assert_eq!(parse_delay("-5"), Some(-5.0));
        assert_eq!(parse_delay("1e3"), Some(1000.0));
        assert_eq!(parse_delay("2E-1"), Some(0.2));
        // 숫자 입력은 이런 글자를 빈 값으로 넘긴다.
        for rejected in [
            "abc", "+5", "5.", " 5", "5 ", "1e", "0x10", "inf", "NaN", "1,000", "1e999", "--1",
        ] {
            assert_eq!(parse_delay(rejected), None, "{rejected:?}");
        }
    }

    #[test]
    fn 진행_줄은_상태마다_다르고_출력은_200자까지다() {
        let progress = |status| WorkflowStepProgress {
            index: 0,
            tool: "t".into(),
            status,
            output: None,
            error: None,
            waiting_for: None,
        };
        assert_eq!(progress_line(&progress(StepStatus::Waiting)), "◐ waiting…");
        let waiting = WorkflowStepProgress {
            waiting_for: Some("#go".into()),
            ..progress(StepStatus::Waiting)
        };
        assert_eq!(progress_line(&waiting), "◐ #go");
        assert_eq!(progress_line(&progress(StepStatus::Running)), "● running…");
        assert_eq!(progress_line(&progress(StepStatus::Done)), "✓ done");
        let empty = WorkflowStepProgress {
            output: Some(String::new()),
            ..progress(StepStatus::Done)
        };
        assert_eq!(progress_line(&empty), "✓ done");
        // slice(0, 200)은 UTF-16 단위다: 한글 300자는 200자가 남는다.
        let long = WorkflowStepProgress {
            output: Some("가".repeat(300)),
            ..progress(StepStatus::Done)
        };
        assert_eq!(progress_line(&long), format!("✓ {}", "가".repeat(200)));
        assert_eq!(progress_line(&progress(StepStatus::Error)), "✕ failed");
        let failed = WorkflowStepProgress {
            error: Some("no such element".into()),
            ..progress(StepStatus::Error)
        };
        assert_eq!(progress_line(&failed), "✕ no such element");
    }

    #[test]
    fn 단계가_없으면_빈_목록이다() {
        assert_eq!(resolve_steps(&[], ""), Ok(vec![]));
    }

    #[test]
    fn 변수가_null이면_자리표시자를_읽을_때_오류다() {
        // 타입스크립트는 여기서 TypeError를 던진다; 같은 문구의 오류로 돌려준다.
        assert_eq!(
            resolve_steps(&[step("t", r#"{ "a": "{{x}}" }"#)], "null"),
            Err("Cannot read properties of null (reading 'x')".to_owned())
        );
    }

    #[test]
    fn 매크로_데이터는_저장_형식_그대로_읽고_쓴다() {
        let data = MacroData {
            steps: vec![
                MacroStep {
                    id: "s1".into(),
                    tool: "t".into(),
                    input: "{}".into(),
                    wait_for: Some("".into()),
                    delay: Some(100.0),
                    save_as: None,
                },
                step("u", ""),
            ],
            vars: "{}".into(),
        };
        let value = data.to_value();
        assert_eq!(
            value,
            json!({
                "steps": [
                    { "id": "s1", "tool": "t", "input": "{}", "waitFor": "", "delay": 100 },
                    { "id": "s1", "tool": "u", "input": "" }
                ],
                "vars": "{}"
            })
        );
        // 지연은 100.0이 아니라 100으로 쓴다.
        assert!(value.to_string().contains(r#""delay":100}"#));
        assert_eq!(MacroData::from_value(&value), data);
    }

    #[test]
    fn 모양이_맞지_않는_데이터는_빈_매크로로_읽는다() {
        assert_eq!(MacroData::from_value(&json!(null)), MacroData::default());
        assert_eq!(
            MacroData::from_value(&json!({ "steps": {}, "vars": [] })),
            MacroData::default()
        );
        let data = MacroData::from_value(&json!({
            "steps": [7, { "id": "s1", "tool": "t", "delay": "soon" }, { "id": "s2" }]
        }));
        assert_eq!(data.steps.len(), 1);
        assert_eq!(data.steps[0].id, "s2");
    }

    #[test]
    fn 단계_id는_s로_시작한다() {
        let id = step_id();
        assert!(id.starts_with('s'));
        assert!(id.len() > 5);
    }
}

#[cfg(test)]
mod view_tests {
    use gpui::{TestAppContext, VisualTestContext};
    use serde_json::json;

    use super::*;
    use crate::testing::{Changes, FakeHost, bind_keys, click, props, retype, shown, tool};

    fn host() -> FakeHost {
        FakeHost {
            tools: vec![
                tool("browser_navigate", "Go to a URL."),
                tool("browser_click", ""),
            ],
            ..Default::default()
        }
    }

    fn open<'a>(
        host: &Rc<FakeHost>,
        data: Value,
        cx: &'a mut TestAppContext,
    ) -> (Entity<MacroEditor>, Changes, &'a mut VisualTestContext) {
        cx.update(bind_keys);
        let workflow = Workflow {
            id: "w1".into(),
            kind: "macro".into(),
            name: "Login".into(),
            data,
            ..Default::default()
        };
        let (props, changes) = props(workflow, host.clone());
        let (editor, cx) = cx.add_window_view(move |_, cx| MacroEditor::new(props, cx));
        (editor, changes, cx)
    }

    fn step(id: &str, tool: &str) -> Value {
        json!({ "id": id, "tool": tool, "input": "{}" })
    }

    fn ids(editor: &Entity<MacroEditor>, cx: &mut VisualTestContext) -> Vec<String> {
        editor.read_with(cx, |editor, _| {
            editor
                .data
                .steps
                .iter()
                .map(|step| step.id.clone())
                .collect()
        })
    }

    #[gpui::test]
    fn 도구_목록이_오기_전에는_단계를_더할_수_없다(cx: &mut TestAppContext) {
        let host = Rc::new(FakeHost::default());
        let (editor, changes, cx) = open(&host, json!({ "steps": [], "vars": "" }), cx);
        click(cx, "macro-add-step");
        assert!(ids(&editor, cx).is_empty());
        // 단계가 없으면 실행 버튼도 눌리지 않는다.
        click(cx, "macro-run");
        assert!(host.runs.borrow().is_empty());
        assert!(changes.borrow().is_empty());
    }

    #[gpui::test]
    fn 단계를_더하면_첫_도구와_빈_입력으로_시작한다(cx: &mut TestAppContext) {
        let host = Rc::new(host());
        let (editor, changes, cx) = open(&host, json!({ "steps": [], "vars": "" }), cx);
        click(cx, "macro-add-step");
        let added = ids(&editor, cx);
        assert_eq!(added.len(), 1);
        assert!(added[0].starts_with('s'));
        let last = changes.borrow().last().cloned().unwrap();
        assert_eq!(
            last.data,
            json!({
                "steps": [{ "id": added[0], "tool": "browser_navigate", "input": "{}" }],
                "vars": ""
            })
        );
        assert_eq!(last.name, "Login");
        assert!(shown(cx, "macro-step-0"));
    }

    #[gpui::test]
    fn 단계를_위아래로_옮기고_지운다(cx: &mut TestAppContext) {
        let host = Rc::new(host());
        let steps = json!([
            step("a", "browser_click"),
            step("b", "browser_click"),
            step("c", "browser_click")
        ]);
        let (editor, changes, cx) = open(&host, json!({ "steps": steps, "vars": "" }), cx);
        click(cx, "macro-down-0");
        assert_eq!(ids(&editor, cx), ["b", "a", "c"]);
        click(cx, "macro-up-2");
        assert_eq!(ids(&editor, cx), ["b", "c", "a"]);
        // 첫 단계의 ↑와 마지막 단계의 ↓는 꺼져 있다.
        let before = changes.borrow().len();
        click(cx, "macro-up-0");
        click(cx, "macro-down-2");
        assert_eq!(ids(&editor, cx), ["b", "c", "a"]);
        assert_eq!(changes.borrow().len(), before);

        click(cx, "macro-remove-1");
        assert_eq!(ids(&editor, cx), ["b", "a"]);
        let saved = MacroData::from_value(&changes.borrow().last().unwrap().data);
        assert_eq!(
            saved
                .steps
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["b", "a"]
        );
    }

    #[gpui::test]
    fn 이름과_단계의_칸과_변수를_고치면_워크플로에_적힌다(
        cx: &mut TestAppContext,
    ) {
        let host = Rc::new(host());
        let (_, changes, cx) = open(
            &host,
            json!({ "steps": [step("s1", "browser_click")], "vars": "" }),
            cx,
        );
        retype(cx, "macro-name", "Sign in");
        retype(cx, "macro-wait-for-0", "#go");
        retype(cx, "macro-delay-0", "250");
        retype(cx, "macro-save-as-0", "clicked");
        retype(cx, "macro-input-0", r#"{ "ref": "r1" }"#);
        retype(cx, "macro-vars", r#"{ "user": "admin" }"#);
        let last = changes.borrow().last().cloned().unwrap();
        assert_eq!(last.name, "Sign in");
        assert_eq!(
            last.data,
            json!({
                "steps": [{
                    "id": "s1",
                    "tool": "browser_click",
                    "input": "{ \"ref\": \"r1\" }",
                    "waitFor": "#go",
                    "delay": 250,
                    "saveAs": "clicked"
                }],
                "vars": "{ \"user\": \"admin\" }"
            })
        );

        // 지연 칸을 비우거나 숫자가 아닌 것을 쓰면 지연이 없다.
        retype(cx, "macro-delay-0", "");
        assert_eq!(
            changes.borrow().last().unwrap().data["steps"][0].get("delay"),
            None
        );
        retype(cx, "macro-delay-0", "1.5");
        assert_eq!(
            changes.borrow().last().unwrap().data["steps"][0]["delay"],
            json!(1.5)
        );
        retype(cx, "macro-delay-0", "soon");
        assert_eq!(
            changes.borrow().last().unwrap().data["steps"][0].get("delay"),
            None
        );
    }

    #[gpui::test]
    fn 목록에서_도구를_고른다(cx: &mut TestAppContext) {
        let host = Rc::new(host());
        let (editor, _, cx) = open(&host, json!({ "steps": [step("s1", "")], "vars": "" }), cx);
        let tool_of = |cx: &mut VisualTestContext| {
            editor.read_with(cx, |editor, _| editor.data.steps[0].tool.clone())
        };
        let open = |cx: &mut VisualTestContext| {
            editor.read_with(cx, |editor, _| editor.open_select.clone())
        };
        assert!(!shown(cx, "macro-tool-0-option-0"));
        click(cx, "macro-tool-0");
        assert_eq!(open(cx).as_deref(), Some("s1"));
        // 도구를 고르지 않은 단계: "Select a tool…", AI 단계, 그리고 도구 둘.
        assert!(shown(cx, "macro-tool-0-option-3"));
        assert!(!shown(cx, "macro-tool-0-option-4"));
        click(cx, "macro-tool-0-option-1");
        assert_eq!(tool_of(cx), AI_TOOL);
        assert_eq!(open(cx), None);

        // 고른 뒤에는 "Select a tool…"이 없어 한 칸씩 당겨진다.
        click(cx, "macro-tool-0");
        click(cx, "macro-tool-0-option-2");
        assert_eq!(tool_of(cx), "browser_click");

        // 바깥을 누르면 고르지 않고 닫힌다.
        click(cx, "macro-tool-0");
        assert_eq!(open(cx).as_deref(), Some("s1"));
        click(cx, "macro-name");
        assert_eq!(open(cx), None);
        assert_eq!(tool_of(cx), "browser_click");
    }

    #[gpui::test]
    fn 입력_스키마는_아는_도구에만_있고_눌러서_편다(cx: &mut TestAppContext) {
        let host = Rc::new(host());
        let steps = json!([
            step("s1", "browser_navigate"),
            step("s2", AI_TOOL),
            step("s3", "gone_tool")
        ]);
        let (editor, _, cx) = open(&host, json!({ "steps": steps, "vars": "" }), cx);
        assert!(shown(cx, "macro-schema-0"));
        assert!(!shown(cx, "macro-schema-1"));
        assert!(!shown(cx, "macro-schema-2"));
        click(cx, "macro-schema-0");
        assert!(editor.read_with(cx, |editor, _| editor.open_schema.contains("s1")));
        click(cx, "macro-schema-0");
        assert!(editor.read_with(cx, |editor, _| editor.open_schema.is_empty()));
    }

    #[gpui::test]
    fn 실행하면_풀린_단계를_보내고_단계마다_진행을_보여준다(
        cx: &mut TestAppContext,
    ) {
        let host = Rc::new(host());
        let progress =
            |index, status, output: Option<&str>, error: Option<&str>| WorkflowStepProgress {
                index,
                tool: "t".into(),
                status,
                output: output.map(str::to_owned),
                error: error.map(str::to_owned),
                waiting_for: None,
            };
        *host.step_results.borrow_mut() = vec![
            progress(0, StepStatus::Running, None, None),
            progress(0, StepStatus::Done, Some("ok"), None),
            progress(1, StepStatus::Error, None, Some("no element")),
        ];
        let steps = json!([
            { "id": "s1", "tool": "browser_navigate", "input": "{ \"url\": \"https://{{host}}/\" }" },
            { "id": "s2", "tool": "browser_click", "input": "", "waitFor": " #go ", "delay": 100 }
        ]);
        let (editor, changes, cx) = open(
            &host,
            json!({ "steps": steps, "vars": "{ \"host\": \"a.test\" }" }),
            cx,
        );
        click(cx, "macro-run");
        assert_eq!(
            *host.runs.borrow(),
            [vec![
                WorkflowRunStep {
                    tool: "browser_navigate".into(),
                    input: json!({ "url": "https://a.test/" }),
                    ..Default::default()
                },
                WorkflowRunStep {
                    tool: "browser_click".into(),
                    input: json!({}),
                    wait_for: Some("#go".into()),
                    delay: Some(100.0),
                    save_as: None,
                },
            ]]
        );
        // 단계마다 마지막 진행이 남는다.
        let lines = editor.read_with(cx, |editor, _| {
            (
                editor.progress.get(&0).map(progress_line),
                editor.progress.get(&1).map(progress_line),
                editor.running,
                editor.run_error.clone(),
            )
        });
        assert_eq!(
            lines,
            (
                Some("✓ ok".to_owned()),
                Some("✕ no element".to_owned()),
                false,
                None
            )
        );
        assert!(shown(cx, "macro-progress-0") && shown(cx, "macro-progress-1"));
        // 실행은 워크플로를 바꾸지 않는다.
        assert!(changes.borrow().is_empty());
    }

    #[gpui::test]
    fn 풀_수_없는_매크로는_실행하지_않고_까닭을_보여준다(
        cx: &mut TestAppContext,
    ) {
        let host = Rc::new(host());
        let (editor, _, cx) = open(
            &host,
            json!({ "steps": [step("s1", "browser_click")], "vars": "{ nope" }),
            cx,
        );
        let error = |cx: &mut VisualTestContext| editor.read_with(cx, |e, _| e.run_error.clone());
        assert!(!shown(cx, "macro-run-error"));
        click(cx, "macro-run");
        assert_eq!(error(cx).as_deref(), Some("Variables are not valid JSON"));
        assert!(shown(cx, "macro-run-error"));
        assert!(host.runs.borrow().is_empty());

        // 고치고 다시 실행하면 오류가 사라진다.
        retype(cx, "macro-vars", "");
        click(cx, "macro-run");
        assert_eq!(error(cx), None);
        assert_eq!(host.runs.borrow().len(), 1);

        // 실행 자체가 실패하면 그 문구를 보여준다.
        *host.run_error.borrow_mut() = Some("executor offline".into());
        click(cx, "macro-run");
        assert_eq!(error(cx).as_deref(), Some("executor offline"));
        assert!(!editor.read_with(cx, |e, _| e.running));
    }

    #[gpui::test]
    fn 실행_중에는_멈춤_버튼이_실행을_취소한다(cx: &mut TestAppContext) {
        let host = Rc::new(host());
        host.hold.set(true);
        let (editor, _, cx) = open(
            &host,
            json!({ "steps": [step("s1", "browser_click")], "vars": "" }),
            cx,
        );
        click(cx, "macro-run");
        assert!(editor.read_with(cx, |e, _| e.running));
        click(cx, "macro-stop");
        assert_eq!(*host.cancels.borrow(), 1);
        assert_eq!(host.runs.borrow().len(), 1);
    }
}
