//! Port of `src/main/mcp/tools/ui-crawl.ts` — the exhaustive click crawler.
//!
//! P2 of docs/feature-plan-red-team-ui-crawl.md. A deterministic loop (the code
//! presses, the AI never decides what to skip): snapshot → click each actionable
//! element once → attribute the requests it fired → recover (navigate back /
//! close a modal) → repeat. The P1 fence makes it safe: mutating requests are
//! captured and answered with a 204, never sent.
//!
//! The fence holds requests through `Fetch.enable`, and `Browser` cannot
//! intercept requests. Clicking everything on a page without it would send what
//! the fence exists to hold back, so the `ui_crawl` tool validates its arguments
//! and then reports the interception as unsupported. The loop itself is
//! [`crawl`], for an embedder that does hold the requests.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use rever_analysis::triggered_requests::ActionWindows;
use rever_scope::fence::{acquire_fence, fence_reason, release_fence};
use rever_scope::scope::{get_scope, scope_block_for_url};
use rever_snapshot::snapshot::SnapshotOptions;
use rever_tools_api::tool::{ToolContext, ToolDef, ToolRegistry, unsupported};
use serde_json::json;
use tokio::time::Instant;
use url::Url;

use crate::gestures::{
    Settle, click_ref, navigate_history, page_url, snapshotter, wait_for_settle,
};
use crate::human_input::HumanInput;
use crate::{js_number, js_space, lock, no_tab, now_ms, schema, tab_schema};

const ACTION_WINDOW_MS: u64 = 500;
const DEFAULT_MAX_ELEMENTS: f64 = 40.0;
const HARD_MAX_ELEMENTS: f64 = 150.0;
const MAX_MS: u64 = 60_000;

