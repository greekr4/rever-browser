//! Port of `src/renderer/src/workflows/macro/index.ts` — M1, macros (hand-authored, deterministic replay).
//!
//! A named sequence of MCP tool calls that runs with no LLM in the loop. The
//! editor doubles as the runner (Run button + per-step results). Self-contained:
//! delete this folder + its line in `crate::default_registry` to remove the kind.
//! (The folder is `macros` because `macro` is a reserved word in Rust.)

pub mod macro_editor;

use crate::core::registry::{WorkflowAction, WorkflowKind};
use crate::core::types::{Workflow, new_workflow_id, now_ms};
use crate::host::{StepStatus, WorkflowStepProgress};
use macro_editor::{MacroData, resolve_steps};

fn create() -> Workflow {
    let now = now_ms();
    Workflow {
        id: new_workflow_id(),
        kind: "macro".to_owned(),
        name: String::new(),
        description: None,
        data: MacroData::default().to_value(),
        created_at: now,
        updated_at: now,
    }
}

/// The first failed step of a finished run, as the list row's error.
pub fn first_failure(results: &[WorkflowStepProgress]) -> Result<(), String> {
    match results
        .iter()
        .find(|result| result.status == StepStatus::Error)
    {
        Some(failed) => Err(format!(
            "Step {} ({}): {}",
            failed.index + 1,
            failed.tool,
            // The component interpolates the missing error as it is.
            failed.error.as_deref().unwrap_or("undefined")
        )),
        None => Ok(()),
    }
}

/// One-click replay from the list. Per-step results are only shown in the
/// editor, so this reports failures and otherwise stays quiet.
fn action(workflow: &Workflow) -> Result<WorkflowAction, String> {
    let data = MacroData::from_value(&workflow.data);
    let steps = resolve_steps(&data.steps, &data.vars)?;
    if steps.is_empty() {
        return Err("No steps to run".to_owned());
    }
    Ok(WorkflowAction::RunSteps {
        steps,
        check: first_failure,
    })
}

pub fn kind() -> WorkflowKind {
    WorkflowKind {
        id: "macro",
        label: "Macro",
        description: "A saved sequence of tool calls, replayed deterministically.",
        create,
        editor: macro_editor::open,
        action_label: Some("▶ Run"),
        action: Some(action),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn progress(index: usize, status: StepStatus, error: Option<&str>) -> WorkflowStepProgress {
        WorkflowStepProgress {
            index,
            tool: format!("tool_{index}"),
            status,
            output: None,
            error: error.map(str::to_owned),
            waiting_for: None,
        }
    }

    #[test]
    fn 빈_매크로를_만든다() {
        let workflow = create();
        assert!(workflow.id.starts_with('w'));
        assert_eq!(workflow.kind, "macro");
        assert_eq!(workflow.name, "");
        assert_eq!(workflow.data, json!({ "steps": [], "vars": "" }));
        assert_eq!(workflow.created_at, workflow.updated_at);
    }

    #[test]
    fn 종류의_이름과_동작_라벨() {
        let kind = kind();
        assert_eq!(kind.id, "macro");
        assert_eq!(kind.label, "Macro");
        assert_eq!(kind.action_label, Some("▶ Run"));
    }

    #[test]
    fn 목록에서_실행하면_풀린_단계를_돌려_달라고_한다() {
        let workflow = Workflow {
            data: json!({
                "steps": [{ "id": "s1", "tool": "browser_navigate", "input": "{ \"url\": \"{{u}}\" }" }],
                "vars": "{ \"u\": \"https://a.test\" }"
            }),
            ..create()
        };
        match action(&workflow) {
            Ok(WorkflowAction::RunSteps { steps, .. }) => {
                assert_eq!(steps.len(), 1);
                assert_eq!(steps[0].tool, "browser_navigate");
                assert_eq!(steps[0].input, json!({ "url": "https://a.test" }));
            }
            _ => panic!("expected a run"),
        }
    }

    #[test]
    fn 단계가_없거나_풀_수_없으면_실행하지_않고_오류다() {
        assert_eq!(action(&create()).err(), Some("No steps to run".to_owned()));
        let broken = Workflow {
            data: json!({ "steps": [{ "id": "s1", "tool": "t", "input": "{ nope" }], "vars": "" }),
            ..create()
        };
        assert_eq!(
            action(&broken).err(),
            Some("Step \"t\" input is not valid JSON".to_owned())
        );
        let bad_vars = Workflow {
            data: json!({ "steps": [{ "id": "s1", "tool": "t", "input": "" }], "vars": "{" }),
            ..create()
        };
        assert_eq!(
            action(&bad_vars).err(),
            Some("Variables are not valid JSON".to_owned())
        );
    }

    #[test]
    fn 실패한_첫_단계를_한_줄로_알린다() {
        let results = [
            progress(0, StepStatus::Done, None),
            progress(1, StepStatus::Error, Some("element not found")),
            progress(2, StepStatus::Error, Some("later")),
        ];
        assert_eq!(
            first_failure(&results),
            Err("Step 2 (tool_1): element not found".to_owned())
        );
    }

    #[test]
    fn 실패한_단계가_없으면_조용하다() {
        assert_eq!(first_failure(&[]), Ok(()));
        assert_eq!(
            first_failure(&[
                progress(0, StepStatus::Done, None),
                progress(1, StepStatus::Waiting, None)
            ]),
            Ok(())
        );
        // 오류 문구가 없는 실패도 실패다.
        assert_eq!(
            first_failure(&[progress(0, StepStatus::Error, None)]),
            Err("Step 1 (tool_0): undefined".to_owned())
        );
    }
}
