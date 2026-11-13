//! How MCP tools reach the page: [`AppBrowser`] implements the tools' `Browser`
//! trait on any thread by queueing a [`Command`] for the main thread, where the
//! engine lives, and waiting for the answer.

use std::str::FromStr;

use futures::channel::mpsc::UnboundedSender;
use futures::channel::oneshot;
use rever_tools_api::tool::{BoxFuture, Browser, Input, PageInfo, unsupported};
use serde_json::Value;
use servo::{
    InputEvent, JSValue, Key, KeyState, KeyboardEvent, MouseButton, MouseButtonAction,
    MouseButtonEvent, MouseMoveEvent, WebViewPoint, WheelDelta, WheelEvent, WheelMode,
};

type Answer<T> = oneshot::Sender<Result<T, String>>;

/// One request from a tool, answered on the main thread.
pub enum Command {
    Page(Answer<PageInfo>),
    Navigate(String, Answer<()>),
    Back(Answer<()>),
    Forward(Answer<()>),
    Reload(Answer<()>),
    Evaluate(String, Answer<Value>),
    Screenshot(Answer<Vec<u8>>),
    Input(Input, Answer<()>),
    Viewport(Answer<(u32, u32)>),
    AddInitScript(String, Answer<String>),
}

pub struct AppBrowser {
    commands: UnboundedSender<Command>,
    wake: UnboundedSender<()>,
}

impl AppBrowser {
    pub fn new(commands: UnboundedSender<Command>, wake: UnboundedSender<()>) -> Self {
        Self { commands, wake }
    }

    fn ask<T: Send + 'static>(
        &self,
        make: impl FnOnce(Answer<T>) -> Command,
    ) -> BoxFuture<'static, Result<T, String>> {
        let (answer, answered) = oneshot::channel();
        let sent = self.commands.unbounded_send(make(answer)).is_ok();
        let _ = self.wake.unbounded_send(());
        Box::pin(async move {
            if !sent {
                return Err("the browser window is closed".to_owned());
            }
            answered
                .await
                .unwrap_or_else(|_| Err("the browser window is closed".to_owned()))
        })
    }
}

impl Browser for AppBrowser {
    fn page(&self) -> BoxFuture<'_, Result<PageInfo, String>> {
        self.ask(Command::Page)
    }

    fn navigate<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<(), String>> {
        self.ask(|answer| Command::Navigate(url.to_owned(), answer))
    }

    fn back(&self) -> BoxFuture<'_, Result<(), String>> {
        self.ask(Command::Back)
    }

    fn forward(&self) -> BoxFuture<'_, Result<(), String>> {
        self.ask(Command::Forward)
    }

    fn reload(&self) -> BoxFuture<'_, Result<(), String>> {
        self.ask(Command::Reload)
    }

    fn evaluate<'a>(&'a self, expression: &'a str) -> BoxFuture<'a, Result<Value, String>> {
        self.ask(|answer| Command::Evaluate(expression.to_owned(), answer))
    }

    fn screenshot(&self) -> BoxFuture<'_, Result<Vec<u8>, String>> {
        self.ask(Command::Screenshot)
    }

    fn input(&self, input: Input) -> BoxFuture<'_, Result<(), String>> {
        self.ask(|answer| Command::Input(input, answer))
    }

    fn viewport(&self) -> BoxFuture<'_, Result<(u32, u32), String>> {
        self.ask(Command::Viewport)
    }

    fn add_init_script<'a>(&'a self, source: &'a str) -> BoxFuture<'a, Result<String, String>> {
        self.ask(|answer| Command::AddInitScript(source.to_owned(), answer))
    }

    // shortcut: Servo's UserContentManager can remove a script only by the handle it
    // was added with; keep handles by id in the engine before porting tools that need this.
    fn remove_init_script<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err(unsupported("removing init scripts")) })
    }
}

