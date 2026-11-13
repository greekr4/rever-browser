//! Port of `src/main/mcp/triggered-requests.ts` — which requests a browser
//! action set off, minus what the page was already sending on its own.
//!
//! The TypeScript keeps the action windows in a module-level array and reads
//! the global store and clock; here they are `ActionWindows`, a `&TrafficStore`
//! and a `now_ms` argument.

use std::collections::{HashMap, VecDeque};

use rever_core::traffic::{ListFilter, StoredRequest, TrafficStore};

use crate::endpoints::{endpoint_key, is_api_type};

/// How many correlated requests to list inline before summarising the rest.
pub const MAX_TRIGGERED: usize = 20;
// Lookback used to learn what the page was already sending on its own.
const BACKGROUND_WINDOW_MS: u64 = 10_000;
// An endpoint seen this often in the lookback is background traffic (polling,
// logging beacons). Once is not enough: pressing the same button twice must
// still show its request the second time.
const BACKGROUND_MIN_HITS: usize = 2;

/// [start, end] of an agent action. Requests an action fired are its own,
/// not background traffic, so they must not teach the noise filter.
pub type Window = (u64, u64);
const MAX_WINDOWS: usize = 50;

/// Drop requests that started inside a past agent action window.
pub fn outside_actions<'a, 'w>(
    reqs: impl IntoIterator<Item = &'a StoredRequest>,
    windows: impl IntoIterator<Item = &'w Window> + Clone,
) -> Vec<&'a StoredRequest> {
    reqs.into_iter()
        .filter(|r| {
            !windows
                .clone()
                .into_iter()
                .any(|(a, b)| r.started_at >= *a && r.started_at <= *b)
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Triggered<'a> {
    pub shown: Vec<&'a StoredRequest>,
    pub hidden: usize,
}

/// Pure filter for the requests an action set off: XHR/Fetch only, the acting
/// tab only (other tabs share the store), minus endpoints that were already
/// recurring before the action. Hidden ones are counted, never dropped silently.
pub fn filter_triggered<'a, 'b>(
    candidates: impl IntoIterator<Item = &'a StoredRequest>,
    background: impl IntoIterator<Item = &'b StoredRequest>,
    tab_id: Option<u64>,
) -> Triggered<'a> {
    let relevant = |r: &StoredRequest| {
        is_api_type(&r.resource_type)
            && (tab_id.is_none() || r.tab_id.is_none() || r.tab_id == tab_id)
    };
    let mut hits: HashMap<String, usize> = HashMap::new();
    for r in background {
        if !relevant(r) {
            continue;
        }
        if let Some(key) = endpoint_key(r) {
            *hits.entry(key).or_default() += 1;
        }
    }
    let mut shown = Vec::new();
    let mut hidden = 0;
    for r in candidates {
        if !relevant(r) {
            continue;
        }
        let recurring = endpoint_key(r)
            .is_some_and(|key| hits.get(&key).copied().unwrap_or(0) >= BACKGROUND_MIN_HITS);
        if recurring {
            hidden += 1;
        } else {
            shown.push(r);
        }
    }
    shown.sort_by_key(|r| r.started_at);
    Triggered { shown, hidden }
}

/// Render the requests an action set off as a compact block appended to a
/// browser tool result — closes the "I clicked, but which request fired?" gap
/// without a separate list_requests round-trip. Always emits `since` so the
/// agent has a real clock value for a follow-up list_requests. The TypeScript
/// defaults `max` to `MAX_TRIGGERED`.
pub fn format_triggered_requests(triggered: &Triggered, since: u64, max: usize) -> String {
    let Triggered { shown, hidden } = triggered;
    let hidden_note = if *hidden > 0 {
        format!(
            "\n({hidden} recurring background request(s) hidden — list_requests({{\"since\":{since}}}) shows everything)"
        )
    } else {
        String::new()
    };
    if shown.is_empty() {
        return format!(
            "\n\n--- no XHR/Fetch requests triggered by this action (since={since}) ---{hidden_note}"
        );
    }
    let lines: Vec<String> = shown
        .iter()
        .take(max)
        .map(|r| {
            let status = r.status.map_or("—".to_owned(), |s| s.to_string());
            format!("#{} {} {status} {}", r.request_id, r.method, r.url)
        })
        .collect();
    let more = if shown.len() > max {
        format!("\n… +{} more", shown.len() - max)
    } else {
        String::new()
    };
    format!(
        "\n\n--- requests triggered by this action ({}, since={since}) ---\n{}{more}{hidden_note}",
        shown.len(),
        lines.join("\n")
    )
}

