//! Port of `src/main/mcp/tools/deob.ts` — deob_detect and deob_auto: guess
//! which packer obfuscated a captured script and run the matching unpackers.
//!
//! Byte counts are UTF-16 code units, the TypeScript's `string.length`.

use std::sync::LazyLock;

use regex::Regex;
use rever_analysis::script_analysis::run_webcrack;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err};
use serde::Serialize;
use serde_json::{Value, json};

use crate::args::Args;
use crate::js::{JS_SPACE, is_js_space, utf16_len, utf16_slice};
use crate::{ok_pretty, traffic};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DetectResult {
    /// jsfuck | aaencode | jjencode | packer | hex-array | webpack | eval-chain | unknown
    pub packer: &'static str,
    // A JSON number as JavaScript prints it: 0.9, and 0 rather than 0.0.
    pub confidence: Value,
    pub evidence: Vec<String>,
}

fn found(packer: &'static str, confidence: f64, evidence: &[&str]) -> DetectResult {
    DetectResult {
        packer,
        confidence: json!(confidence),
        evidence: evidence.iter().map(|e| (*e).to_owned()).collect(),
    }
}

struct Patterns {
    packer: Regex,
    hex_array: Regex,
    eval_call: Regex,
}

static PATTERNS: LazyLock<Patterns> = LazyLock::new(|| {
    let compile = |source: String| Regex::new(&source).expect("the patterns are constants");
    Patterns {
        packer: compile(format!(r"eval\({JS_SPACE}*function\(p,a,c,k,e,[dr]\)")),
        hex_array: compile(format!(
            r"var{JS_SPACE}+(_0x[a-f0-9]+){JS_SPACE}*={JS_SPACE}*\[[^\]]*?\][;,]"
        )),
        // JavaScript's `\b` only knows ASCII word characters.
        eval_call: compile(format!(r"(?-u:\b)eval{JS_SPACE}*\(")),
    }
});

pub fn detect(src: &str) -> DetectResult {
    // JSFuck: only +[]!()
    let mut head = src
        .chars()
        .filter(|c| !is_js_space(*c))
        .take(500)
        .peekable();
    if head.peek().is_some() && head.all(|c| "+-[]!()".contains(c)) {
        return found("jsfuck", 0.9, &["characters limited to +[]!() pattern"]);
    }
    // AAencode: ﾟωﾟ, ﾟДﾟ etc.
    if src.contains(['\u{ff9f}', 'ω', '\u{ff24}', 'ノ', '(', ')', '_', '+'])
        && (src.contains("\u{ff9f}ω\u{ff9f}ノ") || src.contains("\u{ff9f}Д\u{ff9f}"))
    {
        return found(
            "aaencode",
            0.95,
            &["\u{ff9f}ω\u{ff9f} \u{ff9f}Д\u{ff9f} symbols"],
        );
    }
    // JJencode: $=
    let start = utf16_slice(src, 0, 500);
    if src.trim_start_matches(is_js_space).starts_with("$=~[]")
        || start.contains("$={}")
        || start.contains("$$$$$=")
    {
        return found("jjencode", 0.85, &["$= header"]);
    }
    // Dean Edwards packer / eval(function(p,a,c,k,e,d))
    if PATTERNS.packer.is_match(src) {
        return found("packer", 0.95, &["eval(function(p,a,c,k,e,d) header"]);
    }
    // Hex-array obfuscation: large `var _0x = ["...", "..."]`
    if let Some(hex_array) = PATTERNS.hex_array.find(src) {
        return found(
            "hex-array",
            0.85,
            &[
                "hex-named string array",
                utf16_slice(hex_array.as_str(), 0, 80),
            ],
        );
    }
    if src.contains("__webpack_require__") || src.contains("webpackJsonp") {
        return found("webpack", 0.7, &["webpack runtime present"]);
    }
    if PATTERNS.eval_call.is_match(src) && utf16_len(src) > 10_000 {
        return found(
            "eval-chain",
            0.4,
            &["eval() call in large file — may be self-decoding"],
        );
    }
    DetectResult {
        packer: "unknown",
        confidence: json!(0),
        evidence: Vec::new(),
    }
}

/// The URL and body of a captured request; an unknown id and an empty body
/// are the same error.
fn captured(ctx: &ToolContext, request_id: &str) -> Result<(String, String), String> {
    let store = traffic(ctx);
    store
        .get(request_id)
        .and_then(|r| {
            let body = r.response_body.as_deref().filter(|b| !b.is_empty())?;
            Some((r.url.clone(), body.to_owned()))
        })
        .ok_or_else(|| format!("no body for {request_id}"))
}

