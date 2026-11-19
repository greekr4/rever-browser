//! Port of `src/main/mcp/tools/cli.ts` — the tools that record, compile, describe
//! and run a site CLI built from captured requests.

use rever_cli_spec::cli_spec::{
    CliData, CliExtract, CompileOptions, Record, cli_run_example, describe_cli, export_curl,
    parse_extract,
};
use rever_tools_api::tool::{ToolContext, ToolDef, ToolRegistry};
use serde_json::{Map, Value, json};

use crate::args::text;
use crate::cli_exec::{ArmAction, CliRun, arm_cli, compile_captured, execute_cli};
use crate::js::js_trim;
use crate::store::ask;
use crate::{pretty, schema};

const RAW_HTML_NOTE: &str = "Response was cut off before the end of the page. Pass extract once — selector, fields ({ name, selector?, attr? }), optional limit — and later runs return those fields instead of the page.";

fn extract_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "selector": { "type": "string", "description": "CSS selector that matches each row" },
            "limit": {
                "description": "Rows to return. Default 5, maximum 20",
                "type": "integer",
                "minimum": 1,
                "maximum": 20,
            },
            "fields": {
                "minItems": 1,
                "maxItems": 6,
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Field name, such as title or link" },
                        "selector": {
                            "description": "CSS selector relative to the row. Omit to use the row itself",
                            "type": "string",
                        },
                        "attr": {
                            "description": "Attribute to read, such as href. Omit to read the text",
                            "type": "string",
                        },
                    },
                    "required": ["name"],
                },
            },
        },
        "required": ["selector", "fields"],
        "description": "Fields to read from an HTML response. Saved on the command.",
    })
}

fn cli_id() -> Value {
    json!({ "type": "string", "description": "CLI id from cli_list or cli_compile" })
}

fn take_extract(raw: &Value) -> Result<CliExtract, String> {
    parse_extract(raw)?.ok_or_else(|| "extract must be an object".to_owned())
}

/// A `z.record(z.string(), z.string())` argument.
fn string_record(value: Option<&Value>) -> Option<Record<String>> {
    let entries = value?.as_object()?.iter();
    Some(
        entries
            .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
            .collect(),
    )
}

/// The saved CLI behind `cli:get`.
fn saved_cli(ctx: &ToolContext, id: &str) -> Result<CliData, String> {
    let row = ask(ctx, "cli:get", json!({ "id": id }))?;
    serde_json::from_value(row["data"].clone()).map_err(|e| e.to_string())
}

tool!(
    CliArmTool,
    ToolDef::new(
        "cli_arm",
        "Start, stop, or list a recording window of XHR and Fetch calls on the active tab. The list is request ids only and does not save a workflow. Static files are left out. Pass the ids you mean to keep to cli_compile.",
        schema(
            json!({
                "action": {
                    "type": "string",
                    "enum": ["start", "stop", "list"],
                    "description": "start begins a window, stop ends it, list returns the calls inside it",
                },
            }),
            &["action"],
        ),
    ),
    |ctx, args| {
        let action = match text(args, "action") {
            Some("start") => ArmAction::Start,
            Some("stop") => ArmAction::Stop,
            _ => ArmAction::List,
        };
        Ok(pretty(&arm_cli(ctx, action)))
    }
);

