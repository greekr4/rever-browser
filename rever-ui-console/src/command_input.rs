//! The console's `<textarea>`: a multi-line text field, since GPUI ships none and
//! `rever_ui_kit::text_input::TextInput` holds a single line.
//!
//! It reuses that field's actions and key bindings (same "TextInput" key context), so a
//! host that called `text_input::bind_keys` needs nothing more. Enter emits
//! [`Submitted`]; Shift+Enter starts a new line; the field grows to [`MAX_ROWS`] rows.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyDownEvent,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, div, fill,
    point, prelude::*, px, relative, rgba, size,
};
use rever_ui_kit::text_input::{self as keys, Submitted};

/// `rows={Math.min(6, input.split('\n').length)}`.
pub const MAX_ROWS: usize = 6;
/// `line-height: normal` of the 11px monospace font.
pub const LINE_HEIGHT: f32 = 13.0;

/// Byte ranges of the hard lines of `text`, without their newline. Never empty.
fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut start = 0;
    text.split('\n')
        .map(|line| {
            let range = start..start + line.len();
            start = range.end + 1;
            range
        })
        .collect()
}

/// The index of the line holding byte `offset`.
fn line_of(lines: &[Range<usize>], offset: usize) -> usize {
    lines
        .iter()
        .position(|line| offset <= line.end)
        .unwrap_or(lines.len().saturating_sub(1))
}

/// How many rows the field shows for `text`.
pub fn rows(text: &str) -> usize {
    text.split('\n').count().min(MAX_ROWS)
}

/// Where Up/Down puts a cursor at `offset`: the same column one line away, or the
/// start/end of the text from the first/last line, as a textarea does.
fn move_vertical(text: &str, offset: usize, up: bool) -> usize {
    let lines = line_ranges(text);
    let current = line_of(&lines, offset);
    let target = if up {
        current.checked_sub(1)
    } else {
        Some(current + 1).filter(|next| *next < lines.len())
    };
    let (Some(from), Some(to)) = (lines.get(current), target.and_then(|t| lines.get(t))) else {
        return if up { 0 } else { text.len() };
    };
    let column = text
        .get(from.start..offset)
        .map_or(0, |before| before.chars().count());
    let line = text.get(to.clone()).unwrap_or_default();
    to.start
        + line
            .char_indices()
            .nth(column)
            .map_or(line.len(), |(byte, _)| byte)
}

/// The first visible line that keeps `cursor_line` inside a window of `rows` lines.
fn top_line_for(top: usize, cursor_line: usize, rows: usize, line_count: usize) -> usize {
    let top = top.min(line_count.saturating_sub(rows));
    if cursor_line < top {
        cursor_line
    } else if cursor_line >= top + rows {
        cursor_line + 1 - rows
    } else {
        top
    }
}

/// Clamp a byte range into `text`, on character boundaries.
fn clamp_range(text: &str, range: Range<usize>) -> Range<usize> {
    let floor = |mut index: usize| {
        index = index.min(text.len());
        while !text.is_char_boundary(index) {
            index -= 1;
        }
        index
    };
    let start = floor(range.start);
    start..floor(range.end).max(start)
}

/// Where the selection lands after the IME replaced a range with `new_text`. The IME's
/// offsets are UTF-16 and relative to `new_text`, which starts at byte `start`.
fn marked_selection(
    start: usize,
    new_text: &str,
    selected_utf16: Option<Range<usize>>,
) -> Range<usize> {
    let byte_offset = |utf16_offset: usize| {
        let mut utf16_count = 0;
        for (byte, ch) in new_text.char_indices() {
            if utf16_count >= utf16_offset {
                return byte;
            }
            utf16_count += ch.len_utf16();
        }
        new_text.len()
    };
    match selected_utf16 {
        Some(selected) => start + byte_offset(selected.start)..start + byte_offset(selected.end),
        None => start + new_text.len()..start + new_text.len(),
    }
}

/// What the last paint put on screen; mouse positions are resolved against it.
struct Painted {
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    top_line: usize,
    scroll_x: Pixels,
    /// The visible lines, starting at `top_line`.
    lines: Vec<ShapedLine>,
}

pub struct CommandInput {
    focus_handle: FocusHandle,
    content: String,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    /// The first visible line once the text has more than [`MAX_ROWS`] lines.
    top_line: usize,
    painted: Option<Painted>,
    is_selecting: bool,
}

impl EventEmitter<Submitted> for CommandInput {}

