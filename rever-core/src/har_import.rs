//! Port of `src/main/har-import.ts`: HAR 1.2 → `StoredRequest`, for import_har.
//! Lets every traffic tool (get_request, request_diff, export_client, ...) work
//! on captures made elsewhere — DevTools, Burp, Charles, mitmproxy (`hardump`).
//! Idea adapted from morluto/rea's inspect_web_network_capture (MIT).

use std::fmt;

use serde::Serialize;
use serde_json::Value;

use crate::traffic::{CallFrame, Headers, StoredRequest};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Skipped {
    pub index: usize,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HarImportResult {
    pub entries: Vec<StoredRequest>,
    pub skipped: Vec<Skipped>,
    /// `null` when the HAR names no creator.
    pub creator: Option<String>,
}

/// The input has no `log.entries` array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotAHar;

impl fmt::Display for NotAHar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not a HAR file (missing log.entries)")
    }
}

impl std::error::Error for NotAHar {}

/// Chrome's `_resourceType` values → the CDP casing rever stores.
fn resource_type_of(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "document" => "Document",
        "stylesheet" => "Stylesheet",
        "image" => "Image",
        "media" => "Media",
        "font" => "Font",
        "script" => "Script",
        "xhr" => "XHR",
        "fetch" => "Fetch",
        "websocket" => "WebSocket",
        "manifest" => "Manifest",
        "ping" => "Ping",
        "preflight" => "Preflight",
        _ => return None,
    })
}

fn guess_resource_type(mime_type: &str) -> &'static str {
    let has = |part| mime_type.contains(part);
    if has("javascript") || has("ecmascript") {
        "Script"
    } else if mime_type == "text/html" {
        "Document"
    } else if has("json") {
        "Fetch"
    } else if mime_type == "text/css" {
        "Stylesheet"
    } else if mime_type.starts_with("image/") {
        "Image"
    } else if mime_type.starts_with("font/") || has("woff") || has("ttf") || has("otf") {
        "Font"
    } else {
        "Other"
    }
}

/// What JavaScript's string conversion gives for a scalar; objects and arrays
/// come out as JSON text instead of `[object Object]`.
fn js_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// Repeated fields are folded with '\n' — the CDP convention har_export's
/// toHeaders() splits back apart, so an export → import round-trips.
fn header_record(headers: Option<&Value>) -> Headers {
    let mut out = Headers::new();
    for header in headers.and_then(Value::as_array).into_iter().flatten() {
        let Some(name) = header.get("name").and_then(Value::as_str) else {
            continue;
        };
        let value = header.get("value");
        match out.get_mut(name) {
            Some(folded) => {
                folded.push('\n');
                // The TypeScript interpolates here, so a missing value reads "undefined".
                folded.push_str(&value.map_or_else(|| "undefined".to_owned(), js_text));
            }
            None => {
                let first = value.filter(|value| !value.is_null()).map(js_text);
                out.insert(name.to_owned(), first.unwrap_or_default());
            }
        }
    }
    out
}

fn text<'a>(object: Option<&'a Value>, key: &str) -> Option<&'a str> {
    object?.get(key)?.as_str()
}