tool!(
    CliCompile,
    ToolDef::new(
        "cli_compile",
        "Turn explicit captured request ids into one site CLI in the Workflows panel. Only those ids are included. Writes stay off. Pass extract to teach every new command which HTML fields to return. Returns the workflow id and command names, never header or body values.",
        schema(
            json!({
                "requestIds": {
                    "minItems": 1,
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "requestId values from list_requests or cli_arm",
                },
                "name": { "description": "Name shown in the Workflows panel", "type": "string" },
                "site": { "description": "Site slug. Defaults to the host", "type": "string" },
                "title": { "type": "string" },
                "baseUrl": { "description": "Origin to keep. Other origins are dropped", "type": "string" },
                "names": {
                    "description": "Command name per requestId, named for what it does, e.g. { \"123.4\": \"search\" }. Omit for names derived from the path",
                    "type": "object",
                    "propertyNames": { "type": "string" },
                    "additionalProperties": { "type": "string" },
                },
                "extract": extract_schema(),
            }),
            &["requestIds"],
        ),
    ),
    |ctx, args| {
        let ids = args.get("requestIds").and_then(Value::as_array);
        let ids: Vec<String> = ids
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let owned = |key: &str| text(args, key).map(str::to_owned);
        let opts = CompileOptions {
            site: owned("site"),
            title: owned("title"),
            base_url: owned("baseUrl"),
            limit: None,
            names: string_record(args.get("names")),
        };
        let mut data = compile_captured(ctx, &ids, &opts)?;
        if data.commands.is_empty() {
            return Err("no commands from those request ids".to_owned());
        }
        if let Some(extract) = args.get("extract") {
            let extract = take_extract(extract)?;
            for command in &mut data.commands {
                command.extract = Some(extract.clone());
            }
        }
        let name = match (text(args, "name").map(js_trim), data.commands.as_slice()) {
            (Some(name), _) if !name.is_empty() => name.to_owned(),
            (_, [only]) => only.name.clone(),
            _ => data.site.clone(),
        };
        Ok(pretty(&ask(
            ctx,
            "cli:save",
            json!({ "name": name, "data": data }),
        )?))
    }
);

tool!(
    CliList,
    ToolDef::new(
        "cli_list",
        "List saved site CLIs: id, site, command names, effects, and whether writes are on. No header values or secrets.",
        schema(json!({}), &[]),
    ),
    |ctx, _args| { Ok(pretty(&ask(ctx, "cli:list", json!({}))?)) }
);

tool!(
    CliDescribe,
    ToolDef::new(
        "cli_describe",
        "Describe one saved CLI, or one of its commands: method, path, effect, flags, session header names, saved extract, and evidence request ids. The reply ends with the cli_run object (id, command, inputs). Inputs are blank here. The Workflows panel fills them from the captured request. Cookie values are never included.",
        schema(
            json!({
                "id": cli_id(),
                "command": { "description": "One command name. Omit to describe every command", "type": "string" },
            }),
            &["id"],
        ),
    ),
    |ctx, args| {
        let id = text(args, "id").unwrap_or("");
        let command = text(args, "command").filter(|command| !command.is_empty());
        let data = saved_cli(ctx, id)?;
        let described = describe_cli(&data, command);
        let commands = data
            .commands
            .iter()
            .filter(|item| command.is_none_or(|name| item.name == name));
        let calls: Vec<String> = commands
            .map(|item| cli_run_example(id, item, &Record::new()))
            .collect();
        if calls.is_empty() {
            return Ok(described);
        }
        Ok(format!("{described}\ncli_run:\n{}\n", calls.join("\n")))
    }
);

tool!(
    CliRunTool,
    ToolDef::new(
        "cli_run",
        "Run one saved CLI command inside the logged-in tab. Cookies stay in the tab. Read commands send immediately. Write and unknown commands send only when writes are on for that CLI. A failure after a write was sent returns action_outcome_unknown and is not retried. Pass extract once to teach the command which fields to read from an HTML page. Later runs use the saved extract and return those fields instead of the page. Without extract, an HTML page comes back as a short preview.",
        schema(
            json!({
                "id": cli_id(),
                "command": { "type": "string", "description": "Command name from cli_describe" },
                "inputs": {
                    "description": "Flag values keyed by flag name. Required path flags must be present",
                    "type": "object",
                    "propertyNames": { "type": "string" },
                    "additionalProperties": { "type": "string" },
                },
                "extract": extract_schema(),
            }),
            &["id", "command"],
        ),
    ),
    |ctx, args| {
        let id = text(args, "id").unwrap_or("");
        let command = text(args, "command").unwrap_or("");
        let data = saved_cli(ctx, id)?;
        let found = data.commands.iter().find(|item| item.name == command);
        let mut found = found
            .ok_or_else(|| format!("unknown command: {command}"))?
            .clone();
        if let Some(extract) = args.get("extract") {
            let extract = take_extract(extract)?;
            let payload = json!({ "id": id, "command": command, "extract": extract });
            ask(ctx, "cli:set-extract", payload)?;
            found.extract = Some(extract);
        }
        let inputs = string_record(args.get("inputs")).unwrap_or_default();
        let run = CliRun {
            base_url: &data.base_url,
            writes: data.writes,
            command: &found,
            inputs: &inputs,
        };
        let result = execute_cli(ctx, run).await;
        if !result.ok {
            return Err(result.error.unwrap_or_else(|| "cli run failed".to_owned()));
        }
        let mut out = Map::new();
        if let Some(status) = result.status {
            out.insert("status".to_owned(), Value::Number(status));
        }
        if let Some(items) = result.items {
            let matched = result.matched.unwrap_or_else(|| items.len().into());
            out.insert("matched".to_owned(), Value::Number(matched));
            out.insert("items".to_owned(), json!(items));
            if let Some(note) = result.note.filter(|note| !note.is_empty()) {
                out.insert("note".to_owned(), json!(note));
            }
        } else {
            out.insert("body".to_owned(), json!(result.body.unwrap_or_default()));
            if result.body_cut {
                out.insert("note".to_owned(), json!(RAW_HTML_NOTE));
            }
        }
        Ok(pretty(&out))
    }
);

