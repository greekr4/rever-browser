//! Port of `src/renderer/src/components/CopyToast.tsx` — "Copy Element" confirmation:
//! outline the picked element with a "Copied!" badge at its real position, plus a brief
//! bottom toast confirming the clipboard write. The host calls
//! [`CopyToast::element_copied`] where the React component listened to
//! `window.rev.onElementCopied`; the view clears itself when the (longest) toast
//! animation ends.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, App, Context, Corner, FontWeight, Rgba, Window, anchored, deferred,
    div, point, prelude::*, px, rgb, rgba,
};
use serde::Deserialize;

use crate::ui::{LookFn, Tokens, Z_COPY_BOX, Z_TOAST, keyframe, shadow, text};

/// `animation: copy-box 1.3s ease forwards`
pub const BOX_MS: u64 = 1300;
/// `animation: copy-clip-toast 1.6s ease forwards`
pub const TOAST_MS: u64 = 1600;

const BOX_OPACITY: &[(f32, f32)] = &[(0.0, 0.0), (0.12, 1.0), (0.75, 1.0), (1.0, 0.0)];
const TOAST_OPACITY: &[(f32, f32)] = &[(0.0, 0.0), (0.1, 1.0), (0.82, 1.0), (1.0, 0.0)];
/// The `translate` y of the toast's keyframes, in pixels (down is positive).
const TOAST_SHIFT: &[(f32, f32)] = &[(0.0, 10.0), (0.1, 0.0), (0.82, 0.0), (1.0, -4.0)];

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// The payload of the main process's `element-copied` message.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ElementCopied {
    /// The picked element's rect in page coordinates.
    pub rect: Option<Rect>,
    pub selector: Option<String>,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}

/// Where the outline goes, in window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CopiedBox {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

/// The on-screen rect of the page the user is looking at, if it is laid out with a
/// size. The React code searches the DOM for the visible `<webview>`; here the host
/// names it (`visible`), and only the size check is left.
pub fn active_webview_rect(visible: Option<Rect>) -> Option<Rect> {
    visible.filter(|rect| rect.width > 0.0 && rect.height > 0.0)
}

