//! A multi-line text field (`<textarea>`): soft-wrapped lines, selection, clipboard, IME.
//!
//! Modeled on `rever_ui_kit::text_input` (itself GPUI's `examples/input.rs`, Zed
//! Industries, Apache-2.0), which is single-line. What differs: the text is shaped per
//! hard line and wrapped at the field's width, Enter inserts a line break, Up and Down
//! move between rows, and the field scrolls itself to keep the cursor in view. The
//! parent draws the box and sets font, size and line height.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, KeyBinding, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollHandle, SharedString, Style,
    TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine, actions, div, fill,
    point, prelude::*, px, relative, rgba, size,
};
use unicode_segmentation::UnicodeSegmentation;

actions!(
    text_area,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Home,
        End,
        Newline,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
    ]
);

/// The text as it was last painted: one shaped entry per hard line.
struct AreaLayout {
    lines: Vec<WrappedLine>,
    /// Byte offset of each hard line in the content.
    starts: Vec<usize>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
}

/// The byte ranges of a hard line's visual rows.
fn rows(line: &WrappedLine) -> Vec<Range<usize>> {
    let mut out = Vec::with_capacity(line.wrap_boundaries().len() + 1);
    let mut start = 0;
    for boundary in line.wrap_boundaries() {
        let end = line
            .runs()
            .get(boundary.run_ix)
            .and_then(|run| run.glyphs.get(boundary.glyph_ix))
            .map_or(line.len(), |glyph| glyph.index);
        out.push(start..end);
        start = end;
    }
    out.push(start..line.len());
    out
}

impl AreaLayout {
    fn line_top(&self, line: usize) -> Pixels {
        let rows_above: usize = self
            .lines
            .iter()
            .take(line)
            .map(|line| line.wrap_boundaries().len() + 1)
            .sum();
        self.line_height * rows_above as f32
    }

    /// The hard line holding byte `index`.
    fn line_of(&self, index: usize) -> usize {
        self.starts
            .partition_point(|start| *start <= index)
            .saturating_sub(1)
    }

    /// Where byte `index` is, relative to the field's top left.
    fn position(&self, index: usize) -> Point<Pixels> {
        let line = self.line_of(index);
        let (Some(shaped), Some(start)) = (self.lines.get(line), self.starts.get(line)) else {
            return Point::default();
        };
        let local = index.saturating_sub(*start).min(shaped.len());
        let at = shaped
            .position_for_index(local, self.line_height)
            .unwrap_or_default();
        point(at.x, at.y + self.line_top(line))
    }

    /// The byte nearest to a point relative to the field's top left.
    fn index(&self, at: Point<Pixels>) -> usize {
        if at.y < px(0.0) {
            return 0;
        }
        let mut top = px(0.0);
        for (line, start) in self.lines.iter().zip(&self.starts) {
            let height = self.line_height * (line.wrap_boundaries().len() + 1) as f32;
            if at.y < top + height {
                let local = point(at.x.max(px(0.0)), at.y - top);
                // Ok and Err both carry the nearest index.
                let index = line
                    .closest_index_for_position(local, self.line_height)
                    .unwrap_or_else(|index| index);
                return start + index.min(line.len());
            }
            top += height;
        }
        self.starts
            .last()
            .zip(self.lines.last())
            .map_or(0, |(start, line)| start + line.len())
    }
}

pub struct TextArea {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    padding: f32,
    scroll: ScrollHandle,
    layout: Option<AreaLayout>,
    is_selecting: bool,
    /// Set by an edit or a cursor move: the next paint scrolls the cursor into view.
    reveal_cursor: bool,
}

