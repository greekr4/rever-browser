//! The page view: paints the active tab's frame and forwards mouse, wheel and keys to
//! the engine. The bot-check button floats over its bottom-left corner.

use std::str::FromStr;

use euclid::Point2D;
use gpui::{
    Context, Corners, KeyDownEvent, KeyUpEvent, Keystroke, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, Window, canvas,
    div, prelude::*,
};
use rever_core::input::w3c_key_name;
use rever_ui_kit::theme::Theme;
use servo::{
    Code, InputEvent, Key, KeyState, KeyboardEvent, Location, Modifiers, MouseButtonAction,
    MouseButtonEvent, WebViewPoint, WheelDelta, WheelEvent, WheelMode,
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
        _: &mut Context<Self>,
    ) {
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
