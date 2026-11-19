//! Port of `src/main/mcp/tools/auth.ts` — `auth_dump`, `export_python_client`
//! and `export_client`.

use rever_analysis::codegen::{
    CodegenInput, CodegenLang, CodegenOptions, PythonLibrary, generate_code,
};
use rever_core::traffic::ListFilter;
use rever_tools_api::tool::{BoxFuture, Cookie, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Map, Value, json};
use url::Url;

use crate::args::validate;
use crate::js::{json_parse, json_stringify, to_js_string};
use crate::traffic;

const AUTH_HEADERS: [&str; 4] = ["authorization", "cookie", "x-csrf-token", "x-api-key"];

const STORAGE_EXPRESSION: &str = "JSON.stringify({
          ls: (() => { try { return Object.fromEntries(Object.entries(localStorage)) } catch(e) { return {} } })(),
          ss: (() => { try { return Object.fromEntries(Object.entries(sessionStorage)) } catch(e) { return {} } })()
        })";

/// A cookie with the field names and order of a Chrome DevTools Protocol
/// cookie, which is what the TypeScript printed.
fn cookie_json(cookie: &Cookie) -> Value {
    let mut out = json!({
        "name": cookie.name,
        "value": cookie.value,
        "domain": cookie.domain,
        "path": cookie.path,
        // The protocol marks a session cookie with -1.
        "expires": cookie.expires.unwrap_or(-1.0),
        "size": cookie.name.len() + cookie.value.len(),
        "httpOnly": cookie.http_only,
        "secure": cookie.secure,
        "session": cookie.expires.is_none(),
    });
    if let (Some(same_site), Some(out)) = (&cookie.same_site, out.as_object_mut()) {
        out.insert("sameSite".into(), json!(same_site));
    }
    out
}

/// `new URL(origin).host`, empty when the origin does not parse.
fn host_of_origin(origin: &str) -> String {
    let Ok(url) = Url::parse(origin) else {
        return String::new();
    };
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => String::new(),
    }
}

async fn auth_dump(ctx: &ToolContext, origin: Option<&str>) -> Result<String, String> {
    // Resolve origin
    let origin = match origin.filter(|origin| !origin.is_empty()) {
        Some(origin) => origin.to_owned(),
        None => match ctx.browser.evaluate("location.origin").await? {
            Value::String(origin) => origin,
            _ => String::new(),
        },
    };

    // Cookies
    let cookies = ctx.browser.cookies(&origin).await?;

    // localStorage + sessionStorage
    let storage = match ctx.browser.evaluate(STORAGE_EXPRESSION).await? {
        Value::Null => json!({ "ls": {}, "ss": {} }),
        value => json_parse(&to_js_string(Some(&value))).map_err(|e| e.to_string())?,
    };
    if storage.is_null() {
        return Err("Cannot read properties of null (reading 'ls')".into());
    }

    // Recent auth headers from the traffic store
    let mut header_hits = Vec::new();
    {
        let store = traffic(ctx);
        let recent = store.list(&ListFilter {
            host: Some(host_of_origin(&origin)),
            limit: Some(20),
            ..ListFilter::default()
        });
        for request in recent {
            let Some(headers) = &request.request_headers else {
                continue;
            };
            for header in AUTH_HEADERS {
                if let Some(value) = headers.get(header).filter(|value| !value.is_empty()) {
                    header_hits.push(json!({
                        "requestId": request.request_id,
                        "header": header,
                        "value": value,
                    }));
                }
            }
        }
    }

    let mut out = Map::new();
    out.insert("origin".into(), json!(origin));
    out.insert("cookies".into(), cookies.iter().map(cookie_json).collect());
    for (key, name) in [("ls", "localStorage"), ("ss", "sessionStorage")] {
        if let Some(entries) = storage.get(key) {
            out.insert(name.into(), entries.clone());
        }
    }
    out.insert("recentAuthHeaders".into(), Value::Array(header_hits));
    Ok(json_stringify(&Value::Object(out), true))
}

