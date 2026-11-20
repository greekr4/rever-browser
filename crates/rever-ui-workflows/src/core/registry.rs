//! Port of `src/renderer/src/workflows/core/registry.ts` — the pluggable workflow kinds.
//!
//! The module-level `Map` becomes [`WorkflowRegistry`]; the modules register into the one
//! `crate::default_registry` builds.

use std::rc::Rc;

use gpui::{AnyView, App, Window};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::ThemeMode;

use super::types::Workflow;
use crate::host::{WorkflowHost, WorkflowRunStep, WorkflowStepProgress};

/// Called with the whole working copy after every edit.
pub type OnChange = Rc<dyn Fn(Workflow, &mut App)>;

/// What a list item's primary action does with its workflow.
pub type ActionFn = fn(&Workflow) -> Result<WorkflowAction, String>;

/// `WorkflowEditorProps`, plus what the React editors took from context: the IPC
/// surface, the language and the theme.
#[derive(Clone)]
pub struct EditorProps {
    pub workflow: Workflow,
    /// Called with the whole working copy after every edit.
    pub on_change: OnChange,
    pub host: Rc<dyn WorkflowHost>,
    pub lang: Lang,
    pub theme_mode: ThemeMode,
}

/// Builds the editor view for a workflow being created or edited.
pub type EditorFactory = fn(EditorProps, &mut Window, &mut App) -> AnyView;

/// What a kind's list action asks the panel to do. The TypeScript actions did these
/// themselves, through the chat-draft store and `window.rev`.
pub enum WorkflowAction {
    /// Nothing to do.
    Done,
    /// Put this text in the agent chat's input.
    ChatDraft(String),
    /// Replay these steps; `check` turns the finished steps into the row's error.
    RunSteps {
        steps: Vec<WorkflowRunStep>,
        check: fn(&[WorkflowStepProgress]) -> Result<(), String>,
    },
}

/// A pluggable workflow kind. Modules register one of these, so adding/removing a
/// module is a folder + one line in `default_registry` — the core discovers kinds
/// purely through this registry.
#[derive(Clone)]
pub struct WorkflowKind {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// Build a blank workflow of this kind.
    pub create: fn() -> Workflow,
    /// Editor rendered when a workflow of this kind is opened.
    pub editor: EditorFactory,
    /// Optional primary action shown per-item in the list (e.g. "Use", "Run").
    pub action_label: Option<&'static str>,
    pub action: Option<ActionFn>,
}

#[derive(Clone, Default)]
pub struct WorkflowRegistry {
    // A JavaScript Map: insertion order, and setting an existing key keeps its place.
    kinds: Vec<WorkflowKind>,
}

impl WorkflowRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, kind: WorkflowKind) {
        match self.kinds.iter_mut().find(|other| other.id == kind.id) {
            Some(existing) => *existing = kind,
            None => self.kinds.push(kind),
        }
    }

    pub fn get(&self, id: &str) -> Option<&WorkflowKind> {
        self.kinds.iter().find(|kind| kind.id == id)
    }

    pub fn list(&self) -> &[WorkflowKind] {
        &self.kinds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_editor(_: EditorProps, _: &mut Window, _: &mut App) -> AnyView {
        unreachable!("the registry tests open no editor")
    }

    fn kind(id: &'static str, label: &'static str) -> WorkflowKind {
        WorkflowKind {
            id,
            label,
            description: "",
            create: Workflow::default,
            editor: no_editor,
            action_label: None,
            action: None,
        }
    }

    fn ids(registry: &WorkflowRegistry) -> Vec<&str> {
        registry.list().iter().map(|kind| kind.id).collect()
    }

    #[test]
    fn 등록한_순서대로_나열한다() {
        let mut registry = WorkflowRegistry::new();
        assert!(registry.list().is_empty());
        registry.register(kind("template", "Template"));
        registry.register(kind("macro", "Macro"));
        registry.register(kind("cli", "CLI"));
        assert_eq!(ids(&registry), ["template", "macro", "cli"]);
    }

    #[test]
    fn id로_찾고_없는_id는_none이다() {
        let mut registry = WorkflowRegistry::new();
        registry.register(kind("macro", "Macro"));
        assert_eq!(registry.get("macro").map(|kind| kind.label), Some("Macro"));
        assert!(registry.get("pipeline").is_none());
        assert!(registry.get("").is_none());
    }

    #[test]
    fn 같은_id로_다시_등록하면_자리는_그대로_두고_바꿔_넣는다() {
        let mut registry = WorkflowRegistry::new();
        registry.register(kind("template", "Template"));
        registry.register(kind("macro", "Macro"));
        registry.register(kind("template", "Prompt"));
        assert_eq!(ids(&registry), ["template", "macro"]);
        assert_eq!(
            registry.get("template").map(|kind| kind.label),
            Some("Prompt")
        );
    }
}
