//! Port of the SAFE subset of `src/main/repeater.ts` — turning a captured
//! request (plus optional overrides) into a [`RequestSpec`] the probes can
//! send. The attack-delivery paths of repeater.ts (`repeaterSend`, `burst`,
//! `intruder`) are deliberately left out; they live elsewhere.
//!
//! Unlike the TypeScript, the spec is sent by [`crate::send`] over `reqwest`,
//! not an in-page `fetch()`. `fetch()` refuses a long list of request headers
//! (host, cookie2, user-agent, referer, origin, sec-*, …) and silently re-adds
//! cookies/UA from the browser; `reqwest` does neither. So we keep the captured
//! auth-bearing headers (Cookie, Authorization, User-Agent, Referer, Origin)
//! that carry the session, and drop only the few headers the client must own.

use rever_core::traffic::{Headers, StoredRequest, TrafficStore};

/// The safe subset of repeater.ts's `RepeaterModifications`: field overrides a
/// caller layers on the captured base request. No send/burst/intruder knobs.
#[derive(Clone, Debug, Default)]
pub struct Mods {
    pub url: Option<String>,
    pub method: Option<String>,
    pub set_headers: Option<Headers>,
    pub remove_headers: Option<Vec<String>>,
    /// `Some(value)` sets the body, `Some(None)` clears it, `None` leaves the
    /// captured body alone — modelling repeater.ts's `'body' in mods` check.
    pub body: Option<Option<String>>,
}

/// A request ready to send: same fields as `rever-ui-repeater`'s
/// `RepeaterRequestSpec`, kept local so a tools crate need not depend on a UI one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestSpec {
    pub url: String,
    pub method: String,
    pub headers: Headers,
    pub body: Option<String>,
}

/// Headers `reqwest` sets itself; a stale captured copy would contradict the
/// request it builds. Everything else (Cookie, Authorization, User-Agent,
/// Referer, Origin, sec-*) is kept — it carries the session the probe needs.
fn client_owned(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "host"
            | "content-length"
            | "connection"
            | "transfer-encoding"
            | "keep-alive"
            | "te"
            | "trailer"
            | "upgrade"
            | "accept-encoding"
    ) || lower.starts_with("proxy-")
}

fn keep_client_safe(headers: &Headers) -> Headers {
    headers
        .iter()
        .filter(|(name, _)| !client_owned(name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// Remove every header whose name case-insensitively equals `name`.
fn remove_ci(headers: &mut Headers, name: &str) {
    let lower = name.to_ascii_lowercase();
    headers.retain(|existing, _| existing.to_ascii_lowercase() != lower);
}

/// The request to start from: a captured one, or — with no `request_id` — a
/// blank GET to `mods.url`, so a URL not yet captured can still be sent.
fn base_request(
    store: &TrafficStore,
    request_id: Option<&str>,
    mods: Option<&Mods>,
) -> Result<StoredRequest, String> {
    if let Some(id) = request_id {
        return store
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown requestId: {id}"));
    }
    if let Some(url) = mods.and_then(|m| m.url.clone()) {
        return Ok(StoredRequest {
            url,
            method: "GET".to_owned(),
            ..StoredRequest::default()
        });
    }
    Err("pass a requestId or modifications.url".to_owned())
}

/// Build a sendable [`RequestSpec`] from a captured request plus overrides.
pub fn build_request_spec(
    store: &TrafficStore,
    request_id: Option<&str>,
    mods: Option<&Mods>,
) -> Result<RequestSpec, String> {
    let stored = base_request(store, request_id, mods)?;

    let url = mods.and_then(|m| m.url.clone()).unwrap_or(stored.url);
    let method = mods
        .and_then(|m| m.method.clone())
        .or(Some(stored.method))
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "GET".to_owned())
        .to_uppercase();

    let mut headers = stored
        .request_headers
        .as_ref()
        .map(keep_client_safe)
        .unwrap_or_default();

    if let Some(remove) = mods.and_then(|m| m.remove_headers.as_ref()) {
        for name in remove {
            remove_ci(&mut headers, name);
        }
    }
    if let Some(set) = mods.and_then(|m| m.set_headers.as_ref()) {
        for (name, value) in set {
            if client_owned(name) {
                continue;
            }
            remove_ci(&mut headers, name);
            headers.insert(name.clone(), value.clone());
        }
    }

    let body = match mods.map(|m| &m.body) {
        Some(Some(body)) => body.clone(),
        Some(None) => None,
        None => stored.request_post_data,
    };

    Ok(RequestSpec {
        url,
        method,
        headers,
        body,
    })
}

/// A `§` marker typed into a page URL is captured percent-encoded (`%c2%a7`,
/// UTF-8), so it never survives into stored traffic and the fuzz tools reported
/// "marker § not found". Decode it back to a literal `§` in the fields it can
/// appear in. Case-insensitive like the TypeScript's `/%c2%a7/gi`: only the hex
/// letters c and a vary, so four spellings cover it.
pub fn restore_marker(spec: RequestSpec) -> RequestSpec {
    RequestSpec {
        url: restore_in(&spec.url),
        method: spec.method,
        headers: spec
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), restore_in(value)))
            .collect(),
        body: spec.body.as_deref().map(restore_in),
    }
}

