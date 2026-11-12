//! The captured-traffic ring buffer. Port of `src/main/traffic-store.ts`: same
//! limits, same merge-on-upsert and newest-first listing, so tools built on it
//! return what the Electron app returned.
//!
//! Not ported yet: WebSocket frames, console/exception logs, HAR imports.

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde::Serialize;

pub const MAX_ENTRIES: usize = 500;
/// Per-body ceiling. 8MB sits above webcrack's 5MB deobfuscation limit, so large
/// JS bundles survive intact; longer bodies are truncated and flagged.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Budget for all bodies together; past it the oldest bodies are dropped (metadata stays).
pub const MAX_TOTAL_BODY_BYTES: usize = 256 * 1024 * 1024;

pub type Headers = BTreeMap<String, String>;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredRequest {
    pub request_id: String,
    pub url: String,
    pub host: String,
    pub method: String,
    pub resource_type: String,
    /// Epoch milliseconds.
    pub started_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoded_data_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<Headers>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_post_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<Headers>,
    /// Text as-is, or base64 when `response_body_base64` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body_base64: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body_truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<u64>,
}

/// A partial update keyed by request id: `None` fields leave the stored value alone.
#[derive(Clone, Debug, Default)]
pub struct RequestPatch {
    pub request_id: String,
    pub url: Option<String>,
    pub method: Option<String>,
    pub resource_type: Option<String>,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
    pub status: Option<u16>,
    pub mime_type: Option<String>,
    pub encoded_data_length: Option<u64>,
    pub request_headers: Option<Headers>,
    pub request_post_data: Option<String>,
    pub response_headers: Option<Headers>,
    pub response_body: Option<String>,
    pub response_body_base64: Option<bool>,
    pub tab_id: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct ListFilter {
    /// Substring of the host.
    pub host: Option<String>,
    pub tab_id: Option<u64>,
    /// Case-insensitive substring of the method or the resource type.
    pub method_or_type: Option<String>,
    pub since: Option<u64>,
    /// Paging cursor: the boundary entry's `started_at`, and with `before_id` its
    /// request id. The boundary entry itself is never returned again, so every
    /// page makes progress. Without `before_id` the boundary millisecond is inclusive.
    pub before: Option<u64>,
    pub before_id: Option<String>,
    /// Default 50.
    pub limit: Option<usize>,
}

#[derive(Default)]
pub struct TrafficStore {
    order: VecDeque<String>,
    entries: HashMap<String, StoredRequest>,
    total_body_bytes: usize,
}

impl TrafficStore {
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn get(&self, request_id: &str) -> Option<&StoredRequest> {
        self.entries.get(request_id)
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Insert a new request or merge into the one with the same id.
    pub fn upsert(&mut self, mut patch: RequestPatch, now_ms: u64) {
        let mut truncated = false;
        if let Some(body) = &mut patch.response_body {
            if body.len() > MAX_BODY_BYTES {
                body.truncate(floor_char_boundary(body, MAX_BODY_BYTES));
                truncated = true;
            }
        }
        if !self.entries.contains_key(&patch.request_id) {
            self.order.push_back(patch.request_id.clone());
        }
        let entry = self
            .entries
            .entry(patch.request_id.clone())
            .or_insert_with(|| StoredRequest {
                request_id: patch.request_id.clone(),
                resource_type: "Other".into(),
                started_at: now_ms,
                ..StoredRequest::default()
            });
        if let Some(url) = patch.url {
            entry.host = host_of(&url);
            entry.url = url;
        }
        if let Some(body) = patch.response_body {
            let old = entry.response_body.as_ref().map_or(0, String::len);
            self.total_body_bytes = self.total_body_bytes.saturating_sub(old) + body.len();
            entry.response_body = Some(body);
            entry.response_body_truncated = truncated.then_some(true);
        }
        macro_rules! merge {
            ($($field:ident),*) => { $( if let Some(value) = patch.$field { entry.$field = value.into(); } )* };
        }
        merge!(
            method,
            resource_type,
            started_at,
            completed_at,
            status,
            mime_type,
            encoded_data_length,
            request_headers,
            request_post_data,
            response_headers,
            response_body_base64,
            tab_id
        );
        self.evict();
    }

    fn evict(&mut self) {
        while self.order.len() > MAX_ENTRIES {
            if let Some(entry) = self
                .order
                .pop_front()
                .and_then(|id| self.entries.remove(&id))
            {
                let freed = entry.response_body.map_or(0, |body| body.len());
                self.total_body_bytes = self.total_body_bytes.saturating_sub(freed);
            }
        }
        if self.total_body_bytes <= MAX_TOTAL_BODY_BYTES {
            return;
        }
        for id in &self.order {
            if self.total_body_bytes <= MAX_TOTAL_BODY_BYTES {
                break;
            }
            let Some(entry) = self.entries.get_mut(id) else {
                continue;
            };
            if let Some(body) = entry.response_body.take() {
                self.total_body_bytes = self.total_body_bytes.saturating_sub(body.len());
                entry.response_body_truncated = Some(true);
            }
        }
    }

    /// Newest first.
    pub fn list(&self, filter: &ListFilter) -> Vec<&StoredRequest> {
        let limit = filter.limit.unwrap_or(50);
        let needle = filter
            .method_or_type
            .as_ref()
            .map(|needle| needle.to_lowercase());
        // Position of the boundary entry: tells older same-millisecond siblings from the boundary.
        let boundary = filter
            .before_id
            .as_ref()
            .and_then(|id| self.order.iter().position(|o| o == id));
        let mut result = Vec::new();
        for (index, id) in self.order.iter().enumerate().rev() {
            if result.len() >= limit {
                break;
            }
            let Some(entry) = self.entries.get(id) else {
                continue;
            };
            if filter
                .host
                .as_ref()
                .is_some_and(|host| !entry.host.contains(host.as_str()))
            {
                continue;
            }
            if filter.tab_id.is_some() && entry.tab_id != filter.tab_id {
                continue;
            }
            if let Some(needle) = &needle {
                if !entry.method.to_lowercase().contains(needle)
                    && !entry.resource_type.to_lowercase().contains(needle)
                {
                    continue;
                }
            }
            if filter.since.is_some_and(|since| entry.started_at < since) {
                continue;
            }
            if let Some(before) = filter.before {
                if entry.started_at > before {
                    continue;
                }
                if entry.started_at == before {
                    let boundary_or_newer = match boundary {
                        Some(boundary) => index >= boundary,
                        // The boundary was evicted: only it can be excluded by id.
                        None => filter.before_id.as_deref() == Some(entry.request_id.as_str()),
                    };
                    if boundary_or_newer {
                        continue;
                    }
                }
            }
            result.push(entry);
        }
        result
    }
}

/// Servo's fetch destination (its Debug name) → the CDP `Network.ResourceType`
/// names the rest of rever filters on.
pub fn resource_type_of(destination: &str) -> &'static str {
    match destination {
        "Document" | "IFrame" | "Frame" => "Document",
        "Style" => "Stylesheet",
        "Script" | "Worker" | "SharedWorker" | "ServiceWorker" => "Script",
        "Image" => "Image",
        "Font" => "Font",
        "Audio" | "Video" | "Track" => "Media",
        "Manifest" => "Manifest",
        // XHR and fetch() have no destination.
        "None" => "XHR",
        _ => "Other",
    }
}

/// The media type of a `Content-Type` header value, lowercased and without parameters.
pub fn mime_of(headers: &Headers) -> String {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default()
}

/// How a response body is stored: text bodies as-is, everything else base64.
/// Returns the stored string and whether it is base64.
pub fn encode_body(body: &[u8], mime: &str) -> (String, bool) {
    use base64::Engine as _;
    let textual = mime.starts_with("text/")
        || ["json", "javascript", "xml", "x-www-form-urlencoded"]
            .iter()
            .any(|kind| mime.contains(kind));
    match std::str::from_utf8(body) {
        Ok(text) if textual => (text.to_owned(), false),
        _ => (base64::engine::general_purpose::STANDARD.encode(body), true),
    }
}

/// `host[:port]` of a URL, or empty when it has none.
pub fn host_of(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return String::new();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    authority.rsplit('@').next().unwrap_or_default().to_owned()
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str, url: &str, method: &str) -> RequestPatch {
        RequestPatch {
            request_id: id.into(),
            url: Some(url.into()),
            method: Some(method.into()),
            ..RequestPatch::default()
        }
    }

