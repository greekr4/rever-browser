//! Port of `src/main/mcp/tools/antibot.ts` — the `detect_antibot_vm` tool.
//!
//! The heuristics are JavaScript regular expressions. They are rewritten for
//! the regex crate so that they accept exactly the same text: `\s`, `\b`, `.`
//! and the `i` flag are spelled out with their JavaScript meaning, the
//! backreferences are expanded into one alternative per quote character, and
//! the body is matched in a form where one character is one UTF-16 code unit,
//! because the bounded repeats (`.{0,40}`) count code units.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::args::validate;
use crate::js::{JS_SPACE as S, json_stringify, utf16_len};
use crate::traffic;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Signal {
    name: &'static str,
    hit: bool,
    detail: String,
    weight: u32,
}

/// `[0-9a-f]` under the `i` flag, which folds ASCII letters only.
const HEX: &str = "(?i-u:[0-9a-f])";
/// `.` without the `s` flag: anything but a line terminator.
const DOT: &str = r"[^\n\r\x{2028}\x{2029}]";

fn pattern(source: String) -> Regex {
    Regex::new(&source).expect("the pattern is a constant")
}

// /_0x[0-9a-f]{4,}/gi
static HEX_IDS: LazyLock<Regex> = LazyLock::new(|| pattern(format!("_0(?i-u:x){HEX}{{4,}}")));
// /case\s+0?x?[0-9a-f]+\s*:/gi
static SWITCH_CASES: LazyLock<Regex> =
    LazyLock::new(|| pattern(format!("(?i-u:case){S}+0?(?i-u:x)?{HEX}+{S}*:")));
// /(?:var|const|let)\s+_0x[0-9a-f]+\s*=\s*\[\s*(['"]).{0,40}\1\s*(?:,\s*(['"]).{0,40}\2\s*){15,}/i
static BIG_STRING_ARRAY: LazyLock<Regex> = LazyLock::new(|| {
    // (['"]).{0,40}\1 — the closing quote is the opening one.
    let quoted = format!("(?:'{DOT}{{0,40}}'|\"{DOT}{{0,40}}\")");
    pattern(format!(
        r"(?i-u:var|const|let){S}+_0(?i-u:x){HEX}+{S}*={S}*\[{S}*{quoted}{S}*(?:,{S}*{quoted}{S}*){{15,}}"
    ))
});
// /\(function\s*\(_0x[0-9a-f]+\s*,\s*_0x[0-9a-f]+\)\s*\{[\s\S]{0,400}?while\s*\(/i
static ROTATOR: LazyLock<Regex> = LazyLock::new(|| {
    pattern(format!(
        r"\((?i-u:function){S}*\(_0(?i-u:x){HEX}+{S}*,{S}*_0(?i-u:x){HEX}+\){S}*\{{(?s:.){{0,400}}?(?i-u:while){S}*\("
    ))
});
// /\b(eval|Function)\s*\(/g
static EVAL_FN: LazyLock<Regex> =
    LazyLock::new(|| pattern(format!(r"(?-u:\b)(?:eval|Function){S}*\(")));
// /\b(?:var|const|let)\s+[a-z]\b/g
static SINGLE_CHAR_VARS: LazyLock<Regex> =
    LazyLock::new(|| pattern(format!(r"(?-u:\b)(?:var|const|let){S}+[a-z](?-u:\b)")));

/// The body with every character outside the Basic Multilingual Plane
/// replaced by two placeholders, so that one `char` is one UTF-16 code unit.
/// No pattern here tells such characters apart.
fn code_unit_view(body: &str) -> Cow<'_, str> {
    if !body.chars().any(|c| c.len_utf16() == 2) {
        return Cow::Borrowed(body);
    }
    let mut view = String::with_capacity(body.len());
    for c in body.chars() {
        if c.len_utf16() == 2 {
            view.push_str("\u{fffd}\u{fffd}");
        } else {
            view.push(c);
        }
    }
    Cow::Owned(view)
}

