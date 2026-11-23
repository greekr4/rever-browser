//! Port of `src/main/mcp/tools/cmd_probe.ts` — OS command injection probe.
//! Fires ONE nonce-wrapped arithmetic payload per break-out context
//! (single/double quote, semicolon, pipe, backtick, `$()`). A context counts as
//! confirmed only when the shell's computed output (`nonce{a*b}nonce`) shows up
//! in the response — a literal echo of the payload proves nothing.

use std::sync::PoisonError;
use std::time::Duration;

use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef};
use rever_tools_api::utils::{ToolResult, err, ok};
use rever_tools_http::{build_request_spec, restore_marker, send};
use serde_json::{Value, json};

use crate::args::{text, validate};
use crate::common::{
    apply_payload, encode_uri_component, form_slot_to_marker, has_marker, is_form_urlencoded,
    pick_prime, random_nonce, url_slot_to_marker,
};
use crate::{pretty, schema};

const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// One break-out context's payload, plus the shell-computed string that only
/// appears if the shell actually executed the echo.
struct CmdCase {
    context: &'static str,
    payload: String,
    /// Appears in the body ONLY if the shell executed the echo.
    expected: String,
}

/// Build the six break-out payloads (verbatim from cmd_probe.ts). The arithmetic
/// `$(( a * b ))` goes in as a literal; its product only shows up once a shell
/// evaluates it.
fn build_cmd_cases(a: u32, b: u32, nonce: &str) -> Vec<CmdCase> {
    let prod = a * b;
    let expected = format!("{nonce}{prod}{nonce}");
    let echo = format!("echo {nonce}$(( {a} * {b} )){nonce}");
    [
        ("single-quote", format!("'; {echo} || '")),
        ("double-quote", format!("\"; {echo} || \"")),
        ("semicolon", format!("; {echo}")),
        ("pipe", format!("| {echo}")),
        ("backtick", format!("`{echo}`")),
        ("subshell", format!("$({echo})")),
    ]
    .into_iter()
    .map(|(context, payload)| CmdCase {
        context,
        payload,
        expected: expected.clone(),
    })
    .collect()
}

/// A case evaluated iff the shell's computed output is in the body.
fn cmd_evaluated(body: &str, c: &CmdCase) -> bool {
    body.contains(&c.expected)
}

/// Lower a byte index to the nearest char boundary at or below it.
fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Raise a byte index to the nearest char boundary at or above it.
fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// `body.slice(idx-30, idx+expected.length+30)`, boundary-safe (expected is
/// ASCII so its own offset is a boundary); empty when not evaluated.
fn preview(body: &str, c: &CmdCase, evaluated: bool) -> String {
    if !evaluated {
        return String::new();
    }
    let Some(idx) = body.find(&c.expected) else {
        return String::new();
    };
    let start = floor_char_boundary(body, idx.saturating_sub(30));
    let end = ceil_char_boundary(body, (idx + c.expected.len() + 30).min(body.len()));
    body[start..end].to_owned()
}

pub struct CmdProbe;

