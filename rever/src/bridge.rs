//! How MCP tools reach the page: [`AppBrowser`] implements the tools' `Browser`
//! trait on any thread by queueing a [`Command`] for the main thread, where the
//! engine lives, and waiting for the answer.

use std::str::FromStr;

use futures::channel::mpsc::UnboundedSender;
use futures::channel::oneshot;
use rever_tools_api::tool::{BoxFuture, Browser, Cookie, Input, PageInfo, TabInfo};
use serde_json::Value;
use servo::{
    Code, InputEvent, JSValue, Key, KeyState, KeyboardEvent, Location, Modifiers, MouseButton,
    MouseButtonAction, MouseButtonEvent, MouseMoveEvent, WebViewPoint, WheelDelta, WheelEvent,
    WheelMode,
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
    RemoveInitScript(String, Answer<()>),
    /// The agent's decision for a request a modify rule holds.
    Decide(String, crate::intercept::Decision, Answer<()>),
    WriteClipboard(String, Answer<()>),
    Tabs(Answer<Vec<TabInfo>>),
    SelectTab(usize, Answer<()>),
    /// Open a URL in a tab without selecting it; answers the new tab's index.
    OpenTab(String, Answer<usize>),
    ArmDialog(bool, Option<String>, Answer<()>),
    Cookies(String, Answer<Vec<Cookie>>),
    /// A cookie to store for a URL; an expired one deletes.
    SetCookie(String, Cookie, Answer<()>),
    ArmFiles(Vec<String>, Answer<()>),
}

pub struct AppBrowser {
    commands: UnboundedSender<Command>,
    wake: UnboundedSender<()>,
}

impl AppBrowser {
    pub fn new(commands: UnboundedSender<Command>, wake: UnboundedSender<()>) -> Self {
        Self { commands, wake }
    }

    pub(crate) fn ask<T: Send + 'static>(
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

    fn tabs(&self) -> BoxFuture<'_, Result<Vec<TabInfo>, String>> {
        self.ask(Command::Tabs)
    }

    fn select_tab(&self, index: usize) -> BoxFuture<'_, Result<(), String>> {
        self.ask(|answer| Command::SelectTab(index, answer))
    }

    fn open_tab<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<usize, String>> {
        self.ask(|answer| Command::OpenTab(url.to_owned(), answer))
    }

    fn cookies<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Vec<Cookie>, String>> {
        self.ask(|answer| Command::Cookies(url.to_owned(), answer))
    }

    fn set_cookie<'a>(&'a self, url: &'a str, cookie: Cookie) -> BoxFuture<'a, Result<(), String>> {
        self.ask(|answer| Command::SetCookie(url.to_owned(), cookie, answer))
    }

    fn delete_cookie<'a>(
        &'a self,
        url: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        // A cookie that has already expired replaces the stored one and is dropped.
        let expired = Cookie {
            name: name.to_owned(),
            expires: Some(0.0),
            ..Cookie::default()
        };
        self.ask(|answer| Command::SetCookie(url.to_owned(), expired, answer))
    }

    fn arm_dialog_answer(
        &self,
        accept: bool,
        prompt_text: Option<String>,
    ) -> BoxFuture<'_, Result<(), String>> {
        self.ask(|answer| Command::ArmDialog(accept, prompt_text, answer))
    }

    fn arm_file_paths(&self, paths: Vec<String>) -> BoxFuture<'_, Result<(), String>> {
        self.ask(|answer| Command::ArmFiles(paths, answer))
    }

    fn add_init_script<'a>(&'a self, source: &'a str) -> BoxFuture<'a, Result<String, String>> {
        self.ask(|answer| Command::AddInitScript(source.to_owned(), answer))
    }

    fn remove_init_script<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        self.ask(|answer| Command::RemoveInitScript(id.to_owned(), answer))
    }

    fn write_clipboard<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<(), String>> {
        self.ask(|answer| Command::WriteClipboard(text.to_owned(), answer))
    }
}

