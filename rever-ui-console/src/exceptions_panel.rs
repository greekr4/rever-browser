//! Port of `src/renderer/src/components/panels/ExceptionsPanel.tsx` — the exceptions
//! panel: uncaught page errors, each expandable to its stack trace.
//!
//! The React component polled the main process over IPC; this view reads the
//! [`TrafficStore`] it is given, and the host calls [`ExceptionsPanel::refresh`] when
//! something new arrived.

use std::sync::{Arc, Mutex, PoisonError};

use gpui::{ClickEvent, Context, Window, div, prelude::*, px, rgba};
use rever_core::traffic::{RuntimeException, TrafficStore};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};
use serde_json::Value;

use crate::{MONO, StatusTokens, format_time, panel_empty};

/// `console:exceptions` in `src/main/index.ts` returns at most this many, the newest.
pub const EXCEPTIONS_LIMIT: usize = 100;

#[derive(Clone, Debug, PartialEq)]
pub struct ExItem {
    pub exception: RuntimeException,
    pub expanded: bool,
}

/// `Boolean(e.stackTrace)`: a missing trace and JavaScript's falsy values do not count.
pub fn has_stack(stack_trace: Option<&Value>) -> bool {
    match stack_trace {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        // NaN cannot appear in JSON, so zero is the only falsy number.
        Some(Value::Number(number)) => number.as_f64() != Some(0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

/// `Number::toString`: serde_json prints `1.0` and `1e21` where JavaScript prints `1`
/// and `1e+21`.
fn js_number(value: f64) -> String {
    if !value.is_finite() {
        return "null".into();
    }
    if value == 0.0 {
        return "0".into();
    }
    // `{:e}` gives the shortest digits that read back as the same number, which is what
    // JavaScript prints; only the layout of those digits differs.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let point = exponent.parse::<i32>().unwrap_or(0) + 1;
    let count = digits.len() as i32;
    let zeros = |n: i32| "0".repeat(n.max(0) as usize);
    let body = if count <= point && point <= 21 {
        format!("{digits}{}", zeros(point - count))
    } else if 0 < point && point <= 21 {
        let (whole, fraction) = digits.split_at(point as usize);
        format!("{whole}.{fraction}")
    } else if -6 < point && point <= 0 {
        format!("0.{}{digits}", zeros(-point))
    } else {
        let (first, rest) = digits.split_at(digits.len().min(1));
        let dot = if rest.is_empty() { "" } else { "." };
        let sign = if point > 0 { '+' } else { '-' };
        format!("{first}{dot}{rest}e{sign}{}", (point - 1).abs())
    };
    if value < 0.0 {
        format!("-{body}")
    } else {
        body
    }
}

/// A JavaScript array index (0 to 2^32 - 2, written canonically), or `None`.
fn array_index(key: &str) -> Option<u32> {
    key.parse::<u32>()
        .ok()
        .filter(|index| *index != u32::MAX && index.to_string() == key)
}

fn write_js(value: &Value, depth: usize, out: &mut String) {
    let indent = |depth: usize| "  ".repeat(depth);
    match value {
        // Every number goes through a double, as it did in `JSON.parse`.
        Value::Number(number) => out.push_str(&js_number(number.as_f64().unwrap_or(f64::NAN))),
        Value::Array(items) if !items.is_empty() => {
            out.push('[');
            for (position, item) in items.iter().enumerate() {
                out.push_str(if position == 0 { "\n" } else { ",\n" });
                out.push_str(&indent(depth + 1));
                write_js(item, depth + 1, out);
            }
            out.push_str(&format!("\n{}]", indent(depth)));
        }
        Value::Object(map) if !map.is_empty() => {
            // A JavaScript object lists its array-index keys first, in ascending order,
            // then the others in the order they were added. The sort is stable.
            let mut members: Vec<(&String, &Value)> = map.iter().collect();
            members.sort_by_key(|(key, _)| array_index(key).map_or((1, 0), |index| (0, index)));
            out.push('{');
            for (position, (key, member)) in members.into_iter().enumerate() {
                out.push_str(if position == 0 { "\n" } else { ",\n" });
                out.push_str(&indent(depth + 1));
                out.push_str(&Value::String(key.clone()).to_string());
                out.push_str(": ");
                write_js(member, depth + 1, out);
            }
            out.push_str(&format!("\n{}}}", indent(depth)));
        }
        // null, booleans, strings, `[]` and `{}` print the same in both.
        other => out.push_str(&other.to_string()),
    }
}

/// `JSON.stringify(e.stackTrace, null, 2)`.
pub fn stack_text(stack_trace: &Value) -> String {
    let mut out = String::new();
    write_js(stack_trace, 0, &mut out);
    out
}

/// The list after a poll returned `next`, or `None` when nothing changed (same length
/// and same last timestamp). Expanded rows stay expanded by position, as in the component.
pub fn merge(prev: &[ExItem], next: Vec<RuntimeException>) -> Option<Vec<ExItem>> {
    let last_ts_changed = next.last().map(|e| e.ts) != prev.last().map(|item| item.exception.ts);
    if next.len() == prev.len() && !last_ts_changed {
        return None;
    }
    Some(
        next.into_iter()
            .enumerate()
            .map(|(index, exception)| ExItem {
                exception,
                expanded: prev.get(index).is_some_and(|item| item.expanded),
            })
            .collect(),
    )
}

pub struct ExceptionsPanel {
    store: Arc<Mutex<TrafficStore>>,
    lang: Lang,
    /// `toLocaleTimeString()` follows the system locale, not the language chosen in the app.
    time_locale: Lang,
    theme_mode: ThemeMode,
    items: Vec<ExItem>,
}

impl ExceptionsPanel {
    pub fn new(
        store: Arc<Mutex<TrafficStore>>,
        lang: Lang,
        theme_mode: ThemeMode,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            store,
            lang,
            time_locale: Lang::detect(),
            theme_mode,
            items: Vec::new(),
        };
        this.refresh(cx);
        this
    }

    pub fn items(&self) -> &[ExItem] {
        &self.items
    }

    /// Read the store's exceptions again.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        // A panic elsewhere while the store was locked must not take the panel down too.
        let store = self.store.lock().unwrap_or_else(PoisonError::into_inner);
        let all = store.exceptions();
        let next = all[all.len().saturating_sub(EXCEPTIONS_LIMIT)..]
            .iter()
            .map(|exception| (*exception).clone())
            .collect();
        drop(store);
        if let Some(items) = merge(&self.items, next) {
            self.items = items;
            cx.notify();
        }
    }

    /// Show or hide the stack trace of the row at `index`; a row without one stays as it
    /// is (`e.stackTrace && toggle(i)`), or it would hand "expanded" to the row that takes
    /// its place later.
    pub fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(item) = self.items.get_mut(index)
            && has_stack(item.exception.stack_trace.as_ref())
        {
            item.expanded = !item.expanded;
            cx.notify();
        }
    }
}

