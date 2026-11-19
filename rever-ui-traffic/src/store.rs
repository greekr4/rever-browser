//! Port of `src/renderer/src/stores/traffic.ts` — the list's own state: the
//! entry ring buffer, the endpoint buckets, the selection and the open detail.
//!
//! The TypeScript keeps this in a zustand store; here it is one `TrafficState`
//! the views share through a GPUI entity.

use std::collections::HashMap;

use url::Url;

use crate::types::{ApiEndpoint, NetworkEvent, TrafficEntry};

pub const MAX_ENTRIES: usize = 500;
/// Endpoints are aggregated once per request, never by rescanning entries.
/// They outlive the entry ring buffer on purpose — the map should not forget an
/// endpoint just because its request scrolled out.
pub const MAX_ENDPOINTS: usize = 300;
const API_TYPES: [&str; 2] = ["XHR", "Fetch"];
// The TypeScript pattern is /(oauth|token|login|signin|auth|session|refresh)/i.
const AUTH_HINTS: [&str; 7] = [
    "oauth", "token", "login", "signin", "auth", "session", "refresh",
];

fn is_hex(bytes: &[u8]) -> bool {
    bytes.iter().all(u8::is_ascii_hexdigit)
}

/// /users/12/orders/9f8a-... → /users/:id/orders/:id
///
/// Same rules as `templatePath` in the Electron app's shared endpoint-template
/// module, whose Rust port lives in a crate this one may not depend on. The
/// three patterns are ASCII-only: `\d` there never matches other scripts' digits.
fn template_path(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            let b = seg.as_bytes();
            let is_id = !b.is_empty()
                // ^\d+$
                && (b.iter().all(u8::is_ascii_digit)
                    // ^[0-9a-f]{16,}$ (case-insensitive)
                    || (b.len() >= 16 && is_hex(b))
                    // ^[0-9a-f]{8}-[0-9a-f]{4}- (case-insensitive, prefix only)
                    || (b.get(..8).is_some_and(is_hex)
                        && b.get(8) == Some(&b'-')
                        && b.get(9..13).is_some_and(is_hex)
                        && b.get(13) == Some(&b'-')));
            if is_id { ":id" } else { seg }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Count `entry` into its endpoint bucket and stamp the bucket's key on it.
fn aggregate(endpoints: &mut Vec<ApiEndpoint>, entry: &mut TrafficEntry, ts: f64) {
    let Ok(url) = Url::parse(&entry.url) else {
        return;
    };
    let path = template_path(url.path());
    let origin = url.origin().ascii_serialization();
    let key = format!("{} {origin}{path}", entry.method);
    entry.endpoint_key = Some(key.clone());

    match endpoints.iter_mut().find(|endpoint| endpoint.key == key) {
        Some(hit) => {
            hit.count += 1;
            hit.last_request_id = entry.request_id.clone();
            hit.last_seen = ts;
        }
        None => {
            let lower = path.to_ascii_lowercase();
            endpoints.push(ApiEndpoint {
                key,
                origin,
                host: url.host_str().unwrap_or_default().to_owned(),
                auth: AUTH_HINTS.iter().any(|hint| lower.contains(hint)),
                path,
                method: entry.method.clone(),
                count: 1,
                last_request_id: entry.request_id.clone(),
                last_seen: ts,
                statuses: Vec::new(),
            });
        }
    }

    // Drop the least-recently-seen endpoints. Among equals the earliest inserted
    // goes first, which is what the TypeScript's stable sort over the keys does.
    while endpoints.len() > MAX_ENDPOINTS {
        let oldest = endpoints
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.last_seen.total_cmp(&b.last_seen))
            .map(|(index, _)| index);
        let Some(oldest) = oldest else { break };
        endpoints.remove(oldest);
    }
}

#[derive(Clone, Debug, Default)]
pub struct TrafficState {
    pub entries: HashMap<String, TrafficEntry>,
    /// Request ids, oldest first.
    pub order: Vec<String>,
    /// In insertion order. shortcut: lookups scan at most 300 buckets; switch to
    /// an indexed map if the cap grows.
    pub endpoints: Vec<ApiEndpoint>,
    /// In the order the rows were selected, as a JavaScript `Set` keeps them.
    pub selected: Vec<String>,
    pub detail_id: Option<String>,
    pub last_selected_id: Option<String>,
}

impl TrafficState {
    pub fn endpoint(&self, key: &str) -> Option<&ApiEndpoint> {
        self.endpoints.iter().find(|endpoint| endpoint.key == key)
    }

