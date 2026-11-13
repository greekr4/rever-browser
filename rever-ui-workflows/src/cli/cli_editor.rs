//! Port of `src/renderer/src/workflows/cli/CliEditor.tsx` — compile captured API calls into CLI commands, try them, export them.
//!
//! The traffic store the component read and the JSON viewer it embedded belong to the
//! traffic panel; both reach this editor through [`WorkflowHost`].

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, AnyView, App, ClickEvent, ClipboardItem, Context, Entity, ScrollHandle, Window,
    div, prelude::*, px,
};
use rever_cli_spec::cli_spec::{
    CapturedFlagSource, CliCommand, CliData, EXTRACT_NOTE_CAPPED, EXTRACT_NOTE_NONE, Record,
    attach_extract, captured_flag_values, cli_run_example, command_flag_names, export_curl,
    run_json_body,
};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::text_input::TextInput;
use rever_ui_kit::theme::ThemeMode;
use rever_ui_state::format_json::try_pretty;
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use crate::core::registry::EditorProps;
use crate::core::types::Workflow;
use crate::host::{CliRunPayload, CliRunResult, TrafficCall, WorkflowHost};
use crate::js::js_trim;
use crate::ui::{
    ButtonKind, MONO, SANS, Tokens, button, checkbox, clickable, field_label, fill, html_text,
    input_box, new_input, text,
};

/// The resource types that count as API calls.
const API_TYPES: [&str; 2] = ["XHR", "Fetch"];
/// How many recent API calls the request list offers.
pub const MAX_CALLS: usize = 40;
/// How long the Copy button reads "Copied".
const COPIED_MS: u64 = 1200;

