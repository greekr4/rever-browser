//! MCP tools for Red Team scope, findings and scans

use rever_tools_api::tool::{ToolContext, ToolRegistry};
use serde::Serialize;
use serde_json::{Value, json};

mod args;

pub mod findings;
pub mod scan;
pub mod scope;

/// Registers every tool of this crate, in the order `src/main/mcp/server.ts` does.
pub fn register(registry: &mut ToolRegistry) {
    findings::register(registry);
    scan::register(registry);
    scope::register(registry);
}

/// `getActiveTarget()?.wc.getURL()`: the URL of the tab the agent is driving,
/// or `None` when there is no such tab.
pub(crate) async fn active_url(ctx: &ToolContext) -> Option<String> {
    ctx.browser.page().await.ok().map(|page| page.url)
}

/// `JSON.stringify(value, null, 2)`.
pub(crate) fn pretty(value: &impl Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// The schema the MCP SDK lists for a tool registered without `inputSchema`.
pub(crate) fn no_args() -> Value {
    json!({ "type": "object", "properties": {} })
}

/// The schema the MCP SDK derives from a zod shape.
pub(crate) fn schema(properties: Value, required: &[&str]) -> Value {
    let mut schema = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": properties,
    });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use rever_tools_api::tool::{BoxFuture, Browser, PageInfo, Tool, ToolContext, unsupported};
    use rever_tools_api::utils::{Content, ToolResult};
    use serde_json::Value;

    /// A browser whose only tab is at the given URL; `None` is "no tab".
    pub struct Page(Mutex<Option<&'static str>>);

    impl Page {
        pub fn new(url: Option<&'static str>) -> Self {
            Self(Mutex::new(url))
        }

        pub fn go(&self, url: Option<&'static str>) {
            *self.0.lock().unwrap() = url;
        }
    }

    impl Browser for Page {
        fn page(&self) -> BoxFuture<'_, Result<PageInfo, String>> {
            Box::pin(async move {
                let url = self
                    .0
                    .lock()
                    .unwrap()
                    .ok_or_else(|| unsupported("page info"))?;
                Ok(PageInfo {
                    url: url.to_owned(),
                    title: String::new(),
                })
            })
        }
    }

    /// A data folder of its own for one test, removed afterwards.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("rever-redteam-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }

        pub fn ctx(&self, url: Option<&'static str>) -> ToolContext {
            self.ctx_on(Arc::new(Page::new(url)))
        }

        /// A context on a tab the test keeps a handle to, to navigate it later.
        pub fn ctx_on(&self, page: Arc<Page>) -> ToolContext {
            ToolContext::new(Arc::default(), page, self.0.clone())
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub fn text_of(result: &ToolResult) -> &str {
        match result.content.first() {
            Some(Content::Text { text }) => text,
            None => "",
        }
    }

    /// Calls a tool and returns (is_error, text).
    pub async fn call(tool: &impl Tool, ctx: &ToolContext, args: Value) -> (bool, String) {
        let result = tool.call(ctx, args).await;
        (result.is_error, text_of(&result).to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 모든_도구를_서버와_같은_순서로_등록한다() {
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        let names: Vec<String> = registry.defs().into_iter().map(|def| def.name).collect();
        assert_eq!(
            names,
            [
                "finding_add",
                "finding_list",
                "finding_export",
                "finding_remove",
                "redteam_start",
                "scope_status",
                "scope_set",
                "scope_add",
                "scope_clear",
                "lab_target",
            ]
        );
    }

    #[test]
    fn 이름과_설명과_스키마는_타입스크립트가_등록한_것과_같다() {
        // The fixture is what the TypeScript registered: the description as
        // written, the schema as the MCP SDK derives it from the zod shape.
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        let defs: Vec<Value> = registry
            .defs()
            .into_iter()
            .map(|def| {
                json!({
                    "name": def.name,
                    "description": def.description,
                    "inputSchema": def.input_schema,
                })
            })
            .collect();
        assert_eq!(pretty(&defs), include_str!("../fixtures/tool_defs.json"));
    }

    #[test]
    fn 모든_입력_스키마는_객체이고_required는_정의된_속성만_가리킨다() {
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        for def in registry.defs() {
            let schema = &def.input_schema;
            assert_eq!(schema["type"], "object", "{}", def.name);
            assert!(!def.description.is_empty(), "{}", def.name);
            let properties = schema["properties"].as_object();
            assert!(properties.is_some(), "{}", def.name);
            for name in schema["required"].as_array().into_iter().flatten() {
                let known = name
                    .as_str()
                    .is_some_and(|name| properties.is_some_and(|p| p.contains_key(name)));
                assert!(known, "{}: {name}", def.name);
            }
        }
    }
}