/// The recent agent action windows; one per session that drives a browser.
#[derive(Clone, Debug, Default)]
pub struct ActionWindows {
    windows: VecDeque<Window>,
}

impl ActionWindows {
    /// Store lookup + filter for the action that started at `since` on tab `tab_id`.
    pub fn collect_triggered_requests<'a>(
        &mut self,
        store: &'a TrafficStore,
        since: u64,
        tab_id: Option<u64>,
        now_ms: u64,
    ) -> Triggered<'a> {
        let recent = store.list(&ListFilter {
            since: Some(since.saturating_sub(BACKGROUND_WINDOW_MS)),
            limit: Some(500),
            ..ListFilter::default()
        });
        let result = filter_triggered(
            recent.iter().copied().filter(|r| r.started_at >= since),
            outside_actions(
                recent.iter().copied().filter(|r| r.started_at < since),
                &self.windows,
            ),
            tab_id,
        );
        self.windows.push_back((since, now_ms));
        if self.windows.len() > MAX_WINDOWS {
            self.windows.pop_front();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use rever_core::traffic::RequestPatch;

    use super::*;

    fn req(id: &str) -> StoredRequest {
        StoredRequest {
            request_id: id.into(),
            url: "https://api.example.com/x".into(),
            host: "api.example.com".into(),
            method: "GET".into(),
            resource_type: "XHR".into(),
            started_at: 0,
            ..StoredRequest::default()
        }
    }

    fn post(id: &str, url: &str) -> StoredRequest {
        StoredRequest {
            method: "POST".into(),
            url: url.into(),
            ..req(id)
        }
    }

    fn log(id: &str) -> StoredRequest {
        post(id, "https://log.example.com/putlog/client/abc")
    }

    fn cart(id: &str, started_at: u64) -> StoredRequest {
        StoredRequest {
            started_at,
            ..post(id, "https://api.example.com/cart")
        }
    }

    fn ids(reqs: &[&StoredRequest]) -> Vec<String> {
        reqs.iter().map(|r| r.request_id.clone()).collect()
    }

    const NONE: [StoredRequest; 0] = [];

    // ── filterTriggered ─────────────────────────────────────────────────────

    #[test]
    fn xhr_fetch만_남기고_이미지_스크립트는_뺀다() {
        let candidates = [
            StoredRequest {
                resource_type: "Image".into(),
                ..req("1")
            },
            StoredRequest {
                resource_type: "Script".into(),
                ..req("2")
            },
            StoredRequest {
                resource_type: "Fetch".into(),
                url: "https://api.example.com/cart".into(),
                ..req("3")
            },
        ];
        let t = filter_triggered(&candidates, &NONE, Some(1));
        assert_eq!(ids(&t.shown), ["3"]);
    }

    #[test]
    fn 다른_탭_요청은_뺀다() {
        let tab = |id: &str, tab_id: u64| StoredRequest {
            tab_id: Some(tab_id),
            ..req(id)
        };
        let candidates = [tab("1", 1), tab("2", 2)];
        let t = filter_triggered(&candidates, &NONE, Some(1));
        assert_eq!(ids(&t.shown), ["1"]);
    }

    #[test]
    fn 액션_직전에_두_번_이상_반복되던_엔드포인트는_숨기고_개수를_센다() {
        let candidates = [log("a"), post("b", "https://api.example.com/cart")];
        let background = [log("p1"), log("p2")];
        let t = filter_triggered(&candidates, &background, Some(1));
        assert_eq!(ids(&t.shown), ["b"]);
        assert_eq!(t.hidden, 1);
    }

    #[test]
    fn 직전에_한_번만_나온_엔드포인트는_숨기지_않는다() {
        // Pressing the same button twice.
        let candidates = [cart("2", 0)];
        let background = [cart("1", 0)];
        let t = filter_triggered(&candidates, &background, Some(1));
        assert_eq!(ids(&t.shown), ["2"]);
        assert_eq!(t.hidden, 0);
    }

    #[test]
    fn 경로의_숫자_id가_달라도_같은_엔드포인트로_본다() {
        let poll = |id: &str, n: u32| StoredRequest {
            url: format!("https://api.example.com/poll/{n}"),
            ..req(id)
        };
        let candidates = [poll("c", 3)];
        let background = [poll("a", 1), poll("b", 2)];
        let t = filter_triggered(&candidates, &background, Some(1));
        assert!(t.shown.is_empty());
        assert_eq!(t.hidden, 1);
    }

    #[test]
    fn 시작_시각_오름차순으로_돌려준다() {
        let at = |id: &str, started_at: u64| StoredRequest {
            started_at,
            ..req(id)
        };
        let candidates = [at("late", 20), at("early", 10)];
        let t = filter_triggered(&candidates, &NONE, None);
        assert_eq!(ids(&t.shown), ["early", "late"]);
    }

    // ── outsideActions ──────────────────────────────────────────────────────

    #[test]
    fn 이전_액션_구간에서_나간_요청은_배경_판정에서_뺀다() {
        // Hammering the same button.
        let earlier = [cart("1", 100), cart("2", 300)];
        let background = outside_actions(&earlier, &[(90, 150), (290, 350)]);
        assert!(background.is_empty());
        let candidates = [cart("3", 500)];
        let t = filter_triggered(&candidates, background, Some(1));
        assert_eq!(ids(&t.shown), ["3"]);
    }

    #[test]
    fn 액션_구간_밖의_반복_요청은_그대로_배경으로_남긴다() {
        // Polling.
        let poll = |id: &str, started_at: u64| StoredRequest {
            url: "https://api.example.com/poll".into(),
            started_at,
            ..req(id)
        };
        let reqs = [poll("1", 100), poll("2", 200), poll("3", 300)];
        assert_eq!(ids(&outside_actions(&reqs, &[(190, 210)])), ["1", "3"]);
    }

    // ── formatTriggeredRequests ─────────────────────────────────────────────

    #[test]
    fn 요청이_없어도_그_사실과_기준_시각을_한_줄로_알린다() {
        let out = format_triggered_requests(&Triggered::default(), 1000, MAX_TRIGGERED);
        assert!(out.contains("no XHR/Fetch requests triggered by this action"));
        assert!(out.contains("since=1000"));
    }

    #[test]
    fn 요청을_메서드_상태_url_한_줄로_포맷하고_개수와_기준_시각을_머리말에_넣는다() {
        let login = StoredRequest {
            status: Some(200),
            ..post("1", "https://api.example.com/login")
        };
        let out = format_triggered_requests(
            &Triggered {
                shown: vec![&login],
                hidden: 0,
            },
            1000,
            MAX_TRIGGERED,
        );
        assert!(out.contains("requests triggered by this action (1, since=1000)"));
        assert!(out.contains("#1 POST 200 https://api.example.com/login"));
    }

    #[test]
    fn 상태_코드가_없으면_줄표로_표시한다() {
        let pending = StoredRequest {
            status: None,
            ..req("2")
        };
        let out = format_triggered_requests(
            &Triggered {
                shown: vec![&pending],
                hidden: 0,
            },
            0,
            MAX_TRIGGERED,
        );
        assert!(out.contains("#2 GET — "));
    }

    #[test]
    fn 숨긴_배경_요청이_있으면_개수와_전체_보기_방법을_알린다() {
        let out = format_triggered_requests(
            &Triggered {
                shown: vec![],
                hidden: 3,
            },
            1000,
            MAX_TRIGGERED,
        );
        assert!(out.contains("3 recurring background request(s) hidden"));
        assert!(out.contains("list_requests({\"since\":1000})"));
    }

    #[test]
    fn 최대_개수를_넘으면_나머지를_요약한다() {
        let reqs: Vec<StoredRequest> = (0..23).map(|i| req(&i.to_string())).collect();
        let out = format_triggered_requests(
            &Triggered {
                shown: reqs.iter().collect(),
                hidden: 0,
            },
            0,
            20,
        );
        assert!(out.contains("requests triggered by this action (23, since=0)"));
        assert_eq!(out.lines().filter(|l| l.starts_with('#')).count(), 20);
        assert!(out.contains("… +3 more"));
    }

    // ── collectTriggeredRequests (not in the TypeScript suite) ──────────────

    fn fetch(store: &mut TrafficStore, id: &str, url: &str, started_at: u64) {
        store.upsert(
            RequestPatch {
                request_id: id.into(),
                url: Some(url.into()),
                method: Some("GET".into()),
                resource_type: Some("Fetch".into()),
                ..RequestPatch::default()
            },
            started_at,
        );
    }

    #[test]
    fn 저장소에서_액션_이후_요청을_모으고_폴링은_숨긴다() {
        let mut store = TrafficStore::default();
        fetch(&mut store, "p1", "https://api.example.com/poll", 20_000);
        fetch(&mut store, "p2", "https://api.example.com/poll", 25_000);
        fetch(&mut store, "p3", "https://api.example.com/poll", 30_500);
        fetch(&mut store, "c1", "https://api.example.com/cart", 30_600);
        let mut windows = ActionWindows::default();
        let t = windows.collect_triggered_requests(&store, 30_000, None, 31_000);
        assert_eq!(ids(&t.shown), ["c1"]);
        assert_eq!(t.hidden, 1);
    }

    #[test]
    fn 되돌아보는_구간보다_오래된_요청은_배경으로_세지_않는다() {
        let mut store = TrafficStore::default();
        // 10,001 ms before the action: outside the lookback.
        fetch(&mut store, "p1", "https://api.example.com/poll", 19_999);
        fetch(&mut store, "p2", "https://api.example.com/poll", 25_000);
        fetch(&mut store, "p3", "https://api.example.com/poll", 30_500);
        let mut windows = ActionWindows::default();
        let t = windows.collect_triggered_requests(&store, 30_000, None, 31_000);
        assert_eq!(ids(&t.shown), ["p3"]);
        assert_eq!(t.hidden, 0);
    }

    #[test]
    fn 같은_버튼을_연달아_눌러도_앞선_액션의_요청은_배경이_되지_않는다() {
        let mut store = TrafficStore::default();
        let mut windows = ActionWindows::default();
        for (i, since) in [1_000u64, 2_000, 3_000].into_iter().enumerate() {
            fetch(
                &mut store,
                &format!("c{i}"),
                "https://api.example.com/cart",
                since + 10,
            );
            let t = windows.collect_triggered_requests(&store, since, None, since + 100);
            assert_eq!(ids(&t.shown), [format!("c{i}")]);
            assert_eq!(t.hidden, 0);
        }
    }

    #[test]
    fn 액션_구간은_최근_50개만_기억한다() {
        let store = TrafficStore::default();
        let mut windows = ActionWindows::default();
        for since in 0..60 {
            windows.collect_triggered_requests(&store, since, None, since);
        }
        assert_eq!(windows.windows.len(), MAX_WINDOWS);
        assert_eq!(windows.windows.front(), Some(&(10, 10)));
    }
}