/// The tools' cookie as the engine's.
pub fn to_engine_cookie(cookie: &Cookie) -> cookie::Cookie<'static> {
    let mut out = cookie::Cookie::new(cookie.name.clone(), cookie.value.clone());
    if !cookie.domain.is_empty() {
        out.set_domain(cookie.domain.clone());
    }
    out.set_path(if cookie.path.is_empty() {
        "/".to_owned()
    } else {
        cookie.path.clone()
    });
    out.set_http_only(cookie.http_only);
    out.set_secure(cookie.secure);
    out.set_same_site(match cookie.same_site.as_deref() {
        Some("Strict") => Some(cookie::SameSite::Strict),
        Some("Lax") => Some(cookie::SameSite::Lax),
        Some("None") => Some(cookie::SameSite::None),
        _ => None,
    });
    if let Some(seconds) = cookie.expires {
        // Out of range reads as "already expired", which is what such a date means.
        let at = cookie::time::OffsetDateTime::from_unix_timestamp(seconds as i64)
            .unwrap_or(cookie::time::OffsetDateTime::UNIX_EPOCH);
        out.set_expires(at);
    }
    out
}

/// The engine's cookie as the tools'.
pub fn from_engine_cookie(cookie: &cookie::Cookie<'_>) -> Cookie {
    Cookie {
        name: cookie.name().to_owned(),
        value: cookie.value().to_owned(),
        domain: cookie.domain().unwrap_or_default().to_owned(),
        path: cookie.path().unwrap_or("/").to_owned(),
        expires: cookie
            .expires_datetime()
            .map(|at| at.unix_timestamp() as f64),
        http_only: cookie.http_only().unwrap_or(false),
        secure: cookie.secure().unwrap_or(false),
        same_site: cookie.same_site().map(|same_site| same_site.to_string()),
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
        Input::MouseButton { x, y, down, button } => {
            let button = match button {
                1 => MouseButton::Auxiliary,
                2 => MouseButton::Secondary,
                _ => MouseButton::Primary,
            };
            let (action, moved) = match down {
                // Move first, as for the plain press.
                true => (
                    MouseButtonAction::Down,
                    Some(InputEvent::MouseMove(MouseMoveEvent::new(at(*x, *y)))),
                ),
                false => (MouseButtonAction::Up, None),
            };
            let press = InputEvent::MouseButton(MouseButtonEvent::new(action, button, at(*x, *y)));
            moved.into_iter().chain([press]).collect()
        }
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
        Input::KeyChord {
            key: name,
            modifiers,
        } => {
            let Ok(parsed) = Key::from_str(name) else {
                return vec![];
            };
            // The tools' bits: Alt 1, Control 2, Meta 4, Shift 8.
            let mut held = Modifiers::empty();
            held.set(Modifiers::ALT, modifiers & 1 != 0);
            held.set(Modifiers::CONTROL, modifiers & 2 != 0);
            held.set(Modifiers::META, modifiers & 4 != 0);
            held.set(Modifiers::SHIFT, modifiers & 8 != 0);
            [KeyState::Down, KeyState::Up]
                .map(|state| {
                    InputEvent::Keyboard(KeyboardEvent::new_without_event(
                        state,
                        parsed.clone(),
                        Code::Unidentified,
                        Location::Standard,
                        held,
                        false,
                        false,
                    ))
                })
                .to_vec()
        }
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

    #[test]
    fn 쿠키를_엔진_형식으로_바꿨다가_되돌리면_같다() {
        let cookie = Cookie {
            name: "sid".to_owned(),
            value: "abc".to_owned(),
            domain: "a.test".to_owned(),
            path: "/app".to_owned(),
            expires: Some(1_900_000_000.0),
            http_only: true,
            secure: true,
            same_site: Some("Lax".to_owned()),
        };
        assert_eq!(from_engine_cookie(&to_engine_cookie(&cookie)), cookie);
        // A session cookie with nothing else set: the path defaults to the root.
        let bare = Cookie {
            name: "k".to_owned(),
            value: "v".to_owned(),
            ..Cookie::default()
        };
        let back = from_engine_cookie(&to_engine_cookie(&bare));
        assert_eq!(
            (back.path.as_str(), back.expires, back.same_site),
            ("/", None, None)
        );
        // The deletion marker is a cookie that expired at the epoch.
        let expired = to_engine_cookie(&Cookie {
            expires: Some(0.0),
            ..bare
        });
        assert_eq!(
            expired.expires_datetime().map(|at| at.unix_timestamp()),
            Some(0)
        );
    }
}