fn analyze(body: &str) -> (Vec<Signal>, u32) {
    let max_line = body.split('\n').map(utf16_len).max().unwrap_or(0);
    let body = code_unit_view(body);
    let count = |re: &Regex| re.find_iter(&body).count();
    let hex_ids = count(&HEX_IDS);
    let switch_cases = count(&SWITCH_CASES);
    let big_string_array = BIG_STRING_ARRAY.is_match(&body);
    let rotator = ROTATOR.is_match(&body);
    let eval_fn = count(&EVAL_FN);
    let single_char_vars = count(&SINGLE_CHAR_VARS);

    let found = |hit: bool, text: &str| if hit { text } else { "none" }.to_owned();
    let signals = vec![
        Signal {
            name: "single-line-blob",
            hit: max_line > 5000,
            detail: format!("longest line = {max_line} chars"),
            weight: 1,
        },
        Signal {
            name: "hex-identifiers (_0x…)",
            hit: hex_ids > 20,
            detail: format!("{hex_ids} occurrences"),
            weight: 2,
        },
        Signal {
            name: "opcode switch dispatcher",
            hit: switch_cases > 15,
            detail: format!("{switch_cases} numeric case labels"),
            weight: 2,
        },
        Signal {
            name: "rotated string-array",
            hit: big_string_array,
            detail: found(big_string_array, "large _0x string table found"),
            weight: 2,
        },
        Signal {
            name: "string-array rotator IIFE",
            hit: rotator,
            detail: found(rotator, "decoder IIFE with while-loop found"),
            weight: 2,
        },
        Signal {
            name: "eval / Function() usage",
            hit: eval_fn > 3,
            detail: format!("{eval_fn} dynamic-eval sites"),
            weight: 1,
        },
        Signal {
            name: "many single-char vars",
            hit: single_char_vars > 40,
            detail: format!("{single_char_vars} single-char declarations"),
            weight: 1,
        },
    ];
    let score = signals.iter().filter(|s| s.hit).map(|s| s.weight).sum();
    (signals, score)
}

fn detect_antibot_vm(ctx: &ToolContext, request_id: &str) -> ToolResult {
    // Copied out so the store is not locked while a multi-megabyte bundle is scanned.
    let (url, body) = {
        let store = traffic(ctx);
        let Some(entry) = store.get(request_id) else {
            return err(format!("unknown requestId: {request_id}"));
        };
        match entry.response_body.as_deref() {
            Some(body) if !body.is_empty() && entry.response_body_base64 != Some(true) => {
                (entry.url.clone(), body.to_owned())
            }
            _ => {
                return err(format!(
                    "requestId {request_id} has no text body to analyze"
                ));
            }
        }
    };
    let (signals, score) = analyze(&body);
    let verdict = match score {
        5.. => "likely-vm",
        3.. => "possibly-obfuscated",
        _ => "ordinary",
    };
    let recommendation = match verdict {
        "likely-vm" => {
            "Do NOT sink time into static deobfuscation. Treat it as a VM: hook the runtime (crypto_trace for signing, inject_add to wrap the token builder) and capture the generated value in-browser. Step-by-step procedure: docs/jsvmp-recovery.md."
        }
        "possibly-obfuscated" => {
            "Some obfuscation present; try deobfuscate_script, but be ready to fall back to a runtime hook (see docs/jsvmp-recovery.md)."
        }
        _ => {
            "Reads as ordinary/minified code — static analysis (grep_scripts, sourcemap, deobfuscate_script) is fine."
        }
    };
    let mut out = Map::new();
    out.insert("requestId".into(), json!(request_id));
    out.insert("url".into(), json!(url));
    out.insert("score".into(), json!(score));
    out.insert("verdict".into(), json!(verdict));
    out.insert("recommendation".into(), json!(recommendation));
    if verdict != "ordinary" {
        out.insert("playbook".into(), json!("docs/jsvmp-recovery.md"));
    }
    out.insert("signals".into(), json!(signals));
    ok(json_stringify(&Value::Object(out), true))
}

pub struct DetectAntibotVm;

