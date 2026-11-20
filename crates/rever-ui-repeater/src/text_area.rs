//! A multi-line text field: the request body of the HTTP repeater and the frame editor of
//! the WebSocket repeater, both a `<textarea>` in the Electron app.
//!
//! The kit's `TextInput` holds one line (GPUI's line shaper rejects newlines). Adapted
//! from `rever_ui_kit::text_input` (itself from GPUI's `examples/input.rs`, Zed
//! Industries, Apache-2.0). It reuses that module's actions and key bindings, so the host
//! needs no extra `bind_keys` call. As in a textarea, Enter breaks the line, long lines
//! wrap at the field's width, and text taller than the field scrolls inside it. Cmd+Enter
//! and Ctrl+Enter emit the kit's `Submitted`; the parent decides what that means.
//!
//! [`TextArea::single_line`] is the same field as an `<input>`: one line that scrolls
//! sideways with the cursor. The kit's field does not scroll, and a repeater's URLs and
//! header values are routinely longer than their boxes.
//!
//! shortcut: Up and Down keep the column counted in characters, which matches the x
//! position only in a monospace font without wide glyphs; use the shaped lines if the
//! field gets another font.
//! shortcut: Home and End go to the ends of the line as typed, not of the wrapped row.
//! shortcut: the whole text is shaped on every paint; shape only the rows in view if
//! bodies of megabytes are edited here.

use std::borrow::Cow;
use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId,
    KeyDownEvent, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point, ScrollWheelEvent, SharedString, Style, TextAlign, TextRun, UTF16Selection,
    UnderlineStyle, Window, WrappedLine, div, fill, point, prelude::*, px, relative, rgba, size,
};
use rever_ui_kit::text_input as kit;
use rever_ui_kit::text_input::Submitted;
use unicode_segmentation::UnicodeSegmentation;

impl EventEmitter<Submitted> for TextArea {}

pub struct TextArea {
    focus_handle: FocusHandle,
    content: String,
    /// What `set_text` was given, while it differs from `content` and nothing was edited
    /// since. A `<textarea>` shows CRLF and CR as LF, but React keeps the string it was
    /// given until the first edit: an untouched body is sent again byte for byte.
    unedited: Option<String>,
    placeholder: SharedString,
    /// An `<input>` instead of a `<textarea>`: no line breaks, no wrapping.
    single_line: bool,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    /// Counts the changes to `content`, so a layout can tell whether it is of this text.
    edits: u64,
    /// The text as painted last, one entry per typed line, for mouse and IME hit testing.
    last_lines: Vec<WrappedLine>,
    /// The wrapped rows of that paint, and the `edits` they belong to.
    last_rows: Vec<Range<usize>>,
    last_rows_edits: u64,
    last_bounds: Option<Bounds<Pixels>>,
    last_line_height: Pixels,
    content_height: Pixels,
    /// How far the text is scrolled up inside the field, and a single line to the left.
    scroll_y: Pixels,
    scroll_x: Pixels,
    /// The next paint scrolls the cursor into view: set by typing and cursor keys, not
    /// by the wheel.
    reveal_cursor: bool,
    is_selecting: bool,
}