tool!(
    CliWrites,
    ToolDef::new(
        "cli_writes",
        "Turn writes on or off for one saved CLI. Ask the user once before turning writes on. This only flips the saved flag. It does not send a request.",
        schema(
            json!({
                "id": cli_id(),
                "enabled": { "type": "boolean", "description": "true allows write and unknown commands to send" },
            }),
            &["id", "enabled"],
        ),
    ),
    |ctx, args| {
        let payload = json!({ "id": args["id"], "writes": args["enabled"] });
        Ok(pretty(&ask(ctx, "cli:set-writes", payload)?))
    }
);

tool!(
    CliExport,
    ToolDef::new(
        "cli_export",
        "Return a curl script for a saved CLI. Flags are placeholders. Captured secrets and cookie values are not included.",
        schema(json!({ "id": cli_id() }), &["id"]),
    ),
    |ctx, args| {
        Ok(export_curl(&saved_cli(
            ctx,
            text(args, "id").unwrap_or(""),
        )?))
    }
);

pub fn register(registry: &mut ToolRegistry) {
    registry.register(CliArmTool);
    registry.register(CliCompile);
    registry.register(CliList);
    registry.register(CliDescribe);
    registry.register(CliRunTool);
    registry.register(CliWrites);
    registry.register(CliExport);
}

#[cfg(test)]
mod tests {
    use rever_cli_spec::cli_spec::{OUTCOME_UNKNOWN, WRITES_DISABLED};

    use super::*;
    use crate::cli_exec::SendResponse;
    use crate::lock;
    use crate::test_support::{
        FakeBrowser, FakeSender, TestContext, call, call_json, capture_shop, context, context_on,
    };

    /// Compiles the shop's four captured calls and returns the CLI's id.
    async fn compiled(ctx: &ToolContext) -> String {
        capture_shop(ctx);
        let names = json!({ "r1": "search", "r2": "create", "r3": "item", "r4": "page" });
        let args =
            json!({ "requestIds": ["r1", "r2", "r3", "r4"], "name": "  Shop  ", "names": names });
        let saved = call_json(&CliCompile, ctx, args).await;
        saved["id"].as_str().unwrap().to_owned()
    }

    async fn shop() -> (TestContext, String) {
        let ctx = context();
        let id = compiled(&ctx).await;
        (ctx, id)
    }

