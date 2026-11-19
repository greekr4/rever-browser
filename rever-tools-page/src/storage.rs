//! Port of `src/main/mcp/tools/storage.ts` — cookies, localStorage and sessionStorage
//! of the active page.

use rever_tools_api::tool::{BoxFuture, Cookie, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err};
use serde_json::{Map, Value, json};

use crate::{Args, done, js_str, no_args, pretty, schema, validate};

pub fn register(registry: &mut ToolRegistry) {
    registry.register(CookieSet);
    registry.register(CookieDelete);
    registry.register(CookieList);
    for kind in [Kind::Local, Kind::Session] {
        for op in [Op::Get, Op::Set, Op::Delete, Op::Clear] {
            registry.register(StorageTool(kind, op));
        }
    }
}

/// The expression's value when it is a string, as `evalString` typed it.
async fn eval_string(ctx: &ToolContext, expr: &str) -> Result<Option<String>, String> {
    Ok(match ctx.browser.evaluate(expr).await? {
        Value::String(text) => Some(text),
        _ => None,
    })
}

fn string() -> Value {
    json!({ "type": "string" })
}

/// An optional argument the TypeScript only passed on when it was truthy.
fn non_empty(text: Option<&str>) -> Option<&str> {
    text.filter(|text| !text.is_empty())
}

/// A number as JSON.stringify prints it: no ".0" on a whole number.
fn js_number(number: f64) -> Value {
    if number.fract() == 0.0 && number.abs() < 9e15 {
        Value::from(number as i64)
    } else {
        Value::from(number)
    }
}

/// A cookie in the Chrome DevTools Protocol shape `cookie_list` always printed.
/// `priority`, `sameParty`, `sourceScheme` and `sourcePort` are not known to `Browser`.
fn cookie_json(cookie: &Cookie) -> Value {
    let mut out = Map::new();
    out.insert("name".to_owned(), json!(cookie.name));
    out.insert("value".to_owned(), json!(cookie.value));
    out.insert("domain".to_owned(), json!(cookie.domain));
    out.insert("path".to_owned(), json!(cookie.path));
    // The protocol marks a session cookie with -1.
    out.insert(
        "expires".to_owned(),
        cookie.expires.map_or(json!(-1), js_number),
    );
    out.insert(
        "size".to_owned(),
        json!(cookie.name.len() + cookie.value.len()),
    );
    out.insert("httpOnly".to_owned(), json!(cookie.http_only));
    out.insert("secure".to_owned(), json!(cookie.secure));
    out.insert("session".to_owned(), json!(cookie.expires.is_none()));
    if let Some(same_site) = &cookie.same_site {
        out.insert("sameSite".to_owned(), json!(same_site));
    }
    Value::Object(out)
}

tool!(
    CookieSet,
    ToolDef::new(
        "cookie_set",
        "Set or overwrite a cookie on the active page. Use to tamper with auth/session/level cookies for testing.",
        schema(
            json!({
                "name": string(),
                "value": string(),
                "url": {
                    "description": "Cookie URL/origin (defaults to active page origin)",
                    "type": "string"
                },
                "domain": string(),
                "path": string(),
                "secure": { "type": "boolean" },
                "httpOnly": { "type": "boolean" },
                "sameSite": { "type": "string", "enum": ["Strict", "Lax", "None"] },
                "expires": {
                    "description": "Unix seconds (omit for session)",
                    "type": "number"
                }
            }),
            &["name", "value"]
        )
    ),
    |ctx, args| {
        let name = args.str("name");
        let same_site = args.opt_str("sameSite");
        let cookie = Cookie {
            name: name.to_owned(),
            value: args.str("value").to_owned(),
            domain: args.opt_str("domain").unwrap_or_default().to_owned(),
            path: args.opt_str("path").unwrap_or_default().to_owned(),
            expires: args.opt_number("expires"),
            http_only: args.opt_bool("httpOnly").unwrap_or(false),
            secure: args.opt_bool("secure").unwrap_or(false),
            same_site: same_site.map(str::to_owned),
        };
        let url = match args.opt_str("url") {
            Some(url) => url.to_owned(),
            None => eval_string(ctx, "location.origin")
                .await?
                .unwrap_or_default(),
        };
        ctx.browser.set_cookie(&url, cookie).await?;
        Ok(pretty(&json!({ "name": name, "success": true })))
    }
);

