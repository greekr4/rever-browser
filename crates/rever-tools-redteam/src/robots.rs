//! robots.txt consultation for the findings report.
//!
//! The scan pipeline never fetches robots.txt on its own. Whatever
//! `GET /robots.txt` response the captured traffic already holds becomes the
//! rule set the report is annotated with. A scan that captured none gets no
//! annotation rather than a guess.

use std::collections::HashMap;
use std::sync::PoisonError;

use rever_core::traffic::{ListFilter, Source};
use rever_robots::RobotsTxt;
use rever_tools_api::tool::ToolContext;
use url::Url;

/// The agent identity the rules are matched against.
///
/// Named bot groups never match a browser-shaped agent string, so the `*`
/// group governs: the honest semantics for a tool that visits pages as a
/// browser.
const AGENT: &str = "rever";

/// Parsed robots.txt rule sets by origin, taken from captured traffic.
#[derive(Default)]
pub struct RobotsNotes {
    by_origin: HashMap<String, RobotsTxt>,
}

impl RobotsNotes {
    /// Scans the captured traffic for `GET /robots.txt` responses. When an
    /// origin answered more than once the latest response wins.
    pub fn from_traffic(ctx: &ToolContext) -> Self {
        let traffic = ctx.traffic.lock().unwrap_or_else(PoisonError::into_inner);
        // request id → started_at of the newest robots.txt body per origin.
        let mut newest: HashMap<String, (u64, String)> = HashMap::new();
        let filter = ListFilter {
            source: Source::All,
            limit: Some(usize::MAX),
            ..Default::default()
        };
        for entry in traffic.list(&filter) {
            let Some(body) = entry
                .response_body
                .as_deref()
                .filter(|_| !entry.response_body_base64.unwrap_or(false))
                .filter(|body| !body.is_empty())
            else {
                continue;
            };
            if !entry.status.is_some_and(|status| (200..300).contains(&status)) {
                continue;
            }
            let Ok(url) = Url::parse(&entry.url) else {
                continue;
            };
            if url.path() != "/robots.txt" {
                continue;
            }
            let key = origin_key(&url);
            let started = entry.started_at;
            match newest.get(&key) {
                Some((at, _)) if *at >= started => {}
                _ => {
                    newest.insert(key, (started, body.to_owned()));
                }
            }
        }

        Self {
            by_origin: newest
                .into_iter()
                .map(|(origin, (_, body))| (origin, RobotsTxt::parse(&body)))
                .collect(),
        }
    }

    /// Whether `raw_url`'s path is disallowed by its origin's captured rules.
    ///
    /// `false` for URLs without a captured rule set: the report never claims a
    /// robots violation it cannot back with a capture.
    pub fn disallows(&self, raw_url: &str) -> bool {
        let Ok(url) = Url::parse(raw_url) else {
            return false;
        };
        let Some(rules) = self.by_origin.get(&origin_key(&url)) else {
            return false;
        };
        !rules.allows(AGENT, &rever_robots::robots_request_target(&url))
    }

    /// How many origins carry captured rules.
    pub fn len(&self) -> usize {
        self.by_origin.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_origin.is_empty()
    }
}

/// scheme://host[:explicit port]. `Url::port` folds default ports away, so
/// both spellings of an origin land on one key.
fn origin_key(url: &Url) -> String {
    match url.port() {
        Some(port) => format!(
            "{}://{}:{port}",
            url.scheme(),
            url.host_str().unwrap_or_default()
        ),
        None => format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rever_core::traffic::RequestPatch;

    fn capture(ctx: &ToolContext, id: &str, url: &str, status: u16, body: &str, at: u64) {
        let patch = RequestPatch {
            request_id: id.to_owned(),
            url: Some(url.to_owned()),
            method: Some("GET".to_owned()),
            status: Some(status),
            response_body: Some(body.to_owned()),
            started_at: Some(at),
            ..Default::default()
        };
        ctx.traffic
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .upsert(patch, at);
    }

    #[test]
    fn 캡처된_robots_응답에서_정책을_뽑는다() {
        let ctx = ToolContext::for_tests();
        capture(
            &ctx,
            "r1",
            "http://shop.test/robots.txt",
            200,
            "User-agent: *\nDisallow: /admin\n",
            10,
        );
        let notes = RobotsNotes::from_traffic(&ctx);
        assert_eq!(notes.len(), 1);
        assert!(notes.disallows("http://shop.test/admin/panel"));
        assert!(!notes.disallows("http://shop.test/public"));
    }

    #[test]
    fn 같은_오리진은_가장_최근_응답이_이긴다() {
        let ctx = ToolContext::for_tests();
        capture(
            &ctx,
            "r1",
            "http://shop.test/robots.txt",
            200,
            "User-agent: *\nDisallow: /\n",
            10,
        );
        capture(
            &ctx,
            "r2",
            "http://shop.test/robots.txt",
            200,
            "User-agent: *\nDisallow: /admin\n",
            20,
        );
        let notes = RobotsNotes::from_traffic(&ctx);
        assert!(!notes.disallows("http://shop.test/public"));
        assert!(notes.disallows("http://shop.test/admin"));
    }

    #[test]
    fn 오리진이_다르면_정책도_각자_적용된다() {
        let ctx = ToolContext::for_tests();
        capture(
            &ctx,
            "r1",
            "http://a.test/robots.txt",
            200,
            "User-agent: *\nDisallow: /\n",
            10,
        );
        capture(
            &ctx,
            "r2",
            "http://b.test/robots.txt",
            200,
            "User-agent: *\nDisallow:\n",
            10,
        );
        let notes = RobotsNotes::from_traffic(&ctx);
        assert_eq!(notes.len(), 2);
        assert!(notes.disallows("http://a.test/x"));
        assert!(!notes.disallows("http://b.test/x"));
    }

    #[test]
    fn 실패_응답과_base64_본문은_무시한다() {
        let ctx = ToolContext::for_tests();
        capture(&ctx, "r1", "http://err.test/robots.txt", 500, "User-agent: *\nDisallow: /\n", 10);
        let binary = RequestPatch {
            request_id: "r2".to_owned(),
            url: Some("http://bin.test/robots.txt".to_owned()),
            method: Some("GET".to_owned()),
            status: Some(200),
            response_body: Some("VXNlci1hZ2VudDogKg==".to_owned()),
            response_body_base64: Some(true),
            started_at: Some(10),
            ..Default::default()
        };
        ctx.traffic
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .upsert(binary, 10);
        let notes = RobotsNotes::from_traffic(&ctx);
        assert!(notes.is_empty());
    }

    #[test]
    fn robots_이외의_경로는_정책이_되지_않는다() {
        let ctx = ToolContext::for_tests();
        capture(
            &ctx,
            "r1",
            "http://shop.test/sitemap.xml",
            200,
            "User-agent: *\nDisallow: /\n",
            10,
        );
        assert!(RobotsNotes::from_traffic(&ctx).is_empty());
    }

    #[test]
    fn 정책_없는_오리진은_차단하지_않는다() {
        let ctx = ToolContext::for_tests();
        let notes = RobotsNotes::from_traffic(&ctx);
        assert!(!notes.disallows("http://unknown.test/anything"));
    }
}
