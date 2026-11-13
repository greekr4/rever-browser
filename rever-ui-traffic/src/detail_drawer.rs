//! Port of `src/renderer/src/components/DetailDrawer.tsx` — the panel that
//! slides in from the right edge with the request the list has open. It closes on
//! Escape, and its left edge drags to resize.
//!
//! The React component keeps the width in localStorage; this view takes the saved
//! value from its host and reports every change as [`DetailDrawerEvent::Resized`].

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, BoxShadow, Context, DragMoveEvent, Empty, Entity, EventEmitter,
    MouseButton, MouseDownEvent, Subscription, Window, div, point, prelude::*, px, rgba,
};
use rever_core::traffic::TrafficStore;
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::ThemeMode;

use crate::json_view::is_js_whitespace;
use crate::store::TrafficState;
use crate::traffic_detail_drawer::{TrafficDetailDrawer, TrafficDetailDrawerEvent};
use crate::ui::{Tokens, clickable};

pub const INITIAL_WIDTH: f32 = 480.0;
pub const MIN_WIDTH: f32 = 320.0;
pub const MAX_WIDTH: f32 = 720.0;
/// The key the Electron app keeps the width under.
pub const WIDTH_STORAGE_KEY: &str = "rev:detail-w";
/// Long enough for the spring below to come to rest.
const SLIDE: Duration = Duration::from_millis(600);

/// The width to start with, given what was saved last time. Reads the value the
/// way `parseInt(saved, 10)` does: leading whitespace, a sign, then digits.
pub fn restore_width(saved: Option<&str>) -> f32 {
    let Some(saved) = saved else {
        return INITIAL_WIDTH;
    };
    let text = saved.trim_start_matches(is_js_whitespace);
    let (negative, text) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let digits = text
        .find(|c: char| !c.is_ascii_digit())
        .map_or(text, |end| text.get(..end).unwrap_or(text));
    match digits.parse::<f64>() {
        Ok(value) if value.is_finite() => {
            let value = if negative { -value } else { value };
            value.clamp(MIN_WIDTH.into(), MAX_WIDTH.into()) as f32
        }
        _ => INITIAL_WIDTH,
    }
}

/// The width while the left edge is dragged: moving left by a pixel adds one.
pub fn dragged_width(start_width: f32, start_x: f32, x: f32) -> f32 {
    (start_width + (start_x - x)).clamp(MIN_WIDTH, MAX_WIDTH)
}

/// How far the slide has come after `seconds`, from 0 to 1: the spring the React
/// drawer animates with (stiffness 320, damping 32, mass 1). It overshoots by a
/// fraction of a percent before it settles.
pub fn spring(seconds: f32) -> f32 {
    const STIFFNESS: f32 = 320.0;
    const DAMPING: f32 = 32.0;
    let t = seconds.max(0.0);
    let omega = STIFFNESS.sqrt();
    let zeta = DAMPING / (2.0 * omega);
    let damped = omega * (1.0 - zeta * zeta).sqrt();
    let decay = (-zeta * omega * t).exp();
    1.0 - decay * ((damped * t).cos() + zeta * omega / damped * (damped * t).sin())
}

/// What the drawer asks of its host.
#[derive(Clone, Debug, PartialEq)]
pub enum DetailDrawerEvent {
    /// Put this text in the chat composer ("Send to chat").
    PushDraft(String),
    /// The width after a drag, to save under [`WIDTH_STORAGE_KEY`].
    Resized(f32),
}

/// Marks a drag of the left edge.
struct SplitterDrag;

struct Shown {
    request_id: String,
    view: Entity<TrafficDetailDrawer>,
    /// Sliding out: the detail is closed, the panel is still on its way off.
    closing: bool,
    /// False for a drawer that was made with a detail already open: it has
    /// nowhere to slide in from, so it is simply there.
    slides: bool,
}