pub struct DeobDetect;

impl Tool for DeobDetect {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "deob_detect",
            "Detect which obfuscation packer (JSFuck / AAencode / JJencode / Dean Edwards / hex-array / webpack) a captured JS file uses, so the right unpacker can be picked.",
            json!({
                "type": "object",
                "properties": {
                    "requestId": { "type": "string", "description": "requestId of a captured Script" }
                },
                "required": ["requestId"]
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { deob_detect(ctx, &args).unwrap_or_else(err) })
    }
}

fn deob_detect(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("deob_detect", args)?;
    let (url, src) = captured(ctx, args.str("requestId")?)?;
    let DetectResult {
        packer,
        confidence,
        evidence,
    } = detect(&src);
    Ok(ok_pretty(&json!({
        "url": url,
        "bytes": utf16_len(&src),
        "packer": packer,
        "confidence": confidence,
        "evidence": evidence,
    })))
}

pub struct DeobAuto;

impl Tool for DeobAuto {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "deob_auto",
            "Auto-deobfuscate a captured JS file: detects packer, then chains the appropriate unpackers (webcrack for general obfuscation, regex-based for hex-array, eval-unwrap for Dean Edwards packer).",
            json!({
                "type": "object",
                "properties": { "requestId": { "type": "string" } },
                "required": ["requestId"]
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { deob_auto(ctx, &args).await.unwrap_or_else(err) })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Step {
    tool: &'static str,
    bytes_before: usize,
    bytes_after: usize,
    note: String,
}

/// `cur.match(/^eval\((.*)\)\s*$/s)`: the text between a leading `eval(` and
/// the closing parenthesis that ends the script.
fn eval_body(cur: &str) -> Option<&str> {
    cur.strip_prefix("eval(")?
        .trim_end_matches(is_js_space)
        .strip_suffix(')')
}

async fn deob_auto(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("deob_auto", args)?;
    let (url, src) = captured(ctx, args.str("requestId")?)?;
    let det = detect(&src);
    let mut steps = Vec::new();
    let mut cur = src;

    // 1. webcrack (handles webpack/general obfuscation + sourcemaps)
    if matches!(det.packer, "webpack" | "hex-array" | "unknown") {
        let bytes_before = utf16_len(&cur);
        let input = cur.clone();
        let out = tokio::task::spawn_blocking(move || run_webcrack(&input))
            .await
            .map_err(|e| e.to_string())?;
        match out {
            Ok(out) => {
                steps.push(Step {
                    tool: "webcrack",
                    bytes_before,
                    bytes_after: utf16_len(&out),
                    note: "general deobfuscation".into(),
                });
                cur = out;
            }
            Err(message) => steps.push(Step {
                tool: "webcrack",
                bytes_before,
                bytes_after: bytes_before,
                note: format!("failed: {message}"),
            }),
        }
    }

    // 2. Dean Edwards packer: replace eval with print
    if det.packer == "packer"
        && let Some(body) = eval_body(&cur)
    {
        steps.push(Step {
            tool: "packer-unwrap",
            bytes_before: utf16_len(&cur),
            bytes_after: utf16_len(body),
            note: "replaced outer eval(...) with body".into(),
        });
        cur = body.to_owned();
    }

    Ok(ok_pretty(&json!({
        "url": url,
        "detected": det,
        "steps": steps,
        "finalBytes": utf16_len(&cur),
        "preview": utf16_slice(&cur, 0, 2000),
    })))
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(DeobDetect);
    registry.register(DeobAuto);
}

#[cfg(test)]
mod tests {
    use rever_core::traffic::RequestPatch;
    use rever_tools_api::utils::Content;

    use super::*;

    fn context(id: &str, body: &str) -> ToolContext {
        let ctx = ToolContext::for_tests();
        traffic(&ctx).upsert(
            RequestPatch {
                request_id: id.into(),
                url: Some("http://h/app.js".into()),
                response_body: Some(body.into()),
                ..RequestPatch::default()
            },
            1,
        );
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

    #[test]
    fn jsfuck은_공백을_뺀_앞_500자가_여섯_기호뿐일_때다() {
        let result = detect("[+!+[]]+\n (!![]+[])[+[]]\t-()");
        assert_eq!(
            result,
            found("jsfuck", 0.9, &["characters limited to +[]!() pattern"])
        );
        // Only the first 500 non-space characters are looked at.
        assert_eq!(
            detect(&format!("{}alert(1)", "+[]".repeat(200))).packer,
            "jsfuck"
        );
        assert_ne!(
            detect(&format!("{}alert(1)", "+[]".repeat(100))).packer,
            "jsfuck"
        );
        assert_ne!(detect("").packer, "jsfuck");
        assert_ne!(detect(" \n\t").packer, "jsfuck");
    }

    #[test]
    fn aaencode는_얼굴_문자_기호로_알아본다() {
        let result = detect("\u{ff9f}ω\u{ff9f}ノ= /｀ｍ´）ノ ~┻━┻   //*´∇｀*/ ['_'];");
        assert_eq!(result.packer, "aaencode");
        assert_eq!(result.confidence, json!(0.95));
        assert_eq!(
            result.evidence,
            ["\u{ff9f}ω\u{ff9f} \u{ff9f}Д\u{ff9f} symbols"]
        );
        assert_eq!(detect("x = (\u{ff9f}Д\u{ff9f})['c']").packer, "aaencode");
        // The two marks alone, without the face, are not enough.
        assert_eq!(detect("var a = '\u{ff9f}ω';").packer, "unknown");
    }

    #[test]
    fn jjencode는_머리말로_알아본다() {
        assert_eq!(
            detect("\u{feff} \n$=~[];$={___:++$};"),
            found("jjencode", 0.85, &["$= header"])
        );
        assert_eq!(detect("var a;$={};").packer, "jjencode");
        assert_eq!(detect("a.$$$$$=1").packer, "jjencode");
        // Past the first 500 characters the header no longer counts.
        assert_eq!(
            detect(&format!("{}$={{}};", "a;".repeat(250))).packer,
            "unknown"
        );
    }

    #[test]
    fn dean_edwards_packer를_알아본다() {
        let src = "eval(function(p,a,c,k,e,d){return p}('0 1',2,2,'a|b'.split('|'),0,{}))";
        assert_eq!(
            detect(src),
            found("packer", 0.95, &["eval(function(p,a,c,k,e,d) header"])
        );
        assert_eq!(
            detect("x;eval( \n function(p,a,c,k,e,r){})").packer,
            "packer"
        );
    }

    #[test]
    fn hex_배열은_선언의_앞_80자를_근거로_남긴다() {
        let items: Vec<String> = (0..40).map(|i| format!("'s{i}'")).collect();
        let src = format!("var _0x4f2a=[{}];(function(){{}})()", items.join(","));
        let result = detect(&src);
        assert_eq!(result.packer, "hex-array");
        assert_eq!(result.confidence, json!(0.85));
        assert_eq!(result.evidence.len(), 2);
        assert_eq!(result.evidence[0], "hex-named string array");
        assert_eq!(result.evidence[1], src.get(..80).unwrap());
        // The name must be hex and the array must end in ; or ,
        assert_eq!(detect("var _0xzz=['a'];").packer, "unknown");
        assert_eq!(detect("var _0x1f=['a']").packer, "unknown");
        assert_eq!(detect("var\u{a0}_0x1f = ['a'],b;").packer, "hex-array");
    }

    #[test]
    fn webpack과_eval_chain과_unknown을_구분한다() {
        assert_eq!(
            detect("(()=>{__webpack_require__(1)})()"),
            found("webpack", 0.7, &["webpack runtime present"])
        );
        assert_eq!(
            detect("(window.webpackJsonp=[]).push([])").packer,
            "webpack"
        );
        let padding = "a;".repeat(5_000);
        assert_eq!(
            detect(&format!("{padding}eval (x)")),
            found(
                "eval-chain",
                0.4,
                &["eval() call in large file — may be self-decoding"]
            )
        );
        // Exactly 10,000 characters is not "large", and `medieval(` is no eval call.
        assert_eq!(
            detect(&format!("{}eval(x);", "a;".repeat(4_996))).packer,
            "unknown"
        );
        assert_eq!(detect(&format!("{padding}medieval(x)")).packer, "unknown");
        let unknown = detect("var a = 1;");
        assert_eq!(unknown.packer, "unknown");
        assert_eq!(unknown.evidence, Vec::<String>::new());
        // JSON.stringify prints the number 0, not 0.0.
        assert_eq!(
            serde_json::to_string(&unknown).unwrap(),
            r#"{"packer":"unknown","confidence":0,"evidence":[]}"#
        );
    }

    #[tokio::test]
    async fn deob_detect는_url과_길이와_판정을_돌려준다() {
        let ctx = context("s1", "var 한 = __webpack_require__(1);");
        assert_eq!(
            call(DeobDetect, &ctx, json!({ "requestId": "s1" })).await,
            (
                false,
                r#"{
  "url": "http://h/app.js",
  "bytes": 31,
  "packer": "webpack",
  "confidence": 0.7,
  "evidence": [
    "webpack runtime present"
  ]
}"#
                .to_owned()
            )
        );
    }

    #[tokio::test]
    async fn 본문이_없으면_두_도구_모두_같은_에러를_돌려준다() {
        let ctx = context("empty", "");
        for id in ["empty", "nope"] {
            let expected = (true, format!("no body for {id}"));
            assert_eq!(
                call(DeobDetect, &ctx, json!({ "requestId": id })).await,
                expected
            );
            assert_eq!(
                call(DeobAuto, &ctx, json!({ "requestId": id })).await,
                expected
            );
        }
        assert!(call(DeobDetect, &ctx, json!({})).await.0);
        assert!(call(DeobAuto, &ctx, json!({ "requestId": 1 })).await.0);
    }

    #[tokio::test]
    async fn deob_auto는_packer의_바깥_eval을_벗긴다() {
        let inner = "function(p,a,c,k,e,d){return p}('0 1',2,2,'a|b'.split('|'),0,{})";
        let ctx = context("s1", &format!("eval({inner}) \n"));
        assert_eq!(
            call(DeobAuto, &ctx, json!({ "requestId": "s1" })).await,
            (
                false,
                format!(
                    r#"{{
  "url": "http://h/app.js",
  "detected": {{
    "packer": "packer",
    "confidence": 0.95,
    "evidence": [
      "eval(function(p,a,c,k,e,d) header"
    ]
  }},
  "steps": [
    {{
      "tool": "packer-unwrap",
      "bytesBefore": {},
      "bytesAfter": {},
      "note": "replaced outer eval(...) with body"
    }}
  ],
  "finalBytes": {},
  "preview": {}
}}"#,
                    inner.len() + 8,
                    inner.len(),
                    inner.len(),
                    serde_json::to_string(inner).unwrap()
                )
            )
        );
    }

