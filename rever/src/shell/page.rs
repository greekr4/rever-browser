//! The page view: paints the active tab's frame and forwards mouse, wheel and keys to
//! the engine. The bot-check button floats over its bottom-left corner.

use std::str::FromStr;

use euclid::Point2D;
use gpui::{
    AnyElement, ClickEvent, Context, Corners, KeyDownEvent, KeyUpEvent, Keystroke, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent,
    Window, canvas, div, prelude::*, px,
};
use rever_core::input::w3c_key_name;
use rever_ui_kit::theme::Theme;
use servo::{
    Code, InputEvent, Key, KeyState, KeyboardEvent, Location, Modifiers, MouseButtonAction,
    MouseButtonEvent, SelectElementOption, SelectElementOptionOrOptgroup, WebViewPoint, WheelDelta,
    WheelEvent, WheelMode,
};

use crate::app::Browser;

impl Browser {
    pub(super) fn page(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = self.tabs[self.active].frame.clone();
        let bounds_cell = self.page_bounds.clone();
        let this = cx.entity();
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
            move |bounds, _, window, _| {
                if let Some(frame) = frame {
                    let _ = window.paint_image(bounds, Corners::default(), frame, 0, false);
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
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, _| {
                this.on_key(KeyState::Down, &event.keystroke)
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
            .child(self.bot_check.clone())
            .children(self.select_list(theme, cx))
    }

    /// The option list of the `<select>` the user opened, under the element.
    ///
    /// shortcut: one choice closes the list, also for `<select multiple>`; keep it
    /// open and collect choices when a page needs several.
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
        // Servo places the element in device pixels of the page.
        let (area, scale) = (select.position(), self.engine.scale());
        let list = div()
            .id("select-list")
            .absolute()
            .left(px(area.min.x as f32 / scale))
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
        let origin = self.page_bounds.get().origin;
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