tool!(
    CookieDelete,
    ToolDef::new(
        "cookie_delete",
        "Delete cookies by name (optionally scoped to url/domain/path).",
        schema(
            json!({ "name": string(), "url": string(), "domain": string(), "path": string() }),
            &["name"]
        )
    ),
    |ctx, args| {
        let name = args.str("name");
        let url = non_empty(args.opt_str("url"));
        let domain = non_empty(args.opt_str("domain"));
        let path = non_empty(args.opt_str("path"));
        // `Browser` deletes by URL only, so a domain and path become the URL they scope.
        let url = match (url, domain) {
            (Some(url), _) => url.to_owned(),
            (None, Some(domain)) => format!(
                "https://{}{}",
                domain.trim_start_matches('.'),
                path.unwrap_or("/")
            ),
            // What Chrome answered to Network.deleteCookies without either.
            (None, None) => {
                return Err("At least one of the url and domain needs to be specified".to_owned());
            }
        };
        ctx.browser.delete_cookie(&url, name).await?;
        Ok(format!("deleted cookie {name}"))
    }
);

tool!(
    CookieList,
    ToolDef::new(
        "cookie_list",
        "List cookies for given URLs (or the active origin).",
        schema(
            json!({ "urls": { "maxItems": 500, "type": "array", "items": string() } }),
            &[]
        )
    ),
    |ctx, args| {
        let urls: Vec<String> = match args.opt_strings("urls") {
            Some(urls) if !urls.is_empty() => urls.into_iter().map(str::to_owned).collect(),
            _ => vec![
                eval_string(ctx, "location.origin")
                    .await?
                    .unwrap_or_default(),
            ],
        };
        // One list for all URLs, like Network.getCookies: a cookie two URLs share is listed once.
        let mut cookies: Vec<Cookie> = Vec::new();
        for url in &urls {
            for cookie in ctx.browser.cookies(url).await? {
                if !cookies.contains(&cookie) {
                    cookies.push(cookie);
                }
            }
        }
        Ok(pretty(&Value::Array(
            cookies.iter().map(cookie_json).collect(),
        )))
    }
);

#[derive(Clone, Copy)]
pub enum Kind {
    Local,
    Session,
}

#[derive(Clone, Copy)]
pub enum Op {
    Get,
    Set,
    Delete,
    Clear,
}

/// One of the eight `local_storage_*` / `session_storage_*` tools.
pub struct StorageTool(pub Kind, pub Op);

impl StorageTool {
    fn storage(&self) -> &'static str {
        match self.0 {
            Kind::Local => "localStorage",
            Kind::Session => "sessionStorage",
        }
    }

    async fn run(&self, ctx: &ToolContext, args: &Args) -> Result<String, String> {
        let storage = self.storage();
        match self.1 {
            Op::Get => {
                let expr = format!("JSON.stringify(Object.fromEntries(Object.entries({storage})))");
                Ok(eval_string(ctx, &expr)
                    .await?
                    .unwrap_or_else(|| "{}".to_owned()))
            }
            Op::Set => {
                let (key, value) = (args.str("key"), args.str("value"));
                let expr = format!(
                    "({storage}.setItem({}, {}), 'ok')",
                    js_str(key),
                    js_str(value)
                );
                eval_string(ctx, &expr).await?;
                Ok(format!("{storage}.{key} set"))
            }
            Op::Delete => {
                let key = args.str("key");
                eval_string(
                    ctx,
                    &format!("({storage}.removeItem({}), 'ok')", js_str(key)),
                )
                .await?;
                Ok(format!("{storage}.{key} removed"))
            }
            Op::Clear => {
                eval_string(ctx, &format!("({storage}.clear(), 'ok')")).await?;
                Ok(format!("{storage} cleared"))
            }
        }
    }
}

