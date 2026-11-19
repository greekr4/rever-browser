//! Port of `src/renderer/src/hooks/use-resizable.ts` — a pane size the user changes by
//! dragging a handle, kept between min and max and remembered under a storage key.
//!
//! [`Resizable`] is both the size state and the drag handle: the host puts the entity
//! where the splitter goes and reads [`Resizable::width`] for the pane next to it.

use std::rc::Rc;

use gpui::{
    Context, CursorStyle, DispatchPhase, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, Window, anchored, canvas, deferred, div, point, prelude::*, px, rgba,
};
use rever_ui_state::Storage;

use crate::ui::{LookFn, Tokens};

/// Which edge of the pane the handle sits on: dragging toward that side grows the pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Side {
    /// Handle on the left edge, dragging left increases the width.
    #[default]
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Axis {
    #[default]
    X,
    Y,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResizeOptions {
    pub initial: f32,
    pub min: f32,
    pub max: f32,
    /// Storage key to persist the size under.
    pub storage_key: Option<String>,
    pub side: Side,
    pub axis: Axis,
}

/// How the handle is drawn. The hook itself draws nothing; these are the two looks the
/// stylesheet gives the elements that call `startDrag`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HandleLook {
    /// `.splitter`: a 4px bar between two panes that turns accent while hovered or dragged.
    #[default]
    Bar,
    /// `.chip-panel-splitter` / `.detail-drawer-splitter`: an invisible 6px strip across
    /// the pane's edge (the parent must be `relative`), tinted while hovered.
    Edge,
}

/// `.splitter` thickness.
const BAR: f32 = 4.0;
/// How far `.splitter::before` extends the bar's grab area on each side.
const BAR_GRAB: f32 = 3.0;
/// Thickness of the edge strip, centered on the edge.
const EDGE: f32 = 6.0;

/// `Math.max(min, Math.min(max, n))`. Not `f64::clamp`: that panics when min > max,
/// where this gives min.
fn clamp(min: f64, max: f64, n: f64) -> f64 {
    min.max(max.min(n))
}

/// `parseInt(text, 10)`, `None` for NaN: leading whitespace, an optional sign, then
/// digits up to the first character that is not one.
pub fn parse_int(text: &str) -> Option<f64> {
    // JavaScript's whitespace: U+FEFF counts, U+0085 (whitespace for Rust) does not.
    let text =
        text.trim_start_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{FEFF}');
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let mut value: Option<f64> = None;
    for digit in digits.chars().map_while(|c| c.to_digit(10)) {
        value = Some(value.unwrap_or(0.0) * 10.0 + f64::from(digit));
    }
    value.map(|n| if negative { -n } else { n })
}

/// The size to start with: the saved one when it parses, clamped; else `initial` as given.
pub fn initial_width(options: &ResizeOptions, saved: Option<&str>) -> f32 {
    saved
        .filter(|saved| !saved.is_empty())
        .and_then(parse_int)
        .filter(|n| n.is_finite())
        .map(|n| clamp(f64::from(options.min), f64::from(options.max), n) as f32)
        .unwrap_or(options.initial)
}

/// The size while dragging: `start_width` plus how far the pointer moved toward the
/// handle's side since the press at `start_pos`.
pub fn dragged_width(options: &ResizeOptions, start_pos: f32, start_width: f32, pos: f32) -> f32 {
    let delta = match options.side {
        Side::Left | Side::Top => start_pos - pos,
        Side::Right | Side::Bottom => pos - start_pos,
    };
    clamp(
        f64::from(options.min),
        f64::from(options.max),
        f64::from(start_width + delta),
    ) as f32
}