    #[test]
    fn 새_요청은_기본값으로_저장되고_최신순으로_나온다() {
        let mut store = TrafficStore::default();
        store.upsert(request("a", "https://a.test:8443/x", "GET"), 100);
        store.upsert(request("b", "https://b.test/y", "POST"), 200);

        let listed: Vec<&str> = store
            .list(&ListFilter::default())
            .iter()
            .map(|r| r.request_id.as_str())
            .collect();
        assert_eq!(listed, ["b", "a"]);
        let a = store.get("a").unwrap();
        assert_eq!(
            (a.host.as_str(), a.resource_type.as_str(), a.started_at),
            ("a.test:8443", "Other", 100)
        );
    }

    #[test]
    fn 같은_id로_다시_넣으면_필드를_병합한다() {
        let mut store = TrafficStore::default();
        store.upsert(request("a", "https://a.test/login", "POST"), 100);
        store.upsert(
            RequestPatch {
                request_id: "a".into(),
                status: Some(200),
                response_body: Some("{\"ok\":true}".into()),
                ..RequestPatch::default()
            },
            999,
        );

        let a = store.get("a").unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(
            (a.method.as_str(), a.status, a.started_at),
            ("POST", Some(200), 100)
        );
        assert_eq!(a.response_body.as_deref(), Some("{\"ok\":true}"));
    }

