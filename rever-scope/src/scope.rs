//! Port of `src/main/mcp/scope.ts` — the session target-scope guardrail.
//!
//! When a scope is set, active tools (navigate / replay / repeater / intruder /
//! burst) refuse targets outside it, so an autonomous run can't accidentally hit
//! un-authorized third-party hosts.
//!
//! Granularity: entries are stored as registrable domains (eTLD+1, approximated),
//! so adding "example.com" also permits api.example.com / cdn.example.com — the
//! target's own API and CDN subdomains — without over-blocking.
//!
//! Opt-in: an empty allowlist means NO enforcement (every host allowed), which
//! preserves the prior behavior until the user/agent explicitly sets a scope.

use std::sync::Mutex;

use serde::Serialize;
use url::Url;

use crate::{js_trim, lock};

// Minimal multi-label public suffixes so eTLD+1 is right for the common ccTLDs.
const TWO_LEVEL_SUFFIXES: &[&str] = &[
    "co.uk", "org.uk", "ac.uk", "gov.uk", "me.uk", "co.kr", "or.kr", "ne.kr", "go.kr", "re.kr",
    "pe.kr", "co.jp", "or.jp", "ne.jp", "go.jp", "com.au", "net.au", "org.au", "com.br", "com.cn",
    "com.tw", "com.hk", "com.sg", "co.in", "co.nz",
];

/// Tools whose side effects are destructive or high-volume enough that they
/// must always prompt the user, regardless of the global auto-approve setting.
///
/// The only part of `src/shared/risky-tools.ts` that scope.ts uses; the rest of
/// that file (permission-request matching) is not ported here.
pub const ALWAYS_RISKY_TOOLS: [&str; 6] = [
    "intruder_run",
    "burst_send",
    "payload_probe",
    "crlf_test",
    "path_probe",
    "lfi_probe",
];

static ALLOWED: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ScopeState {
    pub domains: Vec<String>,
    pub enabled: bool,
}

/// `/^[a-z][a-z0-9+.-]*:\/\//` — scheme characters exclude ':' and '/', so the
/// scheme can only be what precedes the first "://".
fn has_scheme(s: &str) -> bool {
    s.split_once("://").is_some_and(|(scheme, _)| {
        let mut bytes = scheme.bytes();
        bytes.next().is_some_and(|b| b.is_ascii_lowercase())
            && bytes.all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'+' | b'.' | b'-')
            })
    })
}

