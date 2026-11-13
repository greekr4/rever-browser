//! Port of `src/main/mcp/human-input.ts` — mouse, keyboard and wheel input shaped
//! like a person's: eased paths with jitter, per-step and per-key pauses.
//!
//! The TypeScript dispatched Chrome DevTools Protocol input events; here every
//! event goes through `Browser::input`, which has a left button, single clicks
//! and unmodified keys. A press with another button or a click count, a key with
//! modifiers held, and the browser's own drag machinery are reported as
//! unsupported instead of being sent as something else.

use std::sync::{Arc, Mutex};

use rever_tools_api::tool::{Browser, Input, unsupported};

use crate::cdp_eval::fire;
use crate::{js_round, lock, now_ms, sleep_ms, truthy};

fn ease_in_out_cubic(t: f64) -> f64 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// Which button a press uses and how many clicks it counts as.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mouse<'a> {
    pub button: Option<&'a str>,
    pub click_count: Option<f64>,
}

impl Mouse<'_> {
    /// A single left click, the only press `Input` can express.
    pub fn is_plain(&self) -> bool {
        self.button.unwrap_or("left") == "left" && !self.click_count.is_some_and(|n| n > 1.0)
    }
}

/// What the TypeScript kept in module variables: the pointer position, and the
/// random source standing in for `Math.random`.
pub struct HumanInput {
    // Persisted pointer position so successive clicks travel from where the cursor
    // "was" rather than warping from the corner each time.
    cursor: Mutex<Point>,
    rng: Mutex<u64>,
}

impl Default for HumanInput {
    fn default() -> Self {
        // Unpredictable enough for jitter: the clock, and where this call's stack lives.
        let marker = 0u8;
        let here = std::ptr::from_ref(&marker) as usize as u64;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| u64::from(since.subsec_nanos()));
        let human = Self {
            cursor: Mutex::new(Point { x: 0.0, y: 0.0 }),
            // xorshift never leaves zero.
            rng: Mutex::new((now_ms() ^ nanos.rotate_left(32) ^ here) | 1),
        };
        let start = Point {
            x: 60.0 + human.random() * 200.0,
            y: 60.0 + human.random() * 200.0,
        };
        *lock(&human.cursor) = start;
        human
    }
}

impl HumanInput {
    /// `Math.random()`: uniform in [0, 1).
    fn random(&self) -> f64 {
        let mut state = lock(&self.rng);
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        (*state >> 11) as f64 / (1u64 << 53) as f64
    }

    fn rand(&self, min: f64, max: f64) -> f64 {
        min + self.random() * (max - min)
    }

    async fn pause(&self, min: f64, max: f64) {
        sleep_ms(self.rand(min, max)).await;
    }

    pub fn cursor(&self) -> Point {
        *lock(&self.cursor)
    }

    /// Mouse move from the persisted cursor position to (to_x, to_y) using an
    /// eased path with jitter and per-step delay. Real mousemove events fire
    /// along the path, so sites with behaviour-based bot detection see
    /// human-shaped input rather than a teleport.
    pub async fn mouse_move(
        &self,
        browser: &Arc<dyn Browser>,
        to_x: f64,
        to_y: f64,
    ) -> Result<(), String> {
        let from = self.cursor();
        let dist = (to_x - from.x).hypot(to_y - from.y);
        // ~1 step per 25px, capped 12–60. Larger distance = more steps but each ~10ms.
        let steps = js_round(dist / 25.0).clamp(12.0, 60.0) as u32;

        for i in 1..=steps {
            let e = ease_in_out_cubic(f64::from(i) / f64::from(steps));
            let x = from.x + (to_x - from.x) * e + self.rand(-1.5, 1.5);
            let y = from.y + (to_y - from.y) * e + self.rand(-1.5, 1.5);
            // The cursor sprite is cosmetic, so it is fired without waiting; only
            // the input event itself has to be awaited.
            fire(
                browser,
                format!(
                    "window.__reverAi && window.__reverAi.showCursorAt({}, {})",
                    crate::js_number(x),
                    crate::js_number(y)
                ),
            );
            browser.input(Input::MouseMove { x, y }).await?;
            self.pause(6.0, 14.0).await;
        }
        *lock(&self.cursor) = Point { x: to_x, y: to_y };
        Ok(())
    }

