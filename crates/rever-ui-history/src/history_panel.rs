//! Port of `src/renderer/src/components/panels/HistoryPanel.tsx`: the searchable list of visited pages, grouped by day.
//!
//! The logic (search, order, day groups, labels, which rows are on screen) is in plain
//! functions; [`HistoryPanel`] lays their result out as the component does. Clicking an
//! entry emits [`OpenUrl`] where the component called the navigation-request store.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::{Datelike, Local, NaiveDate, TimeZone, Utc};
use gpui::{
    AnyElement, ClickEvent, Context, Div, Entity, EventEmitter, FontWeight, PromptButton,
    PromptLevel, Rgba, ScrollHandle, StyledImage, Window, div, img, prelude::*, px,
};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::text_input::TextInput;
use rever_ui_kit::theme::{Theme, ThemeMode};
use url::Url;

use crate::history::{HistoryEntry, HistoryStore, now_ms};
use crate::{LocalTokens, MONO_FONT, OpenUrl};

// Inline English in the component (not dictionary keys), kept as it is there.
const CLEAR_CONFIRM: &str = "Clear all history?";
const EMPTY_NO_HISTORY: &str = "No history yet — navigate somewhere to start.";
const EMPTY_NO_MATCHES: &str = "No matches.";

/// A day header: 4px padding above and below a 10px label, and the 1px bottom border.
pub const HEADER_HEIGHT: f32 = 21.0;
/// An entry: 6px padding above and below the title and URL lines with 1px between
/// them, and the 1px bottom border. Both lines are 16px: the row is a `<button>`, and the
/// stylesheet's `button { line-height: 16px }` reaches the two lines inside it.
pub const ROW_HEIGHT: f32 = 46.0;
/// An entry with nothing to show as a title (no title, and a URL without a host): the
/// empty title `<div>` takes no line, so the row is one line shorter.
pub const ROW_HEIGHT_UNTITLED: f32 = 30.0;
/// `line-height` of everything in an entry row.
const ROW_LINE: f32 = 16.0;
/// Rows kept laid out beyond each edge of the list, so a resize or a fast scroll does
/// not show a gap before the next frame.
const OVERSCAN: f32 = 240.0;

// ── Search, order, groups ───────────────────────────────────────────────────

/// `String.prototype.trim`: its whitespace set is not Rust's (U+FEFF counts, U+0085 does not).
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| {
        matches!(
            c,
            '\u{9}'..='\u{d}'
                | ' '
                | '\u{a0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
        )
    })
}

/// Newest first; with a search text, only the entries whose URL or title contains it,
/// ignoring case.
pub fn filter_entries<'a>(entries: &'a [HistoryEntry], filter: &str) -> Vec<&'a HistoryEntry> {
    let query = js_trim(filter).to_lowercase();
    let mut sorted: Vec<&HistoryEntry> = entries.iter().collect();
    // Stable, as Array#sort is: visits in the same millisecond keep their order.
    sorted.sort_by_key(|entry| std::cmp::Reverse(entry.visited_at));
    if !query.is_empty() {
        sorted.retain(|entry| {
            entry.url.to_lowercase().contains(&query) || entry.title.to_lowercase().contains(&query)
        });
    }
    sorted
}

#[derive(Debug, PartialEq)]
pub struct DayGroup<'a> {
    pub label: String,
    pub items: Vec<&'a HistoryEntry>,
}

/// Consecutive entries with the same day label form a group.
pub fn group_by_day<'a, Tz: TimeZone>(
    filtered: &[&'a HistoryEntry],
    now_ms: u64,
    zone: &Tz,
) -> Vec<DayGroup<'a>> {
    let mut groups: Vec<DayGroup<'a>> = Vec::new();
    for &entry in filtered {
        let label = day_label(entry.visited_at, now_ms, zone);
        match groups.last_mut() {
            Some(group) if group.label == label => group.items.push(entry),
            _ => groups.push(DayGroup {
                label,
                items: vec![entry],
            }),
        }
    }
    groups
}

fn local_date<Tz: TimeZone>(ms: u64, zone: &Tz) -> Option<NaiveDate> {
    let instant = zone
        .timestamp_millis_opt(i64::try_from(ms).ok()?)
        .single()?;
    Some(instant.date_naive())
}

/// "Today" and "Yesterday" by the calendar of `zone`; any other day as `YYYY-MM-DD`.
pub fn day_label<Tz: TimeZone>(ms: u64, now_ms: u64, zone: &Tz) -> String {
    let (Some(day), Some(today)) = (local_date(ms, zone), local_date(now_ms, zone)) else {
        // An instant no calendar can hold. The component throws here (toISOString on an
        // Invalid Date); a label is kinder than taking the panel down.
        return "Invalid Date".to_string();
    };
    if day == today {
        return "Today".to_string();
    }
    if today.pred_opt() == Some(day) {
        return "Yesterday".to_string();
    }
    // The component takes this date from toISOString, which is UTC, while the two
    // checks above are local: an older visit can carry the neighbouring day's date.
    let utc = local_date(ms, &Utc).unwrap_or(day);
    if utc.year() > 9999 {
        // toISOString writes such years as +YYYYYY; the component keeps ten characters.
        format!("+{:06}-{:02}", utc.year(), utc.month())
    } else {
        format!("{:04}-{:02}-{:02}", utc.year(), utc.month(), utc.day())
    }
}

/// `HH:MM` on the clock of `zone`: `toTimeString().slice(0, 5)`.
pub fn time_label<Tz: TimeZone>(ms: u64, zone: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    i64::try_from(ms)
        .ok()
        .and_then(|ms| zone.timestamp_millis_opt(ms).single())
        // "Invalid Date".slice(0, 5)
        .map_or_else(
            || "Inval".to_string(),
            |time| time.format("%H:%M").to_string(),
        )
}

/// `new URL(url).hostname`, or `None` where the constructor throws.
fn url_hostname(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    Some(parsed.host_str().unwrap_or_default().to_string())
}

/// The hostname, or the text itself when it is not a URL.
pub fn hostname_of(url: &str) -> String {
    url_hostname(url).unwrap_or_else(|| url.to_string())
}