    #[tokio::test]
    async fn 녹화_창을_열고_닫으며_요청_id를_돌려준다() {
        let ctx = context();
        assert_eq!(
            call(&CliArmTool, &ctx, json!({ "action": "list" })).await,
            Ok("{\n  \"recording\": false,\n  \"startedAt\": null,\n  \"stoppedAt\": null,\n  \"requests\": []\n}".to_owned())
        );
        let started = call_json(&CliArmTool, &ctx, json!({ "action": "start" })).await;
        assert_eq!(started["recording"], true);
        assert!(started["startedAt"].is_u64());
        let stopped = call_json(&CliArmTool, &ctx, json!({ "action": "stop" })).await;
        assert_eq!(stopped["recording"], false);
        assert!(stopped["stoppedAt"].is_u64());
        assert!(
            call(&CliArmTool, &ctx, json!({ "action": "pause" }))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn 고른_요청으로_cli를_만들어_저장한다() {
        let (ctx, id) = shop().await;
        let listed = call_json(&CliList, &ctx, Value::Null).await;
        assert_eq!(
            listed,
            json!([{
                "id": id,
                // The given name, trimmed.
                "name": "Shop",
                "site": "shop-test",
                "baseUrl": "https://shop.test",
                "writes": false,
                "commands": [
                    { "name": "search", "method": "GET", "path": "/api/items", "effect": "read" },
                    { "name": "create", "method": "POST", "path": "/api/items", "effect": "write" },
                    { "name": "item", "method": "GET", "path": "/api/items/{id0}", "effect": "read" },
                    { "name": "page", "method": "GET", "path": "/list", "effect": "read" }
                ]
            }])
        );
    }

    #[tokio::test]
    async fn 이름을_안_주면_명령_하나는_그_이름을_여럿은_사이트_이름을_쓴다() {
        let ctx = context();
        capture_shop(&ctx);
        let one = call_json(
            &CliCompile,
            &ctx,
            json!({ "requestIds": ["r1"], "names": { "r1": "search" }, "name": "  " }),
        )
        .await;
        assert_eq!(one["name"], "search");
        let two = call_json(
            &CliCompile,
            &ctx,
            json!({ "requestIds": ["r1", "r2"], "site": "My Shop" }),
        )
        .await;
        assert_eq!(two["name"], two["site"]);
        assert_eq!(two["commands"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn 컴파일할_것이_없거나_추출이_틀리면_저장하지_않는다() {
        let ctx = context();
        capture_shop(&ctx);
        assert_eq!(
            call(&CliCompile, &ctx, json!({ "requestIds": ["gone"] })).await,
            Err("no commands from those request ids".to_owned())
        );
        assert_eq!(
            call(
                &CliCompile,
                &ctx,
                json!({ "requestIds": ["r1"], "baseUrl": "not a url" })
            )
            .await,
            Err("Invalid URL".to_owned())
        );
        // Passes the schema, fails the extract's own rules.
        let extract = json!({ "selector": "li", "fields": [{ "name": "1bad" }] });
        assert_eq!(
            call(
                &CliCompile,
                &ctx,
                json!({ "requestIds": ["r1"], "extract": extract })
            )
            .await,
            Err("extract field name must be a short identifier".to_owned())
        );
        assert!(
            call(&CliCompile, &ctx, json!({ "requestIds": [] }))
                .await
                .is_err()
        );
        assert_eq!(call(&CliList, &ctx, json!({})).await, Ok("[]".to_owned()));
    }

    #[tokio::test]
    async fn 컴파일할_때_준_추출은_모든_명령에_저장된다() {
        let ctx = context();
        capture_shop(&ctx);
        let extract = json!({ "selector": "li", "limit": 3, "fields": [{ "name": "title", "junk": 1 }], "junk": 2 });
        let saved = call_json(
            &CliCompile,
            &ctx,
            json!({ "requestIds": ["r1", "r4"], "extract": extract }),
        )
        .await;
        let commands = saved["commands"].as_array().unwrap();
        assert_eq!(commands.len(), 2);
        assert!(commands.iter().all(|command| command["extract"] == "title"));
    }

    #[tokio::test]
    async fn 설명은_명령과_cli_run_예시를_돌려준다() {
        let (ctx, id) = shop().await;
        let data = saved_cli(&ctx, &id).unwrap();
        let all = call(&CliDescribe, &ctx, json!({ "id": id })).await.unwrap();
        let examples: Vec<String> = data
            .commands
            .iter()
            .map(|command| cli_run_example(&id, command, &Record::new()))
            .collect();
        assert_eq!(
            all,
            format!(
                "{}\ncli_run:\n{}\n",
                describe_cli(&data, None),
                examples.join("\n")
            )
        );
        assert!(all.starts_with("shop-test  https://shop.test\nwrites: off\n\nsearch\n  GET /api/items\n  effect: read\n"));
        assert!(all.contains(&format!("cli_run:\n{{\n  \"id\": \"{id}\",\n  \"command\": \"search\",\n  \"inputs\": {{\n    \"q\": \"\"\n  }}\n}}")));
        // An empty command name is no command name.
        assert_eq!(
            call(&CliDescribe, &ctx, json!({ "id": id, "command": "" })).await,
            Ok(all)
        );

        let one = call(&CliDescribe, &ctx, json!({ "id": id, "command": "item" }))
            .await
            .unwrap();
        let item = data
            .commands
            .iter()
            .find(|command| command.name == "item")
            .unwrap();
        assert_eq!(
            one,
            format!(
                "{}\ncli_run:\n{}\n",
                describe_cli(&data, Some("item")),
                cli_run_example(&id, item, &Record::new())
            )
        );
        // An unknown command gets the description's own answer and no example.
        let unknown = call(&CliDescribe, &ctx, json!({ "id": id, "command": "nope" }))
            .await
            .unwrap();
        assert_eq!(unknown, describe_cli(&data, Some("nope")));
        assert!(
            unknown
                .ends_with("Unknown action: nope\nAvailable actions: search, create, item, page")
        );
        assert_eq!(
            call(&CliDescribe, &ctx, json!({ "id": "nope" })).await,
            Err("No CLI with id \"nope\"".to_owned())
        );
    }

    #[tokio::test]
    async fn 읽기_명령을_실행해_상태와_본문을_돌려준다() {
        let (ctx, id) = shop().await;
        let sender = FakeSender::body(&ctx, 200, r#"{"items":[]}"#);
        let args = json!({ "id": id, "command": "search", "inputs": { "q": "boots" } });
        assert_eq!(
            call(&CliRunTool, &ctx, args).await,
            Ok("{\n  \"status\": 200,\n  \"body\": \"{\\\"items\\\":[]}\"\n}".to_owned())
        );
        assert_eq!(
            lock(&sender.sent)[0].url,
            "https://shop.test/api/items?q=boots"
        );

        // No inputs at all is fine for a command without required flags.
        assert!(
            call(&CliRunTool, &ctx, json!({ "id": id, "command": "search" }))
                .await
                .is_ok()
        );
        assert_eq!(
            call(&CliRunTool, &ctx, json!({ "id": id, "command": "item" })).await,
            Err("missing id0".to_owned())
        );
        assert_eq!(
            call(&CliRunTool, &ctx, json!({ "id": id, "command": "nope" })).await,
            Err("unknown command: nope".to_owned())
        );
        assert_eq!(
            call(
                &CliRunTool,
                &ctx,
                json!({ "id": "nope", "command": "search" })
            )
            .await,
            Err("No CLI with id \"nope\"".to_owned())
        );
    }

    #[tokio::test]
    async fn 잘린_html에는_추출을_알려_주는_안내를_붙인다() {
        let (ctx, id) = shop().await;
        let page = format!("<html><body>{}</body></html>", "x".repeat(5_000));
        FakeSender::body(&ctx, 200, &page);
        let result = call_json(&CliRunTool, &ctx, json!({ "id": id, "command": "page" })).await;
        let keys: Vec<&String> = result.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["status", "body", "note"]);
        assert_eq!(result["note"], RAW_HTML_NOTE);
        assert_eq!(result["body"].as_str().map(str::len), Some(1_500));
    }

    #[tokio::test]
    async fn 쓰기_명령은_스위치를_켜야_보내고_실패하면_결과를_모른다고_답한다() {
        let (ctx, id) = shop().await;
        let sender = FakeSender::body(&ctx, 201, r#"{"id":7}"#);
        let args = json!({ "id": id, "command": "create", "inputs": { "title": "Boot" } });
        assert_eq!(
            call(&CliRunTool, &ctx, args.clone()).await,
            Err(WRITES_DISABLED.to_owned())
        );
        assert!(lock(&sender.sent).is_empty());

        let on = call_json(&CliWrites, &ctx, json!({ "id": id, "enabled": true })).await;
        assert_eq!(on["writes"], true);
        assert_eq!(on["id"], id.as_str());
        let sent = call_json(&CliRunTool, &ctx, args.clone()).await;
        assert_eq!(sent, json!({ "status": 201, "body": "{\"id\":7}" }));

        let failed = SendResponse {
            error: Some("Failed to fetch".to_owned()),
            ..SendResponse::default()
        };
        FakeSender::answering(&ctx, Ok(failed));
        assert_eq!(
            call(&CliRunTool, &ctx, args.clone()).await,
            Err(OUTCOME_UNKNOWN.to_owned())
        );

        let off = call_json(&CliWrites, &ctx, json!({ "id": id, "enabled": false })).await;
        assert_eq!(off["writes"], false);
        assert_eq!(
            call(&CliRunTool, &ctx, args).await,
            Err(WRITES_DISABLED.to_owned())
        );
        assert_eq!(
            call(&CliWrites, &ctx, json!({ "id": "nope", "enabled": true })).await,
            Err("No CLI with id \"nope\"".to_owned())
        );
    }

    #[tokio::test]
    async fn 추출을_한_번_주면_저장되어_다음_실행부터_필드를_돌려준다() {
        let page = json!({ "status": 200, "matched": 9, "items": [{ "title": "A", "link": "/a" }, { "title": "B", "link": "/b" }] });
        let browser = FakeBrowser::new(Ok(page));
        let ctx = context_on(browser.clone());
        let id = compiled(&ctx).await;
        let sender = FakeSender::body(&ctx, 200, "never");
        let extract = json!({ "selector": "li", "limit": 1, "fields": [{ "name": "title" }, { "name": "link", "attr": "href" }] });
        let first = call(
            &CliRunTool,
            &ctx,
            json!({ "id": id, "command": "page", "extract": extract }),
        )
        .await;
        assert_eq!(
            first,
            Ok("{\n  \"status\": 200,\n  \"matched\": 9,\n  \"items\": [\n    {\n      \"title\": \"A\",\n      \"link\": \"/a\"\n    }\n  ]\n}".to_owned())
        );
        // Saved on the command: the next run needs no extract.
        let listed = call_json(&CliList, &ctx, json!({})).await;
        assert_eq!(listed[0]["commands"][3]["extract"], "title, link");
        let again = call_json(&CliRunTool, &ctx, json!({ "id": id, "command": "page" })).await;
        assert_eq!(again["items"], json!([{ "title": "A", "link": "/a" }]));
        assert_eq!(lock(&browser.evaluated).len(), 2);
        assert!(lock(&sender.sent).is_empty());

        // An extract that breaks the rules is refused before anything is saved or sent.
        let bad = json!({ "selector": "li", "fields": [{ "name": "a" }, { "name": "a" }] });
        assert_eq!(
            call(
                &CliRunTool,
                &ctx,
                json!({ "id": id, "command": "search", "extract": bad })
            )
            .await,
            Err("duplicate extract field: a".to_owned())
        );
        assert!(
            call_json(&CliList, &ctx, json!({})).await[0]["commands"][0]
                .get("extract")
                .is_none()
        );
    }

    #[tokio::test]
    async fn 추출_결과의_안내는_그대로_전한다() {
        let page =
            json!({ "status": 200, "matched": 0, "items": [], "note": "selector matched nothing" });
        let ctx = context_on(FakeBrowser::new(Ok(page)));
        let id = compiled(&ctx).await;
        let extract = json!({ "selector": "li", "fields": [{ "name": "title" }] });
        let result = call(
            &CliRunTool,
            &ctx,
            json!({ "id": id, "command": "page", "extract": extract }),
        )
        .await;
        assert_eq!(
            result,
            Ok("{\n  \"status\": 200,\n  \"matched\": 0,\n  \"items\": [],\n  \"note\": \"selector matched nothing\"\n}".to_owned())
        );
    }

    #[tokio::test]
    async fn curl_스크립트로_내보낸다() {
        let (ctx, id) = shop().await;
        let script = call(&CliExport, &ctx, json!({ "id": id })).await.unwrap();
        assert_eq!(script, export_curl(&saved_cli(&ctx, &id).unwrap()));
        assert!(script.starts_with("# shop-test https://shop.test\n# writes: off\n"));
        assert!(script.contains("curl -sS -A \"$UA\" -X GET \"$BASE/api/items/$id0\""));
        // Captured values never leave: not the search term, not the token.
        assert!(!script.contains("shoes") && !script.contains("tok1") && !script.contains("tok2"));
        assert_eq!(
            call(&CliExport, &ctx, json!({ "id": "nope" })).await,
            Err("No CLI with id \"nope\"".to_owned())
        );
    }
}