    /// Press + release at the current cursor position (no movement). Caller
    /// should `mouse_move` first. Includes a settle delay and realistic press
    /// hold so the highlight has a moment to render before the click.
    pub async fn press_release(
        &self,
        browser: &Arc<dyn Browser>,
        x: f64,
        y: f64,
        mouse: Mouse<'_>,
    ) -> Result<(), String> {
        // A double click is one press/release pair with clickCount 2, not two
        // separate clicks — two browser_click calls are seconds apart and the page
        // pairs nothing. `Input` carries neither a button nor a count, and a left
        // click sent in their place would be a different gesture.
        if !mouse.is_plain() {
            return Err(unsupported(
                "a press with another mouse button or a click count",
            ));
        }
        // Same reason as the cursor move: the press/release sprite is cosmetic, and
        // awaiting it queues behind the page's main thread.

        self.pause(140.0, 260.0).await; // settle — also lets the highlight overlay render
        let press = |down: bool| {
            fire(
                browser,
                format!("window.__reverAi && window.__reverAi.setCursorPress({down})"),
            );
        };
        press(true);
        browser.input(Input::MouseDown { x, y }).await?;
        self.pause(45.0, 95.0).await;
        press(false);
        browser.input(Input::MouseUp { x, y }).await
    }

    /// Press at one point, move while held, release at another — one gesture.
    ///
    /// HTML5 drag-and-drop and pointer-based sortables both need the button to stay
    /// down across the move; a click at the source followed by a click at the target
    /// is two unrelated clicks and neither library sees a drag.
    pub async fn drag(
        &self,
        browser: &Arc<dyn Browser>,
        from: Point,
        to: Point,
    ) -> Result<(), String> {
        self.mouse_move(browser, from.x, from.y).await?;
        let down = Input::MouseDown {
            x: from.x,
            y: from.y,
        };
        browser.input(down).await?;
        self.pause(80.0, 160.0).await;

        // Intermediate moves with the button held. A single jump to the target is
        // ignored by libraries that require a drag threshold to be crossed.
        let steps = 14;
        for i in 1..=steps {
            let t = ease_in_out_cubic(f64::from(i) / f64::from(steps));
            let x = from.x + (to.x - from.x) * t;
            let y = from.y + (to.y - from.y) * t;
            browser.input(Input::MouseMove { x, y }).await?;
            self.pause(10.0, 22.0).await;
        }

        self.pause(60.0, 140.0).await;
        browser.input(Input::MouseUp { x: to.x, y: to.y }).await?;
        *lock(&self.cursor) = to;
        Ok(())
    }

    /// HTML5 drag-and-drop, which mouse events alone cannot drive.
    ///
    /// A `draggable` element hands off to the browser's own drag machinery on
    /// mousedown, and from there mouse input produces nothing the page can see.
    /// The TypeScript intercepted that hand-off (`Input.setInterceptDrags`) and
    /// replayed it as drag events. `Browser` cannot intercept it, so this reports
    /// that the machinery did not take over — the answer the TypeScript gave when
    /// no interception arrived — and sends nothing, leaving the pointer free for
    /// the caller's fallback.
    pub async fn native_drag(
        &self,
        _browser: &Arc<dyn Browser>,
        _from: Point,
        _to: Point,
    ) -> Result<bool, String> {
        Ok(false)
    }

    /// Pre-action "thinking" pause — looking at the page before acting.
    pub async fn thinking_pause(&self) {
        self.pause(380.0, 950.0).await;
    }

    /// Send one key to whatever currently has focus.
    pub async fn press_key(
        &self,
        browser: &Arc<dyn Browser>,
        key: &str,
        modifiers: &[&str],
        repeat: Option<f64>,
    ) -> Result<(), String> {
        // A page that has not been interacted with DROPS key events and reports
        // no error — pressing Escape straight after a navigation silently did
        // nothing. Only a real mouse press or focusing an element grants the
        // renderer keyboard focus. document.hasFocus() separates the two states
        // exactly, so refuse rather than pretend the key was delivered.
        if !truthy(&browser.evaluate("document.hasFocus()").await?) {
            return Err(
                "the page does not have keyboard focus, so the key would be dropped — pass ref to focus an element first, or click something"
                    .to_owned(),
            );
        }

        let press = key_press(key, modifiers)?;
        if press.modifiers != 0 {
            return Err(unsupported("pressing a key with modifiers held"));
        }

        let times = repeat.unwrap_or(1.0).max(1.0);
        let mut i = 0.0;
        while i < times {
            let key = press.key.clone();
            browser.input(Input::Key { key }).await?;
            self.pause(20.0, 60.0).await;
            if i > 0.0 {
                self.pause(30.0, 80.0).await;
            }
            i += 1.0;
        }
        Ok(())
    }

