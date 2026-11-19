//! Port of `src/renderer/src/components/network/TrafficList.tsx` — the request
//! table with its filters, row selection and the "Ask about" bar.
//!
//! The React component hears about traffic over IPC; this view reads the capture
//! store when the host calls [`TrafficList::refresh`] and replays what changed as
//! the same request/response/finished events. What the component sent to other
//! stores (chat draft, repeater) leaves here as a [`TrafficListEvent`].

use std::ops::Range;
use std::sync::{Arc, Mutex};

use gpui::{
    AnyElement, BoxShadow, ClickEvent, Context, Div, Entity, EventEmitter, FontWeight, Rgba,
    Window, div, point, prelude::*, px, rgba, uniform_list,
};
use rever_core::traffic::{ListFilter, StoredRequest, TrafficStore};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::text_input::TextInput;
use rever_ui_kit::theme::{Theme, ThemeMode};

use crate::json_view::js_trim;
use crate::store::{MAX_ENTRIES, TrafficState};
use crate::types::{NetworkEvent, TrafficEntry};
use crate::ui::{
    MONO, Tokens, button, checkbox, clickable, ghost_button, primary_button, seg_button, tooltip,
};

const STATIC_TYPES: [&str; 4] = ["Image", "Stylesheet", "Font", "Media"];
pub const QUICK_TYPES: [&str; 4] = ["XHR", "Fetch", "Document", "Script"];
/// The row height the React list virtualizes with.
const ROW_HEIGHT: f32 = 27.0;

/// `, status: 200` for the prompts; nothing while the status is missing or zero.
pub(crate) fn status_suffix(status: Option<u16>) -> String {
    match status {
        Some(status) if status != 0 => format!(", status: {status}"),
        _ => String::new(),
    }
}

/// The chat draft "Ask about" starts from.
pub fn build_prefill(rows: &[&TrafficEntry]) -> String {
    let lines: Vec<String> = rows
        .iter()
        .map(|row| {
            let status = status_suffix(row.status);
            format!(
                "- {} {} (id: {}{status})",
                row.method, row.url, row.request_id
            )
        })
        .collect();
    format!(
        "Analyze these requests. Use get_request for details if needed:\n{}\n\nQuestion: ",
        lines.join("\n")
    )
}

/// The rows left after the search box, the type buttons and "hide static".
pub fn filter_entries<'a>(
    full: &[&'a TrafficEntry],
    search: &str,
    type_filter: &[&str],
    hide_static: bool,
) -> Vec<&'a TrafficEntry> {
    let query = js_trim(search).to_lowercase();
    full.iter()
        .copied()
        .filter(|entry| {
            let kind = entry.resource_type.as_str();
            if hide_static && STATIC_TYPES.contains(&kind) {
                return false;
            }
            if !type_filter.is_empty() && !type_filter.contains(&kind) {
                return false;
            }
            query.is_empty()
                || entry.url.to_lowercase().contains(&query)
                || entry.method.to_lowercase().contains(&query)
        })
        .collect()
}

/// `Traffic (12)`, or `Traffic (12 / 40)` while a filter hides rows.
pub fn count_label(shown: usize, total: usize) -> String {
    if shown == total {
        format!("Traffic ({shown})")
    } else {
        format!("Traffic ({shown} / {total})")
    }
}

/// Unlike the kit's `Theme::http_status`, anything below 300 — 1xx included — is
/// drawn as a success, and only a missing or zero status is neutral.
pub fn status_color(status: Option<u16>, theme: &Theme) -> Rgba {
    match status {
        None | Some(0) => theme.http_none,
        Some(500..) => theme.http_5xx,
        Some(400..) => theme.http_4xx,
        Some(300..) => theme.http_3xx,
        Some(_) => theme.http_2xx,
    }
}

pub fn format_bytes(n: u64) -> String {
    // JavaScript's toFixed(1) rounds a tie away from zero, Rust's `{:.1}` to even;
    // integer tenths keep 1280 bytes at "1.3KB".
    let tenths = |unit: u128| {
        let tenths = (u128::from(n) * 10 + unit / 2) / unit;
        format!("{}.{}", tenths / 10, tenths % 10)
    };
    if n < 1024 {
        format!("{n}B")
    } else if n < 1024 * 1024 {
        format!("{}KB", tenths(1024))
    } else {
        format!("{}MB", tenths(1024 * 1024))
    }
}

