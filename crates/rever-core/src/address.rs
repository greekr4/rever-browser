//! Port of `src/renderer/src/lib/url.ts`: address-bar input → a loadable URL.
//!
//! A bare host gets a scheme prepended; loopback/localhost default to http://
//! (dev servers rarely serve https), so typing `localhost:8080/my` reaches the
//! dev server instead of failing on TLS.

use std::sync::LazyLock;

use regex::Regex;

static LOOPBACK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(localhost|[^./\s]+\.localhost|127(?:\.\d{1,3}){3}|0\.0\.0\.0|\[?::1\]?)(:\d+)?(/|$|\?|#)")
        .expect("built-in pattern compiles")
});
static HAS_SCHEME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(https?|view-source|file|about|data):").expect("built-in pattern compiles")
});

pub fn normalize_address(input: &str) -> String {
    let target = input.trim();
    if target.is_empty() || HAS_SCHEME.is_match(target) {
        return target.to_owned();
    }
    let scheme = if LOOPBACK.is_match(target) {
        "http://"
    } else {
        "https://"
    };
    format!("{scheme}{target}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 로컬호스트는_http로_보정한다() {
        assert_eq!(
            normalize_address("localhost:8080/my"),
            "http://localhost:8080/my"
        );
        assert_eq!(normalize_address("localhost"), "http://localhost");
        assert_eq!(normalize_address("127.0.0.1:8090"), "http://127.0.0.1:8090");
        assert_eq!(
            normalize_address("app.localhost:3000"),
            "http://app.localhost:3000"
        );
    }

    #[test]
    fn 일반_도메인은_https로_보정한다() {
        assert_eq!(normalize_address("example.com"), "https://example.com");
        assert_eq!(
            normalize_address("naver.com/path"),
            "https://naver.com/path"
        );
    }

    #[test]
    fn 이미_scheme가_있으면_그대로_둔다() {
        assert_eq!(
            normalize_address("http://localhost:8080"),
            "http://localhost:8080"
        );
        assert_eq!(
            normalize_address("https://example.com"),
            "https://example.com"
        );
        assert_eq!(
            normalize_address("view-source:https://x.com"),
            "view-source:https://x.com"
        );
        assert_eq!(normalize_address("about:blank"), "about:blank");
    }

    #[test]
    fn 공백은_다듬고_빈_입력은_빈_문자열() {
        assert_eq!(normalize_address("  example.com  "), "https://example.com");
        assert_eq!(normalize_address("   "), "");
    }
}
