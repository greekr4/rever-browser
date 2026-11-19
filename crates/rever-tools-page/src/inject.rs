//! Port of `src/main/mcp/tools/inject.ts` — JavaScript snippets saved per host glob
//! and registered as init scripts, plus a one-shot run in the current page.
//!
//! The TypeScript registered [`load_and_apply_injections`] as a hook the page layer
//! ran on every attach. There is no hook registry here: the app calls it itself.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use regex::Regex;
use rever_tools_api::tool::{ToolContext, ToolDef, ToolRegistry};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

use crate::{no_args, pretty, schema};

pub fn register(registry: &mut ToolRegistry) {
    registry.register(InjectAdd);
    registry.register(InjectList);
    registry.register(InjectRemove);
    registry.register(InjectToggle);
    registry.register(InjectRunNow);
}

// A field missing from the file reads as empty or false, as loosely as the
// TypeScript treated a hand-edited entry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Snippet {
    pub id: String,
    pub name: String,
    pub host_glob: String,
    pub code: String,
    pub enabled: bool,
}

#[derive(Default)]
pub struct InjectState {
    /// Snippet id → init script id. The TypeScript kept one such map per tab;
    /// `Browser` addresses a single tab.
    script_ids: Mutex<HashMap<String, String>>,
    /// Held while the snippets file is read, changed and written back. The TypeScript
    /// did that synchronously, so two calls could never interleave and lose a snippet.
    file: Mutex<()>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn snippets_path(ctx: &ToolContext) -> PathBuf {
    ctx.data_dir.join("injected-snippets.json")
}

/// A missing or unreadable file is an empty list.
fn load_snippets(ctx: &ToolContext) -> Vec<Snippet> {
    fs::read_to_string(snippets_path(ctx))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Load the snippets, apply `change`, and save them unless it failed.
fn change_snippets<T>(
    ctx: &ToolContext,
    change: impl FnOnce(&mut Vec<Snippet>) -> Result<T, String>,
) -> Result<T, String> {
    let state = ctx.state::<InjectState>();
    let _guard = lock(&state.file);
    let mut snippets = load_snippets(ctx);
    let out = change(&mut snippets)?;
    let text = serde_json::to_string_pretty(&snippets).map_err(|error| error.to_string())?;
    // Electron created the data folder at startup; nothing does that for us.
    fs::create_dir_all(&ctx.data_dir)
        .and_then(|()| fs::write(snippets_path(ctx), text))
        .map_err(|error| error.to_string())?;
    Ok(out)
}

/// The glob as the JavaScript regex the TypeScript built from it: `*` is any run of
/// characters, the characters it escaped are literal, and — because it did not
/// escape `?` — a `?` makes the character before it optional (or the `*` or `?`
/// before it lazy). A `?` with nothing to repeat made `new RegExp` throw there; the
/// regex crate would accept it, so such a glob is caught here and matches nothing.
fn host_matches(host_glob: &str, host: &str) -> bool {
    // What a `?` would apply to: nothing (0), an atom (1), or a quantifier (2).
    let mut repeatable = 0;
    let mut pattern = String::from("^");
    for ch in host_glob.chars() {
        match ch {
            '?' if repeatable == 0 => return false,
            '?' => {
                repeatable = if repeatable == 1 { 2 } else { 0 };
                pattern.push('?');
                continue;
            }
            '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\' => {
                pattern.push('\\');
                pattern.push(ch);
            }
            // JavaScript's `.` stops at these line terminators.
            '*' => {
                pattern.push_str(r"[^\n\r\u{2028}\u{2029}]*");
                repeatable = 2;
                continue;
            }
            _ => pattern.push(ch),
        }
        repeatable = 1;
    }
    pattern.push('$');
    Regex::new(&pattern).is_ok_and(|regex| regex.is_match(host))
}

/// `new URL(url).host`, or nothing for a URL that does not parse.
fn host_of(url: &str) -> String {
    let Ok(url) = Url::parse(url) else {
        return String::new();
    };
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => String::new(),
    }
}

async fn current_host(ctx: &ToolContext) -> String {
    match ctx.browser.page().await {
        Ok(page) => host_of(&page.url),
        Err(_) => String::new(),
    }
}

