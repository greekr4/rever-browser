//! Port of `repeater_send` from `src/main/mcp/tools/repeater.ts` — resend a
//! captured request (or a fresh one) through the ACTIVE TAB's `fetch`, so it
//! behaves like the real browser (cookies from the jar, TLS, HTTP/2, CORS).
//!
//! This is the engine counterpart to `replay_request` (which sends via reqwest,
//! independent of the browser). The engine's `intercept::on_load` applies the
//! capture-not-send fence to the in-page fetch automatically; this tool only
//! adds the up-front scope check, as the TypeScript's `repeaterSendRaw` did.

use rever_scope::scope::scope_block_for_url;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Value, json};

use crate::request_spec::{RequestSpec, build_request_spec, parse_mods};
use crate::{pretty, schema};

/// Response body cap (first 64KB), as the TypeScript's `MAX_BODY_BYTES`.
const MAX_BODY_BYTES: usize = 64 * 1024;
/// In-page fetch timeout; below the engine's evaluate timeout so the
/// AbortController fires first and frees the connection slot.
const FETCH_TIMEOUT_MS: u64 = 15_000;

/// fetch refuses to set these; strip them from copied + caller headers so the
/// page-side fetch doesn't throw. (`credentials:'include'` re-adds Cookie etc.
/// from the jar.) Mirrors repeater.ts's FORBIDDEN_HEADER_RE.
fn forbidden_fetch_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    const EXACT: [&str; 19] = [
        "host",
        "connection",
        "content-length",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "keep-alive",
        "te",
        "expect",
        "cookie2",
        "date",
        "origin",
        "referer",
        "user-agent",
        "accept-charset",
        "accept-encoding",
        "access-control-request-headers",
        "access-control-request-method",
        "via",
    ];
    EXACT.contains(&n.as_str()) || n.starts_with("sec-") || n.starts_with("proxy-")
}

/// Build the in-page fetch expression. Placeholders are replaced rather than
/// `format!`-interpolated to avoid escaping the many `{}` in the JS body.
fn build_eval_expression(spec: &RequestSpec) -> String {
    let headers: serde_json::Map<String, Value> = spec
        .headers
        .iter()
        .filter(|(name, _)| !forbidden_fetch_header(name))
        .map(|(name, value)| (name.clone(), json!(value)))
        .collect();
    let has_body = spec.body.is_some() && spec.method != "GET" && spec.method != "HEAD";
    let body_line = match &spec.body {
        Some(body) if has_body => format!("init.body = {}", json_str(body)),
        _ => String::new(),
    };
    TEMPLATE
        .replace("__TIMEOUT__", &FETCH_TIMEOUT_MS.to_string())
        .replace("__METHOD__", &json_str(&spec.method))
        .replace("__HEADERS__", &Value::Object(headers).to_string())
        .replace("__BODYLINE__", &body_line)
        .replace("__URL__", &json_str(&spec.url))
        .replace("__MAXBYTES__", &MAX_BODY_BYTES.to_string())
}

/// `JSON.stringify(s)`.
fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_owned())
}