pub struct DetailDrawer {
    store: Arc<Mutex<TrafficStore>>,
    state: Entity<TrafficState>,
    width: f32,
    /// Pointer x and width when the edge was grabbed.
    drag_start: Option<(f32, f32)>,
    shown: Option<Shown>,
    /// Counts slides, so each one animates from its start and a stale "slid out"
    /// timer can tell it was overtaken.
    slide: usize,
    lang: Lang,
    theme_mode: ThemeMode,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<DetailDrawerEvent> for DetailDrawer {}

impl DetailDrawer {
    /// `saved_width` is what the host stored on the last [`DetailDrawerEvent::Resized`].
    pub fn new(
        store: Arc<Mutex<TrafficStore>>,
        state: Entity<TrafficState>,
        saved_width: Option<&str>,
        lang: Lang,
        theme_mode: ThemeMode,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = [
            cx.observe(&state, |this, _, cx| this.follow_state(true, cx)),
            // The React component listens on the window, so Escape closes the
            // drawer wherever the focus is.
            cx.observe_keystrokes(|this, event, _, cx| {
                if event.keystroke.key == "escape" {
                    this.close(cx);
                }
            }),
        ];
        let mut drawer = Self {
            store,
            state,
            width: restore_width(saved_width),
            drag_start: None,
            shown: None,
            slide: 0,
            lang,
            theme_mode,
            _subscriptions: subscriptions,
        };
        drawer.follow_state(false, cx);
        drawer
    }

    pub fn width(&self) -> f32 {
        self.width
    }

    /// The open request's view, if any.
    pub fn content(&self) -> Option<&Entity<TrafficDetailDrawer>> {
        self.shown.as_ref().map(|shown| &shown.view)
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.state.read(cx).detail_id.is_some() {
            self.state.update(cx, |state, cx| {
                state.close_detail();
                cx.notify();
            });
        }
    }

    /// Show what the shared state says is open.
    fn follow_state(&mut self, slides: bool, cx: &mut Context<Self>) {
        let detail_id = self.state.read(cx).detail_id.clone();
        match detail_id {
            Some(request_id) => {
                let entering = self.shown.as_ref().is_none_or(|shown| shown.closing);
                let changed = self
                    .shown
                    .as_ref()
                    .is_none_or(|shown| shown.request_id != request_id);
                if entering || changed {
                    let (lang, theme_mode) = (self.lang, self.theme_mode);
                    let view = cx.new(|_| {
                        TrafficDetailDrawer::new(&self.store, &request_id, lang, theme_mode)
                    });
                    cx.subscribe(
                        &view,
                        |this, _, event: &TrafficDetailDrawerEvent, cx| match event {
                            TrafficDetailDrawerEvent::PushDraft(text) => {
                                cx.emit(DetailDrawerEvent::PushDraft(text.clone()))
                            }
                            TrafficDetailDrawerEvent::Close => this.close(cx),
                        },
                    )
                    .detach();
                    if entering {
                        self.slide += 1;
                    }
                    self.shown = Some(Shown {
                        request_id,
                        view,
                        closing: false,
                        // Changing the request of an open drawer keeps how it got there.
                        slides: if entering {
                            slides
                        } else {
                            self.shown.as_ref().is_some_and(|shown| shown.slides)
                        },
                    });
                }
            }
            None => {
                if let Some(shown) = self.shown.as_mut().filter(|shown| !shown.closing) {
                    shown.closing = true;
                    shown.slides = true;
                    self.slide += 1;
                    let slide = self.slide;
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(SLIDE).await;
                        let _ = this.update(cx, |this, cx| {
                            if this.slide == slide {
                                this.shown = None;
                                cx.notify();
                            }
                        });
                    })
                    .detach();
                }
            }
        }
        cx.notify();
    }

    fn on_drag_move(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some((start_x, start_width)) = self.drag_start else {
            return;
        };
        let width = dragged_width(start_width, start_x, x);
        if width != self.width {
            self.width = width;
            cx.emit(DetailDrawerEvent::Resized(width));
            cx.notify();
        }
    }
}

