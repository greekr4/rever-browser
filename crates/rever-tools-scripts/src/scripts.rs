//! Port of `src/main/mcp/tools/scripts.ts` — the bundle-analysis tools:
//! list_scripts, grep_script, grep_scripts, extract_context, detect_bundler and
//! deobfuscate_script, over the script bodies already in the traffic store.
//!
//! Sizes and offsets are UTF-16 code units, the TypeScript's string indices
//! (the tools call them bytes). A custom pattern is a JavaScript regex source;
//! it is translated for the `regex` crate, and what that engine cannot express
//! (lookaround, backreferences) is reported as an invalid regex.

use std::sync::LazyLock;

use regex::Regex;
use rever_analysis::script_analysis::{
    Category, GrepMatch, GrepOptions, ScriptFilter, detect_bundler, grep_body_safe, list_scripts,
    pattern_for_category, run_webcrack,
};
use rever_core::traffic::TrafficStore;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde::Serialize;
use serde_json::{Value, json};

use crate::args::{Args, MAX_SAFE_INTEGER};
use crate::js::{js_trim, utf16_len, utf16_slice};
use crate::js_regex::js_regex;
use crate::{ok_pretty, traffic};

const CATEGORIES: [&str; 11] = [
    "api",
    "urls",
    "env",
    "secrets",
    "auth",
    "hooks",
    "fetch",
    "ai",
    "baas",
    "function-calling",
    "rpc",
];

fn get_script_body(store: &TrafficStore, request_id: &str) -> Result<String, String> {
    let Some(entry) = store.get(request_id) else {
        return Err(format!("unknown requestId: {request_id}"));
    };
    let Some(body) = entry.response_body.as_deref().filter(|b| !b.is_empty()) else {
        return Err(format!(
            "requestId {request_id} has no captured body (still loading or skipped)"
        ));
    };
    if entry.response_body_base64 == Some(true) {
        return Err(format!(
            "requestId {request_id} body is base64 (binary) — not a text script"
        ));
    }
    Ok(body.to_owned())
}

enum Pattern {
    // The preset itself, not a copy: grep recognises the urls preset by
    // identity to filter its noise.
    Category(&'static Regex),
    Custom(Regex),
}

impl Pattern {
    fn regex(&self) -> &Regex {
        match self {
            Self::Category(regex) => regex,
            Self::Custom(regex) => regex,
        }
    }
}

fn build_pattern(category: Option<&str>, pattern: Option<&str>) -> Result<Pattern, String> {
    if let Some(pattern) = pattern.filter(|p| !p.is_empty()) {
        return js_regex(pattern, "g")
            .map(Pattern::Custom)
            .map_err(|e| format!("invalid regex: {e}"));
    }
    let Some(category) = category else {
        return Err("either pattern or category is required".into());
    };
    let category: Category =
        serde_json::from_value(Value::from(category)).map_err(|e| e.to_string())?;
    Ok(Pattern::Category(pattern_for_category(category)))
}

/// Runs a search that may block for seconds off the async runtime's threads.
async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|e| e.to_string())?
}

pub struct ListScripts;