    /// Type the text into the focused element as real keyboard input, one
    /// character at a time, so behaviour-based bot detectors see authentic
    /// keystroke timing instead of a script-dispatched event burst.
    ///
    /// The caller focuses the element first; per-char timing uses the same jitter
    /// profile as `mouse_move`.
    pub async fn type_text(
        &self,
        browser: &Arc<dyn Browser>,
        text: &str,
        submit: bool,
    ) -> Result<(), String> {
        for ch in text.chars() {
            let k = key_params_for(ch);
            // A printable character is text (which is also how non-Latin input
            // reaches the page); a line break is the Enter key.
            let input = if k.key == "Enter" {
                Input::Key { key: k.key }
            } else {
                Input::Text { text: k.text }
            };
            browser.input(input).await?;
            // Typing beam + sparks — cosmetic, fire-and-forget for the same reason
            // as the cursor sprite.
            fire(
                browser,
                "window.__reverAi && window.__reverAi.typeKey()".to_owned(),
            );
            self.pause(15.0, 55.0).await;
            if self.random() < 0.04 {
                self.pause(120.0, 260.0).await;
            }
        }

        if submit {
            self.pause(220.0, 480.0).await;
            let key = "Enter".to_owned();
            browser.input(Input::Key { key }).await?;
        }
        Ok(())
    }

    /// Smooth scroll: break a single scroll request into many small wheel-sized
    /// chunks with per-chunk delay. `total_delta_y` ~ total pixels (sign =
    /// direction). Returns the page's `scrollY` afterwards.
    pub async fn scroll(
        &self,
        browser: &Arc<dyn Browser>,
        total_delta_y: f64,
        absolute_y: Option<f64>,
    ) -> Result<f64, String> {
        // Resolve to a concrete delta. If absolute_y supplied, compute current Y first.
        let mut delta = total_delta_y;
        if let Some(absolute_y) = absolute_y {
            let current = browser.evaluate("window.scrollY").await?;
            delta = absolute_y - current.as_f64().unwrap_or(f64::NAN);
        }

        // Real wheel input rather than window.scrollBy.
        //
        // scrollBy moves the page but fires no scroll event here — the event is
        // dispatched on a frame boundary and an agent-driven window produces none.
        // Infinite scroll, sticky headers and lazy images all hang off that event,
        // so the page moved and never reacted. A wheel goes through the browser's
        // input path and behaves the way a person's does, including handing the
        // scroll to whatever container is under the cursor.
        let Point { x, y } = self.cursor();
        let sign = if delta >= 0.0 { 1.0 } else { -1.0 };
        let mut remaining = delta.abs();
        while remaining > 0.0 {
            let chunk = remaining.min(100.0 + self.rand(-15.0, 15.0));
            let wheel = Input::Wheel {
                x,
                y,
                dx: 0.0,
                dy: sign * chunk,
            };
            browser.input(wheel).await?;
            remaining -= chunk;
            self.pause(14.0, 26.0).await;
        }

        let after = browser.evaluate("({ scrollY: window.scrollY })").await?;
        after
            .get("scrollY")
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| "the page did not report its scroll position".to_owned())
    }
}

/// What one character is as a key event.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyParams {
    pub key: String,
    pub code: Option<String>,
    pub key_code: Option<u32>,
    pub text: String,
}

// Map a single character to its key event. For ordinary printable chars this is
// enough; non-Latin (Korean, etc.) carries only `text`, which is how the IME
// path finally surfaces keys to the page.
pub fn key_params_for(ch: char) -> KeyParams {
    let params = |key: &str, code: Option<String>, key_code: Option<u32>, text: &str| KeyParams {
        key: key.to_owned(),
        code,
        key_code,
        text: text.to_owned(),
    };
    let own = ch.to_string();
    match ch {
        ' ' => params(" ", Some("Space".to_owned()), Some(32), " "),
        '\n' => params("Enter", Some("Enter".to_owned()), Some(13), "\r"),
        'a'..='z' | 'A'..='Z' => {
            let upper = ch.to_ascii_uppercase();
            params(
                &own,
                Some(format!("Key{upper}")),
                Some(u32::from(upper)),
                &own,
            )
        }
        '0'..='9' => params(&own, Some(format!("Digit{ch}")), Some(u32::from(ch)), &own),
        _ => params(&own, None, None, &own),
    }
}