/// The capture events the list has not seen yet for `stored`: what the Electron
/// main process sends as a request is first seen, answered and finished.
pub fn pending_events(stored: &StoredRequest, known: Option<&TrafficEntry>) -> Vec<NetworkEvent> {
    let mut events = Vec::new();
    let request_id = || stored.request_id.clone();
    if known.is_none() {
        events.push(NetworkEvent::Request {
            request_id: request_id(),
            url: stored.url.clone(),
            method: stored.method.clone(),
            resource_type: stored.resource_type.clone(),
            timestamp: stored.started_at as f64,
        });
    }
    let mime = stored.mime_type.as_deref().unwrap_or_default();
    if let Some(status) = stored.status
        && known.is_none_or(|known| {
            known.status != Some(status) || known.mime_type.as_deref().unwrap_or_default() != mime
        })
    {
        events.push(NetworkEvent::Response {
            request_id: request_id(),
            status,
            mime_type: mime.to_owned(),
            timestamp: stored.completed_at.unwrap_or(stored.started_at) as f64,
        });
    }
    let size = stored.encoded_data_length.unwrap_or(0);
    if let Some(completed) = stored.completed_at
        && known.is_none_or(|known| {
            known.completed_at != Some(completed as f64) || known.encoded_data_length != Some(size)
        })
    {
        events.push(NetworkEvent::Finished {
            request_id: request_id(),
            encoded_data_length: size,
            timestamp: completed as f64,
        });
    }
    events
}

/// Bring `state` up to date with the live capture, oldest request first — the
/// order the events would have arrived in.
pub fn sync(state: &mut TrafficState, store: &TrafficStore) {
    let live = store.list(&ListFilter {
        limit: Some(MAX_ENTRIES),
        ..ListFilter::default()
    });
    for stored in live.into_iter().rev() {
        for event in pending_events(stored, state.entries.get(&stored.request_id)) {
            state.apply_event(event);
        }
    }
}

/// What the list asks of the rest of the app.
#[derive(Clone, Debug, PartialEq)]
pub enum TrafficListEvent {
    /// Put this text in the chat composer ("Ask about").
    PushDraft(String),
    /// Load this request into the repeater ("↻R").
    SendToRepeater(String),
}

pub struct TrafficList {
    store: Arc<Mutex<TrafficStore>>,
    state: Entity<TrafficState>,
    search: Entity<TextInput>,
    type_filter: Vec<&'static str>,
    hide_static: bool,
    /// The filtered rows of the last render; the list builds its visible window from it.
    visible: Vec<TrafficEntry>,
    lang: Lang,
    theme_mode: ThemeMode,
}

impl EventEmitter<TrafficListEvent> for TrafficList {}