/// Servo's script result as plain JSON. Element and window handles become their id strings.
pub fn js_to_json(value: &JSValue) -> Value {
    match value {
        JSValue::Undefined | JSValue::Null => Value::Null,
        JSValue::Boolean(b) => Value::Bool(*b),
        JSValue::Number(n) => serde_json::Number::from_f64(*n).map_or(Value::Null, |number| {
            // Whole numbers print as integers, as JSON.stringify does.
            if n.fract() == 0.0 && n.abs() < 9e15 {
                Value::from(*n as i64)
            } else {
                Value::Number(number)
            }
        }),
        JSValue::String(s)
        | JSValue::Element(s)
        | JSValue::ShadowRoot(s)
        | JSValue::Frame(s)
        | JSValue::Window(s) => Value::String(s.clone()),
        JSValue::Array(items) => Value::Array(items.iter().map(js_to_json).collect()),
        JSValue::Object(map) => {
            // Servo hands the object over as a hash map: key order is already lost.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), js_to_json(&map[key])))
                    .collect(),
            )
        }
    }
}

/// A tool's input → the engine events that perform it. Coordinates are CSS pixels.
pub fn input_events(input: &Input, scale: f64) -> Vec<InputEvent> {
    let at = |x: f64, y: f64| WebViewPoint::Page(euclid::Point2D::new(x as f32, y as f32));
    let key = |state, key: Key| InputEvent::Keyboard(KeyboardEvent::from_state_and_key(state, key));
    match input {
        Input::MouseMove { x, y } => vec![InputEvent::MouseMove(MouseMoveEvent::new(at(*x, *y)))],
        // Move first so the press lands on the element under the new position.
        Input::MouseDown { x, y } => vec![
            InputEvent::MouseMove(MouseMoveEvent::new(at(*x, *y))),
            InputEvent::MouseButton(MouseButtonEvent::new(
                MouseButtonAction::Down,
                MouseButton::Primary,
                at(*x, *y),
            )),
        ],
        Input::MouseUp { x, y } => vec![InputEvent::MouseButton(MouseButtonEvent::new(
            MouseButtonAction::Up,
            MouseButton::Primary,
            at(*x, *y),
        ))],
        // Tools use "positive scrolls down"; Servo uses "positive scrolls up", in device pixels.
        Input::Wheel { x, y, dx, dy } => vec![InputEvent::Wheel(WheelEvent::new(
            WheelDelta {
                x: -dx * scale,
                y: -dy * scale,
                z: 0.0,
                mode: WheelMode::DeltaPixel,
            },
            at(*x, *y),
        ))],
        Input::Key { key: name } => match Key::from_str(name) {
            Ok(parsed) => vec![
                key(KeyState::Down, parsed.clone()),
                key(KeyState::Up, parsed),
            ],
            Err(_) => vec![],
        },
        Input::Text { text } => text
            .chars()
            .flat_map(|ch| {
                let character = Key::Character(ch.to_string());
                [
                    key(KeyState::Down, character.clone()),
                    key(KeyState::Up, character),
                ]
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    #[test]
    fn 스크립트_결과를_json으로_바꾼다() {
        let object = JSValue::Object(HashMap::from([
            (
                "b".to_owned(),
                JSValue::Array(vec![JSValue::Boolean(true), JSValue::Null]),
            ),
            ("a".to_owned(), JSValue::Number(1.0)),
            ("c".to_owned(), JSValue::Number(1.5)),
        ]));
        assert_eq!(
            js_to_json(&object),
            json!({ "a": 1, "b": [true, null], "c": 1.5 })
        );
        assert_eq!(js_to_json(&JSValue::Undefined), Value::Null);
        assert_eq!(js_to_json(&JSValue::Number(f64::NAN)), Value::Null);
    }

    #[test]
    fn 글자_입력은_글자마다_누름과_뗌을_만든다() {
        assert_eq!(
            input_events(
                &Input::Text {
                    text: "한a".into()
                },
                2.0
            )
            .len(),
            4
        );
        assert_eq!(
            input_events(
                &Input::Key {
                    key: "Enter".into()
                },
                2.0
            )
            .len(),
            2
        );
        assert!(
            input_events(
                &Input::Key {
                    key: "NotAKey".into()
                },
                2.0
            )
            .is_empty()
        );
        assert_eq!(
            input_events(&Input::MouseDown { x: 1.0, y: 2.0 }, 2.0).len(),
            2
        );
    }

    #[test]
    fn 휠은_방향을_뒤집고_기기_픽셀로_바꾼다() {
        let events = input_events(
            &Input::Wheel {
                x: 0.0,
                y: 0.0,
                dx: 0.0,
                dy: 100.0,
            },
            2.0,
        );
        let [InputEvent::Wheel(wheel)] = &events[..] else {
            panic!("expected one wheel event")
        };
        assert_eq!(wheel.delta.y, -200.0);
    }
}
