//! Port of `src/main/mcp/tools/graphql.ts` — the `graphql_introspect` tool.
//!
//! The response is untrusted JSON that the TypeScript walked without checking
//! its shape, so a malformed schema surfaced as the message of whatever
//! TypeError JavaScript threw. Those messages are reproduced here.

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Map, Value, json};

use crate::args::validate;
use crate::js::{
    is_truthy, js_join, json_parse, json_string, json_stringify, to_js_string, utf16_prefix,
};
use crate::traffic;

const INTROSPECTION_QUERY: &str = "query IntrospectionQuery { __schema { queryType { name } mutationType { name } types { kind name fields { name args { name } type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } } } }";

/// The request headers of a captured request worth replaying for auth.
const REUSED_HEADERS: [&str; 4] = ["authorization", "cookie", "x-api-key", "x-auth-token"];

fn render_ref(t: Option<&Value>) -> String {
    if !is_truthy(t) {
        return "?".into();
    }
    let of_type = t.and_then(|t| t.get("ofType"));
    match t.and_then(|t| t.get("kind")).and_then(Value::as_str) {
        Some("NON_NULL") => render_ref(of_type) + "!",
        Some("LIST") => format!("[{}]", render_ref(of_type)),
        _ => {
            let name = t.and_then(|t| t.get("name"));
            if is_truthy(name) {
                to_js_string(name)
            } else {
                "?".into()
            }
        }
    }
}

/// `t.name && !t.name.startsWith('__')`.
fn is_user_type(t: &Value) -> Result<bool, String> {
    if t.is_null() {
        return Err("Cannot read properties of null (reading 'name')".into());
    }
    match t.get("name") {
        name if !is_truthy(name) => Ok(false),
        Some(Value::String(name)) => Ok(!name.starts_with("__")),
        _ => Err("t.name.startsWith is not a function".into()),
    }
}

fn field_line(f: &Value) -> Result<String, String> {
    if f.is_null() {
        return Err("Cannot read properties of null (reading 'args')".into());
    }
    // `f.args && f.args.length ? … f.args.map(…)`: anything with a truthy
    // `length` gets `.map` called on it, and only an array has one.
    let args = match f.get("args") {
        Some(Value::Array(args)) if !args.is_empty() => {
            let names = args
                .iter()
                .map(|a| match a {
                    Value::Null => Err("Cannot read properties of null (reading 'name')"),
                    a => Ok(a.get("name")),
                })
                .collect::<Result<Vec<_>, _>>()?;
            format!("({})", js_join(names.into_iter(), ", "))
        }
        Some(Value::String(args)) if !args.is_empty() => {
            return Err("f.args.map is not a function".into());
        }
        Some(Value::Object(args)) if is_truthy(args.get("length")) => {
            return Err("f.args.map is not a function".into());
        }
        _ => String::new(),
    };
    Ok(format!(
        "  {}{args}: {}",
        to_js_string(f.get("name")),
        render_ref(f.get("type"))
    ))
}

fn build_sdl(types: &[Value]) -> Result<String, String> {
    let mut out = Vec::new();
    for t in types {
        if !is_user_type(t)? {
            continue;
        }
        let keyword = match t.get("kind").and_then(Value::as_str) {
            Some("OBJECT") => "type",
            Some("INTERFACE") => "interface",
            Some("INPUT_OBJECT") => "input",
            _ => continue,
        };
        let fields = match t.get("fields") {
            fields if !is_truthy(fields) => continue,
            Some(Value::Array(fields)) => fields,
            _ => return Err("t.fields.map is not a function".into()),
        };
        let fields = fields
            .iter()
            .map(field_line)
            .collect::<Result<Vec<_>, _>>()?;
        out.push(format!(
            "{keyword} {} {{\n{}\n}}",
            to_js_string(t.get("name")),
            fields.join("\n")
        ));
    }
    Ok(out.join("\n\n"))
}

