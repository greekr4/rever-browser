//! Port of `src/main/mcp/tools/har.ts` — har_export and import_har.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rever_analysis::har_build::to_headers;
use rever_core::har_import::har_to_entries;
use rever_core::traffic::{ListFilter, StoredRequest};
use rever_tools_api::tool::{ToolContext, ToolDef};
use rever_tools_api::utils::{ToolResult, ok};
use serde::Serialize;
use serde_json::{Map, Value, json};
use url::Url;

use crate::args::{Args, limit_prop, prop, schema};
use crate::js::{INVALID_TIME, js_key_order, to_iso_string, utf16_len, utf16_prefix};
use crate::{ok_pretty, store};

// At most 256KB of body per entry, and 50MB for the whole JSON response.
const BODY_TRUNCATE_BYTES: usize = 256 * 1024;
const HAR_RESPONSE_SIZE_LIMIT: usize = 50 * 1024 * 1024;
// MCP Streamable HTTP sends the response as a single SSE event, and widely
// used clients (e.g. httpx2) enforce a 1MB per-event cap — beyond that the
// stream ends with no response and the client only sees "SSE stream ended
// without a response". Keep output under that bound and let callers fetch
// the older remainder via the log._nextBefore/_nextBeforeId cursor, so a
// misjudged `limit` yields a truncated result instead of a dead stream.
const SSE_SAFE_RESPONSE_BYTES: usize = 900 * 1024;

// HAR files over this are refused rather than parsed into memory.
const MAX_HAR_BYTES: u64 = 300 * 1024 * 1024;

fn set(object: &mut Value, key: &str, value: impl Into<Value>) {
    if let Some(map) = object.as_object_mut() {
        map.insert(key.to_owned(), value.into());
    }
}

/// One HAR entry, or `Ok(None)` for a request that cannot be exported.
fn har_entry(full: &StoredRequest, include_bodies: bool) -> Result<Option<Value>, String> {
    let started_date_time = to_iso_string(full.started_at).ok_or(INVALID_TIME)?;
    let time_ms = match full.completed_at {
        // Within toISOString's range, so the subtraction cannot overflow.
        Some(completed) if completed != 0 => i64::try_from(completed)
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(full.started_at).unwrap_or(i64::MAX)),
        _ => -1,
    };

    // A single entry whose URL won't parse (devtools://, empty) used to kill
    // the whole export with "Invalid URL" — skip it per-entry and surface the
    // count on the log instead.
    let Ok(req_url) = Url::parse(&full.url) else {
        return Ok(None);
    };
    let query_string: Vec<Value> = req_url
        .query_pairs()
        .map(|(name, value)| json!({ "name": name, "value": value }))
        .collect();

    let post_data = full.request_post_data.as_deref().filter(|p| !p.is_empty());
    let mut request = json!({
        "method": full.method,
        "url": full.url,
        "httpVersion": "HTTP/1.1",
        "cookies": [],
        "headers": to_headers(full.request_headers.as_ref()),
        "queryString": query_string,
        "headersSize": -1,
        "bodySize": post_data.map_or(0, utf16_len)
    });
    if let Some(text) = post_data {
        let headers = full.request_headers.as_ref();
        let mime_type = headers
            .and_then(|h| h.get("content-type").or_else(|| h.get("Content-Type")))
            .map_or("application/octet-stream", String::as_str);
        set(
            &mut request,
            "postData",
            json!({ "mimeType": mime_type, "text": text }),
        );
    }

    let body = full.response_body.as_deref().unwrap_or("");
    let body_units = utf16_len(body);
    let mut content = json!({
        "size": body_units,
        "mimeType": full.mime_type.as_deref().unwrap_or("")
    });
    if include_bodies && !body.is_empty() {
        let truncated = body_units > BODY_TRUNCATE_BYTES;
        set(
            &mut content,
            "text",
            utf16_prefix(body, BODY_TRUNCATE_BYTES),
        );
        if full.response_body_base64 == Some(true) {
            set(&mut content, "encoding", "base64");
        }
        if truncated {
            set(
                &mut content,
                "comment",
                format!("truncated (original {body_units} bytes)"),
            );
        }
    }

    Ok(Some(json!({
        "startedDateTime": started_date_time,
        "time": time_ms,
        "request": request,
        "response": {
            "status": full.status.unwrap_or(0),
            "statusText": "",
            "httpVersion": "HTTP/1.1",
            "cookies": [],
            "headers": to_headers(full.response_headers.as_ref()),
            "content": content,
            "redirectURL": "",
            "headersSize": -1,
            "bodySize": full.encoded_data_length.map_or(json!(-1), Value::from)
        },
        "cache": {},
        "timings": {
            "send": 0,
            "wait": if time_ms > 0 { time_ms } else { -1 },
            "receive": 0
        }
    })))
}

#[derive(Serialize)]
struct Har<'a> {
    log: Log<'a>,
}

#[derive(Serialize)]
struct Log<'a> {
    version: &'static str,
    creator: Value,
    entries: &'a [Value],
    // _skippedEntries and the truncation fields, in the order they were set.
    #[serde(flatten)]
    extra: &'a Map<String, Value>,
}

fn serialize(entries: &[Value], extra: &Map<String, Value>) -> Result<String, String> {
    serde_json::to_string_pretty(&Har {
        log: Log {
            version: "1.2",
            creator: json!({ "name": "rever-browser", "version": "0.1.0" }),
            entries,
            extra,
        },
    })
    .map_err(|error| error.to_string())
}

