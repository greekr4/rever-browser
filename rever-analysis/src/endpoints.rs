//! Port of `src/main/mcp/endpoints.ts` — groups captured XHR/Fetch requests
//! into endpoints and formats the list for the agent.

use std::collections::HashMap;

use rever_core::traffic::StoredRequest;
use rever_scope::scope::registrable_domain;
use serde::Serialize;
use url::Url;

use crate::endpoint_template::template_path;

// Same set the renderer's API map aggregates — documents, assets and pings are not endpoints.
pub const API_TYPES: [&str; 2] = ["XHR", "Fetch"];

pub(crate) fn is_api_type(resource_type: &str) -> bool {
    API_TYPES.contains(&resource_type)
}

/// `new URL(url).origin`: "null" for schemes without one.
fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// `METHOD origin/templated-path`, or None for an unparsable URL.
pub fn endpoint_key(r: &StoredRequest) -> Option<String> {
    let u = Url::parse(&r.url).ok()?;
    Some(format!(
        "{} {}{}",
        r.method,
        origin_of(&u),
        template_path(u.path())
    ))
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointSummary {
    pub method: String,
    pub origin: String,
    pub path: String,
    pub count: usize,
    pub statuses: Vec<u16>,
    pub last_request_id: String,
    pub last_seen: u64,
}

/// Group XHR/Fetch requests by method + origin + templated path, most-called first.
pub fn summarize_endpoints<'a>(
    reqs: impl IntoIterator<Item = &'a StoredRequest>,
) -> Vec<EndpointSummary> {
    let mut eps: Vec<EndpointSummary> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for r in reqs {
        if !is_api_type(&r.resource_type) {
            continue;
        }
        let Ok(u) = Url::parse(&r.url) else {
            continue;
        };
        let path = template_path(u.path());
        let origin = origin_of(&u);
        let at = *by_key
            .entry(format!("{} {origin}{path}", r.method))
            .or_insert(eps.len());
        if at == eps.len() {
            eps.push(EndpointSummary {
                method: r.method.clone(),
                origin,
                path,
                count: 0,
                statuses: Vec::new(),
                last_request_id: r.request_id.clone(),
                last_seen: 0,
            });
        }
        let Some(ep) = eps.get_mut(at) else {
            continue;
        };
        ep.count += 1;
        if let Some(status) = r.status
            && !ep.statuses.contains(&status)
        {
            ep.statuses.push(status);
        }
        if r.started_at >= ep.last_seen {
            ep.last_seen = r.started_at;
            ep.last_request_id = r.request_id.clone();
        }
    }
    // Stable, like Array.prototype.sort: ties keep first-seen order.
    eps.sort_by(|a, b| b.count.cmp(&a.count).then(b.last_seen.cmp(&a.last_seen)));
    eps
}

/// eTLD+1 of an http(s) URL; None otherwise. localhost:3000 and :8080 share "localhost".
fn site_of(url: &str) -> Option<String> {
    let u = Url::parse(url).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| registrable_domain(u.host_str().unwrap_or("")))
}

pub struct SiteSplit<'a> {
    pub site: Option<String>,
    pub same: Vec<&'a StoredRequest>,
    pub other: Vec<&'a StoredRequest>,
}

/// The store outlives navigation, so a tab that visited another site still holds
/// its traffic. Keep the current page's site; the rest is returned for counting.
pub fn split_by_site<'a>(
    reqs: impl IntoIterator<Item = &'a StoredRequest>,
    page_url: Option<&str>,
) -> SiteSplit<'a> {
    let site = page_url.and_then(site_of).filter(|s| !s.is_empty());
    let (same, other) = match &site {
        None => (reqs.into_iter().collect(), Vec::new()),
        Some(_) => reqs.into_iter().partition(|r| site_of(&r.url) == site),
    };
    SiteSplit { site, same, other }
}

/// The site the list was narrowed to, and how many endpoints that hid.
pub struct EndpointScope<'a> {
    pub site: &'a str,
    pub other_sites: usize,
}

