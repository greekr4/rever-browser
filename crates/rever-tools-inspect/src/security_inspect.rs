//! Port of `src/main/mcp/tools/security_inspect.ts` — the `security_inspect` tool.

use std::collections::BTreeMap;

use rever_core::traffic::{Headers, ListFilter};
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::args::validate;
use crate::js::json_stringify;
use crate::traffic;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SecurityIssue {
    pub severity: Severity,
    pub header: &'static str,
    pub message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
}

const SECURITY_HEADERS: [&str; 9] = [
    "content-security-policy",
    "strict-transport-security",
    "x-frame-options",
    "x-content-type-options",
    "referrer-policy",
    "permissions-policy",
    "cross-origin-opener-policy",
    "cross-origin-resource-policy",
    "cross-origin-embedder-policy",
];

fn lowercase_names(headers: &Headers) -> BTreeMap<String, &str> {
    headers
        .iter()
        .map(|(name, value)| (name.to_lowercase(), value.as_str()))
        .collect()
}

/// `/max-age=\d+/.test(hsts)`.
fn has_max_age(hsts: &str) -> bool {
    hsts.match_indices("max-age=").any(|(at, key)| {
        hsts.as_bytes()
            .get(at + key.len())
            .is_some_and(u8::is_ascii_digit)
    })
}

pub fn analyze(headers: &Headers) -> Vec<SecurityIssue> {
    let lc = lowercase_names(headers);
    let header = |name: &str| lc.get(name).copied();
    let issue = |severity, header, message, current: Option<&str>| SecurityIssue {
        severity,
        header,
        message,
        current: current.map(str::to_owned),
    };

    let mut out = Vec::new();

    let csp = header("content-security-policy").filter(|csp| !csp.is_empty());
    match csp {
        None => out.push(issue(
            Severity::High,
            "content-security-policy",
            "no CSP set",
            None,
        )),
        Some(csp) => {
            if csp.contains("'unsafe-inline'") {
                let message = "CSP allows 'unsafe-inline'";
                out.push(issue(Severity::Medium, "csp", message, Some(csp)));
            }
            if csp.contains("'unsafe-eval'") {
                let message = "CSP allows 'unsafe-eval'";
                out.push(issue(Severity::Medium, "csp", message, Some(csp)));
            }
            if csp.contains('*') {
                out.push(issue(Severity::Low, "csp", "CSP uses wildcard", Some(csp)));
            }
        }
    }

    match header("strict-transport-security").filter(|hsts| !hsts.is_empty()) {
        None => out.push(issue(
            Severity::Medium,
            "strict-transport-security",
            "no HSTS",
            None,
        )),
        Some(hsts) if !has_max_age(hsts) || hsts.contains("max-age=0") => out.push(issue(
            Severity::Low,
            "hsts",
            "HSTS max-age missing or zero",
            Some(hsts),
        )),
        Some(_) => {}
    }

    let frame_options = header("x-frame-options").filter(|value| !value.is_empty());
    if frame_options.is_none() && !csp.unwrap_or("").contains("frame-ancestors") {
        out.push(issue(
            Severity::Medium,
            "x-frame-options",
            "clickjacking protection (X-Frame-Options or CSP frame-ancestors) absent",
            None,
        ));
    }

    let content_type_options = header("x-content-type-options");
    if content_type_options.map(str::to_lowercase).as_deref() != Some("nosniff") {
        out.push(issue(
            Severity::Low,
            "x-content-type-options",
            "missing nosniff",
            content_type_options,
        ));
    }

    if header("access-control-allow-origin") == Some("*")
        && header("access-control-allow-credentials") == Some("true")
    {
        out.push(issue(
            Severity::High,
            "cors",
            "wildcard ACAO + credentials is invalid and may leak",
            None,
        ));
    }

    out
}