/// `String(width)`, the form the size is stored in.
// shortcut: no exponent forms (1e21 and up, below 1e-6), upgrade if a size can get there.
pub fn stored_width(width: f32) -> String {
    if width.is_infinite() {
        return if width > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    // JavaScript prints -0 as "0".
    if width == 0.0 {
        return "0".to_owned();
    }
    width.to_string()
}

fn along(axis: Axis, position: Point<Pixels>) -> f32 {
    match axis {
        Axis::X => position.x.into(),
        Axis::Y => position.y.into(),
    }
}

pub struct Resizable {
    options: ResizeOptions,
    storage: Rc<dyn Storage>,
    look: LookFn,
    handle: HandleLook,
    width: f32,
    /// Pointer position along the axis and the size, both at the press.
    drag: Option<(f32, f32)>,
    hovered: bool,
}

impl Resizable {
    pub fn new(
        options: ResizeOptions,
        handle: HandleLook,
        storage: Rc<dyn Storage>,
        look: LookFn,
    ) -> Self {
        let saved = options
            .storage_key
            .as_deref()
            .and_then(|key| storage.get(key));
        let this = Self {
            width: initial_width(&options, saved.as_deref()),
            options,
            storage,
            look,
            handle,
            drag: None,
            hovered: false,
        };
        // The hook's effect also runs on mount: the starting size is stored right away.
        this.persist();
        this
    }

    /// The current size along the axis (the hook calls it `width` for both axes).
    pub fn width(&self) -> f32 {
        self.width
    }

    /// Set the size as given: like the hook's `setWidth`, this does not clamp.
    pub fn set_width(&mut self, width: f32, cx: &mut Context<Self>) {
        if width != self.width {
            self.width = width;
            self.persist();
            cx.notify();
        }
    }

    /// True while the handle is being dragged (`body.resizing` in the stylesheet): the
    /// host turns size transitions off for that time.
    pub fn is_resizing(&self) -> bool {
        self.drag.is_some()
    }

    fn persist(&self) {
        if let Some(key) = &self.options.storage_key {
            // A size that could not be stored is only lost on restart; the drag goes on.
            let _ = self.storage.set(key, &stored_width(self.width));
        }
    }

    fn start_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.drag = Some((along(self.options.axis, position), self.width));
        cx.notify();
    }

    fn drag_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some((start_pos, start_width)) = self.drag else {
            return;
        };
        let pos = along(self.options.axis, position);
        let next = dragged_width(&self.options, start_pos, start_width, pos);
        if next != self.width {
            self.width = next;
            cx.notify();
        }
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            // The hook stores every size on the way; the storage here rewrites and syncs
            // a file per write, so only the size the drag ends on is stored.
            self.persist();
            cx.notify();
        }
    }
}

impl Render for Resizable {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::resolve((self.look)(cx).theme_mode, window);
        let (axis, side) = (self.options.axis, self.options.side);
        let dragging = self.drag.is_some();
        let lit = dragging || self.hovered;
        let cursor = match axis {
            Axis::X => CursorStyle::ResizeColumn,
            Axis::Y => CursorStyle::ResizeUpDown,
        };
        // `.detail-drawer-splitter` asks for `ew-resize`; the hook's `col-resize` only
        // takes over for the time of the drag.
        let hover_cursor = match (self.handle, axis) {
            (HandleLook::Edge, Axis::X) => CursorStyle::ResizeLeftRight,
            _ => cursor,
        };

