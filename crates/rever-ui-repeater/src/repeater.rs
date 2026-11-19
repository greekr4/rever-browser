//! Port of `src/renderer/src/stores/repeater.ts` — the HTTP repeater's state: the request
//! being edited, where it came from, and the responses of the sends so far.
//!
//! The zustand store is a module-level object; here it is one [`RepeaterStore`] the host
//! puts in an entity. The two awaits of the TypeScript are the caller's:
//! `loadFromTraffic` gets the stored request it would have fetched over IPC, and `send`
//! is split into [`RepeaterStore::begin_send`] and [`RepeaterStore::finish_send`] around
//! the caller's own `repeater:send-raw`.

use std::fmt;

use rever_core::traffic::StoredRequest;
use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const MAX_HISTORY: usize = 20;

/// A key JavaScript treats as an array index: canonical decimal, below 2^32 - 1.
fn array_index(key: &str) -> Option<u32> {
    let canonical = key == "0" || (!key.starts_with('0') && !key.is_empty());
    if !canonical || !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|index| *index < u32::MAX)
}

/// A `Record<string, string>` as JavaScript keeps it: one value per key, listed in the
/// order `Object.entries` gives (index-like keys first, ascending; then the rest in the
/// order they were first set). The header table shows and edits rows by that order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    pub fn new() -> Self {
        Self::default()
    }

    /// `Object.entries(headers)`.
    pub fn entries(&self) -> &[(String, String)] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `headers[key]`; the name is matched as written, case included.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// `headers[key] = value`: an existing key keeps its place.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let (key, value) = (key.into(), value.into());
        if let Some(entry) = self.0.iter_mut().find(|(name, _)| *name == key) {
            entry.1 = value;
            return;
        }
        let at = match array_index(&key) {
            Some(index) => self
                .0
                .iter()
                .position(|(name, _)| array_index(name).is_none_or(|other| other > index))
                .unwrap_or(self.0.len()),
            None => self.0.len(),
        };
        self.0.insert(at, (key, value));
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for Headers {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut headers = Self::new();
        for (key, value) in iter {
            headers.set(key, value);
        }
        headers
    }
}

impl Serialize for Headers {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Headers {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct HeadersVisitor;

        impl<'de> Visitor<'de> for HeadersVisitor {
            type Value = Headers;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("an object of header names and string values")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Headers, A::Error> {
                let mut headers = Headers::new();
                while let Some((key, value)) = map.next_entry::<String, String>()? {
                    headers.set(key, value);
                }
                Ok(headers)
            }
        }

        deserializer.deserialize_map(HeadersVisitor)
    }
}

/// The argument of `repeater:send-raw`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepeaterRequestSpec {
    pub url: String,
    pub method: String,
    pub headers: Headers,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// The answer of `repeater:send-raw`. A send that never got a response has status 0
/// and says why in `error`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RepeaterResponse {
    pub status: u32,
    pub status_text: String,
    pub headers: Headers,
    pub body: String,
    pub body_truncated: bool,
    pub body_byte_length: u64,
    pub time_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub request: RepeaterRequestSpec,
    pub response: RepeaterResponse,
    /// Epoch milliseconds of the moment the response came back.
    pub ts: u64,
}

#[derive(Debug, Default)]
pub struct RepeaterStore {
    active: Option<RepeaterRequestSpec>,
    source_request_id: Option<String>,
    source_label: Option<String>,
    // Bumped on every load_from_traffic so the UI can reliably switch to the
    // Repeater panel even when the SAME request is re-sent (source_request_id
    // would be unchanged and would not trigger a reaction on its own).
    load_nonce: u64,
    history: Vec<HistoryEntry>,
    loading: bool,
    error: Option<String>,
}

