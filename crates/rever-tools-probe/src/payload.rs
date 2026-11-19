//! Port of `src/main/mcp/tools/payload_probe.ts` — fire reflection / stored-XSS
//! probes at a request slot (marker `§`) and inspect the response body for raw,
//! HTML-escaped, and URL-encoded reflections, reporting the lexical context the
//! payload landed in.

use std::sync::{LazyLock, PoisonError};
use std::time::Duration;

use regex::Regex;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef};
use rever_tools_api::utils::{ToolResult, err, ok};
use rever_tools_http::{build_request_spec, restore_marker, send};
use serde_json::{Value, json};

use crate::args::{strings, text, validate};
use crate::common::{apply_payload, encode_uri_component, has_marker};
use crate::{pretty, schema};

/// The default XSS payload set, verbatim from payload_probe.ts.
const DEFAULT_XSS_PAYLOADS: &[&str] = &[
    "<script>alert(1)</script>",
    "\"><svg/onload=alert(1)>",
    "'><img src=x onerror=alert(1)>",
    "javascript:alert(1)",
    "<details/open/ontoggle=alert(1)>",
    "<x onclick=alert(1)>x",
    "<a href=\"javascript:alert(1)\">x</a>",
];

const SEND_TIMEOUT: Duration = Duration::from_secs(15);

// The TS `detectContext` regexes. The regex crate lacks lookahead but these
// don't need it; `$`/`^` anchor to the end/start of the (already-sliced) window.
static SCRIPT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<script[^>]*>[^<]*$").expect("valid"));
static ATTR_DQ_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"=\s*"[^"]*$"#).expect("valid"));
static ATTR_SQ_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"=\s*'[^']*$").expect("valid"));
static TEXT_BEFORE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r">[^<]*$").expect("valid"));
static TEXT_AFTER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[^<]*<").expect("valid"));

/// Largest char boundary `<= i` (clamped to the string).
fn floor_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest char boundary `>= i` (clamped to the string).
fn ceil_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Port of the TS `htmlEsc` builder — order matters so the `&` introduced by
/// later replacements isn't re-escaped.
fn html_escape(p: &str) -> String {
    p.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The lexical context where `payload` landed in `body`: a 80-char window before
/// and after the match classifies it. `none` when the payload isn't present.
fn detect_context(body: &str, payload: &str) -> &'static str {
    let Some(idx) = body.find(payload) else {
        return "none";
    };
    let before = &body[floor_boundary(body, idx.saturating_sub(80))..idx];
    let after_start = idx + payload.len();
    let after = &body[after_start..ceil_boundary(body, after_start + 80)];
    if SCRIPT_RE.is_match(before) {
        "script"
    } else if ATTR_DQ_RE.is_match(before) || ATTR_SQ_RE.is_match(before) {
        "attribute"
    } else if TEXT_BEFORE_RE.is_match(before) || TEXT_AFTER_RE.is_match(after) {
        "text"
    } else {
        "unknown"
    }
}

/// A ±40-char window (UTF-8 safe) around the first match of `payload`, or empty
/// when it isn't reflected.
fn preview(body: &str, payload: &str) -> String {
    match body.find(payload) {
        Some(idx) => {
            let lo = floor_boundary(body, idx.saturating_sub(40));
            let hi = ceil_boundary(body, idx + payload.len() + 40);
            body[lo..hi].to_owned()
        }
        None => String::new(),
    }
}

pub struct PayloadProbe;