    #[test]
    fn 항목이_상한을_넘으면_가장_오래된_것부터_버린다() {
        let mut store = TrafficStore::default();
        for i in 0..MAX_ENTRIES + 3 {
            store.upsert(
                request(&format!("r{i}"), "https://a.test/", "GET"),
                i as u64,
            );
        }
        assert_eq!(store.len(), MAX_ENTRIES);
        assert!(store.get("r2").is_none());
        assert!(store.get("r3").is_some());
    }

    #[test]
    fn 본문이_상한을_넘으면_잘라내고_표시한다() {
        let mut store = TrafficStore::default();
        // 한글은 3바이트라 상한이 글자 중간에 걸린다 — 글자 경계에서 잘려야 한다.
        let body = "가".repeat(MAX_BODY_BYTES / 3 + 10);
        store.upsert(
            RequestPatch {
                request_id: "big".into(),
                response_body: Some(body),
                ..RequestPatch::default()
            },
            1,
        );
        let big = store.get("big").unwrap();
        let stored = big.response_body.as_ref().unwrap();
        assert!(stored.len() <= MAX_BODY_BYTES && stored.len() > MAX_BODY_BYTES - 3);
        assert_eq!(big.response_body_truncated, Some(true));
    }

    #[test]
    fn 호스트_메서드_since_필터가_적용된다() {
        let mut store = TrafficStore::default();
        store.upsert(request("a", "https://api.a.test/1", "GET"), 100);
        store.upsert(request("b", "https://api.a.test/2", "POST"), 200);
        store.upsert(request("c", "https://cdn.b.test/3", "GET"), 300);
        let ids = |filter: ListFilter| -> Vec<String> {
            store
                .list(&filter)
                .iter()
                .map(|r| r.request_id.clone())
                .collect()
        };

        assert_eq!(
            ids(ListFilter {
                host: Some("a.test".into()),
                ..Default::default()
            }),
            ["b", "a"]
        );
        assert_eq!(
            ids(ListFilter {
                method_or_type: Some("post".into()),
                ..Default::default()
            }),
            ["b"]
        );
        assert_eq!(
            ids(ListFilter {
                since: Some(200),
                ..Default::default()
            }),
            ["c", "b"]
        );
        assert_eq!(
            ids(ListFilter {
                limit: Some(1),
                ..Default::default()
            }),
            ["c"]
        );
    }

    #[test]
    fn before_커서는_경계_항목을_다시_포함하지_않는다() {
        let mut store = TrafficStore::default();
        // 같은 밀리초에 세 건: 경계(b)보다 먼저 들어온 a만 다음 페이지에 나와야 한다.
        for id in ["a", "b", "c"] {
            store.upsert(request(id, "https://a.test/", "GET"), 500);
        }
        let page = store.list(&ListFilter {
            before: Some(500),
            before_id: Some("b".into()),
            ..Default::default()
        });
        assert_eq!(
            page.iter()
                .map(|r| r.request_id.as_str())
                .collect::<Vec<_>>(),
            ["a"]
        );
    }

    #[test]
    fn 텍스트_본문은_그대로_나머지는_base64로_저장한다() {
        assert_eq!(
            encode_body(b"{\"n\":1}", "application/json"),
            ("{\"n\":1}".into(), false)
        );
        assert_eq!(
            encode_body(&[0x89, 0x50], "image/png"),
            ("iVA=".into(), true)
        );
        // 텍스트 mime이어도 UTF-8이 아니면 깨뜨리지 않고 base64로 둔다.
        assert_eq!(
            encode_body(&[0xff, 0xfe], "text/plain"),
            ("//4=".into(), true)
        );
        let headers = Headers::from([("Content-Type".into(), "Text/HTML; charset=utf-8".into())]);
        assert_eq!(mime_of(&headers), "text/html");
    }

    #[test]
    fn 서보의_요청_종류를_cdp_리소스_타입으로_바꾼다() {
        assert_eq!(resource_type_of("None"), "XHR");
        assert_eq!(resource_type_of("Style"), "Stylesheet");
        assert_eq!(resource_type_of("Document"), "Document");
        assert_eq!(resource_type_of("SomethingNew"), "Other");
    }

    #[test]
    fn url에서_호스트를_뽑는다() {
        assert_eq!(host_of("https://user:pw@a.test:8443/p?q#f"), "a.test:8443");
        assert_eq!(host_of("data:text/plain,hi"), "");
    }
}