impl Tool for ListScripts {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "list_scripts",
            "List JavaScript bundles captured from the browser (Script resourceType), largest body first. Use these requestIds with grep_script / extract_context / detect_bundler / deobfuscate_script.",
            json!({
                "type": "object",
                "properties": {
                    "host": { "type": "string", "description": "Substring host filter (e.g. \"vercel.app\")" },
                    "limit": { "type": "integer", "exclusiveMinimum": 0, "maximum": 200, "description": "Max items (default 30)" },
                    "minSize": { "type": "integer", "minimum": 0, "maximum": MAX_SAFE_INTEGER, "description": "Minimum body size in bytes" }
                }
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { list(ctx, &args).unwrap_or_else(err) })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScriptRow<'a> {
    request_id: &'a str,
    url: &'a str,
    host: &'a str,
    size: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime_type: Option<&'a str>,
    bundler: &'static str,
}

fn list(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("list_scripts", args)?;
    let filter = ScriptFilter {
        host: args.opt_str("host")?.map(str::to_owned),
        limit: Some(args.opt_int("limit", 1, Some(200))?.unwrap_or(30)),
        min_size: args.opt_int("minSize", 0, None)?.map(|size| size as u64),
    };
    let store = traffic(ctx);
    let rows: Vec<ScriptRow<'_>> = list_scripts(&store, &filter)
        .into_iter()
        .map(|r| {
            let body = r.response_body.as_deref().unwrap_or("");
            ScriptRow {
                request_id: &r.request_id,
                url: &r.url,
                host: &r.host,
                size: utf16_len(body),
                mime_type: r.mime_type.as_deref(),
                bundler: detect_bundler(body).name,
            }
        })
        .collect();
    Ok(ok_pretty(&rows))
}

pub struct GrepScript;

impl Tool for GrepScript {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "grep_script",
            "Grep a single captured script for a regex or a preset category. Returns matches with byte offsets and a small surrounding snippet — works on minified files where line-based grep fails.",
            json!({
                "type": "object",
                "properties": {
                    "requestId": { "type": "string", "description": "From list_scripts" },
                    "category": {
                        "type": "string",
                        "enum": CATEGORIES,
                        "description": "Preset pattern: api | urls | env | secrets | auth | hooks | fetch | ai | baas | function-calling | rpc"
                    },
                    "pattern": { "type": "string", "description": "Custom JS regex (overrides category). Always treated as global." },
                    "maxMatches": { "type": "integer", "exclusiveMinimum": 0, "maximum": 200, "description": "Default 20" },
                    "contextBefore": { "type": "integer", "minimum": 0, "maximum": 5000, "description": "Bytes before match (default 200)" },
                    "contextAfter": { "type": "integer", "minimum": 0, "maximum": 5000, "description": "Bytes after match (default 400)" }
                },
                "required": ["requestId"]
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { grep_one(ctx, &args).await.unwrap_or_else(err) })
    }
}

async fn grep_one(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("grep_script", args)?;
    let request_id = args.str("requestId")?;
    let category = args.opt_enum("category", &CATEGORIES)?;
    let pattern = args.opt_str("pattern")?;
    let opts = GrepOptions {
        max: args.opt_int("maxMatches", 1, Some(200))?.unwrap_or(20),
        before: args.opt_int("contextBefore", 0, Some(5000))?.unwrap_or(200),
        after: args.opt_int("contextAfter", 0, Some(5000))?.unwrap_or(400),
    };
    let body = get_script_body(&traffic(ctx), request_id)?;
    let pattern = build_pattern(category, pattern)?;
    let matches =
        blocking(move || grep_body_safe(&body, pattern.regex(), &opts).map_err(|e| e.to_string()))
            .await?;
    Ok(ok_pretty(&json!({
        "requestId": request_id,
        "totalShown": matches.len(),
        "matches": matches,
    })))
}

pub struct GrepScripts;

impl Tool for GrepScripts {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "grep_scripts",
            "Grep across ALL captured scripts. Use to locate a symbol/string when you do not yet know which bundle holds it. Returns match per script with byte offsets.",
            json!({
                "type": "object",
                "properties": {
                    "category": { "type": "string", "enum": CATEGORIES },
                    "pattern": { "type": "string", "description": "Custom JS regex (overrides category)" },
                    "host": { "type": "string", "description": "Restrict to scripts from this host substring" },
                    "maxMatchesPerScript": { "type": "integer", "exclusiveMinimum": 0, "maximum": 50, "description": "Default 5" },
                    "maxScripts": { "type": "integer", "exclusiveMinimum": 0, "maximum": 100, "description": "Default 30" }
                }
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { grep_all(ctx, &args).await.unwrap_or_else(err) })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScriptMatches {
    request_id: String,
    url: String,
    matches: Vec<GrepMatch>,
}

async fn grep_all(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("grep_scripts", args)?;
    let category = args.opt_enum("category", &CATEGORIES)?;
    let pattern = args.opt_str("pattern")?;
    let filter = ScriptFilter {
        host: args.opt_str("host")?.map(str::to_owned),
        limit: Some(args.opt_int("maxScripts", 1, Some(100))?.unwrap_or(30)),
        min_size: None,
    };
    let opts = GrepOptions {
        max: args
            .opt_int("maxMatchesPerScript", 1, Some(50))?
            .unwrap_or(5),
        before: 120,
        after: 200,
    };
    let pattern = build_pattern(category, pattern)?;
    // Copied out so the store is not locked while the searches run.
    let scripts: Vec<(String, String, String)> = list_scripts(&traffic(ctx), &filter)
        .into_iter()
        .map(|s| {
            let body = s.response_body.clone().unwrap_or_default();
            (s.request_id.clone(), s.url.clone(), body)
        })
        .collect();
    let scanned = scripts.len();
    let results = blocking(move || {
        let mut results = Vec::new();
        for (request_id, url, body) in scripts {
            let matches =
                grep_body_safe(&body, pattern.regex(), &opts).map_err(|e| e.to_string())?;
            if matches.is_empty() {
                continue;
            }
            results.push(ScriptMatches {
                request_id,
                url,
                matches,
            });
        }
        Ok(results)
    })
    .await?;
    Ok(ok_pretty(&json!({
        "scriptsScanned": scanned,
        "scriptsWithMatches": results.len(),
        "results": results,
    })))
}

pub struct ExtractContext;

impl Tool for ExtractContext {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "extract_context",
            "Return a byte-range slice from a captured script. Use after grep_script to read the function body around a hit (the dd-style trick for minified bundles).",
            json!({
                "type": "object",
                "properties": {
                    "requestId": { "type": "string" },
                    "byteOffset": { "type": "integer", "minimum": 0, "maximum": MAX_SAFE_INTEGER },
                    "before": { "type": "integer", "minimum": 0, "maximum": 50000, "description": "Default 500" },
                    "after": { "type": "integer", "minimum": 0, "maximum": 50000, "description": "Default 4000" }
                },
                "required": ["requestId", "byteOffset"]
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { extract(ctx, &args).unwrap_or_else(err) })
    }
}

fn extract(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("extract_context", args)?;
    let request_id = args.str("requestId")?;
    let byte_offset = args.int("byteOffset", 0, None)?;
    let before = args.opt_int("before", 0, Some(50_000))?.unwrap_or(500);
    let after = args.opt_int("after", 0, Some(50_000))?.unwrap_or(4000);
    let body = get_script_body(&traffic(ctx), request_id)?;
    let size = utf16_len(&body);
    let start = byte_offset.saturating_sub(before);
    let end = size.min(byte_offset.saturating_add(after));
    Ok(ok(format!(
        "# bytes {start}..{end} of requestId {request_id} (size {size})\n\n{}",
        utf16_slice(&body, start, end)
    )))
}

pub struct DetectBundler;

impl Tool for DetectBundler {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "detect_bundler",
            "Heuristically identify the bundler used to produce a captured script. Useful to decide whether deobfuscate_script will help (Webpack/Browserify yes, Vite/Rollup no).",
            json!({
                "type": "object",
                "properties": { "requestId": { "type": "string" } },
                "required": ["requestId"]
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { bundler(ctx, &args).unwrap_or_else(err) })
    }
}

fn bundler(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("detect_bundler", args)?;
    let body = get_script_body(&traffic(ctx), args.str("requestId")?)?;
    Ok(ok_pretty(&detect_bundler(&body)))
}

pub struct DeobfuscateScript;

impl Tool for DeobfuscateScript {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "deobfuscate_script",
            "Run the external `webcrack` CLI on a captured script and return the deobfuscated/unminified source. Only useful for Webpack/Browserify bundles — Vite/Rollup ESM output returns empty. Requires `webcrack` on PATH (npm i -g webcrack on Node 22).",
            json!({
                "type": "object",
                "properties": { "requestId": { "type": "string" } },
                "required": ["requestId"]
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { deobfuscate(ctx, &args).await.unwrap_or_else(err) })
    }
}

async fn deobfuscate(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("deobfuscate_script", args)?;
    let body = get_script_body(&traffic(ctx), args.str("requestId")?)?;
    let out = blocking(move || run_webcrack(&body)).await?;
    if js_trim(&out).is_empty() {
        return Ok(ok(
            "(webcrack returned empty output — likely a Vite/Rollup bundle)",
        ));
    }
    Ok(ok(out))
}

/// `/assets/…` paths a bundle spells out, deduped in source order.
static ASSET_REF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"/assets/[A-Za-z0-9_@][A-Za-z0-9_@./~-]{1,120}"#).expect("static asset pattern")
});

/// `import("…")` specifiers, static or dynamic — the lazy-chunk references.
static IMPORT_REF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"import\(\s*["']([^"']{1,200})["']"#).expect("dynamic import pattern")
});

