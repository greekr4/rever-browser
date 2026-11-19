//! Port of `src/renderer/src/components/panels/ConsolePanel.tsx` — the console panel:
//! the page's console output, a Clear button and a JavaScript command line.
//!
//! The React component polled the main process over IPC; this view reads the
//! [`TrafficStore`] it is given, and the host calls [`ConsolePanel::refresh`] when
//! something new arrived. Running a command needs the page, which the host owns: the
//! view records the command, emits [`Evaluate`], and the host answers with
//! [`ConsolePanel::finish_eval`].

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    ClickEvent, Context, Div, Entity, EventEmitter, Focusable, KeyDownEvent, ListAlignment,
    ListOffset, ListState, Rgba, Window, div, list, prelude::*, px,
};
use rever_core::traffic::{ConsoleEntry, TrafficStore};
use rever_formats::console_format::{EvalOutput, EvalReply, format_eval_result};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::text_input::Submitted;
use rever_ui_kit::theme::{Theme, ThemeMode};

use crate::command_input::CommandInput;
use crate::{MONO, StatusTokens, format_time, panel_empty};

/// `console:list` in `src/main/index.ts` returns at most this many entries per call.
pub const POLL_LIMIT: usize = 200;

/// `TYPE_COLOR[e.type] ?? 'var(--text)'`.
pub fn type_color(kind: &str, theme: &Theme, tokens: &StatusTokens) -> Rgba {
    match kind {
        "error" => theme.status_error,
        "warning" | "warn" => theme.status_warn,
        "info" => tokens.info,
        "debug" => theme.text_dim,
        "command" => theme.text_2,
        // "log", "result" and every type without an entry.
        _ => theme.text,
    }
}

/// Chrome-style markers for lines typed into the command line and their output.
pub fn type_label(kind: &str) -> &str {
    match kind {
        "command" => "›",
        "result" => "‹",
        other => other,
    }
}

/// JavaScript's `String.prototype.trim`: Rust's `trim` also strips U+0085 and leaves
/// the byte order mark.
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}

/// The expression `run` evaluates for what was typed, or `None` when there is nothing.
pub fn prepare_command(input: &str) -> Option<String> {
    Some(js_trim(input))
        .filter(|expr| !expr.is_empty())
        .map(str::to_owned)
}

/// What `console:evaluate` appends before evaluating.
pub fn command_entry(expression: &str, ts: u64) -> ConsoleEntry {
    ConsoleEntry {
        ts,
        kind: "command".into(),
        text: expression.into(),
        ..ConsoleEntry::default()
    }
}

/// What `console:evaluate` appends once the page answered.
pub fn output_entry(output: &EvalOutput, ts: u64) -> ConsoleEntry {
    ConsoleEntry {
        ts,
        kind: if output.is_error { "error" } else { "result" }.into(),
        text: output.text.clone(),
        ..ConsoleEntry::default()
    }
}

/// The entries the panel shows, and where the next poll starts (`entries` and
/// `lastTsRef` in the component).
///
/// The component kept `lastTs = last.ts + 1`, which drops an entry stamped in the same
/// millisecond as the last one read. Polling once a second, that almost never happened.
/// Here the host refreshes as entries arrive, where it would drop the second of any two
/// lines logged back to back, and the output of a command that ran within a millisecond.
/// So the position is the last time read plus how many entries of that time were read.
///
/// shortcut: if the store evicts already-read entries of that last millisecond (over
/// `MAX_CONSOLE` lines in it), new ones of the same millisecond are skipped as in the
/// component; a sequence number in `TrafficStore` would remove the guess.
#[derive(Default)]
pub struct ConsoleLog {
    entries: Vec<ConsoleEntry>,
    last_ts: u64,
    read_at_last_ts: usize,
}

impl ConsoleLog {
    pub fn entries(&self) -> &[ConsoleEntry] {
        &self.entries
    }

