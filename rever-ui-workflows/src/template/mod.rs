//! Port of `src/renderer/src/workflows/template/index.ts` — M2, prompt templates.
//!
//! Reusable prompt snippets that seed the agent chat input. Fully self-contained:
//! delete this folder + its line in `crate::default_registry` to remove the kind entirely.

pub mod template_editor;

use serde_json::Value;

use crate::core::registry::{WorkflowAction, WorkflowKind};
use crate::core::types::{Workflow, new_workflow_id, now_ms};
use crate::js::js_trim;
use template_editor::TemplateData;

fn create() -> Workflow {
    let now = now_ms();
    Workflow {
        id: new_workflow_id(),
        kind: "template".to_owned(),
        name: String::new(),
        description: None,
        data: TemplateData::default().to_value(),
        created_at: now,
        updated_at: now,
    }
}

/// "Use": the body goes to the chat input, unless it is blank.
fn action(workflow: &Workflow) -> Result<WorkflowAction, String> {
    let body = match workflow.data.get("body") {
        Some(Value::String(body)) => body.clone(),
        // The component calls `.trim()` on whatever is there.
        Some(Value::Null) | None => {
            return Err("Cannot read properties of undefined (reading 'trim')".to_owned());
        }
        Some(_) => return Err("body.trim is not a function".to_owned()),
    };
    Ok(if js_trim(&body).is_empty() {
        WorkflowAction::Done
    } else {
        WorkflowAction::ChatDraft(body)
    })
}

pub fn kind() -> WorkflowKind {
    WorkflowKind {
        id: "template",
        label: "Template",
        description: "Reusable prompt inserted into the agent chat input.",
        create,
        editor: template_editor::open,
        action_label: Some("Use"),
        action: Some(action),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn with_body(body: Value) -> Workflow {
        Workflow {
            data: json!({ "body": body }),
            ..create()
        }
    }

    #[test]
    fn 빈_템플릿을_만든다() {
        let workflow = create();
        assert!(workflow.id.starts_with('w'));
        assert_eq!(workflow.kind, "template");
        assert_eq!(workflow.name, "");
        assert_eq!(workflow.description, None);
        assert_eq!(workflow.data, json!({ "body": "" }));
        assert_eq!(workflow.created_at, workflow.updated_at);
        assert!(workflow.created_at > 1_600_000_000_000);
    }

    #[test]
    fn 종류의_이름과_동작_라벨() {
        let kind = kind();
        assert_eq!(kind.id, "template");
        assert_eq!(kind.label, "Template");
        assert_eq!(kind.action_label, Some("Use"));
        assert!(kind.action.is_some());
    }

    #[test]
    fn use는_본문을_채팅_입력으로_보낸다() {
        let action = action(&with_body(json!("  Summarise the traffic.\n")));
        // 다듬지 않은 본문 그대로다.
        assert!(matches!(
            action,
            Ok(WorkflowAction::ChatDraft(body)) if body == "  Summarise the traffic.\n"
        ));
    }

    #[test]
    fn 공백뿐인_본문은_보내지_않는다() {
        for body in ["", "   ", "\n\t", "\u{feff}"] {
            assert!(
                matches!(action(&with_body(json!(body))), Ok(WorkflowAction::Done)),
                "{body:?}"
            );
        }
    }

    #[test]
    fn 본문이_글자가_아니면_오류다() {
        let missing = Workflow {
            data: json!({}),
            ..create()
        };
        assert!(action(&missing).is_err());
        assert!(action(&with_body(json!(5))).is_err());
    }
}
