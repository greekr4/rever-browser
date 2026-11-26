//! Port of `src/main/mcp/tools/body.ts` — fetch_body.

use base64::Engine as _;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rever_core::traffic::Headers;
use rever_tools_api::tool::{ToolContext, ToolDef};
use rever_tools_api::utils::{ToolResult, ok};
use serde_json::json;
use url::Url;

use crate::args::{Args, schema};
use crate::store;

const MAX_BYTES: usize = 8 * 1024 * 1024;

// Headers that must not be forwarded on a re-fetch (the HTTP client sets its
// own, and some break the request). The TypeScript pattern,
// /^(host|content-length|connection|accept-encoding|:.*)/i, has no end anchor:
// it drops every name that merely starts with one of these.
fn is_dropped_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "host",
        "content-length",
        "connection",
        "accept-encoding",
        ":",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

fn looks_binary(buf: &[u8]) -> bool {
    buf.iter().take(1024).any(|byte| *byte == 0)
}

// /^https?:/i
fn is_http_url(url: &str) -> bool {
    let starts_with = |scheme: &str| {
        url.get(..scheme.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
    };
    starts_with("http:") || starts_with("https:")
}

tool!(
    FetchBody,
    ToolDef::new(
        "fetch_body",
        "Force-retrieve the response body for a captured request whose body was skipped (image/video/font/css are skipped by default) or dropped (served from cache / a service worker). Tries the body the engine still holds first, then re-fetches the URL reusing the original request headers (so auth/cookies still apply). Binary bodies are returned base64. Use when you decided after the fact that you need a body list_requests/get_request does not have.",
        schema(
            json!({
                "requestId": { "type": "string", "description": "requestId from list_requests" }
            }),
            &["requestId"],
        ),
    ),
    |ctx, args| fetch_body(ctx, &args).await
);

async fn fetch_body(ctx: &ToolContext, args: &Args<'_>) -> Result<ToolResult, String> {
    let request_id = args.required_str("requestId")?;
    let (url, method, headers) = {
        let store = store(ctx);
        let Some(entry) = store.get(request_id) else {
            return Err(format!("unknown requestId: {request_id}"));
        };
        if entry
            .response_body
            .as_deref()
            .is_some_and(|b| !b.is_empty())
        {
            let already = json!({
                "source": "already-captured",
                "note": "This request already has a stored body — use get_request.",
                "requestId": request_id
            });
            return Ok(ok(already.to_string()));
        }
        (
            entry.url.clone(),
            entry.method.clone(),
            entry.request_headers.clone().unwrap_or_default(),
        )
    };

    // 1) The TypeScript first asks CDP for the live copy
    // (Network.getResponseBody). `Browser` has no call for the engine's own
    // copy of a body, so this goes on as the TypeScript does once that copy
    // has been evicted.

    // 2) Re-fetch the URL with the original request headers.
    if !is_http_url(&url) {
        return Err("no live CDP body and the URL is not http(s) — cannot re-fetch".to_owned());
    }
    let (status, buf, byte_length) = refetch(&url, &method, &headers).await?;
    let truncated = byte_length > MAX_BYTES;
    let binary = looks_binary(&buf);
    let body = if binary {
        base64::engine::general_purpose::STANDARD.encode(&buf)
    } else {
        String::from_utf8_lossy(&buf).into_owned()
    };
    let out = json!({
        "source": "re-fetch",
        "status": status,
        "base64": binary,
        "truncated": truncated,
        "byteLength": byte_length,
        "url": url,
        "body": body
    });
    Ok(ok(out.to_string()))
}

/// The method as fetch() sends it, or the TypeError fetch() throws for it: GET
/// when there is none, and these six in upper case whatever case they were
/// stored in. Any other method goes out as it is ("patch" stays "patch").
fn fetch_method(method: &str) -> Result<reqwest::Method, String> {
    if method.is_empty() {
        return Ok(reqwest::Method::GET);
    }
    let is = |known: &&str| known.eq_ignore_ascii_case(method);
    if ["CONNECT", "TRACE", "TRACK"].iter().any(is) {
        return Err(format!("'{method}' HTTP method is unsupported."));
    }
    let sent = ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT"]
        .into_iter()
        .find(|known| is(known))
        .unwrap_or(method);
    reqwest::Method::from_bytes(sent.as_bytes())
        .map_err(|_| format!("'{method}' is not a valid HTTP method."))
}

/// A string as fetch() puts it on the wire, one byte per UTF-16 unit, or the
/// TypeError fetch() throws for a unit that does not fit in a byte. CDP reports
/// header bytes one per character, so this sends back the bytes the page sent;
/// encoding as UTF-8 would double-encode a non-ASCII cookie.
fn byte_string(text: &str) -> Result<Vec<u8>, String> {
    text.encode_utf16()
        .enumerate()
        .map(|(index, unit)| {
            u8::try_from(unit).map_err(|_| {
                format!(
                    "Cannot convert argument to a ByteString because the character at index \
                     {index} has a value of {unit} which is greater than 255."
                )
            })
        })
        .collect()
}

const HTTP_WHITESPACE: [char; 4] = ['\t', '\n', '\r', ' '];

/// The headers Node's fetch() sends for these captured ones: the captured
/// headers that may be forwarded, then the ones fetch() sets itself.
fn fetch_headers(captured: &Headers, method: &str, https: bool) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    for (name, value) in captured {
        if is_dropped_header(name) {
            continue;
        }
        let invalid = |what: &str, text: &str| {
            format!("Headers.append: \"{text}\" is an invalid header {what}.")
        };
        let header_name =
            HeaderName::from_bytes(&byte_string(name)?).map_err(|_| invalid("name", name))?;
        let value = value.trim_matches(HTTP_WHITESPACE);
        if value.contains(['\0', '\n', '\r']) {
            return Err(invalid("value", value));
        }
        // Any other control character passes Headers and fails in the client.
        let value =
            HeaderValue::from_bytes(&byte_string(value)?).map_err(|_| "fetch failed".to_owned())?;
        headers.append(header_name, value);
    }
    let has_range = headers.contains_key("range");
    let mut default = |name: &'static str, value: &'static str, replace: bool| {
        if replace || !headers.contains_key(name) {
            headers.insert(name, HeaderValue::from_static(value));
        }
    };
    default("accept", "*/*", false);
    default("accept-language", "*", false);
    default("user-agent", "node", false);
    default("sec-fetch-mode", "cors", true);
    default("connection", "keep-alive", true);
    // A captured accept-encoding is never forwarded: the client decodes what
    // it asks for here.
    let encodings = match (has_range, https) {
        (true, _) => "identity",
        (false, true) => "br, gzip, deflate",
        (false, false) => "gzip, deflate",
    };
    default("accept-encoding", encodings, true);
    if matches!(method, "POST" | "PUT" | "PATCH") {
        default("content-length", "0", true);
    }
    Ok(headers)
}

