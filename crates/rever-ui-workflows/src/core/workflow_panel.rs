//! Port of `src/renderer/src/workflows/core/WorkflowPanel.tsx` — the host panel for all workflow kinds.
//!
//! It knows nothing about individual kinds — it lists whatever modules have registered
//! and delegates editing to each kind's editor. A kind's list action that seeds the
//! agent chat is emitted as [`UseInChat`], where the component pushed to the chat-draft
//! store.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyView, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, FontWeight, KeyDownEvent,
    ScrollHandle, Subscription, Window, anchored, deferred, div, point, prelude::*, px, rgba,
};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::ThemeMode;

use super::registry::{EditorProps, WorkflowAction, WorkflowKind, WorkflowRegistry};
use super::store::WorkflowsStore;
use super::types::Workflow;
use crate::host::WorkflowHost;
use crate::js::js_trim;
use crate::ui::{ButtonKind, SANS, Tokens, Z_DIALOG, button, shadow, single_line, text, tooltip};

// Inline English in the component (not dictionary keys), kept as it is there.
const NEW_LABEL: &str = "New:";
const NO_KINDS: &str = "No workflow kinds installed.";
const NO_WORKFLOWS: &str = "No saved workflows yet.";
const SAVE: &str = "Save";
const RUNNING: &str = "Running…";
const RUN: &str = "Run";
const DELETE_TITLE: &str = "Delete this workflow?";
const UNTITLED: &str = "Untitled";

/// A list action asked to put this text in the agent chat's input
/// (`useChatDraft.getState().push(body)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UseInChat(pub String);

/// The workflow currently being created/edited (a working copy) and its editor.
struct Draft {
    workflow: Workflow,
    /// `None` when no kind is registered for the workflow.
    editor: Option<AnyView>,
}

pub struct WorkflowPanel {
    store: Entity<WorkflowsStore>,
    registry: Rc<WorkflowRegistry>,
    host: Rc<dyn WorkflowHost>,
    lang: Lang,
    theme_mode: ThemeMode,
    draft: Option<Draft>,
    /// Per-row state for the list's action button (no editor open to report into).
    running_id: Option<String>,
    row_error: HashMap<String, String>,
    /// Workflow awaiting delete confirmation. Deleting is irreversible (the store
    /// persists), so ✕ asks first.
    confirm_id: Option<String>,
    /// Lives while the confirmation is open: Escape cancels it wherever the focus is.
    escape: Option<Subscription>,
    dialog_focus: FocusHandle,
    list_scroll: ScrollHandle,
}

impl EventEmitter<UseInChat> for WorkflowPanel {}

impl WorkflowPanel {
    pub fn new(
        store: Entity<WorkflowsStore>,
        registry: Rc<WorkflowRegistry>,
        host: Rc<dyn WorkflowHost>,
        lang: Lang,
        theme_mode: ThemeMode,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        Self {
            store,
            registry,
            host,
            lang,
            theme_mode,
            draft: None,
            running_id: None,
            row_error: HashMap::new(),
            confirm_id: None,
            escape: None,
            dialog_focus: cx.focus_handle(),
            list_scroll: ScrollHandle::new(),
        }
    }

    /// The working copy while an editor is open.
    pub fn draft(&self) -> Option<&Workflow> {
        self.draft.as_ref().map(|draft| &draft.workflow)
    }

    /// The open editor's view; `None` in the list and for a kind without an editor.
    pub fn editor_view(&self) -> Option<&AnyView> {
        self.draft.as_ref().and_then(|draft| draft.editor.as_ref())
    }

    /// The "+ Kind" buttons: open the editor on a blank workflow of that kind.
    pub fn start_new(&mut self, kind_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(kind) = self.registry.get(kind_id) {
            let workflow = (kind.create)();
            self.open(workflow, window, cx);
        }
    }

    /// The row's Edit button: open the editor on a copy of the saved workflow.
    pub fn edit(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let saved = self.store.read(cx).workflows().iter().find(|w| w.id == id);
        if let Some(workflow) = saved.cloned() {
            self.open(workflow, window, cx);
        }
    }

