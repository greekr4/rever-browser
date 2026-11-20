//! Port of `src/main/mcp/tools/macros.ts` — the tools that list, edit and replay
//! the macros saved in the Workflows panel.

use rever_tool_profile::macro_guard::{
    CliEffect, MacroStep, macro_run_needs_guard, screen_macro_run,
};
use rever_tools_api::tool::{ToolContext, ToolDef, ToolRegistry};
use serde_json::{Value, json};

use crate::args::text;
use crate::bridge::{call_catalog, list_mcp_tools};
use crate::store::ask;
use crate::workflow_executor::{RunOptions, RunStep, StepStatus, run_workflow};
use crate::{pretty, schema};

fn step_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "tool": { "type": "string", "description": "MCP tool name, e.g. \"browser_click\"" },
            "input": {
                "description": "Tool arguments. String values may contain {{var}} placeholders.",
                "type": "object",
                "propertyNames": { "type": "string" },
                "additionalProperties": {},
            },
            "waitFor": {
                "description": "CSS selector to wait for before this step runs",
                "type": "string",
            },
            "delay": {
                "description": "Extra pause (ms) before this step runs — useful for pacing a demo",
                "type": "number",
            },
            "waitTimeout": { "description": "Max ms to wait for waitFor", "type": "number" },
            "saveAs": {
                "description": "Store this step result in the run vars under this name",
                "type": "string",
            },
        },
        "required": ["tool"],
    })
}

fn macro_id() -> Value {
    json!({ "type": "string", "description": "Macro id from list_macros" })
}

/// Reject unknown tool names up front — a bad name only fails at replay time.
fn unknown_tools(ctx: &ToolContext, steps: Option<&Value>) -> Result<(), String> {
    let Some(steps) = steps.and_then(Value::as_array) else {
        return Ok(());
    };
    let known = list_mcp_tools(ctx)?;
    let mut bad: Vec<&str> = Vec::new();
    for name in steps.iter().filter_map(|step| text(step, "tool")) {
        if !known.iter().any(|tool| tool == name) && !bad.contains(&name) {
            bad.push(name);
        }
    }
    if bad.is_empty() {
        Ok(())
    } else {
        Err(format!("Unknown tool(s): {}", bad.join(", ")))
    }
}

/// The arguments a tool passes on, as the object the TypeScript built from its
/// destructured arguments: an argument that was not given is left out.
fn pick(args: &Value, keys: &[&str]) -> Value {
    let given = keys
        .iter()
        .filter_map(|key| Some(((*key).to_owned(), args.get(key)?.clone())));
    Value::Object(given.collect())
}

fn run_steps(resolved: &Value) -> Result<Vec<RunStep>, String> {
    serde_json::from_value(resolved["steps"].clone()).map_err(|e| e.to_string())
}

fn guard_steps(steps: &[RunStep]) -> Vec<MacroStep> {
    steps
        .iter()
        .map(|step| MacroStep {
            tool: step.tool.clone(),
            input: Some(step.input.clone()),
        })
        .collect()
}

tool!(
    ListMacros,
    ToolDef::new(
        "list_macros",
        "List the macros saved in the Workflows panel (id, name, description, step count).",
        schema(json!({}), &[]),
    ),
    |ctx, _args| { Ok(pretty(&ask(ctx, "macro:list", json!({}))?)) }
);

tool!(
    GetMacro,
    ToolDef::new(
        "get_macro",
        "Read one saved macro in full, including every step and its variables.",
        schema(json!({ "id": macro_id() }), &["id"]),
    ),
    |ctx, args| { Ok(pretty(&ask(ctx, "macro:get", pick(args, &["id"]))?)) }
);

tool!(
    CreateMacro,
    ToolDef::new(
        "create_macro",
        "Save a new macro, which is what the user calls a workflow (it appears in the Workflows panel): a named sequence of MCP tool calls replayed deterministically with no LLM in the loop. Use this to turn actions you just performed into a reusable macro — list the same tool calls, in order, with the arguments you used.",
        schema(
            json!({
                "name": { "type": "string", "description": "Short name shown in the Workflows panel" },
                "description": { "description": "What this macro does", "type": "string" },
                "steps": {
                    "type": "array",
                    "items": step_schema(),
                    "description": "Tool calls in execution order",
                },
                "vars": {
                    "description": "JSON object (as text) supplying values for {{var}} placeholders",
                    "type": "string",
                },
            }),
            &["name", "steps"],
        ),
    ),
    |ctx, args| {
        unknown_tools(ctx, args.get("steps"))?;
        let payload = pick(args, &["name", "description", "steps", "vars"]);
        Ok(pretty(&ask(ctx, "macro:create", payload)?))
    }
);

