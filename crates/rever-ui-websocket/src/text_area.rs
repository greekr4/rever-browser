//! A multi-line text field for the frame composer.
//!
//! The kit's `TextInput` holds one line (GPUI's line shaper rejects newlines), and a
//! WebSocket payload may span lines: the Electron composer is a `<textarea>` that grows
//! to six rows. Adapted from `rever_ui_kit::text_input` (itself from GPUI's
//! `examples/input.rs`, Zed Industries, Apache-2.0). It reuses that module's actions, key
//! bindings and `Submitted` event, so the host needs no extra `bind_keys` call.
//!
//! shortcut: long lines scroll sideways instead of soft-wrapping as a textarea does;
//! upgrade to `shape_text` with a wrap width if payloads are edited as prose.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId,
    KeyDownEvent, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point, ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle,
    Window, div, fill, point, prelude::*, px, relative, rgba, size,
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
    /// given until the first edit: an untouched frame is sent again byte for byte.
    unedited: Option<String>,
    placeholder: SharedString,
    max_rows: usize,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    /// The visible lines as painted last, for mouse and IME hit testing.
    last_lines: Vec<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    last_line_height: Pixels,
    is_selecting: bool,
    /// First visible line and sideways offset; both follow the cursor.
    scroll_top: usize,
    scroll_x: Pixels,
}

