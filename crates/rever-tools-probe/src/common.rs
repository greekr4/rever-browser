//! Shared probe helpers the TypeScript spread across `repeater.ts`,
//! `ssti_probe.ts` (`formSlotToMarker`), `cmd_probe.ts` (`urlSlotToMarker`) and
//! `nosqli_probe.ts` (`injectKv`/`injectJson`). Kept in one place so each probe
//! file holds only its own payloads and detection, and the slot-marking logic
//! can't drift between them.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use regex::Regex;
use rever_tools_http::RequestSpec;
use serde_json::Value;

/// `encodeURIComponent` keeps `A-Za-z0-9` and `- _ . ! ~ * ' ( )`; everything
/// else is percent-encoded. `NON_ALPHANUMERIC` encodes all of those, so remove
/// the nine unreserved marks.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

/// JavaScript's `encodeURIComponent`.
pub fn encode_uri_component(s: &str) -> String {
    utf8_percent_encode(s, COMPONENT).to_string()
}

/// Distinctive factors for the arithmetic payloads — a product of two of these
/// is never a value (like 49) a page might print on its own.
pub const PRIMES: [u32; 12] = [101, 103, 107, 109, 113, 127, 131, 137, 139, 149, 151, 157];

/// A process-wide entropy source good enough for nonce / prime selection (not
/// cryptographic): the clock mixed with a monotonically advancing counter, so
/// two calls in the same nanosecond still differ.
fn entropy() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    nanos ^ COUNTER.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
}

/// `'z' + Math.random().toString(36).slice(2, 7) + 'z'`: a 5-char base-36 blob
/// wrapped in `z…z`, so a coincidental match of the result on a page is
/// essentially impossible.
pub fn random_nonce() -> String {
    let mut v = entropy();
    let mut s = String::with_capacity(7);
    s.push('z');
    for _ in 0..5 {
        s.push(char::from_digit((v % 36) as u32, 36).unwrap_or('0'));
        v /= 36;
    }
    s.push('z');
    s
}

/// A random prime from [`PRIMES`] (the default arithmetic factor).
pub fn pick_prime() -> u32 {
    PRIMES[(entropy() as usize) % PRIMES.len()]
}

/// Whether the request carries a `§` injection marker anywhere.
pub fn has_marker(spec: &RequestSpec) -> bool {
    spec.url.contains('§')
        || spec.body.as_deref().unwrap_or_default().contains('§')
        || spec.headers.values().any(|value| value.contains('§'))
}

/// Whether the body is `application/x-www-form-urlencoded` (payloads must then
/// be URL-encoded, or template/shell chars corrupt the field).
pub fn is_form_urlencoded(spec: &RequestSpec) -> bool {
    spec.headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .is_some_and(|(_, value)| {
            value
                .to_ascii_lowercase()
                .contains("application/x-www-form-urlencoded")
        })
}

/// Substitute every `§` marker with `payload`: URL-encoded in the url (and in a
/// form body), raw in headers (and a non-form body). The shape sqli/cmd/ssti
/// share.
pub fn apply_payload(base: &RequestSpec, payload: &str, form_body: bool) -> RequestSpec {
    let encoded = encode_uri_component(payload);
    RequestSpec {
        url: base.url.replace('§', &encoded),
        method: base.method.clone(),
        headers: base
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), value.replace('§', payload)))
            .collect(),
        body: base
            .body
            .as_deref()
            .map(|body| body.replace('§', if form_body { &encoded } else { payload })),
    }
}

/// Rewrite one query param of a URL to the `§` marker; append it if absent.
pub fn url_slot_to_marker(url: &str, param: &str) -> String {
    let encoded = encode_uri_component(param);
    let re = Regex::new(&format!(r"([?&]){}=[^&]*", regex::escape(param))).expect("valid");
    if re.is_match(url) {
        return re
            .replace(url, |caps: &regex::Captures| {
                format!("{}{}=§", &caps[1], encoded)
            })
            .into_owned();
    }
    format!(
        "{url}{}{encoded}=§",
        if url.contains('?') { "&" } else { "?" }
    )
}