/// What an entry is listed as: its title, or the hostname while it has none.
pub fn display_title(entry: &HistoryEntry) -> String {
    if entry.title.is_empty() {
        hostname_of(&entry.url)
    } else {
        entry.title.clone()
    }
}

/// Text for a one-line slot. `white-space: nowrap` lays line breaks out as spaces;
/// GPUI would start a new line (and its single-line shaper rejects them outright).
pub(crate) fn single_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

pub fn count_label(count: usize) -> String {
    format!("{count} entries")
}

/// The placeholder for an empty list: one text before any visit, another when the
/// search matches nothing.
pub fn empty_message(total: usize, shown: usize) -> Option<&'static str> {
    match (shown, total) {
        (0, 0) => Some(EMPTY_NO_HISTORY),
        (0, _) => Some(EMPTY_NO_MATCHES),
        _ => None,
    }
}

// ── Favicon ─────────────────────────────────────────────────────────────────

/// The host a favicon can be asked for; `None` for text that is not a URL or has no host.
pub fn favicon_host(url: &str) -> Option<String> {
    url_hostname(url).filter(|host| !host.is_empty())
}

/// Google's s2 service resolves a site's real favicon by hostname.
pub fn favicon_url(host: &str) -> String {
    format!("https://www.google.com/s2/favicons?domain={host}&sz=32")
}

/// Where the host keeps the icons of the pages it has shown, one `<host>.png` each.
static FAVICON_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The host saves a page's icon under [`favicon_file`]; those are shown before asking
/// the network, which needs an HTTP client the application may not have.
pub fn set_favicon_dir(dir: PathBuf) {
    let _ = FAVICON_DIR.set(dir);
}

/// The file the icon of `host` is saved in; `None` until [`set_favicon_dir`].
pub fn favicon_file(host: &str) -> Option<PathBuf> {
    favicon_file_in(FAVICON_DIR.get()?, host)
}

fn favicon_file_in(dir: &Path, host: &str) -> Option<PathBuf> {
    // A host name has no path separator, but this must never leave the directory.
    if host.is_empty() || host.contains(['/', '\\']) || host == ".." {
        return None;
    }
    Some(dir.join(format!("{host}.png")))
}

/// The `Favicon` component, shared with the bookmark bar: the site's icon, a grey square
/// for a URL without a host, and an empty square of the same size while the icon is
/// loading or when it cannot be loaded. The icon is fetched with the application's HTTP
/// client; a host that sets none gets the empty square.
pub(crate) fn favicon(url: &str, placeholder: Rgba) -> AnyElement {
    let square = || div().size(px(16.0)).flex_none().rounded(px(2.0));
    match favicon_host(url) {
        None => square().bg(placeholder).into_any_element(),
        Some(host) => match favicon_file(&host).filter(|file| file.is_file()) {
            Some(file) => img(file),
            None => img(favicon_url(&host)),
        }
        .size(px(16.0))
        .flex_none()
        .rounded(px(2.0))
        .with_fallback(move || square().into_any_element())
        .into_any_element(),
    }
}

// ── Rows on screen ──────────────────────────────────────────────────────────

/// The list as it scrolls: each group's header followed by its entries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Row<'a> {
    Header(&'a str),
    Entry(&'a HistoryEntry),
}

impl Row<'_> {
    fn height(&self) -> f32 {
        match self {
            Row::Header(_) => HEADER_HEIGHT,
            Row::Entry(entry) => entry_height(entry),
        }
    }
}

fn entry_height(entry: &HistoryEntry) -> f32 {
    // The title is checked first, so only an untitled entry costs a URL parse.
    if entry.title.is_empty() && hostname_of(&entry.url).is_empty() {
        ROW_HEIGHT_UNTITLED
    } else {
        ROW_HEIGHT
    }
}

