//! The captured-traffic store. Port of `src/main/traffic-store.ts`: same
//! limits, same merge-on-upsert and newest-first listing, so tools built on it
//! return what the Electron app returned.
//!
//! The TypeScript keeps module-level state; here it is one `TrafficStore`, and
//! "now" is passed in. Body sizes are counted in UTF-16 code units, as
//! JavaScript's `string.length` does, so the limits cut where the TypeScript cuts.

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde::Serialize;
use serde_json::Value;

pub const MAX_ENTRIES: usize = 500;
/// Per-body ceiling. 8MB sits above webcrack's 5MB deobfuscation limit, so large
/// JS bundles survive intact; longer bodies are truncated and flagged.
pub const MAX_BODY_CHARS: usize = 8 * 1024 * 1024;
/// Budget for all bodies together; past it the oldest bodies are dropped (metadata stays).
pub const MAX_TOTAL_BODY_BYTES: usize = 256 * 1024 * 1024;
/// Cap frames per request so a single long-lived socket can't grow unbounded
/// even while its request stays inside the ring buffer.
pub const MAX_WS_FRAMES_PER_REQUEST: usize = 2000;
pub const MAX_CONSOLE: usize = 1000;
pub const MAX_EXCEPTIONS: usize = 200;
pub const MAX_IMPORTED: usize = 5_000;
pub const MAX_PENDING_EXTRA_RESPONSE_HEADERS: usize = 1000;

pub type Headers = BTreeMap<String, String>;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallFrame {
    pub function_name: String,
    pub url: String,
    pub line_number: i64,
    pub column_number: i64,
}

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
    pub response_body_error: Option<String>,
    /// The tab that sent it; the store is shared by every tab.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initiator_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initiator_stack: Option<Vec<CallFrame>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initiator_url: Option<String>,
}

/// A partial update keyed by request id: `None` fields leave the stored value alone.
#[derive(Clone, Debug, Default)]
pub struct RequestPatch {
    pub request_id: String,
    pub url: Option<String>,
    /// Derived from `url` when left out.
    pub host: Option<String>,
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
    pub response_body_truncated: Option<bool>,
    pub response_body_error: Option<String>,
    pub tab_id: Option<u64>,
    pub initiator_type: Option<String>,
    pub initiator_stack: Option<Vec<CallFrame>>,
    pub initiator_url: Option<String>,
}