impl TrafficList {
    pub fn new(
        store: Arc<Mutex<TrafficStore>>,
        state: Entity<TrafficState>,
        lang: Lang,
        theme_mode: ThemeMode,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| TextInput::new(lang.t("traffic.filter"), cx));
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let mut list = Self {
            store,
            state,
            search,
            type_filter: Vec::new(),
            hide_static: true,
            visible: Vec::new(),
            lang,
            theme_mode,
        };
        list.refresh(cx);
        list
    }

    /// Read the capture store again. The host calls this when traffic arrives.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        // A poisoned lock means a capture thread died mid-write: keep what is shown.
        let Ok(store) = self.store.lock() else {
            return;
        };
        self.state.update(cx, |state, cx| {
            sync(state, &store);
            cx.notify();
        });
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.clear();
            cx.notify();
        });
        if let Ok(mut store) = self.store.lock() {
            store.clear();
        }
    }

    fn toggle_type(&mut self, kind: &'static str, cx: &mut Context<Self>) {
        match self.type_filter.iter().position(|active| *active == kind) {
            Some(index) => {
                self.type_filter.remove(index);
            }
            None => self.type_filter.push(kind),
        }
        cx.notify();
    }

    fn on_checkbox(&mut self, id: &str, shift: bool, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            if shift {
                state.select_range(id);
            } else {
                state.toggle_select(id);
            }
            cx.notify();
        });
    }

    fn on_row_click(&mut self, id: &str, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            if state.detail_id.as_deref() == Some(id) {
                state.close_detail();
            } else {
                state.open_detail(id);
            }
            cx.notify();
        });
    }

    fn on_ask_about(&mut self, cx: &mut Context<Self>) {
        let prefill = {
            let state = self.state.read(cx);
            let rows: Vec<&TrafficEntry> = state
                .selected
                .iter()
                .filter_map(|id| state.entries.get(id))
                .collect();
            if rows.is_empty() {
                return;
            }
            build_prefill(&rows)
        };
        cx.emit(TrafficListEvent::PushDraft(prefill));
        self.state.update(cx, |state, cx| {
            state.clear_selection();
            cx.notify();
        });
    }

    fn header(&self, total: usize, t: &Tokens, cx: &Context<Self>) -> Div {
        let lang = self.lang;
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.0))
            .px(px(10.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(t.theme.border)
            .text_size(px(11.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .child(count_label(self.visible.len(), total)),
                    )
                    .child(div().flex_1())
                    .child(
                        button("traffic-clear", lang.t("common.clear"), t)
                            .tooltip(tooltip(lang.t("traffic.clear"), t))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.clear(cx))),
                    ),
            )
            .child(
                // The search `<input>`. The kit's field is 20px high on its own, so the
                // box takes less padding than the CSS to stay near the same height.
                clickable("traffic-search")
                    .px(px(8.0))
                    .py(px(1.0))
                    .bg(t.surface)
                    .border_1()
                    .border_color(t.theme.border_2)
                    .rounded(px(4.0))
                    .text_size(px(11.0))
                    .child(self.search.clone()),
            )
            .child(
                // `.seg`
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(2.0))
                    .children(QUICK_TYPES.iter().map(|&kind| {
                        let active = self.type_filter.contains(&kind);
                        seg_button(kind, kind, active, t).on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| this.toggle_type(kind, cx),
                        ))
                    }))
                    .child(div().flex_1())
                    .child(
                        clickable("hide-static")
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .text_size(px(10.0))
                            .opacity(0.8)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.hide_static = !this.hide_static;
                                cx.notify();
                            }))
                            .child(checkbox("hide-static-box", self.hide_static, t))
                            .child("hide static"),
                    ),
            )
    }

    /// The `<thead>`: fixed columns around the one that takes the rest. A cell is
    /// its CSS width plus its horizontal padding, as fixed table layout sizes it.
    fn column_header(&self, t: &Tokens) -> Div {
        let lang = self.lang;
        let th = |width: f32, label: &'static str| {
            div()
                .w(px(width))
                .flex_none()
                .px(px(8.0))
                .py(px(6.0))
                .child(div().opacity(0.7).child(label))
        };
        div()
            .flex()
            .flex_none()
            .bg(t.surface)
            .border_b_1()
            .border_color(t.theme.border)
            .font_family(MONO)
            .text_size(px(11.0))
            .child(div().w(px(32.0)).flex_none())
            .child(th(74.0, lang.t("traffic.method")))
            .child(th(62.0, lang.t("traffic.status")))
            .child(th(90.0, lang.t("traffic.type")))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(8.0))
                    .py(px(6.0))
                    .child(div().opacity(0.7).child(lang.t("traffic.url"))),
            )
            .child(th(82.0, lang.t("traffic.size")))
            .child(div().w(px(46.0)).flex_none())
    }

    fn row(
        &self,
        index: usize,
        entry: &TrafficEntry,
        t: &Tokens,
        cx: &Context<Self>,
    ) -> AnyElement {
        let state = self.state.read(cx);
        let is_selected = state.selected.contains(&entry.request_id);
        let is_active = state.detail_id.as_deref() == Some(entry.request_id.as_str());
        let id = entry.request_id.clone();
        let cell = |width: f32| {
            div()
                .w(px(width))
                .flex_none()
                .px(px(8.0))
                .overflow_hidden()
                .whitespace_nowrap()
        };
        let status = entry
            .status
            .map_or("·".to_owned(), |status| status.to_string());
        let size = entry
            .encoded_data_length
            .map_or("·".to_owned(), format_bytes);
        clickable(("traffic-row", index))
            .h(px(ROW_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(t.theme.border)
            .font_family(MONO)
            .text_size(px(11.0))
            .text_color(t.theme.text)
            .cursor_pointer()
            .when(is_active, |row| row.bg(t.row_active))
            .when(!is_active && is_selected, |row| row.bg(t.row_selected))
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _: &ClickEvent, _, cx| this.on_row_click(&id, cx)
            }))
            .child(
                // Clicks in the checkbox cell never open the detail.
                div()
                    .id(("traffic-check-cell", index))
                    .w(px(32.0))
                    .h_full()
                    .flex()
                    .flex_none()
                    .items_center()
                    .px(px(4.0))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(checkbox(("traffic-check", index), is_selected, t).on_click(
                        cx.listener({
                            let id = id.clone();
                            move |this, event: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.on_checkbox(&id, event.modifiers().shift, cx);
                            }
                        }),
                    )),
            )
            .child(cell(74.0).child(entry.method.clone()))
            .child(
                cell(62.0)
                    .text_color(status_color(entry.status, &t.theme))
                    .child(status),
            )
            .child(cell(90.0).child(div().opacity(0.7).child(entry.resource_type.clone())))
            .child(
                div()
                    .id(("traffic-url", index))
                    .flex_1()
                    .min_w_0()
                    .px(px(8.0))
                    .truncate()
                    .tooltip(tooltip(entry.url.clone(), t))
                    .child(entry.url.clone()),
            )
            .child(cell(82.0).flex().justify_end().child(size))
            .child(
                div()
                    .id(("traffic-repeat-cell", index))
                    .w(px(46.0))
                    .h_full()
                    .flex()
                    .flex_none()
                    .items_center()
                    .px(px(4.0))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        ghost_button(("traffic-repeat", index), "↻R", t)
                            .px(px(5.0))
                            .py(px(0.0))
                            .text_size(px(11.0))
                            .tooltip(tooltip(self.lang.t("traffic.sendToRepeater"), t))
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                cx.emit(TrafficListEvent::SendToRepeater(id.clone()));
                            })),
                    ),
            )
            .into_any_element()
    }

    fn selection_bar(&self, count: usize, t: &Tokens, cx: &Context<Self>) -> Div {
        div()
            .absolute()
            .left(px(12.0))
            .right(px(12.0))
            .bottom(px(12.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(12.0))
            .py(px(8.0))
            .bg(t.chip_ok_bg)
            .border_1()
            .border_color(t.chip_ok_border)
            .rounded(px(6.0))
            .text_size(px(12.0))
            .shadow(vec![BoxShadow {
                color: rgba(0x00000080).into(),
                offset: point(px(0.0), px(2.0)),
                blur_radius: px(12.0),
                spread_radius: px(0.0),
            }])
            // The rows under the bar must not take its clicks.
            .occlude()
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .child(format!("{count} selected")),
            )
            .child(div().flex_1())
            .child(
                primary_button("traffic-ask", "Ask about", t)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.on_ask_about(cx))),
            )
            .child(
                button("traffic-clear-selection", self.lang.t("common.clear"), t).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.state.update(cx, |state, cx| {
                            state.clear_selection();
                            cx.notify();
                        });
                    }),
                ),
            )
    }
}