tool!(
    HarExport,
    ToolDef::new(
        "har_export",
        "Export captured traffic as HAR 1.2 JSON. Suitable for loading into Burp Suite, Caido, or any HAR-compatible analyzer. \
         If the output would exceed the client SSE event-size limit it is automatically truncated to the newest entries — \
         the response then contains log._nextBefore/_nextBeforeId: re-call with before/beforeId set to those values \
         to continue fetching the older remainder.",
        schema(
            json!({
                "host": prop("string", "Substring host filter"),
                "limit": limit_prop("Max entries (default 500)", 2000),
                "includeBodies": prop("boolean", "Include response bodies (default true, may be large)"),
                "before": prop(
                    "number",
                    "Boundary cursor: only include entries strictly older than the entry with this startedAt (epoch-ms). \
                     Lets a client page through large traffic sets in safe-sized chunks (avoids MCP SSE client \
                     event-size limits) — when the server auto-truncates a response, log._nextBefore/_nextBeforeId \
                     carry exactly the values to pass here. Pair with beforeId to disambiguate entries sharing the \
                     same millisecond."
                ),
                "beforeId": prop(
                    "string",
                    "requestId of the boundary entry, paired with `before`. Excludes the boundary entry itself and \
                     newer siblings sharing its startedAt so every page is guaranteed to make progress."
                )
            }),
            &[],
        ),
    ),
    |ctx, args| har_export(ctx, &args)
);

fn har_export(ctx: &ToolContext, args: &Args) -> Result<ToolResult, String> {
    let host = args.str("host")?;
    let limit = args.limit(2000)?.unwrap_or(500);
    let include_bodies = args.bool("includeBodies")?.unwrap_or(true);
    let before = args.number("before")?;
    let before_id = args.str("beforeId")?;

    let store = store(ctx);
    let entries = match before {
        // Every startedAt is newer than a negative boundary.
        Some(before) if before < 0.0 => Vec::new(),
        _ => store.list(&ListFilter {
            host: host.map(str::to_owned),
            limit: Some(limit),
            before: before.map(|before| before.floor() as u64),
            // No startedAt equals a fractional boundary, so the TypeScript's
            // same-millisecond rule, and with it beforeId, never applies to one.
            before_id: before_id
                .filter(|_| before.is_some_and(|before| before.fract() == 0.0))
                .map(str::to_owned),
            ..ListFilter::default()
        }),
    };

    let mut har_entries: Vec<Value> = Vec::new();
    // Boundary candidates for the next page — pushed only for entries that
    // actually make it into har_entries, so the two stay aligned even when an
    // entry is skipped.
    let mut cursors: Vec<(u64, &str)> = Vec::new();
    let mut skipped_entries = 0_u64;
    for full in entries {
        match har_entry(full, include_bodies)? {
            Some(entry) => {
                har_entries.push(entry);
                cursors.push((full.started_at, &full.request_id));
            }
            None => skipped_entries += 1,
        }
    }

    let mut extra = Map::new();
    if skipped_entries > 0 {
        extra.insert("_skippedEntries".to_owned(), skipped_entries.into());
    }

    // All size checks measure BYTES, not UTF-16 code units: one Korean char is
    // 3 UTF-8 bytes, and counting units would let an over-limit response
    // reach the stream.
    let mut serialized = serialize(&har_entries, &extra)?;

    // Over the SSE-safe bound, keep the newest entries that fit the byte
    // budget in a single pass — halving drops more than needed and
    // re-stringifies the whole document each round.
    if serialized.len() > SSE_SAFE_RESPONSE_BYTES && har_entries.len() > 1 {
        let entry_bytes: Vec<usize> = har_entries.iter().map(|e| e.to_string().len()).collect();
        // Envelope ≈ document size minus the sum of its parts.
        let envelope = serialized.len().saturating_sub(entry_bytes.iter().sum());
        let mut kept = 0;
        let mut used = envelope;
        while let Some(bytes) = entry_bytes.get(kept)
            && used + bytes <= SSE_SAFE_RESPONSE_BYTES
        {
            used += bytes;
            kept += 1;
        }
        // Per-entry stringify runs slightly under the embedded size (deeper
        // indentation, separators, the truncation fields), so verify the
        // assembled document and trim the oldest if it ran low.
        while kept >= 1 {
            if kept < har_entries.len()
                && let Some((started_at, request_id)) = cursors.get(kept - 1)
            {
                extra.insert("_truncated".to_owned(), true.into());
                extra.insert(
                    "_droppedEntries".to_owned(),
                    (har_entries.len() - kept).into(),
                );
                extra.insert("_nextBefore".to_owned(), (*started_at).into());
                extra.insert("_nextBeforeId".to_owned(), (*request_id).into());
                extra.insert(
                    "_continuationHint".to_owned(),
                    "Response was cut to fit the MCP SSE event-size limit. Re-call har_export \
                     with before=_nextBefore and beforeId=_nextBeforeId to fetch the older remainder."
                        .into(),
                );
            }
            serialized = serialize(har_entries.get(..kept).unwrap_or_default(), &extra)?;
            if serialized.len() <= SSE_SAFE_RESPONSE_BYTES {
                break;
            }
            kept -= 1;
        }
    }
    let byte_length = serialized.len();
    if byte_length > SSE_SAFE_RESPONSE_BYTES {
        return Err(format!(
            "HAR output exceeds the SSE-safe size ({byte_length} bytes) even with a single entry. \
             Fetch it directly with get_request instead."
        ));
    }
    if byte_length > HAR_RESPONSE_SIZE_LIMIT {
        return Err(format!(
            "HAR output too large ({byte_length} bytes). Reduce limit or use host filter."
        ));
    }
    Ok(ok(serialized))
}