/// The `data` of a CLI workflow, whatever was saved: missing text is empty, a command
/// that does not have a command's shape is left out, and a broken extract is dropped
/// from its command (`attachExtract`).
pub fn as_cli(data: &Value) -> CliData {
    let text = |key: &str| {
        data.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    CliData {
        site: text("site"),
        title: text("title"),
        base_url: text("baseUrl"),
        commands: data
            .get("commands")
            .and_then(Value::as_array)
            .map(|commands| {
                commands
                    .iter()
                    .filter_map(|command| CliCommand::deserialize(command).ok())
                    .map(attach_extract)
                    .collect()
            })
            .unwrap_or_default(),
        writes: data.get("writes") == Some(&Value::Bool(true)),
    }
}

/// The newest API calls first, at most [`MAX_CALLS`]. `traffic` is in capture order.
pub fn recent_api_calls(traffic: &[TrafficCall]) -> Vec<&TrafficCall> {
    traffic
        .iter()
        .rev()
        .filter(|entry| API_TYPES.contains(&entry.resource_type.as_str()))
        .take(MAX_CALLS)
        .collect()
}

/// What a request is listed as: path and query, or the raw URL when it is not absolute.
pub fn request_path(url: &str) -> String {
    match Url::parse(url) {
        Ok(parsed) => match parsed.query().filter(|query| !query.is_empty()) {
            Some(query) => format!("{}?{query}", parsed.path()),
            None => parsed.path().to_owned(),
        },
        // Keep the raw url when it is not absolute.
        Err(_) => url.to_owned(),
    }
}

/// The note of an extracting run in the reader's language; an unknown note stays as it is.
pub fn note_text(note: Option<&str>, lang: Lang) -> Option<String> {
    let note = note.filter(|note| !note.is_empty())?;
    let missed = note.contains(EXTRACT_NOTE_NONE);
    let capped = note.contains(EXTRACT_NOTE_CAPPED);
    Some(match (missed, capped) {
        (true, true) => format!(
            "{} {}",
            lang.t("workflow.cli.noMatchExtract"),
            lang.t("workflow.cli.pageCapped")
        ),
        (true, false) => lang.t("workflow.cli.noMatchExtract").to_owned(),
        (false, true) => lang.t("workflow.cli.pageCapped").to_owned(),
        (false, false) => note.to_owned(),
    })
}

/// A finished run as text: the status, then the extracted rows or the body.
pub fn format_run(result: &CliRunResult, lang: Lang) -> String {
    let status = result.status.map(|status| status.to_string());
    let mut lines = vec![status.unwrap_or_default()];
    if let Some(items) = &result.items {
        for (index, item) in items.iter().enumerate() {
            let cells: Vec<String> = item
                .iter()
                .map(|(key, value)| format!("{key}: {value}"))
                .collect();
            lines.push(format!("{}. {}", index + 1, cells.join("  ·  ")));
        }
        let count = result.matched.unwrap_or(items.len() as u64).to_string();
        lines.push(fill(
            lang.t("workflow.cli.matched"),
            &[("count", count.as_str())],
        ));
        lines.extend(note_text(result.note.as_deref(), lang));
    } else {
        lines.push(result.body.clone().unwrap_or_default());
        if result.body_cut == Some(true) {
            lines.push(lang.t("workflow.cli.bodyCut").to_owned());
        }
    }
    lines.retain(|line| !line.is_empty());
    lines.join("\n")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Busy {
    Compile,
    Run,
}

/// The section a fresh result should be scrolled to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reveal {
    Export,
    Run,
}

pub struct CliEditor {
    workflow: Workflow,
    data: CliData,
    on_change: Rc<dyn Fn(Workflow, &mut App)>,
    host: Rc<dyn WorkflowHost>,
    lang: Lang,
    theme_mode: ThemeMode,
    name: Entity<TextInput>,
    /// The ticked requests, in the order they were ticked.
    picked: Vec<String>,
    busy: Option<Busy>,
    error: Option<String>,
    exported: Option<String>,
    /// Name of the command the flags, the example and Run are about.
    active: String,
    inputs: Record<String>,
    /// One field per flag name; a flag two commands share keeps one value.
    flag_inputs: HashMap<String, Entity<TextInput>>,
    run_text: Option<String>,
    /// A JSON run body renders in the collapsible viewer instead of one raw line.
    run_json: Option<String>,
    run_json_view: Option<AnyView>,
    copied: bool,
    /// `workflow:command:flag` marks: each flag is filled from the capture only once.
    seeded: HashSet<String>,
    scroll: ScrollHandle,
    /// Bring a fresh run result or export into view: both render below the flags,
    /// often past the bottom of a short panel.
    reveal: Option<Reveal>,
}

impl CliEditor {
    pub fn new(props: EditorProps, cx: &mut Context<Self>) -> Self {
        let data = as_cli(&props.workflow.data);
        let name = new_input(&props.workflow.name, "", cx);
        cx.observe(&name, |editor, name, cx| {
            let name = name.read(cx).text().to_owned();
            if editor.workflow.name != name {
                editor.workflow.name = name;
                editor.changed(cx);
            }
        })
        .detach();
        Self {
            active: data
                .commands
                .first()
                .map(|command| command.name.clone())
                .unwrap_or_default(),
            workflow: props.workflow,
            data,
            on_change: props.on_change,
            host: props.host,
            lang: props.lang,
            theme_mode: props.theme_mode,
            name,
            picked: Vec::new(),
            busy: None,
            error: None,
            exported: None,
            inputs: Record::new(),
            flag_inputs: HashMap::new(),
            run_text: None,
            run_json: None,
            run_json_view: None,
            copied: false,
            seeded: HashSet::new(),
            scroll: ScrollHandle::new(),
            reveal: None,
        }
    }

    pub fn data(&self) -> &CliData {
        &self.data
    }

    /// The text of the run result, as shown under "Run result".
    pub fn run_text(&self) -> Option<&str> {
        self.run_text.as_deref()
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        (self.on_change)(self.workflow.clone(), cx);
        cx.notify();
    }

    /// The commands or the writes switch changed: write them into the working copy.
    fn commit(&mut self, cx: &mut Context<Self>) {
        self.workflow.data = serde_json::to_value(&self.data).unwrap_or_default();
        self.changed(cx);
    }

    /// The active command, or the first one when the active name matches none.
    fn command(&self) -> Option<&CliCommand> {
        self.data
            .commands
            .iter()
            .find(|command| command.name == self.active)
            .or_else(|| self.data.commands.first())
    }

    pub fn toggle_request(&mut self, request_id: &str, cx: &mut Context<Self>) {
        match self.picked.iter().position(|id| id == request_id) {
            Some(at) => {
                self.picked.remove(at);
            }
            None => self.picked.push(request_id.to_owned()),
        }
        cx.notify();
    }

    pub fn set_active(&mut self, name: &str, cx: &mut Context<Self>) {
        self.active = name.to_owned();
        cx.notify();
    }

    pub fn set_writes(&mut self, writes: bool, cx: &mut Context<Self>) {
        self.data.writes = writes;
        self.commit(cx);
    }

    pub fn compile(&mut self, cx: &mut Context<Self>) {
        self.busy = Some(Busy::Compile);
        self.error = None;
        self.exported = None;
        cx.notify();
        let compile = self.host.compile_cli(self.picked.clone(), cx);
        cx.spawn(async move |editor, cx| {
            let compiled = compile.await;
            let _ = editor.update(cx, |editor, cx| {
                editor.busy = None;
                match compiled {
                    Err(error) => editor.error = Some(error),
                    Ok(compiled) if compiled.commands.is_empty() => {
                        editor.error = Some(editor.lang.t("workflow.cli.noMatch").to_owned());
                    }
                    Ok(compiled) => editor.take_compiled(compiled, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn take_compiled(&mut self, compiled: CliData, cx: &mut Context<Self>) {
        let first = compiled
            .commands
            .first()
            .map(|command| command.name.clone());
        if js_trim(&self.workflow.name).is_empty() {
            // The first command's name, or the site when that is empty.
            let name = first
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| compiled.site.clone());
            self.workflow.name = name.clone();
            self.name.update(cx, |input, cx| input.set_text(name, cx));
        }
        self.data = CliData {
            writes: false,
            ..compiled
        };
        self.active = first.unwrap_or_default();
        self.commit(cx);
    }

    pub fn export(&mut self, cx: &mut Context<Self>) {
        self.exported = Some(export_curl(&self.data));
        self.reveal = Some(Reveal::Export);
        cx.notify();
    }

    pub fn run(&mut self, cx: &mut Context<Self>) {
        let Some(command) = self.command().cloned() else {
            return;
        };
        self.busy = Some(Busy::Run);
        self.error = None;
        self.run_text = None;
        self.run_json = None;
        self.run_json_view = None;
        cx.notify();
        let run = self.host.run_cli(
            CliRunPayload {
                base_url: self.data.base_url.clone(),
                writes: self.data.writes,
                command,
                inputs: self.inputs.clone(),
            },
            cx,
        );
        cx.spawn(async move |editor, cx| {
            let result = run.await;
            let _ = editor.update(cx, |editor, cx| {
                editor.busy = None;
                editor.show_run(result, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn show_run(&mut self, result: Result<CliRunResult, String>, cx: &mut Context<Self>) {
        self.reveal = Some(Reveal::Run);
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                self.run_text = Some(error);
                return;
            }
        };
        if !result.ok {
            let failed = self.lang.t("workflow.cli.runFailed");
            self.run_text = Some(result.error.unwrap_or_else(|| failed.to_owned()));
            return;
        }
        let body = result.body_part();
        match run_json_body(&body) {
            Some(json) => {
                self.run_json_view = self.host.json_view(json, cx);
                self.run_json = Some(json.to_owned());
                let status = result.status.map(|status| status.to_string());
                self.run_text = Some(status.unwrap_or_default());
            }
            None => self.run_text = Some(format_run(&result, self.lang)),
        }
    }

    pub fn copy_example(&mut self, cx: &mut Context<Self>) {
        let Some(command) = self.command() else {
            return;
        };
        let example = cli_run_example(&self.workflow.id, command, &self.inputs);
        cx.write_to_clipboard(ClipboardItem::new_string(example));
        self.copied = true;
        cx.notify();
        cx.spawn(async move |editor, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(COPIED_MS))
                .await;
            let _ = editor.update(cx, |editor, cx| {
                editor.copied = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// Fill empty flags once from the captured URL while that request is still in
    /// the traffic list. The value is not written back onto the saved workflow.
    fn seed_flags(&mut self, traffic: &[TrafficCall], cx: &mut Context<Self>) {
        let Some(command) = self.command() else {
            return;
        };
        let sources: Vec<CapturedFlagSource> = command
            .evidence_request_ids
            .iter()
            .filter_map(|id| traffic.iter().find(|entry| &entry.request_id == id))
            .map(|entry| CapturedFlagSource {
                url: entry.url.clone(),
                ..Default::default()
            })
            .collect();
        let found = captured_flag_values(command, &sources);
        let prefix = format!("{}:{}:", self.workflow.id, command.name);
        for (name, value) in found {
            if !self.seeded.insert(format!("{prefix}{name}")) {
                continue;
            }
            if self.inputs.get(&name).is_none_or(String::is_empty) {
                if let Some(field) = self.flag_inputs.get(&name) {
                    field.update(cx, |field, cx| field.set_text(value.clone(), cx));
                }
                self.inputs.insert(name, value);
            }
        }
    }

    /// The field of each flag of the active command, made on first use.
    fn flag_fields(
        &mut self,
        flags: &[String],
        cx: &mut Context<Self>,
    ) -> Vec<(String, Entity<TextInput>)> {
        flags
            .iter()
            .map(|name| {
                let field = match self.flag_inputs.get(name) {
                    Some(field) => field.clone(),
                    None => {
                        let value = self.inputs.get(name).cloned().unwrap_or_default();
                        let field = new_input(&value, "", cx);
                        let flag = name.clone();
                        cx.observe(&field, move |editor, field, cx| {
                            let text = field.read(cx).text().to_owned();
                            let current = editor.inputs.get(&flag).map_or("", String::as_str);
                            if current != text {
                                editor.inputs.insert(flag.clone(), text);
                                cx.notify();
                            }
                        })
                        .detach();
                        self.flag_inputs.insert(name.clone(), field.clone());
                        field
                    }
                };
                (name.clone(), field)
            })
            .collect()
    }
}

/// The registry's editor factory.
pub fn open(props: EditorProps, _: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| CliEditor::new(props, cx)).into()
}

/// `<pre style="white-space: pre-wrap; margin: 0">`: Chromium's 13px monospace.
fn pre(content: impl Into<gpui::SharedString>) -> gpui::Div {
    div()
        .font_family(MONO)
        .text_size(px(13.0))
        .line_height(px(15.0))
        .child(content.into())
}

impl Render for CliEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::resolve(self.theme_mode, window);
        let lang = self.lang;
        let dim = t.theme.text_dim;

        let traffic = self.host.traffic(cx);
        self.seed_flags(&traffic, cx);
        let command = self.command().cloned();
        let flags: Vec<String> = command
            .iter()
            .flat_map(|command| command_flag_names(command))
            .map(str::to_owned)
            .collect();
        let flag_fields = self.flag_fields(&flags, cx);
        let example = command
            .as_ref()
            .map(|command| cli_run_example(&self.workflow.id, command, &self.inputs));
        let busy = self.busy;
        let has_commands = !self.data.commands.is_empty();
        let hint = |key: &'static str| div().flex_none().text_color(dim).child(lang.t(key));

        let mut sections: Vec<AnyElement> = Vec::new();
        sections.push(
            field_label(&t)
                .child(lang.t("workflow.cli.name"))
                .child(input_box(&self.name, 28.0, 8.0, &t).debug_selector(|| "cli-name".into()))
                .into_any_element(),
        );

        sections.push(hint("workflow.cli.requests").into_any_element());
        let calls = recent_api_calls(&traffic);
        if calls.is_empty() {
            sections.push(hint("workflow.cli.empty").into_any_element());
        } else {
            let rows = calls.into_iter().map(|entry| {
                let id = entry.request_id.clone();
                let checked = self.picked.contains(&id);
                clickable(gpui::ElementId::Name(
                    format!("cli-request-{}", entry.request_id).into(),
                ))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.0))
                .cursor_pointer()
                .on_click(
                    cx.listener(move |editor, _: &ClickEvent, _, cx| {
                        editor.toggle_request(&id, cx)
                    }),
                )
                .child(checkbox(
                    gpui::ElementId::Name(format!("cli-request-box-{}", entry.request_id).into()),
                    checked,
                    &t,
                ))
                .child(div().flex_1().min_w_0().child(html_text(&format!(
                    "{} {}",
                    entry.method,
                    request_path(&entry.url)
                ))))
            });
            sections.push(
                div()
                    .id("cli-requests")
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(4.0))
                    .max_h(px(180.0))
                    .overflow_y_scroll()
                    .children(rows)
                    .into_any_element(),
            );
        }

        let can_compile = busy.is_none() && !self.picked.is_empty();
        sections.push(
            div()
                .flex()
                .flex_none()
                .child(
                    button(
                        "cli-compile",
                        lang.t(if busy == Some(Busy::Compile) {
                            "workflow.cli.compiling"
                        } else {
                            "workflow.cli.compile"
                        }),
                        ButtonKind::Plain,
                        can_compile,
                        &t,
                    )
                    .when(can_compile, |compile| {
                        compile.on_click(
                            cx.listener(|editor, _: &ClickEvent, _, cx| editor.compile(cx)),
                        )
                    }),
                )
                .into_any_element(),
        );

        sections.push(hint("workflow.cli.commands").into_any_element());
        if !has_commands {
            sections.push(hint("workflow.cli.noCommands").into_any_element());
        }
        for (index, item) in self.data.commands.iter().enumerate() {
            let pressed = command
                .as_ref()
                .is_some_and(|command| command.name == item.name);
            let name = item.name.clone();
            sections.push(
                button(
                    ("cli-command", index),
                    html_text(&format!(
                        "{} · {} {} · {}",
                        item.name, item.method, item.path, item.effect
                    )),
                    ButtonKind::Toggle(pressed),
                    true,
                    &t,
                )
                .on_click(
                    cx.listener(move |editor, _: &ClickEvent, _, cx| editor.set_active(&name, cx)),
                )
                .into_any_element(),
            );
        }

        let writes = self.data.writes;
        sections.push(
            clickable("cli-writes")
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.0))
                .cursor_pointer()
                .on_click(
                    cx.listener(move |editor, _: &ClickEvent, _, cx| {
                        editor.set_writes(!writes, cx)
                    }),
                )
                .child(checkbox("cli-writes-box", writes, &t))
                .child(lang.t("workflow.cli.writes"))
                .into_any_element(),
        );
        sections.push(hint("workflow.cli.writesHint").into_any_element());
        sections.push(hint("workflow.cli.sessionHint").into_any_element());

        let can_run = command.is_some() && busy.is_none();
        sections.push(
            div()
                .flex()
                .flex_none()
                .gap(px(6.0))
                .child(
                    button(
                        "cli-export",
                        lang.t("workflow.cli.export"),
                        ButtonKind::Plain,
                        has_commands,
                        &t,
                    )
                    .when(has_commands, |export| {
                        export.on_click(
                            cx.listener(|editor, _: &ClickEvent, _, cx| editor.export(cx)),
                        )
                    }),
                )
                .child(
                    button(
                        "cli-run",
                        lang.t(if busy == Some(Busy::Run) {
                            "workflow.cli.running"
                        } else {
                            "workflow.cli.run"
                        }),
                        ButtonKind::Plain,
                        can_run,
                        &t,
                    )
                    .when(can_run, |run| {
                        run.on_click(cx.listener(|editor, _: &ClickEvent, _, cx| editor.run(cx)))
                    }),
                )
                .into_any_element(),
        );

        if !flag_fields.is_empty() {
            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(4.0))
                    .children(flag_fields.into_iter().map(|(name, field)| {
                        let selector = format!("cli-flag-{name}");
                        field_label(&t).child(name).child(
                            input_box(&field, 28.0, 8.0, &t).debug_selector(move || selector),
                        )
                    }))
                    .into_any_element(),
            );
        }

        if let Some(example) = example {
            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(4.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_color(dim)
                                    .child(lang.t("workflow.cli.callExample")),
                            )
                            .child(
                                button(
                                    "cli-example-copy",
                                    lang.t(if self.copied {
                                        "cli.copied"
                                    } else {
                                        "cli.copy"
                                    }),
                                    ButtonKind::Plain,
                                    true,
                                    &t,
                                )
                                .on_click(cx.listener(
                                    |editor, _: &ClickEvent, _, cx| editor.copy_example(cx),
                                )),
                            ),
                    )
                    .child(pre(example).debug_selector(|| "cli-call-example".into()))
                    .child(
                        div()
                            .text_color(dim)
                            .child(lang.t("workflow.cli.callExampleHint")),
                    )
                    .into_any_element(),
            );
        }

        if let Some(extract) = command
            .as_ref()
            .and_then(|command| command.extract.as_ref())
        {
            sections.push(
                div()
                    .flex_none()
                    .text_color(dim)
                    .child(fill(
                        lang.t("workflow.cli.extract"),
                        &[
                            ("limit", extract.limit.to_string().as_str()),
                            ("selector", extract.selector.as_str()),
                        ],
                    ))
                    .into_any_element(),
            );
        }

        let mut reveal_at = None;
        if let Some(exported) = &self.exported {
            if self.reveal == Some(Reveal::Export) {
                reveal_at = Some(sections.len());
            }
            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_color(dim)
                            .child(lang.t("workflow.cli.exportHeading")),
                    )
                    .child(pre(exported.clone()).debug_selector(|| "cli-export-text".into()))
                    .into_any_element(),
            );
        }
        if let Some(run_text) = &self.run_text {
            if self.reveal == Some(Reveal::Run) {
                reveal_at = Some(sections.len());
            }
            let json = match (&self.run_json_view, &self.run_json) {
                (Some(view), _) => Some(view.clone().into_any_element()),
                // No viewer from the host: the body as indented text.
                (None, Some(json)) => Some(pre(try_pretty(json)).into_any_element()),
                (None, None) => None,
            };
            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_color(dim)
                            .child(lang.t("workflow.cli.resultHeading")),
                    )
                    .child(
                        div()
                            .debug_selector(|| "cli-run-result".into())
                            .child(pre(run_text.clone()))
                            .children(json),
                    )
                    .into_any_element(),
            );
        }
        if let Some(error) = &self.error {
            sections.push(
                div()
                    .flex_none()
                    .debug_selector(|| "cli-error".into())
                    .text_color(t.error)
                    .child(html_text(error))
                    .into_any_element(),
            );
        }

        if self.reveal.take().is_some()
            && let Some(at) = reveal_at
        {
            self.scroll.scroll_to_top_of_item(at);
        }

        text(12.0)
            .id("cli-editor")
            .debug_selector(|| "cli-editor".into())
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .gap(px(10.0))
            .font_family(SANS)
            .text_color(t.theme.text)
            .children(sections)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str, kind: &str) -> TrafficCall {
        TrafficCall {
            request_id: id.into(),
            method: "GET".into(),
            url: format!("https://a.test/{id}"),
            resource_type: kind.into(),
        }
    }

    fn command(name: &str) -> Value {
        json!({
            "name": name,
            "desc": "",
            "method": "GET",
            "path": "/api/items",
            "effect": "read",
            "evidenceRequestIds": [],
            "params": {},
            "query": {},
            "sessionHeaders": [],
            "responsePointers": [],
        })
    }

    #[test]
    fn 저장된_data를_cli로_읽는다() {
        let data = as_cli(&json!({
            "site": "shop",
            "title": "Shop",
            "baseUrl": "https://shop.test",
            "commands": [command("list")],
            "writes": true
        }));
        assert_eq!(data.site, "shop");
        assert_eq!(data.title, "Shop");
        assert_eq!(data.base_url, "https://shop.test");
        assert_eq!(data.commands.len(), 1);
        assert_eq!(data.commands[0].name, "list");
        assert!(data.writes);
    }

    #[test]
    fn 빠지거나_모양이_다른_값은_빈_값이다() {
        for raw in [
            json!(null),
            json!({}),
            json!("text"),
            json!({ "commands": "x", "site": 5 }),
        ] {
            let data = as_cli(&raw);
            assert_eq!(data.site, "");
            assert_eq!(data.base_url, "");
            assert!(data.commands.is_empty());
            assert!(!data.writes);
        }
        // true가 아닌 값은 쓰기 허용이 아니다.
        assert!(!as_cli(&json!({ "writes": "true" })).writes);
        assert!(!as_cli(&json!({ "writes": 1 })).writes);
    }

    #[test]
    fn 명령이_아닌_항목은_빼고_깨진_추출_규칙은_떼어낸다() {
        let mut with_extract = command("list");
        with_extract["extract"] = json!({ "selector": "", "fields": [] });
        let data = as_cli(&json!({ "commands": [5, { "name": "x" }, with_extract, command("b")] }));
        let names: Vec<&str> = data.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["list", "b"]);
        assert_eq!(data.commands[0].extract, None);
    }

    #[test]
    fn 최근_api_호출만_새것부터_보여준다() {
        let traffic = [
            call("1", "XHR"),
            call("2", "Document"),
            call("3", "Fetch"),
            call("4", "Script"),
            call("5", "XHR"),
        ];
        let ids: Vec<&str> = recent_api_calls(&traffic)
            .iter()
            .map(|entry| entry.request_id.as_str())
            .collect();
        assert_eq!(ids, ["5", "3", "1"]);
        assert!(recent_api_calls(&[]).is_empty());
        // 종류 이름은 대소문자까지 같아야 한다.
        assert!(recent_api_calls(&[call("1", "xhr"), call("2", "fetch")]).is_empty());
    }

    #[test]
    fn 요청은_마흔_개까지만_보여준다() {
        let traffic: Vec<TrafficCall> = (0..60).map(|n| call(&n.to_string(), "XHR")).collect();
        let calls = recent_api_calls(&traffic);
        assert_eq!(calls.len(), MAX_CALLS);
        assert_eq!(calls[0].request_id, "59");
        assert_eq!(calls[MAX_CALLS - 1].request_id, "20");
    }

    #[test]
    fn 요청은_경로와_쿼리로_보여준다() {
        assert_eq!(
            request_path("https://a.test/api/items?q=1&page=2#top"),
            "/api/items?q=1&page=2"
        );
        assert_eq!(request_path("https://a.test"), "/");
        assert_eq!(request_path("https://a.test/x?"), "/x");
        // 절대 URL이 아니면 그대로 보여준다.
        assert_eq!(request_path("/relative/path"), "/relative/path");
        assert_eq!(request_path(""), "");
    }

    // Expected values read from `new URL(...)` under bun.
    #[test]
    fn 요청_경로는_브라우저의_url_풀이와_같다() {
        for (url, shown) in [
            ("data:text/plain,hi?x", "text/plain,hi?x"),
            ("about:blank", "blank"),
            ("http://a.test/a b?x=1 2", "/a%20b?x=1%202"),
            ("HTTP://A.TEST/%7Efoo?x=%20", "/%7Efoo?x=%20"),
            ("http://a.test/a/../b?q", "/b?q"),
            ("blob:https://a.test/uuid", "https://a.test/uuid"),
            ("http://a.test?x", "/?x"),
            ("ws://a.test/s?x#y", "/s?x"),
            ("//a.test/x", "//a.test/x"),
            ("http://a.test/한글?q=값", "/%ED%95%9C%EA%B8%80?q=%EA%B0%92"),
            ("http://a.test/x?#h", "/x"),
            ("file:///tmp/a b", "/tmp/a%20b"),
            ("http://a.test/x?a=b?c", "/x?a=b?c"),
            ("  http://a.test/t  ", "/t"),
            ("http://a.test/\\back?q='\"<>", "//back?q=%27%22%3C%3E"),
        ] {
            assert_eq!(request_path(url), shown, "{url}");
        }
    }

    fn items() -> Vec<Record<String>> {
        let mut first = Record::new();
        first.insert("title".to_owned(), "Keyboard".to_owned());
        first.insert("price".to_owned(), "98,000".to_owned());
        let mut second = Record::new();
        second.insert("title".to_owned(), "Keycaps".to_owned());
        vec![first, second]
    }

    #[test]
    fn 추출한_실행_결과는_줄마다_번호를_붙인다() {
        let result = CliRunResult {
            ok: true,
            status: Some(200),
            items: Some(items()),
            matched: Some(7),
            ..Default::default()
        };
        assert_eq!(
            format_run(&result, Lang::En),
            "200\n1. title: Keyboard  ·  price: 98,000\n2. title: Keycaps\nMatched 7"
        );
        assert!(format_run(&result, Lang::Ko).ends_with("7개 일치"));
    }

    #[test]
    fn 일치_수가_없으면_줄_수를_쓰고_상태가_없으면_줄을_뺀다() {
        let result = CliRunResult {
            ok: true,
            items: Some(items()),
            ..Default::default()
        };
        assert_eq!(
            format_run(&result, Lang::En),
            "1. title: Keyboard  ·  price: 98,000\n2. title: Keycaps\nMatched 2"
        );
        let none = CliRunResult {
            ok: true,
            status: Some(200),
            items: Some(vec![]),
            note: Some(EXTRACT_NOTE_NONE.to_owned()),
            ..Default::default()
        };
        assert_eq!(
            format_run(&none, Lang::En),
            format!(
                "200\nMatched 0\n{}",
                Lang::En.t("workflow.cli.noMatchExtract")
            )
        );
    }

    #[test]
    fn 본문_실행_결과는_상태와_본문이고_잘렸으면_알린다() {
        let result = CliRunResult {
            ok: true,
            status: Some(404),
            body: Some("not found".into()),
            ..Default::default()
        };
        assert_eq!(format_run(&result, Lang::En), "404\nnot found");
        let cut = CliRunResult {
            body_cut: Some(true),
            ..result.clone()
        };
        assert_eq!(
            format_run(&cut, Lang::En),
            format!("404\nnot found\n{}", Lang::En.t("workflow.cli.bodyCut"))
        );
        assert_eq!(format_run(&CliRunResult::default(), Lang::En), "");
    }

    #[test]
    fn 추출_메모는_아는_것만_번역한다() {
        let none = Lang::En.t("workflow.cli.noMatchExtract");
        let capped = Lang::En.t("workflow.cli.pageCapped");
        assert_eq!(note_text(None, Lang::En), None);
        assert_eq!(note_text(Some(""), Lang::En), None);
        assert_eq!(
            note_text(Some(EXTRACT_NOTE_NONE), Lang::En),
            Some(none.to_owned())
        );
        assert_eq!(
            note_text(Some(EXTRACT_NOTE_CAPPED), Lang::En),
            Some(capped.to_owned())
        );
        assert_eq!(
            note_text(
                Some(&format!("{EXTRACT_NOTE_CAPPED}; {EXTRACT_NOTE_NONE}")),
                Lang::En
            ),
            Some(format!("{none} {capped}"))
        );
        assert_eq!(
            note_text(Some("something else"), Lang::En),
            Some("something else".to_owned())
        );
    }
}

