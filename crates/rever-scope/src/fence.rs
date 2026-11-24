//! Port of `src/main/mcp/fence.ts` — the capture-not-send fence, P1 of
//! docs/feature-plan-red-team-ui-crawl.md.
//!
//! While a Red Team UI crawl holds the fence, a browser-originated request that
//! would change server state is answered with a synthetic 204 and recorded,
//! instead of reaching the server. The endpoint's shape is already captured by
//! Network.* (requestWillBeSent fires before Fetch.requestPaused), which is the
//! whole point; the server side stays untouched. Idempotent reads pass through.
//!
//! Blocked = any non-idempotent method, or an auth-invalidation path (logout /
//! revoke) at any method so a stray click can't log the scan out mid-run. Scope
//! is enforced by the caller — the decision here is method/path only. The hold
//! is ref-counted so parallel per-tab crawls can each take it; it lifts only
//! when the last releases.

use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::lock;
use crate::scope::{is_loopback_ipv4, scope_key};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FenceReason {
    Mutation,
    AuthInvalidation,
    /// The host the scope admitted resolves only to addresses no outbound
    /// request may reach (private ranges, cloud metadata, ...).
    ForbiddenIp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FenceBlock {
    pub method: String,
    pub url: String,
    /// Epoch milliseconds.
    pub ts: u64,
    pub reason: FenceReason,
}

const BLOCKS_CAP: usize = 1000;

struct State {
    holders: usize,
    blocks: VecDeque<FenceBlock>,
    // Extra lab targets — non-loopback hosts the user owns and has explicitly
    // marked safe to mutate (e.g. an authorized staging domain), via the
    // lab_target tool. Stored as scope_key()s so loopback-style entries are
    // port-specific; a bare host opens all its ports. Opt-in and off by default.
    lab_hosts: Vec<String>,
}

static STATE: Mutex<State> = Mutex::new(State {
    holders: 0,
    blocks: VecDeque::new(),
    lab_hosts: Vec::new(),
});

/// Loopback = your own machine, so mutating it during a red-team scan is safe and
/// expected (local dev app, a localhost CTF/benchmark). The fence steps aside for
/// loopback BY DEFAULT, so the red team CONFIRMS mutation-only vulns (SSTI,
/// SQLi/cmd-injection via POST, auth bypass) out of the box instead of stopping at
/// "candidate". Every non-loopback host stays fenced unless explicitly marked.
pub fn is_loopback_host(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let hostname = parsed.host_str().unwrap_or("").to_lowercase();
    hostname == "localhost"
        || hostname.ends_with(".localhost")
        || hostname == "::1"
        || hostname == "[::1]"
        || is_loopback_ipv4(&hostname)
}

pub fn set_lab_hosts<S: AsRef<str>>(hosts: &[S]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for h in hosts {
        if let Some(key) = scope_key(h.as_ref())
            && !out.contains(&key)
        {
            out.push(key);
        }
    }
    lock(&STATE).lab_hosts = out.clone();
    out
}

pub fn get_lab_hosts() -> Vec<String> {
    lock(&STATE).lab_hosts.clone()
}

pub fn clear_lab_hosts() {
    lock(&STATE).lab_hosts.clear();
}

/// Whether `url`'s host was marked a disposable lab target (fence steps aside).
pub fn is_lab_host(url: &str) -> bool {
    let state = lock(&STATE);
    if state.lab_hosts.is_empty() {
        return false;
    }
    let Some(key) = scope_key(url) else {
        return false;
    };
    let bare = key.split(':').next().unwrap_or("");
    state.lab_hosts.contains(&key) || (bare != key && state.lab_hosts.iter().any(|h| h == bare))
}

const MUTATION_METHODS: [&str; 4] = ["POST", "PUT", "PATCH", "DELETE"];

// Session-ending paths, blocked at any method. ASCII-only case folding, as in
// JavaScript's /i without the u flag.
static AUTH_INVALIDATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i-u)(?:^|[/._-])(?:logout|log-?off|sign-?out|signoff|revoke|deauth|unauth)(?:[/._?&=-]|$)",
    )
    .expect("built-in pattern compiles")
});

pub fn acquire_fence() {
    let mut state = lock(&STATE);
    if state.holders == 0 {
        state.blocks.clear(); // fresh ledger per crawl session
    }
    state.holders += 1;
}

pub fn release_fence() {
    let mut state = lock(&STATE);
    state.holders = state.holders.saturating_sub(1);
}

pub fn is_fence_active() -> bool {
    lock(&STATE).holders > 0
}

/// Whether this request mutates server state (so the fence must hold it).
pub fn fence_reason(method: &str, url: &str) -> Option<FenceReason> {
    // Disposable lab target (loopback by default, or explicitly marked): allow
    // mutations through so the red team can confirm the vuln.
    if is_loopback_host(url) || is_lab_host(url) {
        return None;
    }
    let parsed = Url::parse(url).ok();
    let path = parsed.as_ref().map_or(url, |u| u.path());
    if AUTH_INVALIDATION.is_match(path) {
        return Some(FenceReason::AuthInvalidation);
    }
    if MUTATION_METHODS.contains(&method.to_uppercase().as_str()) {
        return Some(FenceReason::Mutation);
    }
    None
}