impl Tool for DetectAntibotVm {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "detect_antibot_vm",
            "Heuristically classify a captured script as a custom interpreter / risk-control (antibot) VM — the kind of self-modifying, opcode-dispatched, hex-obfuscated bundle used by Akamai / 风控 / captcha vendors. A high score means static deobfuscation is a dead end and you should plan a runtime approach (crypto_trace, inject a hook, capture the generated token in-browser) instead. Pass a script requestId from list_scripts / list_requests.",
            json!({
                "type": "object",
                "properties": {
                    "requestId": {
                        "type": "string",
                        "description": "requestId of a script (from list_scripts or list_requests)"
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
            let request_id = args.get("requestId").and_then(Value::as_str).unwrap_or("");
            detect_antibot_vm(ctx, request_id)
        })
    }
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(DetectAntibotVm);
}

#[cfg(test)]
mod tests {
    use rever_core::traffic::RequestPatch;

    use super::*;
    use crate::testing::{request, run, store};

    // Every expectation below is what the TypeScript `analyze` returned for
    // the same body (run under node).

    /// Which signals hit, in order, as a string of `+` and `-`.
    fn hits(body: &str) -> String {
        let (signals, _) = analyze(body);
        signals
            .iter()
            .map(|s| if s.hit { '+' } else { '-' })
            .collect()
    }

    fn detail(body: &str, signal: usize) -> String {
        let (signals, _) = analyze(body);
        signals
            .get(signal)
            .map(|s| s.detail.clone())
            .unwrap_or_default()
    }

    fn has_string_table(body: &str) -> bool {
        analyze(body).0.get(3).is_some_and(|s| s.hit)
    }

    fn has_rotator(body: &str) -> bool {
        analyze(body).0.get(4).is_some_and(|s| s.hit)
    }

    /// `var _0x4a2b=[<entries>];` with `count` copies of what `entry` makes.
    fn table(count: usize, entry: impl Fn(usize) -> String) -> String {
        let entries: Vec<String> = (0..count).map(entry).collect();
        format!("var _0x4a2b=[{}];", entries.join(","))
    }

    fn hex_ids(count: usize) -> String {
        let ids: Vec<String> = (0..count).map(|i| format!("_0x{i:04x}")).collect();
        ids.join(" ")
    }

    const ROTATOR: &str = "(function(_0x1a2b,_0x3c4d){var _0x5=function(n){while(--n){_0x1a2b['push'](_0x1a2b['shift']());}};_0x5(++_0x3c4d);}(_0x4a2b,0x1f4));";

    fn script(ctx: &ToolContext, id: &str, body: &str) {
        store(
            ctx,
            RequestPatch {
                response_body: Some(body.to_owned()),
                ..request(id, &format!("https://cdn.test/{id}.js"))
            },
        );
    }

    async fn detect(ctx: &ToolContext, id: &str) -> (bool, String) {
        run(&DetectAntibotVm, ctx, json!({ "requestId": id })).await
    }

    #[tokio::test]
    async fn 평범한_코드는_ordinary로_판정하고_플레이북을_적지_않는다() {
        let expected = r#"{
  "requestId": "ab_plain",
  "url": "https://cdn.test/ab_plain.js",
  "score": 0,
  "verdict": "ordinary",
  "recommendation": "Reads as ordinary/minified code — static analysis (grep_scripts, sourcemap, deobfuscate_script) is fine.",
  "signals": [
    {
      "name": "single-line-blob",
      "hit": false,
      "detail": "longest line = 35 chars",
      "weight": 1
    },
    {
      "name": "hex-identifiers (_0x…)",
      "hit": false,
      "detail": "0 occurrences",
      "weight": 2
    },
    {
      "name": "opcode switch dispatcher",
      "hit": false,
      "detail": "0 numeric case labels",
      "weight": 2
    },
    {
      "name": "rotated string-array",
      "hit": false,
      "detail": "none",
      "weight": 2
    },
    {
      "name": "string-array rotator IIFE",
      "hit": false,
      "detail": "none",
      "weight": 2
    },
    {
      "name": "eval / Function() usage",
      "hit": false,
      "detail": "0 dynamic-eval sites",
      "weight": 1
    },
    {
      "name": "many single-char vars",
      "hit": false,
      "detail": "0 single-char declarations",
      "weight": 1
    }
  ]
}"#;
        let ctx = ToolContext::for_tests();
        let body = "function add(a, b) { return a + b }\nconsole.log(add(1, 2))\n";
        script(&ctx, "ab_plain", body);
        assert_eq!(detect(&ctx, "ab_plain").await, (false, expected.to_owned()));
    }