/// The tool's answer for a response body, or the message of the error the
/// TypeScript would have caught.
fn describe_response(status: u16, text: &str) -> Result<ToolResult, String> {
    let Ok(json) = json_parse(text) else {
        return Ok(err(format!(
            "response was not JSON (status {status}): {}",
            utf16_prefix(text, 300)
        )));
    };
    let schema = json.get("data").and_then(|data| data.get("__schema"));
    let types = schema.and_then(|schema| schema.get("types"));
    if !is_truthy(schema) || !is_truthy(types) {
        let head = json_stringify(
            &json!({
                "introspectionEnabled": false,
                "status": status,
                "note": "No __schema in the response — introspection appears disabled or blocked (a finding in itself).",
                "serverErrors": json.get("errors").cloned().unwrap_or(Value::Null),
            }),
            false,
        );
        // `raw` is written by hand: text.slice(0, 400) can end on half of a
        // surrogate pair, which JSON.stringify prints as a `\udXXX` escape.
        let raw: Vec<u16> = text.encode_utf16().take(400).collect();
        return Ok(ok(format!(
            "{},\"raw\":{}}}",
            head.strip_suffix('}').unwrap_or(&head),
            json_string(&raw)
        )));
    }
    let Some(Value::Array(types)) = types else {
        return Err("schema.types.filter is not a function".into());
    };
    let mut user_types = Vec::new();
    for t in types {
        if is_user_type(t)? {
            user_types.push(t);
        }
    }
    let type_names: Vec<String> = user_types
        .iter()
        .map(|t| {
            format!(
                "{} {}",
                to_js_string(t.get("kind")),
                to_js_string(t.get("name"))
            )
        })
        .collect();
    let sdl = build_sdl(types)?;

    let root_name = |key: &str| schema.and_then(|s| s.get(key)).and_then(|t| t.get("name"));
    let mut out = Map::new();
    out.insert("introspectionEnabled".into(), json!(true));
    // A root type the schema does not have is left out, as `undefined` was.
    for key in ["queryType", "mutationType"] {
        if let Some(name) = root_name(key) {
            out.insert(key.into(), name.clone());
        }
    }
    out.insert("typeCount".into(), json!(user_types.len()));
    out.insert("typeNames".into(), json!(type_names));
    out.insert("sdl".into(), json!(sdl));
    Ok(ok(json_stringify(&Value::Object(out), true)))
}

/// POSTs the introspection query the way Node's `fetch` did and returns the
/// status with the body decoded as UTF-8. Errors carry the message `fetch`
/// rejected with.
async fn post_query(url: &str, headers: &[(String, String)]) -> Result<(u16, String), String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| format!("Failed to parse URL from {url}"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!(
            "Request cannot be constructed from a URL that includes credentials: {url}"
        ));
    }
    // What Node's fetch sends on its own; a server or WAF may key on them.
    let mut header_map = HeaderMap::new();
    header_map.insert("accept-language", HeaderValue::from_static("*"));
    header_map.insert("sec-fetch-mode", HeaderValue::from_static("cors"));
    header_map.insert("user-agent", HeaderValue::from_static("node"));
    for (name, value) in headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("Headers.append: \"{name}\" is an invalid header name."))?;
        let header_value = HeaderValue::from_bytes(value.as_bytes())
            .map_err(|_| format!("Headers.append: \"{value}\" is an invalid header value."))?;
        header_map.append(header_name, header_value);
    }
    let fetch_failed = |_| "fetch failed".to_owned();
    // Node's fetch follows up to 20 redirects and ignores proxy environment variables.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(20))
        .no_proxy()
        .build()
        .map_err(fetch_failed)?;
    let response = client
        .post(parsed)
        .headers(header_map)
        .body(json_stringify(
            &json!({ "query": INTROSPECTION_QUERY }),
            false,
        ))
        .send()
        .await
        .map_err(fetch_failed)?;
    let status = response.status().as_u16();
    let body = response
        .bytes()
        .await
        .map_err(|_| "terminated".to_owned())?;
    // `Response.text()` drops a leading byte-order mark.
    let body = body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&body);
    Ok((status, String::from_utf8_lossy(body).into_owned()))
}