    #[tokio::test]
    async fn eval로_시작하지_않는_packer와_다른_packer는_그대로_둔다() {
        // Detected as packer, but the eval is not the whole script: no unwrap.
        let src = "var x;eval(function(p,a,c,k,e,d){return p}('0',1,1,''.split('|'),0,{}))";
        let ctx = context("s1", src);
        let (is_error, text) = call(DeobAuto, &ctx, json!({ "requestId": "s1" })).await;
        assert!(!is_error);
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["steps"], json!([]));
        assert_eq!(out["finalBytes"], src.len());
        assert_eq!(out["preview"], src);

        // jsfuck has no unpacker here; the preview is capped at 2,000 characters.
        let src = "+[]".repeat(1_000);
        let ctx = context("s2", &src);
        let (_, text) = call(DeobAuto, &ctx, json!({ "requestId": "s2" })).await;
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["detected"]["packer"], "jsfuck");
        assert_eq!(out["steps"], json!([]));
        assert_eq!(out["finalBytes"], 3_000);
        assert_eq!(out["preview"].as_str().map(str::len), Some(2_000));
    }

    #[tokio::test]
    async fn deob_auto는_일반_스크립트를_webcrack에_맡기고_실패해도_단계를_남긴다() {
        let src = "var a = __webpack_require__(1);";
        let ctx = context("s1", src);
        let (is_error, text) = call(DeobAuto, &ctx, json!({ "requestId": "s1" })).await;
        assert!(!is_error, "{text}");
        let out: Value = serde_json::from_str(&text).unwrap();
        let step = &out["steps"][0];
        assert_eq!(out["steps"].as_array().map(Vec::len), Some(1));
        assert_eq!(step["tool"], "webcrack");
        assert_eq!(step["bytesBefore"], src.len());
        // With webcrack installed the note is the success one; without it the
        // step records the failure and the source is left as it was.
        let note = step["note"].as_str().unwrap();
        if note != "general deobfuscation" {
            assert!(note.starts_with("failed: "), "{note}");
            assert_eq!(step["bytesAfter"], src.len());
            assert_eq!(out["preview"], src);
        }
        assert_eq!(out["finalBytes"], step["bytesAfter"]);
    }

    #[test]
    fn 바깥_eval은_끝의_공백을_넘어_마지막_괄호까지다() {
        assert_eq!(eval_body("eval(a(b))\u{feff}\n"), Some("a(b)"));
        assert_eq!(eval_body("eval()"), Some(""));
        assert_eq!(eval_body("eval(a);"), None);
        assert_eq!(eval_body(" eval(a)"), None);
        assert_eq!(eval_body("eval("), None);
    }
}