/// `Number.prototype.toString(36)` of a non-negative integer.
fn to_base36(mut n: u64) -> String {
    let mut digits = Vec::new();
    loop {
        digits.push(char::from_digit((n % 36) as u32, 36).unwrap_or('0'));
        n /= 36;
        if n == 0 {
            break;
        }
    }
    digits.iter().rev().collect()
}

fn import_id(label: Option<&str>, now_ms: u64) -> String {
    let raw = label.map_or_else(|| to_base36(now_ms), str::to_owned);
    let id: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .take(24)
        .collect();
    if id.is_empty() { "imp".to_owned() } else { id }
}

/// Node's text for a failed fs call ("ENOENT: no such file or directory, stat
/// '/x'") for the failures a wrong path causes; the OS text for the rest.
fn node_fs_error(error: &std::io::Error, syscall: &str, path: &str) -> String {
    use std::io::ErrorKind;
    let (code, text) = match error.kind() {
        ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        ErrorKind::NotADirectory => ("ENOTDIR", "not a directory"),
        // A directory opens fine and fails on read(), which names no path.
        ErrorKind::IsADirectory => {
            return "EISDIR: illegal operation on a directory, read".to_owned();
        }
        _ => return error.to_string(),
    };
    format!("{code}: {text}, {syscall} '{path}'")
}

/// Rewrites every `\uD800`–`\uDFFF` escape that has no partner to `\ufffd`.
/// JSON.parse reads such an escape as a lone surrogate; serde_json refuses the
/// whole document, and a Rust string could not hold one. Python writes them
/// for bytes it could not decode (surrogateescape), so a mitmproxy HAR can
/// carry them. U+FFFD is one UTF-16 unit too: every length stays what it was.
fn replace_lone_surrogate_escapes(json: &mut [u8]) {
    let unit = |json: &[u8], at: usize| {
        let hex = json.get(at..at + 6)?.strip_prefix(b"\\u")?;
        u16::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()
    };
    let mut at = 0;
    while at < json.len() {
        if json.get(at) != Some(&b'\\') {
            at += 1;
            continue;
        }
        at += match (unit(json, at), unit(json, at + 6)) {
            (Some(0xD800..=0xDBFF), Some(0xDC00..=0xDFFF)) => 12,
            (Some(0xD800..=0xDFFF), _) => {
                if let Some(hex) = json.get_mut(at + 2..at + 6) {
                    hex.copy_from_slice(b"fffd");
                }
                6
            }
            // Any other escape, an escaped backslash included.
            _ => 2,
        };
    }
}

fn bump(counts: &mut Map<String, Value>, key: &str) {
    let count = counts.get(key).and_then(Value::as_u64).unwrap_or(0);
    counts.insert(key.to_owned(), (count + 1).into());
}

tool!(
    ImportHar,
    ToolDef::new(
        "import_har",
        "Load a HAR file captured elsewhere (Chrome DevTools, Burp, Charles, mitmproxy hardump) so the traffic tools work on it offline — nothing is fetched. Entries get ids `har:<importId>:<n>`; find them with list_requests source=\"import\", then get_request / request_diff / export_client as usual. Imports live apart from live capture (up to 5,000 entries). clear=true drops all imports.",
        schema(
            json!({
                "path": prop("string", "Absolute path to a .har file"),
                "label": prop(
                    "string",
                    "Short id for this import (letters/digits); default is generated"
                ),
                "clear": prop("boolean", "Remove every imported entry instead of loading")
            }),
            &[],
        ),
    ),
    |ctx, args| import_har(ctx, &args)
);