    pub fn apply_event(&mut self, event: NetworkEvent) {
        match event {
            NetworkEvent::Request {
                request_id,
                url,
                method,
                resource_type,
                timestamp,
            } => {
                if self.entries.contains_key(&request_id) {
                    return;
                }
                let mut next = TrafficEntry {
                    request_id: request_id.clone(),
                    url,
                    method,
                    resource_type,
                    started_at: timestamp,
                    ..TrafficEntry::default()
                };
                if API_TYPES.contains(&next.resource_type.as_str()) {
                    aggregate(&mut self.endpoints, &mut next, timestamp);
                }
                self.order.push(request_id.clone());
                self.entries.insert(request_id, next);
                // Over the cap: drop the oldest entries.
                let excess = self.order.len().saturating_sub(MAX_ENTRIES);
                for removed in self.order.drain(..excess) {
                    self.entries.remove(&removed);
                }
            }
            NetworkEvent::Response {
                request_id,
                status,
                mime_type,
                ..
            } => {
                let Some(existing) = self.entries.get_mut(&request_id) else {
                    return;
                };
                existing.status = Some(status);
                existing.mime_type = Some(mime_type);
                let bucket = existing
                    .endpoint_key
                    .as_ref()
                    .and_then(|key| self.endpoints.iter_mut().find(|e| &e.key == key));
                if let Some(bucket) = bucket
                    && !bucket.statuses.contains(&status)
                {
                    bucket.statuses.push(status);
                }
            }
            NetworkEvent::Finished {
                request_id,
                encoded_data_length,
                timestamp,
            } => {
                let Some(existing) = self.entries.get_mut(&request_id) else {
                    return;
                };
                existing.encoded_data_length = Some(encoded_data_length);
                existing.completed_at = Some(timestamp);
            }
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn toggle_select(&mut self, id: &str) {
        match self.selected.iter().position(|selected| selected == id) {
            Some(index) => {
                self.selected.remove(index);
            }
            None => self.selected.push(id.to_owned()),
        }
        self.last_selected_id = Some(id.to_owned());
    }

    /// Add every row between the last selected one and `to_id`, both included.
    pub fn select_range(&mut self, to_id: &str) {
        // An empty id counts as "nothing selected yet", as it is falsy in the TypeScript.
        let Some(last) = self.last_selected_id.as_ref().filter(|id| !id.is_empty()) else {
            self.selected = vec![to_id.to_owned()];
            self.last_selected_id = Some(to_id.to_owned());
            return;
        };
        let position = |id: &str| self.order.iter().position(|o| o == id);
        let (Some(from), Some(to)) = (position(last), position(to_id)) else {
            return;
        };
        let (a, b) = if from <= to { (from, to) } else { (to, from) };
        for id in self.order.get(a..=b).unwrap_or_default() {
            if !self.selected.contains(id) {
                self.selected.push(id.clone());
            }
        }
        self.last_selected_id = Some(to_id.to_owned());
    }

    pub fn clear_selection(&mut self) {
        self.selected.clear();
        self.last_selected_id = None;
    }

    pub fn open_detail(&mut self, id: &str) {
        self.detail_id = Some(id.to_owned());
    }

    pub fn close_detail(&mut self) {
        self.detail_id = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str, url: &str, method: &str, kind: &str, ts: f64) -> NetworkEvent {
        NetworkEvent::Request {
            request_id: id.into(),
            url: url.into(),
            method: method.into(),
            resource_type: kind.into(),
            timestamp: ts,
        }
    }

    fn response(id: &str, status: u16) -> NetworkEvent {
        NetworkEvent::Response {
            request_id: id.into(),
            status,
            mime_type: "application/json".into(),
            timestamp: 0.0,
        }
    }

    fn filled(count: usize) -> TrafficState {
        let mut state = TrafficState::default();
        for i in 0..count {
            let id = i.to_string();
            state.apply_event(request(&id, "https://a.test/", "GET", "Document", i as f64));
        }
        state
    }

    #[test]
    fn 요청_이벤트는_항목을_만들고_순서_끝에_붙인다() {
        let mut state = TrafficState::default();
        state.apply_event(request("a", "https://a.test/x", "GET", "Document", 5.0));
        state.apply_event(request("b", "https://a.test/y", "POST", "Script", 6.0));
        assert_eq!(state.order, ["a", "b"]);
        let entry = &state.entries["a"];
        assert_eq!(entry.url, "https://a.test/x");
        assert_eq!(entry.method, "GET");
        assert_eq!(entry.resource_type, "Document");
        assert_eq!(entry.started_at, 5.0);
        assert_eq!(entry.status, None);
        assert_eq!(entry.endpoint_key, None);
    }

    #[test]
    fn 같은_id의_요청이_다시_오면_무시한다() {
        let mut state = TrafficState::default();
        state.apply_event(request("a", "https://a.test/x", "GET", "XHR", 1.0));
        state.apply_event(request("a", "https://other.test/", "POST", "XHR", 2.0));
        assert_eq!(state.order, ["a"]);
        assert_eq!(state.entries["a"].url, "https://a.test/x");
        assert_eq!(state.endpoints.len(), 1);
        assert_eq!(state.endpoints[0].count, 1);
    }

    #[test]
    fn 응답과_완료는_기존_항목에_합친다() {
        let mut state = TrafficState::default();
        state.apply_event(request("a", "https://a.test/x", "GET", "Document", 1.0));
        state.apply_event(NetworkEvent::Response {
            request_id: "a".into(),
            status: 200,
            mime_type: "text/html".into(),
            timestamp: 2.0,
        });
        state.apply_event(NetworkEvent::Finished {
            request_id: "a".into(),
            encoded_data_length: 1234,
            timestamp: 3.0,
        });
        let entry = &state.entries["a"];
        assert_eq!(entry.status, Some(200));
        assert_eq!(entry.mime_type.as_deref(), Some("text/html"));
        assert_eq!(entry.encoded_data_length, Some(1234));
        assert_eq!(entry.completed_at, Some(3.0));
        assert_eq!(entry.started_at, 1.0);
    }

    #[test]
    fn 모르는_요청의_응답과_완료는_버린다() {
        let mut state = TrafficState::default();
        state.apply_event(response("ghost", 200));
        state.apply_event(NetworkEvent::Finished {
            request_id: "ghost".into(),
            encoded_data_length: 1,
            timestamp: 1.0,
        });
        assert!(state.entries.is_empty());
        assert!(state.order.is_empty());
    }

    #[test]
    fn 상한을_넘으면_가장_오래된_항목부터_지운다() {
        let mut state = filled(MAX_ENTRIES);
        assert_eq!(state.order.len(), MAX_ENTRIES);
        state.apply_event(request("new", "https://a.test/", "GET", "Document", 999.0));
        assert_eq!(state.order.len(), MAX_ENTRIES);
        assert_eq!(state.entries.len(), MAX_ENTRIES);
        assert!(!state.entries.contains_key("0"));
        assert_eq!(state.order.first().map(String::as_str), Some("1"));
        assert_eq!(state.order.last().map(String::as_str), Some("new"));
        assert!(state.entries.contains_key("new"));
    }

    #[test]
    fn xhr과_fetch만_엔드포인트로_모은다() {
        let mut state = TrafficState::default();
        state.apply_event(request(
            "1",
            "https://api.test/users/12/orders",
            "GET",
            "XHR",
            1.0,
        ));
        state.apply_event(request(
            "2",
            "https://api.test/users/34/orders?page=2",
            "GET",
            "Fetch",
            2.0,
        ));
        state.apply_event(request(
            "3",
            "https://api.test/app.js",
            "GET",
            "Script",
            3.0,
        ));
        assert_eq!(state.endpoints.len(), 1);
        let endpoint = &state.endpoints[0];
        assert_eq!(endpoint.key, "GET https://api.test/users/:id/orders");
        assert_eq!(endpoint.origin, "https://api.test");
        assert_eq!(endpoint.host, "api.test");
        assert_eq!(endpoint.path, "/users/:id/orders");
        assert_eq!(endpoint.method, "GET");
        assert_eq!(endpoint.count, 2);
        assert_eq!(endpoint.last_request_id, "2");
        assert_eq!(endpoint.last_seen, 2.0);
        assert!(!endpoint.auth);
        assert_eq!(
            state.entries["1"].endpoint_key.as_deref(),
            Some("GET https://api.test/users/:id/orders")
        );
        assert_eq!(state.entries["3"].endpoint_key, None);
        assert!(
            state
                .endpoint("GET https://api.test/users/:id/orders")
                .is_some()
        );
    }

    #[test]
    fn 메서드와_출처가_다르면_다른_엔드포인트다() {
        let mut state = TrafficState::default();
        state.apply_event(request("1", "https://a.test:8443/x", "GET", "XHR", 1.0));
        state.apply_event(request("2", "https://a.test:8443/x", "POST", "XHR", 2.0));
        state.apply_event(request("3", "https://a.test/x", "GET", "XHR", 3.0));
        // The default port is not part of the origin.
        state.apply_event(request("4", "https://a.test:443/x", "GET", "XHR", 4.0));
        let keys: Vec<_> = state.endpoints.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "GET https://a.test:8443/x",
                "POST https://a.test:8443/x",
                "GET https://a.test/x"
            ]
        );
        assert_eq!(state.endpoints[2].count, 2);
        assert_eq!(state.endpoints[0].host, "a.test");
    }