/// Register every enabled snippet whose glob matches the current page's host. Meant
/// to run each time the page is (re)attached.
pub async fn load_and_apply_injections(ctx: &ToolContext) {
    let snippets = load_snippets(ctx);
    let host = current_host(ctx).await;
    let state = ctx.state::<InjectState>();
    for snippet in snippets {
        if !snippet.enabled || !host_matches(&snippet.host_glob, &host) {
            continue;
        }
        // Remove an earlier registration first, so re-attaching does not pile up duplicates.
        let previous = lock(&state.script_ids).remove(&snippet.id);
        if let Some(previous) = previous {
            let _ = ctx.browser.remove_init_script(&previous).await;
        }
        match ctx.browser.add_init_script(&snippet.code).await {
            Ok(script_id) => {
                lock(&state.script_ids).insert(snippet.id, script_id);
            }
            Err(error) => eprintln!("[inject] failed to register snippet {} {error}", snippet.id),
        }
    }
}

tool!(
    InjectAdd,
    ToolDef::new(
        "inject_add",
        "Add a persistent JavaScript snippet that will be injected on every page load for matching hosts. Optionally registers immediately on the current page.",
        schema(
            json!({
                "name": { "type": "string", "description": "Human-readable name for this snippet" },
                "hostGlob": {
                    "type": "string",
                    "description": "Host glob pattern, e.g. \"*.example.com\" or \"api.example.com\""
                },
                "code": { "type": "string", "description": "JavaScript code to inject" },
                "enabled": {
                    "description": "Whether to enable immediately (default true)",
                    "type": "boolean"
                }
            }),
            &["name", "hostGlob", "code"]
        )
    ),
    |ctx, args| {
        let snippet = Snippet {
            id: Uuid::new_v4().to_string(),
            name: args.str("name").to_owned(),
            host_glob: args.str("hostGlob").to_owned(),
            code: args.str("code").to_owned(),
            enabled: args.opt_bool("enabled").unwrap_or(true),
        };
        change_snippets(ctx, |snippets| {
            snippets.push(snippet.clone());
            Ok(())
        })?;

        // Register immediately if current host matches
        if snippet.enabled {
            let host = current_host(ctx).await;
            if !host.is_empty() && host_matches(&snippet.host_glob, &host) {
                match ctx.browser.add_init_script(&snippet.code).await {
                    Ok(script_id) => {
                        lock(&ctx.state::<InjectState>().script_ids)
                            .insert(snippet.id.clone(), script_id);
                    }
                    Err(error) => eprintln!("[inject] immediate register failed: {error}"),
                }
            }
        }

        Ok(pretty(&json!({
            "id": snippet.id,
            "name": snippet.name,
            "hostGlob": snippet.host_glob,
            "enabled": snippet.enabled,
        })))
    }
);

tool!(
    InjectList,
    ToolDef::new(
        "inject_list",
        "List all saved injection snippets.",
        no_args()
    ),
    |ctx, _args| {
        let snippets = load_snippets(ctx);
        Ok(pretty(&Value::Array(
            snippets
                .iter()
                .map(|snippet| {
                    json!({
                        "id": snippet.id,
                        "name": snippet.name,
                        "hostGlob": snippet.host_glob,
                        "enabled": snippet.enabled,
                    })
                })
                .collect(),
        )))
    }
);

tool!(
    InjectRemove,
    ToolDef::new(
        "inject_remove",
        "Remove an injection snippet by ID.",
        schema(
            json!({ "id": { "type": "string", "description": "Snippet ID from inject_list" } }),
            &["id"]
        )
    ),
    |ctx, args| {
        let id = args.str("id");
        change_snippets(ctx, |snippets| {
            let index = snippets
                .iter()
                .position(|snippet| snippet.id == id)
                .ok_or_else(|| format!("unknown snippet id: {id}"))?;
            snippets.remove(index);
            Ok(())
        })?;

        // Remove from current target if registered
        let script_id = lock(&ctx.state::<InjectState>().script_ids).remove(id);
        if let Some(script_id) = script_id {
            let _ = ctx.browser.remove_init_script(&script_id).await;
        }

        Ok(format!("snippet {id} removed"))
    }
);

