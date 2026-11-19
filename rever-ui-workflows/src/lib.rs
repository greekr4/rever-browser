//! Port of `src/renderer/src/workflows/index.ts` — the workflows subsystem: the panel and its macro, pipeline, CLI and template editors.
//!
//! The TypeScript barrel registers each workflow kind by importing its module;
//! [`default_registry`] does the same by calling each module's `kind()`. To remove a
//! kind, delete its folder and its line there — the core (panel/store) needs no other
//! changes.
//!
//! The host makes one [`WorkflowsStore`] entity over its storage, implements
//! [`WorkflowHost`], calls [`bind_keys`] once, and shows a [`WorkflowPanel`]. It
//! subscribes to [`UseInChat`] to put a template's prompt in the chat input, and answers
//! the macro and CLI tools with [`core::agent_bridge::handle_agent_request`].

pub mod cli;
pub mod core;
pub mod host;
mod js;
pub mod macros;
pub mod pipeline;
pub mod template;
pub mod text_area;
mod ui;

#[cfg(test)]
pub(crate) mod testing;

pub use crate::core::registry::{WorkflowKind, WorkflowRegistry};
pub use crate::core::store::WorkflowsStore;
pub use crate::core::types::Workflow;
pub use crate::core::workflow_panel::{UseInChat, WorkflowPanel};
pub use crate::host::WorkflowHost;

/// Every workflow kind, in the order the panel offers them.
pub fn default_registry() -> WorkflowRegistry {
    let mut registry = WorkflowRegistry::new();
    registry.register(template::kind());
    registry.register(macros::kind());
    registry.register(pipeline::kind());
    registry.register(cli::kind());
    registry
}

/// Editing keys of the multi-line fields. The single-line ones are bound by
/// `rever_ui_kit::text_input::bind_keys`.
pub fn bind_keys(cx: &mut gpui::App) {
    text_area::bind_keys(cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 네_종류를_템플릿_매크로_파이프라인_cli_순으로_등록한다() {
        let registry = default_registry();
        let ids: Vec<&str> = registry.list().iter().map(|kind| kind.id).collect();
        assert_eq!(ids, ["template", "macro", "pipeline", "cli"]);
        let labels: Vec<&str> = registry.list().iter().map(|kind| kind.label).collect();
        assert_eq!(labels, ["Template", "Macro", "Pipeline", "CLI"]);
    }

    #[test]
    fn 등록된_종류는_제_종류의_빈_워크플로를_만든다() {
        let registry = default_registry();
        for kind in registry.list() {
            let workflow = (kind.create)();
            assert_eq!(workflow.kind, kind.id);
            assert_eq!(workflow.name, "");
            assert!(!kind.description.is_empty());
        }
    }

    #[test]
    fn 목록에서_바로_쓰는_동작은_템플릿과_매크로에만_있다() {
        let registry = default_registry();
        let with_action: Vec<&str> = registry
            .list()
            .iter()
            .filter(|kind| kind.action.is_some())
            .map(|kind| kind.id)
            .collect();
        assert_eq!(with_action, ["template", "macro"]);
    }
}