// A Document whose headers never landed (failed navigation, cache hit where
// headers only arrive via ExtraInfo, a response still in flight, or an evicted
// ring-buffer entry) must not be audited as an empty header map — that reads
// as "no security headers" and mints false positives. Pick the newest
// candidate that actually has response headers.
pub fn pick_inspectable<'a, 'h, T>(
    candidates: &'a [T],
    request_id: impl Fn(&T) -> &str,
    response_headers: impl Fn(&str) -> Option<&'h Headers>,
) -> Option<&'a T> {
    candidates
        .iter()
        .find(|candidate| response_headers(request_id(candidate)).is_some_and(|h| !h.is_empty()))
}

fn security_inspect(ctx: &ToolContext, url: Option<&str>) -> ToolResult {
    let store = traffic(ctx);
    let filter = ListFilter {
        limit: Some(200),
        ..ListFilter::default()
    };
    let candidates: Vec<_> = store
        .list(&filter)
        .into_iter()
        .filter(|r| match url.filter(|url| !url.is_empty()) {
            Some(url) => r.url == url,
            None => r.resource_type == "Document",
        })
        .collect();
    if candidates.is_empty() {
        return err("no matching request in traffic store");
    }
    let headers_of = |id: &str| {
        store
            .get(id)
            .and_then(|full| full.response_headers.as_ref())
    };
    let Some(r) = pick_inspectable(&candidates, |r| &r.request_id, headers_of) else {
        return err(
            "no matching request with response headers yet (response may still be in flight)",
        );
    };
    let empty = Headers::new();
    let headers = headers_of(&r.request_id).unwrap_or(&empty);
    let issues = analyze(headers);
    let lc = lowercase_names(headers);
    let present: Map<String, Value> = SECURITY_HEADERS
        .iter()
        .filter_map(|name| Some(((*name).to_owned(), json!(lc.get(*name)?))))
        .collect();
    ok(json_stringify(
        &json!({ "url": r.url, "securityHeaders": present, "issues": issues }),
        true,
    ))
}

pub struct SecurityInspect;

