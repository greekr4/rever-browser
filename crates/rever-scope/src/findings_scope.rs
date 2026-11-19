//! Port of `src/main/mcp/findings-scope.ts` — which site a finding belongs
//! to, and which site a findings export covers.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::js_regex;
use crate::scope::scope_key;

// The TypeScript pattern ends in a lookahead `(?=:\d+$|$)`; the regex crate has
// none, so the optional port is captured and put back instead.
static LOOPBACK_KEY: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^(?:\[?::1\]?|127\.\d{1,3}\.\d{1,3}\.\d{1,3})(:\d+)?$"));

/// Fold loopback spellings in an already-computed site key — heals legacy
/// findings stamped "127.0.0.1:1337" before the fold existed.
pub fn canonical_origin_key(key: Option<&str>) -> Option<String> {
    let key = key.filter(|k| !k.is_empty())?;
    Some(LOOPBACK_KEY.replace(key, "localhost$1").into_owned())
}

/// `/^https?:\/\//i`
fn is_http_url(url: &str) -> bool {
    let starts_with = |prefix: &str| {
        url.as_bytes()
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
    };
    starts_with("http://") || starts_with("https://")
}

/// Scope key of an http(s) URL (localhost:8080 → "localhost:8080"); None otherwise.
pub fn origin_of(url: Option<&str>) -> Option<String> {
    let url = url.filter(|u| is_http_url(u))?;
    canonical_origin_key(scope_key(url).as_deref())
}

/// Anything that may carry the site key it was recorded on.
pub trait HasOrigin {
    fn origin(&self) -> Option<&str>;
}

/// Scope findings to one site. `all` returns everything (incl. legacy findings
/// with no origin). A missing scope returns everything too — better a full
/// report than an empty one when the current site can't be determined.
pub fn filter_by_origin<T: HasOrigin>(list: Vec<T>, scope: Option<&str>, all: bool) -> Vec<T> {
    let Some(scope) = scope.filter(|s| !all && !s.is_empty()) else {
        return list;
    };
    list.into_iter()
        .filter(|f| f.origin() == Some(scope))
        .collect()
}