pub fn record_fence_block(method: &str, url: &str, reason: FenceReason) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let mut state = lock(&STATE);
    state.blocks.push_back(FenceBlock {
        method: method.to_string(),
        url: url.to_string(),
        ts,
        reason,
    });
    while state.blocks.len() > BLOCKS_CAP {
        state.blocks.pop_front();
    }
}

pub fn get_fence_blocks() -> Vec<FenceBlock> {
    lock(&STATE).blocks.iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isolated;

    fn reset() {
        // Drain any holders left by a test so the ref-count starts clean.
        while is_fence_active() {
            release_fence();
        }
        clear_lab_hosts();
    }

    #[test]
    fn 비멱등_메서드는_mutation으로_막는다() {
        let _clean = isolated(reset);
        for m in ["POST", "PUT", "PATCH", "DELETE", "post", "delete"] {
            assert_eq!(
                fence_reason(m, "https://x.test/api/items"),
                Some(FenceReason::Mutation)
            );
        }
    }

    #[test]
    fn 멱등_읽기는_통과시킨다() {
        let _clean = isolated(reset);
        for m in ["GET", "HEAD", "OPTIONS"] {
            assert_eq!(fence_reason(m, "https://x.test/api/items"), None);
        }
    }

    #[test]
    fn 인증_무효화_경로는_메서드와_무관하게_막는다() {
        let _clean = isolated(reset);
        for url in [
            "https://x.test/logout",
            "https://x.test/auth/sign-out",
            "https://x.test/session/revoke",
        ] {
            assert_eq!(
                fence_reason("GET", url),
                Some(FenceReason::AuthInvalidation)
            );
        }
    }

    #[test]
    fn 경로에_logout_유사_문자열이_포함돼도_토큰_경계가_아니면_막지_않는다() {
        let _clean = isolated(reset);
        assert_eq!(
            fence_reason("GET", "https://x.test/products/logouter-brand"),
            None
        );
    }

    #[test]
    fn loopback은_기본으로_변경_요청을_통과시킨다() {
        let _clean = isolated(reset);
        assert_eq!(fence_reason("POST", "http://localhost:1337/"), None);
        assert_eq!(fence_reason("POST", "http://127.0.0.1:5000/x"), None);
        assert_eq!(fence_reason("DELETE", "http://[::1]/api"), None);
    }

    #[test]
    fn loopback_외의_호스트는_기본으로_막고_lab_target으로_지정하면_통과시킨다() {
        let _clean = isolated(reset);
        assert_eq!(
            fence_reason("POST", "https://staging.example.com/api"),
            Some(FenceReason::Mutation)
        );
        set_lab_hosts(&["https://staging.example.com/"]);
        assert_eq!(
            fence_reason("POST", "https://staging.example.com/api"),
            None
        );
        // Other external hosts that were not marked stay fenced.
        assert_eq!(
            fence_reason("POST", "https://x.test/api"),
            Some(FenceReason::Mutation)
        );
    }

    #[test]
    fn loopback_타겟만_true() {
        assert!(is_loopback_host("http://localhost:1337/"));
        assert!(is_loopback_host("http://127.0.0.1:8080/x"));
        assert!(is_loopback_host("http://[::1]:3000/"));
        assert!(is_loopback_host("https://app.localhost/"));
    }

    #[test]
    fn 공인_호스트는_false() {
        assert!(!is_loopback_host("https://example.com/"));
        assert!(!is_loopback_host("https://127.0.0.1.evil.com/"));
        assert!(!is_loopback_host("not a url"));
    }

    #[test]
    fn set_get_clear로_랩_호스트_목록을_관리한다() {
        let _clean = isolated(reset);
        assert_eq!(get_lab_hosts(), Vec::<String>::new());
        set_lab_hosts(&["http://localhost:1337/", "bad host"]);
        assert_eq!(get_lab_hosts(), ["localhost:1337"]);
        clear_lab_hosts();
        assert_eq!(get_lab_hosts(), Vec::<String>::new());
    }

    #[test]
    fn is_lab_host는_포트_단위로_매칭한다() {
        let _clean = isolated(reset);
        set_lab_hosts(&["localhost:1337"]);
        assert!(is_lab_host("http://localhost:1337/path"));
        assert!(!is_lab_host("http://localhost:1338/"));
    }

    #[test]
    fn 홀더가_있을_때만_활성이고_참조_카운트로_동작한다() {
        let _clean = isolated(reset);
        assert!(!is_fence_active());
        acquire_fence();
        acquire_fence();
        assert!(is_fence_active());
        release_fence();
        assert!(is_fence_active()); // one holder still left
        release_fence();
        assert!(!is_fence_active());
    }

    #[test]
    fn 첫_획득_시_지난_차단_원장을_비운다() {
        let _clean = isolated(reset);
        acquire_fence();
        record_fence_block("POST", "https://x.test/a", FenceReason::Mutation);
        release_fence();
        assert_eq!(get_fence_blocks().len(), 1);
        acquire_fence(); // fresh session clears
        assert_eq!(get_fence_blocks().len(), 0);
        release_fence();
    }
}