impl RepeaterStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active(&self) -> Option<&RepeaterRequestSpec> {
        self.active.as_ref()
    }

    pub fn source_request_id(&self) -> Option<&str> {
        self.source_request_id.as_deref()
    }

    pub fn source_label(&self) -> Option<&str> {
        self.source_label.as_deref()
    }

    pub fn load_nonce(&self) -> u64 {
        self.load_nonce
    }

    /// Newest first.
    pub fn history(&self) -> &[HistoryEntry] {
        &self.history
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// `stored` is what `window.rev.traffic.get(requestId)` answered.
    pub fn load_from_traffic(&mut self, request_id: &str, stored: Option<&StoredRequest>) {
        let Some(stored) = stored else {
            self.error = Some("request not found".to_owned());
            return;
        };
        self.active = Some(RepeaterRequestSpec {
            url: stored.url.clone(),
            method: if stored.method.is_empty() {
                "GET".to_owned()
            } else {
                stored.method.clone()
            },
            headers: stored.request_headers.iter().flatten().collect(),
            body: stored.request_post_data.clone(),
        });
        self.source_request_id = Some(request_id.to_owned());
        self.source_label = Some(format!("{} {}", stored.method, stored.url));
        self.load_nonce += 1;
        self.history.clear();
        self.error = None;
    }

    pub fn set_active(&mut self, request: RepeaterRequestSpec) {
        self.active = Some(request);
    }

    /// The first half of `send`: the request to hand to `repeater:send-raw`, or nothing
    /// when no request is loaded. Pass the same request to [`Self::finish_send`], so the
    /// history shows what was sent even if the editor moved on meanwhile.
    pub fn begin_send(&mut self) -> Option<RepeaterRequestSpec> {
        let active = self.active.clone()?;
        self.loading = true;
        self.error = None;
        Some(active)
    }

    /// The second half of `send`. `Err` is the IPC call itself failing (the `catch`).
    pub fn finish_send(
        &mut self,
        request: RepeaterRequestSpec,
        result: Result<RepeaterResponse, String>,
        now_ms: u64,
    ) {
        self.loading = false;
        match result {
            Ok(response) => {
                self.error = response.error.clone();
                self.history.insert(
                    0,
                    HistoryEntry {
                        request,
                        response,
                        ts: now_ms,
                    },
                );
                self.history.truncate(MAX_HISTORY);
            }
            Err(message) => self.error = Some(message),
        }
    }

    pub fn restore_from_history(&mut self, index: usize) {
        if let Some(entry) = self.history.get(index) {
            self.active = Some(entry.request.clone());
        }
    }

    /// Unloads the request. `loading` and the load counter stay as they are.
    pub fn clear(&mut self) {
        self.active = None;
        self.source_request_id = None;
        self.source_label = None;
        self.history.clear();
        self.error = None;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn stored(method: &str, url: &str) -> StoredRequest {
        StoredRequest {
            request_id: "r1".into(),
            url: url.into(),
            method: method.into(),
            request_headers: Some(
                [("accept", "*/*"), ("x-token", "abc")]
                    .into_iter()
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .collect(),
            ),
            request_post_data: Some("{\"a\":1}".into()),
            ..StoredRequest::default()
        }
    }

    fn response(status: u32) -> RepeaterResponse {
        RepeaterResponse {
            status,
            status_text: "OK".into(),
            body: "hi".into(),
            body_byte_length: 2,
            time_ms: 12,
            ..RepeaterResponse::default()
        }
    }

    fn loaded() -> RepeaterStore {
        let mut store = RepeaterStore::new();
        store.load_from_traffic("r1", Some(&stored("POST", "https://api.test/items")));
        store
    }

    // ── Headers ─────────────────────────────────────────────────────────────

    #[test]
    fn 헤더는_처음_넣은_순서를_지키고_같은_키는_자리를_지킨_채_값만_바뀐다() {
        let mut headers = Headers::new();
        headers.set("b", "1");
        headers.set("a", "2");
        headers.set("b", "3");
        assert_eq!(
            headers.entries(),
            [("b".to_owned(), "3".to_owned()), ("a".into(), "2".into())]
        );
        assert_eq!(headers.get("a"), Some("2"));
        // 이름은 쓴 그대로 비교한다.
        assert_eq!(headers.get("A"), None);
        assert!(!headers.contains("A"));
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn 숫자로만_된_키는_자바스크립트처럼_오름차순으로_맨_앞에_온다() {
        let headers: Headers = [("x", ""), ("10", ""), ("2", ""), ("y", ""), ("01", "")]
            .into_iter()
            .collect();
        let keys: Vec<&str> = headers.entries().iter().map(|(k, _)| k.as_str()).collect();
        // "01" 은 정규형 정수가 아니라서 문자열 키다.
        assert_eq!(keys, ["2", "10", "x", "y", "01"]);
        assert_eq!(array_index("4294967294"), Some(4_294_967_294));
        // 2^32 - 1 부터는 배열 인덱스가 아니다.
        assert_eq!(array_index("4294967295"), None);
        assert_eq!(array_index(""), None);
        assert_eq!(array_index("-1"), None);
        assert_eq!(array_index("1.5"), None);
    }

    #[test]
    fn 헤더는_json_객체로_오가며_순서를_지킨다() {
        let headers: Headers = serde_json::from_str(r#"{"b":"1","a":"2","b":"3"}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&headers).unwrap(),
            r#"{"b":"3","a":"2"}"#
        );
        // 값이 문자열이 아니면 받아들이지 않는다(패닉은 없다).
        assert!(serde_json::from_str::<Headers>(r#"{"a":1}"#).is_err());
        assert!(serde_json::from_str::<Headers>("[]").is_err());
    }

    // ── IPC shapes ──────────────────────────────────────────────────────────

    #[test]
    fn 요청은_camel_case로_나가고_본문이_없으면_body를_내지_않는다() {
        let spec = RepeaterRequestSpec {
            url: "https://a.test/".into(),
            method: "GET".into(),
            headers: [("accept", "*/*")].into_iter().collect(),
            body: None,
        };
        assert_eq!(
            serde_json::to_value(&spec).unwrap(),
            json!({ "url": "https://a.test/", "method": "GET", "headers": { "accept": "*/*" } })
        );
        let with_body = RepeaterRequestSpec {
            body: Some(String::new()),
            ..spec.clone()
        };
        // 빈 문자열 본문은 undefined 가 아니다.
        assert_eq!(serde_json::to_value(&with_body).unwrap()["body"], json!(""));
        let back: RepeaterRequestSpec =
            serde_json::from_value(serde_json::to_value(&spec).unwrap()).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn 응답은_camel_case_필드로_읽고_빠진_필드와_모르는_필드를_견딘다() {
        let full: RepeaterResponse = serde_json::from_value(json!({
            "status": 201,
            "statusText": "Created",
            "headers": { "content-type": "application/json" },
            "body": "{}",
            "bodyTruncated": true,
            "bodyByteLength": 70000,
            "timeMs": 31,
            "extra": 1
        }))
        .unwrap();
        assert_eq!(full.status, 201);
        assert_eq!(full.status_text, "Created");
        assert_eq!(full.headers.get("content-type"), Some("application/json"));
        assert!(full.body_truncated);
        assert_eq!(full.body_byte_length, 70_000);
        assert_eq!(full.time_ms, 31);
        assert_eq!(full.error, None);

        let failed: RepeaterResponse =
            serde_json::from_value(json!({ "status": 0, "error": "timeout" })).unwrap();
        assert_eq!(failed.error.as_deref(), Some("timeout"));
        assert_eq!(failed.body, "");
        // 오류가 없으면 error 필드를 내지 않는다.
        assert!(serde_json::to_value(&full).unwrap().get("error").is_none());
        assert_eq!(
            serde_json::to_value(&full).unwrap()["bodyByteLength"],
            json!(70000)
        );
    }

    #[test]
    fn 기록_항목은_request_response_ts로_직렬화된다() {
        let entry = HistoryEntry {
            request: RepeaterRequestSpec::default(),
            response: response(200),
            ts: 1_700_000_000_000,
        };
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["ts"], json!(1_700_000_000_000u64));
        assert_eq!(value["response"]["timeMs"], json!(12));
        assert_eq!(value["request"]["method"], json!(""));
    }

    // ── the store ───────────────────────────────────────────────────────────

    #[test]
    fn 처음에는_불러온_요청도_기록도_없다() {
        let store = RepeaterStore::new();
        assert_eq!(store.active(), None);
        assert_eq!(store.source_request_id(), None);
        assert_eq!(store.source_label(), None);
        assert_eq!(store.load_nonce(), 0);
        assert!(store.history().is_empty());
        assert!(!store.loading());
        assert_eq!(store.error(), None);
    }

    #[test]
    fn 트래픽에서_불러오면_요청과_출처가_채워진다() {
        let store = loaded();
        let active = store.active().expect("a request is loaded");
        assert_eq!(active.url, "https://api.test/items");
        assert_eq!(active.method, "POST");
        assert_eq!(active.headers.get("x-token"), Some("abc"));
        assert_eq!(active.body.as_deref(), Some("{\"a\":1}"));
        assert_eq!(store.source_request_id(), Some("r1"));
        assert_eq!(store.source_label(), Some("POST https://api.test/items"));
        assert_eq!(store.load_nonce(), 1);
    }

    #[test]
    fn 메서드가_비어_있으면_get으로_채우되_출처_라벨은_받은_그대로다() {
        let mut store = RepeaterStore::new();
        let mut request = stored("", "https://a.test/");
        request.request_headers = None;
        request.request_post_data = None;
        store.load_from_traffic("r9", Some(&request));
        let active = store.active().expect("a request is loaded");
        assert_eq!(active.method, "GET");
        assert!(active.headers.is_empty());
        assert_eq!(active.body, None);
        assert_eq!(store.source_label(), Some(" https://a.test/"));
    }

    #[test]
    fn 없는_요청이면_오류만_남기고_나머지는_그대로_둔다() {
        let mut store = loaded();
        store.load_from_traffic("gone", None);
        assert_eq!(store.error(), Some("request not found"));
        assert_eq!(store.source_request_id(), Some("r1"));
        assert_eq!(store.load_nonce(), 1);
        assert!(store.active().is_some());
    }

    #[test]
    fn 같은_요청을_다시_불러와도_nonce가_오르고_기록과_오류가_비워진다() {
        let mut store = loaded();
        let request = store.begin_send().unwrap();
        store.finish_send(request, Err("boom".into()), 1);
        let request = store.begin_send().unwrap();
        store.finish_send(request, Ok(response(200)), 2);
        store.load_from_traffic("gone", None);
        assert_eq!(store.history().len(), 1);
        assert!(store.error().is_some());

        store.load_from_traffic("r1", Some(&stored("POST", "https://api.test/items")));
        assert_eq!(store.load_nonce(), 2);
        assert!(store.history().is_empty());
        assert_eq!(store.error(), None);
    }

    #[test]
    fn 불러온_요청이_없으면_보내기는_아무것도_하지_않는다() {
        let mut store = RepeaterStore::new();
        assert_eq!(store.begin_send(), None);
        assert!(!store.loading());
    }

    #[test]
    fn 보내기를_시작하면_loading이_켜지고_이전_오류가_지워진다() {
        let mut store = loaded();
        let request = store.begin_send().unwrap();
        store.finish_send(request, Err("boom".into()), 1);
        assert_eq!(store.error(), Some("boom"));
        assert!(!store.loading());

        let request = store.begin_send().expect("a request is loaded");
        assert_eq!(request.url, "https://api.test/items");
        assert!(store.loading());
        assert_eq!(store.error(), None);
    }

    #[test]
    fn 응답이_오면_기록_맨_앞에_쌓인다() {
        let mut store = loaded();
        let request = store.begin_send().unwrap();
        store.finish_send(request, Ok(response(200)), 100);
        let request = store.begin_send().unwrap();
        store.finish_send(request, Ok(response(404)), 200);
        let statuses: Vec<u32> = store.history().iter().map(|h| h.response.status).collect();
        assert_eq!(statuses, [404, 200]);
        assert_eq!(store.history()[0].ts, 200);
        assert!(!store.loading());
        assert_eq!(store.error(), None);
    }

    #[test]
    fn 기록은_최근_20개까지만_남는다() {
        let mut store = loaded();
        for n in 0..25u64 {
            let request = store.begin_send().unwrap();
            store.finish_send(request, Ok(response(200)), n);
        }
        assert_eq!(store.history().len(), MAX_HISTORY);
        assert_eq!(store.history()[0].ts, 24);
        assert_eq!(store.history()[19].ts, 5);
    }

    #[test]
    fn 기록에는_보낼_때의_요청이_남고_그_뒤의_편집은_섞이지_않는다() {
        let mut store = loaded();
        let request = store.begin_send().unwrap();
        // 응답을 기다리는 동안 편집기가 바뀐다.
        let mut edited = request.clone();
        edited.url = "https://api.test/other".into();
        edited.headers.set("x-token", "changed");
        store.set_active(edited);
        store.finish_send(request, Ok(response(200)), 1);
        let sent = &store.history()[0].request;
        assert_eq!(sent.url, "https://api.test/items");
        assert_eq!(sent.headers.get("x-token"), Some("abc"));
        assert_eq!(store.active().unwrap().url, "https://api.test/other");
    }

    #[test]
    fn 응답에_error가_있으면_기록에_남기면서_오류로도_올린다() {
        let mut store = loaded();
        let request = store.begin_send().unwrap();
        let failed = RepeaterResponse {
            error: Some("net::ERR_FAILED".into()),
            ..RepeaterResponse::default()
        };
        store.finish_send(request, Ok(failed), 1);
        assert_eq!(store.error(), Some("net::ERR_FAILED"));
        assert_eq!(store.history().len(), 1);
        assert_eq!(store.history()[0].response.status, 0);
    }

    #[test]
    fn 전송_호출이_실패하면_기록_없이_오류만_남긴다() {
        let mut store = loaded();
        let request = store.begin_send().unwrap();
        store.finish_send(request, Err("ipc closed".into()), 1);
        assert!(store.history().is_empty());
        assert_eq!(store.error(), Some("ipc closed"));
        assert!(!store.loading());
    }

    #[test]
    fn 기록에서_복원하면_그때_보낸_요청이_편집_대상이_된다() {
        let mut store = loaded();
        let request = store.begin_send().unwrap();
        store.finish_send(request, Ok(response(200)), 1);
        let mut edited = store.active().unwrap().clone();
        edited.method = "DELETE".into();
        store.set_active(edited);
        let request = store.begin_send().unwrap();
        store.finish_send(request, Ok(response(204)), 2);

        store.restore_from_history(1);
        assert_eq!(store.active().unwrap().method, "POST");
        // 복원은 기록을 건드리지 않는다.
        assert_eq!(store.history().len(), 2);
        // 범위 밖 인덱스는 무시한다.
        store.restore_from_history(7);
        assert_eq!(store.active().unwrap().method, "POST");
    }

    #[test]
    fn clear는_요청과_기록을_비우되_loading과_nonce는_남긴다() {
        let mut store = loaded();
        let request = store.begin_send().unwrap();
        store.finish_send(request, Ok(response(500)), 1);
        store.begin_send();
        store.clear();
        assert_eq!(store.active(), None);
        assert_eq!(store.source_request_id(), None);
        assert_eq!(store.source_label(), None);
        assert!(store.history().is_empty());
        assert_eq!(store.error(), None);
        assert!(store.loading());
        assert_eq!(store.load_nonce(), 1);
    }
}