/// Rewrite one form field of a URL-encoded body to the `§` marker (append if
/// absent), then fill any empty sibling field with a benign dummy so required-
/// field validation doesn't reject the request before the payload reaches its
/// sink.
pub fn form_slot_to_marker(body: &str, param: &str) -> String {
    let encoded = encode_uri_component(param);
    let re = Regex::new(&format!(r"(^|&){}=[^&]*", regex::escape(param))).expect("valid");
    let marked = if re.is_match(body) {
        re.replace(body, |caps: &regex::Captures| {
            format!("{}{}=§", &caps[1], encoded)
        })
        .into_owned()
    } else if body.is_empty() {
        format!("{encoded}=§")
    } else {
        format!("{body}&{encoded}=§")
    };
    fill_empty_fields(&marked)
}

/// Replace each `name=` segment with an empty value by `name=reverprobe`, where
/// `name` matches `[A-Za-z_][\w.-]*`. Done by splitting on `&` (not a lookahead
/// regex, which the regex crate lacks) so consecutive empties (`a=&b=`) all fill.
fn fill_empty_fields(body: &str) -> String {
    let name_re = Regex::new(r"^[A-Za-z_][\w.-]*=$").expect("valid");
    body.split('&')
        .map(|segment| {
            if name_re.is_match(segment) {
                format!("{segment}reverprobe")
            } else {
                segment.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Rewrite a `key=val` grammar string (query or form body): replace the target
/// param's pair with `replacement`, or append it if absent. A prefix-sharing
/// sibling (`user` vs `username`) is left alone.
pub fn inject_kv(s: &str, param: &str, replacement: &str) -> String {
    let re = Regex::new(&format!(
        r"(^|&){}(\[[^\]]*\])?=[^&]*",
        regex::escape(param)
    ))
    .expect("valid");
    if re.is_match(s) {
        return re
            .replace(s, |caps: &regex::Captures| {
                format!("{}{replacement}", &caps[1])
            })
            .into_owned();
    }
    if s.is_empty() {
        replacement.to_owned()
    } else {
        format!("{s}&{replacement}")
    }
}

/// Set one key of a JSON object body to `value` and reserialize. Empty body =
/// `{}`. Errors if the body isn't a JSON object.
pub fn inject_json(body: &str, param: &str, value: Value) -> Result<String, String> {
    let text = if body.is_empty() { "{}" } else { body };
    let mut parsed: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let object = parsed
        .as_object_mut()
        .ok_or_else(|| "body is not a JSON object".to_owned())?;
    object.insert(param.to_owned(), value);
    serde_json::to_string(&parsed).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rever_core::traffic::Headers;
    use serde_json::json;

    #[test]
    fn encode_uri_component는_비예약문자만_남긴다() {
        assert_eq!(encode_uri_component("a b&c=d"), "a%20b%26c%3Dd");
        // The nine unreserved marks pass through.
        assert_eq!(encode_uri_component("-_.!~*'()"), "-_.!~*'()");
        assert_eq!(encode_uri_component("{{7*7}}"), "%7B%7B7*7%7D%7D");
    }

    #[test]
    fn random_nonce는_z로_감싼_7글자() {
        let n = random_nonce();
        assert_eq!(n.len(), 7);
        assert!(n.starts_with('z') && n.ends_with('z'));
        // Two calls differ.
        assert_ne!(random_nonce(), random_nonce());
    }

    #[test]
    fn url_slot은_쿼리_파라미터_값을_마커로_바꾼다() {
        assert_eq!(
            url_slot_to_marker("http://x/?format=%Y-%m-%d&b=2", "format"),
            "http://x/?format=§&b=2"
        );
        assert_eq!(
            url_slot_to_marker("http://x/?a=1&fmt=1", "fmt"),
            "http://x/?a=1&fmt=§"
        );
    }

    #[test]
    fn url_slot은_없으면_추가하고_정규식_특수문자를_이스케이프한다() {
        assert_eq!(
            url_slot_to_marker("http://x/", "format"),
            "http://x/?format=§"
        );
        assert_eq!(
            url_slot_to_marker("http://x/?a=1", "format"),
            "http://x/?a=1&format=§"
        );
        assert_eq!(
            url_slot_to_marker("http://x/?f.mt=1", "f.mt"),
            "http://x/?f.mt=§"
        );
    }

    #[test]
    fn form_slot은_필드값을_마커로_바꾸거나_추가한다() {
        assert_eq!(form_slot_to_marker("text=hello+world", "text"), "text=§");
        assert_eq!(
            form_slot_to_marker("a=1&text=%7B%7B7%2A7%7D%7D&b=2", "text"),
            "a=1&text=§&b=2"
        );
        assert_eq!(form_slot_to_marker("", "text"), "text=§");
        assert_eq!(form_slot_to_marker("a=1", "text"), "a=1&text=§");
        assert_eq!(form_slot_to_marker("q.x=1", "q.x"), "q.x=§");
    }

    #[test]
    fn form_slot은_빈_형제_필드를_더미로_채운다() {
        assert_eq!(
            form_slot_to_marker("username=&password=", "username"),
            "username=§&password=reverprobe"
        );
        assert_eq!(
            form_slot_to_marker("a=&b=&c=1", "c"),
            "a=reverprobe&b=reverprobe&c=§"
        );
        assert_eq!(
            form_slot_to_marker("user=admin&pass=", "user"),
            "user=§&pass=reverprobe"
        );
    }

    #[test]
    fn inject_kv는_대상을_교체하거나_덧붙이고_접두사_형제는_건드리지_않는다() {
        assert_eq!(
            inject_kv("username=rtuser&password=x", "username", "username[$ne]=zz"),
            "username[$ne]=zz&password=x"
        );
        assert_eq!(
            inject_kv("username[$eq]=a&password=x", "username", "username[$ne]=zz"),
            "username[$ne]=zz&password=x"
        );
        assert_eq!(
            inject_kv("password=x", "username", "username[$ne]=zz"),
            "password=x&username[$ne]=zz"
        );
        assert_eq!(
            inject_kv("user=a&username=b", "user", "user[$ne]=zz"),
            "user[$ne]=zz&username=b"
        );
    }

    #[test]
    fn inject_json은_대상_키를_연산자_객체나_스칼라로_세팅한다() {
        let out = inject_json(
            "{\"username\":\"a\",\"password\":\"b\"}",
            "username",
            json!({"$ne":"zz"}),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap(),
            json!({"username":{"$ne":"zz"},"password":"b"})
        );
        let out = inject_json("{\"username\":\"a\"}", "username", json!("zz'\"//")).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap(),
            json!({"username":"zz'\"//"})
        );
    }

    #[test]
    fn apply_payload는_url과_폼바디는_인코딩_헤더는_raw로_치환한다() {
        let base = RequestSpec {
            url: "http://x/?q=§".to_owned(),
            method: "POST".to_owned(),
            headers: [("X-H".to_owned(), "§".to_owned())]
                .into_iter()
                .collect::<Headers>(),
            body: Some("f=§".to_owned()),
        };
        let out = apply_payload(&base, "a b", true);
        assert_eq!(out.url, "http://x/?q=a%20b");
        assert_eq!(out.headers.get("X-H").map(String::as_str), Some("a b"));
        assert_eq!(out.body.as_deref(), Some("f=a%20b"));
        // Non-form body stays raw.
        let out = apply_payload(&base, "a b", false);
        assert_eq!(out.body.as_deref(), Some("f=a b"));
    }
}