    /// Append what the store logged since the last poll; returns how many entries came.
    pub fn poll(&mut self, store: &TrafficStore) -> usize {
        let mut already_read = self.read_at_last_ts;
        let next: Vec<&ConsoleEntry> = store
            .console_since(Some(self.last_ts))
            .into_iter()
            .filter(|entry| {
                let skip = entry.ts == self.last_ts && already_read > 0;
                already_read -= usize::from(skip);
                !skip
            })
            .collect();
        let Some(last) = next.last() else {
            return 0;
        };
        let at_last_ts = next.iter().filter(|entry| entry.ts == last.ts).count();
        self.read_at_last_ts = if last.ts == self.last_ts {
            self.read_at_last_ts + at_last_ts
        } else {
            at_last_ts
        };
        self.last_ts = last.ts;
        // What the limit cuts off is not read again, as in `console:list`.
        let shown = &next[next.len().saturating_sub(POLL_LIMIT)..];
        self.entries
            .extend(shown.iter().map(|entry| (*entry).clone()));
        shown.len()
    }

    /// Empty the panel and the store's log.
    pub fn clear(&mut self, store: &mut TrafficStore) {
        store.clear_console();
        self.entries.clear();
        self.last_ts = 0;
        self.read_at_last_ts = 0;
    }
}

/// The expressions that were run, and where Up/Down currently points
/// (`historyRef` and `histIdxRef`; `None` is the component's -1).
#[derive(Default)]
pub struct CommandHistory {
    items: Vec<String>,
    index: Option<usize>,
}

impl CommandHistory {
    pub fn push(&mut self, expression: String) {
        self.items.push(expression);
        self.index = None;
    }

    /// Walk one step back (`up`) or forward. Returns the text the input takes, or `None`
    /// when there is no history and the key keeps its usual meaning.
    pub fn step(&mut self, up: bool) -> Option<&str> {
        let last = self.items.len().checked_sub(1)?;
        self.index = match (up, self.index) {
            (true, None) => Some(last),
            (true, Some(index)) => Some(index.saturating_sub(1)),
            (false, None) => None,
            (false, Some(index)) if index >= last => None,
            (false, Some(index)) => Some(index + 1),
        };
        Some(
            self.index
                .and_then(|index| self.items.get(index))
                .map_or("", String::as_str),
        )
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// Emitted with the expression the user ran. The host evaluates it in the active tab and
/// reports back through [`ConsolePanel::finish_eval`].
pub struct Evaluate(pub String);

impl EventEmitter<Evaluate> for ConsolePanel {}

pub struct ConsolePanel {
    store: Arc<Mutex<TrafficStore>>,
    lang: Lang,
    /// `toLocaleTimeString()` follows the system locale, not the language chosen in the app.
    time_locale: Lang,
    theme_mode: ThemeMode,
    log: ConsoleLog,
    history: CommandHistory,
    input: Entity<CommandInput>,
    list: ListState,
}

impl ConsolePanel {
    pub fn new(
        store: Arc<Mutex<TrafficStore>>,
        lang: Lang,
        theme_mode: ThemeMode,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| CommandInput::new(lang.t("panel.consoleInput"), cx));
        cx.subscribe(&input, |this, _, _: &Submitted, cx| this.run(cx))
            .detach();
        let mut this = Self {
            store,
            lang,
            time_locale: Lang::detect(),
            theme_mode,
            log: ConsoleLog::default(),
            history: CommandHistory::default(),
            input,
            // Rows wrap, so their heights differ; only the visible ones are laid out.
            list: ListState::new(0, ListAlignment::Top, px(200.0)),
        };
        this.refresh(cx);
        this
    }

    pub fn entries(&self) -> &[ConsoleEntry] {
        self.log.entries()
    }

    /// Put `text` in the command line, as if it had been typed.
    pub fn set_input(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_text(text, cx));
    }