pub fn format_endpoints(
    eps: &[EndpointSummary],
    limit: usize,
    scope: Option<&EndpointScope>,
) -> String {
    let hidden = match scope {
        Some(s) if s.other_sites > 0 => format!(
            "\n({} endpoint(s) on other sites hidden — pass all=true to include them)",
            s.other_sites
        ),
        _ => String::new(),
    };
    if eps.is_empty() {
        let site = scope.map_or(String::new(), |s| format!(" for site {}", s.site));
        return format!("No XHR/Fetch endpoints captured yet{site}.{hidden}");
    }
    let lines: Vec<String> = eps
        .iter()
        .take(limit)
        .map(|e| {
            let mut sorted = e.statuses.clone();
            sorted.sort_unstable();
            let statuses = if sorted.is_empty() {
                String::new()
            } else {
                let joined: Vec<String> = sorted.iter().map(u16::to_string).collect();
                format!(" [{}]", joined.join(","))
            };
            format!(
                "{} {}{} ×{}{statuses} last=#{}",
                e.method, e.origin, e.path, e.count, e.last_request_id
            )
        })
        .collect();
    let more = if eps.len() > limit {
        format!("\n… +{} more", eps.len() - limit)
    } else {
        String::new()
    };
    let place = scope.map_or(String::new(), |s| format!(" on site {}", s.site));
    format!(
        "{} endpoint(s){place}, most-called first. get_request the last=# id for details.\n{}{more}{hidden}",
        eps.len(),
        lines.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(id: &str) -> StoredRequest {
        StoredRequest {
            request_id: id.into(),
            url: "https://api.example.com/x".into(),
            host: "api.example.com".into(),
            method: "GET".into(),
            resource_type: "Fetch".into(),
            started_at: 0,
            ..StoredRequest::default()
        }
    }

    fn at(id: &str, url: &str) -> StoredRequest {
        StoredRequest {
            url: url.into(),
            ..req(id)
        }
    }

    fn ids(reqs: &[&StoredRequest]) -> Vec<String> {
        reqs.iter().map(|r| r.request_id.clone()).collect()
    }

    // ── summarizeEndpoints ──────────────────────────────────────────────────

    #[test]
    fn 숫자_id_경로를_id로_묶어_한_엔드포인트로_센다() {
        let eps = summarize_endpoints(&[
            at("1", "https://api.example.com/users/12/orders"),
            at("2", "https://api.example.com/users/34/orders"),
        ]);
        assert_eq!(eps.len(), 1);
        assert_eq!(
            (eps[0].method.as_str(), eps[0].path.as_str(), eps[0].count),
            ("GET", "/users/:id/orders", 2)
        );
    }

    #[test]
    fn xhr_fetch만_집계한다() {
        let eps = summarize_endpoints(&[
            StoredRequest {
                resource_type: "Image".into(),
                ..at("1", "https://cdn.example.com/a.png")
            },
            StoredRequest {
                resource_type: "Script".into(),
                ..at("2", "https://cdn.example.com/app.js")
            },
            StoredRequest {
                resource_type: "XHR".into(),
                ..at("3", "https://api.example.com/feed")
            },
        ]);
        let paths: Vec<&str> = eps.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["/feed"]);
    }

    #[test]
    fn 메서드가_다르면_다른_엔드포인트다() {
        let eps = summarize_endpoints(&[
            StoredRequest {
                method: "GET".into(),
                ..at("1", "https://api.example.com/cart")
            },
            StoredRequest {
                method: "POST".into(),
                ..at("2", "https://api.example.com/cart")
            },
        ]);
        assert_eq!(eps.len(), 2);
    }

    #[test]
    fn 호출_수가_많은_순으로_정렬하고_상태_코드와_마지막_요청_id를_모은다() {
        let timed = |id: &str, url: &str, status: u16, started_at: u64| StoredRequest {
            status: Some(status),
            started_at,
            ..at(id, url)
        };
        let eps = summarize_endpoints(&[
            timed("1", "https://api.example.com/once", 200, 1),
            timed("2", "https://api.example.com/many", 200, 2),
            timed("3", "https://api.example.com/many", 500, 3),
        ]);
        assert_eq!(eps[0].path, "/many");
        assert_eq!(eps[0].count, 2);
        assert_eq!(eps[0].statuses, [200, 500]);
        assert_eq!(eps[0].last_request_id, "3");
        assert_eq!(eps[1].path, "/once");
    }

    // ── formatEndpoints ─────────────────────────────────────────────────────

    #[test]
    fn 엔드포인트가_없으면_안내_문구를_반환한다() {
        assert!(format_endpoints(&[], 50, None).contains("No XHR/Fetch"));
    }

    #[test]
    fn 한_줄에_메서드_url_호출_수_상태_마지막_요청_id를_쓴다() {
        let out = format_endpoints(
            &summarize_endpoints(&[StoredRequest {
                method: "POST".into(),
                status: Some(200),
                ..at("9", "https://api.example.com/cart")
            }]),
            50,
            None,
        );
        assert!(out.contains("POST https://api.example.com/cart ×1 [200] last=#9"));
    }

    #[test]
    fn limit을_넘으면_나머지_개수를_요약한다() {
        let reqs: Vec<StoredRequest> = (0..5)
            .map(|i| at(&i.to_string(), &format!("https://api.example.com/e{i}")))
            .collect();
        let out = format_endpoints(&summarize_endpoints(&reqs), 3, None);
        assert_eq!(out.lines().filter(|l| l.starts_with("GET ")).count(), 3);
        assert!(out.contains("… +2 more"));
    }

    // ── splitBySite ─────────────────────────────────────────────────────────

    #[test]
    fn 현재_페이지와_같은_사이트의_요청만_남기고_나머지를_따로_돌려준다() {
        let reqs = [
            at("1", "https://api.example.com/feed"),
            at("2", "https://rake.11st.co.kr/log"),
            at("3", "https://www.google-analytics.com/g/collect"),
        ];
        let split = split_by_site(&reqs, Some("https://www.example.com/home"));
        assert_eq!(ids(&split.same), ["1"]);
        assert_eq!(ids(&split.other), ["2", "3"]);
    }

    #[test]
    fn localhost는_포트가_달라도_같은_사이트다() {
        let reqs = [at("1", "http://localhost:3000/v1/jobs")];
        let split = split_by_site(&reqs, Some("http://localhost:8080/my"));
        assert_eq!(split.same.len(), 1);
    }

    #[test]
    fn 페이지_url이_http가_아니면_거르지_않는다() {
        let reqs = [req("1")];
        let split = split_by_site(&reqs, Some("about:blank"));
        assert_eq!(split.same.len(), 1);
        assert_eq!(split.other.len(), 0);
    }

    // ── formatEndpoints 다른 사이트 안내 ────────────────────────────────────

    #[test]
    fn 숨긴_다른_사이트_엔드포인트_수와_전체_보기_방법을_알린다() {
        let out = format_endpoints(
            &summarize_endpoints(&[req("1")]),
            50,
            Some(&EndpointScope {
                site: "example.com",
                other_sites: 7,
            }),
        );
        assert!(out.contains("site example.com"));
        assert!(out.contains("7 endpoint(s) on other sites hidden — pass all=true"));
    }

    // ── not in the TypeScript suite ─────────────────────────────────────────

    #[test]
    fn 해석할_수_없는_url은_키가_없고_집계에서_빠진다() {
        let bad = at("1", "not a url");
        assert_eq!(endpoint_key(&bad), None);
        assert!(summarize_endpoints(&[bad]).is_empty());
        assert_eq!(
            endpoint_key(&at("2", "https://api.example.com:8443/users/7?x=1")).as_deref(),
            Some("GET https://api.example.com:8443/users/:id")
        );
    }

    #[test]
    fn 엔드포인트_키는_자바스크립트_url의_origin과_pathname을_쓴다() {
        // Reference values from the TypeScript's endpointKey.
        let cases = [
            ("https://A.com:443/x/1?q", Some("GET https://a.com/x/:id")),
            ("http://a.com:8080", Some("GET http://a.com:8080/")),
            (
                "blob:https://a.com/uuid",
                Some("GET https://a.comhttps://a.com/uuid"),
            ),
            ("file:///a/1", Some("GET null/a/:id")),
            ("about:blank", Some("GET nullblank")),
            (
                "https://[::1]:3000/1/2",
                Some("GET https://[::1]:3000/:id/:id"),
            ),
            (
                "https://a.com/a b/%41/é",
                Some("GET https://a.com/a%20b/%41/%C3%A9"),
            ),
            (
                "https://a.com/1/../2/./3",
                Some("GET https://a.com/:id/:id"),
            ),
            ("https://a.com\\x\\1", Some("GET https://a.com/x/:id")),
            ("chrome-extension://abc/x/1", Some("GET null/x/:id")),
            (
                "https://bücher.example/1",
                Some("GET https://xn--bcher-kva.example/:id"),
            ),
            ("https://0x7f.1/1", Some("GET https://127.0.0.1/:id")),
            ("//a.com/x", None),
            ("http://a.com:65536/", None),
            ("http://a b.com/", None),
        ];
        for (url, expected) in cases {
            assert_eq!(endpoint_key(&at("1", url)).as_deref(), expected, "{url}");
        }
    }

    #[test]
    fn 페이지_사이트를_알_수_없으면_모두_남기고_ip와_루프백도_사이트로_가른다() {
        let reqs = [
            at("1", "https://api.example.co.kr/1"),
            at("2", "http://localhost:1/"),
            at("3", "http://10.0.0.1/"),
            at("4", "bad"),
        ];
        for page in [None, Some(""), Some("nope"), Some("file:///x")] {
            let split = split_by_site(&reqs, page);
            assert_eq!(split.site, None, "{page:?}");
            assert_eq!((split.same.len(), split.other.len()), (4, 0), "{page:?}");
        }
        let cases = [
            ("https://www.example.co.kr/x", "example.co.kr", "1"),
            ("http://127.0.0.1:3000/", "localhost", "2"),
            ("http://[::1]/", "localhost", "2"),
            ("http://10.0.0.1:8/", "10.0.0.1", "3"),
        ];
        for (page, site, same) in cases {
            let split = split_by_site(&reqs, Some(page));
            assert_eq!(split.site.as_deref(), Some(site), "{page}");
            assert_eq!(ids(&split.same), [same], "{page}");
            assert_eq!(split.other.len(), 3, "{page}");
        }
    }

    #[test]
    fn 호출_수가_같으면_최근에_본_엔드포인트가_먼저다() {
        let timed = |id: &str, url: &str, started_at: u64| StoredRequest {
            started_at,
            ..at(id, url)
        };
        let eps = summarize_endpoints(&[
            timed("1", "https://api.example.com/old", 1),
            timed("2", "https://api.example.com/new", 2),
        ]);
        let paths: Vec<&str> = eps.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["/new", "/old"]);
    }

    #[test]
    fn 상태_코드는_오름차순으로_쓰고_빈_목록에도_사이트를_알린다() {
        let status = |id: &str, status: u16| StoredRequest {
            status: Some(status),
            ..req(id)
        };
        let out = format_endpoints(
            &summarize_endpoints(&[status("1", 500), status("2", 200)]),
            50,
            None,
        );
        assert!(out.contains("×2 [200,500] last=#2"));
        let scope = EndpointScope {
            site: "example.com",
            other_sites: 0,
        };
        assert_eq!(
            format_endpoints(&[], 50, Some(&scope)),
            "No XHR/Fetch endpoints captured yet for site example.com."
        );
    }

    #[test]
    fn 요약은_camel_case_필드로_직렬화한다() {
        let eps = summarize_endpoints(&[req("1")]);
        assert_eq!(
            serde_json::to_string(&eps[0]).unwrap(),
            r#"{"method":"GET","origin":"https://api.example.com","path":"/x","count":1,"statuses":[],"lastRequestId":"1","lastSeen":0}"#
        );
    }
}