impl Render for DetailDrawer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(shown) = &self.shown else {
            return Empty.into_any_element();
        };
        let t = Tokens::resolve(self.theme_mode, window);
        let (width, closing, slides) = (self.width, shown.closing, shown.slides);
        // `.detail-drawer`; the host gives it a positioned parent to pin to.
        let drawer = div()
            .id("detail-drawer")
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(width))
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(t.glass_panel)
            .border_l_1()
            .border_color(t.glass_hairline)
            .shadow(vec![BoxShadow {
                color: t.shadow.into(),
                offset: point(px(-12.0), px(0.0)),
                blur_radius: px(40.0),
                spread_radius: px(0.0),
            }])
            // What lies under the drawer must not take its clicks.
            .occlude()
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<SplitterDrag>, _, cx| {
                    this.on_drag_move(f32::from(event.event.position.x), cx)
                }),
            )
            .child(shown.view.clone())
            .child(
                // `.detail-drawer-splitter`: half of it hangs outside and is clipped.
                clickable("detail-drawer-splitter")
                    .absolute()
                    .left(px(-3.0))
                    .top_0()
                    .bottom_0()
                    .w(px(6.0))
                    .cursor_ew_resize()
                    .hover(|style| style.bg(rgba(0x4a8fff66)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, _| {
                            this.drag_start = Some((f32::from(event.position.x), this.width));
                        }),
                    )
                    .on_drag(SplitterDrag, |_, _, _, cx| cx.new(|_| Empty)),
            );
        if !slides {
            return drawer.into_any_element();
        }
        // Linear time in, the spring applied here: GPUI wants an easing that
        // stays within 0..1, and the spring overshoots.
        drawer
            .with_animation(
                ("detail-drawer-slide", self.slide),
                Animation::new(SLIDE),
                move |drawer, delta| {
                    let progress = spring(delta * SLIDE.as_secs_f32());
                    let hidden = if closing { progress } else { 1.0 - progress };
                    drawer.right(px(-hidden * width))
                },
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 저장된_폭이_없으면_기본_폭으로_연다() {
        assert_eq!(restore_width(None), 480.0);
        assert_eq!(restore_width(Some("")), 480.0);
        assert_eq!(restore_width(Some("abc")), 480.0);
        assert_eq!(restore_width(Some("px500")), 480.0);
        assert_eq!(restore_width(Some("-")), 480.0);
    }

    #[test]
    fn 저장된_폭은_parse_int처럼_읽는다() {
        assert_eq!(restore_width(Some("512")), 512.0);
        assert_eq!(restore_width(Some("  600px")), 600.0);
        assert_eq!(restore_width(Some("512.9")), 512.0);
        assert_eq!(restore_width(Some("+400")), 400.0);
        assert_eq!(restore_width(Some("\u{feff}\n640")), 640.0);
    }

    #[test]
    fn 저장된_폭은_허용_범위로_자른다() {
        assert_eq!(restore_width(Some("100")), 320.0);
        assert_eq!(restore_width(Some("-500")), 320.0);
        assert_eq!(restore_width(Some("0")), 320.0);
        assert_eq!(restore_width(Some("5000")), 720.0);
        assert_eq!(restore_width(Some("320")), 320.0);
        assert_eq!(restore_width(Some("720")), 720.0);
        assert_eq!(restore_width(Some(&"9".repeat(50))), 720.0);
        // More digits than a double holds is not a finite number: back to the default.
        assert_eq!(restore_width(Some(&"9".repeat(400))), 480.0);
    }

    #[test]
    fn 왼쪽으로_끌면_넓어지고_오른쪽으로_끌면_좁아진다() {
        assert_eq!(dragged_width(480.0, 900.0, 850.0), 530.0);
        assert_eq!(dragged_width(480.0, 900.0, 960.0), 420.0);
        assert_eq!(dragged_width(480.0, 900.0, 900.0), 480.0);
    }

    #[test]
    fn 끌어도_폭은_범위를_벗어나지_않는다() {
        assert_eq!(dragged_width(480.0, 900.0, 0.0), 720.0);
        assert_eq!(dragged_width(480.0, 900.0, 2000.0), 320.0);
    }

    #[test]
    fn 스프링은_0에서_시작해_1에_멈춘다() {
        assert_eq!(spring(0.0), 0.0);
        assert_eq!(spring(-1.0), 0.0);
        assert!((spring(SLIDE.as_secs_f32()) - 1.0).abs() < 0.001);
        // Most of the way within the first fifth of a second.
        assert!(spring(0.2) > 0.8);
        // Slightly underdamped: it passes 1, but never by as much as half a percent.
        let peak = (0..=600)
            .map(|ms| spring(ms as f32 / 1000.0))
            .fold(0.0_f32, f32::max);
        assert!(peak > 1.0 && peak < 1.005, "peak {peak}");
    }

    // ── The view, in a test window ──────────────────────────────────────────

    use crate::traffic_detail_drawer::Tab;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use rever_core::traffic::RequestPatch;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Opened<'a> {
        drawer: Entity<DetailDrawer>,
        state: Entity<TrafficState>,
        events: Rc<RefCell<Vec<DetailDrawerEvent>>>,
        cx: &'a mut VisualTestContext,
    }

    /// The drawer alone in a window over a store holding requests `r1` and `r2`.
    /// With `detail` the drawer is made already open, so it is in place at once.
    fn open<'a>(detail: Option<&str>, cx: &'a mut TestAppContext) -> Opened<'a> {
        let mut store = TrafficStore::default();
        for id in ["r1", "r2"] {
            store.upsert(
                RequestPatch {
                    request_id: id.into(),
                    url: Some(format!("https://api.test/{id}")),
                    method: Some("GET".into()),
                    resource_type: Some("XHR".into()),
                    status: Some(200),
                    ..RequestPatch::default()
                },
                1_000,
            );
        }
        let store = Arc::new(Mutex::new(store));
        let state = cx.new(|_| TrafficState {
            detail_id: detail.map(str::to_owned),
            ..TrafficState::default()
        });
        let (drawer, cx) = cx.add_window_view({
            let state = state.clone();
            move |_, cx| DetailDrawer::new(store, state, None, Lang::En, ThemeMode::Light, cx)
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        cx.update(|_, cx| {
            let events = events.clone();
            cx.subscribe(&drawer, move |_, event: &DetailDrawerEvent, _| {
                events.borrow_mut().push(event.clone())
            })
            .detach();
        });
        Opened {
            drawer,
            state,
            events,
            cx,
        }
    }

    impl Opened<'_> {
        fn click(&mut self, selector: &'static str) {
            let at = self
                .cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} is not on screen"))
                .center();
            self.cx.simulate_click(at, Modifiers::none());
        }

        fn show(&mut self, id: &str) {
            self.state.update(self.cx, |state, cx| {
                state.open_detail(id);
                cx.notify();
            });
            self.cx.run_until_parked();
        }

        fn detail(&mut self) -> Option<String> {
            self.state
                .read_with(self.cx, |state, _| state.detail_id.clone())
        }

        /// (request shown, sliding out)
        fn shown(&mut self) -> Option<(String, bool)> {
            self.drawer.read_with(self.cx, |drawer, _| {
                let shown = drawer.shown.as_ref()?;
                Some((shown.request_id.clone(), shown.closing))
            })
        }

        fn tab(&mut self) -> Option<Tab> {
            self.drawer.read_with(self.cx, |drawer, cx| {
                drawer.content().map(|content| content.read(cx).tab())
            })
        }
    }

    #[gpui::test]
    fn 상세가_없으면_아무것도_그리지_않는다(cx: &mut TestAppContext) {
        let mut t = open(None, cx);
        assert_eq!(t.shown(), None);
        assert!(t.cx.debug_bounds("detail-close").is_none());
        t.show("r1");
        assert_eq!(t.shown(), Some(("r1".into(), false)));
        assert_eq!(t.tab(), Some(Tab::Overview));
    }

    #[gpui::test]
    fn 닫기_버튼은_상세를_닫고_밀려난_뒤에_사라진다(cx: &mut TestAppContext) {
        let mut t = open(Some("r1"), cx);
        t.click("detail-close");
        assert_eq!(t.detail(), None);
        // Still there while it slides out.
        assert_eq!(t.shown(), Some(("r1".into(), true)));
        t.cx.executor().advance_clock(SLIDE);
        t.cx.run_until_parked();
        assert_eq!(t.shown(), None);
    }

    #[gpui::test]
    fn 밀려나는_중에_다시_열면_남아_있는다(cx: &mut TestAppContext) {
        let mut t = open(Some("r1"), cx);
        t.click("detail-close");
        t.show("r2");
        assert_eq!(t.shown(), Some(("r2".into(), false)));
        // The first slide's timer must not remove the reopened drawer.
        t.cx.executor().advance_clock(SLIDE * 2);
        t.cx.run_until_parked();
        assert_eq!(t.shown(), Some(("r2".into(), false)));
    }

    #[gpui::test]
    fn esc는_상세를_닫는다(cx: &mut TestAppContext) {
        let mut t = open(Some("r1"), cx);
        t.cx.simulate_keystrokes("escape");
        assert_eq!(t.detail(), None);
        assert_eq!(t.shown(), Some(("r1".into(), true)));
        // With nothing open the key does nothing.
        t.cx.simulate_keystrokes("escape");
        assert_eq!(t.detail(), None);
    }

    #[gpui::test]
    fn 다른_요청을_열면_개요_탭으로_돌아간다(cx: &mut TestAppContext) {
        let mut t = open(Some("r1"), cx);
        t.click("headers");
        assert_eq!(t.tab(), Some(Tab::Headers));
        t.show("r2");
        assert_eq!(t.shown(), Some(("r2".into(), false)));
        assert_eq!(t.tab(), Some(Tab::Overview));
        // Showing the same request again keeps the view and its tab.
        t.click("body");
        t.show("r2");
        assert_eq!(t.tab(), Some(Tab::Body));
    }

    #[gpui::test]
    fn 채팅으로_보내기는_초안을_위로_전한다(cx: &mut TestAppContext) {
        let mut t = open(Some("r1"), cx);
        t.click("detail-send");
        assert_eq!(
            *t.events.borrow(),
            [DetailDrawerEvent::PushDraft(
                "Analyze this request: GET https://api.test/r1 (id: r1, status: 200)\n\nQuestion: "
                    .into()
            )]
        );
    }

    #[gpui::test]
    fn 왼쪽_가장자리를_끌면_폭이_바뀌고_알린다(cx: &mut TestAppContext) {
        let mut t = open(Some("r1"), cx);
        assert_eq!(t.drawer.read_with(t.cx, |drawer, _| drawer.width()), 480.0);
        let drag = |t: &mut Opened, dx: f32| {
            let splitter = t.cx.debug_bounds("detail-drawer-splitter").unwrap();
            // The half inside the drawer; the other half is clipped away.
            let grab = point(splitter.right() - px(1.0), splitter.center().y);
            let (left, none) = (MouseButton::Left, Modifiers::none());
            t.cx.simulate_mouse_down(grab, left, none);
            t.cx.simulate_mouse_move(grab + point(px(dx / 2.0), px(0.0)), left, none);
            t.cx.simulate_mouse_move(grab + point(px(dx), px(0.0)), left, none);
            t.cx.simulate_mouse_up(grab + point(px(dx), px(0.0)), left, none);
            t.drawer.read_with(t.cx, |drawer, _| drawer.width())
        };
        assert_eq!(drag(&mut t, -50.0), 530.0);
        assert_eq!(
            t.events.borrow().last(),
            Some(&DetailDrawerEvent::Resized(530.0))
        );
        assert_eq!(drag(&mut t, 90.0), 440.0);
        assert_eq!(drag(&mut t, 600.0), MIN_WIDTH);
        assert_eq!(drag(&mut t, -900.0), MAX_WIDTH);
    }
}
