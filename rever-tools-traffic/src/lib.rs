//! MCP tools over captured traffic: requests, bodies, HAR, diff, WebSocket, console, secrets

use std::sync::{Mutex, MutexGuard, PoisonError};

use rever_core::traffic::TrafficStore;
use rever_tools_api::tool::{ToolContext, ToolRegistry};
use rever_tools_api::utils::{ToolResult, ok};
use serde::Serialize;

/// Declares a tool: its unit struct, its definition, and a `call` that validates
/// the arguments object and turns an `Err(String)` into the tool's error result.
/// The body is an expression of type `Result<ToolResult, String>` and may await.
macro_rules! tool {
    ($name:ident, $def:expr, |$ctx:ident, $args:ident| $body:expr) => {
        pub struct $name;

        impl rever_tools_api::tool::Tool for $name {
            fn def(&self) -> rever_tools_api::tool::ToolDef {
                $def
            }

            fn call<'a>(
                &'a self,
                $ctx: &'a rever_tools_api::tool::ToolContext,
                args: serde_json::Value,
            ) -> rever_tools_api::tool::BoxFuture<'a, rever_tools_api::utils::ToolResult> {
                Box::pin(async move {
                    let run = async {
                        let $args = $crate::args::Args::new(&args)?;
                        $body
                    };
                    let result: Result<rever_tools_api::utils::ToolResult, String> = run.await;
                    result.unwrap_or_else(rever_tools_api::utils::err)
                })
            }
        }
    };
}

mod args;
mod js;

pub mod body;
pub mod console;
pub mod diff;
pub mod har;
pub mod secrets;
pub mod traffic;
pub mod websocket;

/// Registers every tool of this crate, in the order the TypeScript server did.
pub fn register(registry: &mut ToolRegistry) {
    registry.register(traffic::ListRequests);
    registry.register(traffic::ListEndpoints);
    registry.register(traffic::GetRequest);
    registry.register(traffic::GetRequestInitiator);
    registry.register(body::FetchBody);
    registry.register(har::HarExport);
    registry.register(har::ImportHar);
    registry.register(diff::RequestDiff);
    registry.register(diff::FindApiBase);
    registry.register(websocket::ListWebsockets);
    registry.register(websocket::GetWsFrames);
    registry.register(websocket::WsCaptureEnable);
    registry.register(websocket::WsCaptureList);
    registry.register(websocket::WsSend);
    registry.register(console::ConsoleEval);
    registry.register(console::ConsoleLogs);
    registry.register(console::ConsoleExceptions);
    registry.register(console::ConsoleClear);
    registry.register(console::ConsoleGetProps);
    registry.register(secrets::ScanSecrets);
}

// A tool that panicked while holding a lock must not take the others down with it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn store(ctx: &ToolContext) -> MutexGuard<'_, TrafficStore> {
    lock(&ctx.traffic)
}

/// `ok(JSON.stringify(value, null, 2))`.
fn ok_pretty(value: &impl Serialize) -> Result<ToolResult, String> {
    serde_json::to_string_pretty(value)
        .map(ok)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    use rever_core::traffic::RequestPatch;
    use rever_tools_api::tool::{BoxFuture, Browser, NoBrowser, PageInfo, Tool, ToolContext};
    use rever_tools_api::utils::Content;
    use serde_json::Value;

    use super::lock;

    /// A browser that answers from canned values and records what it was asked.
    #[derive(Default)]
    pub(crate) struct FakeBrowser {
        pub(crate) page_url: Option<String>,
        /// What `evaluate` returns; `None` reports the engine default (unsupported).
        pub(crate) evaluate: Option<Result<Value, String>>,
        /// "evaluate:<expression>", "add:<source>", "remove:<id>", in call order.
        pub(crate) calls: Mutex<Vec<String>>,
    }

    impl FakeBrowser {
        pub(crate) fn calls(&self) -> Vec<String> {
            lock(&self.calls).clone()
        }
    }

    impl Browser for FakeBrowser {
        fn page(&self) -> BoxFuture<'_, Result<PageInfo, String>> {
            Box::pin(async {
                let url = self.page_url.clone().ok_or("no page")?;
                Ok(PageInfo {
                    url,
                    title: String::new(),
                })
            })
        }

        fn evaluate<'a>(&'a self, expression: &'a str) -> BoxFuture<'a, Result<Value, String>> {
            Box::pin(async move {
                lock(&self.calls).push(format!("evaluate:{expression}"));
                match &self.evaluate {
                    Some(result) => result.clone(),
                    None => NoBrowser.evaluate(expression).await,
                }
            })
        }

        fn add_init_script<'a>(&'a self, source: &'a str) -> BoxFuture<'a, Result<String, String>> {
            Box::pin(async move {
                let mut calls = lock(&self.calls);
                calls.push(format!("add:{source}"));
                Ok(format!("script-{}", calls.len()))
            })
        }

        fn remove_init_script<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                lock(&self.calls).push(format!("remove:{id}"));
                Ok(())
            })
        }
    }

    pub(crate) fn ctx_with(browser: Arc<FakeBrowser>) -> ToolContext {
        ToolContext::new(Arc::default(), browser, std::env::temp_dir())
    }

    pub(crate) fn request(id: &str, url: &str, method: &str, kind: &str) -> RequestPatch {
        RequestPatch {
            request_id: id.to_owned(),
            url: Some(url.to_owned()),
            method: Some(method.to_owned()),
            resource_type: Some(kind.to_owned()),
            ..RequestPatch::default()
        }
    }

    /// Stores a request started at `at` (epoch ms).
    pub(crate) fn put(ctx: &ToolContext, patch: RequestPatch, at: u64) {
        lock(&ctx.traffic).upsert(patch, at);
    }

    /// Calls a tool; the result is `Ok(text)` or, for an error result, `Err(text)`.
    pub(crate) async fn call(
        tool: &dyn Tool,
        ctx: &ToolContext,
        args: Value,
    ) -> Result<String, String> {
        let result = tool.call(ctx, args).await;
        let text = match result.content.first() {
            Some(Content::Text { text }) => text.clone(),
            None => String::new(),
        };
        if result.is_error { Err(text) } else { Ok(text) }
    }

    /// Calls a tool that must succeed with JSON output.
    pub(crate) async fn call_json(tool: &dyn Tool, ctx: &ToolContext, args: Value) -> Value {
        let text = call(tool, ctx, args).await.expect("tool failed");
        serde_json::from_str(&text).expect("tool output is not JSON")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use rever_tools_api::tool::ToolRegistry;

    use super::register;

    #[test]
    fn 스무_개의_도구를_겹치지_않는_이름으로_등록한다() {
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        let defs = registry.defs();
        assert_eq!(defs.len(), 20);

        let names: HashSet<&str> = defs.iter().map(|def| def.name.as_str()).collect();
        assert_eq!(names.len(), 20);
        for name in [
            "list_requests",
            "fetch_body",
            "har_export",
            "ws_send",
            "scan_secrets",
        ] {
            assert!(names.contains(name), "{name}");
        }
        for def in &defs {
            assert!(!def.description.is_empty(), "{}", def.name);
            assert_eq!(
                def.input_schema.get("type").and_then(|kind| kind.as_str()),
                Some("object"),
                "{}",
                def.name
            );
            assert!(
                def.input_schema
                    .get("properties")
                    .is_some_and(|p| p.is_object())
            );
        }
    }
}
