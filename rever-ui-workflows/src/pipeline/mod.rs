//! Port of `src/renderer/src/workflows/pipeline/index.ts` — M3, the visual pipeline.
//!
//! A branch-aware flow of tool nodes and if-conditions, rendered as a nested node
//! view and run on the shared executor. Self-contained: delete this folder + its
//! line in `crate::default_registry` to remove the kind.

pub mod pipeline_editor;
pub mod resolve;
pub mod types;

use crate::core::registry::WorkflowKind;
use crate::core::types::{Workflow, new_workflow_id, now_ms};
use types::PipelineData;

fn create() -> Workflow {
    let now = now_ms();
    Workflow {
        id: new_workflow_id(),
        kind: "pipeline".to_owned(),
        name: String::new(),
        description: None,
        data: PipelineData::default().to_value(),
        created_at: now,
        updated_at: now,
    }
}

pub fn kind() -> WorkflowKind {
    WorkflowKind {
        id: "pipeline",
        label: "Pipeline",
        description: "A branch-aware flow of tool calls with if/then/else conditions.",
        create,
        editor: pipeline_editor::open,
        action_label: None,
        action: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 빈_파이프라인을_만든다() {
        let workflow = create();
        assert!(workflow.id.starts_with('w'));
        assert_eq!(workflow.kind, "pipeline");
        assert_eq!(workflow.name, "");
        assert_eq!(workflow.data, json!({ "nodes": [], "vars": "" }));
        assert_eq!(workflow.created_at, workflow.updated_at);
    }

    #[test]
    fn 목록에는_동작_버튼이_없다() {
        let kind = kind();
        assert_eq!(kind.id, "pipeline");
        assert_eq!(kind.label, "Pipeline");
        assert_eq!(kind.action_label, None);
        assert!(kind.action.is_none());
    }
}