impl TextArea {
    /// A field that shows one row per line of text, up to `max_rows`, then scrolls.
    pub fn new(
        placeholder: impl Into<SharedString>,
        max_rows: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: String::new(),
            unedited: None,
            placeholder: placeholder.into(),
            max_rows: max_rows.max(1),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_lines: Vec::new(),
            last_bounds: None,
            last_line_height: px(0.),
            is_selecting: false,
            scroll_top: 0,
            scroll_x: px(0.),
        }
    }

    /// The payload to send: `payload` of the component, not what the field displays.
    pub fn text(&self) -> &str {
        self.unedited.as_deref().unwrap_or(&self.content)
    }

    /// Replace the text and put the cursor at the end.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        let text = text.into();
        self.content = normalize_newlines(&text);
        self.unedited = (self.content != text).then_some(text);
        self.selected_range = self.content.len()..self.content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus_handle);
    }

    fn submit(&mut self, _: &kit::Submit, _: &mut Window, cx: &mut Context<Self>) {
        // `!e.nativeEvent.isComposing`: an Enter that arrives mid-composition is the IME's.
        if self.marked_range.is_some() {
            return;
        }
        cx.emit(Submitted(self.text().to_owned()));
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

    /// The keys the kit binds nothing to: Shift+Enter breaks the line, Enter with another
    /// modifier still sends, Up and Down move between lines.
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        // `e.key === 'Enter' && !e.shiftKey`: Ctrl, Alt and Cmd do not keep Enter from
        // sending. Bare Enter is the kit's `Submit` binding and never gets here.
        if keystroke.key == "enter" && !modifiers.shift {
            self.submit(&kit::Submit, window, cx);
            cx.stop_propagation();
            return;
        }
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        let delta = match keystroke.key.as_str() {
            "enter" if modifiers.shift => {
                self.replace_text_in_range(None, "\n", window, cx);
                cx.stop_propagation();
                return;
            }
            "up" => -1,
            "down" => 1,
            _ => return,
        };
        let target = vertical_target(&self.content, self.cursor_offset(), delta);
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
            // Line breaks stay, as in a textarea, which turns CRLF and CR into LF.
            self.replace_text_in_range(None, &normalize_newlines(&text), window, cx);
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
        cx.notify()
    }

    /// The text offset under a window position, from the lines painted last.
    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds else {
            return 0;
        };
        if self.content.is_empty() || self.last_line_height <= px(0.) {
            return 0;
        }
        let ranges = line_ranges(&self.content);
        let row = ((position.y - bounds.top()) / self.last_line_height).max(0.) as usize;
        let line = (self.scroll_top + row).min(ranges.len().saturating_sub(1));
        let Some(range) = ranges.get(line) else {
            return self.content.len();
        };
        let shaped = line
            .checked_sub(self.scroll_top)
            .and_then(|row| self.last_lines.get(row));
        let Some(shaped) = shaped else {
            return range.end;
        };
        let in_line = shaped.closest_index_for_x(position.x - bounds.left() + self.scroll_x);
        // The painted line may be older than the text: keep the offset inside this line.
        floor_boundary(&self.content, (range.start + in_line).min(range.end))
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

    fn splice(&mut self, range: &Range<usize>, new_text: &str) {
        // Replacing nothing with nothing (Backspace at the start) is not an edit.
        if !(range.is_empty() && new_text.is_empty()) {
            self.unedited = None;
        }
        self.content.replace_range(range.clone(), new_text);
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
        let range = self.range_from_utf16(&range_utf16);
        let ranges = line_ranges(&self.content);
        let line = line_at(&ranges, range.start);
        let line_range = ranges.get(line)?;
        let row = line.checked_sub(self.scroll_top)?;
        let shaped = self.last_lines.get(row)?;
        let x = |offset: usize| {
            let in_line = offset.clamp(line_range.start, line_range.end) - line_range.start;
            bounds.left() - self.scroll_x + shaped.x_for_index(in_line)
        };
        let top = bounds.top() + self.last_line_height * row as f32;
        Some(Bounds::from_corners(
            point(x(range.start), top),
            point(x(range.end), top + self.last_line_height),
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
    lines: Vec<ShapedLine>,
    origin: Point<Pixels>,
    line_height: Pixels,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
    scroll_top: usize,
    scroll_x: Pixels,
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
        let input = self.input.read(cx);
        let rows = visible_rows(&input.content, input.max_rows);
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

        let ranges = line_ranges(&input.content);
        let rows = visible_rows(&input.content, input.max_rows);
        let cursor_offset = input.cursor_offset();
        let cursor_line = line_at(&ranges, cursor_offset);
        let scroll_top = scroll_top_for(input.scroll_top, cursor_line, rows, ranges.len());
        let visible = || ranges.iter().skip(scroll_top).take(rows);

        let lines: Vec<ShapedLine> = if input.content.is_empty() {
            let placeholder = input.placeholder.clone();
            let runs = [run(placeholder.len(), false, true)];
            vec![
                window
                    .text_system()
                    .shape_line(placeholder, font_size, &runs, None),
            ]
        } else {
            visible()
                .map(|range| {
                    let text: SharedString = input
                        .content
                        .get(range.clone())
                        .unwrap_or_default()
                        .to_owned()
                        .into();
                    let [before, marked, after] = marked_split(range, input.marked_range.as_ref());
                    let runs: Vec<TextRun> = [(before, false), (marked, true), (after, false)]
                        .into_iter()
                        .filter(|(len, _)| *len > 0)
                        .map(|(len, marked)| run(len, marked, false))
                        .collect();
                    window
                        .text_system()
                        .shape_line(text, font_size, &runs, None)
                })
                .collect()
        };

        // Offset → x inside its (visible) line. The placeholder is not text: x is 0 there.
        let x_in = |row: usize, range: &Range<usize>, offset: usize| {
            if input.content.is_empty() {
                return px(0.);
            }
            let in_line = offset.clamp(range.start, range.end) - range.start;
            lines
                .get(row)
                .map_or(px(0.), |line| line.x_for_index(in_line))
        };
        let cursor_row = cursor_line.saturating_sub(scroll_top);
        let cursor_x = ranges
            .get(cursor_line)
            .map_or(px(0.), |range| x_in(cursor_row, range, cursor_offset));
        // Keep the cursor inside the field when a line is wider than it.
        let cursor_width = px(2.);
        let scroll_x = if cursor_x < input.scroll_x {
            cursor_x
        } else if cursor_x > input.scroll_x + bounds.size.width - cursor_width {
            cursor_x - bounds.size.width + cursor_width
        } else {
            input.scroll_x
        };
        let origin = point(bounds.left() - scroll_x, bounds.top());
        let row_top = |row: usize| origin.y + line_height * row as f32;

        let selected = input.selected_range.clone();
        let mut selections = Vec::new();
        let mut cursor = None;
        if selected.is_empty() {
            cursor = Some(fill(
                Bounds::new(
                    point(origin.x + cursor_x, row_top(cursor_row)),
                    size(cursor_width, line_height),
                ),
                gpui::blue(),
            ));
        } else {
            for (row, range) in visible().enumerate() {
                if selected.start > range.end || selected.end < range.start {
                    continue;
                }
                let start = x_in(row, range, selected.start);
                // A selection that runs on to the next line also covers this line's break.
                let line_break = if selected.end > range.end {
                    px(4.)
                } else {
                    px(0.)
                };
                let end = x_in(row, range, selected.end) + line_break;
                selections.push(fill(
                    Bounds::from_corners(
                        point(origin.x + start, row_top(row)),
                        point(origin.x + end, row_top(row) + line_height),
                    ),
                    rgba(0x3311ff30),
                ));
            }
        }

        PrepaintState {
            lines,
            origin,
            line_height,
            cursor,
            selections,
            scroll_top,
            scroll_x,
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
        let (origin, line_height) = (prepaint.origin, prepaint.line_height);
        let selections = std::mem::take(&mut prepaint.selections);
        let cursor = prepaint
            .cursor
            .take()
            .filter(|_| focus_handle.is_focused(window));
        // Lines wider than the field are cut at its edge, not painted over the Send button.
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for selection in selections {
                window.paint_quad(selection);
            }
            for (row, line) in lines.iter().enumerate() {
                let at = point(origin.x, origin.y + line_height * row as f32);
                // A line that fails to paint is skipped: there is nothing to recover.
                let _ = line.paint(at, line_height, window, cx);
            }
            if let Some(cursor) = cursor {
                window.paint_quad(cursor);
            }
        });

        let (scroll_top, scroll_x) = (prepaint.scroll_top, prepaint.scroll_x);
        self.input.update(cx, |input, _cx| {
            input.last_lines = lines;
            input.last_bounds = Some(bounds);
            input.last_line_height = line_height;
            input.scroll_top = scroll_top;
            input.scroll_x = scroll_x;
        });
    }
}