impl CommandInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: String::new(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            top_line: 0,
            painted: None,
            is_selecting: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    /// Replace the text and put the cursor at the end.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.content = text.into();
        self.selected_range = self.content.len()..self.content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    fn submit(&mut self, _: &keys::Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(Submitted(self.content.clone()));
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        match keystroke.key.as_str() {
            "enter"
                if modifiers.shift
                    && !(modifiers.control || modifiers.alt || modifiers.platform) =>
            {
                self.replace(self.selected_range.clone(), "\n", cx);
                cx.stop_propagation();
            }
            // `e.key === 'Enter' && !e.shiftKey`: Ctrl/Alt/Cmd+Enter run too. Plain Enter
            // normally arrives as the Submit action; this also covers a host without bindings.
            "enter" if !modifiers.shift => {
                cx.emit(Submitted(self.content.clone()));
                cx.stop_propagation();
            }
            "up" | "down" if !modifiers.modified() => {
                let up = keystroke.key == "up";
                self.move_to(move_vertical(&self.content, self.cursor_offset(), up), cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn left(&mut self, _: &keys::Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &keys::Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &keys::SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &keys::SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &keys::SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    /// The line the cursor is on.
    fn cursor_line(&self) -> Range<usize> {
        let lines = line_ranges(&self.content);
        lines
            .get(line_of(&lines, self.cursor_offset()))
            .cloned()
            .unwrap_or(0..self.content.len())
    }

    fn home(&mut self, _: &keys::Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.cursor_line().start, cx);
    }

    fn end(&mut self, _: &keys::End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.cursor_line().end, cx);
    }

    fn backspace(&mut self, _: &keys::Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace(self.selected_range.clone(), "", cx)
    }

    fn delete(&mut self, _: &keys::Delete, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace(self.selected_range.clone(), "", cx)
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &keys::ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &keys::Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            // A textarea normalizes pasted line breaks to "\n".
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            self.replace(self.selected_range.clone(), &text, cx);
        }
    }

    fn selected_text(&self) -> Option<&str> {
        self.content
            .get(self.selected_range.clone())
            .filter(|text| !text.is_empty())
    }

    fn copy(&mut self, _: &keys::Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        }
    }

    fn cut(&mut self, _: &keys::Cut, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
            self.replace(self.selected_range.clone(), "", cx)
        }
    }

    /// Put `new_text` in place of `range` and the cursor after it.
    fn replace(&mut self, range: Range<usize>, new_text: &str, cx: &mut Context<Self>) {
        // A stale range must never reach replace_range: that is a crash.
        let range = clamp_range(&self.content, range);
        self.content.replace_range(range.clone(), new_text);
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        let Some(painted) = self.painted.as_ref() else {
            return 0;
        };
        if self.content.is_empty() || position.y < painted.bounds.top() {
            return 0;
        }
        let row = ((position.y - painted.bounds.top()) / painted.line_height).floor() as usize;
        let lines = line_ranges(&self.content);
        let (Some(range), Some(shaped)) =
            (lines.get(painted.top_line + row), painted.lines.get(row))
        else {
            return self.content.len();
        };
        let x = position.x - painted.bounds.left() + painted.scroll_x;
        range.start + shaped.closest_index_for_x(x).min(range.len())
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    // shortcut: steps by character, not by grapheme cluster, so a multi-codepoint emoji
    // takes several key presses; use unicode-segmentation if that matters for a command line.
    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .char_indices()
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .char_indices()
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.content.len())
    }

    /// The range an IME call means: its own, the composition, or the selection.
    fn ime_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        clamp_range(&self.content, range)
    }
}

