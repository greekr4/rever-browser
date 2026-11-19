//! Port of `src/renderer/src/ai/red-team-mode.ts` — signals that a Red Team scan is running.
//!
//! The permission layer (acp_permission.rs) reads this to auto-approve risky
//! probe tools without prompting — but only for hosts in the scan's scope. The
//! scope grows as the agent scope_set/scope_add's the target's own API hosts
//! (e.g. a backend on another localhost port), tracked here so the auto-approve
//! matches what the agent (and the server-side scope check in mcp/scope.ts)
//! actually enforce.
//!
//! The TypeScript kept `active` and `scope` as module variables; here they are
//! the [`RedTeamMode`] struct, which the host owns and lends to the permission
//! queue.

use std::collections::HashSet;

use url::Url;

/// `/^\d{1,3}(\.\d{1,3}){3}$/` when `first` is `None`; with `Some("127")` the
/// first part must be exactly that, giving `/^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/`.
fn is_dotted_quad(host: &str, first: Option<&str>) -> bool {
    let mut parts = 0;
    host.split('.').all(|part| {
        parts += 1;
        match first {
            Some(first) if parts == 1 => part == first,
            _ => (1..=3).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit()),
        }
    }) && parts == 4
}

fn is_loopback_or_ip(host: &str) -> bool {
    host == "localhost" || is_dotted_quad(host, None)
}

/// Rough eTLD+1 (mirrors mcp/scope.ts) for a bare hostname (no port).
fn reg_domain(host: &str) -> String {
    // 127.0.0.1 / [::1] fold onto localhost — same machine, same app (mirrors
    // the fold in mcp/scope.ts so renderer scope keys match the server's).
    if host == "::1" || host == "[::1]" || is_dotted_quad(host, Some("127")) {
        return "localhost".to_string();
    }
    if host.is_empty() || is_loopback_or_ip(host) {
        return host.to_string();
    }
    let labels: Vec<&str> = host.split('.').collect();
    match labels.as_slice() {
        [.., a, b] if labels.len() > 2 => format!("{a}.{b}"),
        _ => host.to_string(),
    }
}

/// Port-bearing key rule (mirrors scopeKey in mcp/scope.ts): loopback (any
/// spelling, post-fold) and IP/IPv6 literals keep the port; public domains fold
/// to eTLD+1. Test the FOLDED host — raw "[::1]" would fail the plain checks.
fn port_bearing(host: &str, reg: &str) -> bool {
    reg == "localhost" || is_loopback_or_ip(host) || host.contains(':')
}

fn key(host: &str, port: &str) -> String {
    let reg = reg_domain(host);
    if !port.is_empty() && port_bearing(host, &reg) {
        format!("{reg}:{port}")
    } else {
        reg
    }
}

/// Scope key from a full URL. Loopback / IP keep the port (localhost:8080 vs
/// :8090 are distinct apps); public domains fold to eTLD+1. Mirrors scopeKey in
/// mcp/scope.ts on the renderer side. Empty when the URL does not parse.
pub fn site_key(input: &str) -> String {
    let Ok(u) = Url::parse(input) else {
        return String::new();
    };
    // Like `URL.port`, `port()` is empty for the scheme's default port.
    let port = u.port().map(|port| port.to_string()).unwrap_or_default();
    key(u.host_str().unwrap_or_default(), &port)
}

/// Loopback = a disposable target the fence already lets mutate. Mirrors
/// isLoopbackHost in mcp/fence.ts (the renderer cannot import main-process
/// modules), so renderer-side aggression gating matches the fence decision.
// shortcut: lab-marked non-loopback hosts aren't visible here; extend via IPC when a lab scan needs the unsealed prompts.
pub fn is_loopback_host(url: &str) -> bool {
    let Ok(u) = Url::parse(url) else {
        return false;
    };
    let hostname = u.host_str().unwrap_or_default().to_lowercase();
    hostname == "localhost"
        || hostname.ends_with(".localhost")
        || hostname == "::1"
        || hostname == "[::1]"
        || is_dotted_quad(&hostname, Some("127"))
}

/// Same key from a bare host that may carry a port ("localhost:8090").
fn host_key(host_maybe_port: &str) -> String {
    // `/^(.+):(\d+)$/`: the port is the digits after the last colon, and `.`
    // does not cross a line terminator.
    let split = host_maybe_port.rsplit_once(':').filter(|(host, port)| {
        !host.is_empty()
            && !host.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
            && !port.is_empty()
            && port.bytes().all(|b| b.is_ascii_digit())
    });
    match split {
        Some((host, port)) => key(host, port),
        None => key(host_maybe_port, ""),
    }
}

/// `/^[a-z][a-z0-9+.-]*:\/\//i` — scheme characters exclude ':' and '/', so the
/// scheme can only be what precedes the first "://".
fn has_scheme(s: &str) -> bool {
    s.split_once("://").is_some_and(|(scheme, _)| {
        let mut bytes = scheme.bytes();
        bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
            && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-'))
    })
}