impl Render for ExceptionsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::resolve(self.theme_mode, window.appearance());
        let tokens = StatusTokens::resolve(self.theme_mode, window.appearance());
        let (lang, time_locale) = (self.lang, self.time_locale);
        let rows = self.items.iter().enumerate().map(|(index, item)| {
            let stack = item
                .exception
                .stack_trace
                .as_ref()
                .filter(|stack| has_stack(Some(stack)));
            div()
                .id(("exception", index))
                .px(px(10.0))
                .py(px(6.0))
                .border_b_1()
                .border_color(theme.border)
                .when(stack.is_some(), |row| {
                    row.cursor_pointer().on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle(index, cx)),
                    )
                })
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex_none()
                                .opacity(0.4)
                                .text_size(px(10.0))
                                .child(format_time(item.exception.ts, time_locale)),
                        )
                        // min_w_0 lets the text wrap inside the row instead of widening it.
                        .child(
                            div()
                                .min_w_0()
                                .text_color(theme.status_error)
                                .child(item.exception.text.clone()),
                        )
                        .when(stack.is_some(), |head| {
                            head.child(div().flex_none().opacity(0.4).child(if item.expanded {
                                "▲"
                            } else {
                                "▼"
                            }))
                        }),
                )
                .when_some(stack.filter(|_| item.expanded), |row, stack| {
                    row.child(
                        // The `pre`.
                        div()
                            .id(("stack", index))
                            .mt(px(4.0))
                            .px(px(8.0))
                            .py(px(6.0))
                            .max_h(px(160.0))
                            .overflow_y_scroll()
                            .rounded(px(4.0))
                            // rgba(255,107,107,0.06)
                            .bg(rgba(0xff6b6b0f))
                            .text_size(px(10.0))
                            .line_height(px(12.0))
                            .text_color(tokens.error_dim)
                            .child(stack_text(stack)),
                    )
                })
        });

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .child(
                div()
                    .id("exceptions")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .font_family(MONO)
                    .text_size(px(11.0))
                    .line_height(px(13.0))
                    .children(rows)
                    .when(self.items.is_empty(), |body| {
                        body.child(panel_empty(lang.t("panel.noExceptions"), &theme))
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point};
    use serde_json::json;

    use super::*;

    fn exception(ts: u64, text: &str) -> RuntimeException {
        RuntimeException {
            ts,
            text: text.into(),
            ..RuntimeException::default()
        }
    }

    fn item(ts: u64, text: &str, expanded: bool) -> ExItem {
        ExItem {
            exception: exception(ts, text),
            expanded,
        }
    }

    #[test]
    fn 스택이_없거나_거짓_값이면_펼칠_수_없다() {
        assert!(!has_stack(None));
        for falsy in [json!(null), json!(false), json!(0), json!(0.0), json!("")] {
            assert!(!has_stack(Some(&falsy)), "{falsy}");
        }
    }

    #[test]
    fn 객체와_배열은_비어_있어도_스택으로_본다() {
        for truthy in [
            json!({}),
            json!([]),
            json!({ "callFrames": [] }),
            json!("at foo"),
            json!(1),
            json!(true),
        ] {
            assert!(has_stack(Some(&truthy)), "{truthy}");
        }
    }

    #[test]
    fn 스택은_두_칸_들여쓰기와_받은_키_순서로_출력한다() {
        let stack = json!({
            "callFrames": [{ "functionName": "boom", "url": "https://a.test/app.js", "lineNumber": 12 }],
            "async": false
        });
        assert_eq!(
            stack_text(&stack),
            "{\n  \"callFrames\": [\n    {\n      \"functionName\": \"boom\",\n      \"url\": \"https://a.test/app.js\",\n      \"lineNumber\": 12\n    }\n  ],\n  \"async\": false\n}"
        );
        assert_eq!(stack_text(&json!([])), "[]");
        assert_eq!(stack_text(&json!("한글 \"x\"")), "\"한글 \\\"x\\\"\"");
    }

    #[test]
    fn 스택의_숫자는_자바스크립트가_쓰는_모양으로_출력한다() {
        // bun 의 JSON.stringify 결과와 맞춘 값들.
        for (number, expected) in [
            (json!(1.0), "1"),
            (json!(12), "12"),
            (json!(-7), "-7"),
            (json!(1.25), "1.25"),
            (json!(1e21), "1e+21"),
            (json!(1e20), "100000000000000000000"),
            (json!(123456789012345680000.0), "123456789012345680000"),
            (json!(1.5e-7), "1.5e-7"),
            (json!(0.000001), "0.000001"),
            (json!(0.0000015), "0.0000015"),
            (json!(-0.0), "0"),
            (json!(-3.5e30), "-3.5e+30"),
            (json!(5e-324), "5e-324"),
            (json!(1.7976931348623157e308), "1.7976931348623157e+308"),
            // JSON.parse 는 2^53 을 넘는 정수를 가까운 double 로 읽는다.
            (json!(9007199254740993u64), "9007199254740992"),
        ] {
            assert_eq!(stack_text(&number), expected);
        }
        assert_eq!(stack_text(&json!([1.0, 2.5])), "[\n  1,\n  2.5\n]");
    }

    #[test]
    fn 스택의_정수_키는_자바스크립트처럼_앞에_오름차순으로_온다() {
        let stack: Value = serde_json::from_str(
            r#"{"b":1,"2":0,"1":0,"01":3,"4294967295":1,"4294967294":2,"-1":5}"#,
        )
        .unwrap();
        assert_eq!(
            stack_text(&stack),
            "{\n  \"1\": 0,\n  \"2\": 0,\n  \"4294967294\": 2,\n  \"b\": 1,\n  \"01\": 3,\n  \"4294967295\": 1,\n  \"-1\": 5\n}"
        );
    }

    #[test]
    fn 스택의_빈_객체와_중첩과_제어문자를_자바스크립트처럼_출력한다() {
        let stack = json!({ "a": {}, "b": [], "c": [[]], "d": { "e": [1, { "f": null }] } });
        assert_eq!(
            stack_text(&stack),
            "{\n  \"a\": {},\n  \"b\": [],\n  \"c\": [\n    []\n  ],\n  \"d\": {\n    \"e\": [\n      1,\n      {\n        \"f\": null\n      }\n    ]\n  }\n}"
        );
        assert_eq!(
            stack_text(&json!("한글 \"x\"\u{1}\u{7f}\u{2028}")),
            "\"한글 \\\"x\\\"\\u0001\u{7f}\u{2028}\""
        );
        assert_eq!(
            stack_text(&json!({ "k\n": true })),
            "{\n  \"k\\n\": true\n}"
        );
    }

    #[test]
    fn 길이와_마지막_시각이_같으면_바뀌지_않은_것으로_본다() {
        let prev = [item(1, "a", true), item(2, "b", false)];
        assert_eq!(
            merge(&prev, vec![exception(1, "a"), exception(2, "b")]),
            None
        );
        assert_eq!(merge(&[], vec![]), None);
        // 원본도 길이와 마지막 ts 만 본다: 가운데가 달라져도 그대로 둔다.
        assert_eq!(
            merge(&prev, vec![exception(9, "z"), exception(2, "b")]),
            None
        );
    }

    #[test]
    fn 새_예외가_오면_목록을_바꾸고_펼침_상태는_자리로_이어받는다() {
        let prev = [item(1, "a", true), item(2, "b", false)];
        assert_eq!(
            merge(
                &prev,
                vec![exception(1, "a"), exception(2, "b"), exception(3, "c")]
            ),
            Some(vec![
                item(1, "a", true),
                item(2, "b", false),
                item(3, "c", false)
            ])
        );
    }

    #[test]
    fn 길이가_같아도_마지막_시각이_다르면_바꾼다() {
        // 저장소가 가득 차 앞에서 하나 빠지고 뒤에 하나 붙은 경우. 펼침은 자리에 남는다.
        let prev = [item(1, "a", true), item(2, "b", false)];
        assert_eq!(
            merge(&prev, vec![exception(2, "b"), exception(3, "c")]),
            Some(vec![item(2, "b", true), item(3, "c", false)])
        );
    }

    #[test]
    fn 목록이_비워지면_패널도_비운다() {
        let prev = [item(1, "a", true)];
        assert_eq!(merge(&prev, vec![]), Some(vec![]));
    }

    #[test]
    fn 예외는_원본과_같은_필드_이름으로_직렬화된다() {
        let with_stack = RuntimeException {
            stack_trace: Some(json!({ "callFrames": [] })),
            ..exception(5, "boom")
        };
        assert_eq!(
            serde_json::to_value(with_stack).unwrap(),
            json!({ "ts": 5, "text": "boom", "stackTrace": { "callFrames": [] } })
        );
    }

    // ── The view, in a test window ──────────────────────────────────────────

    fn expanded(panel: &Entity<ExceptionsPanel>, cx: &mut VisualTestContext) -> Vec<bool> {
        panel.read_with(cx, |panel, _| {
            panel.items().iter().map(|item| item.expanded).collect()
        })
    }

    #[gpui::test]
    fn 스택이_있는_줄만_눌러서_펼치고_접는다(cx: &mut TestAppContext) {
        let mut store = TrafficStore::default();
        store.append_exception(RuntimeException {
            stack_trace: Some(json!({ "callFrames": [{ "functionName": "boom" }] })),
            ..exception(1, "with stack")
        });
        store.append_exception(exception(2, "without stack"));
        let store = Arc::new(Mutex::new(store));
        let (panel, cx) = cx.add_window_view(|_, cx| {
            ExceptionsPanel::new(store.clone(), Lang::En, ThemeMode::Light, cx)
        });
        assert_eq!(expanded(&panel, cx), [false, false]);

        // 한 줄짜리 행의 높이는 6 + 13 + 6 + 1 = 26.
        let (first, second) = (point(px(100.0), px(13.0)), point(px(100.0), px(39.0)));
        cx.simulate_click(second, Modifiers::none());
        assert_eq!(expanded(&panel, cx), [false, false]);
        cx.simulate_click(first, Modifiers::none());
        assert_eq!(expanded(&panel, cx), [true, false]);
        cx.simulate_click(first, Modifiers::none());
        assert_eq!(expanded(&panel, cx), [false, false]);
    }

    #[gpui::test]
    fn 새_예외는_refresh_때_나타나고_펼친_줄은_그대로다(cx: &mut TestAppContext) {
        let store = Arc::new(Mutex::new(TrafficStore::default()));
        let (panel, cx) = cx.add_window_view(|_, cx| {
            ExceptionsPanel::new(store.clone(), Lang::En, ThemeMode::Light, cx)
        });
        assert!(expanded(&panel, cx).is_empty());
        store.lock().unwrap().append_exception(RuntimeException {
            stack_trace: Some(json!({ "callFrames": [] })),
            ..exception(1, "first")
        });
        panel.update(cx, |panel, cx| {
            panel.refresh(cx);
            panel.toggle(0, cx);
        });
        store
            .lock()
            .unwrap()
            .append_exception(exception(2, "second"));
        panel.update(cx, |panel, cx| panel.refresh(cx));
        cx.run_until_parked();
        assert_eq!(expanded(&panel, cx), [true, false]);
    }

    #[gpui::test]
    fn 스택이_없는_줄과_없는_자리는_펼쳐지지_않는다(cx: &mut TestAppContext) {
        let mut store = TrafficStore::default();
        store.append_exception(exception(1, "no stack"));
        store.append_exception(RuntimeException {
            stack_trace: Some(json!("")),
            ..exception(2, "falsy stack")
        });
        let store = Arc::new(Mutex::new(store));
        let (panel, cx) = cx.add_window_view(|_, cx| {
            ExceptionsPanel::new(store.clone(), Lang::En, ThemeMode::Light, cx)
        });
        panel.update(cx, |panel, cx| {
            panel.toggle(0, cx);
            panel.toggle(1, cx);
            panel.toggle(99, cx);
        });
        assert_eq!(expanded(&panel, cx), [false, false]);
    }

    #[gpui::test]
    fn 최근_100개까지만_보여준다(cx: &mut TestAppContext) {
        let mut store = TrafficStore::default();
        for ts in 0..150 {
            store.append_exception(exception(ts, "boom"));
        }
        let store = Arc::new(Mutex::new(store));
        let (panel, cx) = cx.add_window_view(|_, cx| {
            ExceptionsPanel::new(store.clone(), Lang::En, ThemeMode::Light, cx)
        });
        let shown: Vec<u64> = panel.read_with(cx, |panel, _| {
            panel.items().iter().map(|item| item.exception.ts).collect()
        });
        assert_eq!(shown.len(), EXCEPTIONS_LIMIT);
        assert_eq!((shown.first(), shown.last()), (Some(&50), Some(&149)));
    }
}