fn export(ctx: &ToolContext, args: &Value, options: CodegenOptions) -> ToolResult {
    let request_id = args.get("requestId").and_then(Value::as_str).unwrap_or("");
    let store = traffic(ctx);
    let Some(entry) = store.get(request_id) else {
        return err(format!("unknown requestId: {request_id}"));
    };
    let input = CodegenInput {
        method: &entry.method,
        url: &entry.url,
        request_headers: entry.request_headers.as_ref(),
        request_post_data: entry.request_post_data.as_deref(),
    };
    ok(generate_code(&input, &options))
}

fn python_library(args: &Value, name: &str) -> Option<PythonLibrary> {
    match args.get(name).and_then(Value::as_str) {
        Some("httpx") => Some(PythonLibrary::Httpx),
        Some(_) => Some(PythonLibrary::Requests),
        None => None,
    }
}

pub struct AuthDump;

impl Tool for AuthDump {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "auth_dump",
            "Dump authentication state for an origin: cookies, localStorage, sessionStorage, and recent auth-related request headers (Authorization, Cookie, X-CSRF-Token, X-API-Key).",
            json!({
                "type": "object",
                "properties": {
                    "origin": {
                        "description": "Origin URL (e.g. \"https://example.com\"). Defaults to active page origin.",
                        "type": "string"
                    }
                },
                "$schema": "http://json-schema.org/draft-07/schema#"
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(invalid) = validate(&self.def(), &args) {
                return invalid;
            }
            let origin = args.get("origin").and_then(Value::as_str);
            match auth_dump(ctx, origin).await {
                Ok(text) => ok(text),
                Err(message) => err(message),
            }
        })
    }
}

pub struct ExportPythonClient;

impl Tool for ExportPythonClient {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "export_python_client",
            "Generate a Python code snippet to reproduce a captured HTTP request using requests or httpx.",
            json!({
                "type": "object",
                "properties": {
                    "requestId": {
                        "type": "string",
                        "description": "requestId to reproduce"
                    },
                    "library": {
                        "description": "Python library to use (default: requests)",
                        "type": "string",
                        "enum": ["requests", "httpx"]
                    }
                },
                "required": ["requestId"],
                "$schema": "http://json-schema.org/draft-07/schema#"
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(invalid) = validate(&self.def(), &args) {
                return invalid;
            }
            // Delegate to the shared generator. Keep secrets visible here (runnable)
            // for back-compat; use export_client with maskSecrets for a shareable one.
            let options = CodegenOptions {
                lang: CodegenLang::Python,
                mask_secrets: Some(false),
                python_library: python_library(&args, "library"),
            };
            export(ctx, &args, options)
        })
    }
}

pub struct ExportClient;