impl Tool for SecurityInspect {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "security_inspect",
            "Inspect security-relevant response headers for the active page (or a specific URL). Reports CSP/HSTS/XFO/CORS issues.",
            json!({
                "type": "object",
                "properties": {
                    "url": {
                        "description": "URL to inspect (defaults to most recent Document request)",
                        "type": "string"
                    }
                },
                "$schema": "http://json-schema.org/draft-07/schema#"
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(invalid) = validate(&self.def(), &args) {
                return invalid;
            }
            security_inspect(ctx, args.get("url").and_then(Value::as_str))
        })
    }
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(SecurityInspect);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rever_core::traffic::RequestPatch;

    use super::*;
    use crate::testing::{headers, request, run, store};

    // Headers measured on arqoven.com on 2026-11-02 — this set produced the
    // "no headers at all" false positive.
    fn arqoven_headers() -> Headers {
        headers(&[
            (
                "content-security-policy",
                "default-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
            ),
            ("strict-transport-security", "max-age=15552000"),
            ("x-frame-options", "DENY"),
            ("x-content-type-options", "nosniff"),
        ])
    }

    struct Candidate {
        request_id: &'static str,
    }

    fn candidates(ids: [&'static str; 2]) -> [Candidate; 2] {
        ids.map(|request_id| Candidate { request_id })
    }

    // --- ported from security_inspect.test.ts ---

    #[test]
    fn 헤더가_비어_있는_document를_건너뛰고_헤더_있는_최신_항목을_고른다() {
        let store = HashMap::from([
            ("a", Headers::new()), // a cache hit, a failed navigation, …
            ("b", arqoven_headers()),
        ]);
        let list = candidates(["a", "b"]);
        let picked = pick_inspectable(&list, |c| c.request_id, |id| store.get(id));
        assert_eq!(picked.map(|c| c.request_id), Some("b"));
    }

    #[test]
    fn get_request가_none을_돌려주는_밀려난_엔트리도_건너뛴다() {
        let full = arqoven_headers();
        let list = candidates(["gone", "ok"]);
        let picked = pick_inspectable(&list, |c| c.request_id, |id| (id == "ok").then_some(&full));
        assert_eq!(picked.map(|c| c.request_id), Some("ok"));
    }

    #[test]
    fn 전부_비어_있으면_none이다_빈_맵_감사_금지() {
        let empty = Headers::new();
        let list = candidates(["a", "b"]);
        let picked = pick_inspectable(&list, |c| c.request_id, |_| Some(&empty));
        assert!(picked.is_none());
    }

    #[test]
    fn 헤더가_온전히_있으면_csp_hsts_xfo_nosniff_이슈를_내지_않는다() {
        let issues = analyze(&arqoven_headers());
        let headers: Vec<&str> = issues.iter().map(|i| i.header).collect();
        assert!(!headers.contains(&"content-security-policy"));
        assert!(!headers.contains(&"strict-transport-security"));
        assert!(!headers.contains(&"x-frame-options"));
        assert!(!headers.contains(&"x-content-type-options"));
    }

    // --- not covered by the TypeScript tests ---

    /// The issues as `severity header`, in order.
    fn found(pairs: &[(&str, &str)]) -> Vec<String> {
        analyze(&headers(pairs))
            .iter()
            .map(|issue| {
                let severity = serde_json::to_value(issue.severity).unwrap_or_default();
                format!("{} {}", severity.as_str().unwrap_or(""), issue.header)
            })
            .collect()
    }

    const NO_HEADERS: [&str; 4] = [
        "high content-security-policy",
        "medium strict-transport-security",
        "medium x-frame-options",
        "low x-content-type-options",
    ];

    #[test]
    fn 보안_헤더가_하나도_없으면_네_가지를_지적한다() {
        assert_eq!(found(&[("server", "nginx")]), NO_HEADERS);
        assert_eq!(found(&[]), NO_HEADERS);
    }

    #[test]
    fn 값이_빈_헤더는_없는_것으로_본다() {
        let issues = analyze(&headers(&[
            ("content-security-policy", ""),
            ("strict-transport-security", ""),
            ("x-frame-options", ""),
            ("x-content-type-options", ""),
        ]));
        let names: Vec<&str> = issues.iter().map(|i| i.header).collect();
        assert_eq!(
            names,
            [
                "content-security-policy",
                "strict-transport-security",
                "x-frame-options",
                "x-content-type-options"
            ]
        );
        // The empty nosniff value is still reported as the current one.
        assert_eq!(issues.last().and_then(|i| i.current.as_deref()), Some(""));
        assert_eq!(issues.first().and_then(|i| i.current.as_deref()), None);
    }

    #[test]
    fn csp의_unsafe_지시어와_와일드카드를_각각_지적한다() {
        let csp = "default-src * 'unsafe-inline' 'unsafe-eval'";
        let issues = analyze(&headers(&[("content-security-policy", csp)]));
        let csp_issues: Vec<(&str, Option<&str>)> = issues
            .iter()
            .filter(|i| i.header == "csp")
            .map(|i| (i.message, i.current.as_deref()))
            .collect();
        assert_eq!(
            csp_issues,
            [
                ("CSP allows 'unsafe-inline'", Some(csp)),
                ("CSP allows 'unsafe-eval'", Some(csp)),
                ("CSP uses wildcard", Some(csp)),
            ]
        );
        // Without the quotes it is not the keyword.
        assert!(
            !found(&[("content-security-policy", "script-src unsafe-inline")])
                .contains(&"medium csp".to_owned())
        );
    }

    #[test]
    fn hsts는_max_age_뒤에_숫자가_있고_0이_아니어야_한다() {
        let hsts_issue = |value: &str| {
            analyze(&headers(&[("strict-transport-security", value)]))
                .into_iter()
                .find(|issue| issue.header == "hsts")
        };
        assert!(hsts_issue("max-age=15552000").is_none());
        assert!(hsts_issue("max-age=x; max-age=31536000").is_none());
        for value in [
            "includeSubDomains",
            "max-age=",
            "max-age=0",
            "max-age=10; max-age=0",
            // An Arabic-Indic digit is not `\d` in JavaScript.
            "max-age=\u{663}; includeSubDomains",
            "MAX-AGE=10",
        ] {
            let issue = hsts_issue(value);
            assert_eq!(
                issue.as_ref().map(|i| (i.severity, i.message)),
                Some((Severity::Low, "HSTS max-age missing or zero")),
                "{value}"
            );
            assert_eq!(issue.and_then(|i| i.current), Some(value.to_owned()));
        }
        // "max-age=0" anywhere counts, even as the start of a longer number.
        assert!(hsts_issue("max-age=0123").is_some());
    }

    #[test]
    fn csp의_frame_ancestors가_x_frame_options를_대신한다() {
        let clickjacking = "medium x-frame-options".to_owned();
        assert!(
            found(&[("content-security-policy", "default-src 'self'")]).contains(&clickjacking)
        );
        assert!(
            !found(&[("content-security-policy", "frame-ancestors 'self'")])
                .contains(&clickjacking)
        );
        assert!(!found(&[("x-frame-options", "SAMEORIGIN")]).contains(&clickjacking));
    }

    #[test]
    fn nosniff는_대소문자만_무시하고_정확히_같아야_한다() {
        let nosniff_issue = |value: &str| {
            analyze(&headers(&[("x-content-type-options", value)]))
                .into_iter()
                .find(|issue| issue.header == "x-content-type-options")
        };
        assert!(nosniff_issue("nosniff").is_none());
        assert!(nosniff_issue("NoSniff").is_none());
        let issue = nosniff_issue("nosniff ");
        assert_eq!(issue.as_ref().map(|i| i.message), Some("missing nosniff"));
        assert_eq!(issue.and_then(|i| i.current), Some("nosniff ".to_owned()));
    }

    #[test]
    fn 와일드카드_cors와_자격_증명이_함께면_지적한다() {
        let cors = "high cors".to_owned();
        let both = [
            ("access-control-allow-origin", "*"),
            ("access-control-allow-credentials", "true"),
        ];
        assert!(found(&both).contains(&cors));
        // Both values are compared exactly.
        let upper = [
            ("access-control-allow-origin", "*"),
            ("access-control-allow-credentials", "TRUE"),
        ];
        assert!(!found(&upper).contains(&cors));
        assert!(!found(&[("access-control-allow-origin", "*")]).contains(&cors));
    }

    #[test]
    fn 헤더_이름의_대소문자는_무시한다() {
        let mixed = [
            (
                "Content-Security-Policy",
                "default-src 'self'; frame-ancestors 'none'",
            ),
            ("Strict-Transport-Security", "max-age=1"),
            ("X-Content-Type-Options", "nosniff"),
        ];
        assert!(found(&mixed).is_empty());
    }

    fn document(
        id: &str,
        url: &str,
        started_at: u64,
        response_headers: Option<Headers>,
    ) -> RequestPatch {
        RequestPatch {
            resource_type: Some("Document".into()),
            started_at: Some(started_at),
            response_headers,
            ..request(id, url)
        }
    }

    async fn inspect(ctx: &ToolContext, args: Value) -> (bool, String) {
        run(&SecurityInspect, ctx, args).await
    }

    const GOOD_REPORT: &str = r#"{
  "url": "https://site.test/good",
  "securityHeaders": {
    "content-security-policy": "default-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
    "strict-transport-security": "max-age=15552000",
    "x-frame-options": "DENY",
    "x-content-type-options": "nosniff"
  },
  "issues": []
}"#;
    const IN_FLIGHT: &str =
        "no matching request with response headers yet (response may still be in flight)";

    #[tokio::test]
    async fn 가장_최근_document의_헤더를_검사한다() {
        // The text the TypeScript tool returned for the same traffic.
        let expected = r#"{
  "url": "https://site.test/bad",
  "securityHeaders": {
    "content-security-policy": "default-src * 'unsafe-inline' 'unsafe-eval'",
    "strict-transport-security": "max-age=0",
    "x-content-type-options": "NoSniff",
    "referrer-policy": "no-referrer"
  },
  "issues": [
    {
      "severity": "medium",
      "header": "csp",
      "message": "CSP allows 'unsafe-inline'",
      "current": "default-src * 'unsafe-inline' 'unsafe-eval'"
    },
    {
      "severity": "medium",
      "header": "csp",
      "message": "CSP allows 'unsafe-eval'",
      "current": "default-src * 'unsafe-inline' 'unsafe-eval'"
    },
    {
      "severity": "low",
      "header": "csp",
      "message": "CSP uses wildcard",
      "current": "default-src * 'unsafe-inline' 'unsafe-eval'"
    },
    {
      "severity": "low",
      "header": "hsts",
      "message": "HSTS max-age missing or zero",
      "current": "max-age=0"
    },
    {
      "severity": "medium",
      "header": "x-frame-options",
      "message": "clickjacking protection (X-Frame-Options or CSP frame-ancestors) absent"
    },
    {
      "severity": "high",
      "header": "cors",
      "message": "wildcard ACAO + credentials is invalid and may leak"
    }
  ]
}"#;
        let ctx = ToolContext::for_tests();
        store(
            &ctx,
            document(
                "d1",
                "https://site.test/good",
                3000,
                Some(arqoven_headers()),
            ),
        );
        assert_eq!(
            inspect(&ctx, json!({})).await,
            (false, GOOD_REPORT.to_owned())
        );

        let bad = headers(&[
            (
                "Content-Security-Policy",
                "default-src * 'unsafe-inline' 'unsafe-eval'",
            ),
            ("Strict-Transport-Security", "max-age=0"),
            ("X-Content-Type-Options", "NoSniff"),
            ("Access-Control-Allow-Origin", "*"),
            ("Access-Control-Allow-Credentials", "true"),
            ("Referrer-Policy", "no-referrer"),
            ("Server", "x"),
        ]);
        store(
            &ctx,
            document("d2", "https://site.test/bad", 3001, Some(bad)),
        );
        assert_eq!(inspect(&ctx, json!({})).await, (false, expected.to_owned()));
        // An empty url means "the latest Document", like no url.
        assert_eq!(inspect(&ctx, json!({ "url": "" })).await.1, expected);
    }

    #[tokio::test]
    async fn 값이_빈_보안_헤더도_있는_헤더로_적는다() {
        let expected = r#"{
  "url": "https://site.test/odd",
  "securityHeaders": {
    "content-security-policy": "",
    "strict-transport-security": "includeSubDomains",
    "x-frame-options": "",
    "x-content-type-options": "",
    "permissions-policy": "",
    "cross-origin-opener-policy": "same-origin",
    "cross-origin-resource-policy": "same-origin",
    "cross-origin-embedder-policy": "require-corp"
  },
  "issues": [
    {
      "severity": "high",
      "header": "content-security-policy",
      "message": "no CSP set"
    },
    {
      "severity": "low",
      "header": "hsts",
      "message": "HSTS max-age missing or zero",
      "current": "includeSubDomains"
    },
    {
      "severity": "medium",
      "header": "x-frame-options",
      "message": "clickjacking protection (X-Frame-Options or CSP frame-ancestors) absent"
    },
    {
      "severity": "low",
      "header": "x-content-type-options",
      "message": "missing nosniff",
      "current": ""
    }
  ]
}"#;
        let odd = headers(&[
            ("content-security-policy", ""),
            ("strict-transport-security", "includeSubDomains"),
            ("x-frame-options", ""),
            ("x-content-type-options", ""),
            ("permissions-policy", ""),
            ("cross-origin-opener-policy", "same-origin"),
            ("cross-origin-resource-policy", "same-origin"),
            ("cross-origin-embedder-policy", "require-corp"),
            ("access-control-allow-origin", "*"),
            ("access-control-allow-credentials", "TRUE"),
        ]);
        let ctx = ToolContext::for_tests();
        store(
            &ctx,
            document("d4", "https://site.test/odd", 3003, Some(odd)),
        );
        assert_eq!(inspect(&ctx, json!({})).await, (false, expected.to_owned()));
    }

    #[tokio::test]
    async fn 헤더가_아직_없는_document는_건너뛰고_그_앞의_것을_검사한다() {
        let ctx = ToolContext::for_tests();
        assert_eq!(
            inspect(&ctx, json!({})).await,
            (true, "no matching request in traffic store".to_owned())
        );
        store(
            &ctx,
            document("d0", "https://site.test/empty", 2999, Some(Headers::new())),
        );
        assert_eq!(inspect(&ctx, json!({})).await, (true, IN_FLIGHT.to_owned()));

        store(
            &ctx,
            document(
                "d1",
                "https://site.test/good",
                3000,
                Some(arqoven_headers()),
            ),
        );
        store(
            &ctx,
            document("d8", "https://site.test/pending", 3007, None),
        );
        store(
            &ctx,
            document(
                "d9",
                "https://site.test/emptyheaders",
                3008,
                Some(Headers::new()),
            ),
        );
        assert_eq!(
            inspect(&ctx, json!({})).await,
            (false, GOOD_REPORT.to_owned())
        );
    }

    #[tokio::test]
    async fn url을_주면_종류와_상관없이_정확히_같은_url의_요청을_검사한다() {
        let ctx = ToolContext::for_tests();
        store(
            &ctx,
            document(
                "d1",
                "https://site.test/good",
                3000,
                Some(arqoven_headers()),
            ),
        );
        store(
            &ctx,
            document("d8", "https://site.test/pending", 3007, None),
        );
        store(
            &ctx,
            RequestPatch {
                response_headers: Some(headers(&[("x-frame-options", "DENY")])),
                ..request("x1", "https://site.test/api")
            },
        );
        let args = json!({ "url": "https://site.test/good" });
        assert_eq!(inspect(&ctx, args).await, (false, GOOD_REPORT.to_owned()));

        // A non-Document request is found by url, never by default.
        let (failed, text) = inspect(&ctx, json!({ "url": "https://site.test/api" })).await;
        assert!(!failed);
        assert!(text.starts_with("{\n  \"url\": \"https://site.test/api\",\n  \"securityHeaders\": {\n    \"x-frame-options\": \"DENY\"\n  },\n"));

        let args = json!({ "url": "https://site.test/pending" });
        assert_eq!(inspect(&ctx, args).await, (true, IN_FLIGHT.to_owned()));
        for url in [
            "https://site.test/nope",
            "https://site.test/good/",
            "https://site.test/GOOD",
        ] {
            assert_eq!(
                inspect(&ctx, json!({ "url": url })).await,
                (true, "no matching request in traffic store".to_owned()),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn 최근_200개_요청_안에서만_찾는다() {
        let ctx = ToolContext::for_tests();
        store(
            &ctx,
            document(
                "d1",
                "https://site.test/good",
                3000,
                Some(arqoven_headers()),
            ),
        );
        for index in 0..199 {
            store(&ctx, request(&format!("x{index}"), "https://site.test/api"));
        }
        assert_eq!(
            inspect(&ctx, json!({})).await,
            (false, GOOD_REPORT.to_owned())
        );
        store(&ctx, request("x199", "https://site.test/api"));
        assert_eq!(
            inspect(&ctx, json!({})).await,
            (true, "no matching request in traffic store".to_owned())
        );
    }

    #[tokio::test]
    async fn url이_문자열이_아니면_검증_오류다() {
        let (failed, text) = inspect(&ToolContext::for_tests(), json!({ "url": 5 })).await;
        assert!(failed);
        assert_eq!(
            text,
            "MCP error -32602: Input validation error: Invalid arguments for tool security_inspect: [\n  {\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"url\"\n    ],\n    \"message\": \"Invalid input: expected string, received number\"\n  }\n]"
        );
    }
}