impl TextArea {
    /// A field that fills its parent, which must be a flex column with a height.
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: String::new(),
            unedited: None,
            placeholder: placeholder.into(),
            single_line: false,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            edits: 0,
            last_lines: Vec::new(),
            last_rows: Vec::new(),
            last_rows_edits: 0,
            last_bounds: None,
            last_line_height: px(0.),
            content_height: px(0.),
            scroll_y: px(0.),
            scroll_x: px(0.),
            reveal_cursor: false,
            is_selecting: false,
        }
    }

    /// An `<input>`: one line as high as the text, as wide as its parent.
    pub fn single_line(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            single_line: true,
            ..Self::new(placeholder, cx)
        }
    }

    /// The value as the component's state holds it, not what the field displays.
    pub fn text(&self) -> &str {
        self.unedited.as_deref().unwrap_or(&self.content)
    }

    /// Replace the text and put the cursor at the end.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        let text = text.into();
        self.content = self.displayed(&text).into_owned();
        self.unedited = (self.content != text).then_some(text);
        self.edits += 1;
        self.selected_range = self.content.len()..self.content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        // A value set from outside does not scroll the field: a long URL or header
        // value still shows its beginning, as an `<input>` given a `value` does.
        self.reveal_cursor = false;
        cx.notify();
    }

    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let placeholder = placeholder.into();
        if self.placeholder != placeholder {
            self.placeholder = placeholder;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus_handle);
    }

    /// What the field shows of a value it is given: a textarea reads CRLF and CR as LF,
    /// an input strips line breaks. The line shaper also refuses a line break in a line.
    fn displayed<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if !text.contains(['\n', '\r']) {
            Cow::Borrowed(text)
        } else if self.single_line {
            Cow::Owned(text.replace(['\n', '\r'], ""))
        } else {
            Cow::Owned(normalize_newlines(text))
        }
    }

    /// Bare Enter, which the kit binds to `Submit` for its one-line field.
    fn newline(&mut self, _: &kit::Submit, window: &mut Window, cx: &mut Context<Self>) {
        // An Enter that arrives mid-composition is the IME's; in an input it does nothing.
        if self.marked_range.is_some() || self.single_line {
            return;
        }
        self.replace_text_in_range(None, "\n", window, cx);
    }

    fn left(&mut self, _: &kit::Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &kit::Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &kit::SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &kit::SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &kit::SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &kit::Home, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.cursor_line_range();
        self.move_to(line.start, cx);
    }

    fn end(&mut self, _: &kit::End, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.cursor_line_range();
        self.move_to(line.end, cx);
    }

    fn backspace(&mut self, _: &kit::Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &kit::Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    /// The keys the kit binds nothing to: Enter with a modifier, Up and Down.
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if keystroke.key == "enter" {
            if self.single_line {
                return;
            }
            // `e.key === 'Enter' && (e.metaKey || e.ctrlKey)` is the parent's shortcut;
            // with Shift or Alt alone a textarea breaks the line as bare Enter does.
            if modifiers.platform || modifiers.control {
                cx.emit(Submitted(self.text().to_owned()));
            } else {
                self.newline(&kit::Submit, window, cx);
            }
            cx.stop_propagation();
            return;
        }
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        let delta = match keystroke.key.as_str() {
            "up" => -1,
            "down" => 1,
            _ => return,
        };
        let target = vertical_target(&self.content, &self.rows(), self.cursor_offset(), delta);
        if modifiers.shift {
            self.select_to(target, cx);
        } else {
            self.move_to(target, cx);
        }
        cx.stop_propagation();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_position(event.position), cx);
        } else {
            self.move_to(self.index_for_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_position(event.position), cx);
        }
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewport = self.last_bounds.map_or(px(0.), |bounds| bounds.size.height);
        let delta = event.delta.pixel_delta(self.last_line_height).y;
        let next = scroll_for(
            f32::from(self.scroll_y - delta),
            None,
            f32::from(self.last_line_height),
            f32::from(viewport),
            f32::from(self.content_height),
        );
        // At either end the wheel goes on to the pane around the field, as a textarea
        // chains its scroll.
        if px(next) != self.scroll_y {
            self.scroll_y = px(next);
            cx.notify();
            cx.stop_propagation();
        }
    }

    fn show_character_palette(
        &mut self,
        _: &kit::ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &kit::Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            // Line breaks stay in a textarea, which turns CRLF and CR into LF; pasted into
            // an input they are spaces, as in the kit's field.
            let text = normalize_newlines(&text);
            let text = if self.single_line {
                text.replace('\n', " ")
            } else {
                text
            };
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn selected_text(&self) -> Option<String> {
        let range = clamp_range(&self.content, self.selected_range.clone());
        (!range.is_empty()).then(|| self.content[range].to_owned())
    }

    fn copy(&mut self, _: &kit::Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn cut(&mut self, _: &kit::Cut, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.reveal_cursor = true;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn cursor_line_range(&self) -> Range<usize> {
        let ranges = line_ranges(&self.content);
        let line = line_at(&ranges, self.cursor_offset());
        ranges.get(line).cloned().unwrap_or(0..0)
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
        self.reveal_cursor = true;
        cx.notify()
    }

    /// Whether the lines painted last are of the current text.
    fn layout_is_current(&self) -> bool {
        self.last_rows_edits == self.edits && !self.last_lines.is_empty()
    }

    /// The rows Up and Down move between: the wrapped rows as painted, or the typed
    /// lines while the text has not been painted since it changed.
    fn rows(&self) -> Vec<Range<usize>> {
        if self.layout_is_current() {
            self.last_rows.clone()
        } else {
            line_ranges(&self.content)
        }
    }

    /// The text offset under a window position, from the lines painted last.
    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds else {
            return 0;
        };
        if self.content.is_empty() || self.last_line_height <= px(0.) {
            return 0;
        }
        if !self.layout_is_current() {
            return self.cursor_offset();
        }
        let mut y = position.y - bounds.top() + self.scroll_y;
        if y < px(0.) {
            return 0;
        }
        let x = (position.x - bounds.left() + self.scroll_x).max(px(0.));
        for (range, shaped) in line_ranges(&self.content).iter().zip(&self.last_lines) {
            let height = self.last_line_height * row_count(shaped);
            if y < height {
                let (Ok(in_line) | Err(in_line)) =
                    shaped.closest_index_for_position(point(x, y), self.last_line_height);
                // The offset must stay inside this line whatever the shaper answers.
                return floor_boundary(&self.content, (range.start + in_line).min(range.end));
            }
            y -= height;
        }
        self.content.len()
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

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    /// The range an edit replaces: the IME's, else the marked text, else the selection.
    fn edit_range(&self, range_utf16: Option<&Range<usize>>) -> Range<usize> {
        let range = range_utf16
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        // A stale range must never reach the slicing in `splice`: that is a crash.
        clamp_range(&self.content, range)
    }

    /// Text on its way in from the keyboard or the IME: an input takes no line break.
    fn typed<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if self.single_line {
            self.displayed(text)
        } else {
            Cow::Borrowed(text)
        }
    }

    fn splice(&mut self, range: &Range<usize>, new_text: &str) {
        // Replacing nothing with nothing (Backspace at the start) is not an edit.
        if !(range.is_empty() && new_text.is_empty()) {
            self.unedited = None;
            self.edits += 1;
        }
        self.content.replace_range(range.clone(), new_text);
        self.reveal_cursor = true;
    }
}

impl EntityInputHandler for TextArea {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = clamp_range(&self.content, self.range_from_utf16(&range_utf16));
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_owned())
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
        let range = self.edit_range(range_utf16.as_ref());
        let new_text = self.typed(new_text);
        let new_text = new_text.as_ref();
        self.splice(&range, new_text);
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.edit_range(range_utf16.as_ref());
        let new_text = self.typed(new_text);
        let new_text = new_text.as_ref();
        self.splice(&range, new_text);
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
        self.layout_is_current().then_some(())?;
        let range = self.range_from_utf16(&range_utf16);
        let lines = line_ranges(&self.content);
        let (start_x, row) = locate(&lines, &self.last_lines, range.start);
        let (end_x, end_row) = locate(&lines, &self.last_lines, range.end);
        // A range that runs on to another row is answered for its first row.
        let end_x = if end_row == row { end_x } else { start_x };
        let top = bounds.top() - self.scroll_y + self.last_line_height * row;
        let left = bounds.left() - self.scroll_x;
        Some(Bounds::from_corners(
            point(left + start_x, top),
            point(left + end_x, top + self.last_line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.last_bounds?.contains(&point).then_some(())?;
        Some(self.offset_to_utf16(self.index_for_position(point)))
    }
}

struct TextElement {
    input: Entity<TextArea>,
}

struct PrepaintState {
    lines: Vec<WrappedLine>,
    rows: Vec<Range<usize>>,
    line_height: Pixels,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
    scroll_y: Pixels,
    scroll_x: Pixels,
    content_height: Pixels,
    is_placeholder: bool,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
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
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        if self.input.read(cx).single_line {
            style.size.height = window.line_height().into();
        } else {
            // Takes whatever height the column around it has left, down to nothing: the
            // text scrolls inside instead of pushing the field taller.
            style.flex_grow = 1.;
            style.flex_shrink = 1.;
            style.flex_basis = px(0.).into();
            style.min_size.height = px(0.).into();
        }
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
        let line_height = window.line_height();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let run = |len: usize, marked: bool, dim: bool| TextRun {
            len,
            font: style.font(),
            // The placeholder is a dimmed text color, so it reads on light and dark backgrounds.
            color: if dim {
                style.color.opacity(0.45)
            } else {
                style.color
            },
            background_color: None,
            underline: marked.then_some(UnderlineStyle {
                color: Some(style.color),
                thickness: px(1.0),
                wavy: false,
            }),
            strikethrough: None,
        };

        let is_placeholder = input.content.is_empty();
        let (text, runs): (SharedString, Vec<TextRun>) = if is_placeholder {
            let placeholder = input.placeholder.clone();
            let runs = vec![run(placeholder.len(), false, true)];
            (placeholder, runs)
        } else {
            let [before, marked, after] =
                marked_split(input.content.len(), input.marked_range.as_ref());
            let runs = [(before, false), (marked, true), (after, false)]
                .into_iter()
                .filter(|(len, _)| *len > 0)
                .map(|(len, marked)| run(len, marked, false))
                .collect();
            (input.content.clone().into(), runs)
        };
        let wrap_width = (!input.single_line).then_some(bounds.size.width);
        // A text that fails to shape is drawn as nothing: there is nothing to recover.
        let lines: Vec<WrappedLine> = window
            .text_system()
            .shape_text(text, font_size, &runs, wrap_width, None)
            .map(|lines| lines.into_vec())
            .unwrap_or_default();

        let line_ranges = line_ranges(&input.content);
        let wraps: Vec<Vec<usize>> = lines.iter().map(wrap_offsets).collect();
        #[expect(clippy::single_range_in_vec_init, reason = "one row that covers nothing, not the numbers of a range")]
        let rows = if is_placeholder {
            vec![0..0]
        } else {
            visual_rows(&line_ranges, &wraps)
        };
        let total_rows: usize = lines.iter().map(row_count).sum();
        let content_height = line_height * total_rows.max(1);

        let cursor_offset = input.cursor_offset();
        let (cursor_x, cursor_row) = if is_placeholder {
            (px(0.), 0)
        } else {
            locate(&line_ranges, &lines, cursor_offset)
        };
        let scroll_y = px(scroll_for(
            f32::from(input.scroll_y),
            input
                .reveal_cursor
                .then_some(f32::from(line_height * cursor_row)),
            f32::from(line_height),
            f32::from(bounds.size.height),
            f32::from(content_height),
        ));
        // A single line follows its cursor sideways; wrapped text never scrolls that way.
        let cursor_width = px(2.);
        let scroll_x = if input.single_line && !is_placeholder {
            let text_width = lines
                .first()
                .map_or(px(0.), |line| line.unwrapped_layout.width);
            px(scroll_for(
                f32::from(input.scroll_x),
                input.reveal_cursor.then_some(f32::from(cursor_x)),
                f32::from(cursor_width),
                f32::from(bounds.size.width),
                f32::from(text_width + cursor_width),
            ))
        } else {
            px(0.)
        };
        let origin = point(bounds.left() - scroll_x, bounds.top() - scroll_y);
        let row_top = |row: usize| origin.y + line_height * row;

        let selected = input.selected_range.clone();
        let mut selections = Vec::new();
        let mut cursor = None;
        if selected.is_empty() || is_placeholder {
            cursor = Some(fill(
                Bounds::new(
                    point(origin.x + cursor_x, row_top(cursor_row)),
                    size(cursor_width, line_height),
                ),
                gpui::blue(),
            ));
        } else {
            let mut row = 0;
            for (line, shaped) in line_ranges.iter().zip(&lines) {
                let x = |offset: usize| {
                    shaped
                        .unwrapped_layout
                        .x_for_index(offset.clamp(line.start, line.end) - line.start)
                };
                let starts = std::iter::once(0).chain(wrap_offsets(shaped));
                let ends = wrap_offsets(shaped)
                    .into_iter()
                    .chain(std::iter::once(line.end - line.start));
                for (start, end) in starts.zip(ends) {
                    let (row_start, row_end) = (line.start + start, line.start + end);
                    let from = selected.start.max(row_start);
                    let to = selected.end.min(row_end);
                    // A selection that runs on past the typed line also covers its break.
                    let line_break = if selected.end > line.end && row_end == line.end {
                        px(4.)
                    } else {
                        px(0.)
                    };
                    if from < to || (from == to && line_break > px(0.)) {
                        selections.push(fill(
                            Bounds::from_corners(
                                point(origin.x + x(from) - x(row_start), row_top(row)),
                                point(
                                    origin.x + x(to) - x(row_start) + line_break,
                                    row_top(row) + line_height,
                                ),
                            ),
                            rgba(0x3311ff30),
                        ));
                    }
                    row += 1;
                }
            }
        }

        PrepaintState {
            lines,
            rows,
            line_height,
            cursor,
            selections,
            scroll_y,
            scroll_x,
            content_height,
            is_placeholder,
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
        let lines = std::mem::take(&mut prepaint.lines);
        let line_height = prepaint.line_height;
        let (scroll_y, scroll_x) = (prepaint.scroll_y, prepaint.scroll_x);
        let selections = std::mem::take(&mut prepaint.selections);
        let cursor = prepaint
            .cursor
            .take()
            .filter(|_| focus_handle.is_focused(window));
        // Text scrolled out of the field is cut at its edge.
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for selection in selections {
                window.paint_quad(selection);
            }
            let mut top = bounds.top() - scroll_y;
            for line in &lines {
                let height = line_height * row_count(line);
                if top + height >= bounds.top() && top <= bounds.bottom() {
                    // A line that fails to paint is skipped: there is nothing to recover.
                    let _ = line.paint(
                        point(bounds.left() - scroll_x, top),
                        line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
                top += height;
            }
            if let Some(cursor) = cursor {
                window.paint_quad(cursor);
            }
        });

        let rows = std::mem::take(&mut prepaint.rows);
        let (content_height, is_placeholder) = (prepaint.content_height, prepaint.is_placeholder);
        self.input.update(cx, |input, _cx| {
            // The placeholder's lines are not the text's: nothing to hit-test against.
            input.last_lines = if is_placeholder { Vec::new() } else { lines };
            input.last_rows = rows;
            input.last_rows_edits = input.edits;
            input.last_bounds = Some(bounds);
            input.last_line_height = line_height;
            input.content_height = content_height;
            input.scroll_y = scroll_y;
            input.scroll_x = scroll_x;
            input.reveal_cursor = false;
        });
    }
}

impl Render for TextArea {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .w_full()
            // A textarea fills the column it is in; an input is as high as its line.
            .when(!self.single_line, |field| field.flex_1().min_h_0())
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
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
            .on_action(cx.listener(Self::newline))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// How many rows a typed line takes once wrapped.
fn row_count(line: &WrappedLine) -> usize {
    line.wrap_boundaries().len() + 1
}

/// Where the wrapped rows of a typed line begin after its first, as byte offsets into
/// the line.
fn wrap_offsets(line: &WrappedLine) -> Vec<usize> {
    line.wrap_boundaries()
        .iter()
        .filter_map(|boundary| {
            let run = line.unwrapped_layout.runs.get(boundary.run_ix)?;
            Some(run.glyphs.get(boundary.glyph_ix)?.index)
        })
        .collect()
}

/// Where `offset` is drawn: x inside its row, and the row counted over the whole text.
/// An offset at a wrap point stays at the end of the row before it.
fn locate(lines: &[Range<usize>], shaped: &[WrappedLine], offset: usize) -> (Pixels, usize) {
    let line = line_at(lines, offset);
    let rows_before: usize = shaped.iter().take(line).map(row_count).sum();
    let (Some(range), Some(shaped)) = (lines.get(line), shaped.get(line)) else {
        return (px(0.), rows_before);
    };
    let in_line = offset.clamp(range.start, range.end) - range.start;
    let wraps = wrap_offsets(shaped);
    let row = row_in_line(&wraps, in_line);
    let row_start = row
        .checked_sub(1)
        .and_then(|before| wraps.get(before))
        .copied()
        .unwrap_or(0);
    let layout = &shaped.unwrapped_layout;
    (
        layout.x_for_index(in_line) - layout.x_for_index(row_start),
        rows_before + row,
    )
}

/// The wrapped row of a typed line that holds the offset `in_line`.
fn row_in_line(wraps: &[usize], in_line: usize) -> usize {
    wraps.iter().filter(|wrap| **wrap < in_line).count()
}

/// The line breaks of a `<textarea>` value: CRLF and a lone CR both read as LF.
fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Byte range of every typed line, without its line break. Never empty: "" is one empty line.
fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut start = 0;
    let mut ranges: Vec<Range<usize>> = text
        .match_indices('\n')
        .map(|(index, _)| {
            let range = start..index;
            start = index + 1;
            range
        })
        .collect();
    ranges.push(start..text.len());
    ranges
}

/// The line holding `offset`; an offset at a line's end belongs to that line.
fn line_at(ranges: &[Range<usize>], offset: usize) -> usize {
    ranges
        .iter()
        .position(|range| offset <= range.end)
        .unwrap_or(ranges.len().saturating_sub(1))
}

/// Every row on screen as a byte range of the text: each typed line cut at its wrap
/// points (`wraps[line]`, offsets into that line). A wrap point outside its line or out
/// of order is ignored. Never empty.
fn visual_rows(lines: &[Range<usize>], wraps: &[Vec<usize>]) -> Vec<Range<usize>> {
    let mut rows = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let mut start = line.start;
        for wrap in wraps.get(index).into_iter().flatten() {
            let at = line.start + wrap;
            if at > start && at < line.end {
                rows.push(start..at);
                start = at;
            }
        }
        rows.push(start..line.end);
    }
    if rows.is_empty() {
        rows.push(0..0);
    }
    rows
}

/// Where the cursor lands one row up (`delta` < 0) or down. Past the first or last row
/// it goes to the start or end of the text, as in a textarea.
fn vertical_target(text: &str, rows: &[Range<usize>], cursor: usize, delta: isize) -> usize {
    let row = line_at(rows, cursor);
    let Some(target) = row
        .checked_add_signed(delta)
        .and_then(|target| rows.get(target))
    else {
        return if delta < 0 { 0 } else { text.len() };
    };
    let column = rows
        .get(row)
        .and_then(|range| text.get(range.start..cursor))
        .map_or(0, |before| before.chars().count());
    let target_text = text.get(target.clone()).unwrap_or_default();
    let in_row = target_text
        .char_indices()
        .nth(column)
        .map_or(target_text.len(), |(index, _)| index);
    target.start + in_row
}

/// How far the text is scrolled along one axis after a wheel move or a cursor move:
/// within what there is to scroll, and far enough that the cursor (when given: where it
/// starts and how much room it takes) is in view. Down the field the cursor takes a
/// row; along a single line, its own width.
fn scroll_for(
    scroll: f32,
    cursor: Option<f32>,
    cursor_size: f32,
    viewport: f32,
    content: f32,
) -> f32 {
    let max = (content - viewport).max(0.0);
    // NaN (a layout that never happened) scrolls nowhere.
    let mut scroll = if scroll.is_nan() { 0.0 } else { scroll };
    if let Some(start) = cursor {
        if start < scroll {
            scroll = start;
        } else if start + cursor_size > scroll + viewport {
            scroll = start + cursor_size - viewport;
        }
    }
    scroll.max(0.0).min(max)
}

/// How the text's bytes split into [before, inside, after] the text the IME is composing.
fn marked_split(len: usize, marked: Option<&Range<usize>>) -> [usize; 3] {
    let Some(marked) = marked else {
        return [len, 0, 0];
    };
    let start = marked.start.min(len);
    let end = marked.end.min(len).max(start);
    [start, end - start, len - end]
}

/// The nearest character boundary at or before `index`.
fn floor_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Clamp a byte range into `text`, on character boundaries.
fn clamp_range(text: &str, range: Range<usize>) -> Range<usize> {
    let start = floor_boundary(text, range.start);
    start..floor_boundary(text, range.end).max(start)
}

/// Where the selection lands after the IME replaced a range with `new_text`. The IME's
/// offsets are UTF-16 and relative to `new_text`, which starts at byte `start`.
///
/// The GPUI example this file comes from added the replaced range's *end* to the
/// selection's end: deleting a Hangul syllable mid-composition left a selection past
/// the end of the text, and the next keystroke crashed.
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
        Some(selected) => {
            let from = start + byte_offset(selected.start);
            from..(start + byte_offset(selected.end)).max(from)
        }
        None => start + new_text.len()..start + new_text.len(),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{Modifiers, ScrollDelta, TestAppContext, TouchPhase, VisualTestContext};

    use super::*;

    #[test]
    fn 줄바꿈마다_줄_범위를_나누고_빈_글도_한_줄이다() {
        assert_eq!(line_ranges(""), vec![0..0]);
        assert_eq!(line_ranges("abc"), vec![0..3]);
        assert_eq!(line_ranges("ab\n\ncd"), vec![0..2, 3..3, 4..6]);
        // 끝의 줄바꿈 뒤에도 빈 줄이 하나 있다.
        assert_eq!(line_ranges("ab\n"), vec![0..2, 3..3]);
    }

    #[test]
    fn 줄_끝의_오프셋은_그_줄에_속한다() {
        let ranges = line_ranges("ab\n\ncd");
        assert_eq!(line_at(&ranges, 0), 0);
        assert_eq!(line_at(&ranges, 2), 0);
        assert_eq!(line_at(&ranges, 3), 1);
        assert_eq!(line_at(&ranges, 4), 2);
        assert_eq!(line_at(&ranges, 6), 2);
        // 글 밖의 오프셋은 마지막 줄로 본다.
        assert_eq!(line_at(&ranges, 99), 2);
        assert_eq!(line_at(&[], 3), 0);
    }

    #[test]
    fn 화면_줄은_입력한_줄을_접힌_자리에서_나눈_것이다() {
        // "abcdefgh\nxy": 첫 줄이 3, 6 에서 접힌다.
        let lines = line_ranges("abcdefgh\nxy");
        assert_eq!(
            visual_rows(&lines, &[vec![3, 6], vec![]]),
            vec![0..3, 3..6, 6..8, 9..11]
        );
        // 접힘이 없으면 입력한 줄 그대로다.
        assert_eq!(visual_rows(&lines, &[]), vec![0..8, 9..11]);
        // 줄 밖이거나 순서가 어긋난 접힘은 무시한다.
        assert_eq!(
            visual_rows(&lines, &[vec![0, 6, 3, 8, 99], vec![5]]),
            vec![0..6, 6..8, 9..11]
        );
        assert_eq!(visual_rows(&[], &[]), vec![0..0]);
    }

    #[test]
    fn 접힌_자리의_오프셋은_앞_줄_끝에_속한다() {
        assert_eq!(row_in_line(&[3, 6], 0), 0);
        assert_eq!(row_in_line(&[3, 6], 3), 0);
        assert_eq!(row_in_line(&[3, 6], 4), 1);
        assert_eq!(row_in_line(&[3, 6], 6), 1);
        assert_eq!(row_in_line(&[3, 6], 8), 2);
        assert_eq!(row_in_line(&[], 5), 0);
    }

    #[test]
    fn 위아래_이동은_같은_열을_유지하고_짧은_줄에서는_줄_끝에_선다() {
        let text = "abcdef\nxy\n한글입니다";
        let rows = line_ranges(text);
        // "abcd|ef" 에서 아래로: "xy" 는 짧으므로 줄 끝.
        assert_eq!(vertical_target(text, &rows, 4, 1), 9);
        // "xy|" 에서 아래로: 3번째 줄의 2열 = "한글" 뒤(바이트 10 + 6).
        assert_eq!(vertical_target(text, &rows, 9, 1), 16);
        // 3번째 줄 2열에서 위로: "xy" 끝.
        assert_eq!(vertical_target(text, &rows, 16, -1), 9);
        // "x|y" 에서 위로: 첫 줄의 1열.
        assert_eq!(vertical_target(text, &rows, 8, -1), 1);
    }

    #[test]
    fn 위아래_이동은_접힌_줄_사이도_한_줄씩_오간다() {
        // 한 줄 "abcdefgh" 가 3글자씩 접혀 있다.
        let text = "abcdefgh";
        let rows = visual_rows(&line_ranges(text), &[vec![3, 6]]);
        // "a|bc" 에서 아래로: 둘째 화면 줄의 1열.
        assert_eq!(vertical_target(text, &rows, 1, 1), 4);
        assert_eq!(vertical_target(text, &rows, 4, 1), 7);
        // 마지막 화면 줄에서 아래로는 글의 끝.
        assert_eq!(vertical_target(text, &rows, 7, 1), 8);
        assert_eq!(vertical_target(text, &rows, 7, -1), 4);
        // 셋째 줄은 두 글자뿐이다: 둘째 줄 끝(접힌 자리, 6)에서 아래로 가면 줄 끝.
        assert_eq!(vertical_target(text, &rows, 6, 1), 8);
    }

    #[test]
    fn 첫_줄에서_위로는_글의_처음_마지막_줄에서_아래로는_글의_끝이다() {
        let text = "abc\ndef";
        let rows = line_ranges(text);
        assert_eq!(vertical_target(text, &rows, 2, -1), 0);
        assert_eq!(vertical_target(text, &rows, 5, 1), 7);
        assert_eq!(vertical_target("", &line_ranges(""), 0, 1), 0);
        assert_eq!(vertical_target("", &line_ranges(""), 0, -1), 0);
        // 낡은 줄 정보(글보다 긴 범위)가 와도 글 밖을 읽지 않는다.
        assert_eq!(vertical_target("ab", &[0..9, 10..20], 1, 1), 10);
    }

    #[test]
    fn 스크롤은_커서_줄이_보이는_만큼만_움직이고_내용_밖으로_나가지_않는다() {
        // 줄 높이 10, 보이는 높이 30, 내용 100.
        assert_eq!(scroll_for(0.0, Some(10.0), 10.0, 30.0, 100.0), 0.0);
        assert_eq!(scroll_for(0.0, Some(30.0), 10.0, 30.0, 100.0), 10.0);
        assert_eq!(scroll_for(50.0, Some(20.0), 10.0, 30.0, 100.0), 20.0);
        assert_eq!(scroll_for(0.0, Some(90.0), 10.0, 30.0, 100.0), 70.0);
        // 휠: 커서와 무관하게 범위 안에서만.
        assert_eq!(scroll_for(-15.0, None, 10.0, 30.0, 100.0), 0.0);
        assert_eq!(scroll_for(500.0, None, 10.0, 30.0, 100.0), 70.0);
        assert_eq!(scroll_for(25.0, None, 10.0, 30.0, 100.0), 25.0);
        // 내용이 다 보이면 스크롤은 없다.
        assert_eq!(scroll_for(40.0, Some(10.0), 10.0, 30.0, 20.0), 0.0);
        assert_eq!(scroll_for(f32::NAN, None, 10.0, 30.0, 100.0), 0.0);
    }

    #[test]
    #[expect(clippy::reversed_empty_ranges, reason = "a reversed range is the input under test")]
    fn 조합_중인_범위는_글을_앞_안_뒤로_나눈다() {
        assert_eq!(marked_split(10, Some(&(6..8))), [6, 2, 2]);
        assert_eq!(marked_split(10, None), [10, 0, 0]);
        // 글 밖으로 걸친 범위는 글 안쪽만.
        assert_eq!(marked_split(10, Some(&(8..20))), [8, 2, 0]);
        assert_eq!(marked_split(10, Some(&(12..14))), [10, 0, 0]);
        // 뒤집힌 범위가 와도 길이가 음수가 되지 않는다.
        assert_eq!(marked_split(10, Some(&(8..6))), [8, 0, 2]);
    }

    #[test]
    #[expect(clippy::reversed_empty_ranges, reason = "a reversed range is the input under test")]
    fn 조합_중인_한글을_지워도_선택_범위가_글자_밖으로_나가지_않는다() {
        assert_eq!(marked_selection(0, "", Some(0..0)), 0..0);
        assert_eq!(marked_selection(0, "하", Some(1..1)), 3..3);
        assert_eq!(marked_selection(6, "한", Some(1..1)), 9..9);
        assert_eq!(marked_selection(6, "한", None), 9..9);
        // 뒤집힌 선택이 와도 범위는 뒤집히지 않는다.
        assert_eq!(marked_selection(0, "한글", Some(2..1)), 6..6);
    }

    #[test]
    fn 낡은_범위는_글자_경계_안으로_잘라낸다() {
        assert_eq!(clamp_range("", 0..3), 0..0);
        assert_eq!(clamp_range("한", 3..6), 3..3);
        assert_eq!(clamp_range("한글", 1..4), 0..3);
        assert_eq!(clamp_range("abc", 1..2), 1..2);
        assert_eq!(floor_boundary("한글", 5), 3);
        assert_eq!(floor_boundary("한글", 99), 6);
    }

    #[test]
    fn crlf와_홀로_있는_cr은_lf가_된다() {
        assert_eq!(normalize_newlines("a\r\nb\rc\nd"), "a\nb\nc\nd");
        assert_eq!(normalize_newlines(""), "");
    }

    // ── the field in a window: keys, clipboard, mouse, wheel, IME ──────────

    const LINE: f32 = 13.0;

    /// A 300 × 65 box (five rows) around the field, as a panel gives it one.
    struct Host(Entity<TextArea>);

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(300.0))
                .h(px(LINE * 5.0))
                .flex()
                .flex_col()
                .font_family("Menlo")
                .text_size(px(11.0))
                .line_height(px(LINE))
                .child(self.0.clone())
        }
    }

    fn open(cx: &mut TestAppContext) -> (Entity<TextArea>, &mut VisualTestContext) {
        cx.update(kit::bind_keys);
        let (host, cx) = cx.add_window_view(|window, cx| {
            let area = cx.new(|cx| TextArea::new("placeholder", cx));
            area.read(cx).focus(window);
            Host(area)
        });
        let area = host.read_with(cx, |host, _| host.0.clone());
        (area, cx)
    }

    fn text(area: &Entity<TextArea>, cx: &mut VisualTestContext) -> String {
        area.read_with(cx, |area, _| area.text().to_owned())
    }

    fn set(area: &Entity<TextArea>, cx: &mut VisualTestContext, text: &str) {
        area.update(cx, |area, cx| area.set_text(text, cx));
        cx.run_until_parked();
    }

    fn submissions(
        area: &Entity<TextArea>,
        cx: &mut VisualTestContext,
    ) -> Rc<RefCell<Vec<String>>> {
        let submitted = Rc::new(RefCell::new(Vec::new()));
        let seen = submitted.clone();
        cx.update(|_, cx| {
            cx.subscribe(area, move |_, event: &Submitted, _| {
                seen.borrow_mut().push(event.0.clone())
            })
            .detach()
        });
        submitted
    }

    #[gpui::test]
    fn 필드는_부모가_준_높이를_채운다(cx: &mut TestAppContext) {
        let (area, cx) = open(cx);
        let bounds = area
            .read_with(cx, |area, _| area.last_bounds)
            .expect("the field was painted");
        assert_eq!(bounds.size.width, px(300.0));
        assert_eq!(bounds.size.height, px(LINE * 5.0));
        // 글이 길어져도 필드는 커지지 않는다.
        set(&area, cx, "0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        let bounds = area.read_with(cx, |area, _| area.last_bounds).unwrap();
        assert_eq!(bounds.size.height, px(LINE * 5.0));
    }

    #[gpui::test]
    fn enter와_shift_enter는_줄을_바꾸고_제출하지_않는다(cx: &mut TestAppContext) {
        let (area, cx) = open(cx);
        let submitted = submissions(&area, cx);
        cx.simulate_input("ab");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("cd");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("ef");
        assert_eq!(text(&area, cx), "ab\ncd\nef");
        assert!(submitted.borrow().is_empty());
        assert_eq!(area.read_with(cx, |area, _| area.last_lines.len()), 3);
    }

    #[gpui::test]
    fn cmd_enter와_ctrl_enter는_줄을_바꾸지_않고_내용을_제출한다(
        cx: &mut TestAppContext,
    ) {
        let (area, cx) = open(cx);
        let submitted = submissions(&area, cx);
        cx.simulate_input("ab");
        cx.simulate_keystrokes("cmd-enter ctrl-enter cmd-shift-enter");
        assert_eq!(*submitted.borrow(), ["ab", "ab", "ab"]);
        assert_eq!(text(&area, cx), "ab");
    }

    #[gpui::test]
    fn 조합_중의_enter는_줄을_바꾸지_않는다(cx: &mut TestAppContext) {
        let (area, cx) = open(cx);
        area.update_in(cx, |area, window, cx| {
            area.replace_and_mark_text_in_range(None, "한", Some(1..1), window, cx)
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        assert_eq!(text(&area, cx), "한");
        // 조합이 확정되면 다시 줄이 바뀐다.
        area.update_in(cx, |area, window, cx| {
            area.replace_text_in_range(None, "한", window, cx)
        });
        cx.simulate_keystrokes("enter");
        assert_eq!(text(&area, cx), "한\n");
    }

    #[gpui::test]
    fn crlf가_든_글은_고치기_전에는_그대로_고치면_lf로_읽힌다(
        cx: &mut TestAppContext,
    ) {
        // textarea 는 CRLF·CR 을 LF 로 보여주지만, React 상태는 첫 입력 전까지 받은 문자열 그대로다.
        let (area, cx) = open(cx);
        set(&area, cx, "SEND\r\nid:1\rbody");
        assert_eq!(text(&area, cx), "SEND\r\nid:1\rbody");
        // 화면에는 세 줄이다.
        assert_eq!(area.read_with(cx, |area, _| area.last_lines.len()), 3);
        // 커서 이동은 입력이 아니다.
        cx.simulate_keystrokes("up down home end");
        assert_eq!(text(&area, cx), "SEND\r\nid:1\rbody");
        cx.simulate_input("!");
        assert_eq!(text(&area, cx), "SEND\nid:1\nbody!");
        // 줄바꿈이 LF 뿐인 글은 처음부터 그대로다.
        set(&area, cx, "a\nb");
        assert_eq!(text(&area, cx), "a\nb");
        assert_eq!(area.read_with(cx, |area, _| area.unedited.clone()), None);
    }

    #[gpui::test]
    fn 맨_앞의_backspace는_고친_것이_아니다(cx: &mut TestAppContext) {
        let (area, cx) = open(cx);
        set(&area, cx, "a\r\nb");
        cx.simulate_keystrokes("cmd-a left backspace");
        assert_eq!(text(&area, cx), "a\r\nb");
        cx.simulate_keystrokes("delete");
        assert_eq!(text(&area, cx), "\nb");
    }

    #[gpui::test]
    fn 위아래_화살표는_줄_사이를_오간다(cx: &mut TestAppContext) {
        let (area, cx) = open(cx);
        set(&area, cx, "abcd\nxy");
        cx.simulate_keystrokes("up");
        cx.simulate_input("1");
        assert_eq!(text(&area, cx), "ab1cd\nxy");
        cx.simulate_keystrokes("down");
        cx.simulate_input("2");
        assert_eq!(text(&area, cx), "ab1cd\nxy2");
        // 첫 줄에서 위로는 글의 처음.
        cx.simulate_keystrokes("up up");
        cx.simulate_input("0");
        assert_eq!(text(&area, cx), "0ab1cd\nxy2");
    }

    #[gpui::test]
    fn shift와_화살표는_줄을_넘어_선택한다(cx: &mut TestAppContext) {
        let (area, cx) = open(cx);
        set(&area, cx, "ab\ncd");
        cx.simulate_keystrokes("shift-up");
        assert_eq!(
            area.read_with(cx, |area, _| area.selected_range.clone()),
            2..5
        );
        cx.simulate_keystrokes("backspace");
        assert_eq!(text(&area, cx), "ab");
    }

    #[gpui::test]
    fn home과_end는_커서가_있는_줄_안에서_움직인다(cx: &mut TestAppContext) {
        let (area, cx) = open(cx);
        set(&area, cx, "ab\ncd");
        cx.simulate_keystrokes("home");
        cx.simulate_input("1");
        assert_eq!(text(&area, cx), "ab\n1cd");
        cx.simulate_keystrokes("end");
        cx.simulate_input("2");
        assert_eq!(text(&area, cx), "ab\n1cd2");
    }

    #[gpui::test]
    fn 지우기는_줄바꿈과_여러_바이트_글자를_한_번에_지운다(
        cx: &mut TestAppContext,
    ) {
        let (area, cx) = open(cx);
        set(&area, cx, "한😀\n");
        cx.simulate_keystrokes("backspace");
        assert_eq!(text(&area, cx), "한😀");
        cx.simulate_keystrokes("backspace");
        assert_eq!(text(&area, cx), "한");
        cx.simulate_keystrokes("left delete");
        assert_eq!(text(&area, cx), "");
        // 빈 글에서 더 지워도 아무 일 없다.
        cx.simulate_keystrokes("backspace delete left right up down home end");
        assert_eq!(text(&area, cx), "");
    }

    #[gpui::test]
    fn 붙여넣기는_줄바꿈을_유지하고_전체_선택_후_잘라내면_비워진다(
        cx: &mut TestAppContext,
    ) {
        let (area, cx) = open(cx);
        cx.write_to_clipboard(ClipboardItem::new_string(
            "SEND\r\nid:1\n\nbody\rend".into(),
        ));
        cx.simulate_keystrokes("cmd-v");
        // CRLF 와 홀로 있는 CR 은 textarea 처럼 LF 가 된다.
        assert_eq!(text(&area, cx), "SEND\nid:1\n\nbody\nend");
        cx.simulate_keystrokes("cmd-a cmd-x");
        assert_eq!(text(&area, cx), "");
        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(clipboard.as_deref(), Some("SEND\nid:1\n\nbody\nend"));
    }

    #[gpui::test]
    fn 긴_줄은_필드_폭에서_접히고_위아래는_접힌_줄을_한_줄씩_오간다(
        cx: &mut TestAppContext,
    ) {
        let (area, cx) = open(cx);
        // 300px 에 11px 고정폭 글자는 45자쯤 들어간다: 120자는 세 줄로 접힌다.
        set(&area, cx, &"a".repeat(120));
        let rows = area.read_with(cx, |area, _| area.last_rows.clone());
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(rows.first().map(|row| row.start), Some(0));
        assert_eq!(rows.last().map(|row| row.end), Some(120));
        // 입력한 줄은 여전히 하나다.
        assert_eq!(area.read_with(cx, |area, _| area.last_lines.len()), 1);
        let width = rows[0].len();

        // 커서는 끝: 위로 한 번이면 접힌 폭만큼만 앞으로 간다.
        cx.simulate_keystrokes("up");
        assert_eq!(
            area.read_with(cx, |area, _| area.selected_range.clone()),
            120 - width..120 - width
        );
        // home 은 입력한 줄의 처음이다.
        cx.simulate_keystrokes("home");
        assert_eq!(
            area.read_with(cx, |area, _| area.selected_range.clone()),
            0..0
        );
        // 둘째 화면 줄을 누르면 그 줄 안의 오프셋이 된다.
        let bounds = area.read_with(cx, |area, _| area.last_bounds).unwrap();
        cx.simulate_click(
            point(bounds.left() + px(1.0), bounds.top() + px(LINE * 1.5)),
            Modifiers::default(),
        );
        let cursor = area.read_with(cx, |area, _| area.selected_range.start);
        assert!((width..2 * width).contains(&cursor), "{cursor}");
    }

    #[gpui::test]
    fn 필드보다_긴_글은_커서를_따라_안에서_스크롤된다(cx: &mut TestAppContext) {
        let (area, cx) = open(cx);
        set(&area, cx, "0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        let scroll = |cx: &mut VisualTestContext| area.read_with(cx, |area, _| area.scroll_y);
        // 밖에서 넣은 값은 스크롤하지 않는다: 글의 처음이 보인다.
        assert_eq!(scroll(cx), px(0.0));
        // 커서(10번째 줄 끝)를 움직이면 마지막 다섯 줄이 보인다.
        cx.simulate_keystrokes("end");
        assert_eq!(scroll(cx), px(LINE * 5.0));
        cx.simulate_keystrokes("up up up up up up up up up");
        assert_eq!(scroll(cx), px(0.0));
        cx.simulate_keystrokes("down down down down down");
        assert_eq!(scroll(cx), px(LINE));
    }

    #[gpui::test]
    fn 휠은_글을_범위_안에서_굴리고_커서는_그대로_둔다(
        cx: &mut TestAppContext,
    ) {
        let (area, cx) = open(cx);
        set(&area, cx, "0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        cx.simulate_keystrokes("cmd-a left");
        let scroll = |cx: &mut VisualTestContext| area.read_with(cx, |area, _| area.scroll_y);
        assert_eq!(scroll(cx), px(0.0));
        let bounds = area.read_with(cx, |area, _| area.last_bounds).unwrap();
        let wheel = |cx: &mut VisualTestContext, y: f32| {
            cx.simulate_event(ScrollWheelEvent {
                position: bounds.center(),
                delta: ScrollDelta::Pixels(point(px(0.0), px(y))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
            });
            cx.run_until_parked();
        };
        wheel(cx, -30.0);
        assert_eq!(scroll(cx), px(30.0));
        // 끝을 넘겨 굴려도 내용의 끝에서 멈춘다.
        wheel(cx, -500.0);
        assert_eq!(scroll(cx), px(LINE * 5.0));
        wheel(cx, 1000.0);
        assert_eq!(scroll(cx), px(0.0));
        assert_eq!(
            area.read_with(cx, |area, _| area.selected_range.clone()),
            0..0
        );
    }

    #[gpui::test]
    fn 누른_자리의_줄로_커서가_가고_끌면_그_사이가_선택된다(
        cx: &mut TestAppContext,
    ) {
        let (area, cx) = open(cx);
        set(&area, cx, "abc\ndef");
        let bounds = area
            .read_with(cx, |area, _| area.last_bounds)
            .expect("the field was painted");
        // 둘째 줄의 왼쪽 끝.
        let second_line = point(bounds.left(), bounds.top() + px(LINE * 1.5));
        cx.simulate_click(second_line, Modifiers::default());
        cx.simulate_input("1");
        assert_eq!(text(&area, cx), "abc\n1def");
        // 첫 줄에서 둘째 줄로 끌면 그 사이가 선택된다.
        let first_line = point(bounds.left(), bounds.top() + px(LINE * 0.5));
        cx.simulate_mouse_down(first_line, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(second_line, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(second_line, MouseButton::Left, Modifiers::default());
        assert_eq!(
            area.read_with(cx, |area, _| area.selected_range.clone()),
            0..4
        );
        cx.simulate_input("2");
        assert_eq!(text(&area, cx), "21def");
        // 글 아래의 빈 곳을 누르면 글의 끝이다.
        cx.simulate_click(
            point(bounds.left() + px(5.0), bounds.bottom() - px(2.0)),
            Modifiers::default(),
        );
        cx.simulate_input("3");
        assert_eq!(text(&area, cx), "21def3");
    }

    #[gpui::test]
    fn 한글_조합은_둘째_줄에서도_밑줄_범위를_지키며_확정된다(
        cx: &mut TestAppContext,
    ) {
        let (area, cx) = open(cx);
        set(&area, cx, "ab\n");
        let compose = |cx: &mut VisualTestContext, text: &str| {
            area.update_in(cx, |area, window, cx| {
                area.replace_and_mark_text_in_range(None, text, Some(1..1), window, cx)
            });
            // 조합 중인 글자가 있는 채로 그려져야 한다.
            cx.run_until_parked();
        };
        compose(cx, "ㅎ");
        assert_eq!(
            area.read_with(cx, |area, _| area.marked_range.clone()),
            Some(3..6)
        );
        compose(cx, "하");
        compose(cx, "한");
        assert_eq!(text(&area, cx), "ab\n한");
        // 조합 창이 뜰 자리는 둘째 줄이다.
        area.update_in(cx, |area, window, cx| {
            let field = area.last_bounds.expect("the field was painted");
            let at = area
                .bounds_for_range(3..4, field, window, cx)
                .expect("the text was painted");
            assert_eq!(at.top(), field.top() + px(LINE));
            assert!(at.size.width > px(0.0));
        });
        // 조합 중에 지우면 IME 는 빈 글자를 보낸다.
        compose(cx, "");
        assert_eq!(text(&area, cx), "ab\n");
        assert_eq!(
            area.read_with(cx, |area, _| area.marked_range.clone()),
            None
        );
        compose(cx, "글");
        area.update_in(cx, |area, window, cx| {
            area.replace_text_in_range(None, "글", window, cx)
        });
        assert_eq!(text(&area, cx), "ab\n글");
        assert_eq!(
            area.read_with(cx, |area, _| area.selected_range.clone()),
            6..6
        );
    }

    #[gpui::test]
    fn ime가_낡은_범위를_보내도_글자_밖을_건드리지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (area, cx) = open(cx);
        set(&area, cx, "한글");
        area.update_in(cx, |area, window, cx| {
            // UTF-16 범위 5..9 는 글(2 단위)보다 길다.
            area.replace_text_in_range(Some(5..9), "!", window, cx);
            let mut actual = None;
            let read = area.text_for_range(1..40, &mut actual, window, cx);
            assert_eq!(read.as_deref(), Some("글!"));
            assert_eq!(actual, Some(1..3));
        });
        assert_eq!(text(&area, cx), "한글!");
    }

    // ── the single-line field ───────────────────────────────────────────────

    /// A 120px-wide box around a one-line field, as an `<input>`'s box is.
    struct LineHost(Entity<TextArea>);

    impl Render for LineHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(120.0))
                .font_family("Menlo")
                .text_size(px(11.0))
                .line_height(px(LINE))
                .child(self.0.clone())
        }
    }

    fn open_line(cx: &mut TestAppContext) -> (Entity<TextArea>, &mut VisualTestContext) {
        cx.update(kit::bind_keys);
        let (host, cx) = cx.add_window_view(|window, cx| {
            let field = cx.new(|cx| TextArea::single_line("placeholder", cx));
            field.read(cx).focus(window);
            LineHost(field)
        });
        let field = host.read_with(cx, |host, _| host.0.clone());
        (field, cx)
    }

    #[gpui::test]
    fn 한_줄_필드는_글_한_줄_높이이고_enter를_받지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (field, cx) = open_line(cx);
        let submitted = submissions(&field, cx);
        let bounds = field.read_with(cx, |field, _| field.last_bounds).unwrap();
        assert_eq!(bounds.size.height, px(LINE));
        assert_eq!(bounds.size.width, px(120.0));
        cx.simulate_input("ab");
        cx.simulate_keystrokes("enter shift-enter cmd-enter ctrl-enter");
        cx.simulate_input("cd");
        assert_eq!(text(&field, cx), "abcd");
        assert!(submitted.borrow().is_empty());
        // IME 가 줄바꿈을 보내도 들어가지 않는다.
        field.update_in(cx, |field, window, cx| {
            field.replace_text_in_range(None, "e\r\nf", window, cx)
        });
        assert_eq!(text(&field, cx), "abcdef");
        assert_eq!(
            field.read_with(cx, |field, _| field.selected_range.clone()),
            6..6
        );
    }

    #[gpui::test]
    fn 한_줄_필드에_준_값의_줄바꿈은_보이지_않지만_고치기_전에는_값에_남는다(
        cx: &mut TestAppContext,
    ) {
        let (field, cx) = open_line(cx);
        set(&field, cx, "a\r\nb\nc");
        assert_eq!(field.read_with(cx, |field, _| field.content.clone()), "abc");
        assert_eq!(text(&field, cx), "a\r\nb\nc");
        cx.simulate_input("!");
        assert_eq!(text(&field, cx), "abc!");
        // 붙여넣은 줄바꿈은 공백이 된다.
        cx.write_to_clipboard(ClipboardItem::new_string("x\r\ny\nz".into()));
        cx.simulate_keystrokes("cmd-a cmd-v");
        assert_eq!(text(&field, cx), "x y z");
    }

    #[gpui::test]
    fn 한_줄_필드는_접히지_않고_커서를_따라_옆으로_민다(
        cx: &mut TestAppContext,
    ) {
        let (field, cx) = open_line(cx);
        // 120px 에 11px 고정폭 글자는 18자쯤 들어간다.
        set(&field, cx, &"a".repeat(60));
        let view = |cx: &mut VisualTestContext| {
            field.read_with(cx, |field, _| (field.scroll_x, field.last_rows.len()))
        };
        // 밖에서 넣은 값은 input 처럼 처음부터 보인다.
        assert_eq!(view(cx), (px(0.0), 1));
        // 커서를 끝으로 보내면 글의 끝이 보이도록 밀린다.
        cx.simulate_keystrokes("end");
        let (scrolled, rows) = view(cx);
        assert_eq!(rows, 1);
        assert!(scrolled > px(200.0), "{scrolled:?}");
        // 위는 글의 처음, 아래는 글의 끝이다.
        cx.simulate_keystrokes("up");
        assert_eq!(view(cx).0, px(0.0));
        assert_eq!(
            field.read_with(cx, |field, _| field.selected_range.clone()),
            0..0
        );
        cx.simulate_keystrokes("down");
        assert_eq!(view(cx).0, scrolled);
        // 민 채로 누르면 보이는 글자의 오프셋이 된다.
        let bounds = field.read_with(cx, |field, _| field.last_bounds).unwrap();
        cx.simulate_click(
            point(bounds.left() + px(1.0), bounds.center().y),
            Modifiers::default(),
        );
        let cursor = field.read_with(cx, |field, _| field.selected_range.start);
        assert!((30..60).contains(&cursor), "{cursor}");
        // 글이 줄어들면 빈 곳이 남지 않게 되돌아온다.
        cx.simulate_keystrokes("cmd-a backspace");
        cx.simulate_input("ab");
        assert_eq!(view(cx).0, px(0.0));
    }

    #[test]
    fn 옆으로_미는_양도_커서가_보이는_만큼만이다() {
        // 폭 100, 글 폭 300, 커서 폭 2.
        assert_eq!(scroll_for(0.0, Some(50.0), 2.0, 100.0, 302.0), 0.0);
        assert_eq!(scroll_for(0.0, Some(300.0), 2.0, 100.0, 302.0), 202.0);
        assert_eq!(scroll_for(202.0, Some(120.0), 2.0, 100.0, 302.0), 120.0);
        // 글이 폭보다 짧으면 밀지 않는다.
        assert_eq!(scroll_for(40.0, Some(30.0), 2.0, 100.0, 32.0), 0.0);
    }

    #[gpui::test]
    fn 빈_필드는_자리표시자를_보이고_바꾸면_다시_그린다(
        cx: &mut TestAppContext,
    ) {
        let (area, cx) = open(cx);
        assert_eq!(text(&area, cx), "");
        // 자리표시자는 글이 아니다: 눌러도 오프셋은 0 이다.
        let bounds = area.read_with(cx, |area, _| area.last_bounds).unwrap();
        cx.simulate_click(bounds.center(), Modifiers::default());
        assert_eq!(
            area.read_with(cx, |area, _| area.selected_range.clone()),
            0..0
        );
        area.update(cx, |area, cx| area.set_placeholder("other", cx));
        cx.run_until_parked();
        assert_eq!(
            area.read_with(cx, |area, _| area.placeholder.clone()),
            SharedString::from("other")
        );
        assert_eq!(text(&area, cx), "");
    }
}
