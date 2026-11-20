//! Port of `src/renderer/src/components/JsonView.tsx` — body viewer for the
//! traffic drawer and the repeater. JSON gets a collapsible, syntax-coloured tree
//! with a Raw toggle; anything else falls back to the plain text block the panels
//! showed before.
//!
//! The React tree keeps one `open` flag per mounted node; here the flags are a map
//! from a node's path to its flag, and the visible tree is flattened into rows so
//! GPUI's uniform list only builds the rows on screen.

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;
use std::time::Duration;

use gpui::{
    AnyElement, ClickEvent, ClipboardItem, Context, Div, ListAlignment,
    ListHorizontalSizingBehavior, ListState, Rgba, SharedString, Stateful, Window, div, list,
    prelude::*, px, rgba, uniform_list,
};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::ThemeMode;
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

use crate::ui::{MONO, Tokens, clickable};

/// Above this the tree is more scroll than signal and the parse cost shows, so
/// large bodies open in raw mode (the toggle still switches to the tree).
pub const TREE_MAX_CHARS: usize = 1_000_000;
/// Rows rendered per container before the rest is summarised. A single API
/// response can carry thousands of items.
pub const MAX_ROWS: usize = 500;
pub const INDENT: f32 = 14.0;
/// 11px text at the tree's 1.5 line height is 16.5px; GPUI lays rows out on whole
/// pixels, so the half pixel is rounded up rather than lost once per row.
const ROW_HEIGHT: f32 = 17.0;
/// The CSS `maxHeight: 360` bounds the content box; padding and border come on top.
const MAX_CONTENT_HEIGHT: f32 = 360.0;
const PADDING: f32 = 10.0;
/// Not in the TypeScript: GPUI shapes and paints a text block whole, so a body of
/// megabytes would stall every frame. A raw text longer than this is drawn as a
/// virtual list of pieces instead, and only the pieces in view are shaped.
const RAW_BLOCK_MAX_BYTES: usize = 100_000;
const RAW_PIECE_BYTES: usize = 4_000;
/// 11px text at the `<pre>`'s default line height.
const RAW_LINE_HEIGHT: f32 = 13.2;

/// A parsed JSON value as JavaScript sees it: numbers are doubles and an object
/// lists its keys in `Object.entries` order.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

/// A key JavaScript treats as an array index: canonical decimal, below 2^32 - 1.
fn array_index(key: &str) -> Option<u32> {
    let canonical = !key.is_empty()
        && (key == "0" || !key.starts_with('0'))
        && key.bytes().all(|b| b.is_ascii_digit());
    key.parse::<u32>()
        .ok()
        .filter(|index| canonical && *index != u32::MAX)
}