/// Map the picked element's in-page rect to window coordinates. No outline without
/// a rect or without a visible page.
pub fn copied_box(rect: Option<Rect>, webview: Option<Rect>) -> Option<CopiedBox> {
    let (rect, webview) = (rect?, active_webview_rect(webview)?);
    Some(CopiedBox {
        left: webview.x + rect.x,
        top: webview.y + rect.y,
        width: rect.width,
        height: rect.height,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Copied {
    pub outline: Option<CopiedBox>,
    /// A new key restarts both animations, as a changed React `key` remounts the nodes.
    pub key: u64,
}

/// What is on screen. Kept apart from the view so it can be tested without a window.
#[derive(Default, Debug)]
pub struct CopyState {
    copied: Option<Copied>,
    seq: u64,
}

impl CopyState {
    pub fn copied(&self) -> Option<Copied> {
        self.copied
    }

    /// Show a confirmation; the returned key is what [`Self::animation_ended`] takes.
    pub fn show(&mut self, outline: Option<CopiedBox>) -> u64 {
        self.seq += 1;
        self.copied = Some(Copied {
            outline,
            key: self.seq,
        });
        self.seq
    }

    /// The toast animation of `key` ran out. A newer copy replaced the nodes of an
    /// older one, whose animation therefore never ends: only the current key clears.
    pub fn animation_ended(&mut self, key: u64) -> bool {
        let current = self.copied.is_some_and(|copied| copied.key == key);
        if current {
            self.copied = None;
        }
        current
    }
}

/// The visible page's rect in window coordinates, `None` when no page is showing.
type PageRect = Box<dyn Fn(&App) -> Option<Rect>>;

pub struct CopyToast {
    state: CopyState,
    /// The visible page's rect in window coordinates, `None` when no page is showing.
    webview_rect: PageRect,
    look: LookFn,
}

impl CopyToast {
    pub fn new(webview_rect: impl Fn(&App) -> Option<Rect> + 'static, look: LookFn) -> Self {
        Self {
            state: CopyState::default(),
            webview_rect: Box::new(webview_rect),
            look,
        }
    }

    pub fn element_copied(&mut self, payload: &ElementCopied, cx: &mut Context<Self>) {
        let key = self
            .state
            .show(copied_box(payload.rect, (self.webview_rect)(cx)));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(TOAST_MS))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.state.animation_ended(key) {
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..color }
}

impl Render for CopyToast {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Out of the host's layout flow: both pieces place themselves in the window.
        let root = div().absolute();
        let Some(copied) = self.state.copied() else {
            return root;
        };
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let viewport = window.viewport_size();
        // No listeners on any of this: like `pointer-events: none`, clicks pass through.

        let outline = copied.outline.map(|outline| {
            // `.copy-box`: a content-box, so the 2px border sits outside the element's size.
            let frame = div()
                .debug_selector(|| "copy-box".into())
                .w(px(outline.width + 4.0))
                .h(px(outline.height + 4.0))
                .border_2()
                .border_color(theme.accent)
                .rounded(px(3.0))
                .bg(with_alpha(theme.accent, 0.12))
                .child(
                    // `.copy-box-label`
                    text(11.0)
                        .absolute()
                        .left(px(-2.0))
                        .top(px(-22.0))
                        .px(px(8.0))
                        .py(px(2.0))
                        .rounded(px(4.0))
                        .bg(theme.accent)
                        .text_color(rgb(0xffffff))
                        .font_weight(FontWeight::BOLD)
                        .whitespace_nowrap()
                        .shadow(vec![shadow(2.0, 8.0, rgba(0x0000004d))])
                        .child(look.lang.t("copy.copied")),
                );
            // The `0 0 0 2px` ring of the CSS box-shadow, as a border around the frame.
            // A GPUI shadow is painted under the whole box, not only outside it, and
            // would darken the see-through frame; so the blurred second shadow is left out.
            let ring = div()
                .border_2()
                .border_color(rgba(0x00000026))
                .rounded(px(5.0))
                .child(frame)
                .with_animation(
                    ("copy-box", copied.key),
                    Animation::new(Duration::from_millis(BOX_MS)),
                    |ring, progress| ring.opacity(keyframe(BOX_OPACITY, progress)),
                );
            deferred(
                anchored()
                    .position(point(px(outline.left - 2.0), px(outline.top - 2.0)))
                    .child(ring),
            )
            .with_priority(Z_COPY_BOX)
        });

        // `.copy-clip-toast`: centered, 44px above the window's bottom edge.
        let toast = div()
            .w(viewport.width)
            .flex()
            .justify_center()
            .child(
                text(12.0)
                    .debug_selector(|| "copy-clip-toast".into())
                    .px(px(14.0))
                    .py(px(7.0))
                    .bg(theme.bg_bar)
                    .border_1()
                    .border_color(theme.border)
                    .rounded(px(8.0))
                    .shadow(vec![shadow(6.0, 24.0, rgba(0x0000004d))])
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .whitespace_nowrap()
                    .child(look.lang.t("copy.toClipboard")),
            )
            .with_animation(
                ("copy-clip-toast", copied.key),
                Animation::new(Duration::from_millis(TOAST_MS)),
                |toast, progress| {
                    toast
                        .pb(px(44.0 - keyframe(TOAST_SHIFT, progress)))
                        .opacity(keyframe(TOAST_OPACITY, progress))
                },
            );

        root.children(outline).child(
            deferred(
                anchored()
                    .anchor(Corner::BottomLeft)
                    .position(point(px(0.0), viewport.height))
                    .child(toast),
            )
            .with_priority(Z_TOAST),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: Rect = Rect {
        x: 0.0,
        y: 86.0,
        width: 1020.0,
        height: 700.0,
    };
    const PICKED: Rect = Rect {
        x: 40.0,
        y: 120.0,
        width: 200.0,
        height: 32.0,
    };

    #[test]
    fn 페이지_좌표를_창_좌표로_옮긴다() {
        assert_eq!(
            copied_box(Some(PICKED), Some(PAGE)),
            Some(CopiedBox {
                left: 40.0,
                top: 206.0,
                width: 200.0,
                height: 32.0
            })
        );
    }

    #[test]
    fn rect가_없으면_테두리를_그리지_않는다() {
        assert_eq!(copied_box(None, Some(PAGE)), None);
    }

    #[test]
    fn 보이는_웹뷰가_없으면_테두리를_그리지_않는다() {
        assert_eq!(copied_box(Some(PICKED), None), None);
    }

    #[test]
    fn 크기가_0인_웹뷰는_보이지_않는_것으로_친다() {
        let flat = Rect {
            height: 0.0,
            ..PAGE
        };
        let thin = Rect { width: 0.0, ..PAGE };
        assert_eq!(active_webview_rect(Some(flat)), None);
        assert_eq!(active_webview_rect(Some(thin)), None);
        assert_eq!(active_webview_rect(Some(PAGE)), Some(PAGE));
        assert_eq!(copied_box(Some(PICKED), Some(flat)), None);
    }

    #[test]
    fn 복사하면_확인이_뜨고_애니메이션이_끝나면_사라진다() {
        let mut state = CopyState::default();
        assert_eq!(state.copied(), None);
        let key = state.show(None);
        assert_eq!(state.copied(), Some(Copied { outline: None, key }));
        assert!(state.animation_ended(key));
        assert_eq!(state.copied(), None);
    }

    #[test]
    fn 새로_복사하면_앞선_확인의_종료는_무시한다() {
        let mut state = CopyState::default();
        let first = state.show(None);
        let second = state.show(copied_box(Some(PICKED), Some(PAGE)));
        assert_ne!(first, second);
        assert!(!state.animation_ended(first));
        assert_eq!(state.copied().map(|copied| copied.key), Some(second));
        assert!(state.animation_ended(second));
        assert!(!state.animation_ended(second));
    }

    #[test]
    fn 페이로드는_rect가_null이어도_읽힌다() {
        let with_rect: ElementCopied = serde_json::from_str(
            r##"{"rect":{"x":1,"y":2.5,"width":30,"height":4},"selector":"#a","ref":"r3"}"##,
        )
        .unwrap();
        assert_eq!(
            with_rect.rect,
            Some(Rect {
                x: 1.0,
                y: 2.5,
                width: 30.0,
                height: 4.0
            })
        );
        assert_eq!(with_rect.selector.as_deref(), Some("#a"));
        assert_eq!(with_rect.reference.as_deref(), Some("r3"));

        let without: ElementCopied =
            serde_json::from_str(r#"{"rect":null,"selector":null,"ref":null}"#).unwrap();
        assert_eq!(without.rect, None);
        assert!(serde_json::from_str::<ElementCopied>(r#"{"rect":"oops"}"#).is_err());
    }

    #[test]
    fn 애니메이션_시간은_css와_같다() {
        // The view clears on the toast's end, the longer of the two.
        assert_eq!(BOX_MS, 1300);
        assert_eq!(TOAST_MS, 1600);
    }
}

#[cfg(test)]
mod view_tests {
    use gpui::{Entity, TestAppContext, VisualTestContext};

    use super::*;
    use crate::ui::testing::{look, stage};

    const PAGE: Rect = Rect {
        x: 100.0,
        y: 80.0,
        width: 800.0,
        height: 600.0,
    };

    fn toast(cx: &mut TestAppContext) -> (Entity<CopyToast>, &mut VisualTestContext) {
        let (toast, _, cx) = stage(cx, |_| CopyToast::new(|_| Some(PAGE), look()));
        (toast, cx)
    }

    fn copy(rect: Option<Rect>, toast: &Entity<CopyToast>, cx: &mut VisualTestContext) {
        let payload = ElementCopied {
            rect,
            selector: None,
            reference: None,
        };
        toast.update(cx, |toast, cx| toast.element_copied(&payload, cx));
        cx.run_until_parked();
    }

    fn wait(ms: u64, cx: &mut VisualTestContext) {
        cx.executor().advance_clock(Duration::from_millis(ms));
        cx.run_until_parked();
    }

    fn showing(toast: &Entity<CopyToast>, cx: &mut VisualTestContext) -> bool {
        toast.read_with(cx, |toast, _| toast.state.copied().is_some())
    }

    #[gpui::test]
    fn 복사_전에는_아무것도_그리지_않는다(cx: &mut TestAppContext) {
        let (toast, cx) = toast(cx);
        assert!(!showing(&toast, cx));
        assert!(cx.debug_bounds("copy-clip-toast").is_none());
        assert!(cx.debug_bounds("copy-box").is_none());
    }

    #[gpui::test]
    fn 고른_요소의_자리에_테두리를_그린다(cx: &mut TestAppContext) {
        let (toast, cx) = toast(cx);
        let picked = Rect {
            x: 30.0,
            y: 40.0,
            width: 120.0,
            height: 24.0,
        };
        copy(Some(picked), &toast, cx);
        let frame = cx.debug_bounds("copy-box").unwrap();
        assert_eq!(frame.origin, point(px(130.0), px(120.0)));
        // The 2px border adds to the element's size on every side.
        assert_eq!(frame.size, gpui::size(px(124.0), px(28.0)));
    }

    #[gpui::test]
    fn rect가_없으면_아래_토스트만_띄운다(cx: &mut TestAppContext) {
        let (toast, cx) = toast(cx);
        copy(None, &toast, cx);
        assert!(cx.debug_bounds("copy-box").is_none());
        let clip = cx.debug_bounds("copy-clip-toast").unwrap();
        // Centered in the window.
        let viewport = cx.update(|window, _| window.viewport_size());
        assert!((clip.center().x - viewport.width / 2.0).abs() <= px(0.5));
    }

    #[gpui::test]
    fn 토스트_애니메이션이_끝나는_1600ms에_사라진다(cx: &mut TestAppContext) {
        let (toast, cx) = toast(cx);
        copy(None, &toast, cx);
        assert!(cx.debug_bounds("copy-clip-toast").is_some());
        wait(TOAST_MS - 1, cx);
        assert!(showing(&toast, cx));
        wait(1, cx);
        assert!(!showing(&toast, cx));
    }

    #[gpui::test]
    fn 다시_복사하면_그때부터_새로_1600ms를_센다(cx: &mut TestAppContext) {
        let (toast, cx) = toast(cx);
        copy(None, &toast, cx);
        wait(1000, cx);
        copy(None, &toast, cx);
        // The first copy's timer runs out here; the second one is still showing.
        wait(700, cx);
        assert!(showing(&toast, cx));
        wait(899, cx);
        assert!(showing(&toast, cx));
        wait(1, cx);
        assert!(!showing(&toast, cx));
    }
}