    fn open(&mut self, workflow: Workflow, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.weak_entity();
        let editor = self.registry.get(&workflow.kind).map(|kind| {
            let props = EditorProps {
                workflow: workflow.clone(),
                on_change: Rc::new(move |workflow, cx: &mut App| {
                    let _ = panel.update(cx, |panel, cx| {
                        if let Some(draft) = &mut panel.draft {
                            draft.workflow = workflow;
                            cx.notify();
                        }
                    });
                }),
                host: self.host.clone(),
                lang: self.lang,
                theme_mode: self.theme_mode,
            };
            (kind.editor)(props, window, cx)
        });
        self.draft = Some(Draft { workflow, editor });
        cx.notify();
    }

    pub fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.draft = None;
        cx.notify();
    }

    pub fn save(&mut self, cx: &mut Context<Self>) {
        if let Some(draft) = self.draft.take() {
            self.store.update(cx, |store, cx| {
                // A failed write keeps the workflow in the list, as with `persist`.
                let _ = store.upsert(draft.workflow);
                cx.notify();
            });
        }
        cx.notify();
    }

    /// The row's action button.
    pub fn run_from_list(&mut self, id: &str, cx: &mut Context<Self>) {
        let saved = self.store.read(cx).workflows().iter().find(|w| w.id == id);
        let Some(workflow) = saved.cloned() else {
            return;
        };
        let Some(action) = self
            .registry
            .get(&workflow.kind)
            .and_then(|kind| kind.action)
        else {
            return;
        };
        self.running_id = Some(workflow.id.clone());
        self.row_error.remove(&workflow.id);
        cx.notify();
        match action(&workflow) {
            Err(error) => self.finish_run(&workflow.id, Err(error), cx),
            Ok(WorkflowAction::Done) => self.finish_run(&workflow.id, Ok(()), cx),
            Ok(WorkflowAction::ChatDraft(prompt)) => {
                cx.emit(UseInChat(prompt));
                self.finish_run(&workflow.id, Ok(()), cx);
            }
            Ok(WorkflowAction::RunSteps { steps, check }) => {
                // Per-step results are only shown in the editor, so progress is dropped.
                let run = self.host.run(steps, Box::new(|_, _| {}), cx);
                let id = workflow.id;
                cx.spawn(async move |panel, cx| {
                    let outcome = run.await.and_then(|results| check(&results));
                    let _ = panel.update(cx, |panel, cx| panel.finish_run(&id, outcome, cx));
                })
                .detach();
            }
        }
    }

    fn finish_run(&mut self, id: &str, outcome: Result<(), String>, cx: &mut Context<Self>) {
        if let Err(error) = outcome {
            self.row_error.insert(id.to_owned(), error);
        }
        if self.running_id.as_deref() == Some(id) {
            self.running_id = None;
        }
        cx.notify();
    }

    /// The row's ✕ button: ask before deleting.
    pub fn ask_delete(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_id = Some(id.to_owned());
        self.escape = Some(cx.observe_keystrokes(|panel, event, _, cx| {
            if event.keystroke.key == "escape" {
                panel.cancel_delete(cx);
            }
        }));
        // The Delete button takes focus in the component, so Enter confirms.
        window.focus(&self.dialog_focus);
        cx.notify();
    }

    pub fn cancel_delete(&mut self, cx: &mut Context<Self>) {
        self.confirm_id = None;
        self.escape = None;
        cx.notify();
    }

    pub fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.confirm_id.take() {
            self.store.update(cx, |store, cx| {
                let _ = store.remove(&id);
                cx.notify();
            });
        }
        self.escape = None;
        cx.notify();
    }

    fn render_draft(
        &self,
        draft: &Draft,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let kind = self.registry.get(&draft.workflow.kind);
        let can_save = !js_trim(&draft.workflow.name).is_empty();
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(kind.map_or_else(|| draft.workflow.kind.clone(), |k| k.label.into())),
            )
            .child(
                div()
                    .ml_auto()
                    .flex()
                    .gap(px(6.0))
                    .child(
                        button(
                            "workflow-cancel",
                            self.lang.t("common.cancel"),
                            ButtonKind::Plain,
                            true,
                            t,
                        )
                        .on_click(
                            cx.listener(|panel, _: &ClickEvent, _, cx| panel.cancel_edit(cx)),
                        ),
                    )
                    .child(
                        button("workflow-save", SAVE, ButtonKind::Primary, can_save, t).when(
                            can_save,
                            |save| {
                                save.on_click(
                                    cx.listener(|panel, _: &ClickEvent, _, cx| panel.save(cx)),
                                )
                            },
                        ),
                    ),
            );
        let body = match &draft.editor {
            // The editor scrolls itself, so the macro's Run bar can stay pinned under it.
            Some(editor) => div().flex_1().min_h_0().child(editor.clone()),
            None => div().text_color(t.theme.text_dim).child(format!(
                "No editor registered for kind “{}”.",
                draft.workflow.kind
            )),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(header)
            .child(body)
    }

    fn render_row(
        &self,
        index: usize,
        workflow: &Workflow,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let kind: Option<&WorkflowKind> = self.registry.get(&workflow.kind);
        let busy = self.running_id.as_deref() == Some(workflow.id.as_str());
        let error = self.row_error.get(&workflow.id);
        let lang = self.lang;

        let action = kind.filter(|kind| kind.action.is_some()).map(|kind| {
            let label = if busy {
                RUNNING
            } else {
                kind.action_label.unwrap_or(RUN)
            };
            let id = workflow.id.clone();
            button(
                ("workflow-action", index),
                label,
                ButtonKind::Plain,
                !busy,
                t,
            )
            .when(!busy, |action| {
                action.on_click(
                    cx.listener(move |panel, _: &ClickEvent, _, cx| panel.run_from_list(&id, cx)),
                )
            })
        });
        let edit_id = workflow.id.clone();
        let delete_id = workflow.id.clone();

        let line = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_none()
                    .min_w(px(58.0))
                    .text_size(px(10.0))
                    .line_height(px(12.0))
                    .text_color(t.theme.text_dim)
                    // text-transform: uppercase; GPUI has no letter-spacing (0.4px).
                    .child(
                        kind.map_or_else(|| workflow.kind.clone(), |kind| kind.label.to_owned())
                            .to_uppercase(),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(single_line(&workflow.name)),
            )
            .children(action)
            .child(
                button(
                    ("workflow-edit", index),
                    lang.t("common.edit"),
                    ButtonKind::Plain,
                    true,
                    t,
                )
                .tooltip(tooltip(lang.t("common.edit"), t))
                .on_click(cx.listener(move |panel, _: &ClickEvent, window, cx| {
                    panel.edit(&edit_id, window, cx)
                })),
            )
            .child(
                button(("workflow-delete", index), "✕", ButtonKind::Plain, true, t)
                    .tooltip(tooltip(lang.t("common.delete"), t))
                    .on_click(cx.listener(move |panel, _: &ClickEvent, window, cx| {
                        panel.ask_delete(&delete_id, window, cx)
                    })),
            );

        div()
            .id(("workflow-row", index))
            .debug_selector(move || format!("workflow-row-{index}"))
            .flex()
            .flex_col()
            .flex_none()
            // The component wraps the error onto its own line; the row gap is the same 8px.
            .gap(px(8.0))
            .px(px(8.0))
            .py(px(6.0))
            .border_1()
            .border_color(t.theme.border)
            .rounded(px(6.0))
            .bg(t.surface)
            .child(line)
            .when_some(error, |row, error| {
                row.child(
                    text(11.0)
                        .debug_selector(move || format!("workflow-row-error-{index}"))
                        .text_color(t.error)
                        .child(error.clone()),
                )
            })
    }

    fn render_list(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let kinds = self.registry.list();
        let new_row = div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .mr(px(4.0))
                    .text_color(t.theme.text_dim)
                    .child(NEW_LABEL),
            )
            .when(kinds.is_empty(), |row| {
                row.child(div().text_color(t.theme.text_dim).child(NO_KINDS))
            })
            .children(kinds.iter().map(|kind| {
                let id = kind.id;
                button(
                    gpui::ElementId::Name(format!("workflow-new-{id}").into()),
                    format!("+ {}", kind.label),
                    ButtonKind::Plain,
                    true,
                    t,
                )
                .tooltip(tooltip(kind.description, t))
                .on_click(cx.listener(move |panel, _: &ClickEvent, window, cx| {
                    panel.start_new(id, window, cx)
                }))
            }));

        let workflows = self.store.read(cx).workflows().to_vec();
        let list = if workflows.is_empty() {
            // `.panel-empty`
            text(11.0)
                .px(px(12.0))
                .py(px(24.0))
                .text_center()
                .text_color(t.theme.text_dim)
                .child(NO_WORKFLOWS)
                .into_any_element()
        } else {
            div()
                .id("workflow-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.list_scroll)
                .flex()
                .flex_col()
                .gap(px(4.0))
                .children(
                    workflows
                        .iter()
                        .enumerate()
                        .map(|(index, workflow)| self.render_row(index, workflow, t, cx)),
                )
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(new_row)
            .child(list)
    }

    /// Compact delete confirmation over the whole window, so the panel's overflow can't
    /// clip it. Escape or a backdrop click cancels; Enter confirms.
    fn render_confirm(
        &self,
        pending: &Workflow,
        window: &Window,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let viewport = window.viewport_size();
        let name = if pending.name.is_empty() {
            UNTITLED.to_owned()
        } else {
            single_line(&pending.name)
        };
        let dialog = div()
            .id("workflow-delete-dialog")
            .debug_selector(|| "workflow-delete-dialog".into())
            .track_focus(&self.dialog_focus)
            .on_key_down(cx.listener(|panel, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "enter" {
                    panel.confirm_delete(cx);
                }
            }))
            // A click inside the dialog is not a backdrop click.
            .on_click(|_, _, cx| cx.stop_propagation())
            .w(px(280.0))
            .p(px(14.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .bg(t.surface)
            .border_1()
            .border_color(t.theme.border_2)
            .rounded(px(8.0))
            .shadow(vec![shadow(12.0, 40.0, t.shadow)])
            .child(
                text(13.0)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(DELETE_TITLE),
            )
            .child(
                text(12.0)
                    .id("workflow-delete-name")
                    .truncate()
                    .text_color(t.theme.text_dim)
                    .tooltip(tooltip(pending.name.clone(), t))
                    .child(name),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(6.0))
                    .child(
                        button(
                            "workflow-delete-cancel",
                            self.lang.t("common.cancel"),
                            ButtonKind::Ghost,
                            true,
                            t,
                        )
                        .on_click(
                            cx.listener(|panel, _: &ClickEvent, _, cx| panel.cancel_delete(cx)),
                        ),
                    )
                    .child(
                        button(
                            "workflow-delete-confirm",
                            self.lang.t("common.delete"),
                            ButtonKind::Danger,
                            true,
                            t,
                        )
                        .on_click(
                            cx.listener(|panel, _: &ClickEvent, _, cx| panel.confirm_delete(cx)),
                        ),
                    ),
            );
        let backdrop = div()
            .id("workflow-delete-backdrop")
            .debug_selector(|| "workflow-delete-backdrop".into())
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000073))
            .font_family(SANS)
            .text_color(t.theme.text)
            .occlude()
            .on_click(cx.listener(|panel, _: &ClickEvent, _, cx| panel.cancel_delete(cx)))
            .child(dialog);
        deferred(anchored().position(point(px(0.0), px(0.0))).child(backdrop))
            .with_priority(Z_DIALOG)
    }
}