pub fn flatten<'a>(groups: &'a [DayGroup<'a>]) -> Vec<Row<'a>> {
    groups
        .iter()
        .flat_map(|group| {
            std::iter::once(Row::Header(&group.label))
                .chain(group.items.iter().map(|entry| Row::Entry(entry)))
        })
        .collect()
}

fn total_height(rows: &[Row]) -> f32 {
    rows.iter().map(Row::height).sum()
}

/// The scroll position the list will settle on: GPUI clamps a stale offset only while
/// painting, after the rows for that frame have been chosen.
fn clamp_scroll(scroll_top: f32, total: f32, viewport: f32) -> f32 {
    scroll_top.clamp(0.0, (total - viewport).max(0.0))
}

#[derive(Debug, PartialEq)]
pub struct VisibleRows {
    pub range: Range<usize>,
    /// Height standing in for the rows before the range.
    pub pad_top: f32,
    /// Height standing in for the rows after the range.
    pub pad_bottom: f32,
}

/// The rows that intersect the viewport (plus the overscan). The history can hold 1000
/// entries; laying out only these keeps a frame cheap, and the two paddings keep the
/// scrolled height what it would be with every row present.
pub fn visible_rows(rows: &[Row], scroll_top: f32, viewport: f32) -> VisibleRows {
    let (from, to) = (scroll_top - OVERSCAN, scroll_top + viewport + OVERSCAN);
    let mut range = 0..0;
    let (mut pad_top, mut shown_bottom, mut top) = (0.0, 0.0, 0.0);
    for (index, row) in rows.iter().enumerate() {
        let bottom = top + row.height();
        if bottom > from && top < to {
            if range.is_empty() {
                range.start = index;
                pad_top = top;
            }
            range.end = index + 1;
            shown_bottom = bottom;
        }
        top = bottom;
    }
    if range.is_empty() {
        // Nothing intersects (an empty list): one padding carries the whole height.
        shown_bottom = top;
        pad_top = top;
    }
    VisibleRows {
        range,
        pad_top,
        pad_bottom: top - shown_bottom,
    }
}

#[derive(Debug, PartialEq)]
pub struct StickyHeader {
    /// Index of the header row to pin.
    pub row: usize,
    /// Zero, or how far the next group has pushed the pinned header up (negative).
    pub shift: f32,
}

/// `position: sticky; top: 0` on the day headers: the header of the group at the top of
/// the list stays pinned, and leaves with the last of its group.
pub fn sticky_header(rows: &[Row], scroll_top: f32) -> Option<StickyHeader> {
    let mut pinned = None;
    let mut group_end = 0.0;
    let mut top = 0.0;
    for (index, row) in rows.iter().enumerate() {
        if matches!(row, Row::Header(_)) {
            if top > scroll_top {
                break;
            }
            pinned = Some(index);
        }
        top += row.height();
        group_end = top;
    }
    // The loop stops at the next group's header, so `group_end` is where this group ends.
    pinned.map(|row| StickyHeader {
        row,
        shift: (group_end - scroll_top - HEADER_HEIGHT).min(0.0),
    })
}

// ── View ────────────────────────────────────────────────────────────────────

pub struct HistoryPanel {
    store: Entity<HistoryStore>,
    filter: Entity<TextInput>,
    scroll: ScrollHandle,
    lang: Lang,
    theme_mode: ThemeMode,
}

impl EventEmitter<OpenUrl> for HistoryPanel {}

impl HistoryPanel {
    pub fn new(
        store: Entity<HistoryStore>,
        lang: Lang,
        theme_mode: ThemeMode,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| TextInput::new(lang.t("history.filter"), cx));
        // Typing in the field and any change to the store both change the list.
        cx.observe(&filter, |_, _, cx| cx.notify()).detach();
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        Self {
            store,
            filter,
            scroll: ScrollHandle::new(),
            lang,
            theme_mode,
        }
    }

    /// The Clear button: ask first, as the component does with `confirm()`.
    fn request_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.store.read(cx).entries().is_empty() {
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            CLEAR_CONFIRM,
            None,
            // Spelled out: GPUI's own conversion from "OK" labels the button "Ok".
            &[PromptButton::ok("OK"), PromptButton::cancel("Cancel")],
            cx,
        );
        let store = self.store.clone();
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(0) {
                let _ = store.update(cx, |store, cx| {
                    store.clear();
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

/// The title over the URL, each cut with an ellipsis.
///
/// The component makes this the row's `flex: 1` item. GPUI shapes a text once, at the
/// width of its first measurement, and a flex item is first measured for its
/// min-content size, without a width: the text would never be cut. An absolutely
/// placed box gets its width from its insets, so the first measurement has one.
fn entry_text(entry: &HistoryEntry, theme: &Theme) -> Div {
    // Row padding, the 50px time, the 16px icon, and the two 10px gaps.
    const LEFT: f32 = 10.0 + 50.0 + 10.0 + 16.0 + 10.0;
    let title = single_line(&display_title(entry));
    div()
        .absolute()
        .left(px(LEFT))
        .right(px(10.0))
        // The row's padding: the row is exactly as tall as this box plus its padding.
        .top(px(6.0))
        // An empty title takes no line, as an empty <div> does; the URL keeps its 1px margin.
        .when(!title.is_empty(), |text| {
            text.child(
                div()
                    .h(px(ROW_LINE))
                    .truncate()
                    .text_color(theme.text)
                    .child(title),
            )
        })
        .child(
            div()
                .mt(px(1.0))
                .h(px(ROW_LINE))
                .truncate()
                .opacity(0.55)
                .font_family(MONO_FONT)
                .text_size(px(10.0))
                .child(single_line(&entry.url)),
        )
}

fn day_header(label: &str, theme: &Theme, local: &LocalTokens) -> Div {
    div()
        .h(px(HEADER_HEIGHT))
        .w_full()
        .flex()
        .flex_none()
        .items_center()
        .px(px(10.0))
        .bg(local.surface)
        .border_b_1()
        .border_color(theme.border)
        .text_size(px(10.0))
        .line_height(px(12.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text_dim)
        // text-transform: uppercase
        .child(label.to_uppercase())
}

impl Render for HistoryPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::resolve(self.theme_mode, window.appearance());
        let local = LocalTokens::resolve(self.theme_mode, window.appearance());
        let lang = self.lang;

        let entries = self.store.read(cx).entries();
        let filtered = filter_entries(entries, self.filter.read(cx).text());
        let groups = group_by_day(&filtered, now_ms(), &Local);
        let rows = flatten(&groups);
        let has_history = !entries.is_empty();

        let scroll_top = clamp_scroll(
            -f32::from(self.scroll.offset().y),
            total_height(&rows),
            f32::from(self.scroll.bounds().size.height),
        );
        // The list is never taller than the window, and the window's size is known
        // before the first paint, when the list's own is not.
        let visible = visible_rows(&rows, scroll_top, f32::from(window.viewport_size().height));
        let sticky = sticky_header(&rows, scroll_top);

        let mut list = div()
            .id("history-list")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll);
        if let Some(message) = empty_message(entries.len(), filtered.len()) {
            // `.panel-empty`
            list = list.child(
                div()
                    .px(px(12.0))
                    .py(px(24.0))
                    .text_center()
                    .text_size(px(11.0))
                    // `line-height: normal` of the system font at 11px; GPUI's default
                    // would make the placeholder 5px taller.
                    .line_height(px(13.0))
                    .text_color(theme.text_dim)
                    .child(message),
            );
        }
        if visible.pad_top > 0.0 {
            list = list.child(div().h(px(visible.pad_top)).flex_none());
        }
        for index in visible.range.clone() {
            let Some(row) = rows.get(index) else { break };
            list = list.child(match *row {
                Row::Header(label) => day_header(label, &theme, &local).into_any_element(),
                Row::Entry(entry) => {
                    let url = entry.url.clone();
                    let hover = local.surface;
                    div()
                        .id(("history-row", index))
                        .debug_selector(|| format!("history-row-{index}"))
                        .relative()
                        .h(px(row.height()))
                        .w_full()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(10.0))
                        .px(px(10.0))
                        .border_b_1()
                        .border_color(theme.border)
                        // `button { border-radius: 4px }`: it shows on the hover fill.
                        .rounded(px(4.0))
                        .text_size(px(11.0))
                        .line_height(px(ROW_LINE))
                        .text_color(theme.text_2)
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                            cx.emit(OpenUrl(url.clone()))
                        }))
                        .child(
                            div()
                                .w(px(50.0))
                                .flex_none()
                                .opacity(0.55)
                                .font_family(MONO_FONT)
                                .child(time_label(entry.visited_at, &Local)),
                        )
                        .child(favicon(&entry.url, theme.surface_3))
                        .child(entry_text(entry, &theme))
                        .into_any_element()
                }
            });
        }
        if visible.pad_bottom > 0.0 {
            list = list.child(div().h(px(visible.pad_bottom)).flex_none());
        }

        let pinned = sticky.and_then(|sticky| match rows.get(sticky.row) {
            Some(Row::Header(label)) => Some(
                day_header(label, &theme, &local)
                    .absolute()
                    .top(px(sticky.shift))
                    .left_0()
                    // The rows scrolled under it must not take its clicks or hover,
                    // but the wheel still scrolls the list.
                    .block_mouse_except_scroll()
                    .debug_selector(|| "history-pinned".into()),
            ),
            _ => None,
        });

        let clear = div()
            .id("history-clear")
            .debug_selector(|| "history-clear".into())
            .flex_none()
            .px(px(8.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .bg(theme.surface_3)
            .text_color(theme.text)
            .text_size(px(12.0))
            .line_height(px(16.0))
            .child(lang.t("common.clear"));
        let clear = if has_history {
            let hover = theme.border_2;
            clear
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.request_clear(window, cx)),
                )
        } else {
            // button:disabled
            clear.opacity(0.42)
        };

        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .p(px(10.0))
            .text_size(px(12.0))
            .text_color(theme.text_2)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.0))
                    .mb(px(10.0))
                    .child(
                        // The field's own line is 20px; the box around it is the
                        // component's 23px (11px text, 4px padding, 1px border).
                        div()
                            .id("history-filter")
                            .debug_selector(|| "history-filter".into())
                            .flex_1()
                            .min_w_0()
                            .h(px(23.0))
                            .px(px(8.0))
                            .flex()
                            .items_center()
                            .overflow_hidden()
                            .bg(theme.bg)
                            .text_color(theme.text)
                            .border_1()
                            .border_color(theme.border_2)
                            .rounded(px(3.0))
                            .text_size(px(11.0))
                            .font_family(MONO_FONT)
                            .child(self.filter.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .opacity(0.55)
                            .text_size(px(11.0))
                            .child(count_label(filtered.len())),
                    )
                    .child(clear),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(px(4.0))
                    .bg(theme.bg_bar)
                    .child(list)
                    .children(pinned),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn kst() -> FixedOffset {
        FixedOffset::east_opt(9 * 3600).unwrap()
    }

    /// Milliseconds of a wall-clock time in Seoul.
    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> u64 {
        kst()
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .unwrap()
            .timestamp_millis() as u64
    }

    fn entry(id: &str, url: &str, title: &str, visited_at: u64) -> HistoryEntry {
        HistoryEntry {
            id: id.into(),
            tab_id: "t1".into(),
            url: url.into(),
            title: title.into(),
            visited_at,
        }
    }

    fn ids(entries: &[&HistoryEntry]) -> Vec<String> {
        entries.iter().map(|entry| entry.id.clone()).collect()
    }

    // ── filter_entries ──────────────────────────────────────────────────────

    #[test]
    fn 검색어가_없으면_최근_방문부터_모두_보여준다() {
        let entries = [
            entry("a", "https://a.test/", "A", 10),
            entry("c", "https://c.test/", "C", 30),
            entry("b", "https://b.test/", "B", 20),
        ];
        assert_eq!(ids(&filter_entries(&entries, "")), ["c", "b", "a"]);
        // 공백뿐인 검색어는 검색어가 없는 것이다.
        assert_eq!(ids(&filter_entries(&entries, "  \t ")), ["c", "b", "a"]);
    }

    #[test]
    fn 방문_시각이_같으면_원래_순서를_지킨다() {
        let entries = [
            entry("a", "u", "", 5),
            entry("b", "u", "", 5),
            entry("c", "u", "", 9),
            entry("d", "u", "", 5),
        ];
        assert_eq!(ids(&filter_entries(&entries, "")), ["c", "a", "b", "d"]);
    }

    #[test]
    fn url이나_제목에_검색어가_있으면_대소문자_구분_없이_찾는다() {
        let entries = [
            entry("a", "https://GitHub.com/rust-lang", "Rust", 1),
            entry("b", "https://example.com/", "GitHub mirror", 2),
            entry("c", "https://example.org/", "Other", 3),
        ];
        assert_eq!(ids(&filter_entries(&entries, "github")), ["b", "a"]);
        assert_eq!(ids(&filter_entries(&entries, "GITHUB")), ["b", "a"]);
        assert_eq!(ids(&filter_entries(&entries, "rust")), ["a"]);
    }

    #[test]
    fn 검색어_앞뒤_공백은_무시한다() {
        let entries = [entry("a", "https://a.test/", "한글 제목", 1)];
        assert_eq!(ids(&filter_entries(&entries, "  한글  ")), ["a"]);
        // 가운데 공백은 검색어의 일부다.
        assert!(filter_entries(&entries, "한글  제목").is_empty());
    }

    #[test]
    fn 맞는_항목이_없으면_빈_목록이다() {
        let entries = [entry("a", "https://a.test/", "A", 1)];
        assert!(filter_entries(&entries, "zzz").is_empty());
        assert!(filter_entries(&[], "a").is_empty());
    }

    #[test]
    fn 공백_다듬기는_자바스크립트_trim과_같다() {
        // U+FEFF는 JS에서만 공백이고, U+0085는 Rust에서만 공백이다.
        assert_eq!(js_trim("\u{feff} a b \u{3000}\n"), "a b");
        assert_eq!(js_trim("\u{85}a\u{85}"), "\u{85}a\u{85}");
        assert_eq!(js_trim(" \u{a0}\u{2028}"), "");
    }

    // ── day_label, time_label ───────────────────────────────────────────────

    #[test]
    fn 같은_날이면_today다() {
        let now = at(2026, 11, 13, 10, 0);
        assert_eq!(day_label(at(2026, 11, 13, 0, 0), now, &kst()), "Today");
        assert_eq!(day_label(at(2026, 11, 13, 23, 59), now, &kst()), "Today");
        assert_eq!(day_label(now, now, &kst()), "Today");
    }

    #[test]
    fn 하루_전_날짜면_yesterday다() {
        let now = at(2026, 11, 13, 10, 0);
        assert_eq!(
            day_label(at(2026, 11, 12, 23, 59), now, &kst()),
            "Yesterday"
        );
        assert_eq!(day_label(at(2026, 11, 12, 0, 0), now, &kst()), "Yesterday");
        // 달이 바뀌는 날도 달력으로 하루 전이다.
        let first = at(2026, 12, 1, 0, 30);
        assert_eq!(
            day_label(at(2026, 11, 30, 12, 0), first, &kst()),
            "Yesterday"
        );
    }

    #[test]
    fn 그보다_오래되면_날짜를_쓴다() {
        let now = at(2026, 11, 13, 10, 0);
        assert_eq!(
            day_label(at(2026, 11, 11, 15, 0), now, &kst()),
            "2026-11-11"
        );
        assert_eq!(day_label(at(2025, 1, 5, 12, 0), now, &kst()), "2025-01-05");
    }

    #[test]
    fn 날짜는_utc_기준이라_현지_날짜와_하루_다를_수_있다() {
        // toISOString은 UTC다: 서울 11일 08:00은 UTC로 10일 23:00.
        let now = at(2026, 11, 13, 10, 0);
        assert_eq!(day_label(at(2026, 11, 11, 8, 0), now, &kst()), "2026-11-10");
    }

    #[test]
    fn today와_yesterday는_현지_달력으로_가린다() {
        // 서울 13일 00:30은 UTC로 12일이지만 현지로는 오늘이다.
        let now = at(2026, 11, 13, 0, 45);
        assert_eq!(day_label(at(2026, 11, 13, 0, 30), now, &kst()), "Today");
        // 같은 순간도 UTC 달력에서는 어제가 아니라 오늘이다.
        let utc_now = now;
        assert_eq!(day_label(at(2026, 11, 13, 0, 30), utc_now, &Utc), "Today");
        assert_eq!(
            day_label(at(2026, 11, 12, 8, 0), utc_now, &Utc),
            "Yesterday"
        );
    }

    #[test]
    fn 미래의_날짜는_날짜로_쓴다() {
        let now = at(2026, 11, 13, 10, 0);
        assert_eq!(
            day_label(at(2026, 11, 14, 12, 0), now, &kst()),
            "2026-11-14"
        );
    }

    #[test]
    fn 만_년_이후는_toisostring처럼_열_글자만_남는다() {
        // new Date(253402300800000).toISOString().slice(0, 10) === "+010000-01"
        assert_eq!(day_label(253_402_300_800_000, 0, &Utc), "+010000-01");
    }

    #[test]
    fn 표현할_수_없는_시각은_invalid_date로_쓴다() {
        assert_eq!(day_label(u64::MAX, 0, &Utc), "Invalid Date");
        assert_eq!(time_label(u64::MAX, &Utc), "Inval");
    }

    #[test]
    fn 시각은_현지_24시간제_시분이다() {
        assert_eq!(time_label(at(2026, 11, 13, 9, 5), &kst()), "09:05");
        assert_eq!(time_label(at(2026, 11, 13, 23, 59), &kst()), "23:59");
        // 같은 순간을 UTC로 보면 9시간 이르다.
        assert_eq!(time_label(at(2026, 11, 13, 9, 5), &Utc), "00:05");
    }

    // ── group_by_day ────────────────────────────────────────────────────────

    #[test]
    fn 날짜_라벨이_같은_이웃끼리_묶는다() {
        let now = at(2026, 11, 13, 10, 0);
        let entries = [
            entry("old", "u", "", at(2026, 11, 10, 12, 0)),
            entry("y1", "u", "", at(2026, 11, 12, 9, 0)),
            entry("t1", "u", "", at(2026, 11, 13, 8, 0)),
            entry("t2", "u", "", at(2026, 11, 13, 9, 0)),
            entry("y2", "u", "", at(2026, 11, 12, 22, 0)),
        ];
        let filtered = filter_entries(&entries, "");
        let groups = group_by_day(&filtered, now, &kst());
        let summary: Vec<(String, Vec<String>)> = groups
            .iter()
            .map(|group| (group.label.clone(), ids(&group.items)))
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "Today".to_string(),
                    vec!["t2".to_string(), "t1".to_string()]
                ),
                (
                    "Yesterday".to_string(),
                    vec!["y2".to_string(), "y1".to_string()]
                ),
                ("2026-11-10".to_string(), vec!["old".to_string()]),
            ]
        );
    }

    #[test]
    fn 항목이_없으면_묶음도_없다() {
        assert!(group_by_day(&[], 0, &Utc).is_empty());
    }

    // ── hostname, title, favicon ────────────────────────────────────────────

    #[test]
    fn 호스트_이름을_뽑고_url이_아니면_그대로_돌려준다() {
        assert_eq!(
            hostname_of("https://www.example.com:8443/a?b#c"),
            "www.example.com"
        );
        assert_eq!(hostname_of("http://127.0.0.1:8777/x.html"), "127.0.0.1");
        assert_eq!(hostname_of("http://[::1]:3000/"), "[::1]");
        assert_eq!(hostname_of("https://EXAMPLE.com/"), "example.com");
        assert_eq!(hostname_of("https://한글.kr/"), "xn--bj0bj06e.kr");
        assert_eq!(hostname_of("not a url"), "not a url");
        assert_eq!(hostname_of("example.com/path"), "example.com/path");
        assert_eq!(hostname_of(""), "");
    }

    #[test]
    fn 호스트가_없는_url의_호스트_이름은_빈_문자열이다() {
        // new URL('about:blank').hostname === ''
        assert_eq!(hostname_of("about:blank"), "");
        assert_eq!(hostname_of("file:///tmp/a.html"), "");
        assert_eq!(hostname_of("data:text/html,hi"), "");
    }

    #[test]
    fn 제목이_없으면_호스트_이름을_보여준다() {
        assert_eq!(
            display_title(&entry("a", "https://a.test/x", "제목", 1)),
            "제목"
        );
        assert_eq!(
            display_title(&entry("a", "https://a.test/x", "", 1)),
            "a.test"
        );
        assert_eq!(display_title(&entry("a", "garbage", "", 1)), "garbage");
        assert_eq!(display_title(&entry("a", "about:blank", "", 1)), "");
    }

    #[test]
    fn 파비콘은_호스트가_있을_때만_요청한다() {
        assert_eq!(
            favicon_host("https://github.com/x"),
            Some("github.com".to_string())
        );
        assert_eq!(favicon_host("about:blank"), None);
        assert_eq!(favicon_host("not a url"), None);
        assert_eq!(favicon_host(""), None);
    }

    #[test]
    fn 저장된_파비콘은_폴더_안의_호스트_이름_파일이다() {
        let dir = Path::new("/data/favicons");
        assert_eq!(
            favicon_file_in(dir, "github.com"),
            Some(PathBuf::from("/data/favicons/github.com.png"))
        );
        // 폴더 밖을 가리킬 수 있는 이름은 받지 않는다.
        assert_eq!(favicon_file_in(dir, "../etc"), None);
        assert_eq!(favicon_file_in(dir, ".."), None);
        assert_eq!(favicon_file_in(dir, ""), None);
    }

    #[test]
    fn 파비콘_주소는_구글_s2에_호스트와_크기_32를_넘긴다() {
        assert_eq!(
            favicon_url("github.com"),
            "https://www.google.com/s2/favicons?domain=github.com&sz=32"
        );
    }

    #[test]
    fn 한_줄_자리의_줄바꿈은_공백으로_바꾼다() {
        assert_eq!(single_line("a\nb\r\nc"), "a b  c");
        assert_eq!(single_line("plain"), "plain");
    }

    // ── count, empty ────────────────────────────────────────────────────────

    #[test]
    fn 개수_라벨은_숫자와_entries다() {
        assert_eq!(count_label(0), "0 entries");
        assert_eq!(count_label(1), "1 entries");
        assert_eq!(count_label(1000), "1000 entries");
    }

    #[test]
    fn 빈_목록_문구는_기록이_없을_때와_검색_결과가_없을_때가_다르다() {
        assert_eq!(
            empty_message(0, 0),
            Some("No history yet — navigate somewhere to start.")
        );
        assert_eq!(empty_message(5, 0), Some("No matches."));
        assert_eq!(empty_message(5, 2), None);
    }

    // ── rows on screen ──────────────────────────────────────────────────────

    #[test]
    fn 묶음을_헤더와_항목의_한_줄_목록으로_편다() {
        let (a, b, c) = (
            entry("a", "u", "", 3),
            entry("b", "u", "", 2),
            entry("c", "u", "", 1),
        );
        let groups = [
            DayGroup {
                label: "Today".into(),
                items: vec![&a, &b],
            },
            DayGroup {
                label: "Yesterday".into(),
                items: vec![&c],
            },
        ];
        assert_eq!(
            flatten(&groups),
            [
                Row::Header("Today"),
                Row::Entry(&a),
                Row::Entry(&b),
                Row::Header("Yesterday"),
                Row::Entry(&c),
            ]
        );
        assert!(flatten(&[]).is_empty());
    }

    /// One header and `count` entries.
    fn long_list(entries: &[HistoryEntry]) -> Vec<Row<'_>> {
        std::iter::once(Row::Header("Today"))
            .chain(entries.iter().map(Row::Entry))
            .collect()
    }

    fn many(count: usize) -> Vec<HistoryEntry> {
        (0..count)
            .map(|index| entry(&index.to_string(), "u", "", index as u64))
            .collect()
    }

    #[test]
    fn 짧은_목록은_모든_줄을_그린다() {
        let entries = many(3);
        let rows = long_list(&entries);
        assert_eq!(
            visible_rows(&rows, 0.0, 300.0),
            VisibleRows {
                range: 0..4,
                pad_top: 0.0,
                pad_bottom: 0.0
            }
        );
    }

    #[test]
    fn 긴_목록은_화면_근처의_줄만_그리고_나머지는_여백으로_둔다() {
        let entries = many(100);
        let rows = long_list(&entries);
        let total = HEADER_HEIGHT + 100.0 * ROW_HEIGHT;
        assert_eq!(total_height(&rows), total);

        // 화면 1760..2640(위아래 240 여유 포함)에 걸치는 항목은 37..=56번째.
        let visible = visible_rows(&rows, 2000.0, 400.0);
        assert_eq!(visible.range, 38..58);
        assert_eq!(visible.pad_top, HEADER_HEIGHT + 37.0 * ROW_HEIGHT);
        assert_eq!(visible.pad_bottom, 43.0 * ROW_HEIGHT);
        // 여백과 그린 줄의 높이를 더하면 전체 높이 그대로다.
        let drawn = visible.range.len() as f32 * ROW_HEIGHT;
        assert_eq!(visible.pad_top + drawn + visible.pad_bottom, total);
    }

    #[test]
    fn 맨_위에서는_위_여백이_없고_맨_아래에서는_아래_여백이_없다() {
        let entries = many(100);
        let rows = long_list(&entries);
        let top = visible_rows(&rows, 0.0, 400.0);
        assert_eq!(top.range.start, 0);
        assert_eq!(top.pad_top, 0.0);
        assert!(top.pad_bottom > 0.0);

        let bottom = visible_rows(&rows, total_height(&rows) - 400.0, 400.0);
        assert_eq!(bottom.range.end, rows.len());
        assert_eq!(bottom.pad_bottom, 0.0);
        assert!(bottom.pad_top > 0.0);
    }

    #[test]
    fn 빈_목록이나_범위_밖_스크롤에서는_그릴_줄이_없다() {
        assert_eq!(
            visible_rows(&[], 0.0, 400.0),
            VisibleRows {
                range: 0..0,
                pad_top: 0.0,
                pad_bottom: 0.0
            }
        );
        let entries = many(2);
        let rows = long_list(&entries);
        let beyond = visible_rows(&rows, 10_000.0, 400.0);
        assert!(beyond.range.is_empty());
        assert_eq!(beyond.pad_top + beyond.pad_bottom, total_height(&rows));
    }

    #[test]
    fn 스크롤_위치는_내용_범위_안으로_맞춘다() {
        assert_eq!(clamp_scroll(50.0, 1000.0, 400.0), 50.0);
        assert_eq!(clamp_scroll(900.0, 1000.0, 400.0), 600.0);
        assert_eq!(clamp_scroll(-5.0, 1000.0, 400.0), 0.0);
        // 내용이 화면보다 짧으면 스크롤할 곳이 없다.
        assert_eq!(clamp_scroll(30.0, 100.0, 400.0), 0.0);
    }

    #[test]
    fn 맨_위_묶음의_헤더가_고정된다() {
        // 높이: 헤더 0..21, 항목 21..67, 67..113, 헤더 113..134, 항목 134..180
        let (a, b, c) = (
            entry("a", "u", "", 3),
            entry("b", "u", "", 2),
            entry("c", "u", "", 1),
        );
        let rows = [
            Row::Header("Today"),
            Row::Entry(&a),
            Row::Entry(&b),
            Row::Header("Yesterday"),
            Row::Entry(&c),
        ];
        assert_eq!(
            sticky_header(&rows, 0.0),
            Some(StickyHeader { row: 0, shift: 0.0 })
        );
        assert_eq!(
            sticky_header(&rows, 50.0),
            Some(StickyHeader { row: 0, shift: 0.0 })
        );
        // 다음 헤더가 맨 위에 닿으면 그 헤더로 바뀐다.
        assert_eq!(
            sticky_header(&rows, 113.0),
            Some(StickyHeader { row: 3, shift: 0.0 })
        );
        assert_eq!(
            sticky_header(&rows, 150.0),
            Some(StickyHeader { row: 3, shift: 0.0 })
        );
    }

    #[test]
    fn 다음_묶음이_올라오면_고정된_헤더가_밀려_올라간다() {
        let (a, b, c) = (
            entry("a", "u", "", 3),
            entry("b", "u", "", 2),
            entry("c", "u", "", 1),
        );
        let rows = [
            Row::Header("Today"),
            Row::Entry(&a),
            Row::Entry(&b),
            Row::Header("Yesterday"),
            Row::Entry(&c),
        ];
        // 묶음 끝(113)까지 9px 남았으면 21px 헤더는 12px 밀려 있다.
        assert_eq!(
            sticky_header(&rows, 104.0),
            Some(StickyHeader {
                row: 0,
                shift: -12.0
            })
        );
        assert_eq!(
            sticky_header(&rows, 92.0),
            Some(StickyHeader { row: 0, shift: 0.0 })
        );
    }

    #[test]
    fn 줄_높이는_크롬이_그리는_것과_같다() {
        // 헤드리스 Chrome에서 잰 값: 헤더 21px, 항목 46px.
        assert_eq!(Row::Header("Today").height(), 21.0);
        assert_eq!(
            Row::Entry(&entry("a", "https://a.test/", "A", 1)).height(),
            46.0
        );
        // 제목이 없어도 호스트 이름이나 URL 글자가 제목 자리를 채우면 두 줄이다.
        assert_eq!(
            Row::Entry(&entry("a", "https://a.test/", "", 1)).height(),
            46.0
        );
        assert_eq!(Row::Entry(&entry("a", "garbage", "", 1)).height(), 46.0);
    }

    #[test]
    fn 보여줄_제목이_없는_항목은_한_줄만큼_낮다() {
        // 제목도 호스트도 없으면 빈 제목 <div>가 줄을 차지하지 않는다: Chrome에서 30px.
        let blank = entry("a", "about:blank", "", 2);
        let empty = entry("b", "", "", 1);
        assert_eq!(Row::Entry(&blank).height(), 30.0);
        assert_eq!(Row::Entry(&empty).height(), 30.0);

        let titled = entry("c", "https://a.test/", "A", 3);
        let rows = [
            Row::Header("Today"),
            Row::Entry(&titled),
            Row::Entry(&blank),
            Row::Entry(&empty),
        ];
        assert_eq!(total_height(&rows), 21.0 + 46.0 + 30.0 + 30.0);
        // 낮은 줄도 제 높이만큼만 자리를 차지한다: 마지막 줄은 97..127.
        let visible = visible_rows(&rows, 97.0 + OVERSCAN, 10.0);
        assert_eq!(visible.range, 3..4);
        assert_eq!(visible.pad_top, 97.0);
        assert_eq!(visible.pad_bottom, 0.0);
    }

    #[test]
    fn 줄이_없으면_고정할_헤더도_없다() {
        assert_eq!(sticky_header(&[], 0.0), None);
    }
}