    #[tokio::test]
    async fn 신호가_모두_켜지면_likely_vm이고_런타임_접근을_권한다() {
        let cases: String = (0..16).map(|i| format!("case 0x{i:x}:break;")).collect();
        let evals = "eval(a);Function ('x');eval\n(b); new Function(c);";
        let vars: String = (0..41u8)
            .map(|i| format!("var {}={i};", char::from(b'a' + i % 26)))
            .collect();
        let body = format!(
            "{}{ROTATOR}{}switch(op){{{cases}}}{evals}{vars}{}",
            table(20, |i| format!("'s{i}'")),
            hex_ids(21),
            "x".repeat(5001)
        );
        let ctx = ToolContext::for_tests();
        script(&ctx, "vm", &body);
        let (failed, text) = detect(&ctx, "vm").await;
        assert!(!failed);
        assert!(text.starts_with(
            "{\n  \"requestId\": \"vm\",\n  \"url\": \"https://cdn.test/vm.js\",\n  \"score\": 11,\n  \"verdict\": \"likely-vm\",\n  \"recommendation\": \"Do NOT sink time into static deobfuscation. Treat it as a VM: hook the runtime (crypto_trace for signing, inject_add to wrap the token builder) and capture the generated value in-browser. Step-by-step procedure: docs/jsvmp-recovery.md.\",\n  \"playbook\": \"docs/jsvmp-recovery.md\",\n  \"signals\": [\n"
        ));
        assert_eq!(hits(&body), "+++++++");
        assert_eq!(detail(&body, 3), "large _0x string table found");
        assert_eq!(detail(&body, 4), "decoder IIFE with while-loop found");
        assert_eq!(detail(&body, 5), "4 dynamic-eval sites");
    }

    #[tokio::test]
    async fn 점수_3과_4는_possibly_obfuscated다() {
        let ctx = ToolContext::for_tests();
        // Hex identifiers (2) + one long line (1).
        script(
            &ctx,
            "three",
            &format!("{}{}", hex_ids(21), "x".repeat(6000)),
        );
        // String table (2) + hex identifiers (2).
        script(
            &ctx,
            "four",
            &format!("{}{}", table(20, |i| format!("'s{i}'")), hex_ids(21)),
        );
        for (id, score) in [("three", 3), ("four", 4)] {
            let (failed, text) = detect(&ctx, id).await;
            assert!(!failed);
            assert!(text.contains(&format!(
                "  \"score\": {score},\n  \"verdict\": \"possibly-obfuscated\",\n  \"recommendation\": \"Some obfuscation present; try deobfuscate_script, but be ready to fall back to a runtime hook (see docs/jsvmp-recovery.md).\",\n  \"playbook\": \"docs/jsvmp-recovery.md\",\n"
            )));
        }
        // Two points are still ordinary.
        script(&ctx, "two", &hex_ids(21));
        assert!(
            detect(&ctx, "two")
                .await
                .1
                .contains("\"verdict\": \"ordinary\"")
        );
    }

    #[test]
    fn 가장_긴_줄은_utf16_단위로_5000을_넘어야_신호다() {
        assert_eq!(hits(&"x".repeat(5001)), "+------");
        let body = format!("{}\n{}", "x".repeat(5000), "y".repeat(10));
        assert_eq!(hits(&body), "-------");
        assert_eq!(detail(&body, 0), "longest line = 5000 chars");
        // An emoji is two code units.
        assert_eq!(detail(&"😀".repeat(2501), 0), "longest line = 5002 chars");
        assert_eq!(hits(&"😀".repeat(2501)), "+------");
        assert_eq!(hits(&format!("{}\n", "😀".repeat(2500))), "-------");
        assert_eq!(detail("", 0), "longest line = 0 chars");
    }