/// Named keys that carry no printable text, plus the few that do.
///
/// Typing cannot stand in for these: a dialog closes on Escape, a listbox moves
/// on ArrowDown, and a field is cleared with Control+A then Delete. Without them
/// a keyboard-driven widget cannot be operated at all.
const NAMED_KEYS: [(&str, &str, u32, Option<&str>); 14] = [
    ("Enter", "Enter", 13, Some("\r")),
    ("Escape", "Escape", 27, None),
    ("Tab", "Tab", 9, Some("\t")),
    ("Backspace", "Backspace", 8, None),
    ("Delete", "Delete", 46, None),
    ("ArrowUp", "ArrowUp", 38, None),
    ("ArrowDown", "ArrowDown", 40, None),
    ("ArrowLeft", "ArrowLeft", 37, None),
    ("ArrowRight", "ArrowRight", 39, None),
    ("Home", "Home", 36, None),
    ("End", "End", 35, None),
    ("PageUp", "PageUp", 33, None),
    ("PageDown", "PageDown", 34, None),
    ("Space", "Space", 32, Some(" ")),
];

const MODIFIER_BITS: [(&str, u8); 4] = [("Alt", 1), ("Control", 2), ("Meta", 4), ("Shift", 8)];

/// Editing shortcuts Chromium routes through named commands rather than through
/// the key event itself. Dispatching Meta+A without this moves nothing: the
/// following Delete then eats a single character instead of the selection,
/// which looked like "clearing a field almost works".
const EDIT_COMMANDS: [(&str, &str); 5] = [
    ("a", "selectAll"),
    ("c", "copy"),
    ("v", "paste"),
    ("x", "cut"),
    ("z", "undo"),
];

/// The accelerator modifier this platform uses for editing shortcuts.
pub const ACCEL: &str = if cfg!(target_os = "macos") {
    "Meta"
} else {
    "Control"
};

pub fn is_named_key(key: &str) -> bool {
    NAMED_KEYS.iter().any(|(name, ..)| *name == key)
}

/// Every key this accepts, for an error message that can be acted on.
pub fn named_keys() -> Vec<&'static str> {
    NAMED_KEYS.iter().map(|(name, ..)| *name).collect()
}

/// One key press, worked out from the key name and the modifiers held.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyPress {
    /// The W3C key value: what `Input::Key` takes.
    pub key: String,
    pub code: Option<String>,
    pub key_code: Option<u32>,
    /// `Alt` 1, `Control` 2, `Meta` 4, `Shift` 8.
    pub modifiers: u8,
    /// The text the key types; none for a shortcut.
    pub text: Option<String>,
    /// The editing command the shortcut stands for.
    pub command: Option<&'static str>,
}

