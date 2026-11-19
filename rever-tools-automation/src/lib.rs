//! MCP tools for macros, saved CLI commands and workflows.
//!
//! What the TypeScript reached through imports, the app hands over through the
//! tool context once everything is registered:
//! - `ctx.state::<bridge::ToolRegistryHandle>().set(registry)` — the tools a
//!   macro or pipeline step may call. Without it, running a workflow and saving
//!   macro steps fail with a clear message.
//! - `ctx.state::<cli_exec::SenderSlot>().set(repeater)` — how `cli_run` sends a
//!   request. The default sends from this process, without the tab's cookies.
//! - `ctx.state::<workflow_executor::AiAgentsSlot>().set(agents)` — the agent
//!   behind an `__ai_prompt` step. Without it such a step fails.
//! - `bridge::with_catalog(catalog, tool.call(…))` around a call from an MCP
//!   session — its catalog. A call without it is screened by the macro guard.

use std::sync::{Mutex, MutexGuard};

use rever_tools_api::tool::ToolRegistry;
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Value, json};

/// Declares a unit-struct tool: its definition, and a body that yields the result
/// text as `Result<String, String>` (`Err` becomes an `isError` result). The body
/// runs in an async block, so `?` works in it; `$args` is the arguments object,
/// already checked against the tool's schema.
macro_rules! tool {
    ($name:ident, $def:expr, |$ctx:ident, $args:ident| $body:block) => {
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
                    let $args = &args;
                    let result: Result<String, String> = async {
                        $crate::args::validate(&self.def(), $args)?;
                        $body
                    }
                    .await;
                    $crate::done(result)
                })
            }
        }
    };
}

mod args;
pub mod bridge;
pub mod cli;
pub mod cli_exec;
mod js;
pub mod macros;
pub mod store;
pub mod workflow_executor;

/// Registers every tool of this crate, in the order `buildMcpServer` does.
pub fn register(registry: &mut ToolRegistry) {
    macros::register(registry);
    cli::register(registry);
}

pub(crate) fn done(result: Result<String, String>) -> ToolResult {
    match result {
        Ok(text) => ok(text),
        Err(text) => err(text),
    }
}

/// The schema the MCP SDK derives from a zod shape.
pub(crate) fn schema(properties: Value, required: &[&str]) -> Value {
    let mut schema = json!({ "type": "object", "properties": properties });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema["$schema"] = json!("http://json-schema.org/draft-07/schema#");
    schema
}

/// `JSON.stringify(value, null, 2)`.
pub(crate) fn pretty(value: &impl serde::Serialize) -> String {
    js::json_stringify(&serde_json::to_value(value).unwrap_or(Value::Null), true)
}

