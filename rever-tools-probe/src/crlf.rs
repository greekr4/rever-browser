//! Port of `src/main/mcp/tools/crlf.ts` — inject CRLF payloads at the `§`
//! marker and report whether an injected header or script survived into the
//! response (header injection / log forgery / open-redirect chains).

use std::sync::PoisonError;
use std::time::Duration;

use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef};
use rever_tools_api::utils::{ToolResult, err, ok};
use rever_tools_http::{RequestSpec, build_request_spec, restore_marker, send};
use serde_json::{Value, json};

use crate::args::{strings, text, validate};
use crate::common::has_marker;
use crate::{pretty, schema};

/// The default CRLF payload set, verbatim from crlf.ts.
const CRLF_PAYLOADS: &[&str] = &[
    "\r\nX-Injected: 1",
    "%0d%0aX-Injected:%201",
    "%0d%0aSet-Cookie:%20injected=1",
    "\r\n\r\n<script>alert(1)</script>",
    // unicode CR/LF bypass
    "%E5%98%8A%E5%98%8DX-Injected: 1",
    "%0aSet-Cookie:%20pwn=1",
];

const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// Replace every `§` marker in the url, header values, and body with `payload`.
fn substitute(base: &RequestSpec, payload: &str) -> RequestSpec {
    RequestSpec {
        url: base.url.replace('§', payload),
        method: base.method.clone(),
        headers: base
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), value.replace('§', payload)))
            .collect(),
        body: base.body.as_deref().map(|body| body.replace('§', payload)),
    }
}

pub struct CrlfTest;

impl Tool for CrlfTest {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "crlf_test",
            "Inject CRLF payloads into a request URL/body/header (marker `§`) and report whether injected headers appear in the response (header injection / log forgery / open redirect chains).",
            schema(
                json!({
                    "requestId": { "type": "string", "description": "Base requestId with § marker" },
                    "payloads": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Override default CRLF payloads (max 500)"
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
                .unwrap_or_else(|| CRLF_PAYLOADS.iter().map(|p| (*p).to_owned()).collect());
            // zod's `.max(500)`: don't fan out past the cap.
            let list: Vec<String> = list.into_iter().take(500).collect();

            let mut results = Vec::with_capacity(list.len());
            let mut vulnerable = false;
            for payload in &list {
                let out = send(&substitute(&base, payload), SEND_TIMEOUT).await;
                let injected = out.header("x-injected").is_some();
                let script = out.body.contains("<script>alert(1)</script>");
                if injected || script {
                    vulnerable = true;
                }
                let headers: serde_json::Map<String, Value> = out
                    .headers
                    .iter()
                    .map(|(name, value)| (name.clone(), json!(value)))
                    .collect();
                results.push(json!({
                    "payload": payload,
                    "status": out.status,
                    "injectedHeaderPresent": injected,
                    "responseHeaders": headers,
                    "bodyHasScript": script,
                }));
            }

            ok(pretty(&json!({ "vulnerable": vulnerable, "results": results })))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rever_core::traffic::Headers;

    fn spec(url: &str, headers: &[(&str, &str)], body: Option<&str>) -> RequestSpec {
        RequestSpec {
            url: url.to_owned(),
            method: "GET".to_owned(),
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect::<Headers>(),
            body: body.map(str::to_owned),
        }
    }

    #[test]
    fn 마커는_url_헤더_바디_어디에_있어도_찾는다() {
        assert!(has_marker(&spec("https://x.test/a?q=§", &[], None)));
        assert!(has_marker(&spec("https://x.test/a", &[("X-H", "§")], None)));
        assert!(has_marker(&spec("https://x.test/a", &[], Some("b=§"))));
        assert!(!has_marker(&spec("https://x.test/a", &[("X-H", "v")], Some("b=1"))));
    }

    #[test]
    fn 치환은_모든_필드의_마커를_페이로드로_바꾼다() {
        let base = spec("https://x.test/a?q=§", &[("X-H", "pre§post")], Some("b=§"));
        let out = substitute(&base, "\r\nX-Injected: 1");
        assert_eq!(out.url, "https://x.test/a?q=\r\nX-Injected: 1");
        assert_eq!(out.headers.get("X-H").map(String::as_str), Some("pre\r\nX-Injected: 1post"));
        assert_eq!(out.body.as_deref(), Some("b=\r\nX-Injected: 1"));
        // A field without a marker is untouched.
        assert_eq!(out.method, "GET");
    }

    #[test]
    fn 기본_페이로드_세트는_crlf_ts와_같다() {
        assert_eq!(CRLF_PAYLOADS.len(), 6);
        assert_eq!(CRLF_PAYLOADS[0], "\r\nX-Injected: 1");
        assert_eq!(CRLF_PAYLOADS[3], "\r\n\r\n<script>alert(1)</script>");
    }
}