pub fn register(registry: &mut ToolRegistry) {
    registry.register(UiCrawl);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElemStatus {
    Clicked,
    BlockedDestructive,
    Failed,
    Navigated,
    Noop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElemResult {
    pub label: String,
    pub status: ElemStatus,
    /// "METHOD origin+path" signatures
    pub endpoints: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actionable {
    pub r#ref: String,
    pub label: String,
    pub key: String,
}

/// The first `[ref=rN]` at or after byte `from`: where it starts and ends.
fn find_ref(line: &str, from: usize) -> Option<(usize, usize)> {
    const OPEN: &str = "[ref=r";
    let mut search = from;
    while let Some(found) = line.get(search..)?.find(OPEN) {
        let start = search + found;
        let digits_from = start + OPEN.len();
        let rest = line.get(digits_from..)?;
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && rest.as_bytes().get(digits) == Some(&b']') {
            return Some((start, digits_from + digits + 1));
        }
        search = digits_from;
    }
    None
}

// Pull actionable nodes (the ones the snapshot tagged with [ref=rN]) out of the
// YAML-like outline. The label (role + accessible name) is a stable identity
// across re-snapshots — refs are reassigned each snapshot, labels are not — so
// it doubles as the visited key. Same-labelled elements collapse to one, which
// matches "several buttons firing the same request are one finding".
pub fn parse_actionable(tree: &str) -> Vec<Actionable> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for raw in tree.split('\n') {
        let Some((start, end)) = find_ref(raw, 0) else {
            continue;
        };
        let r = raw.get(start + 5..end - 1).unwrap_or_default();

        // `raw.replace(/\s*\[ref=r\d+\].*$/, '')`: `.` stops at a line terminator,
        // so the cut is at the first ref with none after it.
        let line_end = |c: char| matches!(c, '\r' | '\u{2028}' | '\u{2029}');
        let mut cut = Some((start, end));
        while let Some((_, tag_end)) = cut {
            let tail = raw.get(tag_end..).unwrap_or_default();
            if !tail.contains(line_end) {
                break;
            }
            cut = find_ref(raw, tag_end);
        }
        let head = cut
            .and_then(|(tag_start, _)| raw.get(..tag_start))
            .map_or(raw, |head| head.trim_end_matches(js_space));
        // `.replace(/^[\s\-•|`]+/, '').trim()`
        let label = head
            .trim_start_matches(|c: char| js_space(c) || matches!(c, '-' | '•' | '|' | '`'))
            .trim_matches(js_space);

        let key = if label.is_empty() { r } else { label };
        if !seen.insert(key.to_owned()) {
            continue;
        }
        out.push(Actionable {
            r#ref: r.to_owned(),
            label: label.to_owned(),
            key: key.to_owned(),
        });
    }
    out
}

// method + origin + path, query stripped — the endpoint identity for dedup and
// for feeding the probe phase (scan.ts parseEndpoints reads "METHOD URL").
pub fn endpoint_sig(method: &str, url: &str) -> String {
    let clean = match Url::parse(url) {
        Ok(parsed) => format!("{}{}", parsed.origin().ascii_serialization(), parsed.path()),
        Err(_) => {
            // `url.replace(/[?#].*$/, '')`: `.` stops at a line terminator and `$`
            // is the end of the text, so only the last line can be cut.
            let line_end = |c: char| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}');
            let last_line = url
                .char_indices()
                .rev()
                .find(|(_, c)| line_end(*c))
                .map_or(0, |(at, c)| at + c.len_utf8());
            let cut = url
                .get(last_line..)
                .and_then(|tail| tail.find(['?', '#']))
                .map_or(url.len(), |at| last_line + at);
            url.get(..cut).unwrap_or(url).to_owned()
        }
    };
    format!("{} {clean}", method.to_uppercase())
}

// Whether a triggered request was (or would be) held by the fence — the same
// decision the network layer applies: in-scope + mutating.
fn was_blocked(method: &str, url: &str) -> bool {
    get_scope().enabled
        && scope_block_for_url(Some(url)).is_none()
        && fence_reason(method, url).is_some()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CrawlResult {
    pub start_url: String,
    pub elements: Vec<ElemResult>,
    pub endpoints: Vec<String>,
}

/// How many elements a crawl may click: `maxElements`, at least 1, at most the hard cap.
pub fn click_budget(max_elements: Option<f64>) -> usize {
    max_elements
        .unwrap_or(DEFAULT_MAX_ELEMENTS)
        .clamp(1.0, HARD_MAX_ELEMENTS) as usize
}

/// Clicks every actionable element of the page once and maps each to the
/// requests it fired.
///
/// Nothing here holds a request back. The caller must have mutating requests
/// captured instead of sent (the fence) for as long as this runs.
pub async fn crawl(ctx: &ToolContext, budget: usize) -> CrawlResult {
    let mut visited: HashSet<String> = HashSet::new();
    let mut result = CrawlResult::default();
    let deadline = Instant::now() + Duration::from_millis(MAX_MS);

    let mut clicks = 0;
    while clicks < budget && Instant::now() < deadline {
        let Ok(snap) = snapshotter(ctx)
            .take_snapshot(ctx.browser.as_ref(), SnapshotOptions::default())
            .await
        else {
            break;
        };
        let url = snap.url;
        if result.start_url.is_empty() {
            result.start_url.clone_from(&url);
        }

        let actionable = parse_actionable(&snap.tree);
        let Some(next) = actionable.into_iter().find(|a| !visited.contains(&a.key)) else {
            break;
        };
        visited.insert(next.key);
        clicks += 1;

        let since = now_ms().saturating_sub(ACTION_WINDOW_MS);
        if click_ref(ctx, &next.r#ref).await.is_err() {
            result.elements.push(ElemResult {
                label: next.label,
                status: ElemStatus::Failed,
                endpoints: Vec::new(),
            });
            continue;
        }
        wait_for_settle(ctx, Settle::default()).await;

        let mut eps: Vec<String> = Vec::new();
        let mut blocked_any = false;
        {
            let traffic = lock(&ctx.traffic);
            let windows = ctx.state::<Mutex<ActionWindows>>();
            let mut windows = lock(&windows);
            let triggered = windows.collect_triggered_requests(&traffic, since, None, now_ms());
            for request in triggered.shown {
                let sig = endpoint_sig(&request.method, &request.url);
                if !eps.contains(&sig) {
                    eps.push(sig.clone());
                }
                if !result.endpoints.contains(&sig) {
                    result.endpoints.push(sig);
                }
                if was_blocked(&request.method, &request.url) {
                    blocked_any = true;
                }
            }
        }

        // Recover: a navigation leaves the crawl on the wrong page — go back; a
        // same-page click may have opened a modal — Escape closes the common ones.
        let mut navigated = false;
        let after = page_url(ctx).await;
        if !after.is_empty() && !url.is_empty() && after != url {
            navigated = true;
            if navigate_history(ctx, "back").await.is_ok() {
                wait_for_settle(ctx, Settle::default()).await;
            }
        } else {
            let human = ctx.state::<HumanInput>();
            let _ = human.press_key(&ctx.browser, "Escape", &[], None).await;
        }

        let status = match (eps.is_empty(), blocked_any, navigated) {
            (false, true, _) => ElemStatus::BlockedDestructive,
            (false, false, _) => ElemStatus::Clicked,
            (true, _, true) => ElemStatus::Navigated,
            (true, _, false) => ElemStatus::Noop,
        };
        result.elements.push(ElemResult {
            label: next.label,
            status,
            endpoints: eps,
        });
    }
    result
}

pub fn format_crawl(tab: Option<f64>, r: &CrawlResult) -> String {
    let by = |status: ElemStatus| r.elements.iter().filter(|e| e.status == status).count();
    let summary = format!(
        "elements: {} — clicked {}, blocked-destructive {}, failed {}, navigated {}, noop {}",
        r.elements.len(),
        by(ElemStatus::Clicked),
        by(ElemStatus::BlockedDestructive),
        by(ElemStatus::Failed),
        by(ElemStatus::Navigated),
        by(ElemStatus::Noop)
    );
    let map: Vec<String> = r
        .elements
        .iter()
        .filter(|e| !e.endpoints.is_empty())
        .map(|e| {
            let tag = if e.status == ElemStatus::BlockedDestructive {
                "  [blocked]"
            } else {
                ""
            };
            format!("  \"{}\" → {}{tag}", e.label, e.endpoints.join(", "))
        })
        .collect();
    let tab = tab.map_or(String::new(), |tab| format!(" (tab {})", js_number(tab)));
    let map = if map.is_empty() {
        String::new()
    } else {
        format!("element → API map:\n{}\n", map.join("\n"))
    };
    format!(
        "--- ui_crawl{tab} ---\npage: {}\n{summary}\n{map}endpoints discovered:\n{}",
        r.start_url,
        json!(r.endpoints)
    )
}

tool!(
    UiCrawl,
    ToolDef::new(
        "ui_crawl",
        "Red Team exhaustive-click crawler: deterministically clicks every actionable element on the page once, attributes the requests each fired (button→API map), and returns the discovered endpoints for the probe phase. On non-loopback hosts, mutating requests are captured and answered with a synthetic 204 by the capture-not-send fence — the server state is never changed; loopback / lab_target hosts pass through so local benchmarks can be confirmed. Pass `tab` (browser_tabs index) to crawl a background tab.",
        schema(
            json!({
                "tab": tab_schema("Tab index from browser_tabs (default: the selected tab)"),
                "maxElements": tab_schema(&format!(
                    "Max elements to click (default {DEFAULT_MAX_ELEMENTS}, hard cap {HARD_MAX_ELEMENTS})"
                )),
            }),
            &[]
        )
    ),
    |ctx, args| {
        no_tab(args)?;
        // Without the fence a crawl would really send every mutating request it
        // triggers, on a tool that promises the server state is never changed.
        if !ctx.browser.holds_fenced_requests() {
            return Err(unsupported(
                "holding mutating requests back (the capture-not-send fence)",
            ));
        }
        let budget = click_budget(args.opt_number("maxElements"));
        // Released when this goes out of scope, also if the call is abandoned midway.
        struct Fence;
        impl Drop for Fence {
            fn drop(&mut self) {
                release_fence();
            }
        }
        acquire_fence();
        let _fence = Fence;
        let result = crawl(ctx, budget).await;
        Ok(format_crawl(None, &result))
    }
);

#[cfg(test)]
mod tests {
    use rever_core::traffic::RequestPatch;
    use serde_json::Value;

    use super::*;
    use crate::test_support::{FakeBrowser, SCOPE, call, context};

    // --- ported from ui-crawl.test.ts ---

    #[test]
    fn ref_가_붙은_행만_뽑고_라벨로_중복을_제거한다() {
        let tree = [
            "- document \"App\"",
            "  - button \"Load results\" [ref=r3]",
            "  - link \"Docs\" [ref=r4]",
            "  - text \"just a label\"",
            "  - button \"Load results\" [ref=r9]", // same label, different ref → one entry
        ]
        .join("\n");
        let out = parse_actionable(&tree);
        let refs: Vec<&str> = out.iter().map(|a| a.r#ref.as_str()).collect();
        assert_eq!(refs, ["r3", "r4"]);
        let labels: Vec<&str> = out.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(labels, ["button \"Load results\"", "link \"Docs\""]);
    }

    #[test]
    fn ref_없는_트리는_빈_배열() {
        assert_eq!(parse_actionable("- document \"x\"\n  - text \"y\""), []);
    }

    #[test]
    fn 라벨이_비면_ref_를_키로_쓴다() {
        let out = parse_actionable("  - [ref=r1]");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "r1");
    }

    #[test]
    fn 메서드를_대문자화하고_쿼리_프래그먼트를_제거한다() {
        assert_eq!(
            endpoint_sig("get", "https://x.test/api/items?page=2#frag"),
            "GET https://x.test/api/items"
        );
        assert_eq!(
            endpoint_sig("POST", "https://x.test/api/items"),
            "POST https://x.test/api/items"
        );
    }

    #[test]
    fn 파싱_불가_url_도_쿼리만_떼고_보존한다() {
        assert_eq!(endpoint_sig("GET", "not a url?x=1"), "GET not a url");
    }

    // --- not covered by the TypeScript tests ---

    #[test]
    fn ref_뒤의_표시는_버리고_앞의_들여쓰기와_글머리표를_떼어_라벨로_쓴다() {
        let out = parse_actionable(
            "\t• | `- textbox \"Search\" [ref=r12] *new [focused]\n- link \"A\" [ref=r2] [ref=r3]",
        );
        assert_eq!(
            out,
            [
                Actionable {
                    r#ref: "r12".to_owned(),
                    label: "textbox \"Search\"".to_owned(),
                    key: "textbox \"Search\"".to_owned(),
                },
                Actionable {
                    r#ref: "r2".to_owned(),
                    label: "link \"A\"".to_owned(),
                    key: "link \"A\"".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn ref_모양이_아닌_것은_ref로_보지_않는다() {
        // No digits, a letter after the digits, a missing bracket.
        assert_eq!(
            parse_actionable("- a [ref=r]\n- b [ref=r1x]\n- c [ref=r2"),
            []
        );
        // The first well-formed one on the line counts.
        let out = parse_actionable("- a [ref=r] b [ref=r7]");
        assert_eq!(out[0].r#ref, "r7");
        assert_eq!(out[0].label, "a [ref=r] b");
        // Not ASCII digits.
        assert_eq!(parse_actionable("- a [ref=r٣]"), []);
    }

    #[test]
    fn 줄_끝_문자가_뒤따르는_ref에서는_라벨을_자르지_않는다() {
        // `.*$` cannot cross a carriage return, so the replace finds nothing to cut.
        let out = parse_actionable("- a [ref=r1]\rb");
        assert_eq!(out[0].r#ref, "r1");
        assert_eq!(out[0].label, "a [ref=r1]\rb");
        // A later ref with a clean tail is where it cuts instead.
        let out = parse_actionable("- a [ref=r1]\rb [ref=r2] x");
        assert_eq!(out[0].label, "a [ref=r1]\rb");
    }

    #[test]
    fn 엔드포인트_서명은_출처와_경로만_남긴다() {
        // Default ports drop, the host lowercases, credentials go: the URL's origin.
        assert_eq!(
            endpoint_sig("delete", "HTTPS://user:pw@X.test:443/a/../b?q#f"),
            "DELETE https://x.test/b"
        );
        assert_eq!(
            endpoint_sig("get", "http://127.0.0.1:8777"),
            "GET http://127.0.0.1:8777/"
        );
        // An origin-less scheme prints "null", as `new URL(...).origin` does.
        assert_eq!(
            endpoint_sig("get", "data:text/plain,hi?x"),
            "GET nulltext/plain,hi"
        );
        assert_eq!(endpoint_sig("get", "/relative#top"), "GET /relative");
        assert_eq!(endpoint_sig("", ""), " ");
    }

    #[test]
    fn 까다로운_입력에서도_타입스크립트와_같은_결과를_낸다() {
        // Expected values are what the TypeScript prints in bun for these inputs.
        for (url, want) in [
            // `.` stops at a line terminator, so `[?#].*$` only cuts on the last line.
            ("not a url?x=1\nfoo#bar", "GET not a url?x=1\nfoo"),
            ("a?b\nc", "GET a?b\nc"),
            ("a?b\u{2028}c?d", "GET a?b\u{2028}c"),
            (
                "blob:https://x.test/uuid?q",
                "GET https://x.testhttps://x.test/uuid",
            ),
            ("file:///a/b?x", "GET null/a/b"),
            (
                "HTTP://EXAMPLE.com:80/a b?x",
                "GET http://example.com/a%20b",
            ),
            ("https://x.test", "GET https://x.test/"),
            ("ws://x.test:443/s?q", "GET ws://x.test:443/s"),
            ("about:blank?x", "GET nullblank"),
            ("javascript:alert(1)?x", "GET nullalert(1)"),
            (
                "https://пример.рф/путь?x",
                "GET https://xn--e1afmkfd.xn--p1ai/%D0%BF%D1%83%D1%82%D1%8C",
            ),
            ("http://[::1]:8080/a?b", "GET http://[::1]:8080/a"),
            ("//x.test/a?b", "GET //x.test/a"),
            ("mailto:a@b?subject=x", "GET nulla@b"),
            (
                "https://x.test/a%20b/./c/../d?z",
                "GET https://x.test/a%20b/d",
            ),
            ("http://x.test\\a\\b?q", "GET http://x.test/a/b"),
            ("  https://x.test/a?q  ", "GET https://x.test/a"),
            ("https://x.test:99999/", "GET https://x.test:99999/"),
            ("http://1.2.3/x", "GET http://1.2.0.3/x"),
            ("http://0x7f.1/x", "GET http://127.0.0.1/x"),
            ("x:?a#b", "GET null"),
        ] {
            assert_eq!(endpoint_sig("geT", url), want, "{url:?}");
        }
        assert_eq!(endpoint_sig("ß", "x"), "SS x");

        let one = |tree: &str| {
            let out = parse_actionable(tree);
            assert_eq!(out.len(), 1, "{tree:?}");
            (
                out[0].r#ref.clone(),
                out[0].label.clone(),
                out[0].key.clone(),
            )
        };
        let entry =
            |r: &str, label: &str, key: &str| (r.to_owned(), label.to_owned(), key.to_owned());
        assert_eq!(
            one("- a [ref=r1] [x]\u{2028} b"),
            entry("r1", "a [ref=r1] [x]\u{2028} b", "a [ref=r1] [x]\u{2028} b")
        );
        assert_eq!(one("\u{a0}- b [ref=r2]\u{3000}"), entry("r2", "b", "b"));
        assert_eq!(one("- c [ref=r03] [ref=r4]"), entry("r03", "c", "c"));
        assert_eq!(one("  -  [ref=r5] x"), entry("r5", "", "r5"));
        assert_eq!(one("- d\t [ref=r6]"), entry("r6", "d", "d"));
        assert_eq!(one("-•|` e [ref=r7]"), entry("r7", "e", "e"));
        assert_eq!(
            one("- f [ref=r8]\r"),
            entry("r8", "f [ref=r8]", "f [ref=r8]")
        );
    }

    #[test]
    fn 클릭_예산은_1에서_150_사이로_맞춘다() {
        assert_eq!(click_budget(None), 40);
        assert_eq!(click_budget(Some(0.0)), 1);
        assert_eq!(click_budget(Some(-5.0)), 1);
        assert_eq!(click_budget(Some(7.0)), 7);
        assert_eq!(click_budget(Some(1e300)), 150);
    }

    fn element(label: &str, status: ElemStatus, endpoints: &[&str]) -> ElemResult {
        ElemResult {
            label: label.to_owned(),
            status,
            endpoints: endpoints.iter().map(|e| (*e).to_owned()).collect(),
        }
    }

    #[test]
    fn 결과는_요약과_요소별_api와_발견한_엔드포인트를_적는다() {
        let result = CrawlResult {
            start_url: "https://a.test/".to_owned(),
            elements: vec![
                element(
                    "button \"Load\"",
                    ElemStatus::Clicked,
                    &["GET https://a.test/api/items"],
                ),
                element(
                    "button \"Delete\"",
                    ElemStatus::BlockedDestructive,
                    &[
                        "DELETE https://a.test/api/items/1",
                        "GET https://a.test/api/items",
                    ],
                ),
                element("link \"Docs\"", ElemStatus::Navigated, &[]),
                element("button \"Broken\"", ElemStatus::Failed, &[]),
                element("button \"Nothing\"", ElemStatus::Noop, &[]),
            ],
            endpoints: vec![
                "GET https://a.test/api/items".to_owned(),
                "DELETE https://a.test/api/items/1".to_owned(),
            ],
        };
        assert_eq!(
            format_crawl(None, &result),
            "--- ui_crawl ---\n\
             page: https://a.test/\n\
             elements: 5 — clicked 1, blocked-destructive 1, failed 1, navigated 1, noop 1\n\
             element → API map:\n  \"button \"Load\"\" → GET https://a.test/api/items\n  \"button \"Delete\"\" → DELETE https://a.test/api/items/1, GET https://a.test/api/items  [blocked]\n\
             endpoints discovered:\n\
             [\"GET https://a.test/api/items\",\"DELETE https://a.test/api/items/1\"]"
        );
    }

    #[test]
    fn 요청이_없었던_결과는_api_표_없이_적는다() {
        assert_eq!(
            format_crawl(Some(2.0), &CrawlResult::default()),
            "--- ui_crawl (tab 2) ---\npage: \nelements: 0 — clicked 0, blocked-destructive 0, failed 0, navigated 0, noop 0\nendpoints discovered:\n[]"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 요청을_붙잡아_둘_수_없어_도구는_인자만_확인하고_거절한다() {
        let (browser, ctx) = context(FakeBrowser::new());
        assert_eq!(
            call(&UiCrawl, &ctx, json!({ "tab": 1 })).await,
            Err(unsupported("driving a tab by index"))
        );
        for args in [json!({}), json!({ "maxElements": 3 })] {
            assert_eq!(
                call(&UiCrawl, &ctx, args).await,
                Err(unsupported(
                    "holding mutating requests back (the capture-not-send fence)"
                ))
            );
        }
        // Nothing was clicked, and the fence was not left held.
        assert!(browser.calls().is_empty() && browser.inputs().is_empty());
        assert!(!rever_scope::fence::is_fence_active());
    }

    fn page(buttons: &[&str]) -> Value {
        let mut nodes = vec![json!({
            "nodeId": "1", "role": { "value": "RootWebArea" },
            "childIds": (0..buttons.len()).map(|i| (i + 2).to_string()).collect::<Vec<_>>()
        })];
        for (i, name) in buttons.iter().enumerate() {
            nodes.push(json!({
                "nodeId": (i + 2).to_string(), "role": { "value": "button" },
                "name": { "value": name }, "backendDOMNodeId": i + 10
            }));
        }
        json!({ "meta": { "url": "https://a.test/", "title": "A" }, "nodes": nodes })
    }

    fn crawl_browser(buttons: &[&str], evaluations: Vec<Result<Value, String>>) -> FakeBrowser {
        let browser = FakeBrowser::answering(evaluations);
        *browser.capture.lock().unwrap() = Some(page(buttons));
        browser
    }

    #[tokio::test(start_paused = true)]
    async fn 요소를_라벨마다_한_번씩_누르고_끝나면_멈춘다() {
        let here = || Ok(json!("https://a.test/"));
        let focused = || Ok(json!(true));
        // Per click: where the page is, then the focus check of Escape.
        let (browser, ctx) = context(crawl_browser(
            &["One", "Two", "One"],
            vec![here(), focused(), here(), focused()],
        ));
        let result = crawl(&ctx, 40).await;
        assert_eq!(result.start_url, "https://a.test/");
        assert_eq!(
            result.elements,
            [
                element("button \"One\"", ElemStatus::Noop, &[]),
                element("button \"Two\"", ElemStatus::Noop, &[]),
            ]
        );
        assert!(result.endpoints.is_empty());
        // Two clicks, each followed by Escape; the third snapshot finds nothing new.
        let inputs = browser.inputs();
        let downs = inputs
            .iter()
            .filter(|i| matches!(i, rever_tools_api::tool::Input::MouseDown { .. }));
        assert_eq!(downs.count(), 2);
        let escapes = inputs
            .iter()
            .filter(|i| matches!(i, rever_tools_api::tool::Input::Key { key } if key == "Escape"));
        assert_eq!(escapes.count(), 2);
        let captures = browser.calls();
        let captures = captures
            .iter()
            .filter(|c| c.starts_with("script:(\"capture\""));
        assert_eq!(captures.count(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn 예산만큼만_누른다() {
        let (_, ctx) = context(crawl_browser(&["One", "Two", "Three"], Vec::new()));
        let result = crawl(&ctx, 1).await;
        assert_eq!(result.elements.len(), 1);
        assert_eq!(crawl(&ctx, 0).await, CrawlResult::default());
    }

    #[tokio::test(start_paused = true)]
    async fn 스냅샷을_못_뜨면_거기서_끝낸다() {
        let (browser, ctx) = context(FakeBrowser::new());
        *browser.capture.lock().unwrap() = None;
        assert_eq!(crawl(&ctx, 5).await, CrawlResult::default());
    }

    #[tokio::test(start_paused = true)]
    async fn 누르지_못한_요소는_실패로_적고_다음으로_넘어간다() {
        let gone =
            json!({ "error": "ref \"r1\" is no longer on the page — call browser_snapshot again" });
        let browser = crawl_browser(&["One", "Two"], Vec::new()).with_actions([gone]);
        let (_, ctx) = context(browser);
        let result = crawl(&ctx, 40).await;
        assert_eq!(
            result.elements,
            [
                element("button \"One\"", ElemStatus::Failed, &[]),
                element("button \"Two\"", ElemStatus::Noop, &[]),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 페이지가_바뀌면_뒤로_돌아가고_이동으로_적는다() {
        let (browser, ctx) = context(crawl_browser(
            &["Go"],
            vec![Ok(json!("https://a.test/next")), Ok(json!(2))],
        ));
        let result = crawl(&ctx, 40).await;
        assert_eq!(
            result.elements,
            [element("button \"Go\"", ElemStatus::Navigated, &[])]
        );
        let calls = browser.calls();
        assert!(calls.contains(&"back".to_owned()));
        // No Escape on the way back.
        assert!(
            !browser
                .inputs()
                .iter()
                .any(|i| matches!(i, rever_tools_api::tool::Input::Key { .. }))
        );
    }

    /// A request the click of this moment sets off: the fake page answers at once.
    fn fire_request(ctx: &ToolContext, id: &str, method: &str, url: &str) {
        let patch = RequestPatch {
            request_id: id.to_owned(),
            url: Some(url.to_owned()),
            method: Some(method.to_owned()),
            resource_type: Some("Fetch".to_owned()),
            started_at: Some(now_ms()),
            completed_at: Some(now_ms()),
            ..RequestPatch::default()
        };
        lock(&ctx.traffic).upsert(patch, now_ms());
    }

    #[tokio::test(start_paused = true)]
    async fn 누른_요소가_일으킨_요청을_엔드포인트로_모은다() {
        let _scope = SCOPE.lock().await;
        let (_, ctx) = context(crawl_browser(&["Load"], Vec::new()));
        // Newest first: requests of the same millisecond list in reverse.
        fire_request(&ctx, "3", "POST", "https://a.test/api/items");
        fire_request(&ctx, "2", "GET", "https://a.test/api/items?page=2");
        fire_request(&ctx, "1", "get", "https://a.test/api/items?page=1");

        // No scope is set, so nothing counts as held by the fence.
        let result = crawl(&ctx, 40).await;
        let endpoints = [
            "GET https://a.test/api/items",
            "POST https://a.test/api/items",
        ];
        assert_eq!(
            result.elements,
            [element("button \"Load\"", ElemStatus::Clicked, &endpoints)]
        );
        assert_eq!(result.endpoints, endpoints);
    }

    #[tokio::test(start_paused = true)]
    async fn 범위_안의_변경_요청은_막힌_것으로_적는다() {
        let _scope = SCOPE.lock().await;
        rever_scope::scope::set_scope(&["a.test"]);
        let blocked = (
            was_blocked("POST", "https://a.test/api/items"),
            was_blocked("GET", "https://a.test/api/items"),
            was_blocked("GET", "https://a.test/logout"),
            // Out of scope, and loopback: the fence lets both through.
            was_blocked("POST", "https://other.test/api"),
            was_blocked("POST", "http://127.0.0.1:8777/api"),
        );
        let (_, ctx) = context(crawl_browser(&["Delete"], Vec::new()));
        fire_request(&ctx, "1", "DELETE", "https://a.test/api/items/1");
        let result = crawl(&ctx, 40).await;
        rever_scope::scope::clear_scope();

        assert_eq!(blocked, (true, false, true, false, false));
        assert_eq!(
            result.elements,
            [element(
                "button \"Delete\"",
                ElemStatus::BlockedDestructive,
                &["DELETE https://a.test/api/items/1"]
            )]
        );
        assert!(!was_blocked("POST", "https://a.test/api/items"));
    }
}