/// `/^[a-z0-9-]+(\.[a-z0-9-]+)*$/` — rejects garbage (spaces, empty labels).
fn is_host_labels(host: &str) -> bool {
    host.split('.').all(|label| {
        !label.is_empty()
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

/// `/^\d{1,3}(\.\d{1,3}){3}$/`
fn is_dotted_quad(hostname: &str) -> bool {
    let mut parts = 0;
    hostname.split('.').all(|part| {
        parts += 1;
        (1..=3).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit())
    }) && parts == 4
}

/// Full 127.0.0.0/8 only — anchored so "127.0.0.1.evil.com" (a public DNS name
/// that merely starts with 127.) is NOT treated as loopback.
pub(crate) fn is_loopback_ipv4(hostname: &str) -> bool {
    hostname.starts_with("127.") && is_dotted_quad(hostname)
}

/// `new URL(s).hostname`, with brackets around IPv6 as in JavaScript.
fn url_hostname(url: &Url) -> Option<&str> {
    url.host_str().filter(|h| !h.is_empty())
}

pub fn hostname_of(input: &str) -> Option<String> {
    let s = js_trim(input).to_lowercase();
    if s.is_empty() {
        return None;
    }
    if has_scheme(&s) {
        return Url::parse(&s)
            .ok()
            .and_then(|u| url_hostname(&u).map(str::to_string));
    }
    // bare host, optionally with wildcard prefix, path, or port
    let rest = s.strip_prefix("*.").unwrap_or(&s);
    let host = rest.split(['/', ':']).next().unwrap_or("");
    is_host_labels(host).then(|| host.to_string())
}

// IP literals have no registrable domain — folding "10.0.0.1" to its last two
// labels ("0.1") would put every x.x.0.1 address in scope.
fn is_ip_literal(hostname: &str) -> bool {
    is_dotted_quad(hostname) || hostname.contains(':')
}

pub fn registrable_domain(hostname: &str) -> String {
    // 127.0.0.1 and [::1] ARE localhost — apps redirect/link between the two
    // spellings freely, so all loopback spellings fold onto ONE key: a scope,
    // finding origin, or export keyed "localhost:1337" must also cover the
    // 127.0.0.1 spelling of the same app (a Percetron scan lost its NoSQLi
    // finding to exactly this split).
    if hostname == "::1" || hostname == "[::1]" || is_loopback_ipv4(hostname) {
        return "localhost".to_string();
    }
    if is_ip_literal(hostname) {
        return hostname.to_string();
    }
    let parts: Vec<&str> = hostname.split('.').filter(|p| !p.is_empty()).collect();
    if parts.len() <= 2 {
        return hostname.to_string();
    }
    let last = |n: usize| parts.get(parts.len() - n..).unwrap_or(&parts).join(".");
    let last2 = last(2);
    if TWO_LEVEL_SUFFIXES.contains(&last2.as_str()) {
        return last(3);
    }
    last2
}

/// The scope membership key. Loopback / IP share one hostname across many apps
/// on different ports (localhost:8080 vs :8090), so for those the port is
/// load-bearing and kept; public domains fold to their registrable domain as
/// before. Returns None for an unparseable input.
pub fn scope_key(input: &str) -> Option<String> {
    let s = js_trim(input).to_lowercase();
    if s.is_empty() {
        return None;
    }
    let (hostname, port) = if has_scheme(&s) {
        let u = Url::parse(&s).ok()?;
        let port = u.port().map(|p| p.to_string()).unwrap_or_default();
        (url_hostname(&u)?.to_string(), port)
    } else {
        let rest = s.strip_prefix("*.").unwrap_or(&s);
        let hp = rest.split('/').next().unwrap_or("");
        // `/^(.+?):(\d+)$/` — the port is all digits, so it follows the last colon.
        let (hostname, port) = match hp.rsplit_once(':') {
            Some((h, p))
                if !h.is_empty() && !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) =>
            {
                (h, p)
            }
            _ => (hp, ""),
        };
        if !is_host_labels(hostname) {
            return None;
        }
        (hostname.to_string(), port.to_string())
    };
    let reg = registrable_domain(&hostname);
    if !port.is_empty() && (hostname == "localhost" || is_ip_literal(&hostname)) {
        return Some(format!("{reg}:{port}"));
    }
    Some(reg)
}

fn normalize<S: AsRef<str>>(hosts: impl IntoIterator<Item = S>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for h in hosts {
        if let Some(key) = scope_key(h.as_ref())
            && !out.contains(&key)
        {
            out.push(key);
        }
    }
    out
}

/// Pure membership test. Empty allowlist = allow all.
pub fn host_in_scope<S: AsRef<str>>(hostname: &str, allowed_domains: &[S]) -> bool {
    if allowed_domains.is_empty() {
        return true;
    }
    let reg = registrable_domain(hostname);
    allowed_domains.iter().any(|d| d.as_ref() == reg)
}

pub fn get_scope() -> ScopeState {
    let allowed = lock(&ALLOWED);
    ScopeState {
        domains: allowed.clone(),
        enabled: !allowed.is_empty(),
    }
}

pub fn set_scope<S: AsRef<str>>(hosts: &[S]) -> Vec<String> {
    let mut allowed = lock(&ALLOWED);
    *allowed = normalize(hosts);
    allowed.clone()
}

pub fn add_scope_hosts<S: AsRef<str>>(hosts: &[S]) -> Vec<String> {
    let mut allowed = lock(&ALLOWED);
    let merged = normalize(
        allowed
            .iter()
            .map(String::as_str)
            .chain(hosts.iter().map(AsRef::as_ref)),
    );
    *allowed = merged;
    allowed.clone()
}

pub fn clear_scope() {
    lock(&ALLOWED).clear();
}

/// Returns a human-readable block message when `url`'s host is outside an active
/// scope, or None when allowed (or when no scope is set, or url can't be parsed
/// — an unparseable target is not blocked, to avoid false positives).
pub fn scope_block_for_url(url: Option<&str>) -> Option<String> {
    let url = url.filter(|u| !u.is_empty())?;
    let allowed = lock(&ALLOWED);
    if allowed.is_empty() {
        return None;
    }
    let key = scope_key(url)?;
    // Exact key, or a port-agnostic scope entry (bare "localhost"/"10.0.0.1"
    // covers all ports of that host; "localhost:8080" covers only that port).
    let bare = key.split(':').next().unwrap_or("");
    if allowed.contains(&key) || (bare != key && allowed.iter().any(|a| a == bare)) {
        return None;
    }
    Some(format!(
        "Out of scope: {key} is not within the allowed scope [{}]. Add it with scope_add if this target is intended.",
        allowed.join(", ")
    ))
}

/// A one-line nudge on active-attack tool results while no scope is enforced.
pub fn scope_off_warning(tool_name: &str) -> Option<String> {
    if !lock(&ALLOWED).is_empty() || !ALWAYS_RISKY_TOOLS.contains(&tool_name) {
        return None;
    }
    Some(format!(
        "[scope off] {tool_name} ran with no target scope — any host is allowed. Pin the authorized target with scope_set before further probing."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isolated;

    fn out_of_scope(url: &str) -> bool {
        scope_block_for_url(Some(url)).is_some_and(|m| m.contains("Out of scope"))
    }

    #[test]
    fn url_bare_host_와일드카드_포트를_모두_호스트명으로_정규화한다() {
        assert_eq!(
            hostname_of("https://api.example.com/v3/order").as_deref(),
            Some("api.example.com")
        );
        assert_eq!(hostname_of("Example.COM").as_deref(), Some("example.com"));
        assert_eq!(hostname_of("*.example.com").as_deref(), Some("example.com"));
        assert_eq!(hostname_of("x.io:8443/path").as_deref(), Some("x.io"));
        assert_eq!(hostname_of(""), None);
    }

    #[test]
    fn 일반_ip_리터럴은_접지_않고_그대로_둔다() {
        assert_eq!(registrable_domain("10.0.0.1"), "10.0.0.1");
        assert_eq!(registrable_domain("192.168.0.1"), "192.168.0.1");
    }

    #[test]
    fn 루프백_표기는_localhost_하나로_접는다() {
        assert_eq!(registrable_domain("127.0.0.1"), "localhost");
        assert_ne!(registrable_domain("127.0.0.1"), "10.0.0.1");
        assert_eq!(registrable_domain("[::1]"), "localhost");
        assert_eq!(
            scope_key("http://127.0.0.1:1337/x").as_deref(),
            Some("localhost:1337")
        );
        assert_eq!(
            scope_key("http://[::1]:1337/x").as_deref(),
            Some("localhost:1337")
        );
    }

    #[test]
    fn localhost_스코프는_127_0_0_1_표기도_같은_앱으로_묶는다() {
        let _clean = isolated(clear_scope);
        set_scope(&["localhost:8080"]);
        assert_eq!(scope_block_for_url(Some("http://127.0.0.1:8080/my")), None);
        assert_eq!(scope_block_for_url(Some("http://localhost:8080/my")), None);
        assert!(out_of_scope("http://127.0.0.1:8090/x"));
    }

    #[test]
    fn 다른_ip는_scope_밖이다() {
        let _clean = isolated(clear_scope);
        set_scope(&["10.0.0.1"]);
        assert_eq!(get_scope().domains, ["10.0.0.1"]);
        assert!(out_of_scope("http://127.0.0.1:8777/x"));
        assert!(out_of_scope("http://192.168.0.1/"));
        assert_eq!(scope_block_for_url(Some("http://10.0.0.1:8080/a")), None);
    }

    #[test]
    fn 포트가_지정되면_그_포트만_범위_안이다() {
        let _clean = isolated(clear_scope);
        set_scope(&["localhost:8080"]);
        assert_eq!(get_scope().domains, ["localhost:8080"]);
        assert_eq!(scope_block_for_url(Some("http://localhost:8080/my")), None);
        assert!(out_of_scope("http://localhost:8090/activity"));
    }

    #[test]
    fn 포트_없이_localhost만_주면_모든_포트를_허용한다() {
        let _clean = isolated(clear_scope);
        set_scope(&["localhost"]);
        assert_eq!(scope_block_for_url(Some("http://localhost:8080/a")), None);
        assert_eq!(scope_block_for_url(Some("http://localhost:9999/b")), None);
    }

    #[test]
    fn 서브도메인을_등록가능도메인으로_접는다() {
        assert_eq!(registrable_domain("api.example.com"), "example.com");
        assert_eq!(registrable_domain("a.b.c.example.com"), "example.com");
        assert_eq!(registrable_domain("example.com"), "example.com");
    }

    #[test]
    fn 두_단계_cctld를_처리한다() {
        assert_eq!(registrable_domain("shop.example.co.kr"), "example.co.kr");
        assert_eq!(registrable_domain("www.example.co.uk"), "example.co.uk");
    }

    #[test]
    fn 빈_허용목록은_모든_호스트를_통과시킨다() {
        assert!(host_in_scope::<&str>("anything.com", &[]));
    }

    #[test]
    fn 같은_등록가능도메인의_서브도메인은_허용된다() {
        assert!(host_in_scope("api.example.com", &["example.com"]));
        assert!(host_in_scope("cdn.example.com", &["example.com"]));
    }

    #[test]
    fn 다른_도메인은_거부된다() {
        assert!(!host_in_scope("evil.com", &["example.com"]));
        assert!(!host_in_scope("ads.doubleclick.net", &["example.com"]));
    }

    #[test]
    fn 입력을_등록가능도메인으로_저장하고_중복을_제거한다() {
        let _clean = isolated(clear_scope);
        set_scope(&["https://www.example.com/x", "api.example.com", "other.io"]);
        assert_eq!(
            get_scope(),
            ScopeState {
                domains: vec!["example.com".to_string(), "other.io".to_string()],
                enabled: true
            }
        );
    }

    #[test]
    fn 스코프_미설정이면_차단하지_않는다() {
        let _clean = isolated(clear_scope);
        assert_eq!(scope_block_for_url(Some("https://anything.com/a")), None);
    }

    #[test]
    fn 스코프_내_호스트는_통과_밖은_메시지를_반환한다() {
        let _clean = isolated(clear_scope);
        set_scope(&["example.com"]);
        assert_eq!(
            scope_block_for_url(Some("https://api.example.com/v3")),
            None
        );
        let blocked = scope_block_for_url(Some("https://evil.com/steal")).unwrap_or_default();
        assert!(blocked.contains("Out of scope"));
        assert!(blocked.contains("evil.com"));
    }

    #[test]
    fn add_scope_hosts로_추가하면_통과한다() {
        let _clean = isolated(clear_scope);
        set_scope(&["example.com"]);
        add_scope_hosts(&["https://partner.io"]);
        assert_eq!(scope_block_for_url(Some("https://partner.io/cb")), None);
    }

    #[test]
    fn 파싱_불가_url은_차단하지_않는다() {
        let _clean = isolated(clear_scope);
        set_scope(&["example.com"]);
        assert_eq!(scope_block_for_url(Some("not a url")), None);
        assert_eq!(scope_block_for_url(None), None);
    }

    #[test]
    fn scope가_꺼져_있으면_공격성_툴_결과에_경고를_단다() {
        let _clean = isolated(clear_scope);
        assert!(scope_off_warning("intruder_run").is_some_and(|w| w.contains("scope_set")));
    }

    #[test]
    fn scope가_켜져_있으면_경고하지_않는다() {
        let _clean = isolated(clear_scope);
        set_scope(&["example.com"]);
        assert_eq!(scope_off_warning("intruder_run"), None);
    }

    #[test]
    fn 공격성_툴이_아니면_scope가_꺼져_있어도_경고하지_않는다() {
        let _clean = isolated(clear_scope);
        assert_eq!(scope_off_warning("list_requests"), None);
    }
}
