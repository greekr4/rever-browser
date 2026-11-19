//! What the views ask of the application: everything the React code did through
//! `window.rev.workflows.*` (IPC to the main process), plus the two things it read from
//! other panels — the captured requests and the JSON body viewer.
//!
//! The host implements [`WorkflowHost`] once; the preview and the tests supply fakes.
//! The value types mirror the preload's (`src/preload/index.ts`), with the same JSON
//! field names.

use gpui::{AnyView, App, Task};
use rever_cli_spec::cli_spec::{CliCommand, CliData, CliRunBody, Record};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::js::number_value;
use crate::pipeline::resolve::ResolvedPipeNode;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub input_schema: Value,
}

fn js_delay<S: serde::Serializer>(delay: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error> {
    delay.map(number_value).serialize(serializer)
}

/// One runnable macro step: `{{var}}` already substituted, the input parsed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunStep {
    pub tool: String,
    pub input: Value,
    /// Wait until this CSS selector matches before running the step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_for: Option<String>,
    /// Extra pause (ms) before running the step, applied after `wait_for`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "js_delay"
    )]
    pub delay: Option<f64>,
    /// Store this step's result under this name for later `{{name}}` references.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_as: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Waiting,
    Running,
    Done,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStepProgress {
    pub index: usize,
    pub tool: String,
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Human-readable reason shown while the status is `waiting`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PipeStatus {
    Running,
    Done,
    Error,
    Branch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BranchTaken {
    Then,
    Else,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipeProgress {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub status: PipeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken: Option<BranchTaken>,
}

/// The argument of `runCli`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliRunPayload {
    pub base_url: String,
    pub writes: bool,
    pub command: CliCommand,
    pub inputs: Record<String>,
}

/// What `runCli` resolves to.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliRunResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_cut: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<Record<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl CliRunResult {
    /// The part `run_json_body` of the CLI spec reads.
    pub fn body_part(&self) -> CliRunBody {
        CliRunBody {
            status: self.status,
            body: self.body.clone(),
            body_cut: self.body_cut,
            items: self.items.clone(),
        }
    }
}

/// One captured request, as much of a traffic entry as the CLI editor reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrafficCall {
    pub request_id: String,
    pub method: String,
    pub url: String,
    /// CDP's resource type: "XHR", "Fetch", "Document", …
    pub resource_type: String,
}

/// Called for each progress message of a run, on the main thread. A host may call it
/// before `run` returns: the editors apply each message after the current update.
pub type OnProgress<T> = Box<dyn Fn(T, &mut App)>;

pub trait WorkflowHost {
    /// `listTools`: the MCP tools a step can call.
    fn list_tools(&self, cx: &mut App) -> Task<Vec<McpToolInfo>>;

    /// `run`: replay resolved macro steps on the active tab. Progress is streamed per
    /// step; the task ends with every step's last state, or `Err` when the run itself
    /// could not be made.
    fn run(
        &self,
        steps: Vec<WorkflowRunStep>,
        on_progress: OnProgress<WorkflowStepProgress>,
        cx: &mut App,
    ) -> Task<Result<Vec<WorkflowStepProgress>, String>>;

    /// `runPipeline`: run a resolved, branch-aware node tree.
    fn run_pipeline(
        &self,
        nodes: Vec<ResolvedPipeNode>,
        on_progress: OnProgress<PipeProgress>,
        cx: &mut App,
    ) -> Task<Result<bool, String>>;

    /// `cancel`: stop the run in progress.
    fn cancel(&self, cx: &mut App);

    /// `compileCli`: turn the captured requests with these ids into CLI commands.
    fn compile_cli(&self, request_ids: Vec<String>, cx: &mut App) -> Task<Result<CliData, String>>;

    /// `runCli`: run one command of a CLI with the given flag values.
    fn run_cli(&self, payload: CliRunPayload, cx: &mut App) -> Task<Result<CliRunResult, String>>;

    /// The traffic store's entries in capture order (oldest first). Read on every
    /// render of the CLI editor; the host notifies the panel when the traffic changes.
    fn traffic(&self, cx: &App) -> Vec<TrafficCall>;

    /// The collapsible JSON viewer of the traffic panel, for a CLI run that returned
    /// JSON. `None` shows the body as indented text instead.
    fn json_view(&self, _text: &str, _cx: &mut App) -> Option<AnyView> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 실행_단계는_preload와_같은_필드_이름으로_직렬화한다() {
        let step = WorkflowRunStep {
            tool: "browser_click".into(),
            input: json!({ "ref": "r1" }),
            wait_for: Some("#go".into()),
            delay: Some(300.0),
            save_as: Some("clicked".into()),
        };
        assert_eq!(
            serde_json::to_string(&step).unwrap(),
            r##"{"tool":"browser_click","input":{"ref":"r1"},"waitFor":"#go","delay":300,"saveAs":"clicked"}"##
        );
        let bare = WorkflowRunStep {
            tool: "t".into(),
            input: json!({}),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&bare).unwrap(),
            r#"{"tool":"t","input":{}}"#
        );
    }

    #[test]
    fn 진행_메시지를_preload_형식에서_읽는다() {
        let step: WorkflowStepProgress = serde_json::from_value(
            json!({ "index": 2, "tool": "t", "status": "waiting", "waitingFor": "#a" }),
        )
        .unwrap();
        assert_eq!(step.status, StepStatus::Waiting);
        assert_eq!(step.waiting_for.as_deref(), Some("#a"));

        let pipe: PipeProgress =
            serde_json::from_value(json!({ "nodeId": "n1", "status": "branch", "taken": "else" }))
                .unwrap();
        assert_eq!(pipe.status, PipeStatus::Branch);
        assert_eq!(pipe.taken, Some(BranchTaken::Else));
    }

    #[test]
    fn 실행_결과에서_json_판정에_쓰는_부분만_꺼낸다() {
        let result = CliRunResult {
            ok: true,
            status: Some(200),
            body: Some("{}".into()),
            matched: Some(3),
            ..Default::default()
        };
        assert_eq!(
            result.body_part(),
            CliRunBody {
                status: Some(200),
                body: Some("{}".into()),
                body_cut: None,
                items: None
            }
        );
    }
}