    #[test]
    fn hex_식별자는_스물을_넘어야_신호다() {
        assert_eq!(detail(&hex_ids(21), 1), "21 occurrences");
        assert_eq!(hits(&hex_ids(21)), "-+-----");
        // Upper case counts; three hex digits or non-hex letters do not.
        let ids: Vec<String> = (0..20).map(|i| format!("_0X{i:04X}")).collect();
        let body = format!("{} _0x123 _0xzzzz", ids.join(" "));
        assert_eq!(detail(&body, 1), "20 occurrences");
        assert_eq!(hits(&body), "-------");
    }

    #[test]
    fn 숫자_case_라벨은_열다섯을_넘어야_신호다() {
        let cases: String = (0..16).map(|i| format!("case 0x{i:x}:break;")).collect();
        assert_eq!(detail(&cases, 2), "16 numeric case labels");
        assert_eq!(hits(&cases), "--+----");
        let cases: String = (0..15).map(|i| format!("CASE\t{i} :break;")).collect();
        let body = format!("switch(op){{{cases}case x: case 0xg: case -1: case 'a':}}");
        assert_eq!(detail(&body, 2), "15 numeric case labels");
        assert_eq!(hits(&body), "-------");
    }

    #[test]
    fn case_라벨의_공백과_대소문자는_자바스크립트_정규식과_같다() {
        // U+00A0 and U+FEFF are `\s` in JavaScript, U+0085 is not.
        let body = "case\u{a0}1: case\u{85}2: case\u{feff}3: case 0x: case x1: case 0xFF : casex: case 00x1:";
        assert_eq!(detail(body, 2), "4 numeric case labels");
        // The `i` flag does not fold U+017F (long s) to "s".
        assert_eq!(detail("ca\u{17f}e 2: case 3:", 2), "1 numeric case labels");
    }

    #[test]
    fn 문자열_테이블은_열여섯_개부터_잡는다() {
        let short = |count| {
            let entries: Vec<String> = (0..count).map(|i| format!("'s{i}'")).collect();
            format!("let _0x1f=[{}];", entries.join(","))
        };
        assert!(!has_string_table(&short(15)));
        assert!(has_string_table(&short(16)));
        assert!(has_string_table(&table(20, |i| format!("'s{i}'"))));
        // Double quotes, spaces around every token, an upper-case identifier.
        let entries: Vec<String> = (0..16).map(|i| format!("\"str{i}\"")).collect();
        let spaced = format!("const _0xABCDEF = [ {} ];", entries.join(" , "));
        assert!(has_string_table(&spaced));
        // Whitespace may be newlines, and the keyword may be upper case.
        let entries: Vec<String> = (0..20).map(|i| format!("'s{i}'")).collect();
        let multiline = format!("VAR\n_0X4A2B\n=\n[\n{}];", entries.join("\n,\n"));
        assert!(has_string_table(&multiline));
    }

    #[test]
    fn 문자열_테이블의_따옴표는_여는_것과_닫는_것이_같아야_한다() {
        // Each entry may use its own quote character...
        let mixed = table(20, |i| {
            if i % 2 == 1 {
                format!("'s{i}'")
            } else {
                format!("\"s{i}\"")
            }
        });
        assert!(has_string_table(&mixed));
        // ...but must close with the one it opened with.
        assert!(!has_string_table(&table(20, |i| format!("'s{i}\""))));
    }

    #[test]
    fn 문자열_테이블의_항목은_utf16으로_40자까지이고_줄을_넘지_않는다() {
        assert!(has_string_table(&table(20, |_| format!(
            "'{}'",
            "x".repeat(40)
        ))));
        assert!(!has_string_table(&table(20, |_| format!(
            "'{}'",
            "x".repeat(41)
        ))));
        // Twenty emoji are forty code units; twenty-one are too many.
        assert!(has_string_table(&table(20, |_| format!(
            "'{}'",
            "😀".repeat(20)
        ))));
        assert!(!has_string_table(&table(20, |_| format!(
            "'{}'",
            "😀".repeat(21)
        ))));
        // `.` stops at line terminators, U+2028 included.
        assert!(!has_string_table(&table(20, |_| "'a\nb'".to_owned())));
        assert!(!has_string_table(&table(20, |_| "'a\u{2028}b'".to_owned())));
    }