/// What a bundle references but the traffic store has never seen: the
/// lazy-loaded surface of a code-split app (hashed chunks behind routes, auth
/// or scroll that the current session did not trigger).
pub struct BundleAssets;

impl Tool for BundleAssets {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "bundle_assets",
            "List asset URLs that captured bundles reference but were never fetched — the lazy-loaded surface of a code-split app (hashed chunks behind routes, auth or scroll). Scans captured script bodies for static /assets/… paths and import() specifiers, deduped, with the referencing script. Fetch the returned URLs with browser_navigate to capture them, then re-run to see the next layer.",
            json!({
                "type": "object",
                "properties": {
                    "host": { "type": "string", "description": "Substring host filter (e.g. \"vercel.app\")" },
                    "limit": { "type": "integer", "exclusiveMinimum": 0, "maximum": 200, "description": "Max refs returned (default 50)" }
                }
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { bundle_assets(ctx, &args).unwrap_or_else(err) })
    }
}

fn bundle_assets(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("bundle_assets", args)?;
    let host = args.opt_str("host")?.map(str::to_owned);
    let limit = args.opt_int("limit", 1, Some(200))?.unwrap_or(50) as usize;
    let store = traffic(ctx);
    let scripts = list_scripts(
        &store,
        &ScriptFilter {
            host,
            limit: Some(200),
            min_size: None,
        },
    );
    // A ref that matches any URL the store already holds (by suffix) is not
    // uncaptured: the chunk may have been fetched another way.
    let captured: Vec<&str> = store
        .list(&rever_core::traffic::ListFilter::default())
        .iter()
        .map(|row| row.url.as_str())
        .collect();
    let scanned = scripts.len();
    let mut refs: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    'outer: for entry in scripts {
        let Some(body) = entry.response_body.as_deref() else {
            continue;
        };
        let mut found: Vec<(&str, &'static str)> = Vec::new();
        for m in ASSET_REF_RE.find_iter(body) {
            found.push((m.as_str(), "asset"));
        }
        for caps in IMPORT_REF_RE.captures_iter(body) {
            let Some(spec) = caps.get(1) else { continue };
            let spec = spec.as_str();
            if spec.starts_with('/') || spec.starts_with("./") || spec.starts_with("../") {
                found.push((spec, "import"));
            }
        }
        for (reference, kind) in found {
            if captured.iter().any(|url| url.ends_with(reference)) {
                continue;
            }
            if !seen.insert((reference.to_owned(), kind)) {
                continue;
            }
            refs.push(json!({
                "ref": reference,
                "kind": kind,
                "referencedBy": entry.url,
            }));
            if refs.len() >= limit {
                break 'outer;
            }
        }
    }
    Ok(ok_pretty(&json!({
        "scriptsScanned": scanned,
        "uncapturedRefs": refs.len(),
        "refs": refs
    })))
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ListScripts);
    registry.register(BundleAssets);
    registry.register(GrepScript);
    registry.register(GrepScripts);
    registry.register(ExtractContext);
    registry.register(DetectBundler);
    registry.register(DeobfuscateScript);
}

