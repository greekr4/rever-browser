//! Port of `src/renderer/src/workflows/pipeline/types.ts` — the pipeline node graph (editor form).
//!
//! Tool inputs are JSON text (may contain `{{var}}`); resolve.rs compiles this into the
//! runnable tree the executor takes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::types::{new_id, now_ms};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CondOn {
    #[default]
    Output,
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CondOp {
    #[default]
    Contains,
    Equals,
    Matches,
    Always,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipeCond {
    pub on: CondOn,
    pub op: CondOp,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PipeNode {
    Tool {
        id: String,
        tool: String,
        input: String,
    },
    If {
        id: String,
        cond: PipeCond,
        then: Vec<PipeNode>,
        #[serde(rename = "else")]
        otherwise: Vec<PipeNode>,
    },
}

impl PipeNode {
    pub fn id(&self) -> &str {
        match self {
            PipeNode::Tool { id, .. } | PipeNode::If { id, .. } => id,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PipelineData {
    pub nodes: Vec<PipeNode>,
    pub vars: String,
}

impl PipelineData {
    /// The `data` of a pipeline workflow. The TypeScript casts without looking; here a
    /// node that is not a node is skipped and anything else missing is empty.
    pub fn from_value(data: &Value) -> Self {
        let nodes = data
            .get("nodes")
            .and_then(Value::as_array)
            .map(|nodes| {
                nodes
                    .iter()
                    .filter_map(|node| PipeNode::deserialize(node).ok())
                    .collect()
            })
            .unwrap_or_default();
        let vars = data.get("vars").and_then(Value::as_str).unwrap_or_default();
        Self {
            nodes,
            vars: vars.to_owned(),
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

pub fn node_id() -> String {
    new_id('n', now_ms(), 4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 노드는_type으로_구분되는_json이다() {
        let nodes = vec![
            PipeNode::Tool {
                id: "n1".into(),
                tool: "browser_navigate".into(),
                input: "{}".into(),
            },
            PipeNode::If {
                id: "n2".into(),
                cond: PipeCond {
                    on: CondOn::Error,
                    op: CondOp::Matches,
                    value: "5\\d\\d".into(),
                },
                then: vec![],
                otherwise: vec![PipeNode::Tool {
                    id: "n3".into(),
                    tool: "t".into(),
                    input: "".into(),
                }],
            },
        ];
        let data = PipelineData {
            nodes,
            vars: "{}".into(),
        };
        let value = data.to_value();
        assert_eq!(
            value,
            json!({
                "nodes": [
                    { "type": "tool", "id": "n1", "tool": "browser_navigate", "input": "{}" },
                    {
                        "type": "if",
                        "id": "n2",
                        "cond": { "on": "error", "op": "matches", "value": "5\\d\\d" },
                        "then": [],
                        "else": [{ "type": "tool", "id": "n3", "tool": "t", "input": "" }]
                    }
                ],
                "vars": "{}"
            })
        );
        assert_eq!(PipelineData::from_value(&value), data);
    }

    #[test]
    fn 모양이_맞지_않는_데이터는_빈_파이프라인으로_읽는다() {
        assert_eq!(
            PipelineData::from_value(&json!(null)),
            PipelineData::default()
        );
        assert_eq!(
            PipelineData::from_value(&json!({ "nodes": "x", "vars": 3 })),
            PipelineData::default()
        );
        // 노드가 아닌 항목만 건너뛴다.
        let data = PipelineData::from_value(&json!({
            "nodes": [5, { "type": "loop" }, { "type": "tool", "id": "n1", "tool": "t", "input": "" }],
            "vars": "{}"
        }));
        assert_eq!(data.nodes.len(), 1);
        assert_eq!(data.nodes[0].id(), "n1");
        assert_eq!(data.vars, "{}");
    }

    #[test]
    fn 노드_id는_n으로_시작하고_난수_네_자리로_끝난다() {
        let id = new_id('n', 1_700_000_000_000, 4);
        assert!(id.starts_with("nloyw3v28"), "{id}");
        assert_eq!(id.len(), 1 + 8 + 4);
        assert!(node_id().starts_with('n'));
    }
}