/// The view in a window: clicks, typing and the Clear prompt.
#[cfg(test)]
mod view_tests {
    use super::*;
    use crate::history::NewHistoryEntry;
    use crate::storage::MemoryStorage;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    fn open<'a>(
        urls: &[&str],
        cx: &'a mut TestAppContext,
    ) -> (
        Entity<HistoryStore>,
        Entity<HistoryPanel>,
        &'a mut VisualTestContext,
    ) {
        cx.update(rever_ui_kit::text_input::bind_keys);
        let mut store = HistoryStore::new(Box::new(MemoryStorage::default()));
        for (index, url) in urls.iter().enumerate() {
            // A minute apart, ending now: all "Today", the last one listed first.
            let age = (urls.len() - index) as u64 * 60_000;
            store.push_at(
                NewHistoryEntry {
                    tab_id: "t1".into(),
                    url: url.to_string(),
                    title: format!("Page {index}"),
                },
                now_ms() - age,
            );
        }
        let store = cx.new(|_| store);
        let (panel, cx) = cx.add_window_view({
            let store = store.clone();
            move |_, cx| HistoryPanel::new(store, Lang::En, ThemeMode::Light, cx)
        });
        (store, panel, cx)
    }

    /// Click where `selector` was last painted. GPUI keeps the bounds of elements that
    /// are gone, so only a click tells whether a row is still there.
    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"));
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn 항목을_누르면_그_url을_열어_달라고_알린다(cx: &mut TestAppContext) {
        let (_, panel, cx) = open(&["https://a.test/", "https://b.test/"], cx);
        let mut opened = cx.events::<OpenUrl, _>(&panel);
        // Row 0 is the "Today" header; rows 1 and 2 are the entries, newest first.
        assert!(cx.debug_bounds("history-row-0").is_none());
        click("history-row-2", cx);
        assert_eq!(
            opened.try_recv().ok(),
            Some(OpenUrl("https://a.test/".into()))
        );
        click("history-row-1", cx);
        assert_eq!(
            opened.try_recv().ok(),
            Some(OpenUrl("https://b.test/".into()))
        );
    }

    #[gpui::test]
    fn 스크롤하면_고정된_헤더가_밑에_깔린_항목의_클릭을_막는다(
        cx: &mut TestAppContext,
    ) {
        let urls: Vec<String> = (0..60).map(|n| format!("https://a.test/{n}")).collect();
        let urls: Vec<&str> = urls.iter().map(String::as_str).collect();
        let (_, panel, cx) = open(&urls, cx);
        let mut opened = cx.events::<OpenUrl, _>(&panel);
        let pinned = cx.debug_bounds("history-pinned").unwrap();
        assert_eq!(pinned.size.height, px(HEADER_HEIGHT));

        // 37px down: the newest entry (row 1) ends 9px below the pinned header, and the
        // point 12px below the header belongs to the second newest (row 2).
        let below = gpui::point(pinned.center().x, pinned.bottom() + px(12.0));
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: below,
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-37.0))),
            ..Default::default()
        });
        cx.run_until_parked();

        cx.simulate_click(pinned.center(), Modifiers::none());
        cx.run_until_parked();
        assert!(opened.try_recv().is_err(), "the header is not a link");

        cx.simulate_click(below, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            opened.try_recv().ok(),
            Some(OpenUrl("https://a.test/58".into()))
        );
    }

    #[gpui::test]
    fn 긴_기록도_화면_근처의_줄만_그린다(cx: &mut TestAppContext) {
        let urls: Vec<String> = (0..1000).map(|n| format!("https://a.test/{n}")).collect();
        let urls: Vec<&str> = urls.iter().map(String::as_str).collect();
        let (_, _, cx) = open(&urls, cx);
        assert!(cx.debug_bounds("history-row-1").is_some());
        // 1000 rows are 46,000px; no window is that tall.
        assert!(cx.debug_bounds("history-row-1000").is_none());
    }

    #[gpui::test]
    fn 줄의_크기는_정해_둔_높이와_같다(cx: &mut TestAppContext) {
        // The windowing and the pinned header are computed from these two heights.
        let (_, _, cx) = open(&["https://a.test/", "https://b.test/"], cx);
        let first = cx.debug_bounds("history-row-1").unwrap();
        let second = cx.debug_bounds("history-row-2").unwrap();
        assert_eq!(first.size.height, px(ROW_HEIGHT));
        assert_eq!(second.top() - first.top(), px(ROW_HEIGHT));
        // Panel padding 10, field 23, margin 10, list border 1, then the header.
        assert_eq!(first.top(), px(10.0 + 23.0 + 10.0 + 1.0 + HEADER_HEIGHT));
    }

    #[gpui::test]
    fn 제목_줄이_없는_항목은_화면에서도_30px다(cx: &mut TestAppContext) {
        let mut store = HistoryStore::new(Box::new(MemoryStorage::default()));
        let now = now_ms();
        for (age, url) in [
            (3, "https://a.test/"),
            (2, "about:blank"),
            (1, "https://b.test/"),
        ] {
            let entry = NewHistoryEntry {
                tab_id: "t1".into(),
                url: url.into(),
                title: String::new(),
            };
            store.push_at(entry, now - age * 1000);
        }
        let store = cx.new(|_| store);
        let (_, cx) = cx
            .add_window_view(move |_, cx| HistoryPanel::new(store, Lang::En, ThemeMode::Light, cx));
        // Newest first: b.test (hostname as title), about:blank (no title line), a.test.
        let first = cx.debug_bounds("history-row-1").unwrap();
        let blank = cx.debug_bounds("history-row-2").unwrap();
        let last = cx.debug_bounds("history-row-3").unwrap();
        assert_eq!(first.size.height, px(ROW_HEIGHT));
        assert_eq!(blank.size.height, px(ROW_HEIGHT_UNTITLED));
        assert_eq!(blank.top() - first.top(), px(ROW_HEIGHT));
        assert_eq!(last.top() - blank.top(), px(ROW_HEIGHT_UNTITLED));
    }

    #[gpui::test]
    fn 검색어를_입력하면_맞는_항목만_남는다(cx: &mut TestAppContext) {
        let (_, panel, cx) = open(&["https://a.test/", "https://b.test/"], cx);
        let mut opened = cx.events::<OpenUrl, _>(&panel);
        click("history-filter", cx);
        cx.simulate_input("A.TEST");
        cx.run_until_parked();
        // The first entry row held b.test; with the search it holds the only match.
        click("history-row-1", cx);
        assert_eq!(
            opened.try_recv().ok(),
            Some(OpenUrl("https://a.test/".into()))
        );

        cx.simulate_input("zzz");
        cx.run_until_parked();
        click("history-row-1", cx);
        assert!(opened.try_recv().is_err(), "no row is left to click");
    }

    #[gpui::test]
    fn clear는_확인을_받은_뒤에만_기록을_비운다(cx: &mut TestAppContext) {
        let (store, panel, cx) = open(&["https://a.test/", "https://b.test/"], cx);
        let mut opened = cx.events::<OpenUrl, _>(&panel);
        click("history-clear", cx);
        assert_eq!(
            cx.pending_prompt().map(|(message, _)| message),
            Some("Clear all history?".to_string())
        );
        assert_eq!(store.read_with(cx, |store, _| store.entries().len()), 2);

        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert_eq!(store.read_with(cx, |store, _| store.entries().len()), 2);

        click("history-clear", cx);
        cx.simulate_prompt_answer("OK");
        cx.run_until_parked();
        assert!(store.read_with(cx, |store, _| store.entries().is_empty()));
        // The list was redrawn without rows: where one was, a click opens nothing.
        click("history-row-1", cx);
        assert!(opened.try_recv().is_err());
    }

    #[gpui::test]
    fn 기록이_없으면_clear는_묻지도_않는다(cx: &mut TestAppContext) {
        let (_, _, cx) = open(&[], cx);
        click("history-clear", cx);
        assert!(!cx.has_pending_prompt());
    }

    #[gpui::test]
    fn 기록이_바뀌면_목록을_다시_그린다(cx: &mut TestAppContext) {
        let (store, _, cx) = open(&["https://a.test/"], cx);
        assert!(cx.debug_bounds("history-row-2").is_none());
        store.update(cx, |store, cx| {
            store.push(NewHistoryEntry {
                tab_id: "t1".into(),
                url: "https://b.test/".into(),
                title: "B".into(),
            });
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("history-row-2").is_some());
    }
}