impl EntityInputHandler for CommandInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        self.content.get(range).map(str::to_owned)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.ime_range(range_utf16);
        self.replace(range, new_text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.ime_range(range_utf16);
        self.content.replace_range(range.clone(), new_text);
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selected_range = marked_selection(range.start, new_text, new_selected_range_utf16);
        self.selection_reversed = false;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let painted = self.painted.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let lines = line_ranges(&self.content);
        let line_index = line_of(&lines, range.start);
        let line = lines.get(line_index)?;
        let row = line_index.checked_sub(painted.top_line)?;
        let shaped = painted.lines.get(row)?;
        let x = |offset: usize| {
            bounds.left() + shaped.x_for_index(offset.saturating_sub(line.start).min(line.len()))
                - painted.scroll_x
        };
        let top = bounds.top() + painted.line_height * row as f32;
        Some(Bounds::from_corners(
            point(x(range.start), top),
            point(x(range.end), top + painted.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.painted.as_ref()?;
        Some(self.offset_to_utf16(self.index_for_mouse_position(point)))
    }
}

struct LinesElement {
    input: Entity<CommandInput>,
}

struct PrepaintState {
    lines: Vec<ShapedLine>,
    scroll_x: Pixels,
    top_line: usize,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
}

impl IntoElement for LinesElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for LinesElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let rows = rows(&self.input.read(cx).content);
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (window.line_height() * rows as f32).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let run = |len: usize, underlined: bool, dimmed: bool| TextRun {
            len,
            font: style.font(),
            // A dimmed version of the text color, so the placeholder reads on light and dark.
            color: if dimmed {
                style.color.opacity(0.45)
            } else {
                style.color
            },
            background_color: None,
            underline: underlined.then(|| UnderlineStyle {
                color: Some(style.color),
                thickness: px(1.0),
                wavy: false,
            }),
            strikethrough: None,
        };

        if input.content.is_empty() {
            let line = window.text_system().shape_line(
                input.placeholder.clone(),
                font_size,
                &[run(input.placeholder.len(), false, true)],
                None,
            );
            return PrepaintState {
                lines: vec![line],
                scroll_x: px(0.),
                top_line: 0,
                cursor: Some(fill(
                    Bounds::new(bounds.origin, size(px(1.), line_height)),
                    style.color,
                )),
                selection: Vec::new(),
            };
        }

        let ranges = line_ranges(&input.content);
        let cursor = input.cursor_offset();
        let cursor_line = line_of(&ranges, cursor);
        let top_line = top_line_for(input.top_line, cursor_line, MAX_ROWS, ranges.len());
        let selected = input.selected_range.clone();
        let marked = input.marked_range.clone().unwrap_or(0..0);

        let mut lines = Vec::new();
        for range in ranges.iter().skip(top_line).take(MAX_ROWS) {
            let text = input.content.get(range.clone()).unwrap_or_default();
            // The composition (IME) is underlined; it never spans a line break.
            let from = marked.start.clamp(range.start, range.end) - range.start;
            let to = marked.end.clamp(range.start, range.end) - range.start;
            let runs: Vec<TextRun> = [
                run(from, false, false),
                run(to - from, true, false),
                run(text.len() - to, false, false),
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect();
            lines.push(window.text_system().shape_line(
                SharedString::from(text.to_owned()),
                font_size,
                &runs,
                None,
            ));
        }

        let x_in = |row: usize, offset: usize| {
            let (Some(range), Some(shaped)) = (ranges.get(top_line + row), lines.get(row)) else {
                return px(0.);
            };
            shaped.x_for_index(offset.clamp(range.start, range.end) - range.start)
        };
        // A long line slides left so the cursor stays inside the field.
        let cursor_row = cursor_line.saturating_sub(top_line);
        let cursor_x = x_in(cursor_row, cursor);
        let scroll_x = (cursor_x - bounds.size.width + px(2.)).max(px(0.));
        let row_top = |row: usize| bounds.top() + line_height * row as f32;

        let mut selection = Vec::new();
        if !selected.is_empty() {
            for row in 0..lines.len() {
                let Some(range) = ranges.get(top_line + row) else {
                    break;
                };
                if selected.end < range.start || selected.start > range.end {
                    continue;
                }
                let left = x_in(row, selected.start);
                // A selected line break shows as a sliver past the last character.
                let tail = if selected.end > range.end {
                    px(4.)
                } else {
                    px(0.)
                };
                let right = x_in(row, selected.end) + tail;
                selection.push(fill(
                    Bounds::from_corners(
                        point(bounds.left() + left - scroll_x, row_top(row)),
                        point(bounds.left() + right - scroll_x, row_top(row) + line_height),
                    ),
                    rgba(0x3311ff30),
                ));
            }
        }
        let cursor = selected.is_empty().then(|| {
            fill(
                Bounds::new(
                    point(bounds.left() + cursor_x - scroll_x, row_top(cursor_row)),
                    size(px(1.), line_height),
                ),
                style.color,
            )
        });
        PrepaintState {
            lines,
            scroll_x,
            top_line,
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        for quad in prepaint.selection.drain(..) {
            window.paint_quad(quad);
        }
        let line_height = window.line_height();
        let lines = std::mem::take(&mut prepaint.lines);
        for (row, line) in lines.iter().enumerate() {
            let origin = point(
                bounds.left() - prepaint.scroll_x,
                bounds.top() + line_height * row as f32,
            );
            // A line that cannot be painted is skipped, not a reason to take the app down.
            let _ = line.paint(origin, line_height, window, cx);
        }
        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        let painted = Painted {
            bounds,
            line_height,
            top_line: prepaint.top_line,
            scroll_x: prepaint.scroll_x,
            lines,
        };
        self.input.update(cx, |input, _cx| {
            input.top_line = painted.top_line;
            input.painted = Some(painted);
        });
    }
}

impl Render for CommandInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .w_full()
            .overflow_hidden()
            .line_height(px(LINE_HEIGHT))
            .child(LinesElement { input: cx.entity() })
    }
}

