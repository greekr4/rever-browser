//! Port of `src/renderer/src/workflows/cli/index.ts` — the CLI kind: commands compiled from captured API calls.

pub mod cli_editor;

use rever_cli_spec::cli_spec::CliData;

use crate::core::registry::WorkflowKind;
use crate::core::types::{Workflow, new_workflow_id, now_ms};

fn empty() -> CliData {
    CliData {
        site: String::new(),
        title: String::new(),
        base_url: String::new(),
        commands: Vec::new(),
        writes: false,
    }
}

fn create() -> Workflow {
    let now = now_ms();
    Workflow {
        id: new_workflow_id(),
        kind: "cli".to_owned(),
        name: String::new(),
        description: None,
        data: serde_json::to_value(empty()).unwrap_or_default(),
        created_at: now,
        updated_at: now,
    }
}

pub fn kind() -> WorkflowKind {
    WorkflowKind {
        id: "cli",
        label: "CLI",
        description: "Commands compiled from the API calls you actually made.",
        create,
        editor: cli_editor::open,
        action_label: None,
        action: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 빈_cli를_만든다() {
        let workflow = create();
        assert!(workflow.id.starts_with('w'));
        assert_eq!(workflow.kind, "cli");
        assert_eq!(workflow.name, "");
        assert_eq!(
            workflow.data,
            json!({ "site": "", "title": "", "baseUrl": "", "commands": [], "writes": false })
        );
        assert_eq!(workflow.created_at, workflow.updated_at);
    }

    #[test]
    fn 목록에는_동작_버튼이_없다() {
        let kind = kind();
        assert_eq!(kind.id, "cli");
        assert_eq!(kind.label, "CLI");
        assert!(kind.action.is_none());
    }
}