tool!(
    UpdateMacro,
    ToolDef::new(
        "update_macro",
        "Overwrite fields of a saved macro. Passing `steps` replaces the whole list — call get_macro first, then send the edited sequence. To drop steps by position, prefer remove_macro_steps.",
        schema(
            json!({
                "id": macro_id(),
                "name": { "type": "string" },
                "description": { "type": "string" },
                "steps": {
                    "description": "Replaces every existing step",
                    "type": "array",
                    "items": step_schema(),
                },
                "vars": {
                    "description": "JSON object (as text) for {{var}} placeholders",
                    "type": "string",
                },
            }),
            &["id"],
        ),
    ),
    |ctx, args| {
        unknown_tools(ctx, args.get("steps"))?;
        let payload = pick(args, &["id", "name", "description", "steps", "vars"]);
        Ok(pretty(&ask(ctx, "macro:update", payload)?))
    }
);

tool!(
    RemoveMacroSteps,
    ToolDef::new(
        "remove_macro_steps",
        "Delete steps from a saved macro by their zero-based position, keeping the rest. Use for \"drop this action from the macro\". Returns the macro as it now stands.",
        schema(
            json!({
                "id": macro_id(),
                "indexes": {
                    "type": "array",
                    "items": { "type": "integer", "minimum": 0, "maximum": 9_007_199_254_740_991_i64 },
                    "description": "Zero-based step positions to delete, as shown by get_macro",
                },
            }),
            &["id", "indexes"],
        ),
    ),
    |ctx, args| {
        Ok(pretty(&ask(
            ctx,
            "macro:remove-steps",
            pick(args, &["id", "indexes"]),
        )?))
    }
);

tool!(
    RunMacro,
    ToolDef::new(
        "run_macro",
        "Replay a saved macro on the active tab: its tool calls run in order and stop at the first failure. Returns each step's status and output. Use when the user asks to run a macro by name — call list_macros first to find its id.",
        schema(
            json!({
                "id": macro_id(),
                "vars": {
                    "description": "JSON object (as text) overriding the macro's saved {{var}} values for this run",
                    "type": "string",
                },
            }),
            &["id"],
        ),
    ),
    |ctx, args| {
        let id = text(args, "id").unwrap_or("");
        let resolved = ask(ctx, "macro:resolve", pick(args, &["id", "vars"]))?;
        let steps = run_steps(&resolved)?;
        if macro_run_needs_guard(call_catalog()) {
            let mut resolve = |inner: &str| -> Result<Vec<MacroStep>, String> {
                let inner = ask(ctx, "macro:resolve", json!({ "id": inner }))?;
                Ok(guard_steps(&run_steps(&inner)?))
            };
            // A missing CLI must not let the nested call run: no answer counts
            // as not a read.
            let effect = |id: &str, command: &str| {
                let effect = ask(ctx, "cli:effect", json!({ "id": id, "command": command }));
                serde_json::from_value::<CliEffect>(effect.ok()?).ok()
            };
            let stack = [id.to_owned()];
            screen_macro_run(&guard_steps(&steps), &mut resolve, &stack, Some(&effect))??;
        }
        let results = run_workflow(ctx, &steps, &mut |_| {}, &RunOptions::default()).await;
        let failed = results
            .iter()
            .any(|result| result.status == StepStatus::Error);
        let body = pretty(&json!({ "macro": resolved["name"], "steps": results }));
        if failed { Err(body) } else { Ok(body) }
    }
);

tool!(
    DeleteMacro,
    ToolDef::new(
        "delete_macro",
        "Delete a saved macro entirely.",
        schema(json!({ "id": macro_id() }), &["id"]),
    ),
    |ctx, args| { Ok(pretty(&ask(ctx, "macro:delete", pick(args, &["id"]))?)) }
);

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ListMacros);
    registry.register(GetMacro);
    registry.register(CreateMacro);
    registry.register(UpdateMacro);
    registry.register(RemoveMacroSteps);
    registry.register(RunMacro);
    registry.register(DeleteMacro);
}

#[cfg(test)]
mod tests {
    use rever_tool_profile::macro_guard::macro_run_refusal;
    use rever_tool_profile::tool_profile::CatalogKind;

    use super::*;
    use crate::bridge::with_catalog;
    use crate::test_support::{
        Echo, TestContext, call, call_json, capture_shop, context, registry_with,
    };