impl Tool for StorageTool {
    fn def(&self) -> ToolDef {
        let storage = self.storage();
        let (description, schema) = match self.1 {
            Op::Get => (
                format!("Get all {storage} keys/values for the active page."),
                no_args(),
            ),
            Op::Set => (
                format!("Set a key in {storage}."),
                schema(
                    json!({ "key": string(), "value": string() }),
                    &["key", "value"],
                ),
            ),
            Op::Delete => (
                format!("Remove a key from {storage}."),
                schema(json!({ "key": string() }), &["key"]),
            ),
            Op::Clear => (
                format!("Clear all {storage} entries for the active page."),
                no_args(),
            ),
        };
        // Spelled out because the port scoreboard counts tools by these literals.
        match (self.0, self.1) {
            (Kind::Local, Op::Get) => ToolDef::new("local_storage_get", &description, schema),
            (Kind::Local, Op::Set) => ToolDef::new("local_storage_set", &description, schema),
            (Kind::Local, Op::Delete) => ToolDef::new("local_storage_delete", &description, schema),
            (Kind::Local, Op::Clear) => ToolDef::new("local_storage_clear", &description, schema),
            (Kind::Session, Op::Get) => ToolDef::new("session_storage_get", &description, schema),
            (Kind::Session, Op::Set) => ToolDef::new("session_storage_set", &description, schema),
            (Kind::Session, Op::Delete) => {
                ToolDef::new("session_storage_delete", &description, schema)
            }
            (Kind::Session, Op::Clear) => {
                ToolDef::new("session_storage_clear", &description, schema)
            }
        }
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(text) = validate(&self.def(), &args) {
                return err(text);
            }
            done(self.run(ctx, &Args(args)).await)
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{FakeBrowser, call, context, invalid, rejected};

    fn cookie(name: &str, expires: Option<f64>, same_site: Option<&str>) -> Cookie {
        Cookie {
            name: name.to_owned(),
            value: "v1".to_owned(),
            domain: ".a.test".to_owned(),
            path: "/".to_owned(),
            expires,
            http_only: true,
            secure: false,
            same_site: same_site.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn 쿠키를_주어진_url에_설정한다() {
        let (browser, ctx) = context(FakeBrowser::default());
        let args = json!({
            "name": "sid", "value": "abc", "url": "https://a.test/", "domain": ".a.test",
            "path": "/x", "secure": true, "httpOnly": true, "sameSite": "Lax", "expires": 1700000000
        });
        assert_eq!(
            call(&CookieSet, &ctx, args).await,
            Ok("{\n  \"name\": \"sid\",\n  \"success\": true\n}".to_owned())
        );
        let expected = Cookie {
            name: "sid".to_owned(),
            value: "abc".to_owned(),
            domain: ".a.test".to_owned(),
            path: "/x".to_owned(),
            expires: Some(1_700_000_000.0),
            http_only: true,
            secure: true,
            same_site: Some("Lax".to_owned()),
        };
        // An explicit URL needs no question to the page.
        assert_eq!(
            browser.calls(),
            [format!("set_cookie:https://a.test/:{expected:?}")]
        );
    }

    #[tokio::test]
    async fn url이_없으면_현재_페이지의_origin에_설정한다() {
        let (browser, ctx) = context(FakeBrowser::answering([Ok(json!("https://b.test"))]));
        assert!(
            call(&CookieSet, &ctx, json!({ "name": "k", "value": "" }))
                .await
                .is_ok()
        );
        let session = Cookie {
            name: "k".to_owned(),
            ..Cookie::default()
        };
        assert_eq!(
            browser.calls(),
            [
                "evaluate:location.origin".to_owned(),
                format!("set_cookie:https://b.test:{session:?}")
            ]
        );
    }

    #[tokio::test]
    async fn 쿠키_설정의_잘못된_인자를_거절한다() {
        let (browser, ctx) = context(FakeBrowser::default());
        for (args, key, expected, received) in [
            (json!({ "value": "v" }), "name", "string", "undefined"),
            (json!({ "name": "n" }), "value", "string", "undefined"),
            (
                json!({ "name": "n", "value": "v", "expires": "soon" }),
                "expires",
                "number",
                "string",
            ),
            (
                json!({ "name": "n", "value": "v", "secure": 1 }),
                "secure",
                "boolean",
                "number",
            ),
            (
                json!({ "name": "n", "value": "v", "url": null }),
                "url",
                "string",
                "null",
            ),
        ] {
            assert_eq!(
                call(&CookieSet, &ctx, args).await,
                invalid("cookie_set", key, expected, received)
            );
        }
        // The enum is case-sensitive, and null is not "absent".
        let same_site = json!({
            "code": "invalid_value", "values": ["Strict", "Lax", "None"], "path": ["sameSite"],
            "message": "Invalid option: expected one of \"Strict\"|\"Lax\"|\"None\""
        });
        for value in [json!("lax"), json!(null)] {
            assert_eq!(
                call(
                    &CookieSet,
                    &ctx,
                    json!({ "name": "n", "value": "v", "sameSite": value })
                )
                .await,
                rejected("cookie_set", json!([same_site]))
            );
        }
        assert!(browser.calls().is_empty());
    }

    #[tokio::test]
    async fn 쿠키_설정은_모든_문제를_스키마_순서로_한꺼번에_알린다() {
        let (_, ctx) = context(FakeBrowser::default());
        let args = json!({ "expires": "soon", "secure": 1, "url": null, "sameSite": 5 });
        // The text the TypeScript server answers to the same call.
        let wrong = |key: &str, expected: &str, received: &str| {
            json!({
                "expected": expected, "code": "invalid_type", "path": [key],
                "message": format!("Invalid input: expected {expected}, received {received}")
            })
        };
        assert_eq!(
            call(&CookieSet, &ctx, args).await,
            rejected(
                "cookie_set",
                json!([
                    wrong("name", "string", "undefined"),
                    wrong("value", "string", "undefined"),
                    wrong("url", "string", "null"),
                    wrong("secure", "boolean", "number"),
                    {
                        "code": "invalid_value", "values": ["Strict", "Lax", "None"],
                        "path": ["sameSite"],
                        "message": "Invalid option: expected one of \"Strict\"|\"Lax\"|\"None\""
                    },
                    wrong("expires", "number", "string")
                ])
            )
        );
    }

    #[tokio::test]
    async fn 엔진이_쿠키를_거절하면_오류_결과가_된다() {
        let (_, ctx) = context(FakeBrowser {
            cookie_error: Some("invalid cookie".to_owned()),
            ..FakeBrowser::default()
        });
        let args = json!({ "name": "n", "value": "v", "url": "https://a.test/" });
        assert_eq!(
            call(&CookieSet, &ctx, args).await,
            Err("invalid cookie".to_owned())
        );
        assert_eq!(
            call(
                &CookieDelete,
                &ctx,
                json!({ "name": "n", "url": "https://a.test/" })
            )
            .await,
            Err("invalid cookie".to_owned())
        );
        assert_eq!(
            call(&CookieList, &ctx, json!({ "urls": ["https://a.test/"] })).await,
            Err("invalid cookie".to_owned())
        );
    }

    #[tokio::test]
    async fn 쿠키를_url이나_도메인으로_지운다() {
        let (browser, ctx) = context(FakeBrowser::default());
        assert_eq!(
            call(
                &CookieDelete,
                &ctx,
                json!({ "name": "sid", "url": "https://a.test/x" })
            )
            .await,
            Ok("deleted cookie sid".to_owned())
        );
        assert!(
            call(
                &CookieDelete,
                &ctx,
                json!({ "name": "sid", "domain": ".a.test", "path": "/app" })
            )
            .await
            .is_ok()
        );
        assert!(
            call(
                &CookieDelete,
                &ctx,
                json!({ "name": "sid", "domain": "b.test" })
            )
            .await
            .is_ok()
        );
        assert_eq!(
            browser.calls(),
            [
                "delete_cookie:https://a.test/x:sid",
                "delete_cookie:https://a.test/app:sid",
                "delete_cookie:https://b.test/:sid"
            ]
        );
    }

    #[tokio::test]
    async fn url도_도메인도_없으면_쿠키를_지우지_않는다() {
        let (browser, ctx) = context(FakeBrowser::default());
        // An empty string counted as "not given" in the TypeScript.
        for args in [
            json!({ "name": "sid" }),
            json!({ "name": "sid", "url": "", "path": "/" }),
        ] {
            assert_eq!(
                call(&CookieDelete, &ctx, args).await,
                Err("At least one of the url and domain needs to be specified".to_owned())
            );
        }
        assert_eq!(
            call(&CookieDelete, &ctx, json!({})).await,
            invalid("cookie_delete", "name", "string", "undefined")
        );
        assert!(browser.calls().is_empty());
    }

    #[tokio::test]
    async fn 쿠키_목록을_프로토콜_모양으로_돌려주고_겹치는_쿠키는_한_번만_싣는다() {
        let (browser, ctx) = context(FakeBrowser {
            cookies: vec![
                cookie("sid", Some(1_700_000_000.0), Some("Lax")),
                cookie("tmp", None, None),
            ],
            ..FakeBrowser::default()
        });
        let args = json!({ "urls": ["https://a.test/", "https://a.test/x"] });
        assert_eq!(
            call(&CookieList, &ctx, args).await,
            Ok(concat!(
                "[\n",
                "  {\n",
                "    \"name\": \"sid\",\n",
                "    \"value\": \"v1\",\n",
                "    \"domain\": \".a.test\",\n",
                "    \"path\": \"/\",\n",
                "    \"expires\": 1700000000,\n",
                "    \"size\": 5,\n",
                "    \"httpOnly\": true,\n",
                "    \"secure\": false,\n",
                "    \"session\": false,\n",
                "    \"sameSite\": \"Lax\"\n",
                "  },\n",
                "  {\n",
                "    \"name\": \"tmp\",\n",
                "    \"value\": \"v1\",\n",
                "    \"domain\": \".a.test\",\n",
                "    \"path\": \"/\",\n",
                "    \"expires\": -1,\n",
                "    \"size\": 5,\n",
                "    \"httpOnly\": true,\n",
                "    \"secure\": false,\n",
                "    \"session\": true\n",
                "  }\n",
                "]"
            )
            .to_owned())
        );
        assert_eq!(
            browser.calls(),
            ["cookies:https://a.test/", "cookies:https://a.test/x"]
        );
    }

    #[tokio::test]
    async fn url이_없거나_비면_현재_origin의_쿠키를_나열한다() {
        for args in [json!({}), json!({ "urls": [] })] {
            let (browser, ctx) = context(FakeBrowser::answering([Ok(json!("https://b.test"))]));
            assert_eq!(call(&CookieList, &ctx, args).await, Ok("[]".to_owned()));
            assert_eq!(
                browser.calls(),
                ["evaluate:location.origin", "cookies:https://b.test"]
            );
        }
    }

    #[tokio::test]
    async fn url_목록은_문자열_500개까지만_받는다() {
        let (_, ctx) = context(FakeBrowser::default());
        assert!(
            call(&CookieList, &ctx, json!({ "urls": vec!["u"; 500] }))
                .await
                .is_ok()
        );
        let too_many = json!({
            "origin": "array", "code": "too_big", "maximum": 500, "inclusive": true,
            "path": ["urls"], "message": "Too big: expected array to have <=500 items"
        });
        let not_string = |index: usize| {
            json!({
                "expected": "string", "code": "invalid_type", "path": ["urls", index],
                "message": "Invalid input: expected string, received number"
            })
        };
        let mut mixed = vec![json!("u"); 500];
        mixed.push(json!(1));
        for (urls, issues) in [
            (json!(vec!["u"; 501]), json!([too_many])),
            (json!(["u", 1]), json!([not_string(1)])),
            // A bad item is reported before the length.
            (json!(mixed), json!([not_string(500), too_many])),
        ] {
            assert_eq!(
                call(&CookieList, &ctx, json!({ "urls": urls })).await,
                rejected("cookie_list", issues)
            );
        }
        assert_eq!(
            call(&CookieList, &ctx, json!({ "urls": "u" })).await,
            invalid("cookie_list", "urls", "array", "string")
        );
    }

    #[test]
    fn 만료_시각은_자바스크립트처럼_출력한다() {
        assert_eq!(js_number(1_700_000_000.0).to_string(), "1700000000");
        assert_eq!(js_number(1_700_000_000.5).to_string(), "1700000000.5");
        assert_eq!(js_number(f64::NAN), Value::Null);
    }

    #[test]
    fn 저장소_도구_여덟_개를_등록한다() {
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        let names: Vec<String> = registry.defs().into_iter().map(|def| def.name).collect();
        assert_eq!(
            names,
            [
                "cookie_set",
                "cookie_delete",
                "cookie_list",
                "local_storage_get",
                "local_storage_set",
                "local_storage_delete",
                "local_storage_clear",
                "session_storage_get",
                "session_storage_set",
                "session_storage_delete",
                "session_storage_clear"
            ]
        );
    }

    #[tokio::test]
    async fn 저장소_전체를_페이지가_만든_json_그대로_돌려준다() {
        let (browser, ctx) = context(FakeBrowser::answering([Ok(json!("{\"a\":\"1\"}"))]));
        assert_eq!(
            call(&StorageTool(Kind::Local, Op::Get), &ctx, json!({})).await,
            Ok("{\"a\":\"1\"}".to_owned())
        );
        assert_eq!(
            browser.calls(),
            ["evaluate:JSON.stringify(Object.fromEntries(Object.entries(localStorage)))"]
        );

        // No value from the page: an empty object.
        let (browser, ctx) = context(FakeBrowser::default());
        assert_eq!(
            call(&StorageTool(Kind::Session, Op::Get), &ctx, json!({})).await,
            Ok("{}".to_owned())
        );
        assert_eq!(
            browser.calls(),
            ["evaluate:JSON.stringify(Object.fromEntries(Object.entries(sessionStorage)))"]
        );
    }

    #[tokio::test]
    async fn 키를_설정하고_지우고_전체를_비운다() {
        let (browser, ctx) = context(FakeBrowser::default());
        let args = json!({ "key": "to\"ken", "value": "a\nb" });
        assert_eq!(
            call(&StorageTool(Kind::Local, Op::Set), &ctx, args).await,
            Ok("localStorage.to\"ken set".to_owned())
        );
        assert_eq!(
            call(
                &StorageTool(Kind::Session, Op::Delete),
                &ctx,
                json!({ "key": "k" })
            )
            .await,
            Ok("sessionStorage.k removed".to_owned())
        );
        assert_eq!(
            call(&StorageTool(Kind::Session, Op::Clear), &ctx, json!({})).await,
            Ok("sessionStorage cleared".to_owned())
        );
        assert_eq!(
            browser.calls(),
            [
                "evaluate:(localStorage.setItem(\"to\\\"ken\", \"a\\nb\"), 'ok')",
                "evaluate:(sessionStorage.removeItem(\"k\"), 'ok')",
                "evaluate:(sessionStorage.clear(), 'ok')"
            ]
        );
    }

    #[tokio::test]
    async fn 저장소_도구의_오류와_잘못된_인자() {
        let (_, ctx) = context(FakeBrowser::answering([Err("SecurityError".to_owned())]));
        assert_eq!(
            call(&StorageTool(Kind::Local, Op::Clear), &ctx, json!({})).await,
            Err("SecurityError".to_owned())
        );
        assert_eq!(
            call(
                &StorageTool(Kind::Local, Op::Set),
                &ctx,
                json!({ "key": "k" })
            )
            .await,
            invalid("local_storage_set", "value", "string", "undefined")
        );
        assert_eq!(
            call(
                &StorageTool(Kind::Session, Op::Delete),
                &ctx,
                json!({ "key": 1 })
            )
            .await,
            invalid("session_storage_delete", "key", "string", "number")
        );
    }
}