        let grab = div()
            .id("resize-grab")
            .debug_selector(|| "resize-handle".into())
            .absolute()
            .cursor(hover_cursor)
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.hovered = *hovered;
                cx.notify();
            }))
            // `onMouseDown` does not look at the button, so any of them starts the drag.
            .on_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                window.prevent_default();
                this.start_drag(event.position, cx);
            }));

        let handle = match self.handle {
            HandleLook::Bar => {
                let bar = div().relative().flex_none().bg(if lit {
                    tokens.theme.accent
                } else {
                    tokens.surface
                });
                match axis {
                    Axis::X => bar.w(px(BAR)).h_full().child(
                        grab.top_0()
                            .bottom_0()
                            .left(px(-BAR_GRAB))
                            .right(px(-BAR_GRAB)),
                    ),
                    Axis::Y => bar.h(px(BAR)).w_full().child(
                        grab.left_0()
                            .right_0()
                            .top(px(-BAR_GRAB))
                            .bottom(px(-BAR_GRAB)),
                    ),
                }
            }
            HandleLook::Edge => {
                // rgba(74, 143, 255, 0.4)
                let grab = grab.size_full().when(lit, |grab| grab.bg(rgba(0x4a8fff66)));
                let strip = div().absolute();
                let strip = match side {
                    Side::Left => strip.left(px(-EDGE / 2.0)).top_0().bottom_0().w(px(EDGE)),
                    Side::Right => strip.right(px(-EDGE / 2.0)).top_0().bottom_0().w(px(EDGE)),
                    Side::Top => strip.top(px(-EDGE / 2.0)).left_0().right_0().h(px(EDGE)),
                    Side::Bottom => strip.bottom(px(-EDGE / 2.0)).left_0().right_0().h(px(EDGE)),
                };
                strip.child(grab)
            }
        };
        // The hook listens on `document`: the drag follows the pointer anywhere, and
        // releasing any button ends it. The listeners are there in every frame, not only
        // while dragging: an event goes to the listeners of the last frame drawn, and a
        // release that comes before the frame after the press would leave the drag on.
        let (on_move, on_up) = (cx.entity(), cx.entity());
        let track = canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture {
                        on_move.update(cx, |this, cx| this.drag_to(event.position, cx));
                    }
                });
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture {
                        on_up.update(cx, |this, cx| this.end_drag(cx));
                    }
                });
            },
        )
        .absolute();
        let handle = handle.child(track);
        if !dragging {
            return handle;
        }
        // `body.resizing`: the resize cursor everywhere, and nothing under the pointer
        // (the page above all) reacts until the button is released.
        let viewport = window.viewport_size();
        let shield = div()
            .w(viewport.width)
            .h(viewport.height)
            .cursor(cursor)
            .occlude();
        handle.child(deferred(
            anchored().position(point(px(0.0), px(0.0))).child(shield),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(side: Side, axis: Axis) -> ResizeOptions {
        ResizeOptions {
            initial: 420.0,
            min: 300.0,
            max: 720.0,
            storage_key: Some("rev:chat-w".into()),
            side,
            axis,
        }
    }

    #[test]
    fn parse_int는_자바스크립트처럼_앞의_정수만_읽는다() {
        assert_eq!(parse_int("420"), Some(420.0));
        assert_eq!(parse_int("  420.7px"), Some(420.0));
        assert_eq!(parse_int("\u{FEFF}\t+12"), Some(12.0));
        assert_eq!(parse_int("-35abc"), Some(-35.0));
        assert_eq!(parse_int("1e3"), Some(1.0));
        assert_eq!(parse_int("007"), Some(7.0));
    }

    #[test]
    fn parse_int는_숫자로_시작하지_않으면_nan이다() {
        for text in [
            "", "abc", "-", "+", ".5", "Infinity", "NaN", "- 5", "\u{85}5",
        ] {
            assert_eq!(parse_int(text), None, "{text:?}");
        }
    }

    #[test]
    fn 저장된_값이_없으면_initial을_그대로_쓴다() {
        let opts = options(Side::Left, Axis::X);
        assert_eq!(initial_width(&opts, None), 420.0);
        assert_eq!(initial_width(&opts, Some("")), 420.0);
        assert_eq!(initial_width(&opts, Some("wide")), 420.0);
        // `initial` itself is never clamped.
        let outside = ResizeOptions {
            initial: 100.0,
            ..opts
        };
        assert_eq!(initial_width(&outside, None), 100.0);
    }

    #[test]
    fn 저장된_값은_min과_max_사이로_잘라_쓴다() {
        let opts = options(Side::Left, Axis::X);
        assert_eq!(initial_width(&opts, Some("500")), 500.0);
        assert_eq!(initial_width(&opts, Some("500.9")), 500.0);
        assert_eq!(initial_width(&opts, Some("10")), 300.0);
        assert_eq!(initial_width(&opts, Some("-10")), 300.0);
        assert_eq!(initial_width(&opts, Some("99999")), 720.0);
    }

    #[test]
    fn 유한하지_않은_저장값은_버린다() {
        let opts = options(Side::Left, Axis::X);
        // 400 digits overflow to Infinity, which `Number.isFinite` rejects.
        assert_eq!(initial_width(&opts, Some(&"9".repeat(400))), 420.0);
    }

    #[test]
    fn min이_max보다_크면_min이_이긴다() {
        let opts = ResizeOptions {
            min: 500.0,
            max: 400.0,
            ..options(Side::Left, Axis::X)
        };
        assert_eq!(initial_width(&opts, Some("450")), 500.0);
        assert_eq!(dragged_width(&opts, 0.0, 450.0, 10.0), 500.0);
    }

    #[test]
    fn 왼쪽과_위쪽_손잡이는_반대_방향으로_끌어야_커진다() {
        for side in [Side::Left, Side::Top] {
            let opts = options(side, Axis::X);
            assert_eq!(dragged_width(&opts, 800.0, 420.0, 750.0), 470.0);
            assert_eq!(dragged_width(&opts, 800.0, 420.0, 850.0), 370.0);
        }
    }

    #[test]
    fn 오른쪽과_아래쪽_손잡이는_같은_방향으로_끌어야_커진다() {
        for side in [Side::Right, Side::Bottom] {
            let opts = options(side, Axis::X);
            assert_eq!(dragged_width(&opts, 800.0, 420.0, 850.0), 470.0);
            assert_eq!(dragged_width(&opts, 800.0, 420.0, 750.0), 370.0);
        }
    }

    #[test]
    fn 끌어도_min과_max를_넘지_않는다() {
        let opts = options(Side::Left, Axis::X);
        assert_eq!(dragged_width(&opts, 800.0, 420.0, 0.0), 720.0);
        assert_eq!(dragged_width(&opts, 800.0, 420.0, 2000.0), 300.0);
        // Fractions of a pixel are kept.
        assert_eq!(dragged_width(&opts, 800.0, 420.0, 799.5), 420.5);
    }

    #[test]
    fn 저장_문자열은_자바스크립트_string과_같다() {
        assert_eq!(stored_width(420.0), "420");
        assert_eq!(stored_width(420.5), "420.5");
        assert_eq!(stored_width(-0.0), "0");
        assert_eq!(stored_width(f32::INFINITY), "Infinity");
        assert_eq!(stored_width(f32::NEG_INFINITY), "-Infinity");
        assert_eq!(stored_width(f32::NAN), "NaN");
    }

    #[test]
    fn 기본값은_왼쪽_손잡이와_x축이다() {
        assert_eq!(Side::default(), Side::Left);
        assert_eq!(Axis::default(), Axis::X);
    }
}

#[cfg(test)]
mod view_tests {
    use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, size};
    use rever_ui_state::MemoryStorage;

    use super::*;
    use crate::ui::testing::{center, look};

    /// A pane and its handle, laid out the way the option's side says.
    struct Stage {
        resize: Entity<Resizable>,
        axis: Axis,
        side: Side,
    }

    impl Render for Stage {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let size = px(self.resize.read(cx).width());
            let pane = div().debug_selector(|| "pane".into()).flex_none();
            let pane = match self.axis {
                Axis::X => pane.w(size).h_full(),
                Axis::Y => pane.h(size).w_full(),
            };
            let row = div().size_full().flex().child(div().flex_1());
            let row = match self.axis {
                Axis::X => row,
                Axis::Y => row.flex_col(),
            };
            match self.side {
                Side::Left | Side::Top => row.child(self.resize.clone()).child(pane),
                // An edge strip lives inside the pane it resizes.
                _ => row.child(pane.relative().child(self.resize.clone())),
            }
        }
    }

    fn stage(
        side: Side,
        axis: Axis,
        handle: HandleLook,
        storage: Rc<MemoryStorage>,
        cx: &mut TestAppContext,
    ) -> (Entity<Resizable>, &mut VisualTestContext) {
        let (stage, cx) = cx.add_window_view(move |_, cx| Stage {
            resize: cx.new(|_| {
                Resizable::new(
                    ResizeOptions {
                        initial: 420.0,
                        min: 300.0,
                        max: 720.0,
                        storage_key: Some("rev:chat-w".into()),
                        side,
                        axis,
                    },
                    handle,
                    storage,
                    look(),
                )
            }),
            axis,
            side,
        });
        cx.simulate_resize(size(px(1200.0), px(800.0)));
        cx.run_until_parked();
        let resize = stage.read_with(cx, |stage, _| stage.resize.clone());
        (resize, cx)
    }

    fn width(resize: &Entity<Resizable>, cx: &mut VisualTestContext) -> f32 {
        resize.read_with(cx, |resize, _| resize.width())
    }

    fn move_to(at: Point<Pixels>, cx: &mut VisualTestContext) {
        cx.simulate_mouse_move(at, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn 만들자마자_처음_크기를_저장한다(cx: &mut TestAppContext) {
        let storage = Rc::new(MemoryStorage::new());
        let (resize, cx) = stage(Side::Left, Axis::X, HandleLook::Bar, storage.clone(), cx);
        assert_eq!(width(&resize, cx), 420.0);
        assert_eq!(storage.get("rev:chat-w").as_deref(), Some("420"));
    }

    #[gpui::test]
    fn 저장된_크기로_시작한다(cx: &mut TestAppContext) {
        let storage = Rc::new(MemoryStorage::new());
        storage.set("rev:chat-w", "9000").unwrap();
        let (resize, cx) = stage(Side::Left, Axis::X, HandleLook::Bar, storage.clone(), cx);
        assert_eq!(width(&resize, cx), 720.0);
        assert_eq!(cx.debug_bounds("pane").unwrap().size.width, px(720.0));
        assert_eq!(storage.get("rev:chat-w").as_deref(), Some("720"));
    }

    #[gpui::test]
    fn 막대_손잡이는_4px이고_양옆_3px까지_잡힌다(cx: &mut TestAppContext) {
        let storage = Rc::new(MemoryStorage::new());
        let (resize, cx) = stage(Side::Left, Axis::X, HandleLook::Bar, storage, cx);
        let grab = cx.debug_bounds("resize-handle").unwrap();
        assert_eq!(grab.size.width, px(10.0));
        assert_eq!(grab.size.height, px(800.0));
        // The bar itself ends where the pane starts.
        let pane = cx.debug_bounds("pane").unwrap();
        assert_eq!(grab.right() - px(3.0), pane.left());
        assert_eq!(pane.left(), px(1200.0 - 420.0));

        // One pixel inside the grab area, outside the bar.
        let at = point(grab.left() + px(1.0), px(400.0));
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
        assert!(resize.read_with(cx, |resize, _| resize.is_resizing()));
    }

    #[gpui::test]
    fn 왼쪽_손잡이를_왼쪽으로_끌면_넓어지고_놓으면_저장한다(
        cx: &mut TestAppContext,
    ) {
        let storage = Rc::new(MemoryStorage::new());
        let (resize, cx) = stage(Side::Left, Axis::X, HandleLook::Bar, storage.clone(), cx);
        let at = center("resize-handle", cx);
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
        move_to(point(at.x - px(100.0), px(20.0)), cx);
        assert_eq!(width(&resize, cx), 520.0);
        assert_eq!(cx.debug_bounds("pane").unwrap().size.width, px(520.0));
        // Far from the handle, and past the maximum.
        move_to(point(px(5.0), px(790.0)), cx);
        assert_eq!(width(&resize, cx), 720.0);
        move_to(point(at.x - px(30.5), px(790.0)), cx);
        assert_eq!(width(&resize, cx), 450.5);

        cx.simulate_mouse_up(
            point(px(5.0), px(5.0)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.run_until_parked();
        assert!(!resize.read_with(cx, |resize, _| resize.is_resizing()));
        assert_eq!(storage.get("rev:chat-w").as_deref(), Some("450.5"));

        // Released: the pointer no longer moves the size.
        move_to(point(px(600.0), px(400.0)), cx);
        assert_eq!(width(&resize, cx), 450.5);
    }

    #[gpui::test]
    fn 어느_버튼으로든_끌_수_있고_어느_버튼을_떼도_끝난다(
        cx: &mut TestAppContext,
    ) {
        let storage = Rc::new(MemoryStorage::new());
        let (resize, cx) = stage(Side::Left, Axis::X, HandleLook::Bar, storage, cx);
        let at = center("resize-handle", cx);
        cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
        move_to(point(at.x + px(50.0), at.y), cx);
        assert_eq!(width(&resize, cx), 370.0);
        cx.simulate_mouse_up(at, MouseButton::Middle, Modifiers::none());
        cx.run_until_parked();
        assert!(!resize.read_with(cx, |resize, _| resize.is_resizing()));
    }

    #[gpui::test]
    fn 다음_프레임이_그려지기_전에_버튼을_떼도_끌기가_끝난다(
        cx: &mut TestAppContext,
    ) {
        let storage = Rc::new(MemoryStorage::new());
        let (resize, cx) = stage(Side::Left, Axis::X, HandleLook::Bar, storage, cx);
        let at = center("resize-handle", cx);
        // A quick tap: the release reaches the window before it draws again, so it goes
        // to the listeners of the frame drawn before the press. The tests' window redraws
        // after every event, so the press is made here without one.
        resize.update(cx, |resize, _| resize.drag = Some((at.x.into(), 420.0)));
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        assert!(!resize.read_with(cx, |resize, _| resize.is_resizing()));
        move_to(point(at.x - px(100.0), at.y), cx);
        assert_eq!(width(&resize, cx), 420.0);
    }

    #[gpui::test]
    fn 위쪽_가장자리_손잡이는_y축으로_끈다(cx: &mut TestAppContext) {
        let storage = Rc::new(MemoryStorage::new());
        let (resize, cx) = stage(Side::Top, Axis::Y, HandleLook::Bar, storage, cx);
        let grab = cx.debug_bounds("resize-handle").unwrap();
        assert_eq!(grab.size, size(px(1200.0), px(10.0)));
        let at = grab.center();
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
        // Sideways movement does nothing on the y axis.
        move_to(point(at.x + px(300.0), at.y), cx);
        assert_eq!(width(&resize, cx), 420.0);
        move_to(point(at.x, at.y - px(80.0)), cx);
        assert_eq!(width(&resize, cx), 500.0);
        assert_eq!(cx.debug_bounds("pane").unwrap().size.height, px(500.0));
    }

    #[gpui::test]
    fn 가장자리_띠는_패널_경계에_6px로_걸친다(cx: &mut TestAppContext) {
        let storage = Rc::new(MemoryStorage::new());
        let (resize, cx) = stage(Side::Right, Axis::X, HandleLook::Edge, storage, cx);
        let pane = cx.debug_bounds("pane").unwrap();
        let grab = cx.debug_bounds("resize-handle").unwrap();
        assert_eq!(grab.size.width, px(6.0));
        assert_eq!(grab.size.height, pane.size.height);
        assert_eq!(grab.center().x, pane.right());

        cx.simulate_mouse_down(grab.center(), MouseButton::Left, Modifiers::none());
        move_to(point(grab.center().x + px(40.0), px(10.0)), cx);
        assert_eq!(width(&resize, cx), 460.0);
    }

    #[gpui::test]
    fn set_width는_자르지_않고_바로_저장한다(cx: &mut TestAppContext) {
        let storage = Rc::new(MemoryStorage::new());
        let (resize, cx) = stage(Side::Left, Axis::X, HandleLook::Bar, storage.clone(), cx);
        resize.update(cx, |resize, cx| resize.set_width(100.0, cx));
        assert_eq!(width(&resize, cx), 100.0);
        assert_eq!(storage.get("rev:chat-w").as_deref(), Some("100"));
    }

    #[gpui::test]
    fn 저장_키가_없으면_아무것도_저장하지_않는다(cx: &mut TestAppContext) {
        let storage = Rc::new(MemoryStorage::new());
        let resize = cx.new(|_| {
            Resizable::new(
                ResizeOptions {
                    initial: 320.0,
                    min: 180.0,
                    max: 600.0,
                    storage_key: None,
                    side: Side::Top,
                    axis: Axis::Y,
                },
                HandleLook::Edge,
                storage.clone(),
                look(),
            )
        });
        resize.update(cx, |resize, cx| resize.set_width(200.0, cx));
        assert_eq!(storage.get("rev:bottom-h"), None);
        assert_eq!(resize.read_with(cx, |resize, _| resize.width()), 200.0);
    }
}