fn restore_in(s: &str) -> String {
    let mut out = s.to_owned();
    for pat in ["%c2%a7", "%C2%a7", "%c2%A7", "%C2%A7"] {
        if out.contains(pat) {
            out = out.replace(pat, "§");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rever_core::traffic::RequestPatch;

    fn headers(pairs: &[(&str, &str)]) -> Headers {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn store_with(
        id: &str,
        url: &str,
        method: &str,
        req_headers: Option<Headers>,
        post_data: Option<String>,
    ) -> TrafficStore {
        let mut store = TrafficStore::default();
        store.upsert(
            RequestPatch {
                request_id: id.to_owned(),
                url: Some(url.to_owned()),
                method: Some(method.to_owned()),
                request_headers: req_headers,
                request_post_data: post_data,
                ..RequestPatch::default()
            },
            1_700_000_000_000,
        );
        store
    }

    #[test]
    fn 캡처된_요청에서_스펙을_만들고_클라이언트_소유_헤더만_버린다() {
        let store = store_with(
            "r1",
            "https://api.example.com/v1/x",
            "post",
            Some(headers(&[
                ("Host", "api.example.com"),
                ("Content-Length", "7"),
                ("Cookie", "sid=abc"),
                ("Authorization", "Bearer t"),
                ("User-Agent", "firefox"),
            ])),
            Some("{\"a\":1}".to_owned()),
        );
        let spec = build_request_spec(&store, Some("r1"), None).unwrap();
        assert_eq!(spec.method, "POST");
        // Session-carrying headers kept; client-owned ones dropped.
        assert_eq!(spec.headers.get("Cookie").map(String::as_str), Some("sid=abc"));
        assert_eq!(spec.headers.get("User-Agent").map(String::as_str), Some("firefox"));
        assert!(!spec.headers.contains_key("Host"));
        assert!(!spec.headers.contains_key("Content-Length"));
        assert_eq!(spec.body.as_deref(), Some("{\"a\":1}"));
    }

    #[test]
    fn 알_수_없는_requestId는_에러() {
        let store = TrafficStore::default();
        assert!(build_request_spec(&store, Some("nope"), None).is_err());
    }

    #[test]
    fn requestId도_url도_없으면_에러() {
        let store = TrafficStore::default();
        assert!(build_request_spec(&store, None, None).is_err());
    }

    #[test]
    fn mods_url만_주면_빈_GET_요청() {
        let store = TrafficStore::default();
        let mods = Mods {
            url: Some("https://x.test/a".to_owned()),
            ..Mods::default()
        };
        let spec = build_request_spec(&store, None, Some(&mods)).unwrap();
        assert_eq!(spec.method, "GET");
        assert_eq!(spec.url, "https://x.test/a");
        assert!(spec.headers.is_empty());
        assert_eq!(spec.body, None);
    }

    #[test]
    fn mods는_헤더를_대소문자_무시로_교체하고_제거한다() {
        let store = store_with(
            "r1",
            "https://x.test/a",
            "GET",
            Some(headers(&[("X-Token", "old"), ("X-Drop", "1")])),
            None,
        );
        let mods = Mods {
            set_headers: Some(headers(&[("x-token", "new")])),
            remove_headers: Some(vec!["X-DROP".to_owned()]),
            body: Some(Some("payload".to_owned())),
            ..Mods::default()
        };
        let spec = build_request_spec(&store, Some("r1"), Some(&mods)).unwrap();
        assert_eq!(spec.headers.get("x-token").map(String::as_str), Some("new"));
        assert!(!spec.headers.contains_key("X-Token"));
        assert!(!spec.headers.contains_key("X-Drop"));
        assert_eq!(spec.body.as_deref(), Some("payload"));
    }

    #[test]
    fn 인코딩된_마커_c2a7를_리터럴_마커로_되돌린다() {
        let spec = RequestSpec {
            url: "https://x.test/a?q=%c2%a7".to_owned(),
            method: "GET".to_owned(),
            headers: headers(&[("X-Mark", "%C2%A7")]),
            body: Some("b=%c2%A7".to_owned()),
            ..RequestSpec::default()
        };
        let restored = restore_marker(spec);
        assert_eq!(restored.url, "https://x.test/a?q=§");
        assert_eq!(restored.headers.get("X-Mark").map(String::as_str), Some("§"));
        assert_eq!(restored.body.as_deref(), Some("b=§"));
    }
}