/// A panic in another call must not take the shared state down with it.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::VecDeque;
    use std::ops::Deref;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use rever_tools_api::tool::{
        BoxFuture, Browser, NoBrowser, Tool, ToolContext, ToolDef, ToolRegistry,
    };
    use rever_tools_api::utils::{Content, ToolResult, err, ok};
    use serde_json::{Value, json};

    use rever_core::traffic::{Headers, RequestPatch};

    use crate::bridge::{ToolRegistryHandle, call_catalog};
    use crate::cli_exec::{RequestSender, SendRequest, SendResponse, SenderSlot};
    use crate::js::now_ms;
    use crate::lock;

    /// A context with its own data folder, removed again when the test ends.
    pub(crate) struct TestContext(ToolContext);

    impl Deref for TestContext {
        type Target = ToolContext;

        fn deref(&self) -> &ToolContext {
            &self.0
        }
    }

    impl Drop for TestContext {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0.data_dir);
        }
    }

    pub(crate) fn context_on(browser: Arc<dyn Browser>) -> TestContext {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let data_dir = std::env::temp_dir().join(format!(
            "rever-tools-automation-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        TestContext(ToolContext::new(Arc::default(), browser, data_dir))
    }

    pub(crate) fn context() -> TestContext {
        context_on(Arc::new(NoBrowser))
    }

    /// Connects a registry holding what `fill` registers.
    pub(crate) fn registry_with(ctx: &ToolContext, fill: impl FnOnce(&mut ToolRegistry)) {
        let mut registry = ToolRegistry::default();
        fill(&mut registry);
        ctx.state::<ToolRegistryHandle>().set(Arc::new(registry));
    }

    /// Answers `name:args`. `{"fail": true}` makes it an error result, and the
    /// tool named `catalog` answers with the catalog its call came through.
    pub(crate) struct Echo(pub(crate) &'static str);

    impl Tool for Echo {
        fn def(&self) -> ToolDef {
            ToolDef::new(self.0, "echoes its arguments", json!({ "type": "object" }))
        }

        fn call<'a>(&'a self, _: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
            Box::pin(async move {
                if self.0 == "catalog" {
                    return ok(format!("catalog:{:?}", call_catalog()));
                }
                let text = format!("{}:{args}", self.0);
                if args["fail"] == true {
                    err(text)
                } else {
                    ok(text)
                }
            })
        }
    }

    /// The result text: `Ok` for a success, `Err` for an `isError` result.
    pub(crate) async fn call(
        tool: &dyn Tool,
        ctx: &ToolContext,
        args: Value,
    ) -> Result<String, String> {
        let result = tool.call(ctx, args).await;
        let text = match result.content.into_iter().next() {
            Some(Content::Text { text }) => text,
            None => String::new(),
        };
        if result.is_error { Err(text) } else { Ok(text) }
    }

    /// A successful result parsed as JSON.
    pub(crate) async fn call_json(tool: &dyn Tool, ctx: &ToolContext, args: Value) -> Value {
        let text = call(tool, ctx, args).await.expect("tool failed");
        serde_json::from_str(&text).expect("result is not JSON")
    }

    /// Puts one captured request in the traffic store: a JSON `Fetch` answered 200
    /// unless `edit` says otherwise.
    pub(crate) fn capture(
        ctx: &ToolContext,
        id: &str,
        method: &str,
        url: &str,
        edit: impl FnOnce(&mut RequestPatch),
    ) {
        let mut patch = RequestPatch {
            request_id: id.to_owned(),
            url: Some(url.to_owned()),
            method: Some(method.to_owned()),
            resource_type: Some("Fetch".to_owned()),
            status: Some(200),
            mime_type: Some("application/json".to_owned()),
            ..RequestPatch::default()
        };
        edit(&mut patch);
        lock(&ctx.traffic).upsert(patch, now_ms());
    }

    pub(crate) fn headers(pairs: &[(&str, &str)]) -> Option<Headers> {
        let pairs = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()));
        Some(pairs.collect())
    }

    /// Four captured calls of one shop: a search (`r1`), a JSON create (`r2`), an
    /// item by id (`r3`) and an HTML page (`r4`).
    pub(crate) fn capture_shop(ctx: &ToolContext) {
        capture(
            ctx,
            "r1",
            "GET",
            "https://shop.test/api/items?q=shoes",
            |patch| {
                patch.request_headers = headers(&[("X-CSRF-Token", "tok1"), ("accept", "*/*")]);
                patch.response_body = Some(r#"{"ok":true}"#.to_owned());
            },
        );
        capture(ctx, "r2", "POST", "https://shop.test/api/items", |patch| {
            patch.request_headers = headers(&[
                ("content-type", "application/json"),
                ("x-csrf-token", "tok2"),
            ]);
            patch.request_post_data = Some(r#"{"title":"Shoe"}"#.to_owned());
            patch.status = Some(201);
        });
        capture(ctx, "r3", "GET", "https://shop.test/api/items/123", |_| {});
        capture(ctx, "r4", "GET", "https://shop.test/list", |patch| {
            patch.resource_type = Some("XHR".to_owned());
            patch.mime_type = Some("text/html".to_owned());
        });
    }

    /// A sender that records what it is asked to send and gives one fixed answer.
    pub(crate) struct FakeSender {
        pub(crate) sent: Mutex<Vec<SendRequest>>,
        pub(crate) answer: Result<SendResponse, String>,
    }

    impl FakeSender {
        /// Plugs the sender into the context and returns it.
        pub(crate) fn answering(
            ctx: &ToolContext,
            answer: Result<SendResponse, String>,
        ) -> Arc<Self> {
            let sender = Arc::new(Self {
                sent: Mutex::default(),
                answer,
            });
            ctx.state::<SenderSlot>().set(sender.clone());
            sender
        }

        pub(crate) fn body(ctx: &ToolContext, status: u16, body: &str) -> Arc<Self> {
            let response = SendResponse {
                status,
                body: body.to_owned(),
                ..SendResponse::default()
            };
            Self::answering(ctx, Ok(response))
        }
    }

    impl RequestSender for FakeSender {
        fn send<'a>(
            &'a self,
            _ctx: &'a ToolContext,
            request: &'a SendRequest,
        ) -> BoxFuture<'a, Result<SendResponse, String>> {
            Box::pin(async move {
                lock(&self.sent).push(request.clone());
                self.answer.clone()
            })
        }
    }

    /// A page whose `evaluate` gives the queued answers in turn, then `fallback`.
    pub(crate) struct FakeBrowser {
        pub(crate) answers: Mutex<VecDeque<Result<Value, String>>>,
        pub(crate) fallback: Result<Value, String>,
        pub(crate) evaluated: Mutex<Vec<String>>,
    }

    impl FakeBrowser {
        pub(crate) fn new(fallback: Result<Value, String>) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::default(),
                fallback,
                evaluated: Mutex::default(),
            })
        }

        pub(crate) fn then(self: Arc<Self>, answer: Result<Value, String>) -> Arc<Self> {
            lock(&self.answers).push_back(answer);
            self
        }
    }

    impl Browser for FakeBrowser {
        fn evaluate<'a>(&'a self, expression: &'a str) -> BoxFuture<'a, Result<Value, String>> {
            Box::pin(async move {
                lock(&self.evaluated).push(expression.to_owned());
                let queued = lock(&self.answers).pop_front();
                queued.unwrap_or_else(|| self.fallback.clone())
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 도구_정의는_타입스크립트가_등록하는_정의와_같다() {
        // The fixture is what macros.ts and cli.ts register, dumped with bun
        // through the MCP SDK's own zod → JSON Schema conversion (tools/list).
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("../fixtures/ts-tool-defs.json")).unwrap();
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        let defs = registry.defs();
        let names: Vec<&str> = defs.iter().map(|def| def.name.as_str()).collect();
        let expected_names: Vec<&str> = expected
            .iter()
            .map(|want| want["name"].as_str().unwrap())
            .collect();
        // The order too: macros before the CLI tools, as the server registers them.
        assert_eq!(names, expected_names);
        assert_eq!(names.len(), 14);
        for (def, want) in defs.iter().zip(&expected) {
            assert_eq!(Value::from(def.description.as_str()), want["description"]);
            assert_eq!(def.input_schema, want["inputSchema"], "{}", def.name);
            // Key order too: a client shows the properties in this order.
            assert_eq!(
                def.input_schema.to_string(),
                want["inputSchema"].to_string(),
                "{}",
                def.name
            );
        }
    }

    #[test]
    fn pretty는_json_stringify의_두_칸_들여쓰기와_같은_모양이다() {
        assert_eq!(
            pretty(&json!({ "deleted": true })),
            "{\n  \"deleted\": true\n}"
        );
        assert_eq!(pretty(&json!([])), "[]");
    }
}