/// Either the site key to export, an error to show instead, or neither when
/// the whole store was asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ExportScope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Scope for finding_export: an explicit `site` wins (the scan knows its target
/// even when no tab is attached anymore), then the active page's URL. Neither →
/// error instead of a whole-store dump: the store spans months of sites and a
/// scopeless export once hit 343KB / 392 findings, which the agent cannot even
/// ingest.
pub fn resolve_export_scope(
    site: Option<&str>,
    active_url: Option<&str>,
    all: bool,
) -> ExportScope {
    if all {
        return ExportScope::default();
    }
    match origin_of(site).or_else(|| origin_of(active_url)) {
        Some(scope) => ExportScope {
            scope: Some(scope),
            error: None,
        },
        None => ExportScope {
            scope: None,
            error: Some(
                "no scope: pass site with the scanned URL (e.g. site: \"http://localhost:1337\") or all=true — otherwise I cannot tell which site the report covers."
                    .to_string(),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct Item {
        id: &'static str,
        origin: Option<&'static str>,
    }

    impl HasOrigin for Item {
        fn origin(&self) -> Option<&str> {
            self.origin
        }
    }

    fn list() -> Vec<Item> {
        vec![
            Item {
                id: "1",
                origin: Some("localhost"),
            },
            Item {
                id: "2",
                origin: Some("careernote.io"),
            },
            // legacy finding — no origin
            Item {
                id: "3",
                origin: None,
            },
        ]
    }

    fn ids(items: Vec<Item>) -> Vec<&'static str> {
        items.iter().map(|f| f.id).collect()
    }

    fn scoped(scope: &str) -> ExportScope {
        ExportScope {
            scope: Some(scope.to_string()),
            error: None,
        }
    }

    #[test]
    fn http_url을_등록가능도메인으로_접는다() {
        assert_eq!(
            origin_of(Some("https://api.careernote.io/v1/x")).as_deref(),
            Some("careernote.io")
        );
        assert_eq!(
            origin_of(Some("https://www.example.co.kr/")).as_deref(),
            Some("example.co.kr")
        );
    }

    #[test]
    fn localhost는_포트를_유지해_앱별로_구분한다() {
        assert_eq!(
            origin_of(Some("http://localhost:8080/my")).as_deref(),
            Some("localhost:8080")
        );
        assert_eq!(
            origin_of(Some("http://localhost:3000/v1/jobs")).as_deref(),
            Some("localhost:3000")
        );
    }

    #[test]
    fn 루프백_표기는_localhost_키로_접는다() {
        assert_eq!(
            origin_of(Some("http://127.0.0.1:1337/panel/search")).as_deref(),
            Some("localhost:1337")
        );
        assert_eq!(
            origin_of(Some("http://[::1]:1337/")).as_deref(),
            Some("localhost:1337")
        );
    }

    #[test]
    fn http가_아니거나_못_읽으면_none() {
        assert_eq!(origin_of(Some("about:blank")), None);
        assert_eq!(origin_of(Some("")), None);
        assert_eq!(origin_of(None), None);
    }

    #[test]
    fn 루프백_ip_포트_스탬프를_localhost_포트로_바꾼다() {
        assert_eq!(
            canonical_origin_key(Some("127.0.0.1:1337")).as_deref(),
            Some("localhost:1337")
        );
        assert_eq!(
            canonical_origin_key(Some("[::1]:1337")).as_deref(),
            Some("localhost:1337")
        );
    }

    #[test]
    fn 루프백이_아닌_키와_none은_그대로_둔다() {
        assert_eq!(
            canonical_origin_key(Some("example.com")).as_deref(),
            Some("example.com")
        );
        assert_eq!(
            canonical_origin_key(Some("10.0.0.1:8080")).as_deref(),
            Some("10.0.0.1:8080")
        );
        assert_eq!(canonical_origin_key(None), None);
    }

    #[test]
    fn all이_true면_origin_없는_옛_발견까지_전부() {
        assert_eq!(
            ids(filter_by_origin(list(), Some("localhost"), true)),
            ["1", "2", "3"]
        );
    }

    #[test]
    fn 스코프를_주면_그_사이트_것만_옛_발견과_다른_사이트는_뺀다() {
        assert_eq!(
            ids(filter_by_origin(list(), Some("localhost"), false)),
            ["1"]
        );
    }

    #[test]
    fn 스코프가_none이면_전부_반환한다() {
        assert_eq!(ids(filter_by_origin(list(), None, false)), ["1", "2", "3"]);
    }

    #[test]
    fn 명시적_site가_활성_탭보다_우선한다() {
        assert_eq!(
            resolve_export_scope(Some("http://localhost:1337/"), None, false),
            scoped("localhost:1337")
        );
        assert_eq!(
            resolve_export_scope(
                Some("https://arqoven.com"),
                Some("http://other:8080/"),
                false
            ),
            scoped("arqoven.com")
        );
    }

    #[test]
    fn site가_없으면_활성_탭_url로_스코프를_잡는다() {
        assert_eq!(
            resolve_export_scope(None, Some("https://api.careernote.io/v1/x"), false),
            scoped("careernote.io")
        );
    }

    #[test]
    fn 둘_다_없으면_전체_덤프_대신_안내_에러를_낸다() {
        let r = resolve_export_scope(None, None, false);
        assert_eq!(r.scope, None);
        assert!(r.error.is_some_and(|e| e.contains("site")));
    }

    #[test]
    fn all이_true면_에러_없이_전체_내보내기_허용() {
        assert_eq!(
            resolve_export_scope(None, None, true),
            ExportScope::default()
        );
    }
}