    // A panic elsewhere while the store was locked must not take the panel down too.
    fn store(&self) -> MutexGuard<'_, TrafficStore> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Show what was logged since the last call, scrolled to the newest line.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let before = self.log.entries.len();
        let store = self.store.lock().unwrap_or_else(PoisonError::into_inner);
        let added = self.log.poll(&store);
        drop(store);
        if added > 0 {
            self.list.splice(before..before, added);
            self.list.scroll_to(ListOffset {
                item_ix: before + added,
                offset_in_item: px(0.0),
            });
            cx.notify();
        }
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        let mut store = self.store.lock().unwrap_or_else(PoisonError::into_inner);
        self.log.clear(&mut store);
        drop(store);
        self.list.reset(0);
        cx.notify();
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        let Some(expression) = prepare_command(self.input.read(cx).text()) else {
            return;
        };
        self.history.push(expression.clone());
        self.input.update(cx, |input, cx| input.set_text("", cx));
        self.store()
            .append_console(command_entry(&expression, now_ms()));
        cx.emit(Evaluate(expression));
        self.refresh(cx);
    }

    /// Log the outcome of an [`Evaluate`]: the page's `Runtime.evaluate` reply, or the
    /// reason there is none ("No active tab", a protocol error).
    pub fn finish_eval(&mut self, reply: Result<EvalReply, String>, cx: &mut Context<Self>) {
        let output = match reply {
            Ok(reply) => format_eval_result(&reply),
            Err(text) => EvalOutput {
                text,
                is_error: true,
            },
        };
        self.store().append_console(output_entry(&output, now_ms()));
        self.refresh(cx);
    }

    /// Up/Down walk the command history, but only from a single-line input.
    fn on_input_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let up = match keystroke.key.as_str() {
            "up" => true,
            "down" => false,
            _ => return,
        };
        // No look at the modifiers: the component's onKeyDown has none either.
        if !self.input.focus_handle(cx).is_focused(window)
            || self.input.read(cx).text().contains('\n')
        {
            return;
        }
        let Some(text) = self.history.step(up).map(str::to_owned) else {
            return;
        };
        cx.stop_propagation();
        self.input.update(cx, |input, cx| input.set_text(text, cx));
    }
}

/// One console line: time, type, text.
fn console_row(
    entry: &ConsoleEntry,
    time_locale: Lang,
    theme: &Theme,
    tokens: &StatusTokens,
) -> Div {
    div()
        .w_full()
        .px(px(10.0))
        .py(px(2.0))
        .flex()
        .items_start()
        .gap(px(8.0))
        .border_b_1()
        .border_color(theme.border)
        .font_family(MONO)
        .text_size(px(11.0))
        .line_height(px(13.0))
        .text_color(type_color(&entry.kind, theme, tokens))
        .child(
            div()
                .flex_none()
                .opacity(0.4)
                .text_size(px(10.0))
                .child(format_time(entry.ts, time_locale)),
        )
        .child(
            div()
                .flex_none()
                .min_w(px(36.0))
                .opacity(0.6)
                .text_size(px(10.0))
                .child(type_label(&entry.kind).to_owned()),
        )
        // min_w_0 lets the text wrap inside the row instead of widening it.
        .child(div().min_w_0().child(entry.text.clone()))
}

