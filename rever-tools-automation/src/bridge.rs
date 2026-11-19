//! How a workflow reaches the other tools, standing in for the MCP client of
//! `src/main/mcp/bridge.ts`. There the executor connects to the in-process MCP
//! server as a client; here it looks tools up in the registry the app hands over
//! once everything is registered (`ctx.state::<ToolRegistryHandle>().set(…)`).

use std::future::Future;
use std::sync::{Arc, Mutex};

use rever_tool_profile::tool_profile::CatalogKind;
use rever_tools_api::tool::{Tool, ToolContext, ToolRegistry};
use rever_tools_api::utils::Content;
use serde_json::Value;

use crate::lock;

tokio::task_local! {
    static CALL_CATALOG: CatalogKind;
}

/// Runs a tool call as coming from a session of that catalog. The TypeScript
/// registers `run_macro` once per MCP session with the session's catalog; here a
/// tool is registered once for every session, so the catalog travels with the call.
pub async fn with_catalog<F: Future>(catalog: CatalogKind, call: F) -> F::Output {
    CALL_CATALOG.scope(catalog, call).await
}

/// The catalog of the session the current call came through. A call nobody
/// labelled counts as `follow`, the catalog of a session without the header, so
/// the macro guard is on unless a caller is known to hold the full catalog.
pub fn call_catalog() -> CatalogKind {
    CALL_CATALOG
        .try_with(|catalog| *catalog)
        .unwrap_or(CatalogKind::Follow)
}

/// The registry workflows run their steps through.
#[derive(Default)]
pub struct ToolRegistryHandle(Mutex<Option<Arc<ToolRegistry>>>);

impl ToolRegistryHandle {
    pub fn set(&self, registry: Arc<ToolRegistry>) {
        *lock(&self.0) = Some(registry);
    }

    pub fn get(&self) -> Option<Arc<ToolRegistry>> {
        lock(&self.0).clone()
    }
}

fn registry(ctx: &ToolContext) -> Result<Arc<ToolRegistry>, String> {
    ctx.state::<ToolRegistryHandle>()
        .get()
        .ok_or_else(|| "the tool registry is not connected to workflows yet".to_owned())
}

// The bridge client asks for the full catalog ("macros and workflows run tools by
// name, so they need every tool"), and that catalog has no profile to switch.
fn in_full_catalog(name: &str) -> bool {
    name != "tool_profile_set"
}

/// Names of the tools a workflow step may call.
pub fn list_mcp_tools(ctx: &ToolContext) -> Result<Vec<String>, String> {
    let names = registry(ctx)?.defs().into_iter().map(|def| def.name);
    Ok(names.filter(|name| in_full_catalog(name)).collect())
}

/// What a workflow keeps of a tool result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutcome {
    pub text: String,
    pub is_error: bool,
}

/// Calls one tool by name. `Err` is the call itself failing (no registry), which
/// the TypeScript saw as a rejected promise; a tool reporting an error is an
/// `Ok` outcome with `is_error` set.
pub async fn call_mcp_tool(
    ctx: &ToolContext,
    name: &str,
    input: Value,
) -> Result<ToolOutcome, String> {
    let tool: Option<Arc<dyn Tool>> = registry(ctx)?.find(name).filter(|_| in_full_catalog(name));
    let Some(tool) = tool else {
        // The SDK reports an unknown tool inside the result, not as a failed call.
        return Ok(ToolOutcome {
            text: format!("MCP error -32602: Tool {name} not found"),
            is_error: true,
        });
    };
    let result = with_catalog(CatalogKind::Full, tool.call(ctx, input)).await;
    let parts: Vec<String> = result
        .content
        .into_iter()
        .map(|Content::Text { text }| text)
        .collect();
    Ok(ToolOutcome {
        text: parts.join("\n"),
        is_error: result.is_error,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{Echo, context, registry_with};

    #[tokio::test]
    async fn 레지스트리가_연결되지_않았으면_호출이_실패한다() {
        let ctx = context();
        let error = call_mcp_tool(&ctx, "echo", json!({})).await.unwrap_err();
        assert_eq!(error, "the tool registry is not connected to workflows yet");
        assert!(list_mcp_tools(&ctx).is_err());
    }

    #[tokio::test]
    async fn 모르는_도구는_sdk와_같은_문구의_에러_결과가_된다() {
        let ctx = context();
        registry_with(&ctx, |registry| registry.register(Echo("echo")));
        let outcome = call_mcp_tool(&ctx, "nope", json!({})).await.unwrap();
        assert_eq!(outcome.text, "MCP error -32602: Tool nope not found");
        assert!(outcome.is_error);
    }

    #[tokio::test]
    async fn 전체_카탈로그에_없는_tool_profile_set은_목록에도_호출에도_없다() {
        let ctx = context();
        registry_with(&ctx, |registry| {
            registry.register(Echo("echo"));
            registry.register(Echo("tool_profile_set"));
        });
        assert_eq!(list_mcp_tools(&ctx).unwrap(), ["echo"]);
        let outcome = call_mcp_tool(&ctx, "tool_profile_set", json!({}))
            .await
            .unwrap();
        assert_eq!(
            outcome.text,
            "MCP error -32602: Tool tool_profile_set not found"
        );
    }

    #[tokio::test]
    async fn 도구_결과의_텍스트와_에러_여부를_돌려준다() {
        let ctx = context();
        registry_with(&ctx, |registry| registry.register(Echo("echo")));
        let outcome = call_mcp_tool(&ctx, "echo", json!({ "a": 1 }))
            .await
            .unwrap();
        assert_eq!(outcome.text, r#"echo:{"a":1}"#);
        assert!(!outcome.is_error);
        let outcome = call_mcp_tool(&ctx, "echo", json!({ "fail": true }))
            .await
            .unwrap();
        assert!(outcome.is_error);
    }

    #[tokio::test]
    async fn 표시가_없는_호출은_follow이고_워크플로의_호출은_full이다() {
        assert_eq!(call_catalog(), CatalogKind::Follow);
        let seen = with_catalog(CatalogKind::Direct, async { call_catalog() }).await;
        assert_eq!(seen, CatalogKind::Direct);

        let ctx = context();
        registry_with(&ctx, |registry| registry.register(Echo("catalog")));
        let outcome = call_mcp_tool(&ctx, "catalog", json!({})).await.unwrap();
        assert_eq!(outcome.text, "catalog:Full");
    }
}