impl TextArea {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            padding: 0.0,
            scroll: ScrollHandle::new(),
            layout: None,
            is_selecting: false,
            reveal_cursor: false,
        }
    }

    /// The CSS `padding` of the textarea: it scrolls with the text.
    pub fn padding(mut self, padding: f32) -> Self {
        self.padding = padding;
        self
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    /// Replace the text and put the cursor at the end.
    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.content = text.into();
        self.selected_range = self.content.len()..self.content.len();
        self.marked_range = None;
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

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    /// The byte one row above (-1) or below (+1) the cursor, at the same x.
    fn vertical(&self, rows: f32) -> usize {
        let Some(layout) = &self.layout else {
            return self.cursor_offset();
        };
        let at = layout.position(self.cursor_offset());
        let target = at.y + layout.line_height * rows;
        if target < px(0.0) {
            return 0;
        }
        layout.index(point(at.x, target + layout.line_height / 2.0))
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.vertical(-1.0), cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.vertical(1.0), cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.vertical(-1.0), cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.vertical(1.0), cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    /// Start of the hard line the cursor is on.
    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let before = self.content.get(..self.cursor_offset()).unwrap_or_default();
        self.move_to(before.rfind('\n').map_or(0, |at| at + 1), cx);
    }

    /// End of the hard line the cursor is on.
    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let after = self.content.get(cursor..).unwrap_or_default();
        self.move_to(cursor + after.find('\n').unwrap_or(after.len()), cx);
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
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
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            // The shaper splits on \n only; a lone \r would be drawn as a box.
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn selected_text(&self) -> Option<String> {
        self.content
            .get(self.selected_range.clone())
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
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

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let Some(layout) = &self.layout else {
            return 0;
        };
        layout.index(position - layout.bounds.origin)
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
    fn edit_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        // A stale range must never reach the slicing: that is a crash.
        clamp_range(&self.content, range)
    }

    fn splice(&mut self, range: &Range<usize>, new_text: &str) {
        let before = self.content.get(..range.start).unwrap_or_default();
        let after = self.content.get(range.end..).unwrap_or_default();
        self.content = format!("{before}{new_text}{after}").into();
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
        let range = self.edit_range(range_utf16);
        self.splice(&range, new_text);
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
        self.reveal_cursor = true;
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
        let range = self.edit_range(range_utf16);
        self.splice(&range, new_text);
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selected_range = marked_selection(range.start, new_text, new_selected_range_utf16);
        self.reveal_cursor = true;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let layout = self.layout.as_ref()?;
        let start = layout.bounds.origin + layout.position(range.start);
        Some(Bounds::new(start, size(px(1.0), layout.line_height)))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let index = layout.index(point - layout.bounds.origin);
        Some(self.offset_to_utf16(index))
    }
}

struct TextAreaElement {
    input: Entity<TextArea>,
}

struct PrepaintState {
    lines: Vec<WrappedLine>,
    starts: Vec<usize>,
    cursor: Option<Bounds<Pixels>>,
    selection: Vec<Bounds<Pixels>>,
}