impl Render for TextArea {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
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
            .on_action(cx.listener(Self::submit))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .w_full()
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// The line breaks of a `<textarea>` value: CRLF and a lone CR both read as LF.
fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Byte range of every line, without its line break. Never empty: "" is one empty line.
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

/// `rows={Math.min(6, payload.split('\n').length)}` in the Electron composer.
fn visible_rows(text: &str, max_rows: usize) -> usize {
    (text.matches('\n').count() + 1).min(max_rows.max(1))
}

/// The first visible line that keeps `cursor_line` among the `rows` shown, moving as
/// little as possible from `top`.
fn scroll_top_for(top: usize, cursor_line: usize, rows: usize, lines: usize) -> usize {
    let top = top.min(lines.saturating_sub(rows));
    if cursor_line < top {
        cursor_line
    } else if cursor_line >= top + rows {
        cursor_line + 1 - rows
    } else {
        top
    }
}

/// Where the cursor lands one line up (`delta` < 0) or down. Past the first or last line
/// it goes to the start or end of the text, as in a textarea.
///
/// shortcut: the column is counted in characters, which matches the x position only in a
/// monospace font without wide glyphs; use the shaped lines if the field gets another font.
fn vertical_target(text: &str, cursor: usize, delta: isize) -> usize {
    let ranges = line_ranges(text);
    let line = line_at(&ranges, cursor);
    let Some(target) = line
        .checked_add_signed(delta)
        .and_then(|target| ranges.get(target))
    else {
        return if delta < 0 { 0 } else { text.len() };
    };
    let column = ranges
        .get(line)
        .and_then(|range| text.get(range.start..cursor))
        .map_or(0, |before| before.chars().count());
    let target_text = text.get(target.clone()).unwrap_or_default();
    let in_line = target_text
        .char_indices()
        .nth(column)
        .map_or(target_text.len(), |(index, _)| index);
    target.start + in_line
}

/// How a line's bytes split into [before, inside, after] the text the IME is composing.
fn marked_split(line: &Range<usize>, marked: Option<&Range<usize>>) -> [usize; 3] {
    let len = line.end - line.start;
    let Some(marked) = marked else {
        return [len, 0, 0];
    };
    let start = marked.start.clamp(line.start, line.end) - line.start;
    let end = marked.end.clamp(line.start, line.end) - line.start;
    let end = end.max(start);
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

    use gpui::{Modifiers, TestAppContext, VisualTestContext};

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
    }

