//! Port of `src/main/mcp/tools/ssti_probe.ts` — server-side template injection
//! probe. Unlike a reflection/XSS probe (which looks for the *literal* payload
//! echoed back), SSTI is confirmed only when the template engine EVALUATES an
//! arithmetic expression: the payload text disappears and its computed result,
//! wrapped in a random nonce, shows up instead — so a coincidental match is
//! essentially impossible.

use std::sync::PoisonError;
use std::time::Duration;

use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef};
use rever_tools_api::utils::{ToolResult, err, ok};
use rever_tools_http::{build_request_spec, restore_marker, send};
use serde_json::{Value, json};

use crate::args::{text, validate};
use crate::common::{
    apply_payload, form_slot_to_marker, has_marker, is_form_urlencoded, pick_prime, random_nonce,
};
use crate::{pretty, schema};

const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// One arithmetic payload for a template engine. `expected` appears in the body
/// ONLY if the engine evaluated the payload.
struct SstiCase {
    engine: &'static str,
    payload: String,
    expected: String,
}

/// Build one arithmetic payload per template engine. `a*b` is a product of two
/// primes (distinctive, never a value like 49 that a page might print) wrapped
/// in `nonce…nonce`. Evaluated output = `nonce{a*b}nonce` (= `expected`);
/// un-evaluated output still carries the raw expression, which never matches.
fn build_ssti_cases(a: u32, b: u32, nonce: &str) -> Vec<SstiCase> {
    let prod = a * b;
    let expected = format!("{nonce}{prod}{nonce}");
    // Literal braces/dollars live in plain string literals (not format args) so
    // there is no brace-escaping to get wrong.
    let ab = format!("{a}*{b}");
    vec![
        // Freemarker / JSP EL / Mako / generic `${…}`
        SstiCase {
            engine: "freemarker/jsp-el/generic ${}",
            payload: nonce.to_owned() + "${" + &ab + "}" + nonce,
            expected: expected.clone(),
        },
        // Apache Velocity — needs #set; ${…} alone is a reference, not arithmetic
        SstiCase {
            engine: "velocity",
            payload: "#set($q=".to_owned() + &ab + ")" + nonce + "${q}" + nonce,
            expected: expected.clone(),
        },
        // Jinja2 / Twig / Nunjucks
        SstiCase {
            engine: "jinja2/twig",
            payload: nonce.to_owned() + "{{" + &ab + "}}" + nonce,
            expected: expected.clone(),
        },
        // ERB / EJS
        SstiCase {
            engine: "erb/ejs",
            payload: nonce.to_owned() + "<%= " + &ab + " %>" + nonce,
            expected: expected.clone(),
        },
        // Thymeleaf inline
        SstiCase {
            engine: "thymeleaf",
            payload: nonce.to_owned() + "[[${" + &ab + "}]]" + nonce,
            expected: expected.clone(),
        },
        // Razor
        SstiCase {
            engine: "razor",
            payload: nonce.to_owned() + "@(" + &ab + ")" + nonce,
            expected,
        },
    ]
}

/// A case evaluated iff the computed result (not the literal payload) is in body.
fn ssti_evaluated(body: &str, case: &SstiCase) -> bool {
    body.contains(&case.expected)
}

/// UTF-8-safe slice that only ever widens to a char boundary, so a snippet
/// around a byte offset never splits a multi-byte char.
fn safe_slice(body: &str, start: usize, end: usize) -> &str {
    let mut s = start.min(body.len());
    while s > 0 && !body.is_char_boundary(s) {
        s -= 1;
    }
    let mut e = end.min(body.len());
    while e < body.len() && !body.is_char_boundary(e) {
        e += 1;
    }
    &body[s..e]
}

pub struct SstiProbe;