/// Checks the key and the modifiers and works out what the press carries.
pub fn key_press(key: &str, modifiers: &[&str]) -> Result<KeyPress, String> {
    let named = NAMED_KEYS.iter().find(|(name, ..)| *name == key);
    let mut chars = key.chars();
    let single = match (chars.next(), chars.next()) {
        (Some(ch), None) if named.is_none() => Some(ch),
        _ => None,
    };
    let (w3c, code, key_code, text) = match (named, single) {
        (Some(&(name, code, key_code, text)), _) => (
            // The key value of the space bar is the space itself.
            text.filter(|_| name == "Space").unwrap_or(name).to_owned(),
            Some(code.to_owned()),
            Some(key_code),
            text.map(str::to_owned),
        ),
        (None, Some(ch)) => {
            let k = key_params_for(ch);
            (k.key, k.code, k.key_code, Some(k.text))
        }
        (None, None) => {
            return Err(format!(
                "unknown key \"{key}\" — use a single character or one of: {}",
                named_keys().join(", ")
            ));
        }
    };
    let mut bits = 0;
    for name in modifiers {
        let bit = MODIFIER_BITS.iter().find(|(known, _)| known == name);
        let Some((_, bit)) = bit else {
            return Err(format!(
                "unknown modifier \"{name}\" — use Alt, Control, Meta or Shift"
            ));
        };
        bits |= bit;
    }

    let command = if modifiers.contains(&ACCEL) && !modifiers.contains(&"Shift") {
        let lower = key.to_lowercase();
        let found = EDIT_COMMANDS.iter().find(|(letter, _)| *letter == lower);
        found.map(|(_, command)| *command)
    } else {
        None
    };
    Ok(KeyPress {
        key: w3c,
        code,
        key_code,
        modifiers: bits,
        // A modified key is a shortcut, not text. Sending `text` alongside makes
        // the browser treat Control+A as typing the letter "a" into the field.
        text: text.filter(|_| bits == 0),
        command,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{FakeBrowser, context, settle_overlay};

    fn human_at(x: f64, y: f64) -> HumanInput {
        let human = HumanInput::default();
        *lock(&human.cursor) = Point { x, y };
        human
    }

    fn moves(inputs: &[Input]) -> Vec<(f64, f64)> {
        inputs
            .iter()
            .filter_map(|input| match input {
                Input::MouseMove { x, y } => Some((*x, *y)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn 이징은_양_끝에서_0과_1이고_가운데에서_절반이다() {
        assert_eq!(ease_in_out_cubic(0.0), 0.0);
        assert_eq!(ease_in_out_cubic(0.25), 0.0625);
        assert_eq!(ease_in_out_cubic(0.5), 0.5);
        assert_eq!(ease_in_out_cubic(0.75), 0.9375);
        assert_eq!(ease_in_out_cubic(1.0), 1.0);
    }

    #[test]
    fn 처음_커서는_60에서_260_사이에_놓이고_난수는_0과_1_사이다() {
        for _ in 0..50 {
            let human = HumanInput::default();
            let Point { x, y } = human.cursor();
            assert!((60.0..260.0).contains(&x) && (60.0..260.0).contains(&y));
            let r = human.rand(6.0, 14.0);
            assert!((6.0..14.0).contains(&r));
        }
        // Not stuck on one value.
        let human = HumanInput::default();
        assert_ne!(human.random(), human.random());
    }

    #[test]
    fn 글자를_키_이벤트로_바꾼다() {
        let params = |key: &str, code: Option<&str>, key_code: Option<u32>, text: &str| KeyParams {
            key: key.to_owned(),
            code: code.map(str::to_owned),
            key_code,
            text: text.to_owned(),
        };
        assert_eq!(
            key_params_for(' '),
            params(" ", Some("Space"), Some(32), " ")
        );
        assert_eq!(
            key_params_for('\n'),
            params("Enter", Some("Enter"), Some(13), "\r")
        );
        assert_eq!(
            key_params_for('a'),
            params("a", Some("KeyA"), Some(65), "a")
        );
        assert_eq!(
            key_params_for('Z'),
            params("Z", Some("KeyZ"), Some(90), "Z")
        );
        assert_eq!(
            key_params_for('7'),
            params("7", Some("Digit7"), Some(55), "7")
        );
        assert_eq!(key_params_for('!'), params("!", None, None, "!"));
        // Only ASCII letters get a code; other scripts are text alone.
        assert_eq!(key_params_for('한'), params("한", None, None, "한"));
        assert_eq!(key_params_for('é'), params("é", None, None, "é"));
    }

    #[test]
    fn 이름_있는_키를_타입스크립트와_같은_순서로_나열한다() {
        assert_eq!(
            named_keys().join(", "),
            "Enter, Escape, Tab, Backspace, Delete, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, Home, End, PageUp, PageDown, Space"
        );
        assert!(is_named_key("Escape"));
        assert!(!is_named_key("escape"));
        assert!(!is_named_key("a"));
    }

    #[test]
    fn 키와_조합키를_확인하고_누름을_계산한다() {
        let escape = key_press("Escape", &[]).unwrap();
        assert_eq!((escape.key.as_str(), escape.key_code), ("Escape", Some(27)));
        assert_eq!(
            (escape.text, escape.modifiers, escape.command),
            (None, 0, None)
        );

        let enter = key_press("Enter", &[]).unwrap();
        assert_eq!(enter.text.as_deref(), Some("\r"));

        // The W3C key value of the space bar is the space.
        let space = key_press("Space", &[]).unwrap();
        assert_eq!(
            (space.key.as_str(), space.code.as_deref()),
            (" ", Some("Space"))
        );

        let letter = key_press("a", &[]).unwrap();
        assert_eq!(
            (letter.key.as_str(), letter.text.as_deref()),
            ("a", Some("a"))
        );
        assert_eq!(key_press("한", &[]).unwrap().key, "한");
        // One character, even when it is two UTF-16 units.
        assert_eq!(key_press("😀", &[]).unwrap().key, "😀");

        // A modified key is a shortcut: no text.
        let shifted = key_press("a", &["Shift", "Alt"]).unwrap();
        assert_eq!(
            (shifted.modifiers, shifted.text, shifted.command),
            (9, None, None)
        );
        assert_eq!(key_press("a", &["Control", "Meta"]).unwrap().modifiers, 6);
    }

    #[test]
    fn 편집_단축키는_플랫폼의_가속키에만_명령을_붙인다() {
        assert_eq!(key_press("a", &[ACCEL]).unwrap().command, Some("selectAll"));
        assert_eq!(key_press("V", &[ACCEL]).unwrap().command, Some("paste"));
        assert_eq!(key_press("z", &[ACCEL, "Shift"]).unwrap().command, None);
        assert_eq!(key_press("b", &[ACCEL]).unwrap().command, None);
        assert_eq!(key_press("a", &["Alt"]).unwrap().command, None);
    }

    #[test]
    fn 모르는_키와_조합키는_고칠_수_있는_글로_거절한다() {
        assert_eq!(
            key_press("Esc", &[]),
            Err("unknown key \"Esc\" — use a single character or one of: Enter, Escape, Tab, Backspace, Delete, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, Home, End, PageUp, PageDown, Space".to_owned())
        );
        assert!(key_press("", &[]).is_err());
        assert_eq!(
            key_press("a", &["Hyper"]),
            Err("unknown modifier \"Hyper\" — use Alt, Control, Meta or Shift".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 마우스는_떨림이_있는_경로로_움직이고_목적지를_기억한다() {
        let (browser, ctx) = context(FakeBrowser::new());
        let human = human_at(100.0, 100.0);

        // 1000px away: 40 steps.
        human.mouse_move(&ctx.browser, 1100.0, 100.0).await.unwrap();
        let path = moves(&browser.inputs());
        assert_eq!(path.len(), 40);
        for (x, y) in &path {
            assert!((98.5..=1101.5).contains(x) && (98.5..=101.5).contains(y));
        }
        // Eased: the first step barely leaves, the last one arrives (within jitter).
        assert!(path[0].0 < 103.0);
        assert!((path[39].0 - 1100.0).abs() <= 1.5);
        assert_eq!(
            human.cursor(),
            Point {
                x: 1100.0,
                y: 100.0
            }
        );

        // The cursor sprite follows without being waited for.
        settle_overlay().await;
        let overlay = browser.overlay();
        assert_eq!(overlay.len(), 40);
        assert!(overlay[0].starts_with("window.__reverAi && window.__reverAi.showCursorAt("));
    }

    #[tokio::test(start_paused = true)]
    async fn 걸음_수는_12에서_60_사이로_제한한다() {
        let (browser, ctx) = context(FakeBrowser::new());
        let human = human_at(100.0, 100.0);
        human.mouse_move(&ctx.browser, 101.0, 100.0).await.unwrap();
        assert_eq!(browser.inputs().len(), 12);

        human.mouse_move(&ctx.browser, 9000.0, 100.0).await.unwrap();
        assert_eq!(browser.inputs().len(), 12 + 60);
    }

    #[tokio::test(start_paused = true)]
    async fn 누르고_떼기는_그_자리에서_한_번씩_보낸다() {
        let (browser, ctx) = context(FakeBrowser::new());
        let human = human_at(10.0, 10.0);
        let started = tokio::time::Instant::now();
        human
            .press_release(&ctx.browser, 30.0, 40.0, Mouse::default())
            .await
            .unwrap();
        assert_eq!(
            browser.inputs(),
            [
                Input::MouseDown { x: 30.0, y: 40.0 },
                Input::MouseUp { x: 30.0, y: 40.0 }
            ]
        );
        // Settle 140–260ms, hold 45–95ms.
        let waited = started.elapsed().as_millis();
        assert!((185..=355).contains(&waited), "{waited}");
        settle_overlay().await;
        assert_eq!(
            browser.overlay(),
            [
                "window.__reverAi && window.__reverAi.setCursorPress(true)",
                "window.__reverAi && window.__reverAi.setCursorPress(false)"
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 다른_버튼과_여러_번_클릭은_보내지_않고_지원하지_않는다고_알린다() {
        let (browser, ctx) = context(FakeBrowser::new());
        let human = human_at(10.0, 10.0);
        let refused = Err(unsupported(
            "a press with another mouse button or a click count",
        ));
        for mouse in [
            Mouse {
                button: Some("right"),
                click_count: None,
            },
            Mouse {
                button: Some("middle"),
                click_count: Some(1.0),
            },
            Mouse {
                button: None,
                click_count: Some(2.0),
            },
            Mouse {
                button: Some("left"),
                click_count: Some(1.5),
            },
        ] {
            assert_eq!(
                human.press_release(&ctx.browser, 1.0, 2.0, mouse).await,
                refused
            );
        }
        assert!(browser.inputs().is_empty());

        // A count of 1 (or below) with the left button is the plain click.
        let plain = Mouse {
            button: Some("left"),
            click_count: Some(1.0),
        };
        assert!(plain.is_plain());
        human
            .press_release(&ctx.browser, 1.0, 2.0, plain)
            .await
            .unwrap();
        assert_eq!(browser.inputs().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn 드래그는_누른_채_14걸음_움직이고_목적지에서_뗀다() {
        let (browser, ctx) = context(FakeBrowser::new());
        let human = human_at(50.0, 50.0);
        let (from, to) = (Point { x: 50.0, y: 60.0 }, Point { x: 250.0, y: 160.0 });
        human.drag(&ctx.browser, from, to).await.unwrap();

        let inputs = browser.inputs();
        let down = inputs
            .iter()
            .position(|input| matches!(input, Input::MouseDown { .. }))
            .unwrap();
        // The approach (12 steps for 10px), then the press at the source.
        assert_eq!(down, 12);
        assert_eq!(inputs[down], Input::MouseDown { x: 50.0, y: 60.0 });
        let held = moves(&inputs[down..]);
        assert_eq!(held.len(), 14);
        // No jitter while held, and the last step is the target itself.
        assert_eq!(held[13], (250.0, 160.0));
        assert_eq!(held[6], (150.0, 110.0));
        assert_eq!(inputs.last(), Some(&Input::MouseUp { x: 250.0, y: 160.0 }));
        assert_eq!(human.cursor(), to);
    }

    #[tokio::test(start_paused = true)]
    async fn 브라우저_자체_드래그는_가로챌_수_없어_아무것도_보내지_않는다() {
        let (browser, ctx) = context(FakeBrowser::new());
        let human = human_at(5.0, 5.0);
        let (from, to) = (Point { x: 1.0, y: 2.0 }, Point { x: 3.0, y: 4.0 });
        assert_eq!(human.native_drag(&ctx.browser, from, to).await, Ok(false));
        assert!(browser.inputs().is_empty());
        assert_eq!(human.cursor(), Point { x: 5.0, y: 5.0 });
    }

    #[tokio::test(start_paused = true)]
    async fn 생각하는_멈춤은_380에서_950밀리초다() {
        let human = HumanInput::default();
        let started = tokio::time::Instant::now();
        human.thinking_pause().await;
        let waited = started.elapsed().as_millis();
        assert!((380..=951).contains(&waited), "{waited}");
    }

    #[tokio::test(start_paused = true)]
    async fn 키는_페이지에_포커스가_있을_때만_보낸다() {
        let (browser, ctx) = context(FakeBrowser::answering([Ok(json!(false))]));
        let human = HumanInput::default();
        assert_eq!(
            human.press_key(&ctx.browser, "Escape", &[], None).await,
            Err("the page does not have keyboard focus, so the key would be dropped — pass ref to focus an element first, or click something".to_owned())
        );
        assert_eq!(browser.calls(), ["evaluate:document.hasFocus()"]);
        assert!(browser.inputs().is_empty());

        // The focus check failing is the tool failing.
        let (_, ctx) = context(FakeBrowser::answering([Err("gone".to_owned())]));
        assert_eq!(
            human.press_key(&ctx.browser, "Escape", &[], None).await,
            Err("gone".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 키를_w3c_값으로_반복_횟수만큼_누른다() {
        let focused = || Ok(json!(true));
        let (browser, ctx) = context(FakeBrowser::answering([focused(), focused(), focused()]));
        let human = HumanInput::default();
        let key = |key: &str| Input::Key {
            key: key.to_owned(),
        };

        human
            .press_key(&ctx.browser, "ArrowDown", &[], Some(3.0))
            .await
            .unwrap();
        assert_eq!(browser.inputs(), vec![key("ArrowDown"); 3]);

        // Below one still presses once; a fraction rounds up like `i < repeat`.
        human
            .press_key(&ctx.browser, "Space", &[], Some(0.0))
            .await
            .unwrap();
        human
            .press_key(&ctx.browser, "x", &[], Some(1.5))
            .await
            .unwrap();
        assert_eq!(browser.inputs()[3..], [key(" "), key("x"), key("x")]);
    }

    #[tokio::test(start_paused = true)]
    async fn 모르는_키와_조합키가_붙은_키는_보내지_않는다() {
        let focused = || Ok(json!(true));
        let (browser, ctx) = context(FakeBrowser::answering([focused(), focused(), focused()]));
        let human = HumanInput::default();
        assert!(
            human
                .press_key(&ctx.browser, "Esc", &[], None)
                .await
                .is_err_and(|e| e.starts_with("unknown key \"Esc\""))
        );
        assert_eq!(
            human.press_key(&ctx.browser, "a", &["Hyper"], None).await,
            Err("unknown modifier \"Hyper\" — use Alt, Control, Meta or Shift".to_owned())
        );
        assert_eq!(
            human.press_key(&ctx.browser, "a", &["Control"], None).await,
            Err(unsupported("pressing a key with modifiers held"))
        );
        assert!(browser.inputs().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn 글자는_하나씩_입력하고_줄바꿈과_제출은_엔터_키다() {
        let (browser, ctx) = context(FakeBrowser::new());
        let human = HumanInput::default();
        let text = |text: &str| Input::Text {
            text: text.to_owned(),
        };
        let enter = Input::Key {
            key: "Enter".to_owned(),
        };

        human
            .type_text(&ctx.browser, "a 한😀\n", true)
            .await
            .unwrap();
        assert_eq!(
            browser.inputs(),
            [
                text("a"),
                text(" "),
                text("한"),
                text("😀"),
                enter.clone(),
                enter
            ]
        );
        settle_overlay().await;
        assert_eq!(browser.overlay().len(), 5);

        let (browser, ctx) = context(FakeBrowser::new());
        human.type_text(&ctx.browser, "", false).await.unwrap();
        assert!(browser.inputs().is_empty());
    }

    fn wheel_total(inputs: &[Input], at: Point) -> f64 {
        inputs
            .iter()
            .map(|input| match input {
                Input::Wheel { x, y, dx, dy } => {
                    assert_eq!((*x, *y, *dx), (at.x, at.y, 0.0));
                    assert!(dy.abs() <= 115.0);
                    *dy
                }
                other => panic!("not a wheel: {other:?}"),
            })
            .sum()
    }

    #[tokio::test(start_paused = true)]
    async fn 스크롤은_휠_크기_조각으로_나눠_보내고_위치를_돌려준다() {
        let (browser, ctx) = context(FakeBrowser::answering([Ok(json!({ "scrollY": 950 }))]));
        let human = human_at(40.0, 70.0);
        assert_eq!(human.scroll(&ctx.browser, 950.0, None).await, Ok(950.0));
        let inputs = browser.inputs();
        // 85–115px a chunk.
        assert!((9..=12).contains(&inputs.len()), "{}", inputs.len());
        let total = wheel_total(&inputs, Point { x: 40.0, y: 70.0 });
        assert!((total - 950.0).abs() < 1e-6);
        assert_eq!(browser.calls(), ["evaluate:({ scrollY: window.scrollY })"]);
    }

    #[tokio::test(start_paused = true)]
    async fn 절대_위치는_현재_위치와의_차이만큼_스크롤한다() {
        let (browser, ctx) = context(FakeBrowser::answering([
            Ok(json!(500)),
            Ok(json!({ "scrollY": 200.5 })),
        ]));
        let human = human_at(40.0, 70.0);
        // The relative delta is ignored once an absolute position is given.
        assert_eq!(
            human.scroll(&ctx.browser, 9999.0, Some(200.0)).await,
            Ok(200.5)
        );
        let total = wheel_total(&browser.inputs(), Point { x: 40.0, y: 70.0 });
        assert!((total + 300.0).abs() < 1e-6);
        assert_eq!(
            browser.calls(),
            [
                "evaluate:window.scrollY",
                "evaluate:({ scrollY: window.scrollY })"
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 움직일_거리가_없거나_알_수_없으면_휠을_보내지_않는다() {
        // The page did not say where it is: the difference is NaN.
        let (browser, ctx) = context(FakeBrowser::answering([
            Ok(json!(null)),
            Ok(json!({ "scrollY": 0 })),
        ]));
        let human = HumanInput::default();
        assert_eq!(human.scroll(&ctx.browser, 0.0, Some(100.0)).await, Ok(0.0));
        assert!(browser.inputs().is_empty());

        let (browser, ctx) = context(FakeBrowser::answering([Ok(json!("nothing"))]));
        assert_eq!(
            human.scroll(&ctx.browser, 0.0, None).await,
            Err("the page did not report its scroll position".to_owned())
        );
        assert!(browser.inputs().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn 입력이_실패하면_그_오류를_돌려준다() {
        let (_, ctx) = context(FakeBrowser {
            fail_input: true,
            ..FakeBrowser::new()
        });
        let human = HumanInput::default();
        let failed = Err("input is broken".to_owned());
        assert_eq!(human.mouse_move(&ctx.browser, 1.0, 1.0).await, failed);
        assert_eq!(
            human
                .press_release(&ctx.browser, 1.0, 1.0, Mouse::default())
                .await,
            failed
        );
        assert_eq!(human.type_text(&ctx.browser, "a", false).await, failed);
        assert_eq!(
            human.scroll(&ctx.browser, 10.0, None).await,
            Err("input is broken".to_owned())
        );
    }
}
