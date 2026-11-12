// Input conversion: CDP `Input.*` params and winit window events → Servo input events.

use std::str::FromStr;

use euclid::Point2D;
use serde_json::Value;
use servo::{
    Code, DevicePoint, InputEvent, Key, KeyState, KeyboardEvent, Location, Modifiers, MouseButton,
    MouseButtonAction, MouseButtonEvent, MouseMoveEvent, WebViewPoint, WheelDelta, WheelEvent,
    WheelMode,
};
use winit::event::{ElementState, MouseScrollDelta};
use winit::keyboard::{Key as WinitKey, ModifiersState};

/// `Input.dispatchMouseEvent` params → Servo input events. CDP coordinates and
/// wheel deltas are CSS pixels; Servo's pixel wheel deltas are device pixels.
pub fn cdp_mouse(params: &Value, scale: f64) -> Vec<InputEvent> {
    let number = |name: &str| params[name].as_f64().unwrap_or_default();
    let point = WebViewPoint::Page(Point2D::new(number("x") as f32, number("y") as f32));
    let button = match params["button"].as_str() {
        Some("middle") => MouseButton::Auxiliary,
        Some("right") => MouseButton::Secondary,
        _ => MouseButton::Primary,
    };
    let press = |action| InputEvent::MouseButton(MouseButtonEvent::new(action, button, point));
    let moved = InputEvent::MouseMove(MouseMoveEvent::new(point));
    match params["type"].as_str() {
        // Move first so the press lands on the element under the new position.
        Some("mousePressed") => vec![moved, press(MouseButtonAction::Down)],
        Some("mouseReleased") => vec![press(MouseButtonAction::Up)],
        Some("mouseMoved") => vec![moved],
        // CDP: positive delta scrolls down/right. Servo: positive scrolls up/left.
        Some("mouseWheel") => vec![InputEvent::Wheel(WheelEvent::new(
            WheelDelta {
                x: -number("deltaX") * scale,
                y: -number("deltaY") * scale,
                z: 0.0,
                mode: WheelMode::DeltaPixel,
            },
            point,
        ))],
        _ => vec![],
    }
}

/// `Input.dispatchKeyEvent` params → a Servo keyboard event. `char` events are
/// skipped: Servo derives the typed text from the key-down itself.
pub fn cdp_key(params: &Value) -> Option<InputEvent> {
    let state = match params["type"].as_str()? {
        "keyDown" | "rawKeyDown" => KeyState::Down,
        "keyUp" => KeyState::Up,
        _ => return None,
    };
    let name = [params["key"].as_str(), params["text"].as_str()]
        .into_iter()
        .flatten()
        .find(|name| !name.is_empty())?;
    let key = Key::from_str(name).ok()?;
    Some(InputEvent::Keyboard(KeyboardEvent::from_state_and_key(
        state, key,
    )))
}

pub fn mouse_move(point: DevicePoint) -> InputEvent {
    InputEvent::MouseMove(MouseMoveEvent::new(point.into()))
}

pub fn mouse_button(
    state: ElementState,
    button: winit::event::MouseButton,
    point: DevicePoint,
) -> InputEvent {
    let action = match state {
        ElementState::Pressed => MouseButtonAction::Down,
        ElementState::Released => MouseButtonAction::Up,
    };
    let button = match button {
        winit::event::MouseButton::Right => MouseButton::Secondary,
        winit::event::MouseButton::Middle => MouseButton::Auxiliary,
        winit::event::MouseButton::Back => MouseButton::Back,
        winit::event::MouseButton::Forward => MouseButton::Forward,
        _ => MouseButton::Primary,
    };
    InputEvent::MouseButton(MouseButtonEvent::new(action, button, point.into()))
}

pub fn wheel(delta: MouseScrollDelta, point: DevicePoint) -> InputEvent {
    let (x, y, mode) = match delta {
        MouseScrollDelta::LineDelta(dx, dy) => {
            ((dx * 76.0) as f64, (dy * 76.0) as f64, WheelMode::DeltaLine)
        }
        MouseScrollDelta::PixelDelta(delta) => (delta.x, delta.y, WheelMode::DeltaPixel),
    };
    InputEvent::Wheel(WheelEvent::new(
        WheelDelta { x, y, z: 0.0, mode },
        point.into(),
    ))
}

/// winit names its named keys like the W3C key values (`Enter`, `ArrowLeft`, …),
/// which is what `Key::from_str` parses.
pub fn key(event: &winit::event::KeyEvent, modifiers: ModifiersState) -> Option<InputEvent> {
    let key = match &event.logical_key {
        WinitKey::Character(text) => Key::Character(text.to_string()),
        WinitKey::Named(named) => Key::from_str(&format!("{named:?}")).ok()?,
        _ => return None,
    };
    let state = match event.state {
        ElementState::Pressed => KeyState::Down,
        ElementState::Released => KeyState::Up,
    };
    let mut flags = Modifiers::empty();
    flags.set(Modifiers::SHIFT, modifiers.shift_key());
    flags.set(Modifiers::CONTROL, modifiers.control_key());
    flags.set(Modifiers::ALT, modifiers.alt_key());
    flags.set(Modifiers::META, modifiers.super_key());
    Some(InputEvent::Keyboard(KeyboardEvent::new_without_event(
        state,
        key,
        Code::Unidentified,
        Location::Standard,
        flags,
        event.repeat,
        false,
    )))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_cdp_press_moves_to_the_point_first() {
        let events = cdp_mouse(
            &json!({ "type": "mousePressed", "x": 10, "y": 20, "button": "left" }),
            2.0,
        );
        assert!(matches!(
            events[..],
            [InputEvent::MouseMove(_), InputEvent::MouseButton(_)]
        ));
    }

    #[test]
    fn a_cdp_wheel_flips_sign_and_scales_to_device_pixels() {
        let events = cdp_mouse(
            &json!({ "type": "mouseWheel", "x": 0, "y": 0, "deltaY": 100 }),
            2.0,
        );
        let [InputEvent::Wheel(wheel)] = &events[..] else {
            panic!("expected one wheel event")
        };
        assert_eq!(wheel.delta.y, -200.0);
    }

    #[test]
    fn cdp_keys_accept_characters_and_named_keys_but_not_char_events() {
        assert!(cdp_key(&json!({ "type": "keyDown", "key": "a" })).is_some());
        assert!(cdp_key(&json!({ "type": "keyUp", "key": "Enter" })).is_some());
        assert!(cdp_key(&json!({ "type": "char", "text": "a" })).is_none());
        assert!(cdp_key(&json!({ "type": "keyDown", "key": "NotAKey" })).is_none());
    }
}