    /// A context whose registry holds this crate's tools, an echo tool, and
    /// stand-ins named like the tools the macro guard knows.
    fn with_tools() -> TestContext {
        let ctx = context();
        registry_with(&ctx, |registry| {
            crate::register(registry);
            for name in ["echo", "browser_click", "intruder_run", "replay_request"] {
                registry.register(Echo(name));
            }
        });
        ctx
    }

    async fn create(ctx: &ToolContext, name: &str, steps: Value) -> String {
        let made = call_json(&CreateMacro, ctx, json!({ "name": name, "steps": steps })).await;
        made["id"].as_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn 매크로를_만들고_읽고_고치고_지운다() {
        let ctx = with_tools();
        assert_eq!(
            call(&ListMacros, &ctx, Value::Null).await,
            Ok("[]".to_owned())
        );

        let text = call(
            &CreateMacro,
            &ctx,
            json!({ "name": "Greet", "steps": [{ "tool": "echo", "input": { "hi": 1 }, "delay": 5 }] }),
        )
        .await
        .unwrap();
        let made: Value = serde_json::from_str(&text).unwrap();
        let id = made["id"].as_str().unwrap();
        // The reply is JSON.stringify(detail, null, 2).
        assert_eq!(
            text,
            format!(
                "{{\n  \"id\": \"{id}\",\n  \"name\": \"Greet\",\n  \"description\": \"\",\n  \"stepCount\": 1,\n  \"updatedAt\": {},\n  \"steps\": [\n    {{\n      \"tool\": \"echo\",\n      \"input\": {{\n        \"hi\": 1\n      }},\n      \"delay\": 5\n    }}\n  ],\n  \"vars\": \"\"\n}}",
                made["updatedAt"]
            )
        );

        let listed = call_json(&ListMacros, &ctx, json!({})).await;
        assert_eq!(listed[0]["name"], "Greet");
        assert_eq!(
            call_json(&GetMacro, &ctx, json!({ "id": id })).await["steps"],
            made["steps"]
        );

        let updated = call_json(
            &UpdateMacro,
            &ctx,
            json!({ "id": id, "name": "Greet twice", "steps": [{ "tool": "echo" }, { "tool": "browser_click" }, { "tool": "echo" }] }),
        )
        .await;
        assert_eq!(updated["name"], "Greet twice");
        assert_eq!(updated["stepCount"], 3);

        let trimmed = call_json(&RemoveMacroSteps, &ctx, json!({ "id": id, "indexes": [1] })).await;
        assert_eq!(
            trimmed["steps"],
            json!([{ "tool": "echo", "input": {} }, { "tool": "echo", "input": {} }])
        );

        assert_eq!(
            call(&DeleteMacro, &ctx, json!({ "id": id })).await,
            Ok("{\n  \"deleted\": true\n}".to_owned())
        );
        assert_eq!(
            call(&GetMacro, &ctx, json!({ "id": id })).await,
            Err(format!("No macro with id \"{id}\""))
        );
    }

    #[tokio::test]
    async fn 모르는_도구가_든_스텝은_저장하기_전에_거절한다() {
        let ctx = with_tools();
        let steps =
            json!([{ "tool": "nope" }, { "tool": "echo" }, { "tool": "typo" }, { "tool": "nope" }]);
        assert_eq!(
            call(&CreateMacro, &ctx, json!({ "name": "m", "steps": steps })).await,
            Err("Unknown tool(s): nope, typo".to_owned())
        );
        // The agent pseudo-tool is not a tool the server lists either.
        assert_eq!(
            call(
                &CreateMacro,
                &ctx,
                json!({ "name": "m", "steps": [{ "tool": "__ai_prompt" }] })
            )
            .await,
            Err("Unknown tool(s): __ai_prompt".to_owned())
        );
        assert_eq!(
            call(&ListMacros, &ctx, json!({})).await,
            Ok("[]".to_owned())
        );

        let id = create(&ctx, "ok", json!([{ "tool": "echo" }])).await;
        assert_eq!(
            call(
                &UpdateMacro,
                &ctx,
                json!({ "id": id, "steps": [{ "tool": "gone" }] })
            )
            .await,
            Err("Unknown tool(s): gone".to_owned())
        );
        // Without steps there is nothing to check.
        assert!(
            call(&UpdateMacro, &ctx, json!({ "id": id, "name": "renamed" }))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn 레지스트리가_없으면_스텝을_확인할_수_없어_저장하지_않는다() {
        let ctx = context();
        assert_eq!(
            call(
                &CreateMacro,
                &ctx,
                json!({ "name": "m", "steps": [{ "tool": "echo" }] })
            )
            .await,
            Err("the tool registry is not connected to workflows yet".to_owned())
        );
        // Reading needs no registry.
        assert_eq!(
            call(&ListMacros, &ctx, json!({})).await,
            Ok("[]".to_owned())
        );
    }

    #[tokio::test]
    async fn 잘못된_인자는_실행_전에_거절한다() {
        let ctx = with_tools();
        let refusal = call(&GetMacro, &ctx, json!({ "id": 5 })).await.unwrap_err();
        assert!(refusal.starts_with(
            "MCP error -32602: Input validation error: Invalid arguments for tool get_macro: ["
        ));
        assert!(call(&RunMacro, &ctx, Value::Null).await.is_err());
    }

    #[tokio::test]
    async fn 매크로를_실행하면_스텝별_결과를_돌려준다() {
        let ctx = with_tools();
        let id = create(
            &ctx,
            "Greet",
            json!([{ "tool": "echo", "input": { "to": "{{who}}" } }, { "tool": "browser_click" }]),
        )
        .await;
        call_json(
            &UpdateMacro,
            &ctx,
            json!({ "id": id, "vars": "{\"who\":\"kim\"}" }),
        )
        .await;

        let text = call(&RunMacro, &ctx, json!({ "id": id })).await.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({
                "macro": "Greet",
                "steps": [
                    { "index": 0, "tool": "echo", "status": "done", "output": "echo:{\"to\":\"kim\"}" },
                    { "index": 1, "tool": "browser_click", "status": "done", "output": "browser_click:{}" }
                ]
            })
        );
        assert!(
            text.starts_with(
                "{\n  \"macro\": \"Greet\",\n  \"steps\": [\n    {\n      \"index\": 0,"
            )
        );

        // Variables for this run only.
        let run = call_json(
            &RunMacro,
            &ctx,
            json!({ "id": id, "vars": "{\"who\":\"lee\"}" }),
        )
        .await;
        assert_eq!(run["steps"][0]["output"], "echo:{\"to\":\"lee\"}");
    }

    #[tokio::test]
    async fn 실패한_스텝이_있으면_같은_본문을_에러로_돌려준다() {
        let ctx = with_tools();
        let id = create(
            &ctx,
            "Breaks",
            json!([{ "tool": "echo", "input": { "fail": true } }, { "tool": "echo" }]),
        )
        .await;
        let body = call(&RunMacro, &ctx, json!({ "id": id }))
            .await
            .unwrap_err();
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({
                "macro": "Breaks",
                "steps": [{ "index": 0, "tool": "echo", "status": "error", "error": "echo:{\"fail\":true}" }]
            })
        );
    }

    #[tokio::test]
    async fn 실행할_수_없는_매크로는_이유를_에러로_돌려준다() {
        let ctx = with_tools();
        assert_eq!(
            call(&RunMacro, &ctx, json!({ "id": "nope" })).await,
            Err("No macro with id \"nope\"".to_owned())
        );
        let id = create(&ctx, "Empty", json!([])).await;
        assert_eq!(
            call(&RunMacro, &ctx, json!({ "id": id })).await,
            Err("Macro \"Empty\" has no steps".to_owned())
        );
        let id = create(&ctx, "Vars", json!([{ "tool": "echo" }])).await;
        assert_eq!(
            call(&RunMacro, &ctx, json!({ "id": id, "vars": "{ nope" })).await,
            Err("Variables are not valid JSON".to_owned())
        );
    }

    #[tokio::test]
    async fn 에이전트가_부른_매크로는_위험한_스텝을_실행하지_않는다() {
        let ctx = with_tools();
        let risky = create(
            &ctx,
            "Risky",
            json!([{ "tool": "echo" }, { "tool": "intruder_run" }]),
        )
        .await;
        // A call nobody labelled is a `follow` session's: screened.
        assert_eq!(
            call(&RunMacro, &ctx, json!({ "id": risky })).await,
            Err(macro_run_refusal(1, "intruder_run"))
        );
        let direct = with_catalog(
            CatalogKind::Direct,
            call(&RunMacro, &ctx, json!({ "id": risky })),
        )
        .await;
        assert_eq!(direct, Err(macro_run_refusal(1, "intruder_run")));

        // A replay is fine as a GET and refused as anything else.
        let get = create(
            &ctx,
            "Get",
            json!([{ "tool": "replay_request", "input": { "method": "get" } }]),
        )
        .await;
        assert!(call(&RunMacro, &ctx, json!({ "id": get })).await.is_ok());
        let post = create(
            &ctx,
            "Post",
            json!([{ "tool": "replay_request", "input": { "method": "POST" } }]),
        )
        .await;
        assert_eq!(
            call(&RunMacro, &ctx, json!({ "id": post })).await,
            Err(macro_run_refusal(0, "replay_request"))
        );

        // The Workflows panel and scripts hold the full catalog: not screened.
        let full = with_catalog(
            CatalogKind::Full,
            call(&RunMacro, &ctx, json!({ "id": risky })),
        )
        .await;
        let full: Value = serde_json::from_str(&full.unwrap()).unwrap();
        assert_eq!(full["steps"][1]["output"], "intruder_run:{}");
    }

    #[tokio::test]
    async fn 중첩된_매크로도_안쪽_스텝까지_검사한다() {
        let ctx = with_tools();
        let inner = create(&ctx, "Inner", json!([{ "tool": "intruder_run" }])).await;
        let outer = create(
            &ctx,
            "Outer",
            json!([{ "tool": "echo" }, { "tool": "run_macro", "input": { "id": inner } }]),
        )
        .await;
        assert_eq!(
            call(&RunMacro, &ctx, json!({ "id": outer })).await,
            Err(macro_run_refusal(1, "intruder_run"))
        );
        // A macro that runs itself is refused rather than followed forever.
        let looping = create(&ctx, "Loop", json!([{ "tool": "echo" }])).await;
        call_json(
            &UpdateMacro,
            &ctx,
            json!({ "id": looping, "steps": [{ "tool": "run_macro", "input": { "id": looping } }] }),
        )
        .await;
        assert_eq!(
            call(&RunMacro, &ctx, json!({ "id": looping })).await,
            Err(macro_run_refusal(0, "run_macro"))
        );
        // A nested macro that cannot be resolved stops the run with its reason.
        let dangling = create(
            &ctx,
            "Dangling",
            json!([{ "tool": "run_macro", "input": { "id": "gone" } }]),
        )
        .await;
        assert_eq!(
            call(&RunMacro, &ctx, json!({ "id": dangling })).await,
            Err("No macro with id \"gone\"".to_owned())
        );
    }

    #[tokio::test]
    async fn 저장된_cli_명령은_읽기일_때만_매크로에서_실행한다() {
        let ctx = with_tools();
        capture_shop(&ctx);
        let names = json!({ "r1": "search", "r2": "create" });
        let cli = call_json(
            &crate::cli::CliCompile,
            &ctx,
            json!({ "requestIds": ["r1", "r2"], "names": names }),
        )
        .await;
        let cli = cli["id"].as_str().unwrap();
        crate::test_support::FakeSender::body(&ctx, 200, "{}");

        let step = |command: &str| json!([{ "tool": "cli_run", "input": { "id": cli, "command": command } }]);
        let read = create(&ctx, "Read", step("search")).await;
        let run = call_json(&RunMacro, &ctx, json!({ "id": read })).await;
        assert_eq!(run["steps"][0]["status"], "done");

        for (name, steps) in [
            ("Write", step("create")),
            ("Missing", step("nope")),
            (
                "NoCli",
                json!([{ "tool": "cli_run", "input": { "id": "gone", "command": "search" } }]),
            ),
        ] {
            let id = create(&ctx, name, steps).await;
            assert_eq!(
                call(&RunMacro, &ctx, json!({ "id": id })).await,
                Err(macro_run_refusal(0, "cli_run")),
                "{name}"
            );
        }
        let writes = create(
            &ctx,
            "Writes",
            json!([{ "tool": "cli_writes", "input": { "id": cli, "enabled": true } }]),
        )
        .await;
        assert_eq!(
            call(&RunMacro, &ctx, json!({ "id": writes })).await,
            Err(macro_run_refusal(0, "cli_writes"))
        );
    }

    #[tokio::test]
    async fn 중첩_실행은_바깥_실행을_밀어내_그_뒤_스텝이_돌지_않는다() {
        // As in the TypeScript: there is one active run, and the inner run_macro
        // supersedes the outer one, which then stops after the step that nested.
        let ctx = with_tools();
        let inner = create(
            &ctx,
            "Inner",
            json!([{ "tool": "echo", "input": { "inner": 1 } }]),
        )
        .await;
        let outer = create(
            &ctx,
            "Outer",
            json!([{ "tool": "run_macro", "input": { "id": inner } }, { "tool": "echo", "input": { "after": 1 } }]),
        )
        .await;
        let run = call_json(&RunMacro, &ctx, json!({ "id": outer })).await;
        let steps = run["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0]["status"], "done");
        let nested: Value = serde_json::from_str(steps[0]["output"].as_str().unwrap()).unwrap();
        assert_eq!(nested["macro"], "Inner");
        assert_eq!(nested["steps"][0]["output"], "echo:{\"inner\":1}");
    }
}