    #[test]
    fn 보이는_줄_수는_줄_수만큼_늘다가_최대에서_멈춘다() {
        assert_eq!(visible_rows("", 6), 1);
        assert_eq!(visible_rows("a", 6), 1);
        assert_eq!(visible_rows("a\nb", 6), 2);
        assert_eq!(visible_rows("1\n2\n3\n4\n5\n6", 6), 6);
        assert_eq!(visible_rows("1\n2\n3\n4\n5\n6\n7\n8", 6), 6);
        // 0줄짜리 필드는 없다.
        assert_eq!(visible_rows("a\nb", 0), 1);
    }

    #[test]
    fn 커서가_보이도록_필요한_만큼만_세로로_민다() {
        // 10줄 중 6줄이 보인다.
        assert_eq!(scroll_top_for(0, 3, 6, 10), 0);
        assert_eq!(scroll_top_for(0, 6, 6, 10), 1);
        assert_eq!(scroll_top_for(4, 2, 6, 10), 2);
        assert_eq!(scroll_top_for(0, 9, 6, 10), 4);
        // 줄이 줄어들면 빈 공간이 남지 않게 위로 당긴다.
        assert_eq!(scroll_top_for(4, 2, 3, 3), 0);
        assert_eq!(scroll_top_for(9, 0, 1, 1), 0);
    }

    #[test]
    fn 위아래_이동은_같은_열을_유지하고_짧은_줄에서는_줄_끝에_선다() {
        let text = "abcdef\nxy\n한글입니다";
        // "abcd|ef" 에서 아래로: "xy" 는 짧으므로 줄 끝.
        assert_eq!(vertical_target(text, 4, 1), 9);
        // "xy|" 에서 아래로: 3번째 줄의 2열 = "한글" 뒤(바이트 10 + 6).
        assert_eq!(vertical_target(text, 9, 1), 16);
        // 3번째 줄 2열에서 위로: "xy" 끝.
        assert_eq!(vertical_target(text, 16, -1), 9);
        // "x|y" 에서 위로: 첫 줄의 1열.
        assert_eq!(vertical_target(text, 8, -1), 1);
    }

    #[test]
    fn 첫_줄에서_위로는_글의_처음_마지막_줄에서_아래로는_글의_끝이다() {
        let text = "abc\ndef";
        assert_eq!(vertical_target(text, 2, -1), 0);
        assert_eq!(vertical_target(text, 5, 1), 7);
        assert_eq!(vertical_target("", 0, 1), 0);
        assert_eq!(vertical_target("", 0, -1), 0);
    }