    #[test]
    fn 경로의_id_조각은_접는다() {
        assert_eq!(template_path("/users/12/orders/0"), "/users/:id/orders/:id");
        assert_eq!(template_path("/f/0123456789ABCDEF00"), "/f/:id");
        // 15 hex digits are one short; a non-hex letter disqualifies.
        assert_eq!(template_path("/f/0123456789abcde"), "/f/0123456789abcde");
        assert_eq!(template_path("/f/0123456789abcdeg"), "/f/0123456789abcdeg");
        assert_eq!(
            template_path("/o/9F8A1b2c-3d4e-5f60-7182-93a4b5c6d7e8/x"),
            "/o/:id/x"
        );
        assert_eq!(template_path("/o/9f8a1b2c-3d4e-anything"), "/o/:id");
        assert_eq!(template_path("/o/9f8a1b2c-3d4e"), "/o/9f8a1b2c-3d4e");
        assert_eq!(template_path("//a/"), "//a/");
        assert_eq!(template_path("/v1/١٢"), "/v1/١٢");
        assert_eq!(template_path(""), "");
    }

    #[test]
    fn 인증처럼_보이는_경로는_auth로_표시한다() {
        let mut state = TrafficState::default();
        state.apply_event(request(
            "1",
            "https://a.test/OAuth/Token",
            "POST",
            "XHR",
            1.0,
        ));
        state.apply_event(request("2", "https://a.test/api/items", "GET", "XHR", 2.0));
        state.apply_event(request(
            "3",
            "https://a.test/v1/session",
            "GET",
            "Fetch",
            3.0,
        ));
        // Only the path counts, not the host or the query.
        state.apply_event(request(
            "4",
            "https://login.test/x?token=1",
            "GET",
            "XHR",
            4.0,
        ));
        let auth: Vec<_> = state.endpoints.iter().map(|e| e.auth).collect();
        assert_eq!(auth, [true, false, true, false]);
    }