impl Render for WorkflowPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::resolve(self.theme_mode, window);
        let content = match &self.draft {
            Some(draft) => self.render_draft(draft, &t, cx).into_any_element(),
            None => self.render_list(&t, cx).into_any_element(),
        };
        let pending = self.confirm_id.as_ref().and_then(|id| {
            let workflows = self.store.read(cx).workflows();
            workflows.iter().find(|w| &w.id == id).cloned()
        });
        let confirm = match (&self.draft, pending) {
            // The dialog belongs to the list; an open editor has no ✕.
            (None, Some(pending)) => Some(self.render_confirm(&pending, window, &t, cx)),
            _ => None,
        };
        text(12.0)
            .size_full()
            .p(px(10.0))
            .font_family(SANS)
            .text_color(t.theme.text)
            .child(content)
            .children(confirm)
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::RefCell;

    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use rever_ui_state::{MemoryStorage, Storage};
    use serde_json::{Value, json};

    use super::*;
    use crate::core::store::STORAGE_KEY;
    use crate::host::{StepStatus, WorkflowStepProgress};
    use crate::testing::{FakeHost, bind_keys, click, retype, shown, tool};

    struct Opened<'a> {
        panel: Entity<WorkflowPanel>,
        store: Entity<WorkflowsStore>,
        storage: Rc<MemoryStorage>,
        /// Every prompt a list action asked to put in the chat input.
        prompts: Rc<RefCell<Vec<String>>>,
        cx: &'a mut VisualTestContext,
    }

    impl Opened<'_> {
        /// (id, name) of the saved workflows, newest first.
        fn saved(&mut self) -> Vec<(String, String)> {
            self.store.read_with(self.cx, |store, _| {
                store
                    .workflows()
                    .iter()
                    .map(|w| (w.id.clone(), w.name.clone()))
                    .collect()
            })
        }

        fn draft(&mut self) -> Option<Workflow> {
            self.panel
                .read_with(self.cx, |panel, _| panel.draft().cloned())
        }

        fn confirming(&mut self) -> Option<String> {
            self.panel
                .read_with(self.cx, |panel, _| panel.confirm_id.clone())
        }

        fn row_error(&mut self, id: &str) -> Option<String> {
            self.panel
                .read_with(self.cx, |panel, _| panel.row_error.get(id).cloned())
        }
    }

    /// The panel over a store holding `saved`, given oldest first.
    fn open<'a>(
        host: &Rc<FakeHost>,
        saved: Vec<Workflow>,
        cx: &'a mut TestAppContext,
    ) -> Opened<'a> {
        cx.update(bind_keys);
        let storage = Rc::new(MemoryStorage::new());
        let store = cx.new(|_| {
            let mut store = WorkflowsStore::new(storage.clone());
            for (at, workflow) in saved.into_iter().enumerate() {
                store.upsert_at(workflow, at as u64 + 1).unwrap();
            }
            store
        });
        let (registry, host) = (Rc::new(crate::default_registry()), host.clone());
        let panel_store = store.clone();
        let (panel, cx) = cx.add_window_view(move |_, cx| {
            WorkflowPanel::new(panel_store, registry, host, Lang::En, ThemeMode::Light, cx)
        });
        let prompts: Rc<RefCell<Vec<String>>> = Rc::default();
        cx.update(|_, cx| {
            let prompts = prompts.clone();
            cx.subscribe(&panel, move |_, event: &UseInChat, _| {
                prompts.borrow_mut().push(event.0.clone())
            })
            .detach();
        });
        Opened {
            panel,
            store,
            storage,
            prompts,
            cx,
        }
    }

    fn workflow(id: &str, kind: &str, name: &str, data: Value) -> Workflow {
        Workflow {
            id: id.into(),
            kind: kind.into(),
            name: name.into(),
            data,
            ..Default::default()
        }
    }

    fn template(id: &str, name: &str, body: &str) -> Workflow {
        workflow(id, "template", name, json!({ "body": body }))
    }

    fn macro_with(id: &str, steps: Value) -> Workflow {
        workflow(id, "macro", "Replay", json!({ "steps": steps, "vars": "" }))
    }

    fn pair(id: &str, name: &str) -> (String, String) {
        (id.to_owned(), name.to_owned())
    }

    #[gpui::test]
    fn 새_워크플로는_이름을_적어야_저장된다(cx: &mut TestAppContext) {
        let host = Rc::new(FakeHost::default());
        let mut at = open(&host, vec![], cx);
        assert!(shown(at.cx, "workflow-new-template") && shown(at.cx, "workflow-new-cli"));
        click(at.cx, "workflow-new-template");
        let draft = at.draft().unwrap();
        assert_eq!((draft.kind.as_str(), draft.name.as_str()), ("template", ""));
        assert!(
            at.panel
                .read_with(at.cx, |panel, _| panel.editor_view().is_some())
        );

        // 이름이 비었거나 공백뿐이면 Save가 꺼져 있다.
        click(at.cx, "workflow-save");
        retype(at.cx, "template-name", "   ");
        click(at.cx, "workflow-save");
        assert!(at.draft().is_some());
        assert!(at.saved().is_empty());

        retype(at.cx, "template-name", "Explain");
        retype(at.cx, "template-body", "Explain this request.");
        // 저장하기 전에는 목록이 그대로다.
        assert!(at.saved().is_empty());
        click(at.cx, "workflow-save");
        assert_eq!(at.draft(), None);
        assert_eq!(at.saved(), [pair(&draft.id, "Explain")]);
        let stored: Value = serde_json::from_str(&at.storage.get(STORAGE_KEY).unwrap()).unwrap();
        assert_eq!(
            stored["state"]["workflows"][0]["data"],
            json!({ "body": "Explain this request." })
        );
        assert!(shown(at.cx, "workflow-row-0"));
    }

    #[gpui::test]
    fn 취소하면_고친_것을_버린다(cx: &mut TestAppContext) {
        let host = Rc::new(FakeHost::default());
        let mut at = open(&host, vec![template("a", "A", "body")], cx);
        click(at.cx, "workflow-edit-0");
        retype(at.cx, "template-name", "Changed");
        assert_eq!(
            at.draft().map(|draft| draft.name),
            Some("Changed".to_owned())
        );
        click(at.cx, "workflow-cancel");
        assert_eq!(at.draft(), None);
        assert_eq!(at.saved(), [pair("a", "A")]);

        click(at.cx, "workflow-new-macro");
        click(at.cx, "workflow-cancel");
        assert_eq!(at.saved(), [pair("a", "A")]);
    }

    #[gpui::test]
    fn 고쳐서_저장하면_바꿔_넣고_맨_앞으로_올린다(cx: &mut TestAppContext) {
        let host = Rc::new(FakeHost::default());
        let mut at = open(
            &host,
            vec![template("a", "A", "first"), template("b", "B", "second")],
            cx,
        );
        assert_eq!(at.saved(), [pair("b", "B"), pair("a", "A")]);
        click(at.cx, "workflow-edit-1");
        assert_eq!(at.draft().map(|draft| draft.id), Some("a".to_owned()));
        retype(at.cx, "template-name", "A2");
        click(at.cx, "workflow-save");
        assert_eq!(at.saved(), [pair("a", "A2"), pair("b", "B")]);
        let body = at
            .store
            .read_with(at.cx, |store, _| store.workflows()[0].data.clone());
        assert_eq!(body, json!({ "body": "first" }));
    }

    #[gpui::test]
    fn 지우기는_먼저_묻고_취소하는_길이_셋이다(cx: &mut TestAppContext) {
        let host = Rc::new(FakeHost::default());
        let mut at = open(
            &host,
            vec![template("a", "A", ""), template("b", "B", "")],
            cx,
        );
        click(at.cx, "workflow-delete-0");
        assert_eq!(at.confirming().as_deref(), Some("b"));
        assert!(shown(at.cx, "workflow-delete-dialog"));
        // 대화상자 안을 누르는 것은 취소가 아니다.
        click(at.cx, "workflow-delete-dialog");
        assert_eq!(at.confirming().as_deref(), Some("b"));

        click(at.cx, "workflow-delete-cancel");
        assert_eq!(at.confirming(), None);

        click(at.cx, "workflow-delete-0");
        at.cx.simulate_keystrokes("escape");
        at.cx.run_until_parked();
        assert_eq!(at.confirming(), None);

        click(at.cx, "workflow-delete-0");
        let backdrop = at.cx.debug_bounds("workflow-delete-backdrop").unwrap();
        at.cx
            .simulate_click(backdrop.origin + point(px(5.0), px(5.0)), Modifiers::none());
        at.cx.run_until_parked();
        assert_eq!(at.confirming(), None);
        assert_eq!(at.saved().len(), 2);
    }

    #[gpui::test]
    fn 지우기를_확인하면_저장소에서도_사라진다(cx: &mut TestAppContext) {
        let host = Rc::new(FakeHost::default());
        let mut at = open(
            &host,
            vec![template("a", "A", ""), template("b", "B", "")],
            cx,
        );
        click(at.cx, "workflow-delete-0");
        click(at.cx, "workflow-delete-confirm");
        assert_eq!(at.confirming(), None);
        assert_eq!(at.saved(), [pair("a", "A")]);

        // Delete 단추에 초점이 있어 Enter가 확인이다.
        click(at.cx, "workflow-delete-0");
        at.cx.simulate_keystrokes("enter");
        at.cx.run_until_parked();
        assert!(at.saved().is_empty());
        assert_eq!(
            at.storage.get(STORAGE_KEY).as_deref(),
            Some(r#"{"state":{"workflows":[]},"version":0}"#)
        );
        // 확인 뒤의 Escape는 아무것도 하지 않는다.
        at.cx.simulate_keystrokes("escape");
        assert_eq!(at.confirming(), None);
    }

    #[gpui::test]
    fn use는_템플릿_본문을_채팅으로_보내라고_알린다(cx: &mut TestAppContext) {
        let host = Rc::new(FakeHost::default());
        let mut at = open(
            &host,
            vec![
                template("blank", "Blank", "  \n"),
                template("a", "A", "Explain {{request}}"),
            ],
            cx,
        );
        click(at.cx, "workflow-action-0");
        assert_eq!(*at.prompts.borrow(), ["Explain {{request}}"]);
        // 빈 본문은 보내지 않고, 오류도 아니다.
        click(at.cx, "workflow-action-1");
        assert_eq!(at.prompts.borrow().len(), 1);
        assert_eq!(at.row_error("blank"), None);
        assert_eq!(
            at.panel
                .read_with(at.cx, |panel, _| panel.running_id.clone()),
            None
        );
        assert!(host.runs.borrow().is_empty());
    }

    #[gpui::test]
    fn 목록에서_매크로를_실행하고_실패는_그_줄에_보여준다(
        cx: &mut TestAppContext,
    ) {
        let host = Rc::new(FakeHost {
            tools: vec![tool("t", "")],
            ..Default::default()
        });
        let steps = json!([{ "id": "s1", "tool": "t", "input": "{ \"n\": 1 }" }]);
        let mut at = open(
            &host,
            vec![macro_with("empty", json!([])), macro_with("m", steps)],
            cx,
        );
        click(at.cx, "workflow-action-0");
        assert_eq!(host.runs.borrow().len(), 1);
        assert_eq!(host.runs.borrow()[0][0].input, json!({ "n": 1 }));
        assert_eq!(at.row_error("m"), None);

        // 실패한 단계는 줄 아래에 적힌다.
        *host.step_results.borrow_mut() = vec![WorkflowStepProgress {
            index: 0,
            tool: "t".into(),
            status: StepStatus::Error,
            output: None,
            error: Some("boom".into()),
            waiting_for: None,
        }];
        click(at.cx, "workflow-action-0");
        assert_eq!(at.row_error("m").as_deref(), Some("Step 1 (t): boom"));
        assert!(shown(at.cx, "workflow-row-error-0"));

        // 다시 실행해 성공하면 오류가 지워진다.
        host.step_results.borrow_mut().clear();
        click(at.cx, "workflow-action-0");
        assert_eq!(at.row_error("m"), None);

        // 실행 자체가 실패해도, 단계가 없어도 그 줄의 오류다.
        *host.run_error.borrow_mut() = Some("executor offline".into());
        click(at.cx, "workflow-action-0");
        assert_eq!(at.row_error("m").as_deref(), Some("executor offline"));
        let runs = host.runs.borrow().len();
        click(at.cx, "workflow-action-1");
        assert_eq!(at.row_error("empty").as_deref(), Some("No steps to run"));
        assert_eq!(host.runs.borrow().len(), runs);
    }

    #[gpui::test]
    fn 실행_중인_줄의_단추는_다시_눌리지_않는다(cx: &mut TestAppContext) {
        let host = Rc::new(FakeHost::default());
        host.hold.set(true);
        let steps = json!([{ "id": "s1", "tool": "t", "input": "" }]);
        let at = open(&host, vec![macro_with("m", steps)], cx);
        click(at.cx, "workflow-action-0");
        assert_eq!(
            at.panel
                .read_with(at.cx, |panel, _| panel.running_id.clone()),
            Some("m".to_owned())
        );
        click(at.cx, "workflow-action-0");
        assert_eq!(host.runs.borrow().len(), 1);
    }

    #[gpui::test]
    fn 등록되지_않은_종류는_동작_단추도_편집기도_없다(cx: &mut TestAppContext) {
        let host = Rc::new(FakeHost::default());
        let mut at = open(
            &host,
            vec![workflow("old", "recorder", "Recorded", Value::Null)],
            cx,
        );
        assert!(shown(at.cx, "workflow-row-0"));
        assert!(!shown(at.cx, "workflow-action-0"));
        click(at.cx, "workflow-edit-0");
        assert_eq!(
            at.draft().map(|draft| draft.kind),
            Some("recorder".to_owned())
        );
        assert!(
            at.panel
                .read_with(at.cx, |panel, _| panel.editor_view().is_none())
        );
        // 이름이 있으니 그대로 저장할 수는 있다.
        click(at.cx, "workflow-save");
        assert_eq!(at.saved(), [pair("old", "Recorded")]);
    }

    #[gpui::test]
    fn 파이프라인과_cli_줄에는_동작_단추가_없다(cx: &mut TestAppContext) {
        let host = Rc::new(FakeHost::default());
        let at = open(
            &host,
            vec![
                workflow("c", "cli", "Shop", json!({})),
                workflow("p", "pipeline", "Flow", json!({ "nodes": [], "vars": "" })),
            ],
            cx,
        );
        assert!(shown(at.cx, "workflow-edit-0") && shown(at.cx, "workflow-edit-1"));
        assert!(!shown(at.cx, "workflow-action-0") && !shown(at.cx, "workflow-action-1"));
    }
}
