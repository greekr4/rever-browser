//! Port of `src/main/mcp/tools/traffic.ts` — list_requests, list_endpoints,
//! get_request and get_request_initiator.

use std::fmt;
use std::sync::Mutex;

use rever_analysis::endpoints::{
    EndpointScope, SiteSplit, format_endpoints, split_by_site, summarize_endpoints,
};
use rever_core::traffic::{ListFilter, Source, StoredRequest};
use rever_tools_api::tool::{ToolContext, ToolDef};
use rever_tools_api::utils::{ResultStore, ToolResult, ok};
use serde::de::{Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::args::{Args, int_prop, limit_prop, prop, schema};
use crate::js::{js_key_order, utf16_len};
use crate::{lock, ok_pretty, store};

/// The top level of a JSON document: an array's length or an object's keys.
/// Members are skipped, not built: JSON.parse accepts values serde_json cannot
/// hold (a lone surrogate escape, 1e999, nesting past 128 levels), and a body
/// can be megabytes.
enum TopLevel {
    Array(usize),
    Object(Map<String, Value>),
}

impl<'de> Deserialize<'de> for TopLevel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TopVisitor;

        impl<'de> Visitor<'de> for TopVisitor {
            type Value = TopLevel;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON array or object")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<TopLevel, A::Error> {
                let mut len = 0;
                while seq.next_element::<IgnoredAny>()?.is_some() {
                    len += 1;
                }
                Ok(TopLevel::Array(len))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<TopLevel, A::Error> {
                // A map keeps a repeated key at its first position, as a
                // JavaScript object does.
                let mut keys = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    map.next_value::<IgnoredAny>()?;
                    keys.insert(key, Value::Null);
                }
                Ok(TopLevel::Object(keys))
            }
        }

        deserializer.deserialize_any(TopVisitor)
    }
}