/// `Date.parse` for the ISO 8601 shapes HAR writers emit: a date, optionally a
/// time, optionally `Z` or a `±HH:mm` offset. Anything else is `None`.
/// A time without an offset is read as UTC (JavaScript uses the local zone).
fn parse_iso_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let number = |from: usize, to: usize| -> Option<i64> {
        let digits = bytes.get(from..to)?;
        digits
            .iter()
            .all(u8::is_ascii_digit)
            .then(|| digits.iter().fold(0, |n, d| n * 10 + i64::from(d - b'0')))
    };
    let is = |at: usize, expected: u8| bytes.get(at) == Some(&expected);

    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    // Days past the month's end roll over into the next month, as V8 does.
    if !is(4, b'-') || !is(7, b'-') || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut ms = days_from_civil(year, month, day) * 86_400_000;
    if bytes.len() == 10 {
        return Some(ms);
    }
    if !matches!(bytes.get(10), Some(b'T' | b't' | b' ')) || !is(13, b':') {
        return None;
    }
    let (hour, minute) = (number(11, 13)?, number(14, 16)?);
    let mut at = 16;
    let mut second = 0;
    if is(at, b':') {
        second = number(17, 19)?;
        at = 19;
        if is(at, b'.') {
            let digits = bytes.get(at + 1..)?;
            let count = digits.iter().take_while(|d| d.is_ascii_digit()).count();
            if count == 0 {
                return None;
            }
            // Milliseconds are the first three digits; the rest is dropped.
            ms += (0..3).fold(0, |n, i| {
                n * 10
                    + digits
                        .get(i)
                        .filter(|_| i < count)
                        .map_or(0, |d| i64::from(d - b'0'))
            });
            at += 1 + count;
        }
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    ms += (hour * 3600 + minute * 60 + second) * 1000;

    let offset = bytes.get(at..)?;
    let offset_minutes = match offset {
        [] | [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), rest @ ..] => {
            let (hours, minutes) = match rest.len() {
                5 if rest.get(2) == Some(&b':') => {
                    (number(at + 1, at + 3)?, number(at + 4, at + 6)?)
                }
                4 => (number(at + 1, at + 3)?, number(at + 3, at + 5)?),
                _ => return None,
            };
            if hours > 23 || minutes > 59 {
                return None;
            }
            (hours * 60 + minutes) * if *sign == b'-' { -1 } else { 1 }
        }
        _ => return None,
    };
    Some(ms - offset_minutes * 60_000)
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar (Howard Hinnant's
/// days_from_civil).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_from_march = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// `new URL(url).host`: the hostname plus a non-default port.
fn url_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str().unwrap_or_default();
    Some(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

fn to_entry(raw: &Value, request_id: String) -> Option<StoredRequest> {
    let request = raw.get("request");
    let response = raw.get("response");
    let content = response.and_then(|response| response.get("content"));
    let initiator = raw.get("_initiator");

    let url = text(request, "url")?;
    let host = url_host(url)?;
    let started_at = text(Some(raw), "startedDateTime")
        .and_then(parse_iso_ms)
        .and_then(|ms| u64::try_from(ms).ok())
        .unwrap_or(0);
    let mime_type = text(content, "mimeType")
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim();
    let non_negative = |value: Option<&Value>| {
        value
            .and_then(Value::as_f64)
            .filter(|number| *number >= 0.0)
    };
    let transfer = response.and_then(|response| {
        response
            .get("_transferSize")
            .filter(|size| !size.is_null())
            .or_else(|| response.get("bodySize"))
    });
    let frames = initiator
        .and_then(|initiator| initiator.get("stack")?.get("callFrames")?.as_array())
        .filter(|frames| !frames.is_empty());
    let body = text(content, "text");

    Some(StoredRequest {
        request_id,
        url: url.to_owned(),
        host,
        method: text(request, "method").unwrap_or("GET").to_owned(),
        resource_type: resource_type_of(text(Some(raw), "_resourceType").unwrap_or_default())
            .unwrap_or_else(|| guess_resource_type(mime_type))
            .to_owned(),
        started_at,
        completed_at: non_negative(raw.get("time")).map(|time| (started_at as f64 + time) as u64),
        status: response
            .and_then(|response| response.get("status")?.as_u64())
            .filter(|status| *status != 0)
            .and_then(|status| u16::try_from(status).ok()),
        mime_type: (!mime_type.is_empty()).then(|| mime_type.to_owned()),
        encoded_data_length: non_negative(transfer).map(|size| size as u64),
        request_headers: Some(header_record(request.and_then(|r| r.get("headers")))),
        request_post_data: text(request.and_then(|r| r.get("postData")), "text").map(str::to_owned),
        response_headers: Some(header_record(response.and_then(|r| r.get("headers")))),
        response_body: body.map(str::to_owned),
        response_body_base64: (body.is_some() && text(content, "encoding") == Some("base64"))
            .then_some(true),
        initiator_type: text(initiator, "type")
            .filter(|kind| !kind.is_empty())
            .map(str::to_owned),
        initiator_stack: frames.map(|frames| {
            frames
                .iter()
                .map(|frame| CallFrame {
                    function_name: text(Some(frame), "functionName")
                        .unwrap_or_default()
                        .to_owned(),
                    url: text(Some(frame), "url").unwrap_or_default().to_owned(),
                    line_number: frame.get("lineNumber").and_then(Value::as_i64).unwrap_or(0),
                    column_number: frame
                        .get("columnNumber")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                })
                .collect()
        }),
        ..StoredRequest::default()
    })
}

pub fn har_to_entries(har: &Value, import_id: &str) -> Result<HarImportResult, NotAHar> {
    let log = har.get("log").ok_or(NotAHar)?;
    let raw_entries = log
        .get("entries")
        .and_then(Value::as_array)
        .ok_or(NotAHar)?;

    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    for (index, raw) in raw_entries.iter().enumerate() {
        match to_entry(raw, format!("har:{import_id}:{index}")) {
            Some(entry) => entries.push(entry),
            None => skipped.push(Skipped {
                index,
                reason: "invalid request.url".into(),
            }),
        }
    }

    // The TypeScript tests truthiness and then interpolates, so a numeric
    // version (`"version": 537`) still prints.
    let truthy = |key: &str| {
        let value = log.get("creator")?.get(key)?;
        js_truthy(value).then(|| js_text(value))
    };
    let creator = truthy("name").map(|name| match truthy("version") {
        Some(version) => format!("{name} {version}"),
        None => name,
    });
    Ok(HarImportResult {
        entries,
        skipped,
        creator,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // Date.parse('2026-10-08T01:02:03.000Z')
    const STARTED_AT: u64 = 1_791_421_323_000;

    fn chrome_entry() -> Value {
        json!({
            "startedDateTime": "2026-10-08T01:02:03.000Z",
            "time": 120,
            "_resourceType": "fetch",
            "_initiator": {
                "type": "script",
                "stack": {
                    "callFrames": [
                        { "functionName": "callApi", "url": "https://x.io/app.js", "lineNumber": 4, "columnNumber": 10 },
                        { "functionName": "", "url": "https://x.io/app.js", "lineNumber": 9, "columnNumber": 2 }
                    ]
                }
            },
            "request": {
                "method": "POST",
                "url": "https://api.x.io/v3/login?x=1",
                "headers": [
                    { "name": "content-type", "value": "application/json" },
                    { "name": "authorization", "value": "Bearer t" }
                ],
                "postData": { "mimeType": "application/json", "text": "{\"u\":\"me\"}" }
            },
            "response": {
                "status": 200,
                "headers": [
                    { "name": "content-type", "value": "application/json; charset=utf-8" },
                    { "name": "set-cookie", "value": "a=1" },
                    { "name": "set-cookie", "value": "b=2" }
                ],
                "content": { "size": 4, "mimeType": "application/json; charset=utf-8", "text": "e30=", "encoding": "base64" },
                "_transferSize": 321
            }
        })
    }

    fn har(entries: Vec<Value>) -> Value {
        json!({ "log": { "version": "1.2", "creator": { "name": "WebInspector", "version": "537" }, "entries": entries } })
    }

    /// `{ ...entry, [key]: value }`
    fn with(mut entry: Value, key: &str, value: Value) -> Value {
        entry.as_object_mut().unwrap().insert(key.to_owned(), value);
        entry
    }

    fn headers<const N: usize>(pairs: [(&str, &str); N]) -> Headers {
        pairs
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect()
    }

    fn frame(function_name: &str, line_number: i64, column_number: i64) -> CallFrame {
        CallFrame {
            function_name: function_name.into(),
            url: "https://x.io/app.js".into(),
            line_number,
            column_number,
        }
    }

    #[test]
    fn chrome_har_항목을_stored_request로_바꾼다() {
        let result = har_to_entries(&har(vec![chrome_entry()]), "imp1").unwrap();
        assert!(result.skipped.is_empty());
        assert_eq!(result.creator.as_deref(), Some("WebInspector 537"));
        assert_eq!(
            result.entries[0],
            StoredRequest {
                request_id: "har:imp1:0".into(),
                url: "https://api.x.io/v3/login?x=1".into(),
                host: "api.x.io".into(),
                method: "POST".into(),
                resource_type: "Fetch".into(),
                started_at: STARTED_AT,
                completed_at: Some(STARTED_AT + 120),
                status: Some(200),
                mime_type: Some("application/json".into()),
                encoded_data_length: Some(321),
                request_headers: Some(headers([
                    ("content-type", "application/json"),
                    ("authorization", "Bearer t")
                ])),
                request_post_data: Some("{\"u\":\"me\"}".into()),
                response_headers: Some(headers([
                    ("content-type", "application/json; charset=utf-8"),
                    ("set-cookie", "a=1\nb=2")
                ])),
                response_body: Some("e30=".into()),
                response_body_base64: Some(true),
                initiator_type: Some("script".into()),
                initiator_stack: Some(vec![frame("callApi", 4, 10), frame("", 9, 2)]),
                ..StoredRequest::default()
            }
        );
    }

    #[test]
    fn resource_type이_없으면_mime_type으로_종류를_추정한다() {
        let kinds: Vec<String> = [
            "application/javascript",
            "text/html",
            "application/json",
            "text/css",
            "image/png",
            "font/woff2",
            "application/zip",
        ]
        .into_iter()
        .map(|mime_type| {
            let entry = json!({
                "startedDateTime": "2026-10-08T00:00:00Z",
                "time": -1,
                "request": { "method": "GET", "url": "https://x.io/a", "headers": [] },
                "response": { "status": 200, "headers": [], "content": { "mimeType": mime_type } }
            });
            let result = har_to_entries(&har(vec![entry]), "i").unwrap();
            result.entries[0].resource_type.clone()
        })
        .collect();
        assert_eq!(
            kinds,
            [
                "Script",
                "Document",
                "Fetch",
                "Stylesheet",
                "Image",
                "Font",
                "Other"
            ]
        );
    }

    #[test]
    fn url이_잘못된_항목은_건너뛰고_이유와_순번을_보고한다() {
        let request = with(chrome_entry()["request"].clone(), "url", json!("not a url"));
        let bad = with(chrome_entry(), "request", request);
        let result = har_to_entries(&har(vec![bad, chrome_entry()]), "i").unwrap();
        let ids: Vec<&str> = result
            .entries
            .iter()
            .map(|e| e.request_id.as_str())
            .collect();
        assert_eq!(ids, ["har:i:1"]);
        assert_eq!(
            result.skipped,
            [Skipped {
                index: 0,
                reason: "invalid request.url".into()
            }]
        );
    }

    #[test]
    fn time이_음수면_completed_at을_두지_않는다() {
        let entry = with(chrome_entry(), "time", json!(-1));
        let result = har_to_entries(&har(vec![entry]), "i").unwrap();
        assert_eq!(result.entries[0].completed_at, None);
    }

    #[test]
    fn har가_아니면_에러를_돌려준다() {
        for not_har in [json!({ "foo": 1 }), Value::Null] {
            let error = har_to_entries(&not_har, "i").unwrap_err();
            assert!(error.to_string().contains("not a HAR"));
        }
    }

    #[test]
    fn har_export의_헤더_형식과_왕복해도_같다() {
        let record = headers([("set-cookie", "a=1\nb=2"), ("accept", "*/*")]);
        // har-build.ts's toHeaders(): one HAR header per '\n'-separated line.
        let exported: Vec<Value> = record
            .iter()
            .flat_map(|(name, value)| {
                value
                    .split('\n')
                    .map(move |line| json!({ "name": name, "value": line }))
            })
            .collect();
        let response = with(
            chrome_entry()["response"].clone(),
            "headers",
            json!(exported),
        );
        let entry = with(chrome_entry(), "response", response);
        let result = har_to_entries(&har(vec![entry]), "i").unwrap();
        assert_eq!(result.entries[0].response_headers, Some(record));
    }

    #[test]
    fn e2e용_실제_픽스처_shop_api_har를_그대로_가져온다() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../test-fixtures/har/shop-api.har"
        );
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let result = har_to_entries(&raw, "shop").unwrap();
        assert!(result.skipped.is_empty());
        let rows: Vec<(&str, &str, Option<u16>)> = result
            .entries
            .iter()
            .map(|e| (e.method.as_str(), e.resource_type.as_str(), e.status))
            .collect();
        assert_eq!(
            rows,
            [
                ("GET", "Document", Some(200)),
                ("GET", "Script", Some(200)),
                ("POST", "Fetch", Some(200)),
                ("GET", "Fetch", Some(200)),
                ("GET", "Fetch", Some(401))
            ]
        );
        let header = |headers: &Option<Headers>, name: &str| {
            headers.as_ref().and_then(|h| h.get(name).cloned())
        };
        assert_eq!(
            header(&result.entries[2].response_headers, "set-cookie").as_deref(),
            Some("refresh=rt_abc; HttpOnly\nsid=s_123; Secure")
        );
        assert_eq!(
            header(&result.entries[3].request_headers, "x-signature").as_deref(),
            Some("a1b2c3d4e5f60718293a4b5c6d7e8f90")
        );
    }

    // The cases below are not in the TypeScript suite. Their expected values
    // were taken from running the TypeScript on the same inputs.

    #[test]
    fn creator는_이름이_있을_때만_버전과_함께_적는다() {
        let creator = |creator: Value| {
            let har = json!({ "log": { "creator": creator, "entries": [] } });
            har_to_entries(&har, "i").unwrap().creator
        };
        assert_eq!(
            creator(json!({ "name": "X", "version": 537 })).as_deref(),
            Some("X 537")
        );
        assert_eq!(creator(json!({ "name": "X" })).as_deref(), Some("X"));
        assert_eq!(
            creator(json!({ "name": "X", "version": "" })).as_deref(),
            Some("X")
        );
        assert_eq!(creator(json!({ "name": "", "version": "1" })), None);
        assert_eq!(creator(Value::Null), None);
    }

    #[test]
    fn 형식이_어긋난_항목도_타입스크립트와_같은_값을_낸다() {
        let entry = json!({
            "startedDateTime": "garbage",
            "time": 0,
            "_resourceType": "Fetch",
            "_initiator": { "type": "parser", "stack": { "callFrames": [{}, { "lineNumber": 3 }] } },
            "request": {
                "url": "HTTP://User:pw@Ex.com:80/a",
                "method": "",
                "headers": [
                    { "name": "a", "value": null }, { "name": "a" }, { "name": "a", "value": null },
                    { "name": "b", "value": 5 }, { "name": "b", "value": true },
                    { "name": "c" }, null, { "value": "x" }, { "name": 7, "value": "x" }
                ],
                "postData": { "text": "" }
            },
            "response": {
                "status": 0,
                "_transferSize": null,
                "bodySize": 10,
                "content": { "text": "", "encoding": "base64", "mimeType": " Text/HTML ; charset=x" }
            }
        });
        let result = har_to_entries(&json!({ "log": { "entries": [null, entry] } }), "p").unwrap();
        assert_eq!(
            result.skipped,
            [Skipped {
                index: 0,
                reason: "invalid request.url".into()
            }]
        );
        assert_eq!(
            result.entries,
            [StoredRequest {
                request_id: "har:p:1".into(),
                url: "HTTP://User:pw@Ex.com:80/a".into(),
                host: "ex.com".into(),
                method: String::new(),
                // `_resourceType` and the mime type are matched case-sensitively.
                resource_type: "Other".into(),
                started_at: 0,
                completed_at: Some(0),
                mime_type: Some("Text/HTML".into()),
                encoded_data_length: Some(10),
                request_headers: Some(headers([
                    ("a", "\nundefined\nnull"),
                    ("b", "5\ntrue"),
                    ("c", "")
                ])),
                request_post_data: Some(String::new()),
                response_headers: Some(Headers::new()),
                response_body: Some(String::new()),
                response_body_base64: Some(true),
                initiator_type: Some("parser".into()),
                initiator_stack: Some(vec![
                    CallFrame::default(),
                    CallFrame {
                        line_number: 3,
                        ..CallFrame::default()
                    }
                ]),
                ..StoredRequest::default()
            }]
        );
        assert_eq!(result.creator, None);
    }

    #[test]
    fn 호스트는_url_객체의_host와_같다() {
        let host = |url: &str| {
            let entry = json!({ "request": { "url": url } });
            let result = har_to_entries(&json!({ "log": { "entries": [entry] } }), "i").unwrap();
            result.entries.first().map(|e| e.host.clone())
        };
        assert_eq!(host("http://[::1]:8080/").as_deref(), Some("[::1]:8080"));
        assert_eq!(host("data:text/plain,hi").as_deref(), Some(""));
        assert_eq!(host("  https://a.com/x  ").as_deref(), Some("a.com"));
        assert_eq!(host("http://한글.kr/a").as_deref(), Some("xn--bj0bj06e.kr"));
        assert_eq!(host("http://a.com:99999"), None);
        assert_eq!(host("//x.io/a"), None);
    }

    // The date parser is hand-written here, so its
    // expectations are pinned against values taken from Node's Date.parse.
    #[test]
    fn 날짜_파서는_date_parse와_같은_값을_낸다() {
        assert_eq!(
            parse_iso_ms("2026-10-08T01:02:03.000Z"),
            Some(1_791_421_323_000)
        );
        assert_eq!(parse_iso_ms("2026-10-08"), Some(1_791_417_600_000));
        assert_eq!(
            parse_iso_ms("2026-10-08T01:02:03.1234567+09:00"),
            Some(1_791_388_923_123)
        );
        assert_eq!(
            parse_iso_ms("2026-10-08T01:02:03+0900"),
            Some(1_791_388_923_000)
        );
        assert_eq!(
            parse_iso_ms("2026-10-08T01:02:03.5Z"),
            Some(1_791_421_323_500)
        );
        assert_eq!(
            parse_iso_ms("2026-02-30T00:00:00Z"),
            Some(1_772_409_600_000)
        );
        assert_eq!(parse_iso_ms("2026-10-08T01:02:03.Z"), None);
        assert_eq!(parse_iso_ms("2026-10-08T1:02:03Z"), None);
        assert_eq!(parse_iso_ms(""), None);
    }
}
