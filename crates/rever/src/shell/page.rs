//! The page view: paints the active tab's frame and forwards mouse, wheel and keys to
//! the engine. The bot-check button floats over its bottom-left corner.

use std::str::FromStr;

use euclid::Point2D;
use std::ops::Range;

use gpui::{
    AnyElement, Bounds, ClickEvent, Context, Corners, CursorStyle, ElementInputHandler,
    EntityInputHandler, KeyDownEvent, KeyUpEvent, Keystroke, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, UTF16Selection,
    Window, canvas, div, prelude::*, px,
};
use rever_core::input::w3c_key_name;
use rever_ui_kit::theme::Theme;
use servo::{
    Code, CompositionEvent, CompositionState, ContextMenuItem, Cursor, ImeEvent, InputEvent, Key,
    KeyState, KeyboardEvent, Location, Modifiers, MouseButtonAction, MouseButtonEvent,
    SelectElementOption, SelectElementOptionOrOptgroup, WebViewPoint, WheelDelta, WheelEvent,
    WheelMode,
};

use crate::app::{Browser, page_area};

impl Browser {
    pub(super) fn page(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = self.tabs[self.active].frame.clone();
        let bounds_cell = self.page_bounds.clone();
        let this = cx.entity();
        let (input, focus) = (cx.entity(), self.page_focus.clone());
        let mobile = self.mobile;
        let painted = canvas(
            move |bounds, window, _| {
                // The engine is resized after this frame: entities cannot be updated mid-paint.
                if bounds_cell.get() != bounds {
                    bounds_cell.set(bounds);
                    window.on_next_frame(move |window, cx| {
                        this.update(cx, |this, cx| this.fit_page(window, cx));
                    });
                }
            },
            move |bounds, _, window, cx| {
                // Composed text (Korean, Japanese, Chinese) reaches the page through this.
                window.handle_input(&focus, ElementInputHandler::new(bounds, input), cx);
                if let Some(frame) = frame {
                    let area = page_area(bounds, mobile);
                    let _ = window.paint_image(area, Corners::default(), frame, 0, false);
                }
            },
        )
        .size_full();

        let page = div()
            .id("page")
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(theme.viewport_bg)
            .track_focus(&self.page_focus)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .cursor(cursor_style(self.engine.cursor(self.active_id())))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                this.on_key(KeyState::Down, &event.keystroke);
                // Handled: otherwise the system types the same character again
                // through the input handler.
                cx.stop_propagation();
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, _| {
                this.on_key(KeyState::Up, &event.keystroke)
            }))
            .child(painted);
        // The button pins itself to the bottom-left of this box.
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .flex()
            .flex_col()
            .child(page)
            .children(self.select_list(theme, cx))
            .children(self.context_menu_list(theme, cx))
            .children(self.prompt_overlay(theme, cx))
            .children(self.color_overlay(theme, cx))
            .children(self.memory_overlay(theme))
    }

    /// The development build's memory readout, in the page's bottom-left corner. It
    /// takes no clicks: they go through to the page.
    fn memory_overlay(&self, theme: &Theme) -> Option<AnyElement> {
        use crate::memory::megabytes;
        let now = self.memory.latest()?;
        let line = |label: &'static str, bytes: u64| {
            div()
                .flex()
                .justify_between()
                .gap(px(10.0))
                .child(self.lang.t(label))
                .child(megabytes(bytes))
        };
        let graph =
            div()
                .h(px(22.0))
                .flex()
                .items_end()
                .children(self.memory.bars().into_iter().map(|share| {
                    div()
                        .w(px(1.0))
                        .h(px((share * 22.0).max(1.0)))
                        .bg(theme.accent)
                }));
        Some(
            div()
                .absolute()
                .left(px(10.0))
                .bottom(px(10.0))
                .w(px(132.0))
                .p(px(6.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .rounded(px(6.0))
                .bg(theme.bg)
                .border_1()
                .border_color(theme.border)
                .opacity(0.9)
                .text_size(px(11.0))
                .text_color(theme.text_dim)
                .child(line("dev.memory", now))
                .child(line("dev.memory.peak", self.memory.peak()))
                .child(graph)
                .into_any_element(),
        )
    }

    /// The `<input type=color>` overlay: a `#rrggbb` field (Servo gives no native
    /// picker here). Backdrop/Cancel submits the color unchanged, OK/Enter the typed one.
    fn color_overlay(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let overlay = self.color_picker.as_ref()?;
        let input = overlay.input.clone();
        let button = |label: &'static str, accept: bool, primary: bool, cx: &mut Context<Self>| {
            let (bg, fg) = if primary {
                (theme.accent, theme.bg)
            } else {
                (theme.surface_3, theme.text)
            };
            div()
                .id(label)
                .px(px(12.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .bg(bg)
                .text_color(fg)
                .cursor_pointer()
                .child(label)
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.submit_color(accept, cx)),
                )
        };
        let modal = div()
            .occlude()
            .min_w(px(260.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .p(px(16.0))
            .bg(theme.bg)
            .border_1()
            .border_color(theme.border)
            .rounded(px(8.0))
            .shadow_md()
            .text_size(px(13.0))
            .child(div().text_color(theme.text).child("Pick a color (#rrggbb)"))
            .child(input)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(button("Cancel", false, false, cx))
                    .child(button("OK", true, true, cx)),
            );
        let backdrop = div()
            .id("color-backdrop")
            .absolute()
            .size_full()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.3))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.submit_color(false, cx)))
            .child(modal);
        Some(backdrop.into_any_element())
    }

    /// The right-click context menu, drawn at the position Servo gave it.
    fn context_menu_list(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.context_menu.as_ref()?;
        let (hover, dim, text) = (theme.chrome_hover, theme.text_dim, theme.text);
        let mut rows = Vec::new();
        for (index, item) in menu.items().iter().enumerate() {
            match item {
                ContextMenuItem::Separator => rows.push(
                    div()
                        .my(px(4.0))
                        .h(px(1.0))
                        .bg(theme.border_2)
                        .into_any_element(),
                ),
                ContextMenuItem::Item {
                    label,
                    action,
                    enabled,
                } => {
                    let action = *action;
                    let enabled = *enabled;
                    rows.push(
                        div()
                            .id(("context-item", index))
                            .px(px(12.0))
                            .py(px(4.0))
                            .text_color(if enabled { text } else { dim })
                            .child(label.clone())
                            .when(enabled, |row| {
                                row.hover(move |style| style.bg(hover))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.choose_context(Some(action), cx)
                                    }))
                            })
                            .into_any_element(),
                    );
                }
            }
        }
        let bounds = self.page_bounds.get();
        let column = page_area(bounds, self.mobile).origin.x - bounds.origin.x;
        let (area, scale) = (menu.position(), self.engine.scale());
        let list = div()
            .id("context-menu")
            .absolute()
            .left(px(area.min.x as f32 / scale) + column)
            .top(px(area.min.y as f32 / scale))
            .min_w(px(160.0))
            .occlude()
            .bg(theme.bg)
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .shadow_md()
            .py(px(4.0))
            .text_size(px(13.0))
            .children(rows);
        Some(list.into_any_element())
    }

    /// The text-input overlay of an open `prompt()` (the system dialog can't take
    /// typed input). A click on the backdrop, or Cancel, dismisses it; Enter in
    /// the field (or OK) confirms with the typed text.
    fn prompt_overlay(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let overlay = self.prompt.as_ref()?;
        let button = |label: &'static str, accept: bool, primary: bool, cx: &mut Context<Self>| {
            let (bg, fg) = if primary {
                (theme.accent, theme.bg)
            } else {
                (theme.surface_3, theme.text)
            };
            div()
                .id(label)
                .px(px(12.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .bg(bg)
                .text_color(fg)
                .cursor_pointer()
                .child(label)
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.confirm_prompt(accept, cx)),
                )
        };
        let modal = div()
            .occlude()
            .min_w(px(320.0))
            .max_w(px(520.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .p(px(16.0))
            .bg(theme.bg)
            .border_1()
            .border_color(theme.border)
            .rounded(px(8.0))
            .shadow_md()
            .text_size(px(13.0))
            .child(div().text_color(theme.text).child(overlay.message.clone()))
            .child(overlay.input.clone())
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(button("Cancel", false, false, cx))
                    .child(button("OK", true, true, cx)),
            );
        let backdrop = div()
            .id("prompt-backdrop")
            .absolute()
            .size_full()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.3))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.confirm_prompt(false, cx)))
            .child(modal);
        Some(backdrop.into_any_element())
    }

    /// The option list of the `<select>` the user opened, under the element. For
    /// `<select multiple>` a click toggles and the list stays open (an outside
    /// press submits the set); a single select closes on the first choice.
    fn select_list(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let select = self.select.as_ref()?;
        let chosen = select.selected_options();
        let (hover, accent, dim, text) =
            (theme.chrome_hover, theme.accent, theme.text_dim, theme.text);
        let row = |option: &SelectElementOption, indent: f32, cx: &mut Context<Self>| {
            let id = option.id;
            let color = match (option.is_disabled, chosen.contains(&id)) {
                (true, _) => dim,
                (false, true) => accent,
                (false, false) => text,
            };
            div()
                .id(("select-option", id))
                .pl(px(10.0 + indent))
                .pr(px(10.0))
                .py(px(4.0))
                .text_color(color)
                .child(option.label.clone())
                .when(!option.is_disabled, |row| {
                    row.hover(move |style| style.bg(hover))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.choose_option(Some(id), cx)
                        }))
                })
                .into_any_element()
        };
        let mut rows = Vec::new();
        for entry in select.options() {
            match entry {
                SelectElementOptionOrOptgroup::Option(option) => rows.push(row(option, 0.0, cx)),
                SelectElementOptionOrOptgroup::Optgroup { label, options } => {
                    let heading = div().px(px(10.0)).py(px(4.0)).text_color(dim);
                    rows.push(heading.child(label.clone()).into_any_element());
                    rows.extend(options.iter().map(|option| row(option, 12.0, cx)));
                }
            }
        }
        // In mobile mode the page is a column that starts right of the area's edge.
        let bounds = self.page_bounds.get();
        let column = page_area(bounds, self.mobile).origin.x - bounds.origin.x;
        // Servo places the element in device pixels of the page.
        let (area, scale) = (select.position(), self.engine.scale());
        let list = div()
            .id("select-list")
            .absolute()
            .left(px(area.min.x as f32 / scale) + column)
            .top(px(area.max.y as f32 / scale))
            .min_w(px(area.width() as f32 / scale))
            .max_h(px(320.0))
            .overflow_y_scroll()
            .occlude()
            .bg(theme.bg)
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .shadow_md()
            .py(px(4.0))
            .text_size(px(13.0))
            .children(rows);
        Some(list.into_any_element())
    }

    fn page_point(&self, position: Point<Pixels>) -> WebViewPoint {
        let origin = page_area(self.page_bounds.get(), self.mobile).origin;
        WebViewPoint::Page(Point2D::new(
            f32::from(position.x - origin.x),
            f32::from(position.y - origin.y),
        ))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A press outside an open option list closes it, as a native list does.
        if self.select.is_some() {
            return self.choose_option(None, cx);
        }
        // Same for a context menu (its own items occlude, so this is an outside press).
        if self.context_menu.is_some() {
            return self.choose_context(None, cx);
        }
        window.focus(&self.page_focus);
        let point = self.page_point(event.position);
        let tab = self.active_id();
        self.engine.input(
            tab,
            InputEvent::MouseMove(servo::MouseMoveEvent::new(point)),
        );
        self.engine.input(
            tab,
            InputEvent::MouseButton(MouseButtonEvent::new(
                MouseButtonAction::Down,
                servo::MouseButton::Primary,
                point,
            )),
        );
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.engine.input(
            self.active_id(),
            InputEvent::MouseButton(MouseButtonEvent::new(
                MouseButtonAction::Up,
                servo::MouseButton::Primary,
                self.page_point(event.position),
            )),
        );
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        let point = self.page_point(event.position);
        self.engine.input(
            self.active_id(),
            InputEvent::MouseMove(servo::MouseMoveEvent::new(point)),
        );
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, _: &mut Context<Self>) {
        // Both sides use "positive scrolls up/left"; Servo's pixel deltas are device pixels.
        let scale = f64::from(window.scale_factor());
        let (x, y, mode) = match event.delta {
            ScrollDelta::Pixels(delta) => (
                f64::from(f32::from(delta.x)) * scale,
                f64::from(f32::from(delta.y)) * scale,
                WheelMode::DeltaPixel,
            ),
            ScrollDelta::Lines(delta) => {
                (f64::from(delta.x), f64::from(delta.y), WheelMode::DeltaLine)
            }
        };
        self.engine.input(
            self.active_id(),
            InputEvent::Wheel(WheelEvent::new(
                WheelDelta { x, y, z: 0.0, mode },
                self.page_point(event.position),
            )),
        );
    }

    fn on_key(&mut self, state: KeyState, keystroke: &Keystroke) {
        let shortcut = keystroke.modifiers.platform || keystroke.modifiers.control;
        let key = match (w3c_key_name(&keystroke.key), keystroke.key_char.as_deref()) {
            (Some(name), _) => Key::from_str(name).ok(),
            (None, Some(text)) if !shortcut => Some(Key::Character(text.to_owned())),
            (None, _) if keystroke.key == "space" => Some(Key::Character(" ".into())),
            (None, _) => Some(Key::Character(keystroke.key.clone())),
        };
        let Some(key) = key else { return };
        let mut modifiers = Modifiers::empty();
        modifiers.set(Modifiers::SHIFT, keystroke.modifiers.shift);
        modifiers.set(Modifiers::CONTROL, keystroke.modifiers.control);
        modifiers.set(Modifiers::ALT, keystroke.modifiers.alt);
        modifiers.set(Modifiers::META, keystroke.modifiers.platform);
        self.engine.input(
            self.active_id(),
            InputEvent::Keyboard(KeyboardEvent::new_without_event(
                state,
                key,
                Code::Unidentified,
                Location::Standard,
                modifiers,
                false,
                false,
            )),
        );
    }
}