    #[test]
    fn 응답_상태는_엔드포인트에_한_번씩만_쌓인다() {
        let mut state = TrafficState::default();
        state.apply_event(request("1", "https://a.test/x", "GET", "XHR", 1.0));
        state.apply_event(request("2", "https://a.test/x", "GET", "XHR", 2.0));
        state.apply_event(request("3", "https://a.test/x", "GET", "XHR", 3.0));
        state.apply_event(response("1", 200));
        state.apply_event(response("2", 200));
        state.apply_event(response("3", 500));
        assert_eq!(state.endpoints[0].statuses, [200, 500]);
    }

    #[test]
    fn 주소를_해석할_수_없으면_엔드포인트를_만들지_않는다() {
        let mut state = TrafficState::default();
        state.apply_event(request("1", "not a url", "GET", "XHR", 1.0));
        assert!(state.endpoints.is_empty());
        assert_eq!(state.entries["1"].endpoint_key, None);
        // The entry itself is still listed.
        assert_eq!(state.order, ["1"]);
    }

    #[test]
    fn 엔드포인트가_상한을_넘으면_가장_오래_전에_본_것을_버린다() {
        let mut state = TrafficState::default();
        for i in 0..MAX_ENDPOINTS {
            let url = format!("https://a.test/p{i}x");
            state.apply_event(request(
                &format!("r{i}"),
                &url,
                "GET",
                "XHR",
                10.0 + i as f64,
            ));
        }
        // Seeing the first endpoint again makes the second one the oldest.
        state.apply_event(request("again", "https://a.test/p0x", "GET", "XHR", 5000.0));
        state.apply_event(request("extra", "https://a.test/new", "GET", "XHR", 5001.0));
        assert_eq!(state.endpoints.len(), MAX_ENDPOINTS);
        assert!(state.endpoint("GET https://a.test/p0x").is_some());
        assert!(state.endpoint("GET https://a.test/p1x").is_none());
        assert!(state.endpoint("GET https://a.test/new").is_some());
    }

    #[test]
    fn 같은_시각에_본_엔드포인트는_먼저_들어온_것부터_버린다() {
        let mut state = TrafficState::default();
        for i in 0..=MAX_ENDPOINTS {
            let url = format!("https://a.test/p{i}x");
            state.apply_event(request(&format!("r{i}"), &url, "GET", "XHR", 1.0));
        }
        assert_eq!(state.endpoints.len(), MAX_ENDPOINTS);
        assert!(state.endpoint("GET https://a.test/p0x").is_none());
        assert!(state.endpoint("GET https://a.test/p1x").is_some());
    }