    #[test]
    #[expect(clippy::reversed_empty_ranges, reason = "a reversed range is the input under test")]
    fn 조합_중인_범위는_줄마다_앞_안_뒤로_나뉜다() {
        // 줄 4..10, 조합 범위 6..8.
        assert_eq!(marked_split(&(4..10), Some(&(6..8))), [2, 2, 2]);
        assert_eq!(marked_split(&(4..10), None), [6, 0, 0]);
        // 다른 줄의 조합 범위는 이 줄에 밑줄을 만들지 않는다.
        assert_eq!(marked_split(&(4..10), Some(&(0..2))), [0, 0, 6]);
        assert_eq!(marked_split(&(4..10), Some(&(12..14))), [6, 0, 0]);
        // 줄 밖으로 걸친 범위는 줄 안쪽만.
        assert_eq!(marked_split(&(4..10), Some(&(8..20))), [4, 2, 0]);
        // 뒤집힌 범위가 와도 길이가 음수가 되지 않는다.
        assert_eq!(marked_split(&(4..10), Some(&(8..6))), [4, 0, 2]);
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

    // ── the field in a window: keys, clipboard, mouse, IME ─────────────────

    fn open(cx: &mut TestAppContext) -> (Entity<TextArea>, &mut VisualTestContext) {
        cx.update(kit::bind_keys);
        cx.add_window_view(|window, cx| {
            let area = TextArea::new("placeholder", 6, cx);
            area.focus(window);
            area
        })
    }

    fn text(area: &Entity<TextArea>, cx: &mut VisualTestContext) -> String {
        area.read_with(cx, |area, _| area.text().to_owned())
    }

    fn set(area: &Entity<TextArea>, cx: &mut VisualTestContext, text: &str) {
        area.update(cx, |area, cx| area.set_text(text, cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn 입력한_글자가_들어가고_shift_enter는_줄을_바꾼다(cx: &mut TestAppContext) {
        let (area, cx) = open(cx);
        cx.simulate_input("ab");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("cd");
        assert_eq!(text(&area, cx), "ab\ncd");
        // 줄이 늘면 보이는 줄 수도 는다.
        assert_eq!(area.read_with(cx, |area, _| area.last_lines.len()), 2);
    }

    #[gpui::test]
    fn enter는_줄을_바꾸지_않고_내용을_제출한다(cx: &mut TestAppContext) {
        let (area, cx) = open(cx);
        let submitted = Rc::new(RefCell::new(Vec::new()));
        let seen = submitted.clone();
        cx.update(|_, cx| {
            cx.subscribe(&area, move |_, event: &Submitted, _| {
                seen.borrow_mut().push(event.0.clone())
            })
            .detach()
        });
        cx.simulate_input("ab");
        cx.simulate_keystrokes("enter");
        assert_eq!(*submitted.borrow(), ["ab"]);
        assert_eq!(text(&area, cx), "ab");
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
    fn ctrl_alt_cmd와_함께_누른_enter도_제출한다(cx: &mut TestAppContext) {
        // 원본의 조건은 `e.key === 'Enter' && !e.shiftKey` 뿐이다.
        let (area, cx) = open(cx);
        let submitted = submissions(&area, cx);
        cx.simulate_input("ab");
        cx.simulate_keystrokes("cmd-enter ctrl-enter alt-enter");
        assert_eq!(*submitted.borrow(), ["ab", "ab", "ab"]);
        // 줄은 바뀌지 않는다(preventDefault).
        assert_eq!(text(&area, cx), "ab");
        // shift 가 끼면 제출이 아니다.
        cx.simulate_keystrokes("cmd-shift-enter");
        assert_eq!(submitted.borrow().len(), 3);
    }

    #[gpui::test]
    fn 조합_중의_enter는_제출하지_않는다(cx: &mut TestAppContext) {
        let (area, cx) = open(cx);
        let submitted = submissions(&area, cx);
        area.update_in(cx, |area, window, cx| {
            area.replace_and_mark_text_in_range(None, "한", Some(1..1), window, cx)
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter cmd-enter");
        assert!(submitted.borrow().is_empty());
        assert_eq!(text(&area, cx), "한");
        // 조합이 확정되면 다시 제출된다.
        area.update_in(cx, |area, window, cx| {
            area.replace_text_in_range(None, "한", window, cx)
        });
        cx.simulate_keystrokes("enter");
        assert_eq!(*submitted.borrow(), ["한"]);
    }

    #[gpui::test]
    fn crlf가_든_글은_고치기_전에는_그대로_고치면_lf로_제출된다(
        cx: &mut TestAppContext,
    ) {
        // textarea 는 CRLF·CR 을 LF 로 보여주지만, React 상태는 첫 입력 전까지 받은 문자열 그대로다.
        let (area, cx) = open(cx);
        let submitted = submissions(&area, cx);
        set(&area, cx, "SEND\r\nid:1\rbody");
        assert_eq!(text(&area, cx), "SEND\r\nid:1\rbody");
        // 화면에는 세 줄이다.
        assert_eq!(area.read_with(cx, |area, _| area.last_lines.len()), 3);
        // 커서 이동은 입력이 아니다.
        cx.simulate_keystrokes("up down home end enter");
        assert_eq!(*submitted.borrow(), ["SEND\r\nid:1\rbody"]);
        cx.simulate_input("!");
        assert_eq!(text(&area, cx), "SEND\nid:1\nbody!");
        cx.simulate_keystrokes("enter");
        assert_eq!(
            submitted.borrow().last().map(String::as_str),
            Some("SEND\nid:1\nbody!")
        );
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
        cx.write_to_clipboard(ClipboardItem::new_string("SEND\r\nid:1\n\nbody".into()));
        cx.simulate_keystrokes("cmd-v");
        assert_eq!(text(&area, cx), "SEND\nid:1\n\nbody");
        cx.simulate_keystrokes("cmd-a cmd-x");
        assert_eq!(text(&area, cx), "");
        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(clipboard.as_deref(), Some("SEND\nid:1\n\nbody"));
    }

    #[gpui::test]
    fn 여섯_줄을_넘으면_커서를_따라_세로로_민다(cx: &mut TestAppContext) {
        let (area, cx) = open(cx);
        set(&area, cx, "0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        let view = |cx: &mut VisualTestContext| {
            area.read_with(cx, |area, _| (area.scroll_top, area.last_lines.len()))
        };
        // 커서는 끝(10번째 줄): 마지막 여섯 줄이 보인다.
        assert_eq!(view(cx), (4, 6));
        cx.simulate_keystrokes("up up up up up up up up up");
        assert_eq!(view(cx), (0, 6));
        cx.simulate_keystrokes("down down down down down down");
        assert_eq!(view(cx), (1, 6));
    }

    #[gpui::test]
    fn 누른_자리의_줄로_커서가_간다(cx: &mut TestAppContext) {
        let (area, cx) = open(cx);
        set(&area, cx, "abc\ndef");
        let (bounds, line_height) =
            area.read_with(cx, |area, _| (area.last_bounds, area.last_line_height));
        let bounds = bounds.expect("the field was painted");
        // 둘째 줄의 왼쪽 끝.
        let second_line = point(bounds.left(), bounds.top() + line_height * 1.5);
        cx.simulate_click(second_line, Modifiers::default());
        cx.simulate_input("1");
        assert_eq!(text(&area, cx), "abc\n1def");
        // 첫 줄에서 둘째 줄로 끌면 그 사이가 선택된다.
        let first_line = point(bounds.left(), bounds.top() + line_height * 0.5);
        cx.simulate_mouse_down(first_line, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(second_line, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(second_line, MouseButton::Left, Modifiers::default());
        assert_eq!(
            area.read_with(cx, |area, _| area.selected_range.clone()),
            0..4
        );
        cx.simulate_input("2");
        assert_eq!(text(&area, cx), "21def");
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

    #[test]
    fn 낡은_범위는_글자_경계_안으로_잘라낸다() {
        assert_eq!(clamp_range("", 0..3), 0..0);
        assert_eq!(clamp_range("한", 3..6), 3..3);
        assert_eq!(clamp_range("한글", 1..4), 0..3);
        assert_eq!(clamp_range("abc", 1..2), 1..2);
        assert_eq!(floor_boundary("한글", 5), 3);
        assert_eq!(floor_boundary("한글", 99), 6);
    }
}