/// The input method's side of the page. The page owns its text, so nothing can be
/// read back: the handler only reports whether a composition is open and forwards
/// what the input method composes and commits.
impl EntityInputHandler for Browser {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.composing.as_deref().map_or(0, utf16_len);
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.composing.as_deref().map(|text| 0..utf16_len(text))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        // The input method gave up the composition: what was composed stays.
        if let Some(text) = self.composing.take() {
            self.compose(CompositionState::End, &text);
        }
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        if self.composing.take().is_some() {
            return self.compose(CompositionState::End, text);
        }
        // Text the input method inserts without composing (an emoji, a dictated word).
        let tab = self.active_id();
        for ch in text.chars() {
            for state in [KeyState::Down, KeyState::Up] {
                let key = Key::Character(ch.to_string());
                let event = KeyboardEvent::from_state_and_key(state, key);
                self.engine.input(tab, InputEvent::Keyboard(event));
            }
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        if self.composing.is_none() {
            self.compose(CompositionState::Start, "");
        }
        if text.is_empty() {
            // Everything composed was deleted: the composition ends with nothing.
            self.composing = None;
            return self.compose(CompositionState::End, "");
        }
        self.compose(CompositionState::Update, text);
        self.composing = Some(text.to_owned());
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // shortcut: the candidate window sits at the page's corner, since the caret's
        // place is the page's to know; ask the engine for it when that bothers.
        Some(bounds)
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Browser {
    fn compose(&self, state: CompositionState, data: &str) {
        let event = CompositionEvent {
            state,
            data: data.to_owned(),
        };
        self.engine.input(
            self.active_id(),
            InputEvent::Ime(ImeEvent::Composition(event)),
        );
    }
}

/// The system cursor for the one a page asks for. The ones macOS has no shape for
/// (help, wait, zoom, ...) stay the arrow.
fn cursor_style(cursor: Cursor) -> CursorStyle {
    match cursor {
        Cursor::None => CursorStyle::None,
        Cursor::Pointer => CursorStyle::PointingHand,
        Cursor::ContextMenu => CursorStyle::ContextualMenu,
        Cursor::Text => CursorStyle::IBeam,
        Cursor::VerticalText => CursorStyle::IBeamCursorForVerticalLayout,
        Cursor::Crosshair => CursorStyle::Crosshair,
        Cursor::Alias => CursorStyle::DragLink,
        Cursor::Copy => CursorStyle::DragCopy,
        Cursor::NoDrop | Cursor::NotAllowed => CursorStyle::OperationNotAllowed,
        Cursor::Grab => CursorStyle::OpenHand,
        Cursor::Grabbing => CursorStyle::ClosedHand,
        Cursor::EResize => CursorStyle::ResizeRight,
        Cursor::WResize => CursorStyle::ResizeLeft,
        Cursor::NResize => CursorStyle::ResizeUp,
        Cursor::SResize => CursorStyle::ResizeDown,
        Cursor::EwResize => CursorStyle::ResizeLeftRight,
        Cursor::NsResize => CursorStyle::ResizeUpDown,
        Cursor::ColResize => CursorStyle::ResizeColumn,
        Cursor::RowResize => CursorStyle::ResizeRow,
        Cursor::NeResize | Cursor::SwResize | Cursor::NeswResize => {
            CursorStyle::ResizeUpRightDownLeft
        }
        Cursor::NwResize | Cursor::SeResize | Cursor::NwseResize => {
            CursorStyle::ResizeUpLeftDownRight
        }
        _ => CursorStyle::Arrow,
    }
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 페이지가_요청한_커서는_시스템_커서로_바뀐다() {
        assert_eq!(cursor_style(Cursor::Pointer), CursorStyle::PointingHand);
        assert_eq!(cursor_style(Cursor::Text), CursorStyle::IBeam);
        assert_eq!(cursor_style(Cursor::Default), CursorStyle::Arrow);
        assert_eq!(
            cursor_style(Cursor::NotAllowed),
            CursorStyle::OperationNotAllowed
        );
        // macOS에 모양이 없는 커서는 화살표로 둔다.
        assert_eq!(cursor_style(Cursor::Wait), CursorStyle::Arrow);
    }
}