impl Render for TrafficList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::resolve(self.theme_mode, window);
        let (total, selected) = {
            let state = self.state.read(cx);
            let full: Vec<&TrafficEntry> = state
                .order
                .iter()
                .filter_map(|id| state.entries.get(id))
                .collect();
            let search = self.search.read(cx).text();
            self.visible = filter_entries(&full, search, &self.type_filter, self.hide_static)
                .into_iter()
                .cloned()
                .collect();
            (full.len(), state.selected.len())
        };
        // Only the rows in view are built, so a full 500-entry buffer costs a couple
        // dozen rows per frame.
        let rows = uniform_list(
            "traffic-rows",
            self.visible.len(),
            cx.processor(move |this, range: Range<usize>, _, cx| {
                range
                    .filter_map(|index| {
                        let entry = this.visible.get(index)?;
                        Some(this.row(index, entry, &t, cx))
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .min_h_0()
        .w_full();

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(self.header(total, &t, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .when(self.visible.is_empty(), |body| {
                        body.child(
                            // `.panel-empty`
                            div()
                                .flex()
                                .flex_none()
                                .justify_center()
                                .px(px(12.0))
                                .py(px(24.0))
                                .text_size(px(11.0))
                                .text_color(t.theme.text_dim)
                                .child("Open a page — traffic will appear here."),
                        )
                    })
                    .child(self.column_header(&t))
                    .child(rows),
            )
            .when(selected > 0, |root| {
                root.child(self.selection_bar(selected, &t, cx))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rever_core::traffic::RequestPatch;

    fn entry(id: &str, method: &str, url: &str, kind: &str) -> TrafficEntry {
        TrafficEntry {
            request_id: id.into(),
            url: url.into(),
            method: method.into(),
            resource_type: kind.into(),
            ..TrafficEntry::default()
        }
    }

    fn ids(rows: &[&TrafficEntry]) -> Vec<String> {
        rows.iter().map(|row| row.request_id.clone()).collect()
    }

    fn sample() -> Vec<TrafficEntry> {
        vec![
            entry("doc", "GET", "https://shop.test/", "Document"),
            entry("css", "GET", "https://shop.test/app.css", "Stylesheet"),
            entry("img", "GET", "https://cdn.test/Logo.PNG", "Image"),
            entry("api", "POST", "https://api.test/v1/Cart", "XHR"),
            entry("js", "GET", "https://shop.test/app.js", "Script"),
            entry("font", "GET", "https://cdn.test/a.woff2", "Font"),
            entry("vid", "GET", "https://cdn.test/a.mp4", "Media"),
            entry("ws", "GET", "wss://api.test/live", "WebSocket"),
        ]
    }

    #[test]
    fn 정적_리소스_숨기기는_이미지_스타일_폰트_미디어를_뺀다() {
        let all = sample();
        let full: Vec<&TrafficEntry> = all.iter().collect();
        assert_eq!(
            ids(&filter_entries(&full, "", &[], true)),
            ["doc", "api", "js", "ws"]
        );
        assert_eq!(filter_entries(&full, "", &[], false).len(), all.len());
    }

    #[test]
    fn 유형_버튼은_고른_유형만_남긴다() {
        let all = sample();
        let full: Vec<&TrafficEntry> = all.iter().collect();
        assert_eq!(ids(&filter_entries(&full, "", &["XHR"], true)), ["api"]);
        assert_eq!(
            ids(&filter_entries(&full, "", &["Script", "Document"], true)),
            ["doc", "js"]
        );
        // A type that "hide static" removes stays hidden even when it is picked.
        assert!(filter_entries(&full, "", &["Image"], true).is_empty());
        assert_eq!(ids(&filter_entries(&full, "", &["Image"], false)), ["img"]);
    }

    #[test]
    fn 검색은_주소와_메서드를_대소문자_없이_찾는다() {
        let all = sample();
        let full: Vec<&TrafficEntry> = all.iter().collect();
        assert_eq!(ids(&filter_entries(&full, "cart", &[], true)), ["api"]);
        assert_eq!(ids(&filter_entries(&full, "  POST ", &[], true)), ["api"]);
        assert_eq!(ids(&filter_entries(&full, "logo.png", &[], false)), ["img"]);
        assert_eq!(
            ids(&filter_entries(&full, "shop.test", &[], true)),
            ["doc", "js"]
        );
        assert!(filter_entries(&full, "nothing", &[], false).is_empty());
        // Only whitespace is no query at all.
        assert_eq!(
            filter_entries(&full, " \t\u{a0}", &[], false).len(),
            all.len()
        );
        // The resource type is not searched.
        assert!(filter_entries(&full, "xhr", &[], false).is_empty());
    }

    #[test]
    fn 세_가지_필터는_함께_적용된다() {
        let all = sample();
        let full: Vec<&TrafficEntry> = all.iter().collect();
        assert_eq!(
            ids(&filter_entries(&full, "shop", &["Script", "XHR"], true)),
            ["js"]
        );
    }

    #[test]
    fn 제목은_걸러진_수와_전체_수를_보여준다() {
        assert_eq!(count_label(0, 0), "Traffic (0)");
        assert_eq!(count_label(12, 12), "Traffic (12)");
        assert_eq!(count_label(3, 40), "Traffic (3 / 40)");
    }

    #[test]
    fn 바이트는_단위를_붙여_적는다() {
        assert_eq!(format_bytes(0), "0B");
        assert_eq!(format_bytes(1023), "1023B");
        assert_eq!(format_bytes(1024), "1.0KB");
        assert_eq!(format_bytes(1536), "1.5KB");
        // toFixed(1) rounds 1.25 up; `{:.1}` would print 1.2.
        assert_eq!(format_bytes(1280), "1.3KB");
        assert_eq!(format_bytes(1024 * 1024 - 1), "1024.0KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0MB");
        assert_eq!(format_bytes(5 * 1024 * 1024 + 512 * 1024), "5.5MB");
        assert_eq!(format_bytes(u64::MAX), "17592186044416.0MB");
    }

    #[test]
    fn 상태_색은_300_미만을_성공으로_본다() {
        let theme = Theme::dark();
        assert_eq!(status_color(None, &theme), theme.http_none);
        assert_eq!(status_color(Some(0), &theme), theme.http_none);
        assert_eq!(status_color(Some(101), &theme), theme.http_2xx);
        assert_eq!(status_color(Some(200), &theme), theme.http_2xx);
        assert_eq!(status_color(Some(299), &theme), theme.http_2xx);
        assert_eq!(status_color(Some(304), &theme), theme.http_3xx);
        assert_eq!(status_color(Some(404), &theme), theme.http_4xx);
        assert_eq!(status_color(Some(499), &theme), theme.http_4xx);
        assert_eq!(status_color(Some(500), &theme), theme.http_5xx);
        assert_eq!(status_color(Some(999), &theme), theme.http_5xx);
    }

    #[test]
    fn 질문_초안은_고른_요청을_한_줄씩_적는다() {
        let mut first = entry("1.1", "GET", "https://a.test/x", "XHR");
        first.status = Some(200);
        let second = entry("1.2", "POST", "https://a.test/y", "Fetch");
        let mut third = entry("1.3", "GET", "https://a.test/z", "XHR");
        third.status = Some(0);
        assert_eq!(
            build_prefill(&[&first, &second, &third]),
            "Analyze these requests. Use get_request for details if needed:\n\
             - GET https://a.test/x (id: 1.1, status: 200)\n\
             - POST https://a.test/y (id: 1.2)\n\
             - GET https://a.test/z (id: 1.3)\n\nQuestion: "
        );
    }

    fn stored(id: &str) -> StoredRequest {
        StoredRequest {
            request_id: id.into(),
            url: "https://a.test/api".into(),
            host: "a.test".into(),
            method: "GET".into(),
            resource_type: "XHR".into(),
            started_at: 1000,
            ..StoredRequest::default()
        }
    }

    #[test]
    fn 처음_보는_요청은_요청_이벤트가_된다() {
        assert_eq!(
            pending_events(&stored("1"), None),
            [NetworkEvent::Request {
                request_id: "1".into(),
                url: "https://a.test/api".into(),
                method: "GET".into(),
                resource_type: "XHR".into(),
                timestamp: 1000.0,
            }]
        );
    }

    #[test]
    fn 이미_끝난_요청은_요청_응답_완료가_차례로_나온다() {
        let mut done = stored("1");
        done.status = Some(201);
        done.mime_type = Some("application/json".into());
        done.completed_at = Some(1250);
        done.encoded_data_length = Some(77);
        let events = pending_events(&done, None);
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[1],
            NetworkEvent::Response {
                request_id: "1".into(),
                status: 201,
                mime_type: "application/json".into(),
                timestamp: 1250.0,
            }
        );
        assert_eq!(
            events[2],
            NetworkEvent::Finished {
                request_id: "1".into(),
                encoded_data_length: 77,
                timestamp: 1250.0,
            }
        );
    }

    #[test]
    fn 이미_반영한_내용은_다시_보내지_않는다() {
        let mut done = stored("1");
        done.status = Some(200);
        done.completed_at = Some(1250);
        let mut state = TrafficState::default();
        for event in pending_events(&done, None) {
            state.apply_event(event);
        }
        assert!(pending_events(&done, state.entries.get("1")).is_empty());

        // A status that changes later (a redirect followed) is sent again.
        done.status = Some(404);
        let events = pending_events(&done, state.entries.get("1"));
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            NetworkEvent::Response { status: 404, .. }
        ));
    }

    #[test]
    fn 캡처_저장소를_읽어_목록_상태를_맞춘다() {
        let mut store = TrafficStore::default();
        let patch = |id: &str, url: &str, kind: &str| RequestPatch {
            request_id: id.into(),
            url: Some(url.into()),
            method: Some("GET".into()),
            resource_type: Some(kind.into()),
            ..RequestPatch::default()
        };
        store.upsert(patch("a", "https://a.test/", "Document"), 10);
        store.upsert(patch("b", "https://a.test/api/users/7", "XHR"), 20);
        let mut state = TrafficState::default();
        sync(&mut state, &store);
        assert_eq!(state.order, ["a", "b"]);
        assert_eq!(state.entries["b"].status, None);
        assert_eq!(state.endpoints.len(), 1);
        assert_eq!(state.endpoints[0].path, "/api/users/:id");

        store.upsert(
            RequestPatch {
                request_id: "b".into(),
                status: Some(200),
                mime_type: Some("application/json".into()),
                completed_at: Some(35),
                encoded_data_length: Some(2048),
                ..RequestPatch::default()
            },
            35,
        );
        store.upsert(patch("c", "https://a.test/app.js", "Script"), 40);
        sync(&mut state, &store);
        assert_eq!(state.order, ["a", "b", "c"]);
        let b = &state.entries["b"];
        assert_eq!(b.status, Some(200));
        assert_eq!(b.mime_type.as_deref(), Some("application/json"));
        assert_eq!(b.encoded_data_length, Some(2048));
        assert_eq!(b.completed_at, Some(35.0));
        assert_eq!(state.endpoints[0].statuses, [200]);
        assert_eq!(state.endpoints[0].count, 1);

        // Nothing new: a second pass changes nothing.
        let before = state.clone();
        sync(&mut state, &store);
        assert_eq!(state.order, before.order);
        assert_eq!(state.entries, before.entries);
        assert_eq!(state.endpoints, before.endpoints);
    }

    #[test]
    fn 오백_개를_넘는_캡처도_최근_오백_개로_맞춘다() {
        let mut store = TrafficStore::default();
        for i in 0..MAX_ENTRIES + 20 {
            store.upsert(
                RequestPatch {
                    request_id: format!("r{i}"),
                    url: Some("https://a.test/".into()),
                    method: Some("GET".into()),
                    resource_type: Some("Document".into()),
                    ..RequestPatch::default()
                },
                i as u64,
            );
        }
        let mut state = TrafficState::default();
        sync(&mut state, &store);
        assert_eq!(state.order.len(), MAX_ENTRIES);
        assert_eq!(state.order.first().map(String::as_str), Some("r20"));
        assert_eq!(
            state.order.last().map(String::as_str),
            Some(format!("r{}", MAX_ENTRIES + 19).as_str())
        );
    }

    // ── The view, in a test window ──────────────────────────────────────────

    use gpui::{Modifiers, Pixels, Point, TestAppContext, VisualTestContext, point};
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Opened<'a> {
        list: Entity<TrafficList>,
        state: Entity<TrafficState>,
        store: Arc<Mutex<TrafficStore>>,
        events: Rc<RefCell<Vec<TrafficListEvent>>>,
        cx: &'a mut VisualTestContext,
    }

    fn capture(id: &str, url: &str, kind: &str) -> RequestPatch {
        RequestPatch {
            request_id: id.into(),
            url: Some(url.into()),
            method: Some("GET".into()),
            resource_type: Some(kind.into()),
            ..RequestPatch::default()
        }
    }

    /// The list alone in a window, over a capture store holding `requests`.
    fn open<'a>(requests: &[(&str, &str, &str)], cx: &'a mut TestAppContext) -> Opened<'a> {
        cx.update(rever_ui_kit::text_input::bind_keys);
        let mut store = TrafficStore::default();
        for (index, (id, url, kind)) in requests.iter().enumerate() {
            store.upsert(capture(id, url, kind), index as u64 + 1);
        }
        let store = Arc::new(Mutex::new(store));
        let state = cx.new(|_| TrafficState::default());
        let (list, cx) = cx.add_window_view({
            let (store, state) = (store.clone(), state.clone());
            move |_, cx| TrafficList::new(store, state, Lang::En, ThemeMode::Light, cx)
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        cx.update(|_, cx| {
            let events = events.clone();
            cx.subscribe(&list, move |_, event: &TrafficListEvent, _| {
                events.borrow_mut().push(event.clone())
            })
            .detach();
        });
        Opened {
            list,
            state,
            store,
            events,
            cx,
        }
    }

    impl Opened<'_> {
        fn center(&mut self, selector: &'static str) -> Point<Pixels> {
            self.cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} is not on screen"))
                .center()
        }

        fn click(&mut self, selector: &'static str) {
            let at = self.center(selector);
            self.cx.simulate_click(at, Modifiers::none());
        }

        fn visible(&mut self) -> Vec<String> {
            self.list.read_with(self.cx, |list, _| {
                list.visible.iter().map(|e| e.request_id.clone()).collect()
            })
        }

        fn selected(&mut self) -> Vec<String> {
            self.state
                .read_with(self.cx, |state, _| state.selected.clone())
        }

        fn detail(&mut self) -> Option<String> {
            self.state
                .read_with(self.cx, |state, _| state.detail_id.clone())
        }
    }

    const FOUR: [(&str, &str, &str); 4] = [
        ("doc", "https://shop.test/", "Document"),
        ("api", "https://api.test/v1/cart", "XHR"),
        ("img", "https://cdn.test/logo.png", "Image"),
        ("js", "https://shop.test/app.js", "Script"),
    ];

    #[gpui::test]
    fn 행을_누르면_상세가_열리고_같은_행을_다시_누르면_닫힌다(
        cx: &mut TestAppContext,
    ) {
        let mut t = open(&FOUR, cx);
        assert_eq!(t.visible(), ["doc", "api", "js"]);
        t.click("traffic-row-0");
        assert_eq!(t.detail().as_deref(), Some("doc"));
        t.click("traffic-row-2");
        assert_eq!(t.detail().as_deref(), Some("js"));
        t.click("traffic-row-2");
        assert_eq!(t.detail(), None);
        assert!(t.selected().is_empty());
    }

    #[gpui::test]
    fn 체크박스는_선택만_바꾸고_상세를_열지_않는다(cx: &mut TestAppContext) {
        let mut t = open(&FOUR, cx);
        t.click("traffic-check-0");
        assert_eq!(t.selected(), ["doc"]);
        assert_eq!(t.detail(), None);
        t.click("traffic-check-2");
        assert_eq!(t.selected(), ["doc", "js"]);
        t.click("traffic-check-0");
        assert_eq!(t.selected(), ["js"]);
        assert_eq!(t.detail(), None);
    }

    #[gpui::test]
    fn 시프트를_누른_체크는_사이의_행을_모두_고른다(cx: &mut TestAppContext) {
        let mut t = open(&FOUR, cx);
        t.click("traffic-check-0");
        let last = t.center("traffic-check-2");
        t.cx.simulate_click(last, Modifiers::shift());
        // The range runs over the capture order, so the hidden image row is taken too.
        assert_eq!(t.selected(), ["doc", "api", "img", "js"]);
        assert_eq!(t.detail(), None);
    }

    #[gpui::test]
    fn 체크박스_칸과_리피터_버튼은_상세를_열지_않는다(cx: &mut TestAppContext) {
        let mut t = open(&FOUR, cx);
        // Beside the checkbox, still inside its cell.
        let row = t.cx.debug_bounds("traffic-row-1").unwrap();
        t.cx.simulate_click(
            point(row.left() + px(1.0), row.center().y),
            Modifiers::none(),
        );
        assert_eq!(t.detail(), None);
        assert!(t.selected().is_empty());

        t.click("traffic-repeat-1");
        assert_eq!(t.detail(), None);
        assert_eq!(
            *t.events.borrow(),
            [TrafficListEvent::SendToRepeater("api".into())]
        );
    }

    #[gpui::test]
    fn 유형_버튼과_정적_숨김은_보이는_행을_바꾼다(cx: &mut TestAppContext) {
        let mut t = open(&FOUR, cx);
        t.click("XHR");
        assert_eq!(t.visible(), ["api"]);
        t.click("Script");
        assert_eq!(t.visible(), ["api", "js"]);
        t.click("XHR");
        assert_eq!(t.visible(), ["js"]);
        t.click("Script");
        assert_eq!(t.visible(), ["doc", "api", "js"]);
        t.click("hide-static");
        assert_eq!(t.visible(), ["doc", "api", "img", "js"]);
        t.click("hide-static");
        assert_eq!(t.visible(), ["doc", "api", "js"]);
    }

    #[gpui::test]
    fn 검색창에_입력하면_목록이_걸러진다(cx: &mut TestAppContext) {
        let mut t = open(&FOUR, cx);
        t.click("traffic-search");
        t.cx.simulate_input("SHOP");
        assert_eq!(t.visible(), ["doc", "js"]);
        t.cx.simulate_input(".test/app");
        assert_eq!(t.visible(), ["js"]);
        t.cx.simulate_keystrokes("cmd-a backspace");
        assert_eq!(t.visible(), ["doc", "api", "js"]);
    }

    #[gpui::test]
    fn 질문하기는_고른_요청으로_초안을_내보내고_선택을_비운다(
        cx: &mut TestAppContext,
    ) {
        let mut t = open(&FOUR, cx);
        assert!(t.cx.debug_bounds("traffic-ask").is_none());
        t.click("traffic-check-2");
        t.click("traffic-check-1");
        t.click("traffic-ask");
        assert_eq!(
            *t.events.borrow(),
            [TrafficListEvent::PushDraft(
                "Analyze these requests. Use get_request for details if needed:\n\
                 - GET https://shop.test/app.js (id: js)\n\
                 - GET https://api.test/v1/cart (id: api)\n\nQuestion: "
                    .into()
            )]
        );
        assert!(t.selected().is_empty());
        // The bar went away with the selection: pressing where it was sends nothing more.
        t.click("traffic-ask");
        assert_eq!(t.events.borrow().len(), 1);
    }

    #[gpui::test]
    fn 선택_막대의_지우기는_선택만_비운다(cx: &mut TestAppContext) {
        let mut t = open(&FOUR, cx);
        t.click("traffic-check-0");
        t.click("traffic-clear-selection");
        assert!(t.selected().is_empty());
        assert_eq!(t.visible(), ["doc", "api", "js"]);
        assert!(t.events.borrow().is_empty());
    }

    #[gpui::test]
    fn 지우기는_목록과_캡처_저장소를_함께_비운다(cx: &mut TestAppContext) {
        let mut t = open(&FOUR, cx);
        t.click("traffic-check-0");
        t.click("traffic-row-1");
        t.click("traffic-clear");
        assert!(t.visible().is_empty());
        assert!(t.selected().is_empty());
        assert_eq!(t.detail(), None);
        assert!(t.store.lock().unwrap().is_empty());
        // Nothing comes back on the next refresh.
        t.list.update(t.cx, |list, cx| list.refresh(cx));
        assert!(t.visible().is_empty());
    }

    #[gpui::test]
    fn 새_트래픽은_refresh_때_나타난다(cx: &mut TestAppContext) {
        let mut t = open(&FOUR, cx);
        {
            let mut store = t.store.lock().unwrap();
            store.upsert(capture("new", "https://api.test/v1/orders", "Fetch"), 50);
            store.upsert(
                RequestPatch {
                    request_id: "api".into(),
                    status: Some(503),
                    completed_at: Some(60),
                    encoded_data_length: Some(1536),
                    ..RequestPatch::default()
                },
                60,
            );
        }
        assert_eq!(t.visible(), ["doc", "api", "js"]);
        t.list.update(t.cx, |list, cx| list.refresh(cx));
        t.cx.run_until_parked();
        assert_eq!(t.visible(), ["doc", "api", "js", "new"]);
        let api = t
            .state
            .read_with(t.cx, |state, _| state.entries["api"].clone());
        assert_eq!(api.status, Some(503));
        assert_eq!(api.encoded_data_length, Some(1536));
    }

    #[gpui::test]
    fn 오백_행이어도_화면에_보이는_행만_만든다(cx: &mut TestAppContext) {
        let requests: Vec<(String, String)> = (0..MAX_ENTRIES)
            .map(|i| (format!("r{i}"), format!("https://a.test/{i}")))
            .collect();
        let requests: Vec<(&str, &str, &str)> = requests
            .iter()
            .map(|(id, url)| (id.as_str(), url.as_str(), "Fetch"))
            .collect();
        let mut t = open(&requests, cx);
        assert_eq!(t.visible().len(), MAX_ENTRIES);
        let first = t.cx.debug_bounds("traffic-row-0").unwrap();
        assert_eq!(first.size.height, px(ROW_HEIGHT));
        assert!(t.cx.debug_bounds("traffic-row-1").is_some());
        // Far below the window: never built.
        assert!(t.cx.debug_bounds("traffic-row-499").is_none());
        assert!(t.cx.debug_bounds("traffic-row-200").is_none());
    }
}
