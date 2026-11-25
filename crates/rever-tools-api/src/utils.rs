//! Port of `src/main/mcp/utils.ts` — tool-result helpers, the replay header
//! merge, and the size budget that parks oversized results for paging.
//!
//! Every length, offset and limit here counts UTF-16 code units, like the
//! JavaScript it replaces: the agent pages with the numbers printed in the
//! footers, so they must mean the same thing on both sides.

use std::collections::VecDeque;
use std::fmt::Display;

use rever_core::traffic::Headers;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Content {
    Text { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    // A successful result has no `isError` key at all, not `isError: false`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    pub content: Vec<Content>,
}

pub fn ok(text: impl Into<String>) -> ToolResult {
    ToolResult {
        is_error: false,
        content: vec![Content::Text { text: text.into() }],
    }
}

/// A result whose one block is an image, written as a `data:` URL.
///
/// shortcut: `Content` only has text, and a new kind breaks every exhaustive match
/// on it, including crates this port must leave alone. The MCP server turns a
/// block that is exactly one image data URL into an image block
/// ([`parse_image_data_url`]); make it a `Content` variant once those can change.
pub fn ok_image(mime_type: &str, base64: &str) -> ToolResult {
    ok(format!("data:{mime_type};base64,{base64}"))
}

/// `(mime type, base64 data)` when `text` is one whole image data URL.
pub fn parse_image_data_url(text: &str) -> Option<(&str, &str)> {
    let (mime_type, data) = text.strip_prefix("data:")?.split_once(";base64,")?;
    let subtype = mime_type.strip_prefix("image/")?;
    let token = |c: char| c.is_ascii_alphanumeric() || "+-.".contains(c);
    let base64 = |c: char| c.is_ascii_alphanumeric() || "+/=".contains(c);
    (!subtype.is_empty()
        && subtype.chars().all(token)
        && !data.is_empty()
        && data.chars().all(base64))
    .then_some((mime_type, data))
}

pub fn err(text: impl Into<String>) -> ToolResult {
    ToolResult {
        is_error: true,
        content: vec![Content::Text { text: text.into() }],
    }
}

pub fn error_message(e: impl Display) -> String {
    e.to_string()
}

// --- replay_request header merge -------------------------------------------
// Two failure modes this merge must prevent: CDP records lowercase header
// names while agents override capitalized ones (a case-sensitive spread keeps
// both, so `Authorization: ""` never strips `authorization: Bearer …`), and
// the page-cookie injection must not stomp an explicit cookie override.
// Normalize to lowercase, inject cookies BEFORE overrides, and treat an
// override value of "" as "remove this header" — the credential-strip idiom
// security scans rely on.
pub fn merge_replay_headers(
    captured: Option<&Headers>,
    cookie_header: &str,
    override_headers: Option<&Headers>,
) -> Headers {
    let mut out = Headers::new();
    for (k, v) in captured.into_iter().flatten() {
        out.insert(k.to_lowercase(), v.clone());
    }
    if !cookie_header.is_empty() {
        out.insert("cookie".to_owned(), cookie_header.to_owned());
    }
    for (k, v) in override_headers.into_iter().flatten() {
        out.insert(k.to_lowercase(), v.clone());
    }
    out.retain(|_, v| !v.is_empty());
    out
}

// --- Tool-result size budgeting -------------------------------------------
// Large tool results (full request bodies, snapshots, bundle dumps) can eat
// tens of thousands of tokens of agent context in one call. ok_budgeted() caps
// what it returns and parks the full text in a small LRU so the agent can page
// the rest with tool_result_more. Keep one store per process (not per MCP
// session) so a result produced in one session can be paged from another
// (e.g. bin/revb.mjs).

const DEFAULT_MAX_CHARS: usize = 12_000;
const MAX_STORED_RESULTS: usize = 50;
// Count alone doesn't bound memory — 50 multi-MB bodies would pin hundreds of
// MB in the main process — so the total size is capped too.
pub const MAX_STORED_CHARS: usize = 20_000_000;

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

// Splits `s` at the UTF-16 offset `units` (clamped to the end) and returns
// the two halves plus the offset actually used. A Rust string cannot hold
// half a surrogate pair, so an offset that lands inside one moves to the
// start of that character, or past it when `forward` is set.
fn split_at_utf16(s: &str, units: usize, forward: bool) -> (&str, &str, usize) {
    let mut seen = 0;
    let mut cut = s.len();
    for (byte, ch) in s.char_indices() {
        let next = seen + ch.len_utf16();
        if seen >= units || (next > units && !forward) {
            cut = byte;
            break;
        }
        seen = next;
    }
    let (head, tail) = s.split_at_checked(cut).unwrap_or((s, ""));
    (head, tail, seen)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget<'a> {
    pub head: &'a str,
    pub truncated: bool,
    pub shown_chars: usize,
}

// Cut `text` to at most `max_chars`, preferring the last newline so a slice
// doesn't end mid-line. Returns the head plus how many chars it kept.
pub fn budget_text(text: &str, max_chars: Option<usize>) -> Budget<'_> {
    let max_chars = max_chars.unwrap_or(DEFAULT_MAX_CHARS);
    let total = utf16_len(text);
    if total <= max_chars {
        return Budget {
            head: text,
            truncated: false,
            shown_chars: total,
        };
    }
    let (hard_slice, _, hard_chars) = split_at_utf16(text, max_chars, false);
    // Only snap to a newline if it keeps most of the budget (avoid tiny slices).
    if let Some((head, _)) = hard_slice.rsplit_once('\n') {
        let last_nl = utf16_len(head);
        // Integer form of `lastNl > maxChars * 0.5`.
        if last_nl > max_chars / 2 {
            return Budget {
                head,
                truncated: true,
                shown_chars: last_nl,
            };
        }
    }
    Budget {
        head: hard_slice,
        truncated: true,
        shown_chars: hard_chars,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultSlice {
    pub slice: String,
    pub offset: usize,
    pub end: usize,
    pub total: usize,
    pub more: bool,
    pub next_offset: Option<usize>,
}

#[derive(Debug)]
struct StoredResult {
    id: String,
    full: String,
    chars: usize,
}

/// The parked full texts, oldest first.
#[derive(Debug, Default)]
pub struct ResultStore {
    entries: VecDeque<StoredResult>,
    result_seq: u64,
    stored_chars: usize,
}

impl ResultStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn put_result(&mut self, full: String) -> String {
        self.result_seq += 1;
        let id = format!("res_{}", self.result_seq);
        let chars = utf16_len(&full);
        self.stored_chars += chars;
        self.entries.push_back(StoredResult {
            id: id.clone(),
            full,
            chars,
        });
        // Evict oldest first, but always keep the result just stored.
        while self.entries.len() > 1
            && (self.entries.len() > MAX_STORED_RESULTS || self.stored_chars > MAX_STORED_CHARS)
        {
            let Some(oldest) = self.entries.pop_front() else {
                break;
            };
            self.stored_chars = self.stored_chars.saturating_sub(oldest.chars);
        }
        id
    }

    pub fn ok_budgeted(&mut self, text: impl Into<String>, max_chars: Option<usize>) -> ToolResult {
        let text = text.into();
        let Budget {
            head,
            truncated,
            shown_chars,
        } = budget_text(&text, max_chars);
        if !truncated {
            return ok(text);
        }
        let head = head.to_owned();
        let total = utf16_len(&text);
        let id = self.put_result(text);
        ok(format!(
            "{head}\n\n[truncated: showing {shown_chars} of {total} chars. \
             Call tool_result_more with resultId=\"{id}\", offset={shown_chars} for the rest.]"
        ))
    }

    // For views that have a cheap summary form (e.g. get_request): full=true pages
    // the full text, full=false forces the summary, and unset returns the full text
    // when it fits the budget — so small entries still cost one call.
    pub fn ok_full_or_summary<S: Serialize>(
        &mut self,
        full_text: impl Into<String>,
        summarize: impl FnOnce() -> S,
        full: Option<bool>,
    ) -> Result<ToolResult, serde_json::Error> {
        let full_text = full_text.into();
        if full == Some(true) {
            return Ok(self.ok_budgeted(full_text, None));
        }
        if full.is_none() && !budget_text(&full_text, None).truncated {
            return Ok(ok(full_text));
        }
        Ok(ok(serde_json::to_string_pretty(&summarize())?))
    }

    pub fn get_result_slice(
        &mut self,
        id: &str,
        offset: usize,
        limit: Option<usize>,
    ) -> Option<ResultSlice> {
        let position = self.entries.iter().position(|e| e.id == id)?;
        // Refresh LRU recency on access.
        let entry = self.entries.remove(position)?;
        self.entries.push_back(entry);
        let entry = self.entries.back()?;

        let limit = limit.unwrap_or(DEFAULT_MAX_CHARS).max(1);
        let (_, rest, safe_offset) = split_at_utf16(&entry.full, offset, false);
        // Forward, so a slice that starts on an astral character is never
        // empty: the pager would be handed the same offset again forever.
        let (slice, _, slice_chars) = split_at_utf16(rest, limit, true);
        let end = safe_offset + slice_chars;
        let more = end < entry.chars;
        Some(ResultSlice {
            slice: slice.to_owned(),
            offset: safe_offset,
            end,
            total: entry.chars,
            more,
            next_offset: more.then_some(end),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn headers(pairs: &[(&str, &str)]) -> Headers {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn text_of(result: &ToolResult) -> &str {
        match result.content.first() {
            Some(Content::Text { text }) => text,
            None => "",
        }
    }

    fn ids(store: &ResultStore) -> Vec<&str> {
        store.entries.iter().map(|e| e.id.as_str()).collect()
    }

    // --- ported from utils.test.ts ---

    #[test]
    fn 대소문자가_달라도_override가_원본_헤더를_실제로_제거한다() {
        let h = merge_replay_headers(
            Some(&headers(&[
                ("authorization", "Bearer real"),
                ("accept", "*/*"),
            ])),
            "",
            Some(&headers(&[("Authorization", "")])),
        );
        assert_eq!(h, headers(&[("accept", "*/*")]));
    }

    #[test]
    fn override가_없으면_페이지_쿠키를_자동_주입한다() {
        let h = merge_replay_headers(Some(&headers(&[("accept", "*/*")])), "sid=1", None);
        assert_eq!(h.get("cookie").map(String::as_str), Some("sid=1"));
    }

    #[test]
    fn 명시적_cookie_override가_자동_주입_쿠키를_이긴다_인증_제거_재생() {
        let h = merge_replay_headers(
            Some(&headers(&[("cookie", "old=1")])),
            "sid=1",
            Some(&headers(&[("cookie", "")])),
        );
        assert!(!h.contains_key("cookie"));
    }

    #[test]
    fn 일반_override는_값을_교체한다() {
        let h = merge_replay_headers(
            Some(&headers(&[("x-token", "a")])),
            "",
            Some(&headers(&[("X-Token", "b")])),
        );
        assert_eq!(h, headers(&[("x-token", "b")]));
    }

    // --- not covered by the TypeScript tests ---

    #[test]
    fn 캡처_헤더가_없어도_쿠키와_override만으로_병합한다() {
        let h = merge_replay_headers(None, "sid=1", Some(&headers(&[("X-A", "1")])));
        assert_eq!(h, headers(&[("cookie", "sid=1"), ("x-a", "1")]));
        assert!(merge_replay_headers(None, "", None).is_empty());
    }

    #[test]
    fn 캡처된_헤더_이름도_소문자로_바꾸고_빈_값은_버린다() {
        let h = merge_replay_headers(
            Some(&headers(&[("Content-Type", "text/plain"), ("X-Empty", "")])),
            "",
            None,
        );
        assert_eq!(h, headers(&[("content-type", "text/plain")]));
    }

    #[test]
    fn 캡처된_쿠키는_페이지_쿠키로_덮어쓴다() {
        let h = merge_replay_headers(Some(&headers(&[("Cookie", "old=1")])), "sid=1", None);
        assert_eq!(h, headers(&[("cookie", "sid=1")]));
    }

    #[test]
    fn 페이지_쿠키가_없으면_캡처된_쿠키를_그대로_둔다() {
        let h = merge_replay_headers(Some(&headers(&[("Cookie", "old=1")])), "", None);
        assert_eq!(h, headers(&[("cookie", "old=1")]));
    }

    #[test]
    fn 헤더_이름은_자바스크립트_to_lower_case와_같게_소문자로_바꾼다() {
        // Values checked against bun: final sigma and dotted capital I.
        let h = merge_replay_headers(Some(&headers(&[("X-ΑΣ", "1"), ("İ", "2")])), "", None);
        assert_eq!(h, headers(&[("x-ας", "1"), ("i\u{307}", "2")]));
    }

    #[test]
    fn ok는_is_error_키_없이_텍스트만_직렬화한다() {
        assert_eq!(
            serde_json::to_value(ok("hi")).ok(),
            Some(json!({ "content": [{ "type": "text", "text": "hi" }] }))
        );
    }

    #[test]
    fn err는_is_error_true와_텍스트를_직렬화한다() {
        assert_eq!(
            serde_json::to_value(err("boom")).ok(),
            Some(json!({ "isError": true, "content": [{ "type": "text", "text": "boom" }] }))
        );
    }

    #[test]
    fn error_message는_에러의_메시지만_돌려준다() {
        let e = std::io::Error::other("disk gone");
        assert_eq!(error_message(&e), "disk gone");
        assert_eq!(error_message("plain"), "plain");
        assert_eq!(error_message(42), "42");
    }

    #[test]
    fn 예산_이하의_텍스트는_자르지_않는다() {
        let text = "a".repeat(DEFAULT_MAX_CHARS);
        let b = budget_text(&text, None);
        assert_eq!(b.head, text);
        assert!(!b.truncated);
        assert_eq!(b.shown_chars, 12_000);
    }

    #[test]
    fn 예산을_넘으면_마지막_줄바꿈에서_자른다() {
        let b = budget_text("aaaaaaa\nbbbbbbbb", Some(10));
        assert_eq!(b.head, "aaaaaaa");
        assert!(b.truncated);
        assert_eq!(b.shown_chars, 7);
    }

    #[test]
    fn 줄바꿈이_예산의_절반_이하에_있으면_그대로_자른다() {
        // lastNl == 5 is not > 10 * 0.5, so the hard slice wins.
        let b = budget_text("aaaaa\nbbbbbbbbbb", Some(10));
        assert_eq!(b.head, "aaaaa\nbbbb");
        assert_eq!(b.shown_chars, 10);

        let b = budget_text("aaaaaa\nbbbbbbbbbb", Some(10));
        assert_eq!(b.head, "aaaaaa");
        assert_eq!(b.shown_chars, 6);
    }

    #[test]
    fn 홀수_예산에서도_절반_기준은_자바스크립트의_실수_비교와_같다() {
        // 6 > 11 * 0.5 snaps to the newline; 5 > 5.5 does not.
        let b = budget_text("aaaaaa\nbbbbbbbbbb", Some(11));
        assert_eq!(b.head, "aaaaaa");
        assert_eq!(b.shown_chars, 6);

        let b = budget_text("aaaaa\nbbbbbbbbbbb", Some(11));
        assert_eq!(b.head, "aaaaa\nbbbbb");
        assert_eq!(b.shown_chars, 11);
    }

    #[test]
    fn 맨_앞의_줄바꿈에서는_자르지_않는다() {
        let b = budget_text("\naaaaaaaaaaaa", Some(1));
        assert_eq!(b.head, "\n");
        assert!(b.truncated);
        assert_eq!(b.shown_chars, 1);
    }

    #[test]
    fn 줄바꿈이_없으면_예산_길이에서_자른다() {
        let b = budget_text("aaaaaaaaaaaaaaaaaaaa", Some(10));
        assert_eq!(b.head, "aaaaaaaaaa");
        assert!(b.truncated);
        assert_eq!(b.shown_chars, 10);
    }

    #[test]
    fn 예산이_0이면_빈_머리를_돌려준다() {
        let b = budget_text("abc", Some(0));
        assert_eq!(b.head, "");
        assert!(b.truncated);
        assert_eq!(b.shown_chars, 0);
        assert!(!budget_text("", Some(0)).truncated);
    }

    #[test]
    fn 길이는_바이트가_아니라_utf16_단위로_센다() {
        // 4 Hangul syllables: 12 bytes, 4 UTF-16 units.
        let b = budget_text("가나다라", Some(4));
        assert!(!b.truncated);
        assert_eq!(b.shown_chars, 4);

        let b = budget_text("가나다라마", Some(4));
        assert_eq!(b.head, "가나다라");
        assert_eq!(b.shown_chars, 4);

        // An emoji is 2 UTF-16 units.
        let b = budget_text("😀😀", Some(4));
        assert!(!b.truncated);
        assert_eq!(b.shown_chars, 4);
    }

    #[test]
    fn 서로게이트_쌍_한가운데서는_자르지_않고_한_단위_앞에서_자른다() {
        let b = budget_text("ab😀cd", Some(3));
        assert_eq!(b.head, "ab");
        assert!(b.truncated);
        assert_eq!(b.shown_chars, 2);
    }

    #[test]
    fn 예산_안의_결과는_저장하지_않고_그대로_돌려준다() {
        let mut store = ResultStore::new();
        let result = store.ok_budgeted("small", None);
        assert_eq!(result, ok("small"));
        assert!(store.entries.is_empty());
        assert_eq!(store.result_seq, 0);
    }

    #[test]
    fn 예산을_넘는_결과는_저장하고_이어_읽을_꼬리말을_붙인다() {
        let mut store = ResultStore::new();
        let text = "a".repeat(20);
        let result = store.ok_budgeted(text.clone(), Some(10));
        assert!(!result.is_error);
        assert_eq!(
            text_of(&result),
            "aaaaaaaaaa\n\n[truncated: showing 10 of 20 chars. \
             Call tool_result_more with resultId=\"res_1\", offset=10 for the rest.]"
        );
        assert_eq!(store.stored_chars, 20);

        let page = store.get_result_slice("res_1", 10, None);
        assert_eq!(
            page,
            Some(ResultSlice {
                slice: "a".repeat(10),
                offset: 10,
                end: 20,
                total: 20,
                more: false,
                next_offset: None,
            })
        );
    }

    #[test]
    fn 기본_예산은_12000자다() {
        let mut store = ResultStore::new();
        let result = store.ok_budgeted("a".repeat(12_001), None);
        assert!(text_of(&result).ends_with(
            "[truncated: showing 12000 of 12001 chars. \
             Call tool_result_more with resultId=\"res_1\", offset=12000 for the rest.]"
        ));
    }

    #[test]
    fn 줄바꿈에서_자른_결과는_꼬리말_offset부터_빠짐없이_이어진다() {
        let mut store = ResultStore::new();
        let text = "가나다라마바사\n😀아자차카타파하";
        let result = store.ok_budgeted(text, Some(10));
        assert!(
            text_of(&result).starts_with("가나다라마바사\n\n[truncated: showing 7 of 17 chars.")
        );

        let page = store.get_result_slice("res_1", 7, None);
        assert_eq!(page.map(|p| p.slice), Some("\n😀아자차카타파하".to_owned()));
    }

    #[test]
    fn 결과_id는_순서대로_증가한다() {
        let mut store = ResultStore::new();
        store.ok_budgeted("a".repeat(5), Some(1));
        let result = store.ok_budgeted("b".repeat(5), Some(1));
        assert!(text_of(&result).contains("resultId=\"res_2\""));
        assert_eq!(ids(&store), ["res_1", "res_2"]);
    }

    #[test]
    fn full이_true면_큰_본문을_예산에_맞춰_돌려준다() {
        let mut store = ResultStore::new();
        let big = "a".repeat(12_001);
        let result = store.ok_full_or_summary(big, || json!({ "k": 1 }), Some(true));
        let result = result.ok();
        assert!(
            result
                .as_ref()
                .is_some_and(|r| text_of(r).contains("resultId=\"res_1\""))
        );

        // A small text with full=true comes back whole, with nothing parked.
        let result = store.ok_full_or_summary("small", || json!({ "k": 1 }), Some(true));
        assert_eq!(result.ok(), Some(ok("small")));
        assert_eq!(store.entries.len(), 1);
    }

    #[test]
    fn full을_지정하지_않으면_예산에_맞는_본문은_그대로_돌려준다() {
        let mut store = ResultStore::new();
        let result = store.ok_full_or_summary("small", || json!({ "k": 1 }), None);
        assert_eq!(result.ok(), Some(ok("small")));
    }

    #[test]
    fn full을_지정하지_않고_본문이_크면_요약을_돌려준다() {
        let mut store = ResultStore::new();
        let big = "a".repeat(12_001);
        let result = store.ok_full_or_summary(big, || json!({ "k": 1, "list": [1, 2] }), None);
        assert_eq!(
            result.ok(),
            Some(ok("{\n  \"k\": 1,\n  \"list\": [\n    1,\n    2\n  ]\n}"))
        );
        assert!(store.entries.is_empty());
    }

    #[test]
    fn full이_false면_작은_본문이어도_요약을_돌려준다() {
        let mut store = ResultStore::new();
        let result = store.ok_full_or_summary("small", || json!({ "k": 1 }), Some(false));
        assert_eq!(result.ok(), Some(ok("{\n  \"k\": 1\n}")));
    }

    #[test]
    fn 요약은_json_stringify의_두_칸_들여쓰기와_같은_모양이다() {
        // Expected strings are what JSON.stringify(v, null, 2) prints in bun.
        let mut store = ResultStore::new();
        let result = store.ok_full_or_summary("small", || json!({}), Some(false));
        assert_eq!(result.ok(), Some(ok("{}")));

        let summary = || json!({ "a": [], "b": {}, "c": "한\"\n" });
        let result = store.ok_full_or_summary("small", summary, Some(false));
        assert_eq!(
            result.ok(),
            Some(ok(
                "{\n  \"a\": [],\n  \"b\": {},\n  \"c\": \"한\\\"\\n\"\n}"
            ))
        );
    }

    #[test]
    fn 조각은_camel_case_키로_직렬화하고_끝에서는_next_offset이_null이다() {
        let mut store = ResultStore::new();
        store.ok_budgeted("0123456789", Some(1));
        let page = store.get_result_slice("res_1", 8, Some(4));
        assert_eq!(
            page.and_then(|p| serde_json::to_value(p).ok()),
            Some(json!({
                "slice": "89", "offset": 8, "end": 10, "total": 10,
                "more": false, "nextOffset": null
            }))
        );
        let page = store.get_result_slice("res_1", 2, Some(0));
        assert_eq!(
            page.and_then(|p| serde_json::to_value(p).ok()),
            Some(json!({
                "slice": "2", "offset": 2, "end": 3, "total": 10,
                "more": true, "nextOffset": 3
            }))
        );
    }

    #[test]
    fn 요약을_직렬화할_수_없으면_에러를_돌려준다() {
        // JSON object keys must be strings; a tuple key cannot be one.
        let mut store = ResultStore::new();
        let result =
            store.ok_full_or_summary("small", || BTreeMap::from([((1, 2), "v")]), Some(false));
        assert!(result.is_err());
    }

    #[test]
    fn 모르는_id는_none을_돌려준다() {
        let mut store = ResultStore::new();
        assert_eq!(store.get_result_slice("res_1", 0, None), None);
        store.ok_budgeted("a".repeat(5), Some(1));
        assert_eq!(store.get_result_slice("res_2", 0, None), None);
        assert_eq!(store.get_result_slice("", 0, None), None);
    }

    #[test]
    fn limit만큼씩_끝까지_이어_읽는다() {
        let mut store = ResultStore::new();
        store.ok_budgeted("0123456789", Some(1));

        let first = store.get_result_slice("res_1", 0, Some(4));
        assert_eq!(
            first,
            Some(ResultSlice {
                slice: "0123".to_owned(),
                offset: 0,
                end: 4,
                total: 10,
                more: true,
                next_offset: Some(4),
            })
        );
        let second = store.get_result_slice("res_1", 4, Some(4));
        assert_eq!(second.as_ref().map(|p| p.slice.as_str()), Some("4567"));
        assert_eq!(second.and_then(|p| p.next_offset), Some(8));
        let last = store.get_result_slice("res_1", 8, Some(4));
        assert_eq!(
            last,
            Some(ResultSlice {
                slice: "89".to_owned(),
                offset: 8,
                end: 10,
                total: 10,
                more: false,
                next_offset: None,
            })
        );
    }

    #[test]
    fn 끝을_넘는_offset은_전체_길이로_맞춘다() {
        let mut store = ResultStore::new();
        store.ok_budgeted("0123456789", Some(1));
        assert_eq!(
            store.get_result_slice("res_1", 999, None),
            Some(ResultSlice {
                slice: String::new(),
                offset: 10,
                end: 10,
                total: 10,
                more: false,
                next_offset: None,
            })
        );
    }

    #[test]
    fn limit이_0이어도_한_글자는_돌려준다() {
        let mut store = ResultStore::new();
        store.ok_budgeted("0123456789", Some(1));
        let page = store.get_result_slice("res_1", 2, Some(0));
        assert_eq!(page.as_ref().map(|p| p.slice.as_str()), Some("2"));
        assert_eq!(page.and_then(|p| p.next_offset), Some(3));
    }

    #[test]
    fn 조각의_offset과_길이도_utf16_단위다() {
        let mut store = ResultStore::new();
        store.ok_budgeted("가나😀다라", Some(1));
        let page = store.get_result_slice("res_1", 2, Some(3));
        assert_eq!(
            page,
            Some(ResultSlice {
                slice: "😀다".to_owned(),
                offset: 2,
                end: 5,
                total: 6,
                more: true,
                next_offset: Some(5),
            })
        );
    }

    #[test]
    fn 서로게이트_쌍_한가운데의_offset과_끝은_글자_경계로_옮긴다() {
        let mut store = ResultStore::new();
        store.ok_budgeted("a😀b😀c", Some(1));

        // Offset 2 is the low half of the first emoji: back to its start.
        // The end (1 + 4 = 5) is the low half of the second: on past it.
        let page = store.get_result_slice("res_1", 2, Some(4));
        assert_eq!(
            page,
            Some(ResultSlice {
                slice: "😀b😀".to_owned(),
                offset: 1,
                end: 6,
                total: 7,
                more: true,
                next_offset: Some(6),
            })
        );

        // limit 1 on an emoji still makes progress.
        let page = store.get_result_slice("res_1", 1, Some(1));
        assert_eq!(page.as_ref().map(|p| p.slice.as_str()), Some("😀"));
        assert_eq!(page.and_then(|p| p.next_offset), Some(3));
    }

    #[test]
    fn 결과가_50개를_넘으면_가장_오래된_것부터_버린다() {
        let mut store = ResultStore::new();
        for _ in 0..MAX_STORED_RESULTS {
            store.ok_budgeted("ab", Some(1));
        }
        assert_eq!(store.entries.len(), 50);
        assert!(store.get_result_slice("res_2", 0, None).is_some());

        store.ok_budgeted("ab", Some(1));
        assert_eq!(store.entries.len(), 50);
        assert_eq!(store.stored_chars, 100);
        assert_eq!(store.get_result_slice("res_1", 0, None), None);
        assert!(store.get_result_slice("res_51", 0, None).is_some());
    }

    #[test]
    fn 읽은_결과는_최근_것으로_갱신되어_늦게_버려진다() {
        let mut store = ResultStore::new();
        for _ in 0..MAX_STORED_RESULTS {
            store.ok_budgeted("ab", Some(1));
        }
        assert!(store.get_result_slice("res_1", 0, None).is_some());
        assert_eq!(store.stored_chars, 100);

        store.ok_budgeted("ab", Some(1));
        assert!(store.entries.iter().any(|e| e.id == "res_1"));
        assert!(!store.entries.iter().any(|e| e.id == "res_2"));
        assert_eq!(ids(&store).last().copied(), Some("res_51"));
    }

    #[test]
    fn 저장된_글자_수가_상한을_넘으면_오래된_것부터_버린다() {
        let mut store = ResultStore::new();
        let chunk = "a".repeat(MAX_STORED_CHARS / 2);
        store.ok_budgeted(chunk.clone(), None);
        store.ok_budgeted(chunk, None);
        // Exactly at the cap: nothing is evicted yet.
        assert_eq!(ids(&store), ["res_1", "res_2"]);
        assert_eq!(store.stored_chars, MAX_STORED_CHARS);

        store.ok_budgeted("a".repeat(12_001), None);
        assert_eq!(ids(&store), ["res_2", "res_3"]);
        assert_eq!(store.stored_chars, MAX_STORED_CHARS / 2 + 12_001);
    }

    #[test]
    fn 상한보다_큰_결과도_방금_저장한_것은_남긴다() {
        let mut store = ResultStore::new();
        store.ok_budgeted("a".repeat(12_001), None);
        store.ok_budgeted("a".repeat(MAX_STORED_CHARS + 1), None);
        assert_eq!(ids(&store), ["res_2"]);
        assert_eq!(store.stored_chars, MAX_STORED_CHARS + 1);
        assert_eq!(
            store
                .get_result_slice("res_2", MAX_STORED_CHARS, None)
                .map(|p| p.slice),
            Some("a".to_owned())
        );
    }

    #[test]
    fn 이미지_데이터_url_하나로만_된_블록을_이미지로_읽는다() {
        let result = ok_image("image/png", "iVBORw0KGgo=");
        assert_eq!(text_of(&result), "data:image/png;base64,iVBORw0KGgo=");
        assert_eq!(
            parse_image_data_url(text_of(&result)),
            Some(("image/png", "iVBORw0KGgo="))
        );
        for text in [
            "see data:image/png;base64,AAAA",
            "data:image/png;base64,AAAA and more",
            "data:image/png;base64,",
            "data:text/html;base64,AAAA",
            "data:image/;base64,AAAA",
            "data:image/png,AAAA",
        ] {
            assert_eq!(parse_image_data_url(text), None, "{text}");
        }
    }
}