fn import_har(ctx: &ToolContext, args: &Args) -> Result<ToolResult, String> {
    let file = args.str("path")?;
    let label = args.str("label")?;
    let clear = args.bool("clear")?.unwrap_or(false);

    if clear {
        let mut store = store(ctx);
        let cleared = store.import_count();
        store.clear_imports();
        return Ok(ok(json!({ "cleared": cleared }).to_string()));
    }
    let Some(file) = file.filter(|file| !file.is_empty()) else {
        return Err("pass path (absolute .har file) or clear=true".to_owned());
    };
    if !Path::new(file).is_absolute() {
        return Err("path must be absolute".to_owned());
    }
    let size = std::fs::metadata(file)
        .map_err(|e| node_fs_error(&e, "stat", file))?
        .len();
    if size > MAX_HAR_BYTES {
        return Err(format!(
            "HAR is {size} bytes — over the {MAX_HAR_BYTES}-byte import limit"
        ));
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);
    let import_id = import_id(label, now_ms);
    // The TypeScript reads inside the same try block as JSON.parse, so a file
    // that cannot be read is reported as invalid JSON too.
    let mut bytes = std::fs::read(file)
        .map_err(|e| format!("not valid JSON: {}", node_fs_error(&e, "open", file)))?;
    replace_lone_surrogate_escapes(&mut bytes);
    // Lossy, like Node's 'utf8' decoding: a stray byte must not refuse the file.
    let parsed: Value = serde_json::from_str(&String::from_utf8_lossy(&bytes))
        .map_err(|e| format!("not valid JSON: {e}"))?;
    let result = har_to_entries(&parsed, &import_id).map_err(|e| e.to_string())?;

    let mut by_type = Map::new();
    let mut by_host = Map::new();
    for e in &result.entries {
        bump(&mut by_type, &e.resource_type);
        bump(&mut by_host, &e.host);
    }
    let by_type: Map<String, Value> = js_key_order(by_type).into_iter().collect();
    let mut top_hosts = js_key_order(by_host);
    // Stable, like Array.prototype.sort: ties keep first-seen order.
    top_hosts.sort_by_key(|(_, count)| std::cmp::Reverse(count.as_u64()));
    top_hosts.truncate(8);
    let times = || {
        result
            .entries
            .iter()
            .map(|e| e.started_at)
            .filter(|t| *t > 0)
    };
    let range = times().min().zip(times().max());
    let imported = result.entries.len();

    store(ctx).import_requests(result.entries);

    let time_range = match range {
        Some((first, last)) => json!([
            to_iso_string(first).ok_or(INVALID_TIME)?,
            to_iso_string(last).ok_or(INVALID_TIME)?
        ]),
        None => Value::Null,
    };
    ok_pretty(&json!({
        "importId": import_id,
        "creator": result.creator,
        "imported": imported,
        "skipped": result.skipped.len(),
        "skippedExamples": result.skipped.iter().take(5).collect::<Vec<_>>(),
        "byType": by_type,
        "topHosts": top_hosts,
        "timeRange": time_range,
        "next": "list_requests source=\"import\" (filter by host / methodOrType), then get_request etc.",
        "limitations": [
            "Only what the HAR recorded: bodies the exporter dropped or truncated stay missing.",
            "grep_scripts / list_scripts search live capture only; get_request-based tools (resolve_source, request_diff, export_client) work on imports."
        ]
    }))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rever_core::traffic::RequestPatch;
    use rever_tools_api::tool::ToolContext;

    use super::*;
    use crate::lock;
    use crate::test_support::{call, call_json, put, request};

    fn entries(har: &Value) -> &[Value] {
        har["log"]["entries"].as_array().map_or(&[], Vec::as_slice)
    }

    fn urls(har: &Value) -> Vec<&str> {
        entries(har)
            .iter()
            .filter_map(|e| e["request"]["url"].as_str())
            .collect()
    }

    #[tokio::test]
    async fn 캡처가_없으면_빈_har을_돌려준다() {
        let ctx = ToolContext::for_tests();
        assert_eq!(
            call(&HarExport, &ctx, json!({})).await.unwrap(),
            r#"{
  "log": {
    "version": "1.2",
    "creator": {
      "name": "rever-browser",
      "version": "0.1.0"
    },
    "entries": []
  }
}"#
        );
    }

    #[tokio::test]
    async fn 요청_하나를_har_1_2_항목으로_내보낸다() {
        let ctx = ToolContext::for_tests();
        let mut patch = request(
            "1",
            "https://api.a.test/v1/items?q=a+b&q=%ED%95%9C&x",
            "POST",
            "XHR",
        );
        patch.started_at = Some(1_700_000_000_123);
        patch.completed_at = Some(1_700_000_000_173);
        patch.status = Some(201);
        patch.mime_type = Some("application/json".to_owned());
        patch.encoded_data_length = Some(321);
        patch.request_headers = Some(
            [("content-type", "application/json"), ("x-token", "t")]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        );
        patch.request_post_data = Some("{\"한\":1}".to_owned());
        patch.response_headers = Some(
            [("set-cookie".to_owned(), "a=1\nb=2".to_owned())]
                .into_iter()
                .collect(),
        );
        patch.response_body = Some("{\"ok\":true}".to_owned());
        put(&ctx, patch, 0);

        let har = call_json(&HarExport, &ctx, json!({})).await;
        assert_eq!(
            har,
            json!({ "log": {
                "version": "1.2",
                "creator": { "name": "rever-browser", "version": "0.1.0" },
                "entries": [{
                    "startedDateTime": "2023-11-14T22:13:20.123Z",
                    "time": 50,
                    "request": {
                        "method": "POST",
                        "url": "https://api.a.test/v1/items?q=a+b&q=%ED%95%9C&x",
                        "httpVersion": "HTTP/1.1",
                        "cookies": [],
                        "headers": [
                            { "name": "content-type", "value": "application/json" },
                            { "name": "x-token", "value": "t" }
                        ],
                        "queryString": [
                            { "name": "q", "value": "a b" },
                            { "name": "q", "value": "한" },
                            { "name": "x", "value": "" }
                        ],
                        "headersSize": -1,
                        "bodySize": 7,
                        "postData": { "mimeType": "application/json", "text": "{\"한\":1}" }
                    },
                    "response": {
                        "status": 201,
                        "statusText": "",
                        "httpVersion": "HTTP/1.1",
                        "cookies": [],
                        "headers": [
                            { "name": "set-cookie", "value": "a=1" },
                            { "name": "set-cookie", "value": "b=2" }
                        ],
                        "content": { "size": 11, "mimeType": "application/json", "text": "{\"ok\":true}" },
                        "redirectURL": "",
                        "headersSize": -1,
                        "bodySize": 321
                    },
                    "cache": {},
                    "timings": { "send": 0, "wait": 50, "receive": 0 }
                }]
            }})
        );
        // Key order is part of the output: it is the TypeScript's.
        let entry_keys: Vec<&String> = entries(&har)[0].as_object().unwrap().keys().collect();
        assert_eq!(
            entry_keys,
            [
                "startedDateTime",
                "time",
                "request",
                "response",
                "cache",
                "timings"
            ]
        );
    }

    #[tokio::test]
    async fn 끝나지_않은_요청과_빈_필드는_har의_기본값으로_채운다() {
        let ctx = ToolContext::for_tests();
        put(
            &ctx,
            request("1", "https://a.test/", "GET", "Document"),
            1000,
        );
        let har = call_json(&HarExport, &ctx, json!({})).await;
        let entry = &entries(&har)[0];
        assert_eq!(entry["time"], -1);
        assert_eq!(entry["timings"]["wait"], -1);
        assert_eq!(entry["request"]["bodySize"], 0);
        assert_eq!(entry["request"].get("postData"), None);
        assert_eq!(entry["request"]["queryString"], json!([]));
        assert_eq!(
            entry["response"],
            json!({
                "status": 0, "statusText": "", "httpVersion": "HTTP/1.1", "cookies": [],
                "headers": [], "content": { "size": 0, "mimeType": "" },
                "redirectURL": "", "headersSize": -1, "bodySize": -1
            })
        );
    }

    #[tokio::test]
    async fn content_type이_없으면_post_본문을_octet_stream으로_적는다() {
        let ctx = ToolContext::for_tests();
        let mut patch = request("1", "https://a.test/", "POST", "XHR");
        patch.request_post_data = Some("a=1".to_owned());
        put(&ctx, patch.clone(), 1000);
        patch.request_id = "2".to_owned();
        patch.request_headers = Some(
            [("Content-Type".to_owned(), "text/plain".to_owned())]
                .into_iter()
                .collect(),
        );
        put(&ctx, patch, 2000);

        let har = call_json(&HarExport, &ctx, json!({})).await;
        assert_eq!(
            entries(&har)[0]["request"]["postData"],
            json!({ "mimeType": "text/plain", "text": "a=1" })
        );
        assert_eq!(
            entries(&har)[1]["request"]["postData"],
            json!({ "mimeType": "application/octet-stream", "text": "a=1" })
        );
    }

    #[tokio::test]
    async fn 주소를_해석할_수_없는_항목은_건너뛰고_개수를_적는다() {
        let ctx = ToolContext::for_tests();
        put(&ctx, request("1", "https://a.test/ok", "GET", "XHR"), 1000);
        put(&ctx, request("2", "", "GET", "Other"), 2000);
        put(&ctx, request("3", "not a url", "GET", "Other"), 3000);

        let text = call(&HarExport, &ctx, json!({})).await.unwrap();
        assert!(
            text.ends_with("    ],\n    \"_skippedEntries\": 2\n  }\n}"),
            "{text}"
        );
        let har: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(urls(&har), ["https://a.test/ok"]);
    }

    #[tokio::test]
    async fn 본문은_옵션에_따라_빼고_base64와_잘림을_표시한다() {
        let ctx = ToolContext::for_tests();
        let mut binary = request("bin", "https://a.test/img", "GET", "Image");
        binary.response_body = Some("iVBORw==".to_owned());
        binary.response_body_base64 = Some(true);
        put(&ctx, binary, 1000);
        let mut long = request("long", "https://a.test/long", "GET", "XHR");
        // 한 is one UTF-16 unit: the cut and both sizes count units, as the TypeScript does.
        long.response_body = Some("한".repeat(BODY_TRUNCATE_BYTES + 10));
        put(&ctx, long, 2000);

        let har = call_json(&HarExport, &ctx, json!({})).await;
        let long = &entries(&har)[0]["response"]["content"];
        assert_eq!(long["size"], BODY_TRUNCATE_BYTES + 10);
        assert_eq!(
            long["text"].as_str().map(utf16_len),
            Some(BODY_TRUNCATE_BYTES)
        );
        assert_eq!(long["comment"], "truncated (original 262154 bytes)");
        assert_eq!(long.get("encoding"), None);
        assert_eq!(
            entries(&har)[1]["response"]["content"],
            json!({ "size": 8, "mimeType": "", "text": "iVBORw==", "encoding": "base64" })
        );

        let bare = call_json(&HarExport, &ctx, json!({ "includeBodies": false })).await;
        assert_eq!(
            entries(&bare)[1]["response"]["content"],
            json!({ "size": 8, "mimeType": "" })
        );
    }

    #[tokio::test]
    async fn host와_limit로_내보낼_항목을_고른다() {
        let ctx = ToolContext::for_tests();
        put(&ctx, request("1", "https://a.test/1", "GET", "XHR"), 1000);
        put(&ctx, request("2", "https://b.test/2", "GET", "XHR"), 2000);
        put(&ctx, request("3", "https://a.test/3", "GET", "XHR"), 3000);

        let by_host = call_json(&HarExport, &ctx, json!({ "host": "a.test" })).await;
        assert_eq!(urls(&by_host), ["https://a.test/3", "https://a.test/1"]);
        let limited = call_json(&HarExport, &ctx, json!({ "limit": 1 })).await;
        assert_eq!(urls(&limited), ["https://a.test/3"]);
        assert!(
            call(&HarExport, &ctx, json!({ "limit": 2001 }))
                .await
                .is_err()
        );
        assert!(
            call(&HarExport, &ctx, json!({ "before": "x" }))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn before_커서는_경계_항목과_같은_밀리초의_더_새_항목을_뺀다() {
        let ctx = ToolContext::for_tests();
        put(
            &ctx,
            request("old", "https://a.test/old", "GET", "XHR"),
            1000,
        );
        put(&ctx, request("s1", "https://a.test/s1", "GET", "XHR"), 2000);
        put(&ctx, request("s2", "https://a.test/s2", "GET", "XHR"), 2000);
        put(
            &ctx,
            request("new", "https://a.test/new", "GET", "XHR"),
            3000,
        );

        let page = call_json(
            &HarExport,
            &ctx,
            json!({ "before": 2000, "beforeId": "s2" }),
        )
        .await;
        assert_eq!(urls(&page), ["https://a.test/s1", "https://a.test/old"]);
        // Without beforeId the boundary millisecond stays inclusive.
        let inclusive = call_json(&HarExport, &ctx, json!({ "before": 2000 })).await;
        assert_eq!(urls(&inclusive).len(), 3);
        // A fractional boundary equals no startedAt: beforeId is not consulted.
        let fractional = call_json(
            &HarExport,
            &ctx,
            json!({ "before": 2000.5, "beforeId": "s2" }),
        )
        .await;
        assert_eq!(urls(&fractional).len(), 3);
        let negative = call_json(&HarExport, &ctx, json!({ "before": -1 })).await;
        assert!(urls(&negative).is_empty());
    }

    #[tokio::test]
    async fn sse_한도를_넘으면_최신_항목만_남기고_커서로_나머지를_이어_받는다() {
        let ctx = ToolContext::for_tests();
        // 8 entries of ~200KB each: 1.6MB against the 900KB bound. Two share a
        // millisecond, so paging needs the id half of the cursor too.
        for n in 0..8_u64 {
            let mut patch = request(
                &format!("r{n}"),
                &format!("https://a.test/{n}"),
                "GET",
                "XHR",
            );
            patch.response_body = Some("x".repeat(200 * 1024));
            put(&ctx, patch, 1000 + n / 2);
        }

        let mut seen: Vec<String> = Vec::new();
        let mut args = json!({});
        for _ in 0..8 {
            let text = call(&HarExport, &ctx, args.clone()).await.unwrap();
            assert!(
                text.len() <= SSE_SAFE_RESPONSE_BYTES,
                "{} bytes",
                text.len()
            );
            let har: Value = serde_json::from_str(&text).unwrap();
            let page = urls(&har);
            assert!(!page.is_empty());
            seen.extend(page.iter().map(|url| (*url).to_owned()));

            let log = &har["log"];
            if log.get("_truncated").is_none() {
                assert_eq!(log.get("_nextBefore"), None);
                break;
            }
            assert_eq!(log["_truncated"], true);
            assert_eq!(log["_droppedEntries"], 8 - seen.len());
            assert_eq!(
                log["_continuationHint"],
                "Response was cut to fit the MCP SSE event-size limit. Re-call har_export \
                 with before=_nextBefore and beforeId=_nextBeforeId to fetch the older remainder."
            );
            // The cursor names the oldest entry of this page.
            let last = seen.last().and_then(|url| url.rsplit('/').next()).unwrap();
            assert_eq!(log["_nextBeforeId"], format!("r{last}"));
            args = json!({ "before": log["_nextBefore"], "beforeId": log["_nextBeforeId"] });
        }
        let expected: Vec<String> = (0..8)
            .rev()
            .map(|n| format!("https://a.test/{n}"))
            .collect();
        assert_eq!(seen, expected);
    }

    #[tokio::test]
    async fn 잘린_응답의_키_순서는_entries_뒤에_커서_필드가_온다() {
        let ctx = ToolContext::for_tests();
        for n in 0..6_u64 {
            let mut patch = request(
                &format!("r{n}"),
                &format!("https://a.test/{n}"),
                "GET",
                "XHR",
            );
            patch.response_body = Some("x".repeat(200 * 1024));
            put(&ctx, patch, 1000 + n);
        }
        put(&ctx, request("bad", "", "GET", "Other"), 5000);
        let har = call_json(&HarExport, &ctx, json!({})).await;
        let keys: Vec<&String> = har["log"].as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "version",
                "creator",
                "entries",
                "_skippedEntries",
                "_truncated",
                "_droppedEntries",
                "_nextBefore",
                "_nextBeforeId",
                "_continuationHint"
            ]
        );
    }

    fn huge_request() -> RequestPatch {
        let mut huge = request("huge", "https://a.test/upload", "POST", "XHR");
        // Post data is never truncated, so this entry alone is over the bound.
        huge.request_post_data = Some("a".repeat(1024 * 1024));
        huge
    }

    #[tokio::test]
    async fn 항목_하나로도_한도를_넘으면_에러를_돌려준다() {
        let ctx = ToolContext::for_tests();
        put(&ctx, huge_request(), 2000);
        let error = call(&HarExport, &ctx, json!({})).await.unwrap_err();
        assert!(
            error.starts_with("HAR output exceeds the SSE-safe size ("),
            "{error}"
        );
        assert!(error.ends_with(
            " bytes) even with a single entry. Fetch it directly with get_request instead."
        ));
    }

    #[tokio::test]
    async fn 가장_새_항목이_한도를_넘으면_에러이고_커서로_그_뒤를_받을_수_있다() {
        let ctx = ToolContext::for_tests();
        put(
            &ctx,
            request("small", "https://a.test/s", "GET", "XHR"),
            1000,
        );
        put(&ctx, huge_request(), 2000);
        // The newest entry cannot be dropped, so nothing fits.
        let error = call(&HarExport, &ctx, json!({})).await.unwrap_err();
        assert!(
            error.starts_with("HAR output exceeds the SSE-safe size ("),
            "{error}"
        );
        let older = call_json(
            &HarExport,
            &ctx,
            json!({ "before": 2000, "beforeId": "huge" }),
        )
        .await;
        assert_eq!(urls(&older), ["https://a.test/s"]);
    }

    #[tokio::test]
    async fn 오래된_항목이_한도를_넘으면_그_앞까지만_내보내고_커서를_준다() {
        let ctx = ToolContext::for_tests();
        put(&ctx, huge_request(), 1000);
        put(
            &ctx,
            request("small", "https://a.test/s", "GET", "XHR"),
            2000,
        );
        let har = call_json(&HarExport, &ctx, json!({})).await;
        assert_eq!(urls(&har), ["https://a.test/s"]);
        assert_eq!(har["log"]["_droppedEntries"], 1);
        assert_eq!(har["log"]["_nextBefore"], 2000);
        assert_eq!(har["log"]["_nextBeforeId"], "small");
    }

    #[tokio::test]
    async fn date가_표현할_수_없는_시각은_자바스크립트와_같은_에러다() {
        let ctx = ToolContext::for_tests();
        put(
            &ctx,
            request("1", "https://a.test/", "GET", "XHR"),
            8_640_000_000_000_001,
        );
        assert_eq!(
            call(&HarExport, &ctx, json!({})).await,
            Err("Invalid time value".to_owned())
        );
    }

    #[test]
    fn 가져오기_id는_라벨에서_허용_문자만_24자까지_쓴다() {
        assert_eq!(import_id(Some("my import/01!"), 0), "myimport01");
        assert_eq!(import_id(Some("a_b-C"), 0), "a_b-C");
        assert_eq!(import_id(Some(&"x".repeat(40)), 0), "x".repeat(24));
        assert_eq!(import_id(Some("한글 !!"), 0), "imp");
        assert_eq!(import_id(Some(""), 0), "imp");
        // No label: Date.now().toString(36).
        assert_eq!(import_id(None, 1_700_000_000_123), "loyw3v5n");
    }

    #[test]
    fn 숫자를_36진수로_쓴다() {
        assert_eq!(to_base36(0), "0");
        assert_eq!(to_base36(35), "z");
        assert_eq!(to_base36(36), "10");
        assert_eq!(to_base36(1_700_000_000_123), "loyw3v5n");
    }

    /// A file in the temp folder, removed when dropped.
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str, contents: &[u8]) -> Self {
            let path = std::env::temp_dir()
                .join(format!("rever-tools-traffic-{}-{name}", std::process::id()));
            std::fs::write(&path, contents).expect("write temp file");
            Self(path)
        }

        fn path(&self) -> &str {
            self.0.to_str().expect("utf-8 temp path")
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[tokio::test]
    async fn path가_없거나_상대_경로거나_파일이_없으면_에러다() {
        let ctx = ToolContext::for_tests();
        let no_path = Err("pass path (absolute .har file) or clear=true".to_owned());
        assert_eq!(call(&ImportHar, &ctx, json!({})).await, no_path);
        assert_eq!(call(&ImportHar, &ctx, json!({ "path": "" })).await, no_path);
        assert_eq!(
            call(&ImportHar, &ctx, json!({ "clear": false })).await,
            no_path
        );
        assert_eq!(
            call(&ImportHar, &ctx, json!({ "path": "captures/a.har" })).await,
            Err("path must be absolute".to_owned())
        );
        // The errors below are worded as Node words them.
        let missing = std::env::temp_dir().join("rever-tools-traffic-no-such-file.har");
        let missing = missing.to_str().expect("utf-8 temp path");
        assert_eq!(
            call(&ImportHar, &ctx, json!({ "path": missing })).await,
            Err(format!(
                "ENOENT: no such file or directory, stat '{missing}'"
            ))
        );
        let file = TempFile::new("plain-file.har", b"{}");
        let under_file = format!("{}/x.har", file.path());
        assert_eq!(
            call(&ImportHar, &ctx, json!({ "path": under_file })).await,
            Err(format!("ENOTDIR: not a directory, stat '{under_file}'"))
        );
        // A directory passes stat and fails in the read, inside the TypeScript's
        // JSON try block.
        let dir = std::env::temp_dir();
        assert_eq!(
            call(&ImportHar, &ctx, json!({ "path": dir })).await,
            Err("not valid JSON: EISDIR: illegal operation on a directory, read".to_owned())
        );
        assert!(call(&ImportHar, &ctx, json!({ "path": 1 })).await.is_err());
    }

    #[tokio::test]
    async fn json이_아니거나_har이_아니면_이유를_알린다() {
        let ctx = ToolContext::for_tests();
        let text = TempFile::new("not-json.har", b"<html>");
        let error = call(&ImportHar, &ctx, json!({ "path": text.path() }))
            .await
            .unwrap_err();
        assert!(error.starts_with("not valid JSON: "), "{error}");

        let json_file = TempFile::new("not-har.har", br#"{"log":{"pages":[]}}"#);
        assert_eq!(
            call(&ImportHar, &ctx, json!({ "path": json_file.path() })).await,
            Err("not a HAR file (missing log.entries)".to_owned())
        );
        assert_eq!(lock(&ctx.traffic).import_count(), 0);
    }

    #[test]
    fn 짝_없는_서로게이트_이스케이프만_fffd로_바꾼다() {
        let fixed = |json: &str| {
            let mut bytes = json.as_bytes().to_vec();
            replace_lone_surrogate_escapes(&mut bytes);
            String::from_utf8(bytes).expect("utf-8")
        };
        // A lone low, a lone high (either hex case), a pair, a lone high after a pair.
        assert_eq!(
            fixed(r#""a\udcffb\uD83Dc\ud83d\ude00d\ud83d""#),
            r#""a\ufffdb\ufffdc\ud83d\ude00d\ufffd""#
        );
        // Two highs in a row: the first is lone, the second has its partner.
        assert_eq!(fixed(r#""\ud83d\ud83d\ude00""#), r#""\ufffd\ud83d\ude00""#);
        // An escaped backslash followed by the letters "ud83d" is text.
        assert_eq!(fixed(r#""\\ud83d \\\ud83d""#), r#""\\ud83d \\\ufffd""#);
        // Other escapes, a cut-off escape and non-ASCII text are left alone.
        for same in [r#""\n\u0041\"한😀""#, r#""\ud8"#, "\\", ""] {
            assert_eq!(fixed(same), same);
        }
        // A high surrogate whose partner is cut off is lone.
        assert_eq!(fixed(r#""\ud83d\ude"#), r#""\ufffd\ude"#);
    }

    #[tokio::test]
    async fn 짝_없는_서로게이트가_든_har도_json_parse처럼_가져온다() {
        let ctx = ToolContext::for_tests();
        // What Python's json.dumps writes for text decoded with surrogateescape.
        let file = TempFile::new(
            "lone-surrogate.har",
            br#"{"log":{"entries":[{"request":{"method":"POST","url":"https://m.test/a",
                "postData":{"text":"p\udc80q\\ud83d"}},
                "response":{"status":200,"content":{"mimeType":"text/plain","text":"a\udcffb\ud83d\ude00"}}}]}}"#,
        );
        let out = call_json(
            &ImportHar,
            &ctx,
            json!({ "path": file.path(), "label": "m" }),
        )
        .await;
        assert_eq!(out["imported"], 1);
        let store = lock(&ctx.traffic);
        let entry = store.get("har:m:0").expect("imported entry");
        // Same UTF-16 length as the JavaScript string: 5 units.
        assert_eq!(entry.response_body.as_deref(), Some("a\u{fffd}b😀"));
        assert_eq!(
            entry.request_post_data.as_deref(),
            Some("p\u{fffd}q\\ud83d")
        );
    }

    #[tokio::test]
    async fn 한도보다_큰_파일은_읽지_않고_거절한다() {
        let ctx = ToolContext::for_tests();
        let big = TempFile::new("big.har", b"");
        // Sparse: only the length matters, the file is never read.
        std::fs::File::options()
            .write(true)
            .open(&big.0)
            .and_then(|file| file.set_len(MAX_HAR_BYTES + 1))
            .expect("grow temp file");
        assert_eq!(
            call(&ImportHar, &ctx, json!({ "path": big.path() })).await,
            Err("HAR is 314572801 bytes — over the 314572800-byte import limit".to_owned())
        );
    }

    const SAMPLE_HAR: &str = r#"{ "log": {
        "creator": { "name": "WebInspector", "version": "537.36" },
        "entries": [
            { "startedDateTime": "2024-01-02T03:04:05.000Z", "time": 12, "_resourceType": "xhr",
              "request": { "method": "GET", "url": "https://api.a.test/v1/items" },
              "response": { "status": 200, "content": { "mimeType": "application/json", "text": "{}" } } },
            { "startedDateTime": "2024-01-02T03:04:06.500Z", "_resourceType": "xhr",
              "request": { "method": "POST", "url": "https://api.a.test/v1/items" },
              "response": { "status": 201, "content": { "mimeType": "application/json" } } },
            { "startedDateTime": "2024-01-02T03:04:04.000Z", "_resourceType": "script",
              "request": { "method": "GET", "url": "https://cdn.b.test/app.js" },
              "response": { "status": 200, "content": { "mimeType": "application/javascript" } } },
            { "request": { "url": "not a url" } }
        ]
    } }"#;

    #[tokio::test]
    async fn har을_가져와_요약을_돌려주고_가져온_항목을_조회할_수_있다() {
        let ctx = ToolContext::for_tests();
        let file = TempFile::new("sample.har", SAMPLE_HAR.as_bytes());
        let text = call(
            &ImportHar,
            &ctx,
            json!({ "path": file.path(), "label": "t 1" }),
        )
        .await
        .unwrap();
        assert_eq!(
            text,
            r#"{
  "importId": "t1",
  "creator": "WebInspector 537.36",
  "imported": 3,
  "skipped": 1,
  "skippedExamples": [
    {
      "index": 3,
      "reason": "invalid request.url"
    }
  ],
  "byType": {
    "XHR": 2,
    "Script": 1
  },
  "topHosts": [
    [
      "api.a.test",
      2
    ],
    [
      "cdn.b.test",
      1
    ]
  ],
  "timeRange": [
    "2024-01-02T03:04:04.000Z",
    "2024-01-02T03:04:06.500Z"
  ],
  "next": "list_requests source=\"import\" (filter by host / methodOrType), then get_request etc.",
  "limitations": [
    "Only what the HAR recorded: bodies the exporter dropped or truncated stay missing.",
    "grep_scripts / list_scripts search live capture only; get_request-based tools (resolve_source, request_diff, export_client) work on imports."
  ]
}"#
        );
        let store = lock(&ctx.traffic);
        assert_eq!(store.import_count(), 3);
        assert_eq!(
            store.get("har:t1:0").map(|e| e.url.as_str()),
            Some("https://api.a.test/v1/items")
        );
        // Imports stay out of the live capture.
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn 시각과_제작자가_없는_har은_null로_적는다() {
        let ctx = ToolContext::for_tests();
        let file = TempFile::new(
            "bare.har",
            br#"{"log":{"entries":[{"request":{"url":"https://a.test/"}}]}}"#,
        );
        let out = call_json(&ImportHar, &ctx, json!({ "path": file.path() })).await;
        assert_eq!(out["creator"], Value::Null);
        assert_eq!(out["timeRange"], Value::Null);
        assert_eq!(out["imported"], 1);
        assert_eq!(out["skippedExamples"], json!([]));
        // Without a label the id is the clock in base 36.
        let id = out["importId"].as_str().unwrap();
        assert!(
            !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()),
            "{id}"
        );
    }

    #[tokio::test]
    async fn clear는_가져온_항목을_모두_지우고_개수를_알린다() {
        let ctx = ToolContext::for_tests();
        let file = TempFile::new("clear.har", SAMPLE_HAR.as_bytes());
        call(&ImportHar, &ctx, json!({ "path": file.path() }))
            .await
            .unwrap();
        put(
            &ctx,
            RequestPatch {
                request_id: "live".to_owned(),
                ..RequestPatch::default()
            },
            1,
        );

        // clear wins over path.
        assert_eq!(
            call(
                &ImportHar,
                &ctx,
                json!({ "clear": true, "path": file.path() })
            )
            .await
            .unwrap(),
            r#"{"cleared":3}"#
        );
        let store = lock(&ctx.traffic);
        assert_eq!(store.import_count(), 0);
        assert_eq!(store.len(), 1);
    }
}