    #[test]
    fn 로테이터는_while까지_utf16으로_400자_안에_있어야_한다() {
        assert!(has_rotator(ROTATOR));
        assert_eq!(hits(ROTATOR), "----+--");
        let gap =
            |filler: String| format!("(function (_0x1a2b , _0x3c4d) {{{filler}while (true){{}}}})");
        assert!(has_rotator(&gap(" ".repeat(400))));
        assert!(!has_rotator(&gap(" ".repeat(401))));
        // The gap may span lines, and the keywords may be upper case.
        let upper = format!(
            "(FUNCTION (_0X1A2B , _0X3C4D) {{{}WHILE (true){{}}}})",
            "\n".repeat(400)
        );
        assert!(has_rotator(&upper));
        assert!(has_rotator(&gap("😀".repeat(200))));
        assert!(!has_rotator(&gap("😀".repeat(201))));
    }

    #[test]
    fn eval과_function_호출은_단어_경계에서만_센다() {
        let body = "eval(a);Function ('x');eval\n(b); new Function(c); myeval(d); evaluate(e); Eval(f); $eval(g); \u{e9}eval(h); _eval(i); function(j)";
        assert_eq!(detail(body, 5), "6 dynamic-eval sites");
        assert_eq!(hits(body), "-----+-");
        assert_eq!(hits("eval(a);eval(b);eval(c)"), "-------");
    }

    #[test]
    fn 한_글자_변수_선언은_자바스크립트의_단어_경계로_센다() {
        let body = "var a;let b=1;const c;var A;var ab;var a1;var a$;var a-b;var\u{a0}z;var\u{85}y;var\u{feff}x;xvar q;var \u{e9};var e\u{e9};_let k;$let m";
        assert_eq!(detail(body, 6), "9 single-char declarations");
        let many = |count: u8| -> String {
            (0..count)
                .map(|i| format!("var {}={i};", char::from(b'a' + i % 26)))
                .collect()
        };
        assert_eq!(hits(&many(40)), "-------");
        assert_eq!(hits(&many(41)), "------+");
    }

    #[test]
    fn 큰_번들도_정규식_한도_안에서_분석한다() {
        // A megabyte of ordinary-looking code with near-misses of every pattern.
        let line =
            "var _0x12ab=['a','b'];(function(_0x1,_0x2){return 1})();case 1:eval (x);let q;\n";
        let body = line.repeat(1024 * 1024 / line.len());
        let (signals, score) = analyze(&body);
        assert_eq!(signals.len(), 7);
        // Hex identifiers, case labels, eval sites and single-char vars all pass their thresholds.
        assert_eq!(score, 6);
    }

    #[tokio::test]
    async fn 글자_본문이_없는_요청과_모르는_요청은_오류다() {
        let ctx = ToolContext::for_tests();
        store(
            &ctx,
            RequestPatch {
                response_body: Some("dmFyIGE=".into()),
                response_body_base64: Some(true),
                ..request("b64", "https://cdn.test/b64.js")
            },
        );
        script(&ctx, "empty", "");
        store(&ctx, request("nobody", "https://cdn.test/n.js"));
        for id in ["b64", "empty", "nobody"] {
            assert_eq!(
                detect(&ctx, id).await,
                (true, format!("requestId {id} has no text body to analyze"))
            );
        }
        assert_eq!(
            detect(&ctx, "missing").await,
            (true, "unknown requestId: missing".to_owned())
        );
        let (failed, text) = run(&DetectAntibotVm, &ctx, json!({})).await;
        assert!(failed);
        assert!(
            text.contains("\"message\": \"Invalid input: expected string, received undefined\"")
        );
    }
}