impl Tool for PayloadProbe {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "payload_probe",
            "Fire reflection / stored-XSS probes at a request slot (marker `§`) and inspect the response body for raw, HTML-escaped, and URL-encoded reflections. Reports the lexical context where the payload landed.",
            schema(
                json!({
                    "requestId": { "type": "string", "description": "Base requestId with § marker" },
                    "payloads": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Override default XSS payloads (max 500)"
                    }
                }),
                &["requestId"],
            ),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(refusal) = validate(&self.def(), &args) {
                return refusal;
            }
            let request_id = text(&args, "requestId").unwrap_or_default().to_owned();

            // Lock the store only long enough to build the spec; the sends below
            // must not hold it.
            let base = {
                let store = ctx.traffic.lock().unwrap_or_else(PoisonError::into_inner);
                match build_request_spec(&store, Some(&request_id), None) {
                    Ok(spec) => restore_marker(spec),
                    Err(error) => return err(error),
                }
            };
            if !has_marker(&base) {
                return err("marker § not found in request");
            }

            let list = strings(&args, "payloads")
                .filter(|list| !list.is_empty())
                .unwrap_or_else(|| DEFAULT_XSS_PAYLOADS.iter().map(|p| (*p).to_owned()).collect());
            // zod's `.max(500)`: don't fan out past the cap.
            let list: Vec<String> = list.into_iter().take(500).collect();

            let mut reflections = Vec::with_capacity(list.len());
            for p in &list {
                // The TS `sub` url-encodes only the url (headers+body raw): that
                // is exactly apply_payload(base, p, false).
                let variant = apply_payload(&base, p, false);
                let out = send(&variant, SEND_TIMEOUT).await;
                let reflected_raw = out.body.contains(p.as_str());
                let html_esc = html_escape(p);
                reflections.push(json!({
                    "payload": p,
                    "status": out.status,
                    "length": out.body.len(),
                    "reflectedRaw": reflected_raw,
                    "reflectedHtmlEscaped": out.body.contains(&html_esc),
                    "reflectedUrlEncoded": out.body.contains(&encode_uri_component(p)),
                    "contextHint": if reflected_raw { detect_context(&out.body, p) } else { "none" },
                    "preview": if reflected_raw { preview(&out.body, p) } else { String::new() },
                }));
            }

            ok(pretty(&json!({ "count": reflections.len(), "reflections": reflections })))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 기본_페이로드_세트는_payload_probe_ts와_같다() {
        assert_eq!(DEFAULT_XSS_PAYLOADS.len(), 7);
        assert_eq!(DEFAULT_XSS_PAYLOADS[0], "<script>alert(1)</script>");
        assert_eq!(DEFAULT_XSS_PAYLOADS[6], "<a href=\"javascript:alert(1)\">x</a>");
    }

    #[test]
    fn detect_context는_script_안의_반사를_찾는다() {
        assert_eq!(detect_context("<script>var x=PAYLOAD</script>", "PAYLOAD"), "script");
        // case-insensitive
        assert_eq!(detect_context("<SCRIPT>PAYLOAD", "PAYLOAD"), "script");
    }

    #[test]
    fn detect_context는_속성값_안의_반사를_찾는다() {
        assert_eq!(detect_context("<input value=\"PAYLOAD\">", "PAYLOAD"), "attribute");
        assert_eq!(detect_context("<input value='PAYLOAD'>", "PAYLOAD"), "attribute");
    }

    #[test]
    fn detect_context는_텍스트_노드의_반사를_찾는다() {
        // before window ends with `>…`
        assert_eq!(detect_context("<div>PAYLOAD</div>", "PAYLOAD"), "text");
        // after window starts with `…<`
        assert_eq!(detect_context("PAYLOAD<br>", "PAYLOAD"), "text");
    }

    #[test]
    fn detect_context는_분류_불가면_unknown_없으면_none() {
        assert_eq!(detect_context("PAYLOAD plain tail", "PAYLOAD"), "unknown");
        assert_eq!(detect_context("nothing here", "PAYLOAD"), "none");
    }

    #[test]
    fn html_escape는_순서대로_치환해_amp를_재이스케이프하지_않는다() {
        // `&` first, so the `&` from `&lt;`/`&gt;`/`&quot;` is never re-escaped.
        assert_eq!(html_escape("\"&<>"), "&quot;&amp;&lt;&gt;");
        assert_eq!(
            html_escape("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
    }

    #[test]
    fn 반사_판정은_raw와_html이스케이프를_구분한다() {
        let p = "<script>alert(1)</script>";
        let html_esc = html_escape(p);

        // Raw reflection: body carries the payload verbatim, not the escaped form.
        let raw_body = "<html>pre <script>alert(1)</script> post</html>";
        assert!(raw_body.contains(p));
        assert!(!raw_body.contains(&html_esc));
        // Payload sits in a text node here (preceded by `>pre `), not script.
        assert_eq!(detect_context(raw_body, p), "text");

        // HTML-escaped reflection: the raw payload is gone, the escaped one present.
        let esc_body = "<p>pre &lt;script&gt;alert(1)&lt;/script&gt; post</p>";
        assert!(!esc_body.contains(p));
        assert!(esc_body.contains(&html_esc));
    }

    #[test]
    fn preview는_매치_주변_윈도우를_utf8_안전하게_자른다() {
        // ASCII: ±40 window around the match.
        let body = format!("{}PAYLOAD{}", "a".repeat(100), "b".repeat(100));
        let p = preview(&body, "PAYLOAD");
        assert_eq!(p, format!("{}PAYLOAD{}", "a".repeat(40), "b".repeat(40)));
        // Multi-byte neighbours must not panic and must stay on char boundaries.
        let body = format!("{}PAYLOAD{}", "가".repeat(50), "나".repeat(50));
        let out = preview(&body, "PAYLOAD");
        assert!(out.contains("PAYLOAD"));
        // No reflection → empty.
        assert_eq!(preview("nope", "PAYLOAD"), "");
    }
}