/// A host or URL (as passed to scope_set/scope_add) → a scope key.
fn key_of(host_or_url: &str) -> String {
    if has_scheme(host_or_url) {
        site_key(host_or_url)
    } else {
        host_key(host_or_url)
    }
}

#[derive(Clone, Debug, Default)]
pub struct RedTeamMode {
    active: bool,
    scope: HashSet<String>,
}

impl RedTeamMode {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_red_team_scan(&mut self, on: bool, target: Option<&str>) {
        self.active = on;
        self.scope.clear();
        if let Some(target) = target.filter(|_| on) {
            let k = site_key(target);
            if !k.is_empty() {
                self.scope.insert(k);
            }
        }
    }

    /// Union-add hosts the agent brought into scope (scope_set/scope_add). Monotonic
    /// grow: the server enforces replace semantics; the renderer only needs to not
    /// prompt for anything the agent authorized, so a superset is safe.
    pub fn add_red_team_scope<S: AsRef<str>>(&mut self, hosts_or_urls: &[S]) {
        for h in hosts_or_urls {
            let k = key_of(h.as_ref());
            if !k.is_empty() {
                self.scope.insert(k);
            }
        }
    }

    pub fn is_red_team_active(&self) -> bool {
        self.active
    }

    /// True while a scan runs AND the request host is within the tracked scope. A
    /// bare entry ("localhost") covers all its ports; "localhost:8080" only that one.
    pub fn host_in_red_team_scope(&self, host: Option<&str>) -> bool {
        let Some(host) = host.filter(|host| self.active && !host.is_empty()) else {
            return false;
        };
        let k = host_key(host);
        if self.scope.contains(&k) {
            return true;
        }
        let bare = k.split(':').next().unwrap_or_default();
        bare != k && self.scope.contains(bare)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The TypeScript suite reset the module state with `afterEach`; each test
    // here owns a fresh RedTeamMode instead.

    #[test]
    fn 비활성일_때는_어떤_호스트도_범위_밖이다() {
        let mode = RedTeamMode::new();
        assert!(!mode.is_red_team_active());
        assert!(!mode.host_in_red_team_scope(Some("calc.assasup.com")));
    }

    #[test]
    fn 스캔_대상과_같은_etld1_호스트만_범위_안이다() {
        let mut mode = RedTeamMode::new();
        mode.set_red_team_scan(true, Some("https://www.assasup.com/my"));
        assert!(mode.is_red_team_active());
        assert!(mode.host_in_red_team_scope(Some("assasup.com")));
        assert!(mode.host_in_red_team_scope(Some("api.assasup.com")));
        assert!(!mode.host_in_red_team_scope(Some("evil.com")));
        assert!(!mode.host_in_red_team_scope(None));
    }

    #[test]
    fn localhost는_포트까지_일치해야_범위_안이다_8080은_8090이_아니다() {
        let mut mode = RedTeamMode::new();
        mode.set_red_team_scan(true, Some("http://localhost:8080/my"));
        assert!(mode.host_in_red_team_scope(Some("localhost:8080")));
        assert!(!mode.host_in_red_team_scope(Some("localhost:8090")));
        assert!(!mode.host_in_red_team_scope(Some("localhost")));
        assert!(!mode.host_in_red_team_scope(Some("example.com")));
    }

    #[test]
    fn 루프백_표기는_localhost로_접는다_같은_앱이_다른_키로_갈라지지_않게() {
        assert_eq!(
            site_key("http://127.0.0.1:1337/panel/search"),
            "localhost:1337"
        );
        assert_eq!(site_key("http://[::1]:1337/"), "localhost:1337");
        let mut mode = RedTeamMode::new();
        mode.set_red_team_scan(true, Some("http://localhost:1337/"));
        assert!(mode.host_in_red_team_scope(Some("127.0.0.1:1337")));
        assert!(!mode.host_in_red_team_scope(Some("127.0.0.1:8090")));
    }

    #[test]
    fn scope_add로_들어온_api_호스트_다른_포트는_범위_안이_된다() {
        let mut mode = RedTeamMode::new();
        mode.set_red_team_scan(true, Some("http://localhost:8080/my"));
        assert!(!mode.host_in_red_team_scope(Some("localhost:3000")));
        mode.add_red_team_scope(&["http://localhost:3000/"]);
        assert!(mode.host_in_red_team_scope(Some("localhost:3000")));
        // A port that was not added stays out.
        assert!(!mode.host_in_red_team_scope(Some("localhost:8090")));
    }

    #[test]
    fn 중지하면_다시_범위_밖이다() {
        let mut mode = RedTeamMode::new();
        mode.set_red_team_scan(true, Some("https://assasup.com/"));
        mode.set_red_team_scan(false, None);
        assert!(!mode.is_red_team_active());
        assert!(!mode.host_in_red_team_scope(Some("assasup.com")));
    }

    // The cases below are not in the TypeScript test file.

    #[test]
    fn site_key는_공개_도메인의_포트를_버리고_ip는_포트를_남긴다() {
        assert_eq!(site_key("https://api.example.com:8443/x"), "example.com");
        assert_eq!(site_key("http://10.0.0.5:8080/"), "10.0.0.5:8080");
        assert_eq!(site_key("http://[2001:db8::1]:8080/"), "[2001:db8::1]:8080");
        // The scheme's default port reads as no port, like URL.port.
        assert_eq!(site_key("http://localhost:80/"), "localhost");
        assert_eq!(site_key("not a url"), "");
        assert_eq!(site_key(""), "");
    }

    #[test]
    fn 포트_없는_범위_항목은_그_호스트의_모든_포트를_덮는다() {
        let mut mode = RedTeamMode::new();
        mode.set_red_team_scan(true, Some("http://localhost/"));
        assert!(mode.host_in_red_team_scope(Some("localhost:8080")));
        assert!(mode.host_in_red_team_scope(Some("127.0.0.1:9")));
        assert!(!mode.host_in_red_team_scope(Some("")));
    }

    #[test]
    fn scope_add는_스킴_없는_호스트도_받고_깨진_입력은_버린다() {
        let mut mode = RedTeamMode::new();
        mode.set_red_team_scan(true, None);
        mode.add_red_team_scope(&["api.example.com:8443", "localhost:3000", "", "http://"]);
        assert!(mode.host_in_red_team_scope(Some("cdn.example.com")));
        assert!(mode.host_in_red_team_scope(Some("localhost:3000")));
        assert!(!mode.host_in_red_team_scope(Some("localhost:3001")));
        // A scan that is switched off forgets its scope.
        mode.set_red_team_scan(false, Some("http://localhost:3000/"));
        assert!(!mode.host_in_red_team_scope(Some("localhost:3000")));
        assert!(mode.scope.is_empty());
    }

    #[test]
    fn is_loopback_host는_루프백_표기만_참이다() {
        assert!(is_loopback_host("http://localhost:1337/"));
        assert!(is_loopback_host("http://app.LOCALHOST/"));
        assert!(is_loopback_host("http://127.9.9.9/"));
        assert!(is_loopback_host("http://[::1]:8080/"));
        assert!(!is_loopback_host("http://10.0.0.5/"));
        assert!(!is_loopback_host("https://localhost.example.com/"));
        assert!(!is_loopback_host("localhost:1337"));
        assert!(!is_loopback_host("nope"));
    }

    // Checked against the TypeScript with bun: the `/^(.+):(\d+)$/` and scheme
    // regexes on inputs where a hand-written matcher could drift.
    #[test]
    fn 호스트_키_정규식의_경계_입력을_타입스크립트와_같게_본다() {
        let in_scope = |entry: &str, host: &str| {
            let mut mode = RedTeamMode::new();
            mode.set_red_team_scan(true, None);
            mode.add_red_team_scope(&[entry]);
            mode.host_in_red_team_scope(Some(host))
        };
        // The scheme test is case-insensitive; a scheme cannot start with a digit,
        // so that entry is keyed as a bare host.
        assert!(in_scope("HTTP://LOCALHOST:8080", "localhost:8080"));
        assert!(!in_scope("HTTP://LOCALHOST:8080", "localhost:8081"));
        assert!(!in_scope("1http://localhost:5/", "localhost:5"));
        assert!(in_scope("1http://localhost:5/", "1http://localhost:5/"));
        // Only the digits after the last colon are a port, and a public name drops it.
        assert!(in_scope("a:80:90", "a:80:90"));
        assert!(!in_scope("a:80:90", "a"));
        assert!(!in_scope("a:80:90", "a:80"));
        // A line break or a non-ASCII digit means "no port": the whole string is the host.
        assert!(!in_scope("localhost:8080", "localhost:8080\n"));
        assert!(!in_scope("localhost:8080", "a\n:8080"));
        assert!(!in_scope("localhost:8080", "localhost:８０８０"));
        // Every loopback spelling folds onto localhost; a short IPv4 form does not.
        assert!(in_scope("localhost:8080", "[::1]:8080"));
        assert!(in_scope("localhost:8080", "::1:8080"));
        assert!(in_scope("localhost:8080", "127.0.0.1:8080"));
        assert!(!in_scope("localhost:8080", "127.1:8080"));
        // An IP keeps its port, so the bare IP is a different key.
        assert!(in_scope("10.0.0.5:8080", "10.0.0.5:8080"));
        assert!(!in_scope("10.0.0.5:8080", "10.0.0.5"));
        assert!(in_scope("http://[2001:db8::1]:8080/", "[2001:db8::1]:8080"));
        assert!(!in_scope(
            "http://[2001:db8::1]:8080/",
            "[2001:db8::1]:8081"
        ));
        assert!(in_scope("api.example.com:8443", "x.example.com:1"));
        assert!(!in_scope("api.example.com:8443", "example.com."));
    }
}