const TEMPLATE: &str = r#"
(async () => {
  const t0 = performance.now()
  const __revAc = new AbortController()
  const __revTimer = setTimeout(() => __revAc.abort(), __TIMEOUT__)
  try {
    const init = {
      method: __METHOD__,
      headers: __HEADERS__,
      credentials: 'include',
      redirect: 'follow',
      cache: 'no-store',
      signal: __revAc.signal
    }
    __BODYLINE__
    const res = await fetch(__URL__, init)
    clearTimeout(__revTimer)
    const buf = await res.arrayBuffer()
    const bytes = new Uint8Array(buf)
    const total = bytes.length
    const slice = total > __MAXBYTES__ ? bytes.slice(0, __MAXBYTES__) : bytes
    const ctm = /charset\s*=\s*["']?([^;"'\s]+)/i.exec(res.headers.get('content-type') || '')
    let body
    try {
      body = new TextDecoder(ctm ? ctm[1].toLowerCase() : 'utf-8', { fatal: false }).decode(slice)
    } catch (_e) {
      body = new TextDecoder('utf-8', { fatal: false }).decode(slice)
    }
    if (body.charCodeAt(0) === 0xfeff) body = body.slice(1)
    if (total > __MAXBYTES__) body = body.replace(/�+$/, '')
    const headers = {}
    res.headers.forEach((v, k) => { headers[k] = v })
    return {
      status: res.status,
      statusText: res.statusText,
      headers,
      body,
      bodyTruncated: total > __MAXBYTES__,
      bodyByteLength: total,
      timeMs: Math.round(performance.now() - t0)
    }
  } catch (e) {
    clearTimeout(__revTimer)
    const isAbort = e && (e.name === 'AbortError')
    return {
      status: 0, statusText: '', headers: {}, body: '',
      bodyTruncated: false, bodyByteLength: 0,
      timeMs: Math.round(performance.now() - t0),
      error: isAbort ? 'repeater: fetch aborted (target likely hung)' : ((e && e.message) ? e.message : String(e))
    }
  }
})()
"#;

pub struct RepeaterSend;

impl Tool for RepeaterSend {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "repeater_send",
            "Send an HTTP request from the active tab (cookies, TLS, HTTP/2 — behaves like the real browser): replay a captured request (requestId) with optional modifications, OR omit requestId and give modifications.url (+ method/setHeaders/body) to send to a URL that was never captured. Use this instead of curl. The fetch runs in the active tab, so navigate to the target origin first to avoid CORS. Returns status, headers, and body (first 64KB). Forbidden fetch headers (Cookie, Host, User-Agent, Origin, Referer, sec-*) are stripped — Cookie is auto-attached from the browser jar.",
            schema(
                json!({
                    "requestId": { "type": "string", "description": "requestId from list_requests. Omit to send a fresh request to modifications.url" },
                    "modifications": {
                        "type": "object",
                        "properties": {
                            "url": { "type": "string" },
                            "method": { "type": "string" },
                            "setHeaders": { "type": "object", "description": "Headers to add or overwrite (case-insensitive replace)" },
                            "removeHeaders": { "type": "array", "items": { "type": "string" }, "description": "Header names to remove (case-insensitive)" },
                            "body": { "type": "string", "description": "null clears body; omit to keep original" }
                        }
                    }
                }),
                &[],
            ),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let request_id = args["requestId"].as_str();
            let mods = parse_mods(&args["modifications"]);
            let spec = {
                let store = ctx
                    .traffic
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match build_request_spec(&store, request_id, mods.as_ref()) {
                    Ok(spec) => spec,
                    Err(error) => return err(error),
                }
            };
            // Scope guardrail (the in-page fetch bypasses send()'s check).
            if let Some(block) = scope_block_for_url(Some(&spec.url)) {
                return err(block);
            }
            match ctx.browser.evaluate(&build_eval_expression(&spec)).await {
                Ok(response) => ok(pretty(&response)),
                Err(error) => ok(pretty(&json!({
                    "status": 0, "statusText": "", "headers": {}, "body": "",
                    "bodyTruncated": false, "bodyByteLength": 0, "timeMs": 0,
                    "error": error,
                }))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rever_core::traffic::Headers;

    fn spec(method: &str, body: Option<&str>, headers: &[(&str, &str)]) -> RequestSpec {
        RequestSpec {
            url: "https://api.test/x".to_owned(),
            method: method.to_owned(),
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect::<Headers>(),
            body: body.map(str::to_owned),
        }
    }

    #[test]
    fn 금지_헤더를_판별한다() {
        for h in ["Host", "cookie2", "User-Agent", "Sec-Fetch-Mode", "Proxy-Authorization", "accept-encoding"] {
            assert!(forbidden_fetch_header(h), "{h} should be forbidden");
        }
        for h in ["Accept", "Authorization", "Cookie", "Content-Type", "X-Custom"] {
            assert!(!forbidden_fetch_header(h), "{h} should be allowed");
        }
    }

    #[test]
    fn 금지_헤더는_표현식에서_빠지고_허용헤더는_들어간다() {
        let expr = build_eval_expression(&spec(
            "GET",
            None,
            &[("Authorization", "Bearer t"), ("Host", "evil"), ("User-Agent", "x")],
        ));
        assert!(expr.contains("\"Authorization\":\"Bearer t\""));
        assert!(!expr.contains("evil"));
        assert!(!expr.contains("\"User-Agent\""));
        // All placeholders resolved (JS vars like __revAc legitimately remain).
        for ph in ["__TIMEOUT__", "__METHOD__", "__HEADERS__", "__BODYLINE__", "__URL__", "__MAXBYTES__"] {
            assert!(!expr.contains(ph), "{ph} not resolved");
        }
    }

    #[test]
    fn get는_바디를_붙이지_않고_post는_붙인다() {
        let get = build_eval_expression(&spec("GET", Some("x=1"), &[]));
        assert!(!get.contains("init.body"));
        let post = build_eval_expression(&spec("POST", Some("x=1"), &[]));
        assert!(post.contains("init.body = \"x=1\""));
    }
}