impl Render for ConsolePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::resolve(self.theme_mode, window.appearance());
        let lang = self.lang;
        let hover = theme.border_2;
        let rows = list(
            self.list.clone(),
            cx.processor(|this, index: usize, window, _| {
                let theme = Theme::resolve(this.theme_mode, window.appearance());
                let tokens = StatusTokens::resolve(this.theme_mode, window.appearance());
                match this.log.entries.get(index) {
                    Some(entry) => console_row(entry, this.time_locale, &theme, &tokens),
                    None => div(),
                }
                .into_any_element()
            }),
        )
        .size_full();

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .child(
                // `.panel-toolbar`
                div()
                    .px(px(10.0))
                    .py(px(6.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_end()
                    .gap(px(8.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        // A plain `button` of styles.css.
                        div()
                            .id("console-clear")
                            .px(px(8.0))
                            .py(px(3.0))
                            .rounded(px(4.0))
                            .bg(theme.surface_3)
                            .text_size(px(12.0))
                            .line_height(px(16.0))
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.clear(cx)))
                            .child(lang.t("common.clear")),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .font_family(MONO)
                    .when(self.log.entries.is_empty(), |body| {
                        body.child(panel_empty(lang.t("panel.noConsole"), &theme))
                    })
                    .when(!self.log.entries.is_empty(), |body| body.child(rows)),
            )
            .child(
                // The `textarea`.
                div()
                    .px(px(10.0))
                    .py(px(4.0))
                    .flex_none()
                    .border_t_1()
                    .border_color(theme.border)
                    .bg(theme.bg)
                    .font_family(MONO)
                    .text_size(px(11.0))
                    .capture_key_down(cx.listener(Self::on_input_key))
                    .child(self.input.clone()),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{TestAppContext, VisualTestContext};
    use rever_formats::console_format::EvalReply;
    use serde_json::json;

    use super::*;

    fn entry(ts: u64, kind: &str, text: &str) -> ConsoleEntry {
        ConsoleEntry {
            ts,
            kind: kind.into(),
            text: text.into(),
            ..ConsoleEntry::default()
        }
    }

    fn texts(log: &ConsoleLog) -> Vec<&str> {
        log.entries().iter().map(|e| e.text.as_str()).collect()
    }

    #[test]
    fn 종류마다_정해진_색을_쓰고_모르는_종류는_본문색이다() {
        let (theme, tokens) = (Theme::dark(), StatusTokens::dark());
        let color = |kind| type_color(kind, &theme, &tokens);
        assert!(color("error") == theme.status_error);
        assert!(color("warning") == theme.status_warn);
        assert!(color("warn") == theme.status_warn);
        assert!(color("info") == tokens.info);
        assert!(color("log") == theme.text);
        assert!(color("debug") == theme.text_dim);
        assert!(color("command") == theme.text_2);
        assert!(color("result") == theme.text);
        assert!(color("table") == theme.text);
        assert!(color("") == theme.text);
    }

    #[test]
    fn 명령과_결과는_화살표로_나머지는_종류_이름으로_표시한다() {
        assert_eq!(type_label("command"), "›");
        assert_eq!(type_label("result"), "‹");
        assert_eq!(type_label("error"), "error");
        assert_eq!(type_label("warning"), "warning");
        assert_eq!(type_label(""), "");
    }

    #[test]
    fn 새로_온_항목만_뒤에_붙인다() {
        let mut store = TrafficStore::default();
        let mut log = ConsoleLog::default();
        assert_eq!(log.poll(&store), 0);
        store.append_console(entry(10, "log", "one"));
        store.append_console(entry(20, "warn", "two"));
        assert_eq!(log.poll(&store), 2);
        assert_eq!(log.poll(&store), 0);
        store.append_console(entry(30, "log", "three"));
        assert_eq!(log.poll(&store), 1);
        assert_eq!(texts(&log), ["one", "two", "three"]);
    }

    #[test]
    fn 이미_읽은_밀리초에_뒤늦게_온_항목도_한_번씩_보인다() {
        // 원본의 lastTs = 마지막 ts + 1 은 이런 항목을 버린다. 원본은 1초마다 읽어서 드물지만,
        // 항목이 올 때마다 읽으면 연달아 찍힌 두 줄 중 뒤의 것이 늘 빠진다.
        let mut store = TrafficStore::default();
        let mut log = ConsoleLog::default();
        store.append_console(entry(10, "log", "a"));
        assert_eq!(log.poll(&store), 1);
        store.append_console(entry(10, "log", "b"));
        assert_eq!(log.poll(&store), 1);
        store.append_console(entry(10, "log", "c"));
        store.append_console(entry(11, "log", "d"));
        assert_eq!(log.poll(&store), 2);
        store.append_console(entry(11, "log", "e"));
        assert_eq!(log.poll(&store), 1);
        assert_eq!(log.poll(&store), 0);
        assert_eq!(texts(&log), ["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn 같은_밀리초의_똑같은_줄도_개수만큼_보인다() {
        let mut store = TrafficStore::default();
        let mut log = ConsoleLog::default();
        for _ in 0..3 {
            store.append_console(entry(0, "log", "tick"));
            assert_eq!(log.poll(&store), 1);
        }
        assert_eq!(log.poll(&store), 0);
        assert_eq!(texts(&log), ["tick", "tick", "tick"]);
    }

    #[test]
    fn 명령과_같은_밀리초에_나온_결과도_보인다() {
        let mut store = TrafficStore::default();
        let mut log = ConsoleLog::default();
        store.append_console(entry(20, "command", "run()"));
        log.poll(&store);
        store.append_console(entry(20, "log", "during"));
        log.poll(&store);
        store.append_console(entry(20, "result", "done"));
        log.poll(&store);
        assert_eq!(texts(&log), ["run()", "during", "done"]);
    }

    #[test]
    fn 한_번에_최근_200개까지만_가져온다() {
        let mut store = TrafficStore::default();
        let mut log = ConsoleLog::default();
        for i in 0..250 {
            store.append_console(entry(i, "log", &format!("m{i}")));
        }
        assert_eq!(log.poll(&store), POLL_LIMIT);
        assert_eq!(log.entries().first().map(|e| e.text.as_str()), Some("m50"));
        assert_eq!(log.entries().last().map(|e| e.text.as_str()), Some("m249"));
        // 잘려 나간 앞쪽 50개는 다음 폴링에서도 돌아오지 않는다.
        assert_eq!(log.poll(&store), 0);
        assert_eq!(log.entries().len(), POLL_LIMIT);
    }

    #[test]
    fn 꼭_200개면_하나도_잘리지_않는다() {
        let mut store = TrafficStore::default();
        let mut log = ConsoleLog::default();
        for i in 0..POLL_LIMIT as u64 {
            store.append_console(entry(i / 10, "log", &format!("m{i}")));
        }
        assert_eq!(log.poll(&store), POLL_LIMIT);
        assert_eq!(log.entries().first().map(|e| e.text.as_str()), Some("m0"));
        store.append_console(entry(19, "log", "one more"));
        assert_eq!(log.poll(&store), 1);
        assert_eq!(log.entries().len(), POLL_LIMIT + 1);
    }

    #[test]
    fn 저장소가_오래된_항목을_버려도_패널은_본_것을_유지한다() {
        let mut store = TrafficStore::default();
        let mut log = ConsoleLog::default();
        store.append_console(entry(1, "log", "first"));
        log.poll(&store);
        for i in 0..rever_core::traffic::MAX_CONSOLE as u64 {
            store.append_console(entry(100 + i, "log", "later"));
            log.poll(&store);
        }
        assert_eq!(log.entries().len(), rever_core::traffic::MAX_CONSOLE + 1);
        assert_eq!(
            log.entries().first().map(|e| e.text.as_str()),
            Some("first")
        );
    }

    #[test]
    fn 지우면_패널과_저장소가_비고_처음부터_다시_읽는다() {
        let mut store = TrafficStore::default();
        let mut log = ConsoleLog::default();
        store.append_console(entry(50, "log", "old"));
        log.poll(&store);
        log.clear(&mut store);
        assert!(log.entries().is_empty());
        assert_eq!(store.console_count(), 0);
        // 지우기 전 마지막 시각보다 이르거나 같은 항목도 다시 보인다.
        store.append_console(entry(5, "log", "early"));
        store.append_console(entry(50, "log", "same as old"));
        assert_eq!(log.poll(&store), 2);
        assert_eq!(texts(&log), ["early", "same as old"]);
    }

    #[test]
    fn 입력은_앞뒤_공백을_떼고_비어_있으면_실행하지_않는다() {
        assert_eq!(prepare_command("  1 + 1 \n"), Some("1 + 1".into()));
        assert_eq!(prepare_command("a\n  b"), Some("a\n  b".into()));
        assert_eq!(prepare_command(""), None);
        assert_eq!(prepare_command(" \t\n\r "), None);
    }

    #[test]
    fn 공백_판정은_자바스크립트의_trim을_따른다() {
        // U+FEFF 는 JS 에서 공백이고, U+0085 는 아니다. Rust 의 trim 은 반대다.
        assert_eq!(prepare_command("\u{feff}x\u{feff}"), Some("x".into()));
        assert_eq!(prepare_command("\u{85}"), Some("\u{85}".into()));
        assert_eq!(prepare_command("\u{a0}\u{3000}\u{2028}"), None);
    }

    #[test]
    fn 기록이_없으면_위아래_키를_가로채지_않는다() {
        let mut history = CommandHistory::default();
        assert_eq!(history.step(true), None);
        assert_eq!(history.step(false), None);
    }

    #[test]
    fn 위_키는_최근_명령부터_거슬러_올라가_첫_명령에서_멈춘다() {
        let mut history = CommandHistory::default();
        for expression in ["a", "b", "c"] {
            history.push(expression.into());
        }
        assert_eq!(history.step(true), Some("c"));
        assert_eq!(history.step(true), Some("b"));
        assert_eq!(history.step(true), Some("a"));
        assert_eq!(history.step(true), Some("a"));
    }

    #[test]
    fn 아래_키는_다음_명령으로_가고_끝을_지나면_입력을_비운다() {
        let mut history = CommandHistory::default();
        for expression in ["a", "b", "c"] {
            history.push(expression.into());
        }
        // 기록을 걷는 중이 아니면 아래 키는 입력을 비운다.
        assert_eq!(history.step(false), Some(""));
        history.step(true);
        history.step(true);
        history.step(true);
        assert_eq!(history.step(false), Some("b"));
        assert_eq!(history.step(false), Some("c"));
        assert_eq!(history.step(false), Some(""));
        assert_eq!(history.step(true), Some("c"));
    }

    #[test]
    fn 명령을_실행하면_기록_위치가_처음으로_돌아간다() {
        let mut history = CommandHistory::default();
        history.push("a".into());
        history.push("b".into());
        history.step(true);
        history.step(true);
        history.push("c".into());
        assert_eq!(history.step(true), Some("c"));
    }

    #[test]
    fn 명령은_command_결과는_result_실패는_error_로_남긴다() {
        assert_eq!(command_entry("1+1", 7), entry(7, "command", "1+1"));
        let reply: EvalReply = serde_json::from_value(
            json!({ "result": { "type": "number", "value": 2, "description": "2" } }),
        )
        .unwrap();
        assert_eq!(
            output_entry(&format_eval_result(&reply), 8),
            entry(8, "result", "2")
        );
        let failed = EvalOutput {
            text: "No active tab".into(),
            is_error: true,
        };
        assert_eq!(output_entry(&failed, 9), entry(9, "error", "No active tab"));
    }

    #[test]
    fn 항목은_원본과_같은_필드_이름으로_직렬화된다() {
        assert_eq!(
            serde_json::to_value(command_entry("x", 1)).unwrap(),
            json!({ "ts": 1, "type": "command", "text": "x" })
        );
    }

    // ── The view, in a test window ──────────────────────────────────────────

    struct Opened<'a> {
        panel: Entity<ConsolePanel>,
        store: Arc<Mutex<TrafficStore>>,
        /// The expressions the panel asked the host to evaluate.
        evaluated: Rc<RefCell<Vec<String>>>,
        cx: &'a mut VisualTestContext,
    }

    /// A panel alone in a window, with the command line focused.
    fn open(cx: &mut TestAppContext) -> Opened<'_> {
        cx.update(rever_ui_kit::text_input::bind_keys);
        let store = Arc::new(Mutex::new(TrafficStore::default()));
        let (panel, cx) = cx.add_window_view({
            let store = store.clone();
            move |window, cx| {
                let panel = ConsolePanel::new(store, Lang::En, ThemeMode::Light, cx);
                panel.input.focus_handle(cx).focus(window);
                panel
            }
        });
        let evaluated = Rc::new(RefCell::new(Vec::new()));
        cx.update(|_, cx| {
            let evaluated = evaluated.clone();
            cx.subscribe(&panel, move |_, event: &Evaluate, _| {
                evaluated.borrow_mut().push(event.0.clone())
            })
            .detach();
        });
        Opened {
            panel,
            store,
            evaluated,
            cx,
        }
    }

    impl Opened<'_> {
        fn input(&mut self) -> String {
            self.panel
                .read_with(self.cx, |panel, cx| panel.input.read(cx).text().to_owned())
        }

        fn shown(&mut self) -> Vec<(String, String)> {
            self.panel.read_with(self.cx, |panel, _| {
                let entries = panel.entries().iter();
                entries.map(|e| (e.kind.clone(), e.text.clone())).collect()
            })
        }

        fn list_rows(&mut self) -> usize {
            self.panel
                .read_with(self.cx, |panel, _| panel.list.item_count())
        }
    }

    fn pair(kind: &str, text: &str) -> (String, String) {
        (kind.into(), text.into())
    }

    #[gpui::test]
    fn 엔터는_명령을_기록하고_평가를_요청한_뒤_입력을_비운다(
        cx: &mut TestAppContext,
    ) {
        let mut t = open(cx);
        t.cx.simulate_input("ab");
        assert_eq!(t.input(), "ab");
        t.cx.simulate_keystrokes("enter");
        assert_eq!(t.input(), "");
        assert_eq!(*t.evaluated.borrow(), ["ab"]);
        assert_eq!(t.shown(), [pair("command", "ab")]);
        assert_eq!(t.store.lock().unwrap().console_count(), 1);
        assert_eq!(t.list_rows(), 1);
    }

    #[gpui::test]
    fn 평가_결과와_실패가_명령_뒤에_붙는다(cx: &mut TestAppContext) {
        let mut t = open(cx);
        t.cx.simulate_input("a");
        t.cx.simulate_keystrokes("enter");
        let reply: EvalReply =
            serde_json::from_value(json!({ "result": { "type": "string", "value": "zz" } }))
                .unwrap();
        t.panel.update(t.cx, |panel, cx| {
            panel.finish_eval(Ok(reply), cx);
            panel.finish_eval(Err("No active tab".into()), cx);
        });
        let expected = [
            pair("command", "a"),
            pair("result", "\"zz\""),
            pair("error", "No active tab"),
        ];
        let logged: Vec<_> = {
            let store = t.store.lock().unwrap();
            let all = store.console_since(None).into_iter();
            all.map(|e| (e.kind.clone(), e.text.clone())).collect()
        };
        assert_eq!(logged, expected);
        // 원본은 평가가 끝난 뒤 한 번 읽어 명령과 결과를 함께 받는다. 같은 밀리초에
        // 끝난 평가의 결과도 패널에 보여야 한다.
        assert_eq!(t.shown(), expected);
        assert_eq!(t.list_rows(), 3);
    }

    #[gpui::test]
    fn 같은_밀리초에_잇달아_실행한_명령도_모두_보인다(cx: &mut TestAppContext) {
        let mut t = open(cx);
        for text in ["a", "b", "c", "d", "e"] {
            t.cx.simulate_input(text);
            t.cx.simulate_keystrokes("enter");
        }
        let commands = ["a", "b", "c", "d", "e"].map(|text| pair("command", text));
        assert_eq!(t.shown(), commands);
        assert_eq!(t.store.lock().unwrap().console_count(), 5);
    }

    #[gpui::test]
    fn 조합키와_함께_누른_위아래_키도_기록을_걷는다(cx: &mut TestAppContext) {
        // 원본의 onKeyDown 은 화살표 키에서 조합키를 보지 않는다.
        let mut t = open(cx);
        t.cx.simulate_input("a");
        t.cx.simulate_keystrokes("enter");
        t.cx.simulate_input("b");
        t.cx.simulate_keystrokes("enter");
        t.cx.simulate_keystrokes("shift-up");
        assert_eq!(t.input(), "b");
        t.cx.simulate_keystrokes("alt-up");
        assert_eq!(t.input(), "a");
        t.cx.simulate_keystrokes("cmd-down");
        assert_eq!(t.input(), "b");
        t.cx.simulate_keystrokes("ctrl-down");
        assert_eq!(t.input(), "");
    }

    #[gpui::test]
    fn 공백뿐인_입력에서_엔터는_아무것도_하지_않는다(cx: &mut TestAppContext) {
        let mut t = open(cx);
        t.cx.simulate_keystrokes("space enter");
        assert_eq!(t.input(), " ");
        assert!(t.evaluated.borrow().is_empty());
        assert!(t.shown().is_empty());
    }

    // The test platform types "\n" itself when nothing handles Shift+Enter, so this shows
    // that the line break is not doubled and nothing runs, not which side inserted it.
    #[gpui::test]
    fn 시프트_엔터는_실행하지_않고_줄을_한_번_바꾼다(cx: &mut TestAppContext) {
        let mut t = open(cx);
        t.cx.simulate_input("a");
        t.cx.simulate_keystrokes("shift-enter");
        t.cx.simulate_input("b");
        assert_eq!(t.input(), "a\nb");
        assert!(t.evaluated.borrow().is_empty());
        t.cx.simulate_keystrokes("enter");
        assert_eq!(*t.evaluated.borrow(), ["a\nb"]);
    }

    #[gpui::test]
    fn 시프트만_아니면_다른_조합키와_함께_누른_엔터도_실행한다(
        cx: &mut TestAppContext,
    ) {
        let mut t = open(cx);
        t.cx.simulate_input("a");
        t.cx.simulate_keystrokes("cmd-enter");
        t.cx.simulate_input("b");
        t.cx.simulate_keystrokes("ctrl-enter");
        t.cx.simulate_input("c");
        t.cx.simulate_keystrokes("alt-enter");
        assert_eq!(*t.evaluated.borrow(), ["a", "b", "c"]);
        assert_eq!(t.input(), "");
    }

    #[gpui::test]
    fn 위아래_키로_실행한_명령을_다시_불러온다(cx: &mut TestAppContext) {
        let mut t = open(cx);
        t.cx.simulate_input("a");
        t.cx.simulate_keystrokes("enter");
        t.cx.simulate_input("b");
        t.cx.simulate_keystrokes("enter up");
        assert_eq!(t.input(), "b");
        t.cx.simulate_keystrokes("up");
        assert_eq!(t.input(), "a");
        t.cx.simulate_keystrokes("up");
        assert_eq!(t.input(), "a");
        t.cx.simulate_keystrokes("down");
        assert_eq!(t.input(), "b");
        t.cx.simulate_keystrokes("down");
        assert_eq!(t.input(), "");
    }

    #[gpui::test]
    fn 여러_줄_입력에서는_위_키가_기록_대신_커서를_옮긴다(
        cx: &mut TestAppContext,
    ) {
        let mut t = open(cx);
        t.cx.simulate_input("x");
        t.cx.simulate_keystrokes("enter");
        t.cx.simulate_input("a");
        t.cx.simulate_keystrokes("shift-enter");
        t.cx.simulate_input("b");
        t.cx.simulate_keystrokes("up");
        assert_eq!(t.input(), "a\nb");
        // 커서는 윗줄의 같은 열에 있다.
        t.cx.simulate_input("c");
        assert_eq!(t.input(), "ac\nb");
    }

    #[gpui::test]
    fn 기록이_없으면_위아래_키는_커서를_처음과_끝으로_보낸다(
        cx: &mut TestAppContext,
    ) {
        let mut t = open(cx);
        t.cx.simulate_input("ab");
        t.cx.simulate_keystrokes("up");
        t.cx.simulate_input("c");
        assert_eq!(t.input(), "cab");
        t.cx.simulate_keystrokes("down");
        t.cx.simulate_input("d");
        assert_eq!(t.input(), "cabd");
    }

    #[gpui::test]
    fn 새_항목은_refresh_때_목록에_붙고_지우면_모두_사라진다(
        cx: &mut TestAppContext,
    ) {
        let mut t = open(cx);
        for (ts, text) in [(1, "one"), (2, "two\nlines"), (3, "three")] {
            t.store
                .lock()
                .unwrap()
                .append_console(entry(ts, "log", text));
        }
        // 호스트가 알리기 전에는 보이지 않는다.
        assert_eq!(t.list_rows(), 0);
        t.panel.update(t.cx, |panel, cx| panel.refresh(cx));
        t.cx.run_until_parked();
        assert_eq!(t.list_rows(), 3);
        assert_eq!(t.shown().len(), 3);
        t.panel.update(t.cx, |panel, cx| panel.clear(cx));
        t.cx.run_until_parked();
        assert_eq!(t.list_rows(), 0);
        assert!(t.shown().is_empty());
        assert_eq!(t.store.lock().unwrap().console_count(), 0);
    }

    #[gpui::test]
    fn 입력을_채우면_여섯_줄까지만_보여도_내용은_그대로다(
        cx: &mut TestAppContext,
    ) {
        let mut t = open(cx);
        let text = "1\n2\n3\n4\n5\n6\n7\n8";
        t.panel.update(t.cx, |panel, cx| panel.set_input(text, cx));
        t.cx.run_until_parked();
        assert_eq!(t.input(), text);
        assert_eq!(crate::command_input::rows(&t.input()), 6);
        // 커서는 끝에 있다: 지우면 마지막 글자가 사라진다.
        t.cx.simulate_keystrokes("backspace backspace");
        assert_eq!(t.input(), "1\n2\n3\n4\n5\n6\n7");
    }
}