#[cfg(test)]
mod view_tests {
    use gpui::{TestAppContext, VisualTestContext};
    use rever_cli_spec::cli_spec::{CapturedCall, CompileOptions, compile_cli};
    use serde_json::json;

    use super::*;
    use crate::testing::{Changes, FakeHost, bind_keys, click, props, retype, shown};

    /// (request id, method, URL, resource type)
    const TRAFFIC: [(&str, &str, &str, &str); 4] = [
        ("1.1", "GET", "https://shop.test/", "Document"),
        (
            "1.2",
            "GET",
            "https://shop.test/api/products?category=keyboard&page=2",
            "Fetch",
        ),
        ("1.3", "GET", "https://shop.test/api/cart", "XHR"),
        ("1.4", "GET", "https://shop.test/app.js", "Script"),
    ];

    fn host() -> FakeHost {
        let host = FakeHost::default();
        *host.traffic.borrow_mut() = TRAFFIC
            .iter()
            .map(|(id, method, url, kind)| TrafficCall {
                request_id: id.to_string(),
                method: method.to_string(),
                url: url.to_string(),
                resource_type: kind.to_string(),
            })
            .collect();
        host
    }

    /// What compiling these captured requests gives.
    fn compiled(ids: &[&str]) -> CliData {
        let calls: Vec<CapturedCall> = TRAFFIC
            .iter()
            .filter(|(id, ..)| ids.contains(id))
            .map(|(id, method, url, kind)| CapturedCall {
                request_id: id.to_string(),
                method: method.to_string(),
                url: url.to_string(),
                resource_type: Some(kind.to_string()),
                status: Some(200),
                mime_type: Some("application/json".to_owned()),
                ..Default::default()
            })
            .collect();
        compile_cli(&calls, &CompileOptions::default()).unwrap()
    }