impl IntoElement for TextAreaElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The text to shape and its style: the placeholder, dimmed, while the field is empty.
fn display(input: &TextArea, window: &Window) -> (SharedString, Vec<TextRun>, Pixels) {
    let style = window.text_style();
    let (text, color) = if input.content.is_empty() {
        // A dimmed version of the text color, so it reads on light and dark backgrounds.
        (input.placeholder.clone(), style.color.opacity(0.45))
    } else {
        (input.content.clone(), style.color)
    };
    let run = TextRun {
        len: text.len(),
        font: style.font(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let runs = match input.marked_range.as_ref() {
        Some(marked) if !input.content.is_empty() && marked.end <= text.len() => vec![
            TextRun {
                len: marked.start,
                ..run.clone()
            },
            TextRun {
                len: marked.end - marked.start,
                underline: Some(UnderlineStyle {
                    color: Some(run.color),
                    thickness: px(1.0),
                    wavy: false,
                }),
                ..run.clone()
            },
            TextRun {
                len: text.len() - marked.end,
                ..run
            },
        ]
        .into_iter()
        .filter(|run| run.len > 0)
        .collect(),
        _ => vec![run],
    };
    (text, runs, style.font_size.to_pixels(window.rem_size()))
}

fn shape(
    text: SharedString,
    runs: &[TextRun],
    font_size: Pixels,
    width: Option<Pixels>,
    window: &Window,
) -> Vec<WrappedLine> {
    window
        .text_system()
        .shape_text(text, font_size, runs, width, None)
        .map(|lines| lines.into_vec())
        .unwrap_or_default()
}

fn row_count(lines: &[WrappedLine]) -> usize {
    lines
        .iter()
        .map(|line| line.wrap_boundaries().len() + 1)
        .sum::<usize>()
        .max(1)
}

impl Element for TextAreaElement {
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
        let (text, runs, font_size) = display(self.input.read(cx), window);
        let line_height = window.line_height();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        // As tall as the wrapped text: the height depends on the width layout gives.
        let layout = window.request_measured_layout(style, move |known, available, window, _| {
            let width = known.width.or(match available.width {
                gpui::AvailableSpace::Definite(width) => Some(width),
                _ => None,
            });
            let lines = shape(text.clone(), &runs, font_size, width, window);
            size(
                width.unwrap_or_default(),
                line_height * row_count(&lines) as f32,
            )
        });
        (layout, ())
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
        let (text, runs, font_size) = display(input, window);
        let line_height = window.line_height();
        let lines = shape(
            text.clone(),
            &runs,
            font_size,
            Some(bounds.size.width),
            window,
        );
        let mut starts = Vec::with_capacity(lines.len());
        let mut start = 0;
        for line in &lines {
            starts.push(start);
            start += line.len() + 1;
        }
        let layout = AreaLayout {
            lines,
            starts,
            bounds,
            line_height,
        };

        // The placeholder is not content: the cursor sits at the origin and nothing is selected.
        let (selected, cursor) = if input.content.is_empty() {
            (0..0, 0)
        } else {
            (input.selected_range.clone(), input.cursor_offset())
        };
        let mut selection = Vec::new();
        if !selected.is_empty() {
            for (index, (line, start)) in layout.lines.iter().zip(&layout.starts).enumerate() {
                let top = layout.line_top(index);
                for (row, range) in rows(line).into_iter().enumerate() {
                    let from = selected.start.max(start + range.start);
                    let to = selected.end.min(start + range.end);
                    if from >= to {
                        continue;
                    }
                    // A row's first byte is also the previous row's end, which is where
                    // `position_for_index` puts it; the row itself starts at x = 0.
                    let x = |at: usize| {
                        if at == start + range.start {
                            px(0.0)
                        } else {
                            line.position_for_index(at - start, line_height)
                                .map_or(px(0.0), |at| at.x)
                        }
                    };
                    let y = top + line_height * row as f32;
                    selection.push(Bounds::from_corners(
                        bounds.origin + point(x(from), y),
                        bounds.origin + point(x(to), y + line_height),
                    ));
                }
            }
        }
        let cursor = selected.is_empty().then(|| {
            Bounds::new(
                bounds.origin + layout.position(cursor),
                size(px(2.0), line_height),
            )
        });
        PrepaintState {
            lines: layout.lines,
            starts: layout.starts,
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
            window.paint_quad(fill(quad, rgba(0x3311ff30)));
        }
        let line_height = window.line_height();
        let mut top = bounds.top();
        for line in &prepaint.lines {
            // A line that fails to paint is skipped; the rest of the text still shows.
            let _ = line.paint(
                point(bounds.left(), top),
                line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
            top += line_height * (line.wrap_boundaries().len() + 1) as f32;
        }
        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor
        {
            window.paint_quad(fill(cursor, gpui::blue()));
        }

        let lines = std::mem::take(&mut prepaint.lines);
        let starts = std::mem::take(&mut prepaint.starts);
        let cursor = prepaint.cursor;
        let scrolled = self.input.update(cx, |input, _| {
            input.layout = Some(AreaLayout {
                lines,
                starts,
                bounds,
                line_height,
            });
            if !std::mem::take(&mut input.reveal_cursor) {
                return false;
            }
            let (Some(cursor), view) = (cursor, input.scroll.bounds()) else {
                return false;
            };
            if view.size.height <= px(0.0) {
                return false;
            }
            let pad = px(input.padding);
            let offset = input.scroll.offset();
            let shift = if cursor.top() < view.top() + pad {
                view.top() + pad - cursor.top()
            } else if cursor.bottom() > view.bottom() - pad {
                view.bottom() - pad - cursor.bottom()
            } else {
                return false;
            };
            input.scroll.set_offset(point(offset.x, offset.y + shift));
            true
        });
        if scrolled {
            // This frame was laid out at the old offset.
            window.refresh();
        }
    }
}

impl Render for TextArea {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("text-area")
            .key_context("TextArea")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p(px(self.padding))
            .child(TextAreaElement { input: cx.entity() })
    }
}

impl Focusable for TextArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
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

/// Editing keys for every [`TextArea`]. Scoped to the field's key context, so they do
/// not take keys away from the page.
pub fn bind_keys(cx: &mut App) {
    let context = Some("TextArea");
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("left", Left, context),
        KeyBinding::new("right", Right, context),
        KeyBinding::new("up", Up, context),
        KeyBinding::new("down", Down, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("shift-up", SelectUp, context),
        KeyBinding::new("shift-down", SelectDown, context),
        KeyBinding::new("cmd-a", SelectAll, context),
        KeyBinding::new("cmd-v", Paste, context),
        KeyBinding::new("cmd-c", Copy, context),
        KeyBinding::new("cmd-x", Cut, context),
        KeyBinding::new("home", Home, context),
        KeyBinding::new("end", End, context),
        KeyBinding::new("enter", Newline, context),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, context),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

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
        assert_eq!(clamp_range("a\nb", 1..2), 1..2);
    }

    struct Host {
        area: Entity<TextArea>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(200.0))
                .h(px(60.0))
                .text_size(px(12.0))
                .line_height(px(16.0))
                .child(self.area.clone())
        }
    }

    fn open<'a>(
        text: &str,
        cx: &'a mut TestAppContext,
    ) -> (Entity<TextArea>, &'a mut VisualTestContext) {
        cx.update(bind_keys);
        let text = text.to_owned();
        let (host, cx) = cx.add_window_view(move |_, cx| Host {
            area: cx.new(|cx| {
                let mut area = TextArea::new("placeholder", cx);
                area.set_text(text, cx);
                area
            }),
        });
        let area = host.read_with(cx, |host, _| host.area.clone());
        area.update_in(cx, |area, window, cx| {
            window.focus(&area.focus_handle(cx));
        });
        cx.run_until_parked();
        (area, cx)
    }

    fn text(area: &Entity<TextArea>, cx: &mut VisualTestContext) -> String {
        area.read_with(cx, |area, _| area.text().to_owned())
    }

    #[gpui::test]
    fn 엔터는_줄을_바꾸고_입력은_커서_자리에_들어간다(cx: &mut TestAppContext) {
        let (area, cx) = open("ab", cx);
        cx.simulate_keystrokes("enter");
        cx.simulate_input("c");
        assert_eq!(text(&area, cx), "ab\nc");
        cx.simulate_keystrokes("left left left");
        cx.simulate_input("X");
        assert_eq!(text(&area, cx), "aXb\nc");
    }

    #[gpui::test]
    fn 위아래_키는_줄_사이를_오간다(cx: &mut TestAppContext) {
        let (area, cx) = open("abc\ndef\nghi", cx);
        // 커서는 끝(둘째 줄 아래, "ghi" 뒤)에 있다.
        cx.simulate_keystrokes("up");
        cx.simulate_input("1");
        assert_eq!(text(&area, cx), "abc\ndef1\nghi");
        cx.simulate_keystrokes("up up");
        cx.simulate_input("2");
        // 첫 줄에서 더 올라가면 맨 앞이다.
        assert_eq!(text(&area, cx), "2abc\ndef1\nghi");
        cx.simulate_keystrokes("down down down");
        cx.simulate_input("3");
        assert_eq!(text(&area, cx), "2abc\ndef1\nghi3");
    }

    #[gpui::test]
    fn home과_end는_지금_줄의_처음과_끝으로_간다(cx: &mut TestAppContext) {
        let (area, cx) = open("abc\ndef", cx);
        cx.simulate_keystrokes("home");
        cx.simulate_input("<");
        cx.simulate_keystrokes("end");
        cx.simulate_input(">");
        assert_eq!(text(&area, cx), "abc\n<def>");
    }

    #[gpui::test]
    fn 지우기와_전체_선택이_여러_줄에_걸쳐_동작한다(cx: &mut TestAppContext) {
        let (area, cx) = open("가\n나", cx);
        cx.simulate_keystrokes("backspace backspace");
        assert_eq!(text(&area, cx), "가");
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("x\ny");
        assert_eq!(text(&area, cx), "x\ny");
        cx.simulate_keystrokes("cmd-a backspace");
        assert_eq!(text(&area, cx), "");
        // 빈 필드에서 지워도 아무 일이 없다.
        cx.simulate_keystrokes("backspace delete up down");
        assert_eq!(text(&area, cx), "");
    }

    #[gpui::test]
    fn 붙여넣기는_줄바꿈을_지킨다(cx: &mut TestAppContext) {
        let (area, cx) = open("", cx);
        cx.write_to_clipboard(ClipboardItem::new_string("a\r\nb\rc".into()));
        cx.simulate_keystrokes("cmd-v");
        assert_eq!(text(&area, cx), "a\nb\nc");
        cx.simulate_keystrokes("cmd-a cmd-x");
        assert_eq!(text(&area, cx), "");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("a\nb\nc".to_owned())
        );
    }

    #[gpui::test]
    fn 긴_줄은_너비에_맞춰_접히고_높이가_늘어난다(cx: &mut TestAppContext) {
        let (area, cx) = open("one", cx);
        let rows_of = |area: &Entity<TextArea>, cx: &mut VisualTestContext| {
            area.read_with(cx, |area, _| {
                area.layout.as_ref().map(|layout| row_count(&layout.lines))
            })
        };
        assert_eq!(rows_of(&area, cx), Some(1));
        area.update(cx, |area, cx| {
            area.set_text("word ".repeat(40), cx);
        });
        cx.run_until_parked();
        assert!(rows_of(&area, cx).unwrap() > 3);
    }
}