#[cfg(test)]
mod tests {
    use rever_core::traffic::RequestPatch;
    use rever_tools_api::utils::Content;


    #[tokio::test]
    async fn lists_referenced_but_uncaptured_chunks() {
        let ctx = context(vec![
            script(
                "main",
                "http://a.test/assets/main.js",
                r#"import x from "/assets/known.js"; import("/assets/lazy-B.js"); "/assets/chunk-C.js""#,
            ),
            script("known", "http://a.test/assets/known.js", "// already here"),
        ]);
        let out = json_of(BundleAssets, &ctx, json!({})).await;
        assert_eq!(out["scriptsScanned"], 2);
        let refs: Vec<&str> = out["refs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["ref"].as_str().unwrap())
            .collect();
        assert!(refs.contains(&"/assets/lazy-B.js"), "{refs:?}");
        assert!(refs.contains(&"/assets/chunk-C.js"), "{refs:?}");
        assert!(!refs.contains(&"/assets/known.js"), "captured ref leaked");
    }

    #[tokio::test]
    async fn resolves_relative_import_specifiers() {
        let ctx = context(vec![script(
            "main",
            "http://a.test/assets/main.js",
            r#"import("./chunk-L.js")"#,
        )]);
        let out = json_of(BundleAssets, &ctx, json!({})).await;
        let refs: Vec<&str> = out["refs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["ref"].as_str().unwrap())
            .collect();
        assert!(refs.contains(&"./chunk-L.js"), "{refs:?}");
    }

    #[tokio::test]
    async fn filters_by_host() {
        let ctx = context(vec![
            script("a", "http://a.test/assets/a.js", r#""/assets/a-lazy.js""#),
            script("b", "http://b.test/assets/b.js", r#""/assets/b-lazy.js""#),
        ]);
        let out = json_of(BundleAssets, &ctx, json!({ "host": "a.test" })).await;
        assert_eq!(out["scriptsScanned"], 1);
        let refs: Vec<&str> = out["refs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["ref"].as_str().unwrap())
            .collect();
        assert_eq!(refs, ["/assets/a-lazy.js"]);
    }

    use super::*;

    fn script(id: &str, url: &str, body: &str) -> RequestPatch {
        RequestPatch {
            request_id: id.into(),
            url: Some(url.into()),
            resource_type: Some("Script".into()),
            mime_type: Some("application/javascript".into()),
            response_body: Some(body.into()),
            ..RequestPatch::default()
        }
    }

    fn context(patches: Vec<RequestPatch>) -> ToolContext {
        let ctx = ToolContext::for_tests();
        for (at, patch) in patches.into_iter().enumerate() {
            traffic(&ctx).upsert(patch, at as u64);
        }
        ctx
    }

    async fn call(tool: impl Tool, ctx: &ToolContext, args: Value) -> (bool, String) {
        let result = tool.call(ctx, args).await;
        let text = match result.content.first() {
            Some(Content::Text { text }) => text.clone(),
            None => String::new(),
        };
        (result.is_error, text)
    }

    async fn json_of(tool: impl Tool, ctx: &ToolContext, args: Value) -> Value {
        let (is_error, text) = call(tool, ctx, args).await;
        assert!(!is_error, "{text}");
        serde_json::from_str(&text).unwrap()
    }

    #[tokio::test]
    async fn 스크립트를_본문이_큰_순서로_번들러와_함께_나열한다() {
        let ctx = context(vec![
            script("small", "http://a.test/s.js", "var a = 1;"),
            script(
                "big",
                "http://b.test/b.js",
                "__webpack_require__(1); var 한글 = '😀';",
            ),
            RequestPatch {
                resource_type: Some("Document".into()),
                ..script("doc", "http://a.test/", "<html>")
            },
        ]);
        let (is_error, text) = call(ListScripts, &ctx, json!({})).await;
        assert!(!is_error);
        // `size` is the UTF-16 length: the emoji counts two.
        assert_eq!(
            text,
            r#"[
  {
    "requestId": "big",
    "url": "http://b.test/b.js",
    "host": "b.test",
    "size": 38,
    "mimeType": "application/javascript",
    "bundler": "webpack"
  },
  {
    "requestId": "small",
    "url": "http://a.test/s.js",
    "host": "a.test",
    "size": 10,
    "mimeType": "application/javascript",
    "bundler": "unknown"
  }
]"#
        );
    }

    #[tokio::test]
    async fn 스크립트_목록은_host와_limit과_min_size로_거른다() {
        let ctx = context(vec![
            script("s1", "http://a.test/1.js", "var a = 1;"),
            script("s2", "http://a.test/2.js", "var bb = 22;"),
            script("s3", "http://b.test/3.js", "var ccc = 333;"),
            RequestPatch {
                mime_type: None,
                ..script("s4", "http://c.test/4.js", "x")
            },
        ]);
        let ids = |rows: Value| -> Vec<String> {
            rows.as_array()
                .unwrap()
                .iter()
                .map(|row| row["requestId"].as_str().unwrap().to_owned())
                .collect()
        };
        let rows = json_of(ListScripts, &ctx, json!({ "host": "a.test" })).await;
        assert_eq!(ids(rows), ["s2", "s1"]);
        let rows = json_of(ListScripts, &ctx, json!({ "limit": 1 })).await;
        assert_eq!(ids(rows), ["s3"]);
        let rows = json_of(ListScripts, &ctx, json!({ "minSize": 12 })).await;
        assert_eq!(ids(rows), ["s3", "s2"]);
        // A script without a MIME type has no mimeType key at all.
        let rows = json_of(ListScripts, &ctx, json!({ "host": "c.test" })).await;
        assert_eq!(
            rows,
            json!([{ "requestId": "s4", "url": "http://c.test/4.js", "host": "c.test", "size": 1, "bundler": "unknown" }])
        );
        let (_, text) = call(ListScripts, &context(vec![]), Value::Null).await;
        assert_eq!(text, "[]");
    }

    #[tokio::test]
    async fn 범위를_벗어난_숫자_인자는_거절한다() {
        let ctx = context(vec![script("s1", "http://a.test/1.js", "var a = 1;")]);
        for args in [
            json!({ "limit": 0 }),
            json!({ "limit": 201 }),
            json!({ "limit": 1.5 }),
            json!({ "minSize": -1 }),
            json!({ "host": 3 }),
        ] {
            let (is_error, text) = call(ListScripts, &ctx, args).await;
            assert!(is_error);
            assert!(
                text.starts_with(
                    "Input validation error: Invalid arguments for tool list_scripts: "
                ),
                "{text}"
            );
        }
        for args in [
            json!({ "requestId": "s1", "pattern": "a", "maxMatches": 201 }),
            json!({ "requestId": "s1", "pattern": "a", "contextBefore": 5001 }),
            json!({ "requestId": "s1", "category": "nope" }),
            json!({ "pattern": "a" }),
        ] {
            assert!(call(GrepScript, &ctx, args).await.0);
        }
        assert!(
            call(
                GrepScripts,
                &ctx,
                json!({ "pattern": "a", "maxScripts": 101 })
            )
            .await
            .0
        );
        assert!(
            call(
                GrepScripts,
                &ctx,
                json!({ "pattern": "a", "maxMatchesPerScript": 51 })
            )
            .await
            .0
        );
        assert!(
            call(ExtractContext, &ctx, json!({ "requestId": "s1" }))
                .await
                .0
        );
        assert!(
            call(
                ExtractContext,
                &ctx,
                json!({ "requestId": "s1", "byteOffset": 0, "after": 50001 })
            )
            .await
            .0
        );
    }

    #[tokio::test]
    async fn 한_스크립트를_사용자_정규식으로_검색한다() {
        let ctx = context(vec![script(
            "s1",
            "http://a.test/1.js",
            "한글; const key = sign(payload); other();",
        )]);
        let (is_error, text) = call(
            GrepScript,
            &ctx,
            json!({ "requestId": "s1", "pattern": r"sign\(\w+\)", "contextBefore": 6, "contextAfter": 2 }),
        )
        .await;
        assert!(!is_error);
        // The offset is in UTF-16 units: "한글" counts two, not six.
        assert_eq!(
            text,
            r#"{
  "requestId": "s1",
  "totalShown": 1,
  "matches": [
    {
      "offset": 16,
      "match": "sign(payload)",
      "snippet": "key = sign(payload); "
    }
  ]
}"#
        );
    }

    #[tokio::test]
    async fn 한_스크립트를_프리셋_카테고리로_검색하고_개수를_제한한다() {
        let body = (0..30)
            .map(|i| format!("fetch('/api/v{i}/items-{i}');{}", " ".repeat(300)))
            .collect::<String>();
        let ctx = context(vec![script("s1", "http://a.test/1.js", &body)]);
        let found = json_of(
            GrepScript,
            &ctx,
            json!({ "requestId": "s1", "category": "fetch" }),
        )
        .await;
        assert_eq!(found["totalShown"], 20);
        assert_eq!(found["matches"][0]["match"], "fetch('/api/v0/items-0')");
        let found = json_of(
            GrepScript,
            &ctx,
            json!({ "requestId": "s1", "category": "fetch", "maxMatches": 3 }),
        )
        .await;
        assert_eq!(found["matches"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn 사용자_패턴이_카테고리보다_우선하고_빈_패턴은_없는_것으로_본다() {
        let ctx = context(vec![script(
            "s1",
            "http://a.test/1.js",
            "useState(); fetch('/api/items');",
        )]);
        let found = json_of(
            GrepScript,
            &ctx,
            json!({ "requestId": "s1", "category": "fetch", "pattern": "useState" }),
        )
        .await;
        assert_eq!(found["matches"][0]["match"], "useState");
        let found = json_of(
            GrepScript,
            &ctx,
            json!({ "requestId": "s1", "category": "hooks", "pattern": "" }),
        )
        .await;
        assert_eq!(found["matches"][0]["match"], "useState");
    }

    #[tokio::test]
    async fn urls_프리셋은_잡음_url을_걸러낸다() {
        let ctx = context(vec![script(
            "s1",
            "http://a.test/1.js",
            r#"a="http://www.w3.org/2000/svg" b="https://api.example.com/v1/sign""#,
        )]);
        let found = json_of(
            GrepScript,
            &ctx,
            json!({ "requestId": "s1", "category": "urls" }),
        )
        .await;
        assert_eq!(found["totalShown"], 1);
        assert_eq!(
            found["matches"][0]["match"],
            "https://api.example.com/v1/sign"
        );
    }

    #[tokio::test]
    async fn 본문을_읽을_수_없는_요청은_이유를_알린다() {
        let ctx = context(vec![
            RequestPatch {
                response_body: None,
                ..script("pending", "http://a.test/p.js", "")
            },
            RequestPatch {
                response_body_base64: Some(true),
                ..script("binary", "http://a.test/b.js", "AAAA")
            },
        ]);
        let expected = [
            ("nope", "unknown requestId: nope"),
            (
                "pending",
                "requestId pending has no captured body (still loading or skipped)",
            ),
            (
                "binary",
                "requestId binary body is base64 (binary) — not a text script",
            ),
        ];
        for (id, message) in expected {
            let args = json!({ "requestId": id, "pattern": "a", "byteOffset": 0 });
            assert_eq!(
                call(GrepScript, &ctx, args.clone()).await,
                (true, message.to_owned())
            );
            assert_eq!(
                call(ExtractContext, &ctx, args.clone()).await,
                (true, message.to_owned())
            );
            assert_eq!(
                call(DetectBundler, &ctx, args.clone()).await,
                (true, message.to_owned())
            );
            assert_eq!(
                call(DeobfuscateScript, &ctx, args).await,
                (true, message.to_owned())
            );
        }
    }

    #[tokio::test]
    async fn 패턴도_카테고리도_없거나_정규식이_잘못되면_에러다() {
        let ctx = context(vec![script("s1", "http://a.test/1.js", "var a = 1;")]);
        assert_eq!(
            call(GrepScript, &ctx, json!({ "requestId": "s1" })).await,
            (true, "either pattern or category is required".to_owned())
        );
        assert_eq!(
            call(GrepScripts, &ctx, json!({})).await,
            (true, "either pattern or category is required".to_owned())
        );
        let (is_error, text) = call(
            GrepScript,
            &ctx,
            json!({ "requestId": "s1", "pattern": "(" }),
        )
        .await;
        assert!(is_error);
        assert!(
            text.starts_with("invalid regex: Invalid regular expression: /(/g: "),
            "{text}"
        );
        assert_eq!(
            call(GrepScripts, &ctx, json!({ "pattern": "a(?=b)" })).await,
            (
                true,
                "invalid regex: Invalid regular expression: /a(?=b)/g: lookahead is not supported by this regex engine"
                    .to_owned()
            )
        );
        assert_eq!(
            call(GrepScript, &ctx, json!({ "requestId": "s1", "pattern": r"(a)\1" })).await,
            (
                true,
                r"invalid regex: Invalid regular expression: /(a)\1/g: backreferences are not supported by this regex engine"
                    .to_owned()
            )
        );
        // The body is looked up before the pattern is compiled.
        assert_eq!(
            call(
                GrepScript,
                &ctx,
                json!({ "requestId": "nope", "pattern": "(" })
            )
            .await,
            (true, "unknown requestId: nope".to_owned())
        );
    }

    #[tokio::test]
    async fn 모든_스크립트를_검색해_맞은_스크립트만_돌려준다() {
        let ctx = context(vec![
            script("s1", "http://a.test/1.js", "const a = hmacSign(x);"),
            script("s2", "http://a.test/2.js", "nothing to see here at all"),
            script("s3", "http://b.test/3.js", "hmacSign(1);hmacSign(2)"),
        ]);
        let (is_error, text) = call(GrepScripts, &ctx, json!({ "pattern": "hmacSign" })).await;
        assert!(!is_error);
        assert_eq!(
            text,
            r#"{
  "scriptsScanned": 3,
  "scriptsWithMatches": 2,
  "results": [
    {
      "requestId": "s3",
      "url": "http://b.test/3.js",
      "matches": [
        {
          "offset": 0,
          "match": "hmacSign",
          "snippet": "hmacSign(1);hmacSign(2)"
        }
      ]
    },
    {
      "requestId": "s1",
      "url": "http://a.test/1.js",
      "matches": [
        {
          "offset": 10,
          "match": "hmacSign",
          "snippet": "const a = hmacSign(x);"
        }
      ]
    }
  ]
}"#
        );
        let found = json_of(
            GrepScripts,
            &ctx,
            json!({ "pattern": "hmacSign", "host": "a.test", "maxScripts": 1 }),
        )
        .await;
        assert_eq!(found["scriptsScanned"], 1);
        let found = json_of(GrepScripts, &ctx, json!({ "category": "hooks" })).await;
        assert_eq!(
            found,
            json!({ "scriptsScanned": 3, "scriptsWithMatches": 0, "results": [] })
        );
    }

    #[tokio::test]
    async fn 오프셋_주변의_본문을_utf16_단위로_잘라_돌려준다() {
        let ctx = context(vec![script(
            "s1",
            "http://a.test/1.js",
            "가나다라0123456789",
        )]);
        let slice = |args: Value| {
            let ctx = &ctx;
            async move { call(ExtractContext, ctx, args).await.1 }
        };
        assert_eq!(
            slice(json!({ "requestId": "s1", "byteOffset": 4, "before": 2, "after": 3 })).await,
            "# bytes 2..7 of requestId s1 (size 14)\n\n다라012"
        );
        // Defaults reach past both ends and are clamped.
        assert_eq!(
            slice(json!({ "requestId": "s1", "byteOffset": 4 })).await,
            "# bytes 0..14 of requestId s1 (size 14)\n\n가나다라0123456789"
        );
        // Past the end: an empty slice, with the range as computed.
        assert_eq!(
            slice(json!({ "requestId": "s1", "byteOffset": 100, "before": 10 })).await,
            "# bytes 90..14 of requestId s1 (size 14)\n\n"
        );
    }

    #[tokio::test]
    async fn 번들러를_이름과_근거로_알린다() {
        let ctx = context(vec![
            script("v", "http://a.test/v.js", "import.meta.env.MODE"),
            script("u", "http://a.test/u.js", "var a = 1;"),
        ]);
        assert_eq!(
            call(DetectBundler, &ctx, json!({ "requestId": "v" })).await,
            (
                false,
                "{\n  \"name\": \"vite\",\n  \"signature\": \"import.meta / __vite__\"\n}"
                    .to_owned()
            )
        );
        assert_eq!(
            json_of(DetectBundler, &ctx, json!({ "requestId": "u" })).await,
            json!({ "name": "unknown", "signature": "" })
        );
    }

    #[tokio::test]
    async fn webcrack이_없으면_설치_안내를_있으면_결과를_돌려준다() {
        let ctx = context(vec![script("s1", "http://a.test/1.js", "var a = 1;")]);
        let (is_error, text) = call(DeobfuscateScript, &ctx, json!({ "requestId": "s1" })).await;
        // Whether the CLI is installed is the machine's business; either way
        // the tool answers with text and does not panic.
        if is_error {
            assert!(text.contains("webcrack"), "{text}");
        } else {
            assert!(!text.is_empty());
        }
    }
}