/// JavaScript objects list index-like keys first, in numeric order, then the
/// rest in insertion order.
fn js_key_order(entries: Vec<(String, Json)>) -> Vec<(String, Json)> {
    if !entries.iter().any(|(key, _)| array_index(key).is_some()) {
        return entries;
    }
    let (mut indexed, named): (Vec<_>, Vec<_>) = entries
        .into_iter()
        .partition(|(key, _)| array_index(key).is_some());
    indexed.sort_by_key(|(key, _)| array_index(key));
    indexed.extend(named);
    indexed
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct JsonVisitor;

        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = Json;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a JSON value")
            }

            fn visit_unit<E>(self) -> Result<Json, E> {
                Ok(Json::Null)
            }

            fn visit_bool<E>(self, value: bool) -> Result<Json, E> {
                Ok(Json::Bool(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Json, E> {
                Ok(Json::Number(value as f64))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Json, E> {
                Ok(Json::Number(value as f64))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Json, E> {
                Ok(Json::Number(value))
            }

            fn visit_str<E>(self, value: &str) -> Result<Json, E> {
                Ok(Json::String(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> Result<Json, E> {
                Ok(Json::String(value))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Json::Array(items))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
                let mut entries: Vec<(String, Json)> = Vec::new();
                let mut positions: HashMap<String, usize> = HashMap::new();
                while let Some((key, value)) = map.next_entry::<String, Json>()? {
                    // A repeated key keeps its first place and takes the last value.
                    match positions.get(&key).and_then(|&i| entries.get_mut(i)) {
                        Some(slot) => slot.1 = value,
                        None => {
                            positions.insert(key.clone(), entries.len());
                            entries.push((key, value));
                        }
                    }
                }
                Ok(Json::Object(js_key_order(entries)))
            }
        }

        deserializer.deserialize_any(JsonVisitor)
    }
}

/// Whitespace as `String.prototype.trim` and `parseInt` see it: not the same set
/// as Rust's (which takes U+0085 and leaves U+FEFF).
pub(crate) fn is_js_whitespace(c: char) -> bool {
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
}

pub(crate) fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// `text.length` in JavaScript: UTF-16 code units.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The value to show as a tree, or `None` when the text is not JSON.
pub fn parse_json(text: &str, content_type: Option<&str>) -> Option<Json> {
    let trimmed = js_trim(text);
    let looks_json = (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
        || content_type.is_some_and(|content_type| content_type.contains("json"));
    if !looks_json {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

/// The other shortest digit string for `value`, when `shortest` is the wrong one
/// of two. A double that lies exactly halfway between two shortest candidates
/// (14278465619.9765625 between …562 and …563) goes up in Rust; JavaScript
/// takes the candidate whose last digit is even.
fn tie_to_even(value: f64, shortest: &str) -> Option<String> {
    let mantissa_digits = |text: &str| -> String {
        let mantissa = text.split_once('e').map_or(text, |(mantissa, _)| mantissa);
        mantissa.chars().filter(char::is_ascii_digit).collect()
    };
    let count = shortest.len();
    // One digit more than the shortest: a tie ends in 5 there. Most values stop here.
    let next = format!("{:.*e}", count, value);
    if !mantissa_digits(&next).ends_with('5') {
        return None;
    }
    // A double's decimal expansion has at most 767 significant digits, so this is
    // all of it: a tie has nothing but zeros after that 5.
    let exact = format!("{value:.780e}");
    let exponent: i32 = exact.split_once('e')?.1.parse().ok()?;
    let exact = mantissa_digits(&exact);
    let (lower, rest) = exact.split_at_checked(count)?;
    let tail = rest.strip_prefix('5')?;
    if !tail.bytes().all(|b| b == b'0') || lower == shortest {
        return None;
    }
    let even = lower
        .bytes()
        .last()
        .is_some_and(|digit| (digit - b'0').is_multiple_of(2));
    // The lower candidate must still read back as the same double.
    let reads_back = format!("{lower}e{}", exponent - (count as i32 - 1))
        .parse::<f64>()
        .is_ok_and(|parsed| parsed == value);
    (even && reads_back).then(|| lower.to_owned())
}

/// `String(number)` in JavaScript.
pub fn js_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if value == 0.0 {
        return "0".into();
    }
    // Rust's `{:e}` prints the shortest digits that round-trip, the digits the
    // JavaScript algorithm starts from; only the layout rules differ.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = tie_to_even(value.abs(), &digits).unwrap_or(digits);
    let point = exponent.parse::<i32>().unwrap_or(0) + 1;
    let count = digits.len() as i32;
    let zeros = |n: i32| "0".repeat(n.max(0) as usize);
    let split = |at: i32| {
        digits
            .split_at_checked(at.max(0) as usize)
            .unwrap_or((&digits, ""))
    };
    let body = if count <= point && point <= 21 {
        format!("{digits}{}", zeros(point - count))
    } else if 0 < point && point <= 21 {
        let (whole, fraction) = split(point);
        format!("{whole}.{fraction}")
    } else if -6 < point && point <= 0 {
        format!("0.{}{digits}", zeros(-point))
    } else {
        let exponent = point - 1;
        let sign = if exponent < 0 { '-' } else { '+' };
        let (first, rest) = split(1);
        let dot = if rest.is_empty() { "" } else { "." };
        format!("{first}{dot}{rest}e{sign}{}", exponent.abs())
    };
    if value < 0.0 {
        format!("-{body}")
    } else {
        body
    }
}

/// `JSON.stringify(string)`.
fn quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

/// `JSON.stringify(value, null, 2)`.
pub fn stringify_pretty(value: &Json) -> String {
    fn write(value: &Json, depth: usize, out: &mut String) {
        let indent = |depth: usize, out: &mut String| out.push_str(&"  ".repeat(depth));
        match value {
            Json::Null => out.push_str("null"),
            Json::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Json::Number(value) if value.is_finite() => out.push_str(&js_number(*value)),
            Json::Number(_) => out.push_str("null"),
            Json::String(value) => out.push_str(&quote(value)),
            Json::Array(items) if items.is_empty() => out.push_str("[]"),
            Json::Object(entries) if entries.is_empty() => out.push_str("{}"),
            Json::Array(items) => {
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    indent(depth + 1, out);
                    write(item, depth + 1, out);
                    out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
                }
                indent(depth, out);
                out.push(']');
            }
            Json::Object(entries) => {
                out.push_str("{\n");
                for (i, (key, item)) in entries.iter().enumerate() {
                    indent(depth + 1, out);
                    out.push_str(&quote(key));
                    out.push_str(": ");
                    write(item, depth + 1, out);
                    out.push_str(if i + 1 < entries.len() { ",\n" } else { "\n" });
                }
                indent(depth, out);
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    write(value, 0, &mut out);
    out
}

// ── The tree as rows ────────────────────────────────────────────────────────

/// What precedes a value: its position in an array or its key in an object.
#[derive(Clone, Debug, PartialEq)]
pub enum Label {
    Index(usize),
    Name(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarKind {
    Null,
    String,
    Number,
    Bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RowKind {
    /// A leaf: `label value,`
    Scalar { kind: ScalarKind, text: String },
    /// The first line of an open container: `▾ label {`
    Open { bracket: char },
    /// A closed container on one line: `▸ label { … 3 keys },`
    Closed {
        open: char,
        close: char,
        count: usize,
        is_array: bool,
    },
    /// `… N more`, after a container's first [`MAX_ROWS`] children.
    More(usize),
    /// The last line of an open container: `},`
    Close { bracket: char },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Left padding in pixels.
    pub pad: f32,
    /// The container this row opens or closes; empty for the other rows.
    pub path: Vec<usize>,
    pub label: Option<Label>,
    pub kind: RowKind,
    pub comma: bool,
}

/// Explicit open flags by node path (child positions from the root). A node
/// without one is open when it is less than two levels deep.
pub type OpenFlags = HashMap<Vec<usize>, bool>;

/// Two levels open is enough to see the shape of a response without the tree
/// scrolling off-screen on anything nested.
pub fn is_open(flags: &OpenFlags, path: &[usize]) -> bool {
    flags.get(path).copied().unwrap_or(path.len() < 2)
}

pub fn set_open(flags: &mut OpenFlags, path: &[usize], open: bool) {
    if !open {
        // Closing a node unmounts its subtree in React, so the nodes below come
        // back with their default state the next time it opens.
        flags.retain(|other, _| !(other.len() > path.len() && other.starts_with(path)));
    }
    flags.insert(path.to_vec(), open);
}

fn scalar(value: &Json) -> RowKind {
    let (kind, text) = match value {
        Json::String(value) => (ScalarKind::String, quote(value)),
        Json::Number(value) => (ScalarKind::Number, js_number(*value)),
        Json::Bool(value) => (ScalarKind::Bool, value.to_string()),
        _ => (ScalarKind::Null, "null".to_owned()),
    };
    RowKind::Scalar { kind, text }
}

fn push_node(
    value: &Json,
    path: &mut Vec<usize>,
    label: Option<Label>,
    comma: bool,
    flags: &OpenFlags,
    rows: &mut Vec<Row>,
) {
    let pad = path.len() as f32 * INDENT;
    let (open, close, count, is_array) = match value {
        Json::Array(items) => ('[', ']', items.len(), true),
        Json::Object(entries) => ('{', '}', entries.len(), false),
        _ => {
            rows.push(Row {
                pad: pad + 12.0,
                path: Vec::new(),
                label,
                kind: scalar(value),
                comma,
            });
            return;
        }
    };
    if !is_open(flags, path) {
        rows.push(Row {
            pad,
            path: path.clone(),
            label,
            kind: RowKind::Closed {
                open,
                close,
                count,
                is_array,
            },
            comma,
        });
        return;
    }
    rows.push(Row {
        pad,
        path: path.clone(),
        label,
        kind: RowKind::Open { bracket: open },
        comma: false,
    });
    let mut child = |index: usize, label: Label, value: &Json, rows: &mut Vec<Row>| {
        path.push(index);
        push_node(value, path, Some(label), index + 1 < count, flags, rows);
        path.pop();
    };
    match value {
        Json::Array(items) => {
            for (index, item) in items.iter().take(MAX_ROWS).enumerate() {
                child(index, Label::Index(index), item, rows);
            }
        }
        Json::Object(entries) => {
            for (index, (key, item)) in entries.iter().take(MAX_ROWS).enumerate() {
                child(index, Label::Name(key.clone()), item, rows);
            }
        }
        _ => {}
    }
    if count > MAX_ROWS {
        rows.push(Row {
            pad: pad + INDENT + 12.0,
            path: Vec::new(),
            label: None,
            kind: RowKind::More(count - MAX_ROWS),
            comma: false,
        });
    }
    rows.push(Row {
        pad: pad + 12.0,
        path: path.clone(),
        label: None,
        kind: RowKind::Close { bracket: close },
        comma,
    });
}

/// The lines of the tree that are visible with these open flags, top to bottom.
pub fn flatten(value: &Json, flags: &OpenFlags) -> Vec<Row> {
    let mut rows = Vec::new();
    push_node(value, &mut Vec::new(), None, false, flags, &mut rows);
    rows
}

/// `t(key, { n })`: the kit returns the template, the placeholder is filled here.
fn with_count(lang: Lang, key: &'static str, n: usize) -> String {
    lang.t(key).replace("{n}", &n.to_string())
}

fn closed_summary(lang: Lang, open: char, close: char, count: usize, is_array: bool) -> String {
    let key = if is_array { "json.items" } else { "json.keys" };
    format!("{open} … {} {close}", with_count(lang, key, count))
}

/// A row as one line of text, as it reads on screen.
pub fn row_text(row: &Row, lang: Lang) -> String {
    let label = match &row.label {
        Some(Label::Index(index)) => format!("{index}: "),
        Some(Label::Name(name)) => format!("{}: ", quote(name)),
        None => String::new(),
    };
    let comma = if row.comma { "," } else { "" };
    match &row.kind {
        RowKind::Scalar { text, .. } => format!("{label}{text}{comma}"),
        RowKind::Open { bracket } => format!("▾ {label}{bracket}"),
        RowKind::Closed {
            open,
            close,
            count,
            is_array,
        } => {
            let summary = closed_summary(lang, *open, *close, *count, *is_array);
            format!("▸ {label}{summary}{comma}")
        }
        RowKind::More(n) => with_count(lang, "json.more", *n),
        RowKind::Close { bracket } => format!("{bracket}{comma}"),
    }
}

/// The row that reaches furthest right, by a monospace estimate: the list
/// measures that one row to know how far it scrolls sideways.
fn widest_row(rows: &[Row], lang: Lang) -> usize {
    let width = |row: &Row| {
        let cells: usize = row_text(row, lang)
            .chars()
            .map(|c| if c.is_ascii() { 1 } else { 2 })
            .sum();
        row.pad + cells as f32 * 6.7
    };
    rows.iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| width(a).total_cmp(&width(b)))
        .map_or(0, |(index, _)| index)
}

/// A `<pre>` does not draw the empty line a final newline would start.
fn without_final_newline(text: &str) -> &str {
    text.strip_suffix('\n').unwrap_or(text)
}

/// Where to cut `text` into pieces of at most `limit` bytes, every byte in one
/// piece: after the last newline that fits, or inside the line when it has none.
///
/// shortcut: a line longer than `limit` breaks at each cut where the `<pre>`
/// would run on to the edge; cutting at the wrap width needs the laid-out text.
fn raw_pieces(text: &str, limit: usize) -> Vec<Range<usize>> {
    let mut pieces = Vec::new();
    let mut start = 0;
    while let Some(rest) = text.get(start..).filter(|rest| !rest.is_empty()) {
        let len = if rest.len() <= limit {
            rest.len()
        } else {
            let fits = (0..=limit)
                .rev()
                .find(|&at| rest.is_char_boundary(at))
                .unwrap_or(0);
            let window = rest.get(..fits).unwrap_or_default();
            // Never an empty piece, even under a limit smaller than one character.
            let first = rest.chars().next().map_or(rest.len(), char::len_utf8);
            window.rfind('\n').map_or(fits, |at| at + 1).max(first)
        };
        pieces.push(start..start + len);
        start += len;
    }
    pieces
}

// ── The view ────────────────────────────────────────────────────────────────

pub struct JsonView {
    text: String,
    parsed: Option<Json>,
    /// Tree mode; false shows the text as it came.
    pretty: bool,
    copied: bool,
    flags: OpenFlags,
    rows: Vec<Row>,
    widest: usize,
    /// The text of raw mode when it is short enough for one block.
    raw: SharedString,
    /// The byte ranges of a longer text's pieces; empty when `raw` is drawn.
    raw_pieces: Vec<Range<usize>>,
    raw_list: ListState,
    lang: Lang,
    theme_mode: ThemeMode,
}

impl JsonView {
    pub fn new(
        text: impl Into<String>,
        content_type: Option<&str>,
        lang: Lang,
        theme_mode: ThemeMode,
    ) -> Self {
        let text = text.into();
        let parsed = parse_json(&text, content_type);
        // Bytes never undercount UTF-16 units, so most texts skip the exact count.
        let too_big = text.len() > TREE_MAX_CHARS && utf16_len(&text) > TREE_MAX_CHARS;
        let raw_pieces = if text.len() > RAW_BLOCK_MAX_BYTES {
            raw_pieces(&text, RAW_PIECE_BYTES)
        } else {
            Vec::new()
        };
        let raw = if raw_pieces.is_empty() {
            without_final_newline(&text).to_owned().into()
        } else {
            SharedString::default()
        };
        let mut view = Self {
            raw,
            raw_list: ListState::new(raw_pieces.len(), ListAlignment::Top, px(200.0)),
            raw_pieces,
            text,
            parsed,
            pretty: !too_big,
            copied: false,
            flags: OpenFlags::new(),
            rows: Vec::new(),
            widest: 0,
            lang,
            theme_mode,
        };
        if view.pretty {
            view.rebuild();
        }
        view
    }

    fn rebuild(&mut self) {
        self.rows = self
            .parsed
            .as_ref()
            .map(|value| flatten(value, &self.flags))
            .unwrap_or_default();
        self.widest = widest_row(&self.rows, self.lang);
    }

    fn set_pretty(&mut self, pretty: bool, cx: &mut Context<Self>) {
        self.pretty = pretty;
        if pretty && self.rows.is_empty() {
            self.rebuild();
        }
        cx.notify();
    }

    fn set_node_open(&mut self, path: &[usize], open: bool, cx: &mut Context<Self>) {
        set_open(&mut self.flags, path, open);
        self.rebuild();
        cx.notify();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let text = match &self.parsed {
            Some(value) if self.pretty => stringify_pretty(value),
            _ => self.text.clone(),
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1200))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.copied = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// One piece of a long raw text. The block break after it stands for the
    /// newline it ends with.
    fn raw_piece(&self, index: usize, t: &Tokens) -> AnyElement {
        let piece = self
            .raw_pieces
            .get(index)
            .and_then(|range| self.text.get(range.clone()))
            .unwrap_or_default();
        clickable(("json-raw-piece", index))
            .w_full()
            // A piece that is only a newline is still a line.
            .min_h(px(RAW_LINE_HEIGHT))
            .font_family(MONO)
            .text_size(px(11.0))
            .line_height(px(RAW_LINE_HEIGHT))
            .text_color(t.theme.text)
            .child(without_final_newline(piece).to_owned())
            .into_any_element()
    }

    /// The plain block: the whole text, wrapped, as high as it needs up to the cap.
    fn raw_block(&self, t: &Tokens, cx: &Context<Self>) -> Stateful<Div> {
        let block = clickable("json-raw")
            .w_full()
            .p(px(PADDING))
            .bg(t.theme.bg)
            .border_1()
            .border_color(t.theme.border)
            .rounded(px(4.0));
        if self.raw_pieces.is_empty() {
            return block
                .max_h(px(MAX_CONTENT_HEIGHT + 2.0 * PADDING + 2.0))
                .overflow_y_scroll()
                .font_family(MONO)
                .text_size(px(11.0))
                .line_height(px(RAW_LINE_HEIGHT))
                .child(self.raw.clone());
        }
        // A text this long always fills the cap, so the list takes it whole.
        let t = *t;
        block.child(
            list(
                self.raw_list.clone(),
                cx.processor(move |this, index: usize, _, _| this.raw_piece(index, &t)),
            )
            .w_full()
            .h(px(MAX_CONTENT_HEIGHT)),
        )
    }

    fn toolbar_button(
        &self,
        id: &'static str,
        label: &'static str,
        active: bool,
        t: &Tokens,
    ) -> Stateful<Div> {
        clickable(id)
            .px(px(8.0))
            .py(px(2.0))
            .rounded(px(3.0))
            .border_1()
            .text_size(px(10.0))
            .line_height(px(16.0))
            .cursor_pointer()
            .border_color(if active {
                t.accent_border
            } else {
                t.theme.border
            })
            .bg(if active {
                t.accent_soft
            } else {
                rgba(0x00000000)
            })
            .text_color(if active {
                t.accent_text
            } else {
                t.theme.text_dim
            })
            .child(label)
    }

    fn render_row(&self, index: usize, row: &Row, t: &Tokens, cx: &Context<Self>) -> AnyElement {
        let span = |color: Rgba, text: String| div().flex_none().text_color(color).child(text);
        let punct = t.json_punct;
        let mut line = div()
            .id(("json-row", index))
            .h(px(ROW_HEIGHT))
            .pl(px(row.pad))
            .flex()
            .items_center()
            .whitespace_nowrap()
            .font_family(MONO)
            .text_size(px(11.0))
            .line_height(px(ROW_HEIGHT))
            .text_color(t.theme.text);

        if let RowKind::Open { .. } | RowKind::Closed { .. } = row.kind {
            let open = matches!(row.kind, RowKind::Open { .. });
            let path = row.path.clone();
            line = line.child(
                clickable(("json-caret", index))
                    .flex_none()
                    .cursor_pointer()
                    .text_color(punct)
                    .child(if open { "▾ " } else { "▸ " })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_node_open(&path, !open, cx)
                    })),
            );
        }
        match &row.label {
            Some(Label::Index(index)) => {
                line = line.child(span(punct, format!("{index}: ")));
            }
            Some(Label::Name(name)) => {
                line = line
                    .child(span(t.json_key, quote(name)))
                    .child(span(punct, ": ".to_owned()));
            }
            None => {}
        }
        let comma = |line: Stateful<Div>| {
            line.when(row.comma, |line| line.child(span(punct, ",".to_owned())))
        };
        let line = match &row.kind {
            RowKind::Scalar { kind, text } => {
                let color = match kind {
                    ScalarKind::Null => t.json_null,
                    ScalarKind::String => t.json_string,
                    ScalarKind::Number => t.json_number,
                    ScalarKind::Bool => t.json_bool,
                };
                comma(line.child(span(color, text.clone())))
            }
            RowKind::Open { bracket } => line.child(span(punct, bracket.to_string())),
            RowKind::Closed {
                open,
                close,
                count,
                is_array,
            } => {
                let path = row.path.clone();
                let summary = closed_summary(self.lang, *open, *close, *count, *is_array);
                comma(
                    line.child(
                        clickable(("json-summary", index))
                            .flex_none()
                            .cursor_pointer()
                            .text_color(punct)
                            .child(summary)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_node_open(&path, true, cx)
                            })),
                    ),
                )
            }
            RowKind::More(n) => line.child(span(punct, with_count(self.lang, "json.more", *n))),
            RowKind::Close { bracket } => comma(line.child(span(punct, bracket.to_string()))),
        };
        line.into_any_element()
    }
}

impl Render for JsonView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::resolve(self.theme_mode, window);
        if self.parsed.is_none() {
            return div().child(self.raw_block(&t, cx));
        }
        let lang = self.lang;
        let copy_label = if self.copied {
            lang.t("json.copied")
        } else {
            lang.t("json.copy")
        };
        let toolbar = div()
            .flex()
            .gap(px(4.0))
            .mb(px(4.0))
            .child(
                self.toolbar_button("json-pretty", lang.t("json.pretty"), self.pretty, &t)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_pretty(true, cx))),
            )
            .child(
                self.toolbar_button("json-raw-toggle", lang.t("json.raw"), !self.pretty, &t)
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.set_pretty(false, cx)),
                    ),
            )
            .child(
                self.toolbar_button("json-copy", copy_label, false, &t)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy(cx))),
            );
        if !self.pretty {
            return div().child(toolbar).child(self.raw_block(&t, cx));
        }
        let content = (self.rows.len() as f32 * ROW_HEIGHT).min(MAX_CONTENT_HEIGHT);
        let tree = uniform_list(
            "json-tree",
            self.rows.len(),
            cx.processor(move |this, range: Range<usize>, _, cx| {
                range
                    .filter_map(|index| {
                        let row = this.rows.get(index)?;
                        Some(this.render_row(index, row, &t, cx))
                    })
                    .collect::<Vec<_>>()
            }),
        )
        // Lines do not wrap (`white-space: pre`): the block scrolls sideways instead.
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .with_width_from_item(Some(self.widest))
        .debug_selector(|| "json-tree".into())
        .w_full()
        .h(px(content));
        // The box is a wrapper: the list counts its own padding twice when it
        // places rows.
        let tree = div()
            .w_full()
            .p(px(PADDING))
            .bg(t.theme.bg)
            .border_1()
            .border_color(t.theme.border)
            .rounded(px(4.0))
            .child(tree);
        div().child(toolbar).child(tree)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Json {
        parse_json(text, None).expect("valid JSON")
    }

    fn lines(value: &Json, flags: &OpenFlags) -> Vec<String> {
        flatten(value, flags)
            .iter()
            .map(|row| row_text(row, Lang::En))
            .collect()
    }

    #[test]
    fn 중괄호나_대괄호로_감싼_본문만_json으로_본다() {
        assert_eq!(
            parse_json(r#"{"a":1}"#, None),
            Some(Json::Object(vec![("a".into(), Json::Number(1.0))]))
        );
        assert_eq!(
            parse_json("[true, null]", None),
            Some(Json::Array(vec![Json::Bool(true), Json::Null]))
        );
        assert_eq!(parse_json("42", None), None);
        assert_eq!(parse_json("\"text\"", None), None);
        assert_eq!(parse_json("hello", None), None);
        assert_eq!(parse_json("", None), None);
        // Mismatched ends do not look like JSON.
        assert_eq!(parse_json("{1]", None), None);
    }

    #[test]
    fn content_type에_json이_있으면_스칼라도_해석한다() {
        assert_eq!(
            parse_json("42", Some("application/json")),
            Some(Json::Number(42.0))
        );
        assert_eq!(
            parse_json("null", Some("application/problem+json; charset=utf-8")),
            Some(Json::Null)
        );
        assert_eq!(parse_json("42", Some("text/plain")), None);
        // The header value is matched as written, case included.
        assert_eq!(parse_json("42", Some("application/JSON")), None);
    }

    #[test]
    fn 깨진_json은_트리로_보여주지_않는다() {
        assert_eq!(parse_json("{not json}", None), None);
        assert_eq!(parse_json("{\"a\":1,}", None), None);
        assert_eq!(parse_json("[1] trailing]", None), None);
        assert_eq!(parse_json("oops", Some("application/json")), None);
        assert_eq!(parse_json("", Some("application/json")), None);
    }

    #[test]
    fn 앞뒤_공백은_자바스크립트_trim처럼_떼어낸다() {
        assert!(parse_json(" \n\t{\"a\":1}\r\n ", None).is_some());
        // A byte order mark and a no-break space count as whitespace in JavaScript.
        assert!(parse_json("\u{feff}[1]\u{a0}", None).is_some());
        // U+0085 does not, so the text does not start with a bracket.
        assert_eq!(parse_json("\u{85}[1]", None), None);
        assert_eq!(js_trim("\u{3000} x \u{2028}"), "x");
        assert_eq!(js_trim("\u{85}x"), "\u{85}x");
    }

    #[test]
    fn 객체의_키는_자바스크립트_순서를_따른다() {
        let value = parse(r#"{"b":1,"10":2,"a":3,"2":4,"01":5}"#);
        let Json::Object(entries) = value else {
            panic!("expected an object");
        };
        let keys: Vec<_> = entries.iter().map(|(key, _)| key.as_str()).collect();
        // Index-like keys first in numeric order; "01" is not canonical, so it is a name.
        assert_eq!(keys, ["2", "10", "b", "a", "01"]);
    }

    #[test]
    fn 겹치는_키는_첫_자리에_마지막_값을_둔다() {
        assert_eq!(
            parse(r#"{"a":1,"b":2,"a":3}"#),
            Json::Object(vec![
                ("a".into(), Json::Number(3.0)),
                ("b".into(), Json::Number(2.0))
            ])
        );
    }

    #[test]
    fn 숫자는_자바스크립트처럼_문자열로_바꾼다() {
        assert_eq!(js_number(1.0), "1");
        assert_eq!(js_number(-0.0), "0");
        assert_eq!(js_number(1.5), "1.5");
        assert_eq!(js_number(-42.25), "-42.25");
        assert_eq!(js_number(100.0), "100");
        assert_eq!(js_number(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(js_number(0.000001), "0.000001");
        assert_eq!(js_number(1e-7), "1e-7");
        assert_eq!(js_number(1.5e-7), "1.5e-7");
        assert_eq!(js_number(1e21), "1e+21");
        assert_eq!(js_number(1.5e300), "1.5e+300");
        assert_eq!(js_number(123456789012345680000.0), "123456789012345680000");
        assert_eq!(js_number(f64::NAN), "NaN");
        assert_eq!(js_number(f64::INFINITY), "Infinity");
        assert_eq!(js_number(f64::NEG_INFINITY), "-Infinity");
    }

    #[test]
    fn 두_후보의_정확히_가운데인_수는_끝자리가_짝수인_쪽으로_적는다() {
        // Each value is exactly …5 one digit past its shortest form. Rust's own
        // formatting rounds all of them up.
        assert_eq!(js_number(14_278_465_619.976_563), "14278465619.976562");
        assert_eq!(js_number(8_853_955_072.007_813), "8853955072.007812");
        assert_eq!(js_number(-9_914_647_680.039_063), "-9914647680.039062");
        // Up is the even side here.
        assert_eq!(js_number(8_853_955_072.023_438), "8853955072.023438");
        assert_eq!(js_number(8_608_789_888.054_688), "8608789888.054688");
        // Short exact values are not ties: all their digits are needed.
        assert_eq!(js_number(0.0078125), "0.0078125");
        assert_eq!(js_number(2.5), "2.5");
        assert_eq!(
            lines(&parse("[14278465619.976562]"), &OpenFlags::new())[1],
            "0: 14278465619.976562"
        );
    }

    #[test]
    fn 큰_정수는_배정밀도로_읽는다() {
        // JSON.parse('12345678901234567890') prints as 12345678901234567000.
        assert_eq!(
            lines(
                &parse("[12345678901234567890, 9007199254740993, -5]"),
                &OpenFlags::new()
            ),
            [
                "▾ [",
                "0: 12345678901234567000,",
                "1: 9007199254740992,",
                "2: -5",
                "]"
            ]
        );
    }

    #[test]
    fn 두_단계까지는_펼치고_그_아래는_접는다() {
        let value = parse(r#"{"user":{"name":"kim","tags":["a","b"],"meta":{}},"ok":true}"#);
        assert_eq!(
            lines(&value, &OpenFlags::new()),
            [
                "▾ {",
                "▾ \"user\": {",
                "\"name\": \"kim\",",
                "▸ \"tags\": [ … 2 items ],",
                "▸ \"meta\": { … 0 keys }",
                "},",
                "\"ok\": true",
                "}",
            ]
        );
    }

    #[test]
    fn 줄마다_깊이에_맞는_들여쓰기를_준다() {
        let value = parse(r#"{"a":{"b":{"c":1}},"n":null}"#);
        let rows = flatten(&value, &OpenFlags::new());
        let pads: Vec<f32> = rows.iter().map(|row| row.pad).collect();
        // Container lines start at depth × 14; leaves and closing lines sit 12 further,
        // where the caret of a container line ends.
        assert_eq!(pads, [0.0, 14.0, 28.0, 26.0, 26.0, 12.0]);
        assert_eq!(rows[2].path, [0, 0]);
        assert_eq!(
            rows[2].kind,
            RowKind::Closed {
                open: '{',
                close: '}',
                count: 1,
                is_array: false
            }
        );
        assert_eq!(
            rows[4].kind,
            RowKind::Scalar {
                kind: ScalarKind::Null,
                text: "null".into()
            }
        );
    }

    #[test]
    fn 스칼라는_종류와_표시_문자열을_가진다() {
        let rows = flatten(&parse(r#"["a\"\n", 1.50, false, null]"#), &OpenFlags::new());
        let kinds: Vec<_> = rows
            .iter()
            .filter_map(|row| match &row.kind {
                RowKind::Scalar { kind, text } => Some((*kind, text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            kinds,
            [
                (ScalarKind::String, r#""a\"\n""#),
                (ScalarKind::Number, "1.5"),
                (ScalarKind::Bool, "false"),
                (ScalarKind::Null, "null"),
            ]
        );
        assert_eq!(rows[1].label, Some(Label::Index(0)));
    }

    #[test]
    fn 루트가_스칼라면_한_줄이다() {
        let value = parse_json("\"hi\"", Some("application/json")).unwrap();
        let rows = flatten(&value, &OpenFlags::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pad, 12.0);
        assert_eq!(row_text(&rows[0], Lang::En), "\"hi\"");
    }

    #[test]
    fn 빈_컨테이너도_펼치면_여는_줄과_닫는_줄이_있다() {
        assert_eq!(lines(&parse("[]"), &OpenFlags::new()), ["▾ [", "]"]);
        assert_eq!(lines(&parse("{}"), &OpenFlags::new()), ["▾ {", "}"]);
    }

    #[test]
    fn 접힌_줄을_펼치고_다시_접을_수_있다() {
        let value = parse(r#"{"a":{"list":[1,2]}}"#);
        let mut flags = OpenFlags::new();
        assert!(!is_open(&flags, &[0, 0]));
        set_open(&mut flags, &[0, 0], true);
        assert_eq!(
            lines(&value, &flags),
            [
                "▾ {",
                "▾ \"a\": {",
                "▾ \"list\": [",
                "0: 1,",
                "1: 2",
                "]",
                "}",
                "}"
            ]
        );
        set_open(&mut flags, &[], false);
        assert_eq!(lines(&value, &flags), ["▸ { … 1 keys }"]);
    }

    #[test]
    fn 접으면_그_아래_노드의_상태는_기본값으로_돌아간다() {
        let value = parse(r#"{"a":{"list":[1,2]},"b":[3]}"#);
        let mut flags = OpenFlags::new();
        set_open(&mut flags, &[0, 0], true);
        set_open(&mut flags, &[1], false);
        // Closing "a" forgets that its list was opened; "b" is a sibling and keeps its flag.
        set_open(&mut flags, &[0], false);
        set_open(&mut flags, &[0], true);
        assert_eq!(
            lines(&value, &flags),
            [
                "▾ {",
                "▾ \"a\": {",
                "▸ \"list\": [ … 2 items ]",
                "},",
                "▸ \"b\": [ … 1 items ]",
                "}",
            ]
        );
    }

    #[test]
    fn 컨테이너마다_500줄까지만_그리고_나머지는_요약한다() {
        let items: Vec<String> = (0..MAX_ROWS + 3).map(|i| i.to_string()).collect();
        let value = parse(&format!("[{}]", items.join(",")));
        let rows = flatten(&value, &OpenFlags::new());
        // Opening line, 500 items, the summary, closing line.
        assert_eq!(rows.len(), MAX_ROWS + 3);
        assert_eq!(row_text(&rows[MAX_ROWS], Lang::En), "499: 499,");
        assert_eq!(rows[MAX_ROWS + 1].kind, RowKind::More(3));
        assert_eq!(rows[MAX_ROWS + 1].pad, INDENT + 12.0);
        assert_eq!(row_text(&rows[MAX_ROWS + 1], Lang::En), "… 3 more");
        assert_eq!(row_text(&rows[MAX_ROWS + 2], Lang::En), "]");

        // Exactly at the limit there is nothing to summarise.
        let items: Vec<String> = (0..MAX_ROWS).map(|i| i.to_string()).collect();
        let value = parse(&format!("[{}]", items.join(",")));
        let rows = flatten(&value, &OpenFlags::new());
        assert_eq!(rows.len(), MAX_ROWS + 2);
        assert_eq!(row_text(&rows[MAX_ROWS], Lang::En), "499: 499");
    }

    #[test]
    fn 요약_문구는_언어를_따른다() {
        let value = parse(r#"{"a":{"b":[1,2,3]}}"#);
        let rows = flatten(&value, &OpenFlags::new());
        assert_eq!(row_text(&rows[2], Lang::En), "▸ \"b\": [ … 3 items ]");
        assert_eq!(row_text(&rows[2], Lang::Ko), "▸ \"b\": [ … 항목 3개 ]");
        let mut flags = OpenFlags::new();
        set_open(&mut flags, &[0], false);
        let rows = flatten(&value, &flags);
        assert_eq!(row_text(&rows[1], Lang::Ko), "▸ \"a\": { … 키 1개 }");
    }

    #[test]
    fn 복사용_문자열은_두_칸_들여쓰기로_찍는다() {
        let value = parse(r#"{"b":[1,{"c":null}],"e":[],"o":{},"s":"a\"b","n":1.0}"#);
        assert_eq!(
            stringify_pretty(&value),
            "{\n  \"b\": [\n    1,\n    {\n      \"c\": null\n    }\n  ],\n  \"e\": [],\n  \"o\": {},\n  \"s\": \"a\\\"b\",\n  \"n\": 1\n}"
        );
        assert_eq!(stringify_pretty(&Json::Number(f64::NAN)), "null");
        assert_eq!(
            stringify_pretty(&Json::String("\u{1}한".into())),
            "\"\\u0001한\""
        );
    }

    #[test]
    fn 백만_글자를_넘는_본문은_원본_모드로_연다() {
        let big = format!("[\"{}\"]", "a".repeat(TREE_MAX_CHARS));
        let view = JsonView::new(big, None, Lang::En, ThemeMode::Light);
        assert!(view.parsed.is_some());
        assert!(!view.pretty);
        assert!(view.rows.is_empty());

        // The limit counts UTF-16 units: 400,000 Hangul syllables are 1.2MB of UTF-8
        // but well under a million units.
        let hangul = format!("[\"{}\"]", "가".repeat(400_000));
        let view = JsonView::new(hangul, None, Lang::En, ThemeMode::Light);
        assert!(view.pretty);
        assert_eq!(view.rows.len(), 3);

        // Exactly a million units is still a tree.
        let exact = format!("[\"{}\"]", "a".repeat(TREE_MAX_CHARS - 4));
        let view = JsonView::new(exact, None, Lang::En, ThemeMode::Light);
        assert!(view.pretty);
        assert_eq!(view.rows.len(), 3);
    }

    #[test]
    fn json이_아니면_트리_없이_원문만_가진다() {
        let view = JsonView::new(
            "<html></html>",
            Some("text/html"),
            Lang::En,
            ThemeMode::Light,
        );
        assert!(view.parsed.is_none());
        assert!(view.rows.is_empty());
        assert_eq!(view.raw.as_ref(), "<html></html>");
        assert!(view.raw_pieces.is_empty());
    }

    #[test]
    fn 원본의_마지막_줄바꿈_하나는_그리지_않는다() {
        assert_eq!(without_final_newline("short"), "short");
        // One final newline is not drawn; any other is.
        assert_eq!(without_final_newline("a\nb\n"), "a\nb");
        assert_eq!(without_final_newline("a\n\n"), "a\n");
        let view = JsonView::new("a\nb\n", None, Lang::En, ThemeMode::Light);
        assert_eq!(view.raw.as_ref(), "a\nb");
    }

    fn pieces(text: &str, limit: usize) -> Vec<&str> {
        raw_pieces(text, limit)
            .into_iter()
            .map(|range| &text[range])
            .collect()
    }

    #[test]
    fn 긴_원본은_줄바꿈에서_조각으로_나눈다() {
        assert_eq!(pieces("ab\ncd\nefgh\ni", 6), ["ab\ncd\n", "efgh\ni"]);
        assert_eq!(
            pieces("ab\ncd\nefgh\ni", 5),
            ["ab\n", "cd\n", "efgh\n", "i"]
        );
        // A line that does not fit is cut inside; a newline alone is still a piece.
        assert_eq!(pieces("\nabcdefgh\nij", 4), ["\n", "abcd", "efgh", "\nij"]);
        assert_eq!(pieces("abc", 3), ["abc"]);
        assert_eq!(pieces("abc", 100), ["abc"]);
        assert!(pieces("", 4).is_empty());
    }

    #[test]
    fn 조각은_글자_중간을_자르지_않고_본문을_빠짐없이_덮는다() {
        // Three bytes each: a limit of four holds one syllable.
        assert_eq!(pieces("가나다", 4), ["가", "나", "다"]);
        assert_eq!(pieces("a가나", 4), ["a가", "나"]);
        // A limit below one character still moves forward.
        assert_eq!(pieces("가나", 1), ["가", "나"]);
        assert_eq!(pieces("😀b", 0), ["😀", "b"]);

        let text = "한 줄 line\n".repeat(700) + &"긴".repeat(5_000) + "\n끝";
        let ranges = raw_pieces(&text, RAW_PIECE_BYTES);
        let mut next = 0;
        for range in &ranges {
            assert_eq!(range.start, next);
            assert!(range.len() <= RAW_PIECE_BYTES && !range.is_empty());
            assert!(text.get(range.clone()).is_some());
            next = range.end;
        }
        assert_eq!(next, text.len());
    }

    #[test]
    fn 십만_바이트를_넘는_원본만_조각으로_그린다() {
        let short = JsonView::new(
            "x".repeat(RAW_BLOCK_MAX_BYTES),
            None,
            Lang::En,
            ThemeMode::Light,
        );
        assert!(short.raw_pieces.is_empty());
        assert_eq!(short.raw.len(), RAW_BLOCK_MAX_BYTES);

        let long = JsonView::new(
            "x".repeat(RAW_BLOCK_MAX_BYTES + 1),
            None,
            Lang::En,
            ThemeMode::Light,
        );
        assert_eq!(long.raw_pieces.len(), 26);
        assert_eq!(long.raw_list.item_count(), 26);
        assert!(long.raw.is_empty());
    }

    #[test]
    fn 가장_긴_줄을_가로_폭의_기준으로_고른다() {
        let value = parse(r#"{"a":1,"long":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx","b":2}"#);
        let rows = flatten(&value, &OpenFlags::new());
        assert_eq!(widest_row(&rows, Lang::En), 2);
        assert_eq!(widest_row(&[], Lang::En), 0);
    }

    // ── The view, in a test window ──────────────────────────────────────────

    use gpui::{
        Entity, ListOffset, Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext,
        VisualTestContext, point,
    };

    fn open<'a>(
        text: &str,
        cx: &'a mut TestAppContext,
    ) -> (Entity<JsonView>, &'a mut VisualTestContext) {
        let text = text.to_owned();
        cx.add_window_view(move |_, _| JsonView::new(text, None, Lang::En, ThemeMode::Light))
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let at = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"))
            .center();
        cx.simulate_click(at, Modifiers::none());
    }

    fn shown(view: &Entity<JsonView>, cx: &mut VisualTestContext) -> Vec<String> {
        view.read_with(cx, |view, _| {
            view.rows
                .iter()
                .map(|row| row_text(row, Lang::En))
                .collect()
        })
    }

    #[gpui::test]
    fn 캐럿을_누르면_접히고_요약을_누르면_펼쳐진다(cx: &mut TestAppContext) {
        let (view, cx) = open(r#"{"a":{"b":[1,2]},"c":1}"#, cx);
        assert_eq!(
            shown(&view, cx),
            [
                "▾ {",
                "▾ \"a\": {",
                "▸ \"b\": [ … 2 items ]",
                "},",
                "\"c\": 1",
                "}"
            ]
        );
        click("json-summary-2", cx);
        assert_eq!(
            shown(&view, cx),
            [
                "▾ {",
                "▾ \"a\": {",
                "▾ \"b\": [",
                "0: 1,",
                "1: 2",
                "]",
                "},",
                "\"c\": 1",
                "}"
            ]
        );
        click("json-caret-1", cx);
        assert_eq!(
            shown(&view, cx),
            ["▾ {", "▸ \"a\": { … 1 keys },", "\"c\": 1", "}"]
        );
        // Opening it again by the caret: the list inside is back to closed.
        click("json-caret-1", cx);
        assert_eq!(
            shown(&view, cx),
            [
                "▾ {",
                "▾ \"a\": {",
                "▸ \"b\": [ … 2 items ]",
                "},",
                "\"c\": 1",
                "}"
            ]
        );
        click("json-caret-0", cx);
        assert_eq!(shown(&view, cx), ["▸ { … 2 keys }"]);
    }

    #[gpui::test]
    fn 원본과_보기_좋게_사이를_오간다(cx: &mut TestAppContext) {
        let (view, cx) = open(r#"{"a":1}"#, cx);
        assert!(cx.debug_bounds("json-raw").is_none());
        click("json-raw-toggle", cx);
        assert!(!view.read_with(cx, |view, _| view.pretty));
        assert!(cx.debug_bounds("json-raw").is_some());
        click("json-pretty", cx);
        assert!(view.read_with(cx, |view, _| view.pretty));
        // The tree is live again: its caret still folds the root.
        click("json-caret-0", cx);
        assert_eq!(shown(&view, cx), ["▸ { … 1 keys }"]);
    }

    #[gpui::test]
    fn 복사는_보이는_모드의_글을_클립보드에_넣는다(cx: &mut TestAppContext) {
        let (view, cx) = open(" {\"a\":[1,2]}\n", cx);
        click("json-copy", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("{\n  \"a\": [\n    1,\n    2\n  ]\n}".to_owned())
        );
        assert!(view.read_with(cx, |view, _| view.copied));
        // "Copied" goes back to "Copy" after 1.2 seconds.
        cx.executor().advance_clock(Duration::from_millis(1199));
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.copied));
        cx.executor().advance_clock(Duration::from_millis(2));
        cx.run_until_parked();
        assert!(!view.read_with(cx, |view, _| view.copied));

        click("json-raw-toggle", cx);
        click("json-copy", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(" {\"a\":[1,2]}\n".to_owned())
        );
    }

    #[gpui::test]
    fn json이_아니면_버튼_없이_원문만_보인다(cx: &mut TestAppContext) {
        let (view, cx) = open("plain text, not JSON", cx);
        assert!(view.read_with(cx, |view, _| view.parsed.is_none()));
        assert!(cx.debug_bounds("json-raw").is_some());
        assert!(cx.debug_bounds("json-copy").is_none());
        assert!(cx.debug_bounds("json-pretty").is_none());
    }

    #[gpui::test]
    fn 긴_원본은_보이는_조각만_만들고_끝까지_넘길_수_있다(
        cx: &mut TestAppContext,
    ) {
        let lines: Vec<String> = (0..40_000).map(|i| format!("line {i}")).collect();
        let (view, cx) = open(&lines.join("\n"), cx);
        let count = view.read_with(cx, |view, _| view.raw_pieces.len());
        assert!(count > 50);
        // The box is the 360px cap plus its padding and border.
        let raw = cx.debug_bounds("json-raw").unwrap();
        assert_eq!(
            raw.size.height,
            px(MAX_CONTENT_HEIGHT + 2.0 * PADDING + 2.0)
        );
        assert!(cx.debug_bounds("json-raw-piece-0").is_some());
        let last: &'static str = format!("json-raw-piece-{}", count - 1).leak();
        assert!(cx.debug_bounds(last).is_none());

        // The wheel moves through the text a stretch at a time, past the first piece.
        // (A selector's bounds outlive its element in a test window, so what is in
        // view is read from the list.)
        for _ in 0..4 {
            cx.simulate_event(ScrollWheelEvent {
                position: raw.center(),
                delta: ScrollDelta::Pixels(point(px(0.0), px(-4000.0))),
                ..ScrollWheelEvent::default()
            });
        }
        let top = view.read_with(cx, |view, _| view.raw_list.logical_scroll_top());
        assert!(top.item_ix >= 1, "still at piece {}", top.item_ix);

        // Nothing is left out: the last piece can be brought into view.
        view.update(cx, |view, cx| {
            view.raw_list.scroll_to(ListOffset {
                item_ix: count - 1,
                offset_in_item: px(0.0),
            });
            cx.notify();
        });
        cx.run_until_parked();
        let end = cx.debug_bounds(last).expect("the last piece is drawn");
        assert!(end.top() < raw.bottom() && end.bottom() > raw.top());
    }

    #[gpui::test]
    fn 줄이_많아도_화면에_보이는_줄만_만든다(cx: &mut TestAppContext) {
        let items: Vec<String> = (0..400).map(|i| i.to_string()).collect();
        let (view, cx) = open(&format!("[{}]", items.join(",")), cx);
        assert_eq!(view.read_with(cx, |view, _| view.rows.len()), 402);
        // The box stops at its 360px cap; rows past it are not built.
        assert!(cx.debug_bounds("json-caret-0").is_some());
        let tree = cx.debug_bounds("json-tree").unwrap();
        assert_eq!(tree.size.height, px(MAX_CONTENT_HEIGHT));
    }
}