async fn graphql_introspect(ctx: &ToolContext, args: &Value) -> ToolResult {
    let text = |name: &str| args.get(name).and_then(Value::as_str);
    let url = text("url").unwrap_or("");
    let mut headers = vec![("content-type".to_owned(), "application/json".to_owned())];
    if let Some(request_id) = text("requestId").filter(|id| !id.is_empty()) {
        let store = traffic(ctx);
        let captured = store
            .get(request_id)
            .and_then(|entry| entry.request_headers.as_ref());
        for (name, value) in captured.into_iter().flatten() {
            if REUSED_HEADERS.iter().any(|h| name.eq_ignore_ascii_case(h)) {
                headers.push((name.clone(), value.clone()));
            }
        }
    }
    let answer = match post_query(url, &headers).await {
        Ok((status, body)) => describe_response(status, &body),
        Err(message) => Err(message),
    };
    answer.unwrap_or_else(err)
}

pub struct GraphqlIntrospect;

impl Tool for GraphqlIntrospect {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "graphql_introspect",
            "Run a GraphQL introspection query against an endpoint and reconstruct its schema (SDL: types, fields, and arguments). The fastest way to map a GraphQL API surface. Optionally reuse a captured request's headers (auth) by passing its requestId. If introspection is disabled the server will say so — that itself is a useful finding.",
            json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "GraphQL endpoint URL (e.g. https://site/graphql)"
                    },
                    "requestId": {
                        "description": "Optional: reuse this captured request's headers (cookies/authorization)",
                        "type": "string"
                    }
                },
                "required": ["url"],
                "$schema": "http://json-schema.org/draft-07/schema#"
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(invalid) = validate(&self.def(), &args) {
                return invalid;
            }
            graphql_introspect(ctx, &args).await
        })
    }
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(GraphqlIntrospect);
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    use rever_core::traffic::RequestPatch;

    use super::*;
    use crate::testing::{headers, request, run, store};

    const DISABLED_NOTE: &str = "No __schema in the response — introspection appears disabled or blocked (a finding in itself).";

    /// The answer for a response body: whether it is an error, and its text.
    fn describe(status: u16, body: &str) -> (bool, String) {
        match describe_response(status, body) {
            Ok(result) => match result.content.first() {
                Some(rever_tools_api::utils::Content::Text { text }) => {
                    (result.is_error, text.clone())
                }
                None => (result.is_error, String::new()),
            },
            Err(message) => (true, message),
        }
    }

    fn with_types(types: Value) -> String {
        json!({ "data": { "__schema": { "types": types } } }).to_string()
    }

    fn disabled(status: u16, server_errors: &str, raw: &str) -> String {
        format!(
            "{{\"introspectionEnabled\":false,\"status\":{status},\"note\":\"{DISABLED_NOTE}\",\"serverErrors\":{server_errors},\"raw\":{}}}",
            Value::from(raw)
        )
    }

    fn type_ref(kind: &str, name: Option<&str>, of_type: Value) -> Value {
        json!({ "kind": kind, "name": name, "ofType": of_type })
    }

    #[test]
    fn 스키마를_sdl로_재구성한다() {
        let id = type_ref(
            "NON_NULL",
            None,
            type_ref("SCALAR", Some("ID"), Value::Null),
        );
        let user = type_ref("OBJECT", Some("User"), Value::Null);
        let users = type_ref(
            "NON_NULL",
            None,
            type_ref("LIST", None, type_ref("NON_NULL", None, user.clone())),
        );
        let node_list = type_ref(
            "LIST",
            None,
            type_ref("INTERFACE", Some("Node"), Value::Null),
        );
        let string = type_ref("SCALAR", Some("String"), Value::Null);
        let body = json!({ "data": { "__schema": {
            "queryType": { "name": "Query" },
            "mutationType": { "name": "Mutation" },
            "types": [
                { "kind": "OBJECT", "name": "Query", "fields": [
                    { "name": "user", "args": [{ "name": "id" }, { "name": "withPosts" }], "type": user },
                    { "name": "users", "args": [], "type": users },
                    { "name": "search", "args": [{ "name": "q" }], "type": node_list }
                ] },
                { "kind": "OBJECT", "name": "Mutation", "fields": [
                    { "name": "login", "args": [{ "name": "input" }], "type": string }
                ] },
                { "kind": "OBJECT", "name": "User", "fields": [
                    { "name": "id", "args": [], "type": id },
                    { "name": "이름", "type": string }
                ] },
                { "kind": "INTERFACE", "name": "Node", "fields": [{ "name": "id", "args": [], "type": id }] },
                { "kind": "INPUT_OBJECT", "name": "LoginInput", "fields": null },
                { "kind": "INPUT_OBJECT", "name": "Other", "fields": [] },
                { "kind": "ENUM", "name": "Role", "fields": null },
                { "kind": "SCALAR", "name": "String" },
                { "kind": "UNION", "name": "U", "fields": [{ "name": "x" }] },
                { "kind": "OBJECT", "name": "__Schema", "fields": [{ "name": "types", "args": [], "type": string }] },
                { "kind": "OBJECT", "name": "", "fields": [] },
                { "kind": "OBJECT", "fields": [] }
            ]
        } } });
        // The text the TypeScript tool returned for the same response.
        let expected = r#"{
  "introspectionEnabled": true,
  "queryType": "Query",
  "mutationType": "Mutation",
  "typeCount": 9,
  "typeNames": [
    "OBJECT Query",
    "OBJECT Mutation",
    "OBJECT User",
    "INTERFACE Node",
    "INPUT_OBJECT LoginInput",
    "INPUT_OBJECT Other",
    "ENUM Role",
    "SCALAR String",
    "UNION U"
  ],
  "sdl": "type Query {\n  user(id, withPosts): User\n  users: [User!]!\n  search(q): [Node]\n}\n\ntype Mutation {\n  login(input): String\n}\n\ntype User {\n  id: ID!\n  이름: String\n}\n\ninterface Node {\n  id: ID!\n}\n\ninput Other {\n\n}"
}"#;
        assert_eq!(
            describe(200, &body.to_string()),
            (false, expected.to_owned())
        );
    }

    #[test]
    fn 인트로스펙션이_꺼져_있으면_한_줄_json으로_알린다() {
        // Numbers and key order of `serverErrors` are those of a JavaScript
        // round trip: integer keys first, 1.0 as 1, the big integer rounded.
        let body = r#"{"errors":[{"message":"introspection disabled","extensions":{"code":"X","n":1.0,"big":12345678901234567890,"2":"a","1":"b","f":1e21,"s":1e-7}}]}"#;
        let server_errors = r#"[{"message":"introspection disabled","extensions":{"1":"b","2":"a","code":"X","n":1,"big":12345678901234567000,"f":1e+21,"s":1e-7}}]"#;
        assert_eq!(
            describe(400, body),
            (false, disabled(400, server_errors, body))
        );
    }

    #[test]
    fn 스키마가_없거나_types가_거짓이면_꺼진_것으로_본다() {
        for body in [
            "null",
            "5",
            "[]",
            "\"just a string\"",
            r#"{"data":null}"#,
            r#"{"data":"x"}"#,
            r#"{"data":{"__schema":"abc"}}"#,
            r#"{"data":{"__schema":[]}}"#,
            r#"{"data":{"__schema":{"types":""}}}"#,
            r#"{"data":{"__schema":{"types":0}}}"#,
            r#"{"data":{"__schema":{"types":null}}}"#,
        ] {
            assert_eq!(
                describe(200, body),
                (false, disabled(200, "null", body)),
                "{body}"
            );
        }
    }

    #[test]
    fn 꺼진_응답의_raw는_본문_앞_400자다() {
        let body = format!("{{{}\"data\": {{}}, \"errors\": \"e\"}}", " ".repeat(500));
        let raw: String = body.chars().take(400).collect();
        assert_eq!(describe(200, &body), (false, disabled(200, "\"e\"", &raw)));
    }

    #[test]
    fn 꺼진_응답의_raw가_서로게이트_쌍을_자르면_반쪽을_이스케이프로_적는다() {
        // 394 code units, then emoji: the cut at 400 falls inside the fourth one.
        let body = format!("{{\"e\":\"{}{}\"}}", "x".repeat(387), "😀".repeat(20));
        let (failed, text) = describe(200, &body);
        assert!(!failed);
        let expected_raw = format!(
            "\"raw\":\"{{\\\"e\\\":\\\"{}😀😀😀\\ud83d\"}}",
            "x".repeat(387)
        );
        assert!(text.ends_with(&expected_raw), "{text}");
        // A cut between characters leaves nothing behind.
        let body = format!("{{\"e\":\"{}{}\"}}", "x".repeat(388), "😀".repeat(20));
        let expected_raw = format!("\"raw\":\"{{\\\"e\\\":\\\"{}😀😀😀\"}}", "x".repeat(388));
        assert!(describe(200, &body).1.ends_with(&expected_raw));
    }

    #[test]
    fn types가_비어_있어도_켜진_것으로_본다() {
        let expected = "{\n  \"introspectionEnabled\": true,\n  \"typeCount\": 0,\n  \"typeNames\": [],\n  \"sdl\": \"\"\n}";
        assert_eq!(
            describe(200, &with_types(json!([]))),
            (false, expected.to_owned())
        );
    }

    #[test]
    fn 같은_키가_두_번_나오면_뒤의_값을_쓴다() {
        let body = r#"{"data":{"__schema":{"types":[]}},"data":{"__schema":{"types":[{"kind":"ENUM","name":"Second"}]}}}"#;
        let (failed, text) = describe(200, body);
        assert!(!failed);
        assert!(text.contains("\"typeNames\": [\n    \"ENUM Second\"\n  ],"));
    }

    #[test]
    fn json이_아니면_상태와_본문_앞_300자를_돌려준다() {
        assert_eq!(
            describe(404, "nope"),
            (true, "response was not JSON (status 404): nope".to_owned())
        );
        assert_eq!(
            describe(204, ""),
            (true, "response was not JSON (status 204): ".to_owned())
        );
        // 300 UTF-16 code units, not bytes or characters.
        let body = format!("<html>{}", "가".repeat(400));
        let (failed, text) = describe(403, &body);
        assert!(failed);
        assert_eq!(
            text,
            format!(
                "response was not JSON (status 403): <html>{}",
                "가".repeat(294)
            )
        );
        // The cut falls inside the emoji, which is left out whole.
        let body = format!("{}😀{}", "x".repeat(299), "y".repeat(200));
        assert_eq!(
            describe(200, &body).1,
            format!("response was not JSON (status 200): {}", "x".repeat(299))
        );
    }

    #[test]
    fn 모양이_틀린_스키마는_자바스크립트의_오류_문구로_실패한다() {
        let object = |fields: Value| json!([{ "kind": "OBJECT", "name": "A", "fields": fields }]);
        let cases = [
            (json!(5), "schema.types.filter is not a function"),
            (json!("abc"), "schema.types.filter is not a function"),
            (json!(true), "schema.types.filter is not a function"),
            (json!({ "a": 1 }), "schema.types.filter is not a function"),
            (
                json!([{ "kind": "OBJECT", "name": "A", "fields": [] }, null]),
                "Cannot read properties of null (reading 'name')",
            ),
            (
                json!([{ "name": 5 }]),
                "t.name.startsWith is not a function",
            ),
            (
                json!([{ "name": true }]),
                "t.name.startsWith is not a function",
            ),
            (object(json!({})), "t.fields.map is not a function"),
            (object(json!(true)), "t.fields.map is not a function"),
            (
                json!([{ "kind": "INPUT_OBJECT", "name": "A", "fields": 5 }]),
                "t.fields.map is not a function",
            ),
            (
                json!([{ "kind": "INTERFACE", "name": "A", "fields": "abc" }]),
                "t.fields.map is not a function",
            ),
            (
                object(json!([null])),
                "Cannot read properties of null (reading 'args')",
            ),
            (
                object(json!([{ "name": "x", "args": "abc" }])),
                "f.args.map is not a function",
            ),
            (
                object(json!([{ "name": "x", "args": { "length": 2 } }])),
                "f.args.map is not a function",
            ),
            (
                object(json!([{ "name": "x", "args": [null] }])),
                "Cannot read properties of null (reading 'name')",
            ),
        ];
        for (types, message) in cases {
            assert_eq!(
                describe(200, &with_types(types.clone())),
                (true, message.to_owned()),
                "{types}"
            );
        }
    }

    #[test]
    fn 이상한_값은_자바스크립트의_문자열_변환대로_적는다() {
        // Written as text: the literal is too deep for the json! macro.
        let body = r#"{ "data": { "__schema": {
            "queryType": { "name": null }, "mutationType": "str", "extra": 1,
            "types": [
                5, "x", true, [], {}, { "name": 0 }, { "name": "NoKind", "fields": [1] },
                { "kind": 5, "name": "NumKind" }, { "kind": null, "name": "NullKind" },
                { "kind": [1, [2, null]], "name": "ArrKind" }, { "kind": { "a": 1 }, "name": "ObjKind" },
                { "kind": true, "name": "BoolKind" }, { "kind": 1.5e21, "name": "BigKind" },
                { "kind": "OBJECT", "name": "T", "fields": [
                    5, "str", true, [], {},
                    { "name": null, "type": null }, { "name": 5, "type": "" }, { "name": [1, 2], "type": 0 },
                    { "name": { "a": 1 }, "type": { "kind": "LIST" } },
                    { "name": "a", "args": 5, "type": { "kind": "NON_NULL", "ofType": { "kind": "LIST", "ofType": { "name": "X" } } } },
                    { "name": "b", "args": "", "type": { "name": 5 } },
                    { "name": "c", "args": {}, "type": { "name": [1, 2] } },
                    { "name": "d", "args": { "length": 0 }, "type": { "name": {} } },
                    { "name": "e", "args": true, "type": { "name": true } },
                    { "name": "f", "args": [5, "s", {}, { "name": null }, { "name": 7 }, { "name": [1, null] }, { "name": "ok" }, []], "type": { "name": 1e21 } },
                    { "name": "g", "type": { "kind": "NON_NULL" } },
                    { "name": "h", "type": { "kind": "LIST", "ofType": { "kind": "NON_NULL", "ofType": 5 } } },
                    { "name": "i", "type": "Str" }, { "name": "j", "type": [1] },
                    { "name": "k", "type": { "kind": "non_null", "name": "lower", "ofType": { "name": "Z" } } }
                ] },
                { "kind": "INTERFACE", "name": "I", "fields": [] }
            ]
        } } }"#;
        // The text the TypeScript tool returned for the same response.
        let expected = r#"{
  "introspectionEnabled": true,
  "queryType": null,
  "typeCount": 9,
  "typeNames": [
    "undefined NoKind",
    "5 NumKind",
    "null NullKind",
    "1,2, ArrKind",
    "[object Object] ObjKind",
    "true BoolKind",
    "1.5e+21 BigKind",
    "OBJECT T",
    "INTERFACE I"
  ],
  "sdl": "type T {\n  undefined: ?\n  undefined: ?\n  undefined: ?\n  undefined: ?\n  undefined: ?\n  null: ?\n  5: ?\n  1,2: ?\n  [object Object]: [?]\n  a: [X]!\n  b: 5\n  c: 1,2\n  d: [object Object]\n  e: true\n  f(, , , , 7, 1,, ok, ): 1e+21\n  g: ?!\n  h: [?!]\n  i: ?\n  j: ?\n  k: lower\n}\n\ninterface I {\n\n}"
}"#;
        assert_eq!(describe(200, body), (false, expected.to_owned()));
    }

    /// Answers one connection per response, in order, and returns the requests read.
    fn serve(responses: Vec<(u16, &'static str, Vec<u8>)>) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free local port");
        let port = listener.local_addr().expect("the bound address").port();
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, extra_header, body) in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut request = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let text = String::from_utf8_lossy(&request).to_lowercase();
                    if let Some((head, sent)) = text.split_once("\r\n\r\n") {
                        let length = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .and_then(|length| length.parse::<usize>().ok())
                            .unwrap_or(0);
                        if sent.len() >= length {
                            break;
                        }
                    }
                    match stream.read(&mut chunk) {
                        Ok(read) if read > 0 => request.extend_from_slice(&chunk[..read]),
                        _ => break,
                    }
                }
                requests.push(String::from_utf8_lossy(&request).into_owned());
                let head = format!(
                    "HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Length: {}\r\n{extra_header}\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
            requests
        });
        (format!("http://127.0.0.1:{port}"), handle)
    }

    const TINY_SCHEMA: &str = r#"{"errors":[{"message":"partial"}],"data":{"__schema":{"queryType":{"name":"Q"},"types":[{"kind":"OBJECT","name":"Q","fields":[{"name":"a","type":{"name":"Int"}}]}]}}}"#;
    const TINY_ANSWER: &str = "{\n  \"introspectionEnabled\": true,\n  \"queryType\": \"Q\",\n  \"typeCount\": 1,\n  \"typeNames\": [\n    \"OBJECT Q\"\n  ],\n  \"sdl\": \"type Q {\\n  a: Int\\n}\"\n}";

    #[tokio::test]
    async fn 인트로스펙션_쿼리를_post하고_응답을_해석한다() {
        let (base, server) = serve(vec![(200, "", TINY_SCHEMA.as_bytes().to_vec())]);
        let ctx = ToolContext::for_tests();
        let args = json!({ "url": format!("{base}/graphql") });
        assert_eq!(
            run(&GraphqlIntrospect, &ctx, args).await,
            (false, TINY_ANSWER.to_owned())
        );

        let requests = server.join().unwrap_or_default();
        let request = requests.first().map(String::as_str).unwrap_or("");
        assert!(
            request.starts_with("POST /graphql HTTP/1.1\r\n"),
            "{request}"
        );
        // The headers Node's fetch sent, and the body the TypeScript built.
        for line in [
            "content-type: application/json\r\n",
            "user-agent: node\r\n",
            "accept: */*\r\n",
            "accept-language: *\r\n",
            "sec-fetch-mode: cors\r\n",
            "content-length: 226\r\n",
        ] {
            assert!(request.contains(line), "{line:?} in {request}");
        }
        assert!(request.ends_with(
            "\r\n\r\n{\"query\":\"query IntrospectionQuery { __schema { queryType { name } mutationType { name } types { kind name fields { name args { name } type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } } } }\"}"
        ));
        assert!(!request.contains("authorization"));
    }

    #[tokio::test]
    async fn 캡처한_요청의_인증_헤더만_다시_쓴다() {
        let (base, server) = serve(vec![
            (200, "", b"{}".to_vec()),
            (200, "", b"{}".to_vec()),
            (200, "", b"{}".to_vec()),
        ]);
        let ctx = ToolContext::for_tests();
        store(
            &ctx,
            RequestPatch {
                request_headers: Some(headers(&[
                    ("AUTHORIZATION", "Bearer secret"),
                    ("cookie", "sid=1; theme=dark"),
                    ("X-Api-Key", "k-123"),
                    ("x-auth-token", "tok"),
                    ("x-csrf-token", "not reused"),
                    ("content-type", "text/plain"),
                    ("accept", "text/html"),
                ])),
                ..request("with-auth", "https://shop.test/api")
            },
        );
        store(&ctx, request("no-headers", "https://shop.test/other"));
        for id in ["with-auth", "no-headers", "unknown"] {
            let args = json!({ "url": base, "requestId": id });
            let (failed, text) = run(&GraphqlIntrospect, &ctx, args).await;
            assert!(!failed);
            assert_eq!(text, disabled(200, "null", "{}"));
        }

        let requests = server.join().unwrap_or_default();
        let [with_auth, no_headers, unknown] = requests.as_slice() else {
            panic!("three requests expected, got {}", requests.len());
        };
        for line in [
            "authorization: Bearer secret\r\n",
            "cookie: sid=1; theme=dark\r\n",
            "x-api-key: k-123\r\n",
            "x-auth-token: tok\r\n",
            "content-type: application/json\r\n",
            "accept: */*\r\n",
        ] {
            assert!(with_auth.contains(line), "{line:?} in {with_auth}");
        }
        assert!(!with_auth.contains("x-csrf-token"));
        assert!(!with_auth.contains("text/plain"));
        for request in [no_headers, unknown] {
            assert!(!request.contains("authorization"));
            assert!(!request.contains("cookie"));
        }
    }

    #[tokio::test]
    async fn 리다이렉트를_따라가고_bom과_깨진_utf8을_견딘다() {
        let with_bom = [b"\xEF\xBB\xBF".as_slice(), TINY_SCHEMA.as_bytes()].concat();
        let (base, server) = serve(vec![
            (302, "Location: /moved\r\n", Vec::new()),
            (200, "", with_bom),
            (500, "", b"oops \xff\xfe {\xe2\x82".to_vec()),
        ]);
        let ctx = ToolContext::for_tests();
        let args = json!({ "url": format!("{base}/graphql") });
        assert_eq!(
            run(&GraphqlIntrospect, &ctx, args.clone()).await,
            (false, TINY_ANSWER.to_owned())
        );
        assert_eq!(
            run(&GraphqlIntrospect, &ctx, args).await,
            (
                true,
                "response was not JSON (status 500): oops \u{fffd}\u{fffd} {\u{fffd}".to_owned()
            )
        );
        let requests = server.join().unwrap_or_default();
        assert!(
            requests
                .get(1)
                .is_some_and(|r| r.contains(" /moved HTTP/1.1\r\n"))
        );
    }

    #[tokio::test]
    async fn 요청을_보낼_수_없으면_fetch의_오류_문구로_실패한다() {
        let ctx = ToolContext::for_tests();
        // A port nothing listens on: bound, read, and released.
        let closed = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .map_or(1, |address| address.port());
        let cases = [
            ("not a url".to_owned(), "Failed to parse URL from not a url"),
            ("/relative".to_owned(), "Failed to parse URL from /relative"),
            (String::new(), "Failed to parse URL from "),
            ("ftp://127.0.0.1/x".to_owned(), "fetch failed"),
            (format!("http://127.0.0.1:{closed}/x"), "fetch failed"),
            (
                "https://u:p@127.0.0.1:1/x".to_owned(),
                "Request cannot be constructed from a URL that includes credentials: https://u:p@127.0.0.1:1/x",
            ),
            (
                "http://u@127.0.0.1:1/x".to_owned(),
                "Request cannot be constructed from a URL that includes credentials: http://u@127.0.0.1:1/x",
            ),
        ];
        for (url, message) in cases {
            assert_eq!(
                run(&GraphqlIntrospect, &ctx, json!({ "url": url })).await,
                (true, message.to_owned()),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn 헤더로_보낼_수_없는_값은_fetch의_오류_문구로_실패한다() {
        let ctx = ToolContext::for_tests();
        store(
            &ctx,
            RequestPatch {
                request_headers: Some(headers(&[("cookie", "a\nb")])),
                ..request("bad", "https://shop.test/api")
            },
        );
        let args = json!({ "url": "http://127.0.0.1:1/x", "requestId": "bad" });
        assert_eq!(
            run(&GraphqlIntrospect, &ctx, args).await,
            (
                true,
                "Headers.append: \"a\nb\" is an invalid header value.".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn url이_없거나_문자열이_아니면_검증_오류다() {
        let ctx = ToolContext::for_tests();
        let (failed, text) = run(&GraphqlIntrospect, &ctx, json!({})).await;
        assert!(failed);
        assert_eq!(
            text,
            "MCP error -32602: Input validation error: Invalid arguments for tool graphql_introspect: [\n  {\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"url\"\n    ],\n    \"message\": \"Invalid input: expected string, received undefined\"\n  }\n]"
        );
        let args = json!({ "url": 5, "requestId": 6 });
        let (failed, text) = run(&GraphqlIntrospect, &ctx, args).await;
        assert!(failed);
        assert_eq!(text.matches("expected string, received number").count(), 2);
    }
}