impl Tool for ExportClient {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "export_client",
            "Generate code to reproduce a captured request in curl, Python (requests/httpx) or TypeScript (fetch). By default credential values are MASKED: auth headers, session-like cookies, token-like URL query params and JSON/form body fields (password, token, secret, …). Plain-text bodies are not inspected, so review before sharing. Pass maskSecrets=false for a runnable copy.",
            json!({
                "type": "object",
                "properties": {
                    "requestId": {
                        "type": "string",
                        "description": "requestId to reproduce"
                    },
                    "lang": {
                        "type": "string",
                        "enum": ["curl", "python", "typescript"],
                        "description": "Output language"
                    },
                    "maskSecrets": {
                        "description": "Mask credential values (default true — safe to share)",
                        "type": "boolean"
                    },
                    "pythonLibrary": {
                        "description": "Python only (default requests)",
                        "type": "string",
                        "enum": ["requests", "httpx"]
                    }
                },
                "required": ["requestId", "lang"],
                "$schema": "http://json-schema.org/draft-07/schema#"
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(invalid) = validate(&self.def(), &args) {
                return invalid;
            }
            let options = CodegenOptions {
                lang: match args.get("lang").and_then(Value::as_str) {
                    Some("curl") => CodegenLang::Curl,
                    Some("typescript") => CodegenLang::Typescript,
                    _ => CodegenLang::Python,
                },
                mask_secrets: args.get("maskSecrets").and_then(Value::as_bool),
                python_library: python_library(&args, "pythonLibrary"),
            };
            export(ctx, &args, options)
        })
    }
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(AuthDump);
    registry.register(ExportPythonClient);
    registry.register(ExportClient);
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use rever_core::traffic::RequestPatch;
    use rever_tools_api::tool::{Browser, unsupported};

    use super::*;
    use crate::testing::{headers, request, run, store};

    /// A page at `origin` with fixed cookies and storage; records what it was asked.
    #[derive(Default)]
    struct FakeBrowser {
        origin: &'static str,
        cookies: Vec<Cookie>,
        /// What the storage expression evaluates to.
        storage: Value,
        asked: Mutex<Vec<String>>,
    }

    impl FakeBrowser {
        fn record(&self, what: String) {
            self.asked
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(what);
        }
    }

    impl Browser for FakeBrowser {
        fn evaluate<'a>(&'a self, expression: &'a str) -> BoxFuture<'a, Result<Value, String>> {
            Box::pin(async move {
                if expression == "location.origin" {
                    self.record("origin".into());
                    return Ok(json!(self.origin));
                }
                self.record("storage".into());
                Ok(self.storage.clone())
            })
        }

        fn cookies<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Vec<Cookie>, String>> {
            Box::pin(async move {
                self.record(format!("cookies {url}"));
                Ok(self.cookies.clone())
            })
        }
    }

    fn cookie(name: &str, value: &str, domain: &str, path: &str) -> Cookie {
        Cookie {
            name: name.into(),
            value: value.into(),
            domain: domain.into(),
            path: path.into(),
            ..Cookie::default()
        }
    }

    fn shop_page() -> FakeBrowser {
        FakeBrowser {
            origin: "https://shop.test",
            cookies: vec![
                Cookie {
                    expires: Some(1893456000.123456),
                    http_only: true,
                    secure: true,
                    same_site: Some("Lax".into()),
                    ..cookie("sid", "s3cr3t", "shop.test", "/")
                },
                cookie("theme", "dark", ".shop.test", "/app"),
                Cookie {
                    expires: Some(1893456000.0),
                    secure: true,
                    same_site: Some("None".into()),
                    ..cookie("한", "값😀", "shop.test", "/")
                },
            ],
            storage: json!(r#"{"ls":{"b":"2","10":"x","2":"y","token":"t"},"ss":{}}"#),
            asked: Mutex::default(),
        }
    }

    fn captured(
        id: &str,
        url: &str,
        started_at: u64,
        request_headers: &[(&str, &str)],
    ) -> RequestPatch {
        RequestPatch {
            started_at: Some(started_at),
            request_headers: Some(headers(request_headers)),
            ..request(id, url)
        }
    }

    /// A context over `browser` with three captured requests on two hosts.
    fn context(browser: Arc<FakeBrowser>) -> ToolContext {
        let ctx = ToolContext::new(Arc::default(), browser, std::env::temp_dir());
        store(
            &ctx,
            captured(
                "ex1",
                "https://shop.test/api/login",
                4000,
                &[
                    ("accept", "*/*"),
                    ("authorization", "Bearer secret-token-value"),
                    ("cookie", "sid=s3cr3t; theme=dark"),
                    ("x-api-key", "k-123"),
                ],
            ),
        );
        // An empty value and a name that is not lowercase are not reported.
        store(
            &ctx,
            captured(
                "ex2",
                "https://shop.test/items",
                4001,
                &[
                    ("Authorization", "Upper"),
                    ("cookie", ""),
                    ("x-csrf-token", "csrf-1"),
                ],
            ),
        );
        store(
            &ctx,
            captured(
                "ex3",
                "https://other.test:8443/x",
                4002,
                &[("authorization", "Basic b3RoZXI=")],
            ),
        );
        store(&ctx, request("ex4", "https://shop.test/noheaders"));
        ctx
    }

    fn hits(text: &str) -> Vec<String> {
        let parsed: Value = serde_json::from_str(text).unwrap_or_default();
        parsed["recentAuthHeaders"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|hit| {
                format!(
                    "{} {}",
                    hit["requestId"].as_str().unwrap_or(""),
                    hit["header"].as_str().unwrap_or("")
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn 쿠키와_저장소와_최근_인증_헤더를_모아_돌려준다() {
        // The text the TypeScript tool returned for the same page and traffic.
        let expected = r#"{
  "origin": "https://shop.test",
  "cookies": [
    {
      "name": "sid",
      "value": "s3cr3t",
      "domain": "shop.test",
      "path": "/",
      "expires": 1893456000.123456,
      "size": 9,
      "httpOnly": true,
      "secure": true,
      "session": false,
      "sameSite": "Lax"
    },
    {
      "name": "theme",
      "value": "dark",
      "domain": ".shop.test",
      "path": "/app",
      "expires": -1,
      "size": 9,
      "httpOnly": false,
      "secure": false,
      "session": true
    },
    {
      "name": "한",
      "value": "값😀",
      "domain": "shop.test",
      "path": "/",
      "expires": 1893456000,
      "size": 10,
      "httpOnly": false,
      "secure": true,
      "session": false,
      "sameSite": "None"
    }
  ],
  "localStorage": {
    "2": "y",
    "10": "x",
    "b": "2",
    "token": "t"
  },
  "sessionStorage": {},
  "recentAuthHeaders": [
    {
      "requestId": "ex2",
      "header": "x-csrf-token",
      "value": "csrf-1"
    },
    {
      "requestId": "ex1",
      "header": "authorization",
      "value": "Bearer secret-token-value"
    },
    {
      "requestId": "ex1",
      "header": "cookie",
      "value": "sid=s3cr3t; theme=dark"
    },
    {
      "requestId": "ex1",
      "header": "x-api-key",
      "value": "k-123"
    }
  ]
}"#;
        let browser = Arc::new(shop_page());
        let ctx = context(browser.clone());
        assert_eq!(
            run(&AuthDump, &ctx, json!({})).await,
            (false, expected.to_owned())
        );
        // An empty origin falls back to the page's, like a missing one.
        assert_eq!(
            run(&AuthDump, &ctx, json!({ "origin": "" })).await,
            (false, expected.to_owned())
        );
        let asked = browser
            .asked
            .lock()
            .map(|asked| asked.clone())
            .unwrap_or_default();
        assert_eq!(
            asked.get(..3),
            Some(
                [
                    "origin".to_owned(),
                    "cookies https://shop.test".to_owned(),
                    "storage".to_owned()
                ]
                .as_slice()
            )
        );
    }

    #[tokio::test]
    async fn origin을_주면_페이지에_묻지_않고_그_호스트의_요청만_본다() {
        let browser = Arc::new(shop_page());
        let ctx = context(browser.clone());
        let args = json!({ "origin": "https://other.test:8443" });
        let (failed, text) = run(&AuthDump, &ctx, args).await;
        assert!(!failed);
        assert!(text.starts_with("{\n  \"origin\": \"https://other.test:8443\",\n"));
        assert_eq!(hits(&text), ["ex3 authorization"]);
        let asked = browser
            .asked
            .lock()
            .map(|asked| asked.clone())
            .unwrap_or_default();
        assert_eq!(asked, ["cookies https://other.test:8443", "storage"]);
    }

    #[tokio::test]
    async fn origin의_호스트는_url로_해석해_비교한다() {
        let ctx = context(Arc::new(shop_page()));
        // Upper case and the default port are normalized away: host "shop.test".
        let args = json!({ "origin": "https://SHOP.test:443/path?x=1" });
        let (_, text) = run(&AuthDump, &ctx, args).await;
        assert!(text.starts_with("{\n  \"origin\": \"https://SHOP.test:443/path?x=1\",\n"));
        assert_eq!(
            hits(&text),
            [
                "ex2 x-csrf-token",
                "ex1 authorization",
                "ex1 cookie",
                "ex1 x-api-key"
            ]
        );
        // Not a URL: no host filter at all.
        let (_, text) = run(&AuthDump, &ctx, json!({ "origin": "not a url" })).await;
        assert_eq!(
            hits(&text).first().map(String::as_str),
            Some("ex3 authorization")
        );
        assert_eq!(hits(&text).len(), 5);
    }

    #[tokio::test]
    async fn 최근_요청은_스무_개까지만_본다() {
        let ctx = context(Arc::new(shop_page()));
        for index in 0..25 {
            let id = format!("new{index}");
            let headers = [("authorization", "Bearer x")];
            store(
                &ctx,
                captured(&id, "https://shop.test/new", 5000 + index, &headers),
            );
        }
        let (_, text) = run(&AuthDump, &ctx, json!({})).await;
        let found = hits(&text);
        assert_eq!(found.len(), 20);
        assert_eq!(
            found.first().map(String::as_str),
            Some("new24 authorization")
        );
        assert_eq!(found.last().map(String::as_str), Some("new5 authorization"));
    }

    #[tokio::test]
    async fn 저장소_값이_없으면_빈_저장소로_적고_없는_키는_뺀다() {
        let empty = FakeBrowser {
            origin: "https://nomatch.test",
            ..FakeBrowser::default()
        };
        let ctx = context(Arc::new(empty));
        assert_eq!(
            run(&AuthDump, &ctx, json!({})).await,
            (
                false,
                "{\n  \"origin\": \"https://nomatch.test\",\n  \"cookies\": [],\n  \"localStorage\": {},\n  \"sessionStorage\": {},\n  \"recentAuthHeaders\": []\n}"
                    .to_owned()
            )
        );

        let only_session = FakeBrowser {
            origin: "https://nomatch.test",
            storage: json!(r#"{"ss":{"k":"v"}}"#),
            ..FakeBrowser::default()
        };
        let ctx = context(Arc::new(only_session));
        assert_eq!(
            run(&AuthDump, &ctx, json!({})).await.1,
            "{\n  \"origin\": \"https://nomatch.test\",\n  \"cookies\": [],\n  \"sessionStorage\": {\n    \"k\": \"v\"\n  },\n  \"recentAuthHeaders\": []\n}"
        );
    }

    #[tokio::test]
    async fn 저장소_값이_json이_아니면_오류다() {
        let broken = FakeBrowser {
            storage: json!("{not json"),
            ..FakeBrowser::default()
        };
        let (failed, _) = run(&AuthDump, &context(Arc::new(broken)), json!({})).await;
        assert!(failed);

        let null = FakeBrowser {
            storage: json!("null"),
            ..FakeBrowser::default()
        };
        assert_eq!(
            run(&AuthDump, &context(Arc::new(null)), json!({})).await,
            (
                true,
                "Cannot read properties of null (reading 'ls')".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn 브라우저가_못_하는_일은_그_오류를_그대로_돌려준다() {
        let ctx = ToolContext::for_tests();
        assert_eq!(
            run(&AuthDump, &ctx, json!({})).await,
            (true, unsupported("evaluate"))
        );
        assert_eq!(
            run(&AuthDump, &ctx, json!({ "origin": "https://shop.test" })).await,
            (true, unsupported("reading cookies"))
        );
    }

    #[tokio::test]
    async fn origin이_문자열이_아니면_검증_오류다() {
        let (failed, text) =
            run(&AuthDump, &ToolContext::for_tests(), json!({ "origin": 5 })).await;
        assert!(failed);
        assert_eq!(
            text,
            "MCP error -32602: Input validation error: Invalid arguments for tool auth_dump: [\n  {\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"origin\"\n    ],\n    \"message\": \"Invalid input: expected string, received number\"\n  }\n]"
        );
    }

    #[test]
    fn origin의_호스트는_자바스크립트의_url_host와_같다() {
        assert_eq!(host_of_origin("https://Example.COM"), "example.com");
        assert_eq!(host_of_origin("https://example.com:443/a"), "example.com");
        assert_eq!(
            host_of_origin("http://example.com:8080"),
            "example.com:8080"
        );
        assert_eq!(host_of_origin("http://user:pw@[::1]:3000/"), "[::1]:3000");
        assert_eq!(host_of_origin("example.com"), "");
        assert_eq!(host_of_origin(""), "");
    }

    /// A login POST whose headers are stored in the order the TypeScript saw them.
    fn login_request(ctx: &ToolContext) {
        store(
            ctx,
            RequestPatch {
                method: Some("POST".into()),
                request_post_data: Some(r#"{"user":"kim","password":"hunter2","n":1}"#.into()),
                ..captured(
                    "ex1",
                    "https://shop.test/api/login?token=abc123&q=1",
                    4000,
                    &[
                        ("accept", "*/*"),
                        ("authorization", "Bearer secret-token-value"),
                        ("content-type", "application/json"),
                        ("cookie", "sid=s3cr3t; theme=dark"),
                        ("x-api-key", "k-123"),
                    ],
                )
            },
        );
    }

    #[tokio::test]
    async fn 파이썬_스니펫은_비밀값을_가리지_않는다() {
        // The text the TypeScript tool returned for the same request.
        let expected = r#"import requests

s = requests.Session()
s.headers.update({
    "accept": "*/*",
    "authorization": "Bearer secret-token-value",
    "content-type": "application/json",
    "x-api-key": "k-123"
})
s.cookies.update({
    "sid": "s3cr3t",
    "theme": "dark"
})
data = "{\"user\":\"kim\",\"password\":\"hunter2\",\"n\":1}"
resp = s.post("https://shop.test/api/login?token=abc123&q=1", data=data)
print(resp.status_code, resp.text[:500])"#;
        let ctx = ToolContext::for_tests();
        login_request(&ctx);
        let args = json!({ "requestId": "ex1" });
        assert_eq!(
            run(&ExportPythonClient, &ctx, args).await,
            (false, expected.to_owned())
        );
        let args = json!({ "requestId": "ex1", "library": "requests" });
        assert_eq!(run(&ExportPythonClient, &ctx, args).await.1, expected);

        let args = json!({ "requestId": "ex1", "library": "httpx" });
        let (_, text) = run(&ExportPythonClient, &ctx, args).await;
        assert!(text.starts_with("import httpx\n\ns = httpx.Client()\n"));
        assert!(text.contains("\"authorization\": \"Bearer secret-token-value\""));
    }

    #[tokio::test]
    async fn 헤더도_본문도_없는_요청은_짧은_스니펫이_된다() {
        let ctx = ToolContext::for_tests();
        store(&ctx, request("ex4", "https://shop.test/noheaders"));
        assert_eq!(
            run(&ExportPythonClient, &ctx, json!({ "requestId": "ex4" })).await,
            (
                false,
                "import requests\n\ns = requests.Session()\nresp = s.get(\"https://shop.test/noheaders\")\nprint(resp.status_code, resp.text[:500])"
                    .to_owned()
            )
        );
    }

    #[tokio::test]
    async fn export_client는_기본으로_비밀값을_가린다() {
        // Each text is what the TypeScript tool returned for the same request.
        let masked_curl = r#"curl -X POST 'https://shop.test/api/login?token=****&q=1' \
  -H 'accept: */*' \
  -H 'authorization: Bear…alue (25 chars)' \
  -H 'content-type: application/json' \
  -H 'x-api-key: ****' \
  -b 'sid=****; theme=dark' \
  --data-raw '{"user":"kim","password":"****","n":1}'"#;
        let plain_curl = r#"curl -X POST 'https://shop.test/api/login?token=abc123&q=1' \
  -H 'accept: */*' \
  -H 'authorization: Bearer secret-token-value' \
  -H 'content-type: application/json' \
  -H 'x-api-key: k-123' \
  -b 'sid=s3cr3t; theme=dark' \
  --data-raw '{"user":"kim","password":"hunter2","n":1}'"#;
        let ctx = ToolContext::for_tests();
        login_request(&ctx);
        let args = json!({ "requestId": "ex1", "lang": "curl" });
        assert_eq!(
            run(&ExportClient, &ctx, args).await,
            (false, masked_curl.to_owned())
        );
        let args = json!({ "requestId": "ex1", "lang": "curl", "maskSecrets": true });
        assert_eq!(run(&ExportClient, &ctx, args).await.1, masked_curl);
        let args = json!({ "requestId": "ex1", "lang": "curl", "maskSecrets": false });
        assert_eq!(run(&ExportClient, &ctx, args).await.1, plain_curl);
    }

    #[tokio::test]
    async fn export_client는_언어와_파이썬_라이브러리를_고른다() {
        let typescript = r#"const res = await fetch("https://shop.test/api/login?token=****&q=1", {
  method: "POST",
  headers: {
    "accept": "*/*",
    "authorization": "Bear…alue (25 chars)",
    "content-type": "application/json",
    "x-api-key": "****",
    "Cookie": "sid=****; theme=dark"
  },
  credentials: 'include',
  body: "{\"user\":\"kim\",\"password\":\"****\",\"n\":1}"
})
console.log(res.status, await res.text())"#;
        let httpx = r#"import httpx

s = httpx.Client()
s.headers.update({
    "accept": "*/*",
    "authorization": "Bear…alue (25 chars)",
    "content-type": "application/json",
    "x-api-key": "****"
})
s.cookies.update({
    "sid": "****",
    "theme": "dark"
})
data = "{\"user\":\"kim\",\"password\":\"****\",\"n\":1}"
resp = s.post("https://shop.test/api/login?token=****&q=1", data=data)
print(resp.status_code, resp.text[:500])"#;
        let ctx = ToolContext::for_tests();
        login_request(&ctx);
        let args = json!({ "requestId": "ex1", "lang": "typescript" });
        assert_eq!(
            run(&ExportClient, &ctx, args).await,
            (false, typescript.to_owned())
        );
        let args = json!({ "requestId": "ex1", "lang": "python", "pythonLibrary": "httpx" });
        assert_eq!(
            run(&ExportClient, &ctx, args).await,
            (false, httpx.to_owned())
        );
        let args = json!({ "requestId": "ex1", "lang": "python" });
        let (_, text) = run(&ExportClient, &ctx, args).await;
        assert_eq!(
            text,
            httpx
                .replace("import httpx", "import requests")
                .replace("httpx.Client()", "requests.Session()")
        );
    }

    #[tokio::test]
    async fn 모르는_요청과_틀린_인자는_오류다() {
        let ctx = ToolContext::for_tests();
        let unknown = (true, "unknown requestId: nope".to_owned());
        let args = json!({ "requestId": "nope" });
        assert_eq!(run(&ExportPythonClient, &ctx, args).await, unknown);
        let args = json!({ "requestId": "nope", "lang": "curl" });
        assert_eq!(run(&ExportClient, &ctx, args).await, unknown);

        let args = json!({ "requestId": "ex1", "library": "urllib" });
        let (failed, text) = run(&ExportPythonClient, &ctx, args).await;
        assert!(failed);
        assert!(text.ends_with(
            "\"message\": \"Invalid option: expected one of \\\"requests\\\"|\\\"httpx\\\"\"\n  }\n]"
        ));

        // `lang` is required.
        let (failed, text) = run(&ExportClient, &ctx, json!({ "requestId": "ex1" })).await;
        assert!(failed);
        assert_eq!(
            text,
            "MCP error -32602: Input validation error: Invalid arguments for tool export_client: [\n  {\n    \"code\": \"invalid_value\",\n    \"values\": [\n      \"curl\",\n      \"python\",\n      \"typescript\"\n    ],\n    \"path\": [\n      \"lang\"\n    ],\n    \"message\": \"Invalid option: expected one of \\\"curl\\\"|\\\"python\\\"|\\\"typescript\\\"\"\n  }\n]"
        );
    }
}
