//! Port of `src/main/mcp/bridge.ts` — running tools by name without an agent in
//! the loop, for the workflow executor and the macro editor's tool list.
//!
//! The TypeScript connected an MCP client to the in-process server over HTTP and
//! kept that connection in a module variable. The tools live in the same process
//! here, so the bridge is a struct over the registry and calls them directly;
//! what a caller sees — the tool list, and a result flattened to its text — is
//! the same.

use std::sync::Arc;

use rever_tools_api::tool::{ToolContext, ToolRegistry};
use rever_tools_api::utils::Content;
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
}

/// What a caller keeps of a tool result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolResult {
    pub text: String,
    pub is_error: bool,
}

/// A standalone way into the tools, intentionally separate from the agent's own
/// connection so workflow features never touch the agent hot path.
pub struct Bridge {
    registry: Arc<ToolRegistry>,
}

impl Bridge {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }

    // Macros and workflows run tools by name, so they get every tool.
    pub fn list_mcp_tools(&self) -> Vec<McpToolInfo> {
        let defs = self.registry.defs().into_iter();
        defs.map(|def| McpToolInfo {
            name: def.name,
            description: Some(def.description),
            input_schema: def.input_schema,
        })
        .collect()
    }

    pub async fn call_mcp_tool(
        &self,
        ctx: &ToolContext,
        name: &str,
        input: Value,
    ) -> McpToolResult {
        let Some(tool) = self.registry.find(name) else {
            // What the MCP server answers for a name it does not know.
            return McpToolResult {
                text: format!("MCP error -32602: Tool {name} not found"),
                is_error: true,
            };
        };
        let result = tool.call(ctx, input).await;
        let parts: Vec<String> = result
            .content
            .into_iter()
            .map(|Content::Text { text }| text)
            .collect();
        McpToolResult {
            text: parts.join("\n"),
            is_error: result.is_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use rever_tools_api::tool::{BoxFuture, Tool, ToolDef};
    use rever_tools_api::utils::{ToolResult, err, ok};
    use serde_json::json;

    use super::*;

    /// Answers with the lines it was given, as separate content parts.
    struct Lines;

    impl Tool for Lines {
        fn def(&self) -> ToolDef {
            ToolDef::new(
                "lines",
                "one part per line",
                json!({ "type": "object", "properties": { "lines": { "type": "array" } } }),
            )
        }

        fn call<'a>(&'a self, _: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
            Box::pin(async move {
                let lines = args["lines"].as_array().cloned().unwrap_or_default();
                let content = lines
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|line| Content::Text {
                        text: line.to_owned(),
                    });
                ToolResult {
                    is_error: false,
                    content: content.collect(),
                }
            })
        }
    }

    struct Fails;

    impl Tool for Fails {
        fn def(&self) -> ToolDef {
            ToolDef::new("fails", "", json!({ "type": "object", "properties": {} }))
        }

        fn call<'a>(&'a self, _: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
            Box::pin(async move {
                if args.is_null() {
                    ok("fine")
                } else {
                    err("boom")
                }
            })
        }
    }

    fn bridge() -> Bridge {
        let mut registry = ToolRegistry::default();
        registry.register(Lines);
        registry.register(Fails);
        Bridge::new(Arc::new(registry))
    }

    #[test]
    fn 등록된_도구를_이름과_설명과_스키마로_나열한다() {
        let tools = bridge().list_mcp_tools();
        assert_eq!(
            serde_json::to_value(&tools).unwrap(),
            json!([
                {
                    "name": "lines",
                    "description": "one part per line",
                    "inputSchema": { "type": "object", "properties": { "lines": { "type": "array" } } }
                },
                {
                    "name": "fails",
                    "description": "",
                    "inputSchema": { "type": "object", "properties": {} }
                }
            ])
        );
    }

    #[test]
    fn 설명이_없는_도구_정보는_설명_키_없이_직렬화한다() {
        let info = McpToolInfo {
            name: "x".to_owned(),
            description: None,
            input_schema: json!({}),
        };
        assert_eq!(
            serde_json::to_string(&info).unwrap(),
            "{\"name\":\"x\",\"inputSchema\":{}}"
        );
    }

    #[tokio::test]
    async fn 결과의_텍스트_조각을_줄바꿈으로_이어_돌려준다() {
        let ctx = ToolContext::for_tests();
        let bridge = bridge();
        let result = bridge
            .call_mcp_tool(&ctx, "lines", json!({ "lines": ["a", "b", "c"] }))
            .await;
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({ "text": "a\nb\nc", "isError": false })
        );
        // No content at all is an empty text, not an error.
        let empty = bridge.call_mcp_tool(&ctx, "lines", json!({})).await;
        assert_eq!((empty.text.as_str(), empty.is_error), ("", false));
    }

    #[tokio::test]
    async fn 도구가_알린_오류는_is_error로_전한다() {
        let ctx = ToolContext::for_tests();
        let bridge = bridge();
        let failed = bridge.call_mcp_tool(&ctx, "fails", json!({})).await;
        assert_eq!((failed.text.as_str(), failed.is_error), ("boom", true));
        let fine = bridge.call_mcp_tool(&ctx, "fails", Value::Null).await;
        assert_eq!((fine.text.as_str(), fine.is_error), ("fine", false));
    }

    #[tokio::test]
    async fn 모르는_도구는_서버와_같은_글의_오류로_답한다() {
        let ctx = ToolContext::for_tests();
        let missing = bridge().call_mcp_tool(&ctx, "nope", json!({})).await;
        assert_eq!(
            missing,
            McpToolResult {
                text: "MCP error -32602: Tool nope not found".to_owned(),
                is_error: true,
            }
        );
    }

    #[tokio::test]
    async fn 이_크레이트의_도구도_이름으로_부른다() {
        let mut registry = ToolRegistry::default();
        crate::register(&mut registry);
        let bridge = Bridge::new(Arc::new(registry));
        assert_eq!(bridge.list_mcp_tools().len(), 26);
        let result = bridge
            .call_mcp_tool(&ToolContext::for_tests(), "browser_wait_for", json!({}))
            .await;
        assert_eq!(
            result,
            McpToolResult {
                text: "give at least one of `selector` or `text`".to_owned(),
                is_error: true,
            }
        );
    }
}