/// Which store `list` reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Source {
    /// Captured in this app.
    #[default]
    Live,
    /// Loaded by import_har.
    Import,
    All,
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
    /// page makes progress (an inclusive cursor loops forever once a single entry
    /// fills a page on its own). Without `before_id` the boundary millisecond is
    /// inclusive: HAR entries carry no request ids, so a caller paging by
    /// timestamp alone can't name the boundary and would lose same-ms siblings.
    ///
    /// The upper bound exists because har_export ships its whole response as one
    /// SSE event and MCP clients cap the event size (httpx2: 1MB) — paging large
    /// traffic sets in safe-sized chunks is something `since` alone can't express.
    pub before: Option<u64>,
    pub before_id: Option<String>,
    /// Default 50.
    pub limit: Option<usize>,
    pub source: Source,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WsDirection {
    Sent,
    Received,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WsFrame {
    pub direction: WsDirection,
    pub opcode: u8,
    pub payload_data: String,
    /// Epoch milliseconds.
    pub timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleEntry {
    pub ts: u64,
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace: Option<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeException {
    pub ts: u64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace: Option<Value>,
}

struct PendingExtraHeaders {
    request_id: String,
    status_code: u16,
    headers: Headers,
}

#[derive(Default)]
pub struct TrafficStore {
    order: VecDeque<String>,
    entries: HashMap<String, StoredRequest>,
    // In UTF-16 code units, like the per-body cap.
    total_body_bytes: usize,
    ws_frames: HashMap<String, VecDeque<WsFrame>>,
    console_logs: VecDeque<ConsoleEntry>,
    runtime_exceptions: VecDeque<RuntimeException>,
    // Imported captures are kept apart from the live ring buffer so loading a
    // large HAR can't push out what the user is capturing right now. Ids are
    // `har:<importId>:<index>`.
    import_order: VecDeque<String>,
    imported: HashMap<String, StoredRequest>,
    // Oldest first. shortcut: lookups scan at most 1000 entries; switch to an
    // ordered map if the cap grows.
    pending_extra_response_headers: VecDeque<PendingExtraHeaders>,
}

impl TrafficStore {
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn get(&self, request_id: &str) -> Option<&StoredRequest> {
        self.entries
            .get(request_id)
            .or_else(|| self.imported.get(request_id))
    }

    /// Drops the live capture; console logs, exceptions and imports stay.
    pub fn clear(&mut self) {
        self.order.clear();
        self.entries.clear();
        self.ws_frames.clear();
        self.pending_extra_response_headers.clear();
        self.total_body_bytes = 0;
    }

    /// Insert a new request or merge into the one with the same id.
    pub fn upsert(&mut self, mut patch: RequestPatch, now_ms: u64) {
        if patch.response_body.as_mut().is_some_and(cap_body) {
            patch.response_body_truncated = Some(true);
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
            let old = entry.response_body.as_deref().map_or(0, utf16_len);
            self.total_body_bytes = self.total_body_bytes.saturating_sub(old) + utf16_len(&body);
            entry.response_body = Some(body);
        }
        macro_rules! merge {
            ($($field:ident),*) => { $( if let Some(value) = patch.$field { entry.$field = value.into(); } )* };
        }
        merge!(
            host,
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
            response_body_truncated,
            response_body_error,
            tab_id,
            initiator_type,
            initiator_stack,
            initiator_url
        );
        self.evict();
    }

    fn evict(&mut self) {
        while self.order.len() > MAX_ENTRIES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                let freed = entry.response_body.as_deref().map_or(0, utf16_len);
                self.total_body_bytes = self.total_body_bytes.saturating_sub(freed);
            }
            // Free any WebSocket frames keyed by this request id — otherwise the
            // frame map grows forever as requests churn through the ring buffer.
            self.ws_frames.remove(&oldest);
        }
        if self.total_body_bytes <= MAX_TOTAL_BODY_BYTES {
            return;
        }
        // Over the global budget: empty the oldest bodies first. The entry itself
        // stays so its metadata (URL, status, ...) is still reachable.
        for id in &self.order {
            if self.total_body_bytes <= MAX_TOTAL_BODY_BYTES {
                break;
            }
            let Some(entry) = self.entries.get_mut(id) else {
                continue;
            };
            if let Some(body) = entry.response_body.take_if(|body| !body.is_empty()) {
                self.total_body_bytes = self.total_body_bytes.saturating_sub(utf16_len(&body));
                entry.response_body_truncated = Some(true);
            }
        }
    }

    /// Newest first.
    pub fn list(&self, filter: &ListFilter) -> Vec<&StoredRequest> {
        let limit = filter.limit.unwrap_or(50);
        let live = || scan(&self.order, &self.entries, filter, limit);
        let import = || scan(&self.import_order, &self.imported, filter, limit);
        match filter.source {
            Source::Live => live(),
            Source::Import => import(),
            Source::All => {
                let mut all = live();
                all.extend(import());
                all.sort_by(|a, b| b.started_at.cmp(&a.started_at));
                all.truncate(limit);
                all
            }
        }
    }

    // ── WebSocket frames ────────────────────────────────────────────────────

    pub fn append_ws_frame(&mut self, request_id: &str, frame: WsFrame) {
        let frames = self.ws_frames.entry(request_id.to_owned()).or_default();
        frames.push_back(frame);
        if frames.len() > MAX_WS_FRAMES_PER_REQUEST {
            frames.pop_front();
        }
    }

    /// Oldest first; with `since`, only frames at or after that timestamp.
    pub fn ws_frames(&self, request_id: &str, since: Option<u64>) -> Vec<&WsFrame> {
        self.ws_frames
            .get(request_id)
            .into_iter()
            .flatten()
            .filter(|frame| since.is_none_or(|since| frame.timestamp >= since))
            .collect()
    }

    pub fn web_socket_count(&self) -> usize {
        self.order
            .iter()
            .filter(|id| {
                self.entries
                    .get(*id)
                    .is_some_and(|entry| entry.resource_type == "WebSocket")
            })
            .count()
    }

    // ── Console logs ────────────────────────────────────────────────────────

    pub fn append_console(&mut self, entry: ConsoleEntry) {
        self.console_logs.push_back(entry);
        if self.console_logs.len() > MAX_CONSOLE {
            self.console_logs.pop_front();
        }
    }

    pub fn console_since(&self, since: Option<u64>) -> Vec<&ConsoleEntry> {
        self.console_logs
            .iter()
            .filter(|entry| since.is_none_or(|since| entry.ts >= since))
            .collect()
    }

    pub fn console_count(&self) -> usize {
        self.console_logs.len()
    }

    pub fn clear_console(&mut self) {
        self.console_logs.clear();
    }

    // ── Runtime exceptions ──────────────────────────────────────────────────

    pub fn append_exception(&mut self, entry: RuntimeException) {
        self.runtime_exceptions.push_back(entry);
        if self.runtime_exceptions.len() > MAX_EXCEPTIONS {
            self.runtime_exceptions.pop_front();
        }
    }

    pub fn exceptions(&self) -> Vec<&RuntimeException> {
        self.runtime_exceptions.iter().collect()
    }

    pub fn exception_count(&self) -> usize {
        self.runtime_exceptions.len()
    }

    // ── Imported captures (import_har) ──────────────────────────────────────

    pub fn import_requests(&mut self, requests: Vec<StoredRequest>) {
        for mut request in requests {
            if request.response_body.as_mut().is_some_and(cap_body) {
                request.response_body_truncated = Some(true);
            }
            if !self.imported.contains_key(&request.request_id) {
                self.import_order.push_back(request.request_id.clone());
            }
            self.imported.insert(request.request_id.clone(), request);
        }
        while self.import_order.len() > MAX_IMPORTED {
            let Some(oldest) = self.import_order.pop_front() else {
                break;
            };
            self.imported.remove(&oldest);
        }
    }

    pub fn clear_imports(&mut self) {
        self.import_order.clear();
        self.imported.clear();
    }

    pub fn import_count(&self) -> usize {
        self.import_order.len()
    }

    // ── responseReceivedExtraInfo merge ─────────────────────────────────────
    // Network.responseReceived.response.headers omits Set-Cookie (verified against
    // Chrome via CDP: the event's header map carries no Set-Cookie even when the
    // server sets one). Chromium delivers the complete raw header block in
    // Network.responseReceivedExtraInfo instead — the same event Puppeteer uses
    // for rawHeaders. Ordering of the two events is not guaranteed, so whichever
    // arrives first is buffered here and merged when the other lands.
    //
    // Redirect hops share the requestId, so a buffered block may belong to an
    // earlier hop than the response being stored. The block's own statusCode is
    // kept alongside so callers can tell a 302's headers from the final 200's.

    pub fn merge_extra_response_headers(
        &mut self,
        request_id: &str,
        extra_headers: Headers,
        status_code: u16,
    ) {
        // responseReceived already arrived — merge in place. ExtraInfo is the
        // raw authoritative block, so it wins for shared names (Chromium folds
        // repeated Set-Cookie into one '\n'-joined value). A block whose
        // statusCode doesn't match belongs to a different hop of this requestId:
        // if its responseReceived hasn't landed yet (redirect, auth retry) it is
        // re-buffered so that event's statusCode check can pick it up; a truly
        // stale block just expires via the cap.
        if let Some(existing) = self.entries.get_mut(request_id)
            && existing.status.is_none_or(|status| status == status_code)
            && let Some(headers) = &mut existing.response_headers
        {
            headers.extend(extra_headers);
            return;
        }
        let pending = &mut self.pending_extra_response_headers;
        if pending.len() >= MAX_PENDING_EXTRA_RESPONSE_HEADERS {
            pending.pop_front();
        }
        // A block that replaces a buffered one keeps that one's place in line.
        match pending.iter_mut().find(|p| p.request_id == request_id) {
            Some(buffered) => {
                buffered.status_code = status_code;
                buffered.headers = extra_headers;
            }
            None => pending.push_back(PendingExtraHeaders {
                request_id: request_id.to_owned(),
                status_code,
                headers: extra_headers,
            }),
        }
    }

    pub fn take_pending_extra_response_headers(
        &mut self,
        request_id: &str,
        status_code: u16,
    ) -> Option<Headers> {
        let pending = &mut self.pending_extra_response_headers;
        let index = pending.iter().position(|p| p.request_id == request_id)?;
        let extra = pending.remove(index)?;
        // Only hand the block over when it describes the response that just
        // arrived — a redirect hop's block shares the requestId but not the status.
        (extra.status_code == status_code).then_some(extra.headers)
    }

    pub fn discard_pending_extra_response_headers(&mut self, request_id: &str) {
        self.pending_extra_response_headers
            .retain(|p| p.request_id != request_id);
    }
}

/// JavaScript's `string.length`.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Byte length of the longest prefix that fits in `units` UTF-16 code units:
/// where JavaScript's `slice(0, units)` cuts. A surrogate pair split by the cut
/// is left out whole, since a Rust string can't hold the lone half.
pub(crate) fn utf16_prefix_len(text: &str, units: usize) -> usize {
    let mut seen = 0;
    text.char_indices()
        .find(|(_, c)| {
            seen += c.len_utf16();
            seen > units
        })
        .map_or(text.len(), |(at, _)| at)
}

/// Truncates a body longer than `MAX_BODY_CHARS`; true when it cut.
fn cap_body(body: &mut String) -> bool {
    // UTF-16 never needs more units than UTF-8 needs bytes, so a body this
    // short in bytes is under the cap without counting.
    if body.len() <= MAX_BODY_CHARS {
        return false;
    }
    let keep = utf16_prefix_len(body, MAX_BODY_CHARS);
    let over = keep < body.len();
    body.truncate(keep);
    over
}

fn scan<'a>(
    ids: &VecDeque<String>,
    store: &'a HashMap<String, StoredRequest>,
    filter: &ListFilter,
    limit: usize,
) -> Vec<&'a StoredRequest> {
    let needle = filter
        .method_or_type
        .as_ref()
        .map(|needle| needle.to_lowercase());
    // Position of the boundary entry: tells older same-millisecond siblings from the boundary.
    let boundary = filter
        .before_id
        .as_ref()
        .and_then(|id| ids.iter().position(|o| o == id));
    let mut result = Vec::new();
    for (index, id) in ids.iter().enumerate().rev() {
        if result.len() >= limit {
            break;
        }
        let Some(entry) = store.get(id) else {
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
                // Keep only entries strictly older than the boundary — earlier in
                // `ids` means inserted earlier.
                let boundary_or_newer = match boundary {
                    Some(boundary) => index >= boundary,
                    // The boundary was evicted: siblings can't be ordered against
                    // it, so only it is excluded by id (callers dedupe by request id).
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

    fn body(id: &str, text: String) -> RequestPatch {
        RequestPatch {
            request_id: id.into(),
            response_body: Some(text),
            ..RequestPatch::default()
        }
    }

    // The TypeScript cap is `responseBody.length`, so Hangul (3 bytes, 1 unit)
    // reaches it three times later than a byte count would.
    #[test]
    fn 본문_상한은_utf16_단위로_센다() {
        let mut store = TrafficStore::default();
        store.upsert(body("under", "가".repeat(MAX_BODY_CHARS / 3 + 10)), 1);
        let under = store.get("under").unwrap();
        assert_eq!(
            under.response_body.as_deref().map(utf16_len),
            Some(MAX_BODY_CHARS / 3 + 10)
        );
        assert_eq!(under.response_body_truncated, None);

        store.upsert(body("over", "가".repeat(MAX_BODY_CHARS + 100)), 1);
        let over = store.get("over").unwrap();
        assert_eq!(
            over.response_body.as_deref().map(utf16_len),
            Some(MAX_BODY_CHARS)
        );
        assert_eq!(over.response_body_truncated, Some(true));
    }

    #[test]
    fn 상한이_서로게이트_쌍_중간에_걸리면_글자_경계에서_자른다() {
        let mut store = TrafficStore::default();
        // 1 + 2n units: the cap lands inside a pair. JavaScript keeps the lone
        // half (length = cap); here the whole pair goes.
        let text = format!("a{}", "😀".repeat(MAX_BODY_CHARS / 2 + 1));
        store.upsert(body("pair", text), 1);
        let pair = store.get("pair").unwrap();
        assert_eq!(
            pair.response_body.as_deref().map(utf16_len),
            Some(MAX_BODY_CHARS - 1)
        );
        assert_eq!(pair.response_body_truncated, Some(true));
    }

    #[test]
    fn 전체_본문_예산도_utf16_단위로_센다() {
        let mut store = TrafficStore::default();
        store.upsert(body("a", "가나다".into()), 1);
        assert_eq!(store.total_body_bytes, 3);
        store.upsert(body("a", "😀".into()), 1);
        assert_eq!(store.total_body_bytes, 2);
        store.upsert(body("b", "한글 ok".into()), 1);
        assert_eq!(store.total_body_bytes, 7);
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

    // ── Ported from src/main/traffic-store.test.ts, one test per `it` ────────

    const NOW: u64 = 1_700_000_000_000;

    fn patch(id: &str, url: &str, host: &str) -> RequestPatch {
        RequestPatch {
            request_id: id.into(),
            url: Some(url.into()),
            host: Some(host.into()),
            ..RequestPatch::default()
        }
    }

    fn typed(
        id: &str,
        url: &str,
        host: &str,
        method: &str,
        resource_type: &str,
        started_at: u64,
    ) -> RequestPatch {
        RequestPatch {
            method: Some(method.into()),
            resource_type: Some(resource_type.into()),
            started_at: Some(started_at),
            ..patch(id, url, host)
        }
    }

    /// The `listRequests` suite's `beforeEach`.
    fn seeded() -> TrafficStore {
        let mut store = TrafficStore::default();
        store.upsert(
            typed("r1", "https://a.com/1", "a.com", "GET", "XHR", 100),
            NOW,
        );
        store.upsert(
            typed("r2", "https://b.com/2", "b.com", "POST", "Fetch", 200),
            NOW,
        );
        store.upsert(
            typed("r3", "https://a.com/3", "a.com", "GET", "Script", 300),
            NOW,
        );
        store
    }

    fn ids(store: &TrafficStore, filter: ListFilter) -> Vec<String> {
        store
            .list(&filter)
            .iter()
            .map(|r| r.request_id.clone())
            .collect()
    }

    fn headers<const N: usize>(pairs: [(&str, &str); N]) -> Headers {
        pairs
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect()
    }

    /// `{ ...base, ...pending }` as the CDP callers build it.
    fn spread(mut base: Headers, pending: Option<Headers>) -> Headers {
        base.extend(pending.unwrap_or_default());
        base
    }

    fn response(id: &str, status: u16, response_headers: Headers) -> RequestPatch {
        RequestPatch {
            request_id: id.into(),
            status: Some(status),
            response_headers: Some(response_headers),
            ..RequestPatch::default()
        }
    }

    fn response_header<'a>(store: &'a TrafficStore, id: &str, name: &str) -> Option<&'a str> {
        store
            .get(id)?
            .response_headers
            .as_ref()?
            .get(name)
            .map(String::as_str)
    }

    fn frame(direction: WsDirection, payload: &str, timestamp: u64) -> WsFrame {
        WsFrame {
            direction,
            opcode: 1,
            payload_data: payload.into(),
            timestamp,
            mask: None,
        }
    }

    fn console(ts: u64, kind: &str, text: &str) -> ConsoleEntry {
        ConsoleEntry {
            ts,
            kind: kind.into(),
            text: text.into(),
            ..ConsoleEntry::default()
        }
    }

    fn extra() -> Headers {
        headers([
            ("Content-Type", "text/html"),
            ("Set-Cookie", "session=abc; HttpOnly\npref=dark; Secure"),
        ])
    }

    fn imp(id: &str) -> StoredRequest {
        StoredRequest {
            request_id: id.into(),
            url: format!("https://x.io/{id}"),
            host: "x.io".into(),
            method: "GET".into(),
            resource_type: "Fetch".into(),
            started_at: 1,
            ..StoredRequest::default()
        }
    }

    #[test]
    fn 새_항목을_기본값으로_만든다() {
        let mut store = TrafficStore::default();
        store.upsert(patch("r1", "https://a.com/x", "a.com"), NOW);
        let r = store.get("r1").unwrap();
        assert_eq!(r.url, "https://a.com/x");
        assert_eq!(r.resource_type, "Other");
        assert_eq!(r.started_at, NOW);
    }

    #[test]
    fn 기존_항목을_바꾸지_않고_필드를_병합한다() {
        let mut store = TrafficStore::default();
        store.upsert(
            RequestPatch {
                method: Some("GET".into()),
                ..patch("r1", "https://a.com", "a.com")
            },
            NOW,
        );
        store.upsert(
            RequestPatch {
                request_id: "r1".into(),
                status: Some(200),
                mime_type: Some("application/json".into()),
                ..RequestPatch::default()
            },
            NOW,
        );
        let r = store.get("r1").unwrap();
        assert_eq!(r.method, "GET"); // preserved
        assert_eq!(r.status, Some(200)); // added
        assert_eq!(r.mime_type.as_deref(), Some("application/json"));
    }

    #[test]
    fn 최신순으로_돌려준다() {
        assert_eq!(ids(&seeded(), ListFilter::default()), ["r3", "r2", "r1"]);
    }

    #[test]
    fn limit을_지킨다() {
        let filter = ListFilter {
            limit: Some(2),
            ..Default::default()
        };
        assert_eq!(ids(&seeded(), filter), ["r3", "r2"]);
    }

    #[test]
    fn 호스트_부분_문자열로_거른다() {
        let filter = ListFilter {
            host: Some("a.com".into()),
            ..Default::default()
        };
        assert_eq!(ids(&seeded(), filter), ["r3", "r1"]);
    }

    #[test]
    fn tab_id로_거른다() {
        let mut store = seeded();
        store.upsert(
            RequestPatch {
                started_at: Some(400),
                tab_id: Some(7),
                ..patch("r4", "https://c.com/4", "c.com")
            },
            NOW,
        );
        let by_tab = |tab_id| ListFilter {
            tab_id: Some(tab_id),
            ..Default::default()
        };
        assert_eq!(ids(&store, by_tab(7)), ["r4"]);
        assert!(store.list(&by_tab(99)).is_empty());
    }

    #[test]
    fn 메서드로_대소문자_구분_없이_거른다() {
        let filter = ListFilter {
            method_or_type: Some("post".into()),
            ..Default::default()
        };
        assert_eq!(ids(&seeded(), filter), ["r2"]);
    }

    #[test]
    fn 리소스_타입으로_거른다() {
        let filter = ListFilter {
            method_or_type: Some("script".into()),
            ..Default::default()
        };
        assert_eq!(ids(&seeded(), filter), ["r3"]);
    }

    #[test]
    fn since_시각으로_거른다() {
        let filter = ListFilter {
            since: Some(250),
            ..Default::default()
        };
        assert_eq!(ids(&seeded(), filter), ["r3"]);
    }

    // `before`/`before_id` are the pagination cursor pair: entries strictly
    // older than the boundary entry. `before` alone stays inclusive on the
    // boundary ms — HAR entries carry no requestId, so a caller paging by
    // timestamp alone can't name the boundary and would lose same-ms siblings.
    #[test]
    fn before만_주면_경계_밀리초의_항목을_포함한다() {
        let filter = ListFilter {
            before: Some(300),
            ..Default::default()
        };
        assert_eq!(ids(&seeded(), filter), ["r3", "r2", "r1"]);
    }

    fn cursor(before: u64, before_id: &str) -> ListFilter {
        ListFilter {
            before: Some(before),
            before_id: Some(before_id.into()),
            ..Default::default()
        }
    }

    #[test]
    fn before와_before_id는_경계_항목을_빼서_페이지가_전진한다() {
        assert_eq!(ids(&seeded(), cursor(300, "r3")), ["r2", "r1"]);
    }

    #[test]
    fn before와_before_id는_경계보다_오래된_같은_밀리초_항목을_남긴다() {
        let mut store = seeded();
        store.upsert(
            typed("r4", "https://c.com/4", "c.com", "GET", "XHR", 300),
            NOW,
        );
        // r3 and r4 share ms 300; r4 was inserted later (newer). Boundary=r4
        // keeps the older sibling r3…
        let mut page = ids(&store, cursor(300, "r4"));
        page.sort();
        assert_eq!(page, ["r1", "r2", "r3"]);
        // …and boundary=r3 excludes the newer same-ms sibling r4.
        assert_eq!(ids(&store, cursor(300, "r3")), ["r2", "r1"]);
    }

    #[test]
    fn 가장_오래된_항목이_경계면_빈_목록이라_페이징이_끝난다() {
        assert!(ids(&seeded(), cursor(100, "r1")).is_empty());
    }

    #[test]
    fn before와_since를_함께_주면_구간을_고른다() {
        let filter = ListFilter {
            since: Some(150),
            before: Some(299),
            ..Default::default()
        };
        assert_eq!(ids(&seeded(), filter), ["r2"]);
    }

    #[test]
    fn 모든_항목보다_이른_before는_빈_목록이다() {
        let filter = ListFilter {
            before: Some(50),
            ..Default::default()
        };
        assert!(ids(&seeded(), filter).is_empty());
    }

    #[test]
    fn 밀려난_before_id는_그_id만_제외한다() {
        // The boundary entry is gone from the store — same-ms siblings can't be
        // ordered against it, so they're kept and the caller dedupes.
        assert_eq!(ids(&seeded(), cursor(300, "gone")), ["r3", "r2", "r1"]);
    }

    #[test]
    fn 상한을_넘는_본문은_잘라내고_표시한다() {
        let mut store = TrafficStore::default();
        store.upsert(
            RequestPatch {
                response_body: Some("a".repeat(MAX_BODY_CHARS + 100)),
                ..patch("big", "https://x/b.js", "x")
            },
            NOW,
        );
        let r = store.get("big").unwrap();
        assert_eq!(
            r.response_body.as_ref().map(String::len),
            Some(8 * 1024 * 1024)
        );
        assert_eq!(r.response_body_truncated, Some(true));
    }

    #[test]
    fn 보통_본문은_그대로_둔다() {
        let mut store = TrafficStore::default();
        store.upsert(
            RequestPatch {
                response_body: Some("hello".into()),
                ..patch("ok", "https://x/s.js", "x")
            },
            NOW,
        );
        let r = store.get("ok").unwrap();
        assert_eq!(r.response_body.as_deref(), Some("hello"));
        assert_eq!(r.response_body_truncated, None);
    }

    #[test]
    fn 항목이_500건을_넘으면_가장_오래된_것을_버린다() {
        let mut store = TrafficStore::default();
        for i in 0..510 {
            store.upsert(
                RequestPatch {
                    started_at: Some(i),
                    ..patch(&format!("e{i}"), &format!("https://x/{i}"), "x")
                },
                NOW,
            );
        }
        assert!(store.get("e0").is_none()); // evicted
        assert!(store.get("e9").is_none()); // evicted
        assert!(store.get("e10").is_some()); // first survivor
        assert!(store.get("e509").is_some());
        let all = store.list(&ListFilter {
            limit: Some(1000),
            ..Default::default()
        });
        assert_eq!(all.len(), 500);
    }

    #[test]
    fn 프레임을_요청별로_쌓고_since로_거른다() {
        let mut store = TrafficStore::default();
        store.append_ws_frame("ws1", frame(WsDirection::Sent, "a", 10));
        store.append_ws_frame("ws1", frame(WsDirection::Received, "b", 20));
        assert_eq!(store.ws_frames("ws1", None).len(), 2);
        let payloads: Vec<&str> = store
            .ws_frames("ws1", Some(15))
            .iter()
            .map(|f| f.payload_data.as_str())
            .collect();
        assert_eq!(payloads, ["b"]);
        assert!(store.ws_frames("missing", None).is_empty());
    }

    #[test]
    fn 요청별_프레임_수를_제한해_오래_열린_소켓이_무한히_커지지_않는다() {
        let mut store = TrafficStore::default();
        for i in 0..2100 {
            store.append_ws_frame("wsCap", frame(WsDirection::Received, &format!("f{i}"), i));
        }
        let frames = store.ws_frames("wsCap", None);
        assert_eq!(frames.len(), 2000); // MAX_WS_FRAMES_PER_REQUEST
        assert_eq!(frames.first().unwrap().payload_data, "f100"); // oldest 100 dropped
        assert_eq!(frames.last().unwrap().payload_data, "f2099");
    }

    #[test]
    fn 요청이_링_버퍼에서_밀려나면_프레임도_버린다() {
        let mut store = TrafficStore::default();
        store.upsert(
            RequestPatch {
                resource_type: Some("WebSocket".into()),
                started_at: Some(0),
                ..patch("wsEvict", "wss://x", "x")
            },
            NOW,
        );
        store.append_ws_frame("wsEvict", frame(WsDirection::Sent, "p", 1));
        assert_eq!(store.ws_frames("wsEvict", None).len(), 1);
        // Push the WS request out of the 500-entry window.
        for i in 0..500 {
            store.upsert(
                RequestPatch {
                    started_at: Some(i + 1),
                    ..patch(&format!("pad{i}"), &format!("https://x/{i}"), "x")
                },
                NOW,
            );
        }
        assert!(store.get("wsEvict").is_none()); // evicted
        assert!(store.ws_frames("wsEvict", None).is_empty()); // frames freed, no leak
    }

    #[test]
    fn 트래픽을_지우면_프레임도_모두_지운다() {
        let mut store = TrafficStore::default();
        store.upsert(
            RequestPatch {
                resource_type: Some("WebSocket".into()),
                ..patch("wsClear", "wss://x", "x")
            },
            NOW,
        );
        store.append_ws_frame("wsClear", frame(WsDirection::Sent, "p", 1));
        store.clear();
        assert!(store.ws_frames("wsClear", None).is_empty());
    }

    #[test]
    fn 콘솔_로그를_쌓고_since로_거른다() {
        let mut store = TrafficStore::default();
        store.append_console(console(10, "log", "one"));
        store.append_console(console(20, "warn", "two"));
        let texts = |since| -> Vec<String> {
            store
                .console_since(since)
                .iter()
                .map(|e| e.text.clone())
                .collect()
        };
        assert_eq!(texts(None), ["one", "two"]);
        assert_eq!(texts(Some(15)), ["two"]);
    }

    #[test]
    fn 콘솔_링_버퍼는_1000건으로_제한된다() {
        let mut store = TrafficStore::default();
        for i in 0..1050 {
            store.append_console(console(i, "log", &format!("m{i}")));
        }
        let all = store.console_since(None);
        assert_eq!(all.len(), 1000);
        assert_eq!(all.first().unwrap().text, "m50"); // oldest 50 dropped
    }

    #[test]
    fn 예외를_쌓고_200건으로_제한한다() {
        let mut store = TrafficStore::default();
        for i in 0..250 {
            store.append_exception(RuntimeException {
                ts: i,
                text: format!("boom{i}"),
                ..RuntimeException::default()
            });
        }
        let all = store.exceptions();
        assert_eq!(all.len(), 200);
        assert_eq!(all.last().unwrap().text, "boom249");
    }

    // CDP Network.responseReceived.response.headers omits Set-Cookie; the raw
    // header block arrives in responseReceivedExtraInfo in either order.

    #[test]
    fn extra_info가_response_received보다_먼저_와도_병합한다() {
        let mut store = TrafficStore::default();
        store.upsert(patch("r1", "https://a.com/", "a.com"), NOW);
        store.merge_extra_response_headers("r1", extra(), 200);
        // responseReceived lands: caller merges pending extra headers over the
        // event's own header map (same shape as chrome-cdp.ts / external-cdp.ts).
        let pending = store.take_pending_extra_response_headers("r1", 200);
        let event = headers([("Content-Type", "text/html"), ("Server", "x")]);
        store.upsert(response("r1", 200, spread(event, pending)), NOW);
        assert_eq!(
            response_header(&store, "r1", "Set-Cookie"),
            Some("session=abc; HttpOnly\npref=dark; Secure")
        );
        assert_eq!(response_header(&store, "r1", "Server"), Some("x"));
    }

    #[test]
    fn extra_info가_response_received보다_나중에_와도_병합한다() {
        let mut store = TrafficStore::default();
        store.upsert(
            RequestPatch {
                status: Some(200),
                response_headers: Some(headers([("Content-Type", "text/html")])),
                ..patch("r2", "https://a.com/", "a.com")
            },
            NOW,
        );
        store.merge_extra_response_headers("r2", extra(), 200);
        assert_eq!(
            response_header(&store, "r2", "Set-Cookie"),
            Some("session=abc; HttpOnly\npref=dark; Secure")
        );
        assert_eq!(
            response_header(&store, "r2", "Content-Type"),
            Some("text/html")
        );
        assert_eq!(store.take_pending_extra_response_headers("r2", 200), None);
    }

    #[test]
    fn 버퍼에_extra_info가_없는_요청은_none을_돌려준다() {
        let mut store = TrafficStore::default();
        assert_eq!(store.take_pending_extra_response_headers("nope", 200), None);
    }

    #[test]
    fn 원본_extra_info_블록이_이벤트_헤더_맵을_이긴다() {
        let mut store = TrafficStore::default();
        store.upsert(
            RequestPatch {
                status: Some(200),
                response_headers: Some(headers([("X-Shared", "event-value")])),
                ..patch("r2b", "https://a.com/", "a.com")
            },
            NOW,
        );
        store.merge_extra_response_headers("r2b", headers([("X-Shared", "raw-value")]), 200);
        assert_eq!(
            response_header(&store, "r2b", "X-Shared"),
            Some("raw-value")
        );
    }

    #[test]
    fn 리다이렉트_홉의_extra_info를_최종_응답에_흘리지_않고_버린다() {
        // Redirect hops share the requestId: /login 302 (Set-Cookie: hop=REDIRECT)
        // followed by /home 200 (Set-Cookie: hop=FINAL). The 302's raw block must
        // never land on the 200's stored headers.
        let mut store = TrafficStore::default();
        store.upsert(patch("r3", "https://a.com/login", "a.com"), NOW);
        store.merge_extra_response_headers("r3", headers([("Set-Cookie", "hop=REDIRECT")]), 302);
        // requestWillBeSent for the next hop carries redirectResponse → discard.
        store.discard_pending_extra_response_headers("r3");
        let pending = store.take_pending_extra_response_headers("r3", 200);
        let event = headers([("Content-Type", "text/html")]);
        store.upsert(response("r3", 200, spread(event, pending)), NOW);
        // The final hop's own ExtraInfo arrives after and wins.
        store.merge_extra_response_headers("r3", headers([("Set-Cookie", "hop=FINAL")]), 200);
        assert_eq!(
            response_header(&store, "r3", "Set-Cookie"),
            Some("hop=FINAL")
        );
    }

    #[test]
    fn 상태_코드가_맞지_않는_버퍼된_리다이렉트_블록을_버린다() {
        // A 302's ExtraInfo buffered after the redirect requestWillBeSent (or
        // without one being observed) is still rejected by the status check.
        let mut store = TrafficStore::default();
        store.upsert(patch("r4", "https://a.com/login", "a.com"), NOW);
        store.merge_extra_response_headers("r4", headers([("Set-Cookie", "hop=REDIRECT")]), 302);
        assert_eq!(store.take_pending_extra_response_headers("r4", 200), None);
        // Consumed — a later take must not resurrect it.
        assert_eq!(store.take_pending_extra_response_headers("r4", 302), None);
    }

    #[test]
    fn 최종_응답_뒤에_늦게_온_리다이렉트_홉_extra_info를_버린다() {
        let mut store = TrafficStore::default();
        store.upsert(
            RequestPatch {
                status: Some(200),
                response_headers: Some(headers([("Set-Cookie", "hop=FINAL")])),
                ..patch("r5", "https://a.com/home", "a.com")
            },
            NOW,
        );
        store.merge_extra_response_headers("r5", headers([("Set-Cookie", "hop=REDIRECT")]), 302);
        assert_eq!(
            response_header(&store, "r5", "Set-Cookie"),
            Some("hop=FINAL")
        );
    }

    #[test]
    fn 같은_request_id의_인증_재시도에서_최종_응답_헤더를_지킨다() {
        // HTTP auth challenges reuse the requestId without redirectResponse:
        // 401 → 200. The 200's ExtraInfo can land while the stored status is
        // still 401 — it must not be dropped as a stale redirect hop.
        let mut store = TrafficStore::default();
        store.upsert(patch("r6", "https://a.com/", "a.com"), NOW);
        store.merge_extra_response_headers("r6", headers([("Set-Cookie", "challenge=1")]), 401);
        let pending = store.take_pending_extra_response_headers("r6", 401);
        let challenge = headers([("WWW-Authenticate", "Basic")]);
        store.upsert(response("r6", 401, spread(challenge, pending)), NOW);
        store.merge_extra_response_headers("r6", headers([("Set-Cookie", "hop=FINAL")]), 200);
        let pending = store.take_pending_extra_response_headers("r6", 200);
        let event = headers([("Content-Type", "text/html")]);
        store.upsert(response("r6", 200, spread(event, pending)), NOW);
        assert_eq!(
            response_header(&store, "r6", "Set-Cookie"),
            Some("hop=FINAL")
        );
    }

    #[test]
    fn 가져온_항목은_get으로_찾을_수_있다() {
        let mut store = TrafficStore::default();
        store.import_requests(vec![imp("har:a:0")]);
        assert_eq!(
            store.get("har:a:0").map(|r| r.url.as_str()),
            Some("https://x.io/har:a:0")
        );
    }

    #[test]
    fn list는_기본으로_실시간_캡처만_source로_가져온_것과_전체를_고른다() {
        let mut store = TrafficStore::default();
        store.upsert(
            typed("live-1", "https://x.io/live", "x.io", "GET", "Fetch", 5),
            NOW,
        );
        store.import_requests(vec![imp("har:a:0")]);
        let from = |source| ListFilter {
            source,
            ..Default::default()
        };
        assert!(!ids(&store, ListFilter::default()).contains(&"har:a:0".to_owned()));
        assert_eq!(ids(&store, from(Source::Import)), ["har:a:0"]);
        let mut all = ids(&store, from(Source::All));
        all.sort();
        assert_eq!(all, ["har:a:0", "live-1"]);
    }

    #[test]
    fn 가져온_항목은_5000건을_넘으면_오래된_것부터_지운다() {
        let mut store = TrafficStore::default();
        store.import_requests((0..5_001).map(|i| imp(&format!("har:b:{i}"))).collect());
        assert!(store.get("har:b:0").is_none());
        assert!(store.get("har:b:5000").is_some());
    }

    #[test]
    fn clear_imports는_가져온_항목만_지운다() {
        let mut store = TrafficStore::default();
        store.upsert(typed("live-2", "u", "x.io", "GET", "Fetch", 1), NOW);
        store.import_requests(vec![imp("har:c:0")]);
        store.clear_imports();
        assert!(store.get("har:c:0").is_none());
        assert!(store.get("live-2").is_some());
    }
}