impl Focusable for CommandInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 줄_수만큼_커지되_여섯_줄을_넘지_않는다() {
        assert_eq!(rows(""), 1);
        assert_eq!(rows("a"), 1);
        assert_eq!(rows("a\nb"), 2);
        assert_eq!(rows("a\n"), 2);
        assert_eq!(rows("1\n2\n3\n4\n5\n6"), 6);
        assert_eq!(rows("1\n2\n3\n4\n5\n6\n7\n8"), 6);
    }

    #[test]
    fn 줄_범위는_줄바꿈을_빼고_빈_줄도_센다() {
        assert_eq!(line_ranges(""), vec![0..0]);
        assert_eq!(line_ranges("ab\n\ncd"), vec![0..2, 3..3, 4..6]);
        assert_eq!(line_ranges("ab\n"), vec![0..2, 3..3]);
        let lines = line_ranges("ab\n\ncd");
        assert_eq!(line_of(&lines, 0), 0);
        assert_eq!(line_of(&lines, 2), 0);
        assert_eq!(line_of(&lines, 3), 1);
        assert_eq!(line_of(&lines, 6), 2);
        // 범위를 벗어난 위치는 마지막 줄로 본다.
        assert_eq!(line_of(&lines, 99), 2);
    }

    #[test]
    fn 위아래_키는_같은_열의_이웃_줄로_옮긴다() {
        let text = "abcd\nxy\n한글입력";
        // "abcd"의 3열 → 짧은 줄에서는 줄 끝.
        assert_eq!(move_vertical(text, 3, false), 7);
        // "xy"의 1열 → 아래 줄의 1열(한 글자는 3바이트).
        assert_eq!(move_vertical(text, 6, false), 8 + 3);
        assert_eq!(move_vertical(text, 8 + 6, true), 5 + 2);
        assert_eq!(move_vertical(text, 6, true), 1);
    }

    #[test]
    fn 첫_줄에서_위는_맨_앞_마지막_줄에서_아래는_맨_끝이다() {
        assert_eq!(move_vertical("abc", 2, true), 0);
        assert_eq!(move_vertical("abc", 1, false), 3);
        assert_eq!(move_vertical("ab\ncd", 1, true), 0);
        assert_eq!(move_vertical("ab\ncd", 4, false), 5);
        assert_eq!(move_vertical("", 0, true), 0);
        assert_eq!(move_vertical("", 0, false), 0);
    }

    #[test]
    fn 글자_중간이나_범위_밖_위치로도_죽지_않는다() {
        assert_eq!(move_vertical("한\n글", 1, false), 4);
        assert_eq!(move_vertical("ab", 99, true), 0);
        assert_eq!(move_vertical("ab", 99, false), 2);
    }

    #[test]
    fn 커서가_보이도록_첫_줄을_옮긴다() {
        assert_eq!(top_line_for(0, 3, 6, 10), 0);
        assert_eq!(top_line_for(0, 6, 6, 10), 1);
        assert_eq!(top_line_for(4, 2, 6, 10), 2);
        assert_eq!(top_line_for(4, 9, 6, 10), 4);
        // 줄이 지워져 줄 수가 줄면 빈 화면이 남지 않게 당긴다.
        assert_eq!(top_line_for(4, 2, 6, 3), 0);
        assert_eq!(top_line_for(4, 6, 6, 7), 1);
    }

    #[test]
    fn 조합_중인_한글을_지워도_선택_범위가_글자_밖으로_나가지_않는다() {
        assert_eq!(marked_selection(0, "", Some(0..0)), 0..0);
        assert_eq!(marked_selection(0, "하", Some(1..1)), 3..3);
        assert_eq!(marked_selection(6, "한", Some(1..1)), 9..9);
        assert_eq!(marked_selection(6, "한", None), 9..9);
    }

    #[test]
    fn 낡은_범위는_글자_경계_안으로_잘라낸다() {
        assert_eq!(clamp_range("", 0..3), 0..0);
        assert_eq!(clamp_range("한", 3..6), 3..3);
        assert_eq!(clamp_range("한글", 1..4), 0..3);
        assert_eq!(clamp_range("abc", 1..2), 1..2);
    }
}