impl Tool for CmdProbe {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "cmd_probe",
            "Confirm OS command injection at a reflecting parameter. Fires ONE nonce-wrapped arithmetic payload per break-out context (quote break, ;, |, backtick, $()) and reports which context let the SHELL execute — the computed result (nonce{a*b}nonce), not the literal payload, appears in the response. Pass param=<query or form field name> and the tool marks the slot itself. Use this instead of concluding \"not injectable\" from a payload echoed back or a quote-mangled error: date/format/exec-style params (strftime, template date, shell-built strings) are the classic carriers. A positive result is RCE-class; escalate severity accordingly.",
            schema(
                json!({
                    "requestId": { "type": "string" },
                    "param": { "type": "string", "description": "Query or form field that carries the value (e.g. \"format\") — the tool marks it with § itself" },
                    "a": { "type": "integer", "description": "First factor (test determinism; default random prime)" },
                    "b": { "type": "integer", "description": "Second factor (default random prime)" }
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
            let param = text(&args, "param");
            let a = args["a"]
                .as_u64()
                .map(|n| n as u32)
                .unwrap_or_else(pick_prime);
            let b = args["b"]
                .as_u64()
                .map(|n| n as u32)
                .unwrap_or_else(pick_prime);
            let nonce = random_nonce();

            // Lock only to build the spec; the sends below must not hold it.
            let mut base = {
                let store = ctx.traffic.lock().unwrap_or_else(PoisonError::into_inner);
                match build_request_spec(&store, Some(&request_id), None) {
                    Ok(spec) => restore_marker(spec),
                    Err(error) => return err(error),
                }
            };

            if let Some(param) = param {
                if base
                    .url
                    .contains(&format!("{}=", encode_uri_component(param)))
                {
                    base.url = url_slot_to_marker(&base.url, param);
                } else {
                    base.body = Some(form_slot_to_marker(
                        base.body.as_deref().unwrap_or(""),
                        param,
                    ));
                }
            }
            if !has_marker(&base) {
                return err(
                    "no injectable slot: pass param=<query or form field name> (e.g. param: \"format\") or stage a request with a literal § marker first",
                );
            }

            let form_body = is_form_urlencoded(&base);
            let cases = build_cmd_cases(a, b, &nonce);
            let mut results = Vec::with_capacity(cases.len());
            let mut contexts: Vec<&'static str> = Vec::new();
            for case in &cases {
                let variant = apply_payload(&base, &case.payload, form_body);
                let out = send(&variant, SEND_TIMEOUT).await;
                let evaluated = cmd_evaluated(&out.body, case);
                if evaluated {
                    contexts.push(case.context);
                }
                results.push(json!({
                    "context": case.context,
                    "status": out.status,
                    "evaluated": evaluated,
                    "preview": preview(&out.body, case, evaluated),
                }));
            }

            let cmdi = !contexts.is_empty();
            ok(pretty(&json!({
                "cmdi": cmdi,
                "contexts": contexts,
                "note": if cmdi {
                    "Command injection confirmed (shell evaluated the arithmetic) — RCE-class, record as critical."
                } else {
                    "No context let the shell execute; the value is echoed but not executed (consider reflected XSS instead)."
                },
                "results": results,
            })))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::url_slot_to_marker;

    #[test]
    fn 탈출_컨텍스트별_페이로드를_만들고_기대결과는_산술곱을_nonce로_감싼다() {
        let cases = build_cmd_cases(101, 103, "zXYZz");
        let prod = 101 * 103; // 10403
        let contexts: Vec<&str> = cases.iter().map(|c| c.context).collect();
        assert_eq!(
            contexts,
            [
                "single-quote",
                "double-quote",
                "semicolon",
                "pipe",
                "backtick",
                "subshell"
            ]
        );
        for c in &cases {
            assert_eq!(c.expected, format!("zXYZz{prod}zXYZz"));
            // The arithmetic goes in literally; the product does not.
            assert!(c.payload.contains("$(( 101 * 103 ))"));
            assert!(!c.payload.contains(&prod.to_string()));
        }
    }

    #[test]
    fn 셸이_실행한_경우만_confirmed_리터럴_반사는_아니다() {
        let cases = build_cmd_cases(101, 103, "zXYZz");
        let sq = &cases[0];
        assert!(cmd_evaluated("<h1>zXYZz10403zXYZz</h1>", sq));
        assert!(!cmd_evaluated(&format!("<h1>{}</h1>", sq.payload), sq));
        assert!(!cmd_evaluated(
            "error: sh: 1: Syntax error: '' unmatched",
            sq
        ));
    }

    #[test]
    fn url_slot은_기존_쿼리_파라미터_값을_마커로_바꾼다() {
        assert_eq!(
            url_slot_to_marker("http://x/?format=%Y-%m-%d&b=2", "format"),
            "http://x/?format=§&b=2"
        );
        assert_eq!(
            url_slot_to_marker("http://x/?a=1&fmt=1", "fmt"),
            "http://x/?a=1&fmt=§"
        );
    }

    #[test]
    fn url_slot은_파라미터가_없으면_추가한다() {
        assert_eq!(
            url_slot_to_marker("http://x/", "format"),
            "http://x/?format=§"
        );
        assert_eq!(
            url_slot_to_marker("http://x/?a=1", "format"),
            "http://x/?a=1&format=§"
        );
    }

    #[test]
    fn url_slot은_파라미터명의_정규식_특수문자는_이스케이프된다() {
        assert_eq!(
            url_slot_to_marker("http://x/?f.mt=1", "f.mt"),
            "http://x/?f.mt=§"
        );
    }
}