// Top-level keys of a JSON response body, for the summary view. Returns None
// when the body isn't present/decodable JSON.
fn top_level_json_keys(entry: &StoredRequest) -> Option<Vec<String>> {
    if entry.response_body_base64 == Some(true) {
        return None;
    }
    let body = entry.response_body.as_deref().filter(|b| !b.is_empty())?;
    match serde_json::from_str(body).ok()? {
        TopLevel::Array(len) => Some(vec![format!("(array, length {len})")]),
        TopLevel::Object(keys) => {
            Some(js_key_order(keys).into_iter().map(|(key, _)| key).collect())
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Summary<'a> {
    request_id: &'a str,
    method: &'a str,
    url: &'a str,
    host: &'a str,
    resource_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime_type: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encoded_data_length: Option<u64>,
    started_at: u64,
}

fn to_summary(e: &StoredRequest) -> Summary<'_> {
    Summary {
        request_id: &e.request_id,
        method: &e.method,
        url: &e.url,
        host: &e.host,
        resource_type: &e.resource_type,
        status: e.status,
        mime_type: e.mime_type.as_deref(),
        encoded_data_length: e.encoded_data_length,
        started_at: e.started_at,
    }
}

// The agent addresses tabs by the index it sees in browser_tabs; resolve that
// index to the tab's stable id (the `tab_id` stamped on captured requests).
async fn resolve_tab(ctx: &ToolContext, args: &Args<'_>) -> Result<Option<u64>, String> {
    let Some(index) = args.any_int("tab")? else {
        return Ok(None);
    };
    let index = usize::try_from(index).map_err(|_| format!("invalid tab index: {index}"))?;
    let tabs = ctx.browser.tabs().await?;
    let tab = tabs
        .get(index)
        .ok_or_else(|| format!("no tab at index {index} (there are {} tabs)", tabs.len()))?;
    Ok(Some(tab.id))
}

tool!(
    ListRequests,
    ToolDef::new(
        "list_requests",
        "List recent network requests captured by the browser, newest first. Filter by host / method / type / since. Response headers and body are NOT included — call get_request for details.",
        schema(
            json!({
                "host": prop("string", "Substring host filter (e.g. \"danawa.com\")"),
                "methodOrType": prop(
                    "string",
                    "Substring match against HTTP method (GET/POST...) or ResourceType (XHR/Fetch/Document...)"
                ),
                "since": prop("number", "Only include requests started after this epoch ms"),
                "tab": int_prop("Only this tab, by its index in browser_tabs (default: all tabs)"),
                "limit": limit_prop("Max items (default 50)", 200),
                "source": {
                    "description": "live = captured in this app (default), import = loaded with import_har, all = both",
                    "type": "string",
                    "enum": ["live", "import", "all"]
                }
            }),
            &[],
        ),
    ),
    |ctx, args| list_requests(ctx, &args).await
);

async fn list_requests(ctx: &ToolContext, args: &Args<'_>) -> Result<ToolResult, String> {
    let filter = ListFilter {
        host: args.str("host")?.map(str::to_owned),
        method_or_type: args.str("methodOrType")?.map(str::to_owned),
        since: args.since()?,
        limit: args.limit(200)?,
        tab_id: resolve_tab(ctx, args).await?,
        source: match args.str("source")? {
            None | Some("live") => Source::Live,
            Some("import") => Source::Import,
            Some("all") => Source::All,
            Some(_) => {
                return Err(
                    "invalid arguments: `source` must be one of live, import, all".to_owned(),
                );
            }
        },
        ..ListFilter::default()
    };
    let store = store(ctx);
    let rows: Vec<Summary> = store.list(&filter).into_iter().map(to_summary).collect();
    ok_pretty(&rows)
}

tool!(
    ListEndpoints,
    ToolDef::new(
        "list_endpoints",
        "Map of the API endpoints seen so far: captured XHR/Fetch requests grouped by method + origin + path (numeric/hex/uuid segments become :id), one line each with call count, status codes and the latest requestId. Defaults to the current page's site (eTLD+1; other sites visited earlier are counted, not listed). Far smaller than list_requests — call this first to find the API, then get_request the last=# id.",
        schema(
            json!({
                "host": prop(
                    "string",
                    "Substring host filter (e.g. \"api.example.com\"); replaces the current-site default"
                ),
                "all": prop("boolean", "Include every site in the store, not just the current page's"),
                "tab": int_prop("Only endpoints captured in this tab, by its browser_tabs index"),
                "limit": limit_prop("Max endpoints listed (default 50)", 200)
            }),
            &[],
        ),
    ),
    |ctx, args| list_endpoints(ctx, &args).await
);

async fn list_endpoints(ctx: &ToolContext, args: &Args<'_>) -> Result<ToolResult, String> {
    let host = args.str("host")?;
    let all = args.bool("all")?.unwrap_or(false);
    let limit = args.limit(200)?.unwrap_or(50);
    let tab_id = resolve_tab(ctx, args).await?;

    let every_site = host.is_some_and(|host| !host.is_empty()) || all;
    // Asked before the store is locked: the guard cannot be held across an await.
    let page_url = if every_site {
        None
    } else {
        ctx.browser.page().await.ok().map(|page| page.url)
    };

    let store = store(ctx);
    // The live ring buffer holds 500 entries, so this scans all of it.
    let rows = store.list(&ListFilter {
        host: host.map(str::to_owned),
        limit: Some(500),
        tab_id,
        ..ListFilter::default()
    });
    if every_site {
        return Ok(ok(format_endpoints(
            &summarize_endpoints(rows),
            limit,
            None,
        )));
    }
    let SiteSplit { site, same, other } = split_by_site(rows, page_url.as_deref());
    let scope = site.as_deref().map(|site| EndpointScope {
        site,
        other_sites: summarize_endpoints(other).len(),
    });
    Ok(ok(format_endpoints(
        &summarize_endpoints(same),
        limit,
        scope.as_ref(),
    )))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LargeEntrySummary<'a> {
    request_id: &'a str,
    method: &'a str,
    url: &'a str,
    host: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime_type: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encoded_data_length: Option<u64>,
    response_body_base64: bool,
    body_chars: usize,
    response_body_top_keys: Option<Vec<String>>,
    hint: &'static str,
}

tool!(
    GetRequest,
    ToolDef::new(
        "get_request",
        "Return a captured request/response incl. headers and body. Small entries come back in full; large ones come back as a compact SUMMARY (method, url, status, content-type, body size, top-level JSON keys) — then pass full=true to page the complete entry via tool_result_more. full=false always returns the summary. If the body is base64-encoded, responseBodyBase64=true.",
        schema(
            json!({
                "requestId": { "type": "string", "description": "requestId returned by list_requests" },
                "full": prop(
                    "boolean",
                    "true = full entry (paged when large), false = summary only, omit = full when small"
                )
            }),
            &["requestId"],
        ),
    ),
    |ctx, args| get_request(ctx, &args)
);

fn get_request(ctx: &ToolContext, args: &Args) -> Result<ToolResult, String> {
    let request_id = args.required_str("requestId")?;
    let full = args.bool("full")?;
    let store = store(ctx);
    let Some(entry) = store.get(request_id) else {
        return Err(format!("unknown requestId: {request_id}"));
    };
    let text = serde_json::to_string_pretty(entry).map_err(|error| error.to_string())?;
    let summarize = || LargeEntrySummary {
        request_id: &entry.request_id,
        method: &entry.method,
        url: &entry.url,
        host: &entry.host,
        status: entry.status,
        mime_type: entry.mime_type.as_deref(),
        encoded_data_length: entry.encoded_data_length,
        response_body_base64: entry.response_body_base64.unwrap_or(false),
        body_chars: entry.response_body.as_deref().map_or(0, utf16_len),
        response_body_top_keys: top_level_json_keys(entry),
        hint: "entry too large for one response — call get_request with full=true to page headers and body",
    };
    // One store per process, shared with tool_result_more: both reach it as
    // this state type.
    let results = ctx.state::<Mutex<ResultStore>>();
    lock(&results)
        .ok_full_or_summary(text, summarize, full)
        .map_err(|error| error.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Frame<'a> {
    function_name: &'a str,
    url: &'a str,
    line: i64,
    column: i64,
    location: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Initiator<'a> {
    request_id: &'a str,
    method: &'a str,
    url: &'a str,
    initiator_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    initiator_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fired_from: Option<&'a str>,
    stack: &'a [Frame<'a>],
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'static str>,
}

tool!(
    GetRequestInitiator,
    ToolDef::new(
        "get_request_initiator",
        "Show what caused a captured request to fire: the initiator type (script/parser/preload/other) and, for script-initiated XHR/Fetch, the JavaScript call stack (top frame = the code that directly issued it). Use this to jump from a request straight to the script:line that built it, before setting a breakpoint. Line/column are 1-based for readability.",
        schema(
            json!({
                "requestId": { "type": "string", "description": "requestId returned by list_requests" }
            }),
            &["requestId"],
        ),
    ),
    |ctx, args| get_request_initiator(ctx, &args)
);

fn get_request_initiator(ctx: &ToolContext, args: &Args) -> Result<ToolResult, String> {
    let request_id = args.required_str("requestId")?;
    let store = store(ctx);
    let Some(entry) = store.get(request_id) else {
        return Err(format!("unknown requestId: {request_id}"));
    };
    let stack: Vec<Frame> = entry
        .initiator_stack
        .iter()
        .flatten()
        .map(|f| {
            let line = f.line_number.saturating_add(1);
            let column = f.column_number.saturating_add(1);
            Frame {
                function_name: if f.function_name.is_empty() {
                    "(anonymous)"
                } else {
                    &f.function_name
                },
                url: &f.url,
                line,
                column,
                location: format!("{}:{line}:{column}", f.url),
            }
        })
        .collect();
    ok_pretty(&Initiator {
        request_id: &entry.request_id,
        method: &entry.method,
        url: &entry.url,
        initiator_type: entry.initiator_type.as_deref().unwrap_or("unknown"),
        initiator_url: entry.initiator_url.as_deref(),
        fired_from: stack.first().map(|top| top.location.as_str()),
        stack: &stack,
        note: stack.is_empty().then_some(
            "No JS call stack — this request was not script-initiated (e.g. parser/document, preload, or a redirect). initiatorType tells you which.",
        ),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rever_core::traffic::{CallFrame, RequestPatch};
    use rever_tools_api::tool::ToolContext;

    use super::*;
    use crate::test_support::{FakeBrowser, call, call_json, ctx_with, put, request};

    fn with_body(mut patch: RequestPatch, body: &str) -> RequestPatch {
        patch.response_body = Some(body.to_owned());
        patch
    }

    fn entry_with_body(body: &str, base64: bool) -> StoredRequest {
        StoredRequest {
            response_body: Some(body.to_owned()),
            response_body_base64: base64.then_some(true),
            ..StoredRequest::default()
        }
    }

    #[test]
    fn 본문의_최상위_json_키를_자바스크립트_순서로_돌려준다() {
        let keys = |body: &str| top_level_json_keys(&entry_with_body(body, false));
        assert_eq!(
            keys(r#"{"b":1,"a":{"x":1},"2":0,"1":0}"#),
            Some(vec![
                "1".to_owned(),
                "2".to_owned(),
                "b".to_owned(),
                "a".to_owned()
            ])
        );
        assert_eq!(keys("[1,2,3]"), Some(vec!["(array, length 3)".to_owned()]));
        assert_eq!(keys("{}"), Some(Vec::new()));
    }

    #[test]
    fn 본문이_없거나_base64이거나_json_객체가_아니면_키가_없다() {
        assert_eq!(top_level_json_keys(&StoredRequest::default()), None);
        assert_eq!(top_level_json_keys(&entry_with_body("", false)), None);
        assert_eq!(
            top_level_json_keys(&entry_with_body("{\"a\":1}", true)),
            None
        );
        assert_eq!(top_level_json_keys(&entry_with_body("<html>", false)), None);
        assert_eq!(top_level_json_keys(&entry_with_body("42", false)), None);
        assert_eq!(top_level_json_keys(&entry_with_body("null", false)), None);
    }

    #[test]
    fn serde_json이_담지_못하는_json도_json_parse처럼_최상위_키를_읽는다() {
        let keys = |body: &str| top_level_json_keys(&entry_with_body(body, false));
        let ab = Some(vec!["a".to_owned(), "b".to_owned()]);
        // What Object.keys(JSON.parse(body)) gives in bun for each of these.
        // A lone surrogate escape in a value.
        assert_eq!(keys(r#"{"a":"\ud83d","b":1}"#), ab);
        // Numbers past the double range parse as Infinity and 0.
        assert_eq!(keys(r#"{"a":1e999,"b":-1E-999}"#), ab);
        // Nesting deeper than serde_json's recursion limit.
        let deep = format!("{}1{}", "{\"a\":".repeat(300), "}".repeat(300));
        assert_eq!(keys(&deep), Some(vec!["a".to_owned()]));
        let nested = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
        assert_eq!(keys(&nested), Some(vec!["(array, length 1)".to_owned()]));
        // A repeated key keeps its first position.
        assert_eq!(
            keys(r#"{"a":1,"b":3,"a":2,"__proto__":4}"#),
            Some(vec!["a".to_owned(), "b".to_owned(), "__proto__".to_owned()])
        );
        assert_eq!(
            keys(r#"[1,[2,3],{"x":1}]"#),
            Some(vec!["(array, length 3)".to_owned()])
        );
        assert_eq!(keys(" \n{\"a\":1,\"b\":2}\t "), ab);
    }

    #[test]
    fn 건너뛰는_값도_json_문법은_지켜야_한다() {
        let keys = |body: &str| top_level_json_keys(&entry_with_body(body, false));
        // JSON.parse throws on each of these.
        for body in [
            r#"{"a":1}garbage"#,
            r#"{"a":01}"#,
            r#"{"a":"\x41"}"#,
            "{\"a\":\"tab\there\"}",
            "\u{feff}{\"a\":1}",
            r#"{"a":1,}"#,
            r#"{"a":[1,}"#,
            r#"{"a":tru}"#,
            r#"{"a":"\u12"}"#,
            r#"{a:1}"#,
            "[1,2",
        ] {
            assert_eq!(keys(body), None, "{body}");
        }
        assert_eq!(keys("\"str\""), None);
        assert_eq!(keys("true"), None);
        assert_eq!(keys("1e999"), None);
    }

    #[tokio::test]
    async fn 요청_목록은_최신순_요약이고_없는_필드는_키도_없다() {
        let ctx = ToolContext::for_tests();
        put(
            &ctx,
            request("1", "https://a.test/x", "GET", "Document"),
            100,
        );
        let mut second = request("2", "https://api.b.test/v1?q=1", "POST", "XHR");
        second.status = Some(201);
        second.mime_type = Some("application/json".to_owned());
        second.encoded_data_length = Some(12);
        second.response_body = Some("{}".to_owned());
        put(&ctx, second, 200);

        let text = call(&ListRequests, &ctx, json!({})).await.unwrap();
        assert_eq!(
            text,
            r#"[
  {
    "requestId": "2",
    "method": "POST",
    "url": "https://api.b.test/v1?q=1",
    "host": "api.b.test",
    "resourceType": "XHR",
    "status": 201,
    "mimeType": "application/json",
    "encodedDataLength": 12,
    "startedAt": 200
  },
  {
    "requestId": "1",
    "method": "GET",
    "url": "https://a.test/x",
    "host": "a.test",
    "resourceType": "Document",
    "startedAt": 100
  }
]"#
        );
    }

    #[tokio::test]
    async fn 요청_목록을_호스트_메서드_since_limit로_거른다() {
        let ctx = ToolContext::for_tests();
        put(
            &ctx,
            request("1", "https://a.test/1", "GET", "Document"),
            100,
        );
        put(
            &ctx,
            request("2", "https://api.a.test/2", "POST", "XHR"),
            200,
        );
        put(&ctx, request("3", "https://b.test/3", "GET", "Fetch"), 300);

        let ids = |rows: Value| -> Vec<String> {
            rows.as_array()
                .into_iter()
                .flatten()
                .filter_map(|row| row.get("requestId")?.as_str().map(str::to_owned))
                .collect()
        };
        let by_host = call_json(&ListRequests, &ctx, json!({ "host": "a.test" })).await;
        assert_eq!(ids(by_host), ["2", "1"]);
        let by_type = call_json(&ListRequests, &ctx, json!({ "methodOrType": "fetch" })).await;
        assert_eq!(ids(by_type), ["3"]);
        let by_method = call_json(&ListRequests, &ctx, json!({ "methodOrType": "post" })).await;
        assert_eq!(ids(by_method), ["2"]);
        let since = call_json(&ListRequests, &ctx, json!({ "since": 199.5 })).await;
        assert_eq!(ids(since), ["3", "2"]);
        let limited = call_json(&ListRequests, &ctx, json!({ "limit": 1 })).await;
        assert_eq!(ids(limited), ["3"]);
    }

    #[tokio::test]
    async fn source로_가져온_har과_실시간_캡처를_고른다() {
        let ctx = ToolContext::for_tests();
        put(&ctx, request("live", "https://a.test/", "GET", "XHR"), 100);
        lock(&ctx.traffic).import_requests(vec![StoredRequest {
            request_id: "har:x:0".to_owned(),
            url: "https://old.test/".to_owned(),
            host: "old.test".to_owned(),
            method: "GET".to_owned(),
            resource_type: "XHR".to_owned(),
            started_at: 50,
            ..StoredRequest::default()
        }]);
        let count = |rows: Value| rows.as_array().map_or(0, Vec::len);
        assert_eq!(count(call_json(&ListRequests, &ctx, json!({})).await), 1);
        let imported = call_json(&ListRequests, &ctx, json!({ "source": "import" })).await;
        assert_eq!(imported[0]["requestId"], "har:x:0");
        let all = call_json(&ListRequests, &ctx, json!({ "source": "all" })).await;
        assert_eq!(count(all), 2);
        assert_eq!(
            call(&ListRequests, &ctx, json!({ "source": "disk" })).await,
            Err("invalid arguments: `source` must be one of live, import, all".to_owned())
        );
    }

    #[tokio::test]
    async fn 잘못된_인자와_탭_필터는_에러_결과다() {
        let ctx = ToolContext::for_tests();
        assert!(
            call(&ListRequests, &ctx, json!({ "limit": 201 }))
                .await
                .is_err()
        );
        assert!(
            call(&ListRequests, &ctx, json!({ "host": 3 }))
                .await
                .is_err()
        );
        assert!(
            call(&ListRequests, &ctx, json!({ "tab": 1.5 }))
                .await
                .is_err()
        );
        // A tab filter on a ctx with no tabs can't resolve the index → error.
        assert!(
            call(&ListRequests, &ctx, json!({ "tab": 0 }))
                .await
                .is_err()
        );
        assert!(
            call(&ListEndpoints, &ctx, json!({ "tab": 2 }))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn 탭_인덱스를_해석해_그_탭의_요청만_필터한다() {
        use rever_tools_api::tool::TabInfo;
        use std::sync::Arc;

        // browser_tabs index 0 → id 10, index 1 → id 20.
        let ctx = ctx_with(Arc::new(FakeBrowser {
            tabs: vec![
                TabInfo {
                    id: 10,
                    ..TabInfo::default()
                },
                TabInfo {
                    id: 20,
                    ..TabInfo::default()
                },
            ],
            ..FakeBrowser::default()
        }));
        let tab_req = |id: &str, tab: u64| {
            let mut patch = request(id, "https://x.test/a", "GET", "XHR");
            patch.tab_id = Some(tab);
            patch
        };
        put(&ctx, tab_req("a", 10), 1);
        put(&ctx, tab_req("b", 20), 2);

        // tab:0 resolves to id 10 → only request "a".
        let rows = call_json(&ListRequests, &ctx, json!({ "tab": 0 })).await;
        let ids: Vec<&str> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["requestId"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["a"]);
        // tab:1 → id 20 → only "b".
        let rows = call_json(&ListRequests, &ctx, json!({ "tab": 1 })).await;
        assert_eq!(rows[0]["requestId"], "b");
        // Out-of-range index errors.
        assert!(
            call(&ListRequests, &ctx, json!({ "tab": 9 }))
                .await
                .is_err()
        );
    }

    fn endpoints_ctx(page_url: Option<&str>) -> ToolContext {
        let ctx = ctx_with(Arc::new(FakeBrowser {
            page_url: page_url.map(str::to_owned),
            ..FakeBrowser::default()
        }));
        put(
            &ctx,
            request("1", "https://api.shop.test/items/1", "GET", "XHR"),
            100,
        );
        put(
            &ctx,
            request("2", "https://api.shop.test/items/2", "GET", "XHR"),
            200,
        );
        put(
            &ctx,
            request("3", "https://other.test/track", "POST", "Fetch"),
            300,
        );
        put(
            &ctx,
            request("4", "https://www.shop.test/", "GET", "Document"),
            400,
        );
        ctx
    }

    #[tokio::test]
    async fn 엔드포인트는_현재_페이지의_사이트로_좁히고_나머지는_센다() {
        let ctx = endpoints_ctx(Some("https://www.shop.test/cart"));
        assert_eq!(
            call(&ListEndpoints, &ctx, json!({})).await.unwrap(),
            "1 endpoint(s) on site shop.test, most-called first. get_request the last=# id for details.\n\
             GET https://api.shop.test/items/:id ×2 last=#2\n\
             (1 endpoint(s) on other sites hidden — pass all=true to include them)"
        );
    }

    #[tokio::test]
    async fn all이나_host를_주면_사이트로_좁히지_않는다() {
        let ctx = endpoints_ctx(Some("https://www.shop.test/cart"));
        assert_eq!(
            call(&ListEndpoints, &ctx, json!({ "all": true }))
                .await
                .unwrap(),
            "2 endpoint(s), most-called first. get_request the last=# id for details.\n\
             GET https://api.shop.test/items/:id ×2 last=#2\n\
             POST https://other.test/track ×1 last=#3"
        );
        assert_eq!(
            call(&ListEndpoints, &ctx, json!({ "host": "other" }))
                .await
                .unwrap(),
            "1 endpoint(s), most-called first. get_request the last=# id for details.\n\
             POST https://other.test/track ×1 last=#3"
        );
        // An empty host is falsy in the TypeScript: the site default still applies.
        let scoped = call(&ListEndpoints, &ctx, json!({ "host": "" }))
            .await
            .unwrap();
        assert!(scoped.starts_with("1 endpoint(s) on site shop.test"));
        let limited = call(&ListEndpoints, &ctx, json!({ "all": true, "limit": 1 }))
            .await
            .unwrap();
        assert!(limited.ends_with("last=#2\n… +1 more"), "{limited}");
    }

    #[tokio::test]
    async fn 페이지_주소를_모르면_모든_사이트를_보여준다() {
        let ctx = endpoints_ctx(None);
        let text = call(&ListEndpoints, &ctx, json!({})).await.unwrap();
        assert!(
            text.starts_with("2 endpoint(s), most-called first."),
            "{text}"
        );

        let empty = ToolContext::for_tests();
        assert_eq!(
            call(&ListEndpoints, &empty, json!({})).await.unwrap(),
            "No XHR/Fetch endpoints captured yet."
        );
    }

    #[tokio::test]
    async fn 작은_요청은_전체를_그대로_돌려준다() {
        let ctx = ToolContext::for_tests();
        let mut patch = with_body(request("1", "https://a.test/x", "GET", "XHR"), "{\"a\":1}");
        patch.status = Some(200);
        put(&ctx, patch, 100);

        let entry = call_json(&GetRequest, &ctx, json!({ "requestId": "1" })).await;
        assert_eq!(
            entry,
            json!({
                "requestId": "1", "url": "https://a.test/x", "host": "a.test", "method": "GET",
                "resourceType": "XHR", "startedAt": 100, "status": 200, "responseBody": "{\"a\":1}"
            })
        );
        assert_eq!(
            call(&GetRequest, &ctx, json!({ "requestId": "nope" })).await,
            Err("unknown requestId: nope".to_owned())
        );
        assert!(call(&GetRequest, &ctx, json!({})).await.is_err());
        assert!(
            call(&GetRequest, &ctx, json!({ "requestId": "1", "full": 1 }))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn 큰_요청은_요약을_돌려주고_full이면_나눠_읽게_한다() {
        let ctx = ToolContext::for_tests();
        // 한 is one UTF-16 unit and three bytes: bodyChars counts units.
        let body = format!("{{\"items\":\"{}\",\"total\":1}}", "한".repeat(13_000));
        let mut patch = with_body(request("big", "https://a.test/list", "GET", "Fetch"), &body);
        patch.status = Some(200);
        patch.mime_type = Some("application/json".to_owned());
        put(&ctx, patch, 100);

        let expected = json!({
            "requestId": "big",
            "method": "GET",
            "url": "https://a.test/list",
            "host": "a.test",
            "status": 200,
            "mimeType": "application/json",
            "responseBodyBase64": false,
            "bodyChars": 13_022,
            "responseBodyTopKeys": ["items", "total"],
            "hint": "entry too large for one response — call get_request with full=true to page headers and body"
        });
        let summary = call_json(&GetRequest, &ctx, json!({ "requestId": "big" })).await;
        assert_eq!(summary, expected);
        // The key order is the TypeScript's, not alphabetical.
        let keys: Vec<&String> = summary.as_object().unwrap().keys().collect();
        assert_eq!(keys[..3], ["requestId", "method", "url"]);

        let paged = call(
            &GetRequest,
            &ctx,
            json!({ "requestId": "big", "full": true }),
        )
        .await
        .unwrap();
        assert!(
            paged.starts_with("{\n  \"url\": \"https://a.test/list\","),
            "{}",
            &paged[..40]
        );
        assert!(paged.contains("Call tool_result_more with resultId=\"res_1\""));
        // The parked text is in the state tool_result_more reads.
        let results = ctx.state::<Mutex<ResultStore>>();
        assert!(
            lock(&results)
                .get_result_slice("res_1", 0, Some(10))
                .is_some()
        );
    }

    #[tokio::test]
    async fn full이_false면_작은_요청도_요약이고_json이_아닌_본문은_키가_null이다() {
        let ctx = ToolContext::for_tests();
        put(
            &ctx,
            with_body(request("1", "https://a.test/x", "GET", "XHR"), "<html>"),
            100,
        );
        let summary = call_json(
            &GetRequest,
            &ctx,
            json!({ "requestId": "1", "full": false }),
        )
        .await;
        assert_eq!(summary["bodyChars"], 6);
        assert_eq!(summary["responseBodyTopKeys"], Value::Null);
        assert_eq!(summary.get("status"), None);
    }

    #[tokio::test]
    async fn 호출_스택을_1부터_세는_줄과_열로_보여준다() {
        let ctx = ToolContext::for_tests();
        let mut patch = request("1", "https://a.test/api", "POST", "Fetch");
        patch.initiator_type = Some("script".to_owned());
        patch.initiator_url = Some("https://a.test/app.js".to_owned());
        patch.initiator_stack = Some(vec![
            CallFrame {
                function_name: "send".to_owned(),
                url: "https://a.test/app.js".to_owned(),
                line_number: 9,
                column_number: 4,
            },
            CallFrame {
                function_name: String::new(),
                url: "https://a.test/app.js".to_owned(),
                line_number: 0,
                column_number: 0,
            },
        ]);
        put(&ctx, patch, 100);

        let text = call(&GetRequestInitiator, &ctx, json!({ "requestId": "1" }))
            .await
            .unwrap();
        assert_eq!(
            text,
            r#"{
  "requestId": "1",
  "method": "POST",
  "url": "https://a.test/api",
  "initiatorType": "script",
  "initiatorUrl": "https://a.test/app.js",
  "firedFrom": "https://a.test/app.js:10:5",
  "stack": [
    {
      "functionName": "send",
      "url": "https://a.test/app.js",
      "line": 10,
      "column": 5,
      "location": "https://a.test/app.js:10:5"
    },
    {
      "functionName": "(anonymous)",
      "url": "https://a.test/app.js",
      "line": 1,
      "column": 1,
      "location": "https://a.test/app.js:1:1"
    }
  ]
}"#
        );
    }

    #[tokio::test]
    async fn 스택이_없으면_이유를_알리는_note를_붙인다() {
        let ctx = ToolContext::for_tests();
        put(
            &ctx,
            request("1", "https://a.test/", "GET", "Document"),
            100,
        );
        let out = call_json(&GetRequestInitiator, &ctx, json!({ "requestId": "1" })).await;
        assert_eq!(
            out,
            json!({
                "requestId": "1", "method": "GET", "url": "https://a.test/",
                "initiatorType": "unknown", "stack": [],
                "note": "No JS call stack — this request was not script-initiated (e.g. parser/document, preload, or a redirect). initiatorType tells you which."
            })
        );
        assert_eq!(
            call(&GetRequestInitiator, &ctx, json!({ "requestId": "x" })).await,
            Err("unknown requestId: x".to_owned())
        );
    }
}