    fn open<'a>(
        host: &Rc<FakeHost>,
        name: &str,
        data: Value,
        cx: &'a mut TestAppContext,
    ) -> (Entity<CliEditor>, Changes, &'a mut VisualTestContext) {
        cx.update(bind_keys);
        let workflow = Workflow {
            id: "w1".into(),
            kind: "cli".into(),
            name: name.into(),
            data,
            ..Default::default()
        };
        let (props, changes) = props(workflow, host.clone());
        let (editor, cx) = cx.add_window_view(move |_, cx| CliEditor::new(props, cx));
        (editor, changes, cx)
    }

    fn saved(ids: &[&str]) -> Value {
        serde_json::to_value(compiled(ids)).unwrap()
    }

    #[gpui::test]
    fn api_호출만_보여주고_고른_것을_컴파일한다(cx: &mut TestAppContext) {
        let host = Rc::new(host());
        let both = compiled(&["1.2", "1.3"]);
        *host.compiled.borrow_mut() = Some(Ok(CliData {
            writes: true,
            ..both.clone()
        }));
        let (editor, changes, cx) = open(&host, "", json!({}), cx);
        // 문서와 스크립트는 목록에 없다.
        assert!(shown(cx, "cli-request-1.2") && shown(cx, "cli-request-1.3"));
        assert!(!shown(cx, "cli-request-1.1") && !shown(cx, "cli-request-1.4"));

        // 고른 것이 없으면 컴파일 단추가 꺼져 있다.
        click(cx, "cli-compile");
        assert!(host.compile_requests.borrow().is_empty());

        click(cx, "cli-request-1.3");
        click(cx, "cli-request-1.2");
        click(cx, "cli-request-1.3");
        click(cx, "cli-request-1.3");
        click(cx, "cli-compile");
        // 고른 순서대로 보낸다.
        assert_eq!(*host.compile_requests.borrow(), [vec!["1.2", "1.3"]]);

        let first = both.commands[0].name.clone();
        let last = changes.borrow().last().cloned().unwrap();
        // 이름이 비어 있으면 첫 명령의 이름을 쓰고, 쓰기는 꺼진 채로 받는다.
        assert_eq!(last.name, first);
        assert_eq!(last.data, serde_json::to_value(&both).unwrap());
        assert_eq!(last.data["writes"], false);
        let state = editor.read_with(cx, |editor, _| {
            (editor.active.clone(), editor.busy, editor.error.clone())
        });
        assert_eq!(state, (first, None, None));
        assert!(shown(cx, "cli-command-0") && shown(cx, "cli-command-1"));
    }

    #[gpui::test]
    fn 이름이_있으면_컴파일해도_그대로_둔다(cx: &mut TestAppContext) {
        let host = Rc::new(host());
        *host.compiled.borrow_mut() = Some(Ok(compiled(&["1.3"])));
        let (_, changes, cx) = open(&host, "My shop", json!({}), cx);
        click(cx, "cli-request-1.3");
        click(cx, "cli-compile");
        let last = changes.borrow().last().cloned().unwrap();
        assert_eq!(last.name, "My shop");
        assert_eq!(last.data["commands"].as_array().map(Vec::len), Some(1));
    }

    #[gpui::test]
    fn 컴파일이_명령을_못_만들거나_실패하면_까닭을_보여준다(
        cx: &mut TestAppContext,
    ) {
        let host = Rc::new(host());
        *host.compiled.borrow_mut() = Some(Ok(CliData {
            commands: Vec::new(),
            ..compiled(&["1.3"])
        }));
        let (editor, changes, cx) = open(&host, "", json!({}), cx);
        let error = |cx: &mut VisualTestContext| editor.read_with(cx, |e, _| e.error.clone());
        click(cx, "cli-request-1.3");
        click(cx, "cli-compile");
        assert_eq!(
            error(cx).as_deref(),
            Some(Lang::En.t("workflow.cli.noMatch"))
        );
        assert!(shown(cx, "cli-error"));
        assert!(changes.borrow().is_empty());

        *host.compiled.borrow_mut() = Some(Err("capture is gone".into()));
        click(cx, "cli-compile");
        assert_eq!(error(cx).as_deref(), Some("capture is gone"));
        assert_eq!(editor.read_with(cx, |e, _| e.busy), None);
        assert!(changes.borrow().is_empty());
    }

    #[gpui::test]
    fn 플래그는_잡힌_요청에서_한_번만_채우고_고친_값으로_실행한다(
        cx: &mut TestAppContext,
    ) {
        let host = Rc::new(host());
        *host.cli_result.borrow_mut() = Some(Ok(CliRunResult {
            ok: true,
            status: Some(200),
            body: Some(r#"{"items":[]}"#.into()),
            ..Default::default()
        }));
        let data = compiled(&["1.2"]);
        let (editor, changes, cx) = open(&host, "Shop", saved(&["1.2"]), cx);
        let inputs = |cx: &mut VisualTestContext| {
            editor.read_with(cx, |editor, _| {
                editor
                    .inputs
                    .iter()
                    .map(|(name, value)| format!("{name}={value}"))
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(inputs(cx), ["category=keyboard", "page=2"]);
        assert!(shown(cx, "cli-flag-category") && shown(cx, "cli-flag-page"));

        retype(cx, "cli-flag-page", "3");
        // 비운 칸을 잡힌 값으로 다시 채우지 않는다.
        retype(cx, "cli-flag-category", "");
        assert_eq!(inputs(cx), ["category=", "page=3"]);
        // 플래그 값은 저장되는 워크플로에 적히지 않는다.
        assert!(changes.borrow().is_empty());

        click(cx, "cli-run");
        let runs = host.cli_runs.borrow().clone();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].base_url, data.base_url);
        assert!(!runs[0].writes);
        assert_eq!(runs[0].command, data.commands[0]);
        assert_eq!(runs[0].inputs.get("page").map(String::as_str), Some("3"));
        assert_eq!(runs[0].inputs.get("category").map(String::as_str), Some(""));

        // JSON 본문은 상태만 글자로 두고 본문은 뷰어로 보낸다.
        let shown_run = editor.read_with(cx, |editor, _| {
            (
                editor.run_text.clone(),
                editor.run_json.clone(),
                editor.busy,
            )
        });
        assert_eq!(
            shown_run,
            (
                Some("200".to_owned()),
                Some(r#"{"items":[]}"#.to_owned()),
                None
            )
        );
        assert!(shown(cx, "cli-run-result"));
    }

    #[gpui::test]
    fn 실행_결과는_본문이나_실패_까닭을_글자로_보여준다(
        cx: &mut TestAppContext,
    ) {
        let host = Rc::new(host());
        let (editor, _, cx) = open(&host, "Shop", saved(&["1.3"]), cx);
        let run = |result: Result<CliRunResult, String>, cx: &mut VisualTestContext| {
            *host.cli_result.borrow_mut() = Some(result);
            click(cx, "cli-run");
            editor.read_with(cx, |editor, _| {
                (editor.run_text.clone(), editor.run_json.clone())
            })
        };
        let ok = CliRunResult {
            ok: true,
            status: Some(200),
            ..Default::default()
        };
        assert_eq!(
            run(
                Ok(CliRunResult {
                    body: Some("<html>cart</html>".into()),
                    body_cut: Some(true),
                    ..ok.clone()
                }),
                cx
            ),
            (
                Some(format!(
                    "200\n<html>cart</html>\n{}",
                    Lang::En.t("workflow.cli.bodyCut")
                )),
                None
            )
        );
        assert_eq!(
            run(
                Ok(CliRunResult {
                    ok: false,
                    error: Some("writes disabled for this CLI".into()),
                    ..Default::default()
                }),
                cx
            ),
            (Some("writes disabled for this CLI".to_owned()), None)
        );
        assert_eq!(
            run(Ok(CliRunResult::default()), cx),
            (Some(Lang::En.t("workflow.cli.runFailed").to_owned()), None)
        );
        assert_eq!(
            run(Err("no active tab".into()), cx),
            (Some("no active tab".to_owned()), None)
        );
        // 실행 실패는 결과 자리에 쓰고, 오류 줄은 쓰지 않는다.
        assert_eq!(editor.read_with(cx, |editor, _| editor.error.clone()), None);
        assert_eq!(host.cli_runs.borrow().len(), 4);
    }

    #[gpui::test]
    fn 명령을_고르고_쓰기를_켜면_그_명령을_쓰기_허용으로_실행한다(
        cx: &mut TestAppContext,
    ) {
        let host = Rc::new(host());
        let data = compiled(&["1.2", "1.3"]);
        let (editor, changes, cx) = open(&host, "Shop", saved(&["1.2", "1.3"]), cx);
        let active = |cx: &mut VisualTestContext| editor.read_with(cx, |e, _| e.active.clone());
        assert_eq!(active(cx), data.commands[0].name);
        click(cx, "cli-command-1");
        assert_eq!(active(cx), data.commands[1].name);
        // 명령을 고르는 것은 워크플로를 바꾸지 않는다.
        assert!(changes.borrow().is_empty());

        click(cx, "cli-writes");
        assert_eq!(changes.borrow().last().unwrap().data["writes"], true);
        click(cx, "cli-run");
        let runs = host.cli_runs.borrow().clone();
        assert_eq!(runs[0].command, data.commands[1]);
        assert!(runs[0].writes);

        click(cx, "cli-writes");
        assert_eq!(changes.borrow().last().unwrap().data["writes"], false);
    }

    #[gpui::test]
    fn 명령이_없으면_내보내기와_실행이_꺼져_있다(cx: &mut TestAppContext) {
        let host = Rc::new(host());
        let (editor, _, cx) = open(&host, "", json!({}), cx);
        click(cx, "cli-export");
        click(cx, "cli-run");
        assert_eq!(editor.read_with(cx, |e, _| e.exported.clone()), None);
        assert!(host.cli_runs.borrow().is_empty());
        assert!(!shown(cx, "cli-call-example") && !shown(cx, "cli-example-copy"));
    }

    #[gpui::test]
    fn 내보내기는_curl을_보여주고_복사는_호출_예시를_클립보드에_넣는다(
        cx: &mut TestAppContext,
    ) {
        let host = Rc::new(host());
        let data = compiled(&["1.2"]);
        let (editor, _, cx) = open(&host, "Shop", saved(&["1.2"]), cx);
        click(cx, "cli-export");
        assert_eq!(
            editor.read_with(cx, |e, _| e.exported.clone()),
            Some(export_curl(&data))
        );
        assert!(shown(cx, "cli-export-text"));

        click(cx, "cli-example-copy");
        let mut inputs = Record::new();
        inputs.insert("category".to_owned(), "keyboard".to_owned());
        inputs.insert("page".to_owned(), "2".to_owned());
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(cli_run_example("w1", &data.commands[0], &inputs))
        );
        let copied = |cx: &mut VisualTestContext| editor.read_with(cx, |e, _| e.copied);
        assert!(copied(cx));
        // 1.2초 뒤에 단추가 "Copy"로 돌아온다.
        cx.executor()
            .advance_clock(Duration::from_millis(COPIED_MS - 1));
        cx.run_until_parked();
        assert!(copied(cx));
        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        assert!(!copied(cx));
    }
}