    #[test]
    fn 엔드포인트는_항목이_밀려나도_남는다() {
        let mut state = TrafficState::default();
        state.apply_event(request("api", "https://a.test/api", "GET", "XHR", 0.0));
        for i in 0..MAX_ENTRIES {
            let id = format!("d{i}");
            state.apply_event(request(&id, "https://a.test/", "GET", "Document", 1.0));
        }
        assert!(!state.entries.contains_key("api"));
        assert_eq!(state.endpoints.len(), 1);
    }

    #[test]
    fn 선택은_누를_때마다_켜지고_꺼진다() {
        let mut state = filled(3);
        state.toggle_select("1");
        state.toggle_select("0");
        assert_eq!(state.selected, ["1", "0"]);
        assert_eq!(state.last_selected_id.as_deref(), Some("0"));
        state.toggle_select("1");
        assert_eq!(state.selected, ["0"]);
        // Unselecting also moves the range anchor.
        assert_eq!(state.last_selected_id.as_deref(), Some("1"));
    }

    #[test]
    fn 기준이_없을_때의_범위_선택은_그_행만_고른다() {
        let mut state = filled(5);
        state.selected = vec!["0".into()];
        state.select_range("3");
        assert_eq!(state.selected, ["3"]);
        assert_eq!(state.last_selected_id.as_deref(), Some("3"));

        // An empty anchor id is treated like no anchor.
        state.last_selected_id = Some(String::new());
        state.select_range("1");
        assert_eq!(state.selected, ["1"]);
    }

    #[test]
    fn 범위_선택은_기준과_대상_사이를_모두_더한다() {
        let mut state = filled(6);
        state.toggle_select("1");
        state.select_range("4");
        assert_eq!(state.selected, ["1", "2", "3", "4"]);
        assert_eq!(state.last_selected_id.as_deref(), Some("4"));
        // Backwards from the new anchor; rows already selected keep their place.
        state.select_range("0");
        assert_eq!(state.selected, ["1", "2", "3", "4", "0"]);
        assert_eq!(state.last_selected_id.as_deref(), Some("0"));
    }

    #[test]
    fn 기준이나_대상이_목록에_없으면_범위_선택은_아무것도_하지_않는다() {
        let mut state = filled(3);
        state.toggle_select("1");
        state.select_range("missing");
        assert_eq!(state.selected, ["1"]);
        assert_eq!(state.last_selected_id.as_deref(), Some("1"));

        state.last_selected_id = Some("gone".into());
        state.select_range("2");
        assert_eq!(state.selected, ["1"]);
        assert_eq!(state.last_selected_id.as_deref(), Some("gone"));
    }

    #[test]
    fn 선택_해제는_기준도_지운다() {
        let mut state = filled(3);
        state.toggle_select("1");
        state.clear_selection();
        assert!(state.selected.is_empty());
        assert_eq!(state.last_selected_id, None);
    }

    #[test]
    fn 상세는_열고_닫을_수_있다() {
        let mut state = filled(2);
        state.open_detail("1");
        assert_eq!(state.detail_id.as_deref(), Some("1"));
        state.open_detail("0");
        assert_eq!(state.detail_id.as_deref(), Some("0"));
        state.close_detail();
        assert_eq!(state.detail_id, None);
    }

    #[test]
    fn 지우기는_모든_상태를_비운다() {
        let mut state = TrafficState::default();
        state.apply_event(request("1", "https://a.test/api", "GET", "XHR", 1.0));
        state.toggle_select("1");
        state.open_detail("1");
        state.clear();
        assert!(state.entries.is_empty());
        assert!(state.order.is_empty());
        assert!(state.endpoints.is_empty());
        assert!(state.selected.is_empty());
        assert_eq!(state.detail_id, None);
        assert_eq!(state.last_selected_id, None);
    }

    #[test]
    fn 밀려난_항목의_선택과_상세는_그대로_남는다() {
        // The TypeScript store never prunes these; the views cope with the missing entry.
        let mut state = filled(MAX_ENTRIES);
        state.toggle_select("0");
        state.open_detail("0");
        state.apply_event(request("new", "https://a.test/", "GET", "Document", 1.0));
        assert!(!state.entries.contains_key("0"));
        assert_eq!(state.selected, ["0"]);
        assert_eq!(state.detail_id.as_deref(), Some("0"));
    }
}