tool!(
    InjectToggle,
    ToolDef::new(
        "inject_toggle",
        "Enable or disable an injection snippet.",
        schema(
            json!({
                "id": { "type": "string", "description": "Snippet ID from inject_list" },
                "enabled": { "type": "boolean", "description": "New enabled state" }
            }),
            &["id", "enabled"]
        )
    ),
    |ctx, args| {
        let (id, enabled) = (args.str("id"), args.opt_bool("enabled").unwrap_or(false));
        change_snippets(ctx, |snippets| {
            let snippet = snippets
                .iter_mut()
                .find(|snippet| snippet.id == id)
                .ok_or_else(|| format!("unknown snippet id: {id}"))?;
            snippet.enabled = enabled;
            Ok(())
        })?;
        let state = if enabled { "enabled" } else { "disabled" };
        Ok(format!("snippet {id} {state}"))
    }
);

tool!(
    InjectRunNow,
    ToolDef::new(
        "inject_run_now",
        "Run a one-shot JavaScript snippet in the current page immediately (not persisted).",
        schema(
            json!({ "code": { "type": "string", "description": "JavaScript code to run now" } }),
            &["code"]
        )
    ),
    |ctx, args| {
        Ok(match ctx.browser.evaluate(args.str("code")).await? {
            Value::Null => "undefined".to_owned(),
            value => value.to_string(),
        })
    }
);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{FakeBrowser, call, context, invalid};

    fn on_page(url: &str) -> FakeBrowser {
        FakeBrowser {
            url: url.to_owned(),
            ..FakeBrowser::default()
        }
    }

    fn seed(ctx: &ToolContext, snippets: &str) {
        fs::create_dir_all(&ctx.data_dir).unwrap();
        fs::write(snippets_path(ctx), snippets).unwrap();
    }

    fn saved(ctx: &ToolContext) -> String {
        fs::read_to_string(snippets_path(ctx)).unwrap()
    }

    fn cleanup(ctx: &ToolContext) {
        let _ = fs::remove_dir_all(&ctx.data_dir);
    }

    const THREE: &str = r#"[
        {"id":"a","name":"on","hostGlob":"*.example.com","code":"A()","enabled":true},
        {"id":"b","name":"off","hostGlob":"*.example.com","code":"B()","enabled":false},
        {"id":"c","name":"elsewhere","hostGlob":"other.test","code":"C()","enabled":true}
    ]"#;

    #[test]
    fn 호스트_글롭은_별표만_와일드카드로_본다() {
        assert!(host_matches("*.example.com", "api.example.com"));
        assert!(host_matches("*.example.com", "a.b.example.com"));
        assert!(!host_matches("*.example.com", "example.com"));
        // The dot is literal, and the match covers the whole host.
        assert!(!host_matches("api.example.com", "apiXexample.com"));
        assert!(!host_matches("example.com", "example.com.evil.test"));
        assert!(!host_matches("example.com", "notexample.com"));
        assert!(host_matches("api.example.com", "api.example.com"));
        assert!(host_matches("*", ""));
        assert!(host_matches("*", "localhost:8777"));
        assert!(host_matches("localhost:*", "localhost:8777"));
        assert!(host_matches("a+b(c)[d]{1}|^$\\", "a+b(c)[d]{1}|^$\\"));
        assert!(!host_matches("Example.com", "example.com"));
    }

    #[test]
    fn 글롭의_물음표는_타입스크립트처럼_앞_글자를_선택으로_만든다() {
        // `?` was not escaped there, so it stayed a regex quantifier.
        assert!(host_matches("ab?.test", "a.test"));
        assert!(host_matches("ab?.test", "ab.test"));
        assert!(!host_matches("ab?.test", "ab?.test"));
    }

    #[test]
    fn 글롭은_노드에서_돌린_타입스크립트와_같은_답을_낸다() {
        // Answers of the TypeScript `hostMatches` under node; `None` is where it threw
        // "Nothing to repeat", which matches nothing here.
        for (glob, host, expected) in [
            ("?", "", None),
            ("?a", "a", None),
            ("a??", "a", Some(true)),
            ("a???", "a", None),
            ("*?", "abc", Some(true)),
            ("*??", "abc", None),
            ("a*?b", "ab", Some(true)),
            ("*?*", "x", Some(true)),
            ("??", "", None),
            ("a?*", "", Some(true)),
            ("a-b", "a-b", Some(true)),
            ("a b", "a b", Some(true)),
            ("a#b", "a#b", Some(true)),
            ("a&&b", "a&&b", Some(true)),
            ("a~~b", "a~~b", Some(true)),
            ("a--b", "a--b", Some(true)),
            ("<a>", "<a>", Some(true)),
            ("a!b=c", "a!b=c", Some(true)),
            ("a\"b", "a\"b", Some(true)),
            ("a@b%c", "a@b%c", Some(true)),
            ("a`b;c", "a`b;c", Some(true)),
            ("\t", "\t", Some(true)),
            ("é*", "éx", Some(true)),
            // `*` does not cross a line break, and `$` is the very end.
            ("*", "a\nb", Some(false)),
            ("*", "a\u{2028}b", Some(false)),
            ("a", "a\n", Some(false)),
        ] {
            assert_eq!(
                host_matches(glob, host),
                expected.unwrap_or(false),
                "{glob:?} {host:?}"
            );
        }
    }

    #[test]
    fn 호스트는_자바스크립트의_url_host와_같다() {
        assert_eq!(host_of("https://API.Example.com/x?y#z"), "api.example.com");
        assert_eq!(host_of("http://localhost:8777/a.html"), "localhost:8777");
        // A default port is not part of the host.
        assert_eq!(host_of("https://example.com:443/"), "example.com");
        assert_eq!(host_of("http://[::1]:8080/"), "[::1]:8080");
        assert_eq!(host_of("about:blank"), "");
        assert_eq!(host_of("not a url"), "");
        assert_eq!(host_of(""), "");
        // Answers of `new URL(url).host` under node.
        for (url, host) in [
            ("http://user:pass@host:8080/", "host:8080"),
            ("HTTP://EXAMPLE.com:80/", "example.com"),
            ("http://bücher.de/", "xn--bcher-kva.de"),
            ("file:///a/b", ""),
            ("view-source:https://a.test/", ""),
            ("chrome://settings/", "settings"),
            ("blob:https://a.test/uuid", ""),
            ("http://a.test:0/", "a.test:0"),
            ("https://a.test./", "a.test."),
            ("http://1.1/", "1.0.0.1"),
            ("javascript:void(0)", ""),
            ("http://[::1]/", "[::1]"),
            ("ws://a.test:443/", "a.test:443"),
            ("https://a.test:80/", "a.test:80"),
            ("data:text/html,hi", ""),
            ("  https://a.test/  ", "a.test"),
            ("https://a.test:65536/", ""),
            ("custom://Host:99/x", "Host:99"),
            ("http://a%20b.test/", ""),
        ] {
            assert_eq!(host_of(url), host, "{url}");
        }
    }

    #[tokio::test]
    async fn 스니펫을_저장하고_호스트가_맞으면_바로_등록한다() {
        let (browser, ctx) = context(on_page("https://api.example.com/x"));
        let args = json!({ "name": "hook", "hostGlob": "*.example.com", "code": "window.x = 1" });
        let text = call(&InjectAdd, &ctx, args).await.unwrap();
        let id = serde_json::from_str::<Value>(&text).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        // crypto.randomUUID(): a lowercase version 4 UUID.
        assert_eq!(
            Uuid::parse_str(&id).map(|uuid| uuid.get_version_num()),
            Ok(4)
        );
        assert_eq!(id, id.to_lowercase());
        assert_eq!(
            text,
            format!(
                "{{\n  \"id\": \"{id}\",\n  \"name\": \"hook\",\n  \"hostGlob\": \"*.example.com\",\n  \"enabled\": true\n}}"
            )
        );
        assert_eq!(
            saved(&ctx),
            format!(
                "[\n  {{\n    \"id\": \"{id}\",\n    \"name\": \"hook\",\n    \"hostGlob\": \"*.example.com\",\n    \"code\": \"window.x = 1\",\n    \"enabled\": true\n  }}\n]"
            )
        );
        assert_eq!(browser.calls(), ["add_init_script:window.x = 1"]);

        // The registration is remembered: removing the snippet unregisters it.
        assert_eq!(
            call(&InjectRemove, &ctx, json!({ "id": id })).await,
            Ok(format!("snippet {id} removed"))
        );
        assert_eq!(
            browser.calls().last().map(String::as_str),
            Some("remove_init_script:script-1")
        );
        assert_eq!(saved(&ctx), "[]");
        cleanup(&ctx);
    }

    #[tokio::test]
    async fn 호스트가_다르거나_꺼져_있거나_페이지가_없으면_저장만_한다() {
        for (browser, enabled) in [
            (on_page("https://other.test/"), true),
            (on_page("https://api.example.com/"), false),
            (FakeBrowser::default(), true),
            (on_page("about:blank"), true),
        ] {
            let (browser, ctx) = context(browser);
            let args = json!({ "name": "n", "hostGlob": "*.example.com", "code": "c", "enabled": enabled });
            let text = call(&InjectAdd, &ctx, args).await.unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&text).unwrap()["enabled"],
                json!(enabled)
            );
            assert!(browser.calls().is_empty());
            assert_eq!(load_snippets(&ctx).len(), 1);
            cleanup(&ctx);
        }
    }

    #[tokio::test]
    async fn 바로_등록이_실패해도_스니펫은_저장된다() {
        let (_, ctx) = context(FakeBrowser {
            fail_init_scripts: true,
            ..on_page("https://api.example.com/")
        });
        let args = json!({ "name": "n", "hostGlob": "*", "code": "c" });
        assert!(call(&InjectAdd, &ctx, args).await.is_ok());
        assert_eq!(load_snippets(&ctx).len(), 1);
        // Nothing was registered, so there is nothing to unregister on removal.
        assert!(lock(&ctx.state::<InjectState>().script_ids).is_empty());
        cleanup(&ctx);
    }

    #[tokio::test]
    async fn 스니펫을_추가할_때_잘못된_인자를_거절한다() {
        let (_, ctx) = context(FakeBrowser::default());
        for (args, key, expected, received) in [
            (
                json!({ "hostGlob": "*", "code": "c" }),
                "name",
                "string",
                "undefined",
            ),
            (
                json!({ "name": "n", "code": "c" }),
                "hostGlob",
                "string",
                "undefined",
            ),
            (
                json!({ "name": "n", "hostGlob": "*" }),
                "code",
                "string",
                "undefined",
            ),
            (
                json!({ "name": "n", "hostGlob": "*", "code": "c", "enabled": "yes" }),
                "enabled",
                "boolean",
                "string",
            ),
        ] {
            assert_eq!(
                call(&InjectAdd, &ctx, args).await,
                invalid("inject_add", key, expected, received)
            );
        }
        // Nothing was written for a rejected call.
        assert!(!snippets_path(&ctx).exists());
    }

    #[tokio::test]
    async fn 목록은_코드를_빼고_돌려준다() {
        let (_, ctx) = context(FakeBrowser::default());
        assert_eq!(
            call(&InjectList, &ctx, json!({})).await,
            Ok("[]".to_owned())
        );

        seed(&ctx, THREE);
        let text = call(&InjectList, &ctx, json!({})).await.unwrap();
        assert!(text.starts_with(
            "[\n  {\n    \"id\": \"a\",\n    \"name\": \"on\",\n    \"hostGlob\": \"*.example.com\",\n    \"enabled\": true\n  },"
        ));
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!([
                { "id": "a", "name": "on", "hostGlob": "*.example.com", "enabled": true },
                { "id": "b", "name": "off", "hostGlob": "*.example.com", "enabled": false },
                { "id": "c", "name": "elsewhere", "hostGlob": "other.test", "enabled": true }
            ])
        );
        cleanup(&ctx);
    }

    #[tokio::test]
    async fn 깨진_스니펫_파일은_빈_목록으로_본다() {
        let (_, ctx) = context(FakeBrowser::default());
        for broken in ["{not json", "{}", "[1, 2]"] {
            seed(&ctx, broken);
            assert_eq!(
                call(&InjectList, &ctx, json!({})).await,
                Ok("[]".to_owned())
            );
        }
        // An entry with fields missing still loads; what is missing is empty or false.
        seed(&ctx, r#"[{"id":"x","code":"c"}]"#);
        assert_eq!(
            load_snippets(&ctx),
            [Snippet {
                id: "x".to_owned(),
                code: "c".to_owned(),
                ..Snippet::default()
            }]
        );
        cleanup(&ctx);
    }

    #[tokio::test]
    async fn 스니펫을_지우고_모르는_id는_오류로_알린다() {
        let (browser, ctx) = context(FakeBrowser::default());
        seed(&ctx, THREE);
        assert_eq!(
            call(&InjectRemove, &ctx, json!({ "id": "b" })).await,
            Ok("snippet b removed".to_owned())
        );
        let left: Vec<String> = load_snippets(&ctx).into_iter().map(|s| s.id).collect();
        assert_eq!(left, ["a", "c"]);
        // Never registered on this page: nothing to unregister.
        assert!(browser.calls().is_empty());

        let before = saved(&ctx);
        assert_eq!(
            call(&InjectRemove, &ctx, json!({ "id": "b" })).await,
            Err("unknown snippet id: b".to_owned())
        );
        assert_eq!(saved(&ctx), before);
        assert_eq!(
            call(&InjectRemove, &ctx, json!({})).await,
            invalid("inject_remove", "id", "string", "undefined")
        );
        cleanup(&ctx);
    }

    #[tokio::test]
    async fn 스니펫을_켜고_끈다() {
        let (browser, ctx) = context(on_page("https://api.example.com/"));
        seed(&ctx, THREE);
        assert_eq!(
            call(&InjectToggle, &ctx, json!({ "id": "b", "enabled": true })).await,
            Ok("snippet b enabled".to_owned())
        );
        assert_eq!(
            call(&InjectToggle, &ctx, json!({ "id": "a", "enabled": false })).await,
            Ok("snippet a disabled".to_owned())
        );
        let enabled: Vec<bool> = load_snippets(&ctx).into_iter().map(|s| s.enabled).collect();
        assert_eq!(enabled, [false, true, true]);
        // Toggling only changes the saved state; it registers nothing by itself.
        assert!(browser.calls().is_empty());

        assert_eq!(
            call(&InjectToggle, &ctx, json!({ "id": "zz", "enabled": true })).await,
            Err("unknown snippet id: zz".to_owned())
        );
        assert_eq!(
            call(&InjectToggle, &ctx, json!({ "id": "a" })).await,
            invalid("inject_toggle", "enabled", "boolean", "undefined")
        );
        cleanup(&ctx);
    }

    #[tokio::test]
    async fn 코드를_지금_실행하고_값을_json으로_돌려준다() {
        let (browser, ctx) = context(FakeBrowser::answering([
            Ok(json!({ "a": [1, "x"] })),
            Ok(json!("text")),
            Ok(json!(false)),
            Ok(Value::Null),
            Err("ReferenceError: nope is not defined".to_owned()),
        ]));
        let run = |code: &'static str| call(&InjectRunNow, &ctx, json!({ "code": code }));
        assert_eq!(run("f()").await, Ok("{\"a\":[1,\"x\"]}".to_owned()));
        assert_eq!(run("g()").await, Ok("\"text\"".to_owned()));
        // Only null and undefined print as "undefined"; false is a value.
        assert_eq!(run("h()").await, Ok("false".to_owned()));
        assert_eq!(run("void 0").await, Ok("undefined".to_owned()));
        assert_eq!(
            run("nope").await,
            Err("ReferenceError: nope is not defined".to_owned())
        );
        assert_eq!(browser.calls()[0], "evaluate:f()");
        assert_eq!(
            call(&InjectRunNow, &ctx, json!({ "code": 1 })).await,
            invalid("inject_run_now", "code", "string", "number")
        );
    }

    #[tokio::test]
    async fn 붙을_때_켜져_있고_호스트가_맞는_스니펫만_등록한다() {
        let (browser, ctx) = context(on_page("https://api.example.com/"));
        seed(&ctx, THREE);
        load_and_apply_injections(&ctx).await;
        assert_eq!(browser.calls(), ["add_init_script:A()"]);

        // Attaching again replaces the earlier registration instead of adding a second.
        load_and_apply_injections(&ctx).await;
        assert_eq!(
            browser.calls(),
            [
                "add_init_script:A()",
                "remove_init_script:script-1",
                "add_init_script:A()"
            ]
        );
        assert_eq!(
            lock(&ctx.state::<InjectState>().script_ids).get("a"),
            Some(&"script-2".to_owned())
        );
        cleanup(&ctx);
    }

    #[tokio::test]
    async fn 붙을_때_등록이_실패해도_나머지_스니펫을_계속_시도한다() {
        let (browser, ctx) = context(FakeBrowser {
            fail_init_scripts: true,
            ..on_page("https://x.test/")
        });
        seed(
            &ctx,
            r#"[{"id":"a","hostGlob":"*","code":"A()","enabled":true},
                {"id":"b","hostGlob":"x.test","code":"B()","enabled":true}]"#,
        );
        load_and_apply_injections(&ctx).await;
        assert_eq!(
            browser.calls(),
            ["add_init_script:A()", "add_init_script:B()"]
        );
        assert!(lock(&ctx.state::<InjectState>().script_ids).is_empty());
        cleanup(&ctx);
    }
}