impl Tool for SstiProbe {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "ssti_probe",
            "Confirm server-side template injection at a reflecting parameter. Fires ONE arithmetic payload per template engine (Velocity, Freemarker/JSP-EL, Jinja2/Twig, ERB/EJS, Thymeleaf, Razor) and reports which engine EVALUATED it — i.e. the computed result (not the literal payload) appears in the response. Pass param=<form field name> (e.g. param: \"text\") and the tool marks the slot itself; no pre-staged § marker needed. Use this instead of concluding \"not SSTI\" from a literal ${7*7}/{{7*7}} echo: Java stacks only evaluate #set/${…} forms, never {{…}}. A positive result is RCE-class (template eval → code execution); escalate severity accordingly.",
            schema(
                json!({
                    "requestId": { "type": "string" },
                    "param": { "type": "string", "description": "Form field that reflects input (e.g. \"text\") — the tool marks it with § itself" },
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
            let param = text(&args, "param").map(str::to_owned);
            let a = args["a"].as_u64().map(|n| n as u32);
            let b = args["b"].as_u64().map(|n| n as u32);

            let mut base = {
                let store = ctx.traffic.lock().unwrap_or_else(PoisonError::into_inner);
                match build_request_spec(&store, Some(&request_id), None) {
                    Ok(spec) => restore_marker(spec),
                    Err(error) => return err(error),
                }
            };

            if let Some(param) = &param {
                // ssti marks ONLY the body.
                base.body = Some(form_slot_to_marker(base.body.as_deref().unwrap_or(""), param));
            }
            if !has_marker(&base) {
                return err(
                    "no injectable slot: pass param=<form field name> (e.g. param: \"text\") or stage a request with a literal § marker first",
                );
            }

            let form_body = is_form_urlencoded(&base);
            let a = a.unwrap_or_else(pick_prime);
            let b = b.unwrap_or_else(pick_prime);
            let nonce = random_nonce();
            let cases = build_ssti_cases(a, b, &nonce);

            let mut results = Vec::with_capacity(cases.len());
            let mut engines: Vec<&'static str> = Vec::new();
            for case in &cases {
                let variant = apply_payload(&base, &case.payload, form_body);
                let out = send(&variant, SEND_TIMEOUT).await;
                let evaluated = ssti_evaluated(&out.body, case);
                if evaluated {
                    engines.push(case.engine);
                }
                let preview = if evaluated {
                    match out.body.find(&case.expected) {
                        Some(idx) => safe_slice(
                            &out.body,
                            idx.saturating_sub(30),
                            idx + case.expected.len() + 30,
                        )
                        .to_owned(),
                        None => String::new(),
                    }
                } else {
                    String::new()
                };
                results.push(json!({
                    "engine": case.engine,
                    "status": out.status,
                    "evaluated": evaluated,
                    "preview": preview,
                }));
            }

            let ssti = !engines.is_empty();
            let note = if ssti {
                "SSTI confirmed (template evaluated arithmetic) — RCE-class, record as critical."
            } else {
                "No engine evaluated the payload; input is reflected literally (consider reflected XSS, not SSTI)."
            };
            ok(pretty(&json!({
                "ssti": ssti,
                "engines": engines,
                "note": note,
                "results": results,
            })))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::form_slot_to_marker;

    #[test]
    fn 엔진별_페이로드를_만들고_기대결과는_산술곱을_nonce로_감싼다() {
        let cases = build_ssti_cases(101, 103, "zABCz");
        let prod = 101 * 103; // 10403
        assert!(cases.len() >= 6);
        for c in &cases {
            assert_eq!(c.expected, format!("zABCz{prod}zABCz"));
            // 페이로드에는 산술식이 리터럴로 들어 있고, 아직 계산 결과는 없다
            assert!(c.payload.contains("101"));
            assert!(c.payload.contains("103"));
            assert!(!c.payload.contains(&prod.to_string()));
        }
        let engines: Vec<&str> = cases.iter().map(|c| c.engine).collect();
        assert!(engines.contains(&"velocity"));
        assert!(engines.contains(&"jinja2/twig"));
    }

    #[test]
    fn 계산결과가_응답에_있으면_confirmed_리터럴만_있으면_아니다() {
        let cases = build_ssti_cases(101, 103, "zABCz");
        let generic = &cases[0];
        // 엔진이 평가한 경우: nonce + 10403 + nonce 가 응답에 나타난다
        assert!(ssti_evaluated("<h2>zABCz10403zABCz</h2>", generic));
        // 평가 안 된 경우: 페이로드가 리터럴로 반사(계산 안 됨)
        assert!(!ssti_evaluated(&format!("<h2>{}</h2>", generic.payload), generic));
        // 무관한 본문
        assert!(!ssti_evaluated("<h2>Example text</h2>", generic));
    }

    #[test]
    fn velocity_페이로드는_set과_nonce로_감싼_참조를_쓴다() {
        let cases = build_ssti_cases(101, 103, "zABCz");
        let velocity = cases.iter().find(|c| c.engine == "velocity").unwrap();
        assert!(velocity.payload.contains("#set($q=101*103)"));
        assert!(velocity.payload.contains("zABCz${q}zABCz"));
    }

    // formSlotToMarker lives in common.rs; its cases are mirrored here per the
    // TS ssti_probe.test.ts `describe('formSlotToMarker')`.
    #[test]
    fn 기존_폼_필드_값을_마커로_바꾼다() {
        assert_eq!(form_slot_to_marker("text=hello+world", "text"), "text=§");
        assert_eq!(
            form_slot_to_marker("a=1&text=%7B%7B7%2A7%7D%7D&b=2", "text"),
            "a=1&text=§&b=2"
        );
    }

    #[test]
    fn 필드가_없으면_추가한다() {
        assert_eq!(form_slot_to_marker("", "text"), "text=§");
        assert_eq!(form_slot_to_marker("a=1", "text"), "a=1&text=§");
    }

    #[test]
    fn 파라미터명의_정규식_특수문자는_이스케이프된다() {
        assert_eq!(form_slot_to_marker("q.x=1", "q.x"), "q.x=§");
    }

    #[test]
    fn 빈_형제_필드는_더미_값으로_채운다() {
        assert_eq!(
            form_slot_to_marker("username=&password=", "username"),
            "username=§&password=reverprobe"
        );
        assert_eq!(
            form_slot_to_marker("a=&b=&c=1", "c"),
            "a=reverprobe&b=reverprobe&c=§"
        );
        assert_eq!(form_slot_to_marker("user=admin&pass=", "user"), "user=§&pass=reverprobe");
    }
}