/// Status, the first `MAX_BYTES` of the body, and the body's full length. A
/// failure is worded as Node's fetch() words it, checked in the order fetch()
/// checks: header characters, the URL, the method, header syntax, the network.
async fn refetch(
    url: &str,
    method: &str,
    headers: &Headers,
) -> Result<(u16, Vec<u8>, usize), String> {
    for (name, value) in headers {
        if !is_dropped_header(name) {
            byte_string(name)?;
            byte_string(value)?;
        }
    }
    let parsed = Url::parse(url).map_err(|_| format!("Failed to parse URL from {url}"))?;
    if !parsed.username().is_empty() || parsed.password().is_some_and(|p| !p.is_empty()) {
        return Err(format!(
            "Request cannot be constructed from a URL that includes credentials: {url}"
        ));
    }
    let method = fetch_method(method)?;
    let headers = fetch_headers(headers, method.as_str(), parsed.scheme() == "https")?;
    // fetch() reports every network failure with these two words.
    let failed = |_| "fetch failed".to_owned();
    let client = reqwest::Client::builder()
        // Node's fetch follows up to 20 redirects and ignores the proxy
        // environment variables.
        .redirect(reqwest::redirect::Policy::limited(20))
        .no_proxy()
        .build()
        .map_err(failed)?;
    let request = client.request(method, parsed).headers(headers);
    let mut response = request.send().await.map_err(failed)?;
    let status = response.status().as_u16();
    // The TypeScript buffers the whole body and then cuts it; reading in chunks
    // gives the same result without holding more than the cap.
    let mut buf = Vec::new();
    let mut byte_length = 0;
    // A body that breaks off or does not decode is "terminated" to fetch().
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "terminated".to_owned())?
    {
        byte_length += chunk.len();
        let room = MAX_BYTES.saturating_sub(buf.len()).min(chunk.len());
        buf.extend_from_slice(chunk.get(..room).unwrap_or_default());
    }
    Ok((status, buf, byte_length))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    use rever_tools_api::tool::ToolContext;
    use serde_json::Value;

    use super::*;
    use crate::test_support::{call, call_json, put, request};

    /// Serves one HTTP response on a loopback port; returns the URL and a
    /// channel that yields the request head the client sent.
    fn serve_once(response: Vec<u8>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let (seen, head) = mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut socket, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut chunk = [0; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match socket.read(&mut chunk) {
                    Ok(n) if n > 0 => request.extend_from_slice(&chunk[..n]),
                    _ => break,
                }
            }
            let _ = seen.send(String::from_utf8_lossy(&request).to_lowercase());
            let _ = socket.write_all(&response);
        });
        (format!("http://127.0.0.1:{port}/data?x=1"), head)
    }

    fn http_response(status: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn 다시_보내면_안_되는_헤더는_접두사로_걸러낸다() {
        for name in [
            "Host",
            "content-length",
            "Connection",
            "Accept-Encoding",
            ":authority",
            ":path",
        ] {
            assert!(is_dropped_header(name), "{name}");
        }
        // No end anchor in the TypeScript pattern: a longer name is dropped too.
        assert!(is_dropped_header("hostname-hint"));
        for name in [
            "cookie",
            "authorization",
            "x-host",
            "accept",
            "content-type",
        ] {
            assert!(!is_dropped_header(name), "{name}");
        }
    }

    #[test]
    fn 앞_1024바이트에_nul이_있으면_바이너리로_본다() {
        assert!(looks_binary(&[b'a', 0, b'b']));
        assert!(!looks_binary(b"plain text"));
        assert!(!looks_binary(&[]));
        let mut late = vec![b'a'; 1024];
        late.push(0);
        assert!(!looks_binary(&late));
        late[1023] = 0;
        assert!(looks_binary(&late));
    }

    #[test]
    fn 메서드는_fetch가_정규화하는_여섯_개만_대문자로_바꾼다() {
        // What `new Request(url, { method }).method` gives in node.
        for (stored, sent) in [
            ("", "GET"),
            ("GET", "GET"),
            ("get", "GET"),
            ("Post", "POST"),
            ("delete", "DELETE"),
            ("options", "OPTIONS"),
            ("head", "HEAD"),
            ("put", "PUT"),
            ("patch", "patch"),
            ("PATCH", "PATCH"),
            ("propfind", "propfind"),
        ] {
            let method = fetch_method(stored).expect(stored);
            assert_eq!(method.as_str(), sent, "{stored}");
        }
    }

    #[tokio::test]
    async fn fetch가_거절하는_메서드는_보내지_않고_같은_에러를_돌려준다() {
        let ctx = ToolContext::for_tests();
        // Port 9 is never contacted: the method is refused first.
        for (id, method, error) in [
            ("1", "TRACE", "'TRACE' HTTP method is unsupported."),
            ("2", "connect", "'connect' HTTP method is unsupported."),
            ("3", "track", "'track' HTTP method is unsupported."),
            ("4", "a b", "'a b' is not a valid HTTP method."),
        ] {
            put(&ctx, request(id, "http://127.0.0.1:9/", method, "XHR"), 100);
            assert_eq!(
                call(&FetchBody, &ctx, json!({ "requestId": id })).await,
                Err(error.to_owned())
            );
        }
    }

    #[test]
    fn http와_https_주소만_다시_가져올_수_있다() {
        assert!(is_http_url("http://a.test/"));
        assert!(is_http_url("HTTPS://a.test/"));
        assert!(!is_http_url("ws://a.test/"));
        assert!(!is_http_url("data:text/plain,hi"));
        assert!(!is_http_url("httpx://a.test/"));
        assert!(!is_http_url(""));
        // A multi-byte head must not be cut inside a character.
        assert!(!is_http_url("한글://a"));
    }

    #[tokio::test]
    async fn 모르는_요청과_잘못된_인자는_에러다() {
        let ctx = ToolContext::for_tests();
        assert_eq!(
            call(&FetchBody, &ctx, json!({ "requestId": "nope" })).await,
            Err("unknown requestId: nope".to_owned())
        );
        assert!(call(&FetchBody, &ctx, json!({})).await.is_err());
        assert!(
            call(&FetchBody, &ctx, json!({ "requestId": 7 }))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn 이미_본문이_있으면_get_request를_쓰라고_알린다() {
        let ctx = ToolContext::for_tests();
        let mut patch = request("1", "https://a.test/x", "GET", "XHR");
        patch.response_body = Some("hello".to_owned());
        put(&ctx, patch, 100);
        assert_eq!(
            call(&FetchBody, &ctx, json!({ "requestId": "1" }))
                .await
                .unwrap(),
            r#"{"source":"already-captured","note":"This request already has a stored body — use get_request.","requestId":"1"}"#
        );
    }

    #[tokio::test]
    async fn http가_아닌_주소는_다시_가져오지_않는다() {
        let ctx = ToolContext::for_tests();
        put(
            &ctx,
            request("1", "blob:https://a.test/1234", "GET", "Other"),
            100,
        );
        assert_eq!(
            call(&FetchBody, &ctx, json!({ "requestId": "1" })).await,
            Err("no live CDP body and the URL is not http(s) — cannot re-fetch".to_owned())
        );
    }

    #[tokio::test]
    async fn 원래_요청_헤더로_다시_가져와_텍스트_본문을_돌려준다() {
        let (url, head) = serve_once(http_response("200 OK", "안녕 body".as_bytes()));
        let ctx = ToolContext::for_tests();
        let mut patch = request("1", &url, "GET", "Image");
        patch.request_headers = Some(
            [
                ("Cookie", "sid=1"),
                ("X-Token", "abc"),
                ("Host", "evil.test"),
                ("Accept-Encoding", "br"),
                (":authority", "evil.test"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect(),
        );
        put(&ctx, patch, 100);

        let text = call(&FetchBody, &ctx, json!({ "requestId": "1" }))
            .await
            .unwrap();
        assert_eq!(
            text,
            format!(
                r#"{{"source":"re-fetch","status":200,"base64":false,"truncated":false,"byteLength":11,"url":"{url}","body":"안녕 body"}}"#
            )
        );
        let head = head.recv().expect("request head");
        assert!(head.starts_with("get /data?x=1 http/1.1\r\n"), "{head}");
        assert!(head.contains("cookie: sid=1\r\n"));
        assert!(head.contains("x-token: abc\r\n"));
        assert!(!head.contains("evil.test"));
        assert!(!head.contains("accept-encoding: br"));
    }

    #[tokio::test]
    async fn 바이너리_본문은_base64로_돌려주고_원래_메서드와_상태를_유지한다() {
        let (url, head) = serve_once(http_response("404 Not Found", &[0x89, b'P', 0, 0xff]));
        let ctx = ToolContext::for_tests();
        put(&ctx, request("1", &url, "POST", "Image"), 100);

        let out = call_json(&FetchBody, &ctx, json!({ "requestId": "1" })).await;
        assert_eq!(
            out,
            json!({
                "source": "re-fetch", "status": 404, "base64": true, "truncated": false,
                "byteLength": 4, "url": url, "body": "iVAA/w=="
            })
        );
        assert!(head.recv().expect("request head").starts_with("post /data"));
    }

    #[tokio::test]
    async fn 잘못된_utf8은_대체_문자로_읽는다() {
        // A stray byte, cut-off sequences, a surrogate, an overlong form and an
        // out-of-range lead byte.
        let bytes = [
            b'a', 0xff, b'b', 0xe0, 0xa4, b'c', 0xf0, 0x9f, 0x98, b'd', 0xed, 0xa0, 0x80, b'e',
            0xc0, 0xaf, b'f', 0xf5, 0x80, b'g', 0xe2, 0x82,
        ];
        let (url, _head) = serve_once(http_response("200 OK", &bytes));
        let ctx = ToolContext::for_tests();
        put(&ctx, request("1", &url, "GET", "Font"), 100);
        let out = call_json(&FetchBody, &ctx, json!({ "requestId": "1" })).await;
        // What Buffer.from(bytes).toString('utf8') gives in node.
        let expected = "a\u{fffd}b\u{fffd}c\u{fffd}d\u{fffd}\u{fffd}\u{fffd}e\u{fffd}\u{fffd}f\u{fffd}\u{fffd}g\u{fffd}";
        assert_eq!(out["body"], Value::from(expected));
        assert_eq!(out["byteLength"], bytes.len());
    }

    #[tokio::test]
    async fn 본문이_8mb를_넘으면_앞부분만_돌려주고_전체_길이를_알린다() {
        let (url, _head) = serve_once(http_response("200 OK", &vec![b'a'; MAX_BYTES + 5]));
        let ctx = ToolContext::for_tests();
        put(&ctx, request("1", &url, "GET", "Media"), 100);
        let out = call_json(&FetchBody, &ctx, json!({ "requestId": "1" })).await;
        assert_eq!(out["truncated"], true);
        assert_eq!(out["byteLength"], MAX_BYTES + 5);
        assert_eq!(out["body"].as_str().map(str::len), Some(MAX_BYTES));
    }

    #[tokio::test]
    async fn 연결할_수_없으면_에러_결과다() {
        // Bind then drop: nothing listens on the port any more.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").port()
        };
        let ctx = ToolContext::for_tests();
        put(
            &ctx,
            request("1", &format!("http://127.0.0.1:{port}/"), "GET", "Image"),
            100,
        );
        // Node's fetch() says only this for any network failure.
        assert_eq!(
            call(&FetchBody, &ctx, json!({ "requestId": "1" })).await,
            Err("fetch failed".to_owned())
        );
    }

    fn captured(pairs: &[(&str, &str)]) -> Headers {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// Header lines as "name: value", sorted; a value is shown one char per byte.
    fn lines(headers: &HeaderMap) -> Vec<String> {
        let mut lines: Vec<String> = headers
            .iter()
            .map(|(name, value)| {
                let value: String = value.as_bytes().iter().map(|b| char::from(*b)).collect();
                format!("{name}: {value}")
            })
            .collect();
        lines.sort();
        lines
    }

    #[test]
    fn fetch가_스스로_넣는_헤더를_같이_보낸다() {
        // What a server receives from Node's fetch(url, { method, headers }).
        assert_eq!(
            lines(&fetch_headers(&Headers::new(), "GET", false).unwrap()),
            [
                "accept-encoding: gzip, deflate",
                "accept-language: *",
                "accept: */*",
                "connection: keep-alive",
                "sec-fetch-mode: cors",
                "user-agent: node"
            ]
        );
        let given = captured(&[
            ("User-Agent", "UA"),
            ("Accept-Language", "ko"),
            ("Accept", "text/html"),
            ("Sec-Fetch-Mode", "navigate"),
            ("Range", "bytes=0-5"),
        ]);
        assert_eq!(
            lines(&fetch_headers(&given, "POST", true).unwrap()),
            [
                // No compression is asked for with a Range.
                "accept-encoding: identity",
                "accept-language: ko",
                "accept: text/html",
                "connection: keep-alive",
                "content-length: 0",
                "range: bytes=0-5",
                // Always cors, whatever was captured.
                "sec-fetch-mode: cors",
                "user-agent: UA"
            ]
        );
        let encoding = |method: &str, https: bool| {
            let headers = fetch_headers(&Headers::new(), method, https).unwrap();
            let length = headers.contains_key("content-length");
            (
                headers["accept-encoding"].to_str().unwrap().to_owned(),
                length,
            )
        };
        assert_eq!(
            encoding("GET", true),
            ("br, gzip, deflate".to_owned(), false)
        );
        assert!(encoding("PUT", false).1);
        assert!(encoding("PATCH", false).1);
        // fetch() does not upper-case "patch", and sends no length for it.
        assert!(!encoding("patch", false).1);
        assert!(!encoding("DELETE", false).1);
    }

    #[test]
    fn 헤더_값은_문자_하나를_바이트_하나로_보내고_앞뒤_공백을_뗀다() {
        let headers = fetch_headers(
            &captured(&[("X-Latin", "caf\u{e9}\u{ff}"), ("x-ws", "\t\n a\tb \r\n")]),
            "GET",
            false,
        )
        .unwrap();
        // 0xE9, not the two bytes of UTF-8.
        assert_eq!(headers["x-latin"].as_bytes(), b"caf\xe9\xff");
        assert_eq!(headers["x-ws"].as_bytes(), b"a\tb");
        assert_eq!(byte_string("a\u{ff}"), Ok(vec![b'a', 0xff]));
    }

    #[test]
    fn 보낼_수_없는_헤더는_fetch와_같은_에러다() {
        let error = |name: &str, value: &str| {
            fetch_headers(&captured(&[(name, value)]), "GET", false).unwrap_err()
        };
        // The index and the value are those of the UTF-16 unit.
        assert_eq!(
            error("x-k", "ab😀"),
            "Cannot convert argument to a ByteString because the character at index 2 has a value of 55357 which is greater than 255."
        );
        assert_eq!(
            error("한", "v"),
            "Cannot convert argument to a ByteString because the character at index 0 has a value of 54620 which is greater than 255."
        );
        assert_eq!(
            error("bad name", "v"),
            "Headers.append: \"bad name\" is an invalid header name."
        );
        assert_eq!(
            error("", "v"),
            "Headers.append: \"\" is an invalid header name."
        );
        assert_eq!(
            error("x-k", " a\nb "),
            "Headers.append: \"a\nb\" is an invalid header value."
        );
        assert_eq!(
            error("x-k", "a\0b"),
            "Headers.append: \"a\0b\" is an invalid header value."
        );
        // Other control characters get past Headers and fail in the client.
        assert_eq!(error("x-k", "a\u{1}b"), "fetch failed");
        assert_eq!(error("x-k", "a\u{7f}b"), "fetch failed");
        // A header that is not forwarded is not checked.
        assert!(fetch_headers(&captured(&[("Hostess", "한"), (":x", "한")]), "GET", false).is_ok());
    }

    #[tokio::test]
    async fn 에러는_fetch가_확인하는_순서대로_난다() {
        let ctx = ToolContext::for_tests();
        let add = |id: &str, url: &str, method: &str, header: Option<(&str, &str)>| {
            let mut patch = request(id, url, method, "XHR");
            patch.request_headers = header.map(|pair| captured(&[pair]));
            put(&ctx, patch, 100);
        };
        // Header characters, then the URL, then the method, then header syntax.
        add("1", "http://a b/", "TRACE", Some(("x", "한")));
        add("2", "http://a b/", "TRACE", Some(("bad name", "x")));
        add("3", "http://u:p@127.0.0.1:9/", "TRACE", None);
        add("4", "http://127.0.0.1:9/", "TRACE", Some(("bad name", "x")));
        add("5", "http://127.0.0.1:9/", "GET", Some(("bad name", "x")));
        add("6", "http://127.0.0.1:99999/", "GET", None);
        for (id, error) in [
            (
                "1",
                "Cannot convert argument to a ByteString because the character at index 0 has a value of 54620 which is greater than 255.",
            ),
            ("2", "Failed to parse URL from http://a b/"),
            (
                "3",
                "Request cannot be constructed from a URL that includes credentials: http://u:p@127.0.0.1:9/",
            ),
            ("4", "'TRACE' HTTP method is unsupported."),
            (
                "5",
                "Headers.append: \"bad name\" is an invalid header name.",
            ),
            ("6", "Failed to parse URL from http://127.0.0.1:99999/"),
        ] {
            assert_eq!(
                call(&FetchBody, &ctx, json!({ "requestId": id })).await,
                Err(error.to_owned()),
                "{id}"
            );
        }
    }

    #[tokio::test]
    async fn 압축된_응답은_풀어서_돌려준다() {
        // gzip of "zipped body", sent whether or not it was asked for.
        let gzip = base64::engine::general_purpose::STANDARD
            .decode("H4sIAAAAAAAC/6vKLChITVFIyk+pBADAE813CwAAAA==")
            .expect("base64");
        let mut response = format!(
            "HTTP/1.1 200 OK\r\ncontent-encoding: gzip\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            gzip.len()
        )
        .into_bytes();
        response.extend_from_slice(&gzip);
        let (url, head) = serve_once(response);
        let ctx = ToolContext::for_tests();
        put(&ctx, request("1", &url, "GET", "Stylesheet"), 100);
        let out = call_json(&FetchBody, &ctx, json!({ "requestId": "1" })).await;
        assert_eq!(out["body"], "zipped body");
        assert_eq!(out["base64"], false);
        assert_eq!(out["byteLength"], 11);
        let head = head.recv().expect("request head");
        assert!(
            head.contains("accept-encoding: gzip, deflate\r\n"),
            "{head}"
        );
        assert!(head.contains("user-agent: node\r\n"), "{head}");
    }

    #[tokio::test]
    async fn 본문이_중간에_끊기면_terminated_에러다() {
        let (url, _head) = serve_once(
            b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\nconnection: close\r\n\r\n0123456789"
                .to_vec(),
        );
        let ctx = ToolContext::for_tests();
        put(&ctx, request("1", &url, "GET", "Image"), 100);
        assert_eq!(
            call(&FetchBody, &ctx, json!({ "requestId": "1" })).await,
            Err("terminated".to_owned())
        );
    }
}
