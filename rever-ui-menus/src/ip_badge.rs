//! Port of `src/renderer/src/components/IpBadge.tsx` — always-on egress IP badge for
//! the titlebar. Shows the public IP as seen through the ACTIVE tab's session, so a tab
//! proxy (isolated partition) is reflected. Refreshes on creation and on right-click;
//! the host calls [`IpBadge::refresh`] on an active-tab change and after a proxy change
//! (the React effect's `activeId` and `refreshSignal` dependencies). A click masks the
//! display (e.g. before screen recording) without pausing the underlying refresh.

use std::rc::Rc;

use gpui::{App, Context, MouseButton, Task, Window, div, prelude::*, px};
use rever_ui_kit::i18n::Lang;
use serde::Deserialize;

use crate::ui::{LookFn, MONO, Tokens, fill, tooltip};

/// What `window.rev.getCurrentIp` resolves to.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct IpResult {
    pub ip: Option<String>,
    pub error: Option<String>,
}

/// Where the badge gets what the React component took from its stores and `window.rev`.
pub trait IpBadgeHost {
    /// The egress IP through the active tab's session. `Err` is a failed call (the
    /// rejected promise), which the badge shows like an `error` result.
    fn current_ip(&self, cx: &mut App) -> Task<Result<IpResult, String>>;
    /// `useIpVisibilityStore`'s `hidden`: persisted by the host, read on every render.
    fn hidden(&self, cx: &App) -> bool;
    fn toggle_hidden(&self, cx: &mut App);
}

/// The badge's state. Kept apart from the view so it can be tested without a window.
#[derive(Clone, Debug, PartialEq)]
pub struct IpState {
    ip: Option<String>,
    error: Option<String>,
    loading: bool,
    /// Monotonic request id — a late response from a superseded fetch is dropped.
    seq: u64,
}

impl Default for IpState {
    fn default() -> Self {
        Self {
            ip: None,
            error: None,
            loading: true,
            seq: 0,
        }
    }
}

impl IpState {
    /// A fetch starts; the returned id goes back into [`Self::finish`].
    pub fn begin(&mut self) -> u64 {
        self.seq += 1;
        self.loading = true;
        self.seq
    }

    /// The fetch `id` resolved. False (and no change) when a newer fetch has started.
    pub fn finish(&mut self, id: u64, result: Result<IpResult, String>) -> bool {
        if id != self.seq {
            return false;
        }
        let result = result.unwrap_or_else(|error| IpResult {
            ip: None,
            error: Some(error),
        });
        self.loading = false;
        // An empty `ip` is falsy in the TypeScript: it is kept for display, but counts
        // as a failure.
        self.error = match &result.ip {
            Some(ip) if !ip.is_empty() => None,
            _ => Some(result.error.unwrap_or_else(|| "unknown".to_owned())),
        };
        self.ip = result.ip;
        true
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The text after "IP:".
    pub fn display(&self) -> &str {
        if self.loading {
            "…"
        } else {
            self.ip.as_deref().unwrap_or("—")
        }
    }

    /// The badge's `title`. The error string is localized here, at render time.
    pub fn title(&self, lang: Lang, hidden: bool) -> String {
        if hidden {
            return lang.t("ip.showTitle").to_owned();
        }
        match self.error.as_deref().filter(|error| !error.is_empty()) {
            Some(error) => fill(lang.t("ip.errorTitle"), &[("error", error)]),
            None => lang.t("ip.title").to_owned(),
        }
    }
}

/// What a hidden badge draws in place of the value. GPUI cannot blur text as the CSS
/// `filter: blur(5px)` does, so every character becomes a dot; the width stays the same
/// in the badge's monospace font.
pub fn masked(value: &str) -> String {
    "•".repeat(value.chars().count())
}

pub struct IpBadge {
    state: IpState,
    host: Rc<dyn IpBadgeHost>,
    look: LookFn,
}

impl IpBadge {
    pub fn new(host: Rc<dyn IpBadgeHost>, look: LookFn, cx: &mut Context<Self>) -> Self {
        let mut badge = Self {
            state: IpState::default(),
            host,
            look,
        };
        badge.refresh(cx);
        badge
    }

    pub fn state(&self) -> &IpState {
        &self.state
    }

    /// Re-check the egress IP. A fetch still in flight is superseded, not cancelled.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let id = self.state.begin();
        cx.notify();
        let fetch = self.host.current_ip(cx);
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            let _ = this.update(cx, |this, cx| {
                if this.state.finish(id, result) {
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

impl Render for IpBadge {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let hidden = self.host.hidden(cx);
        let hover = theme.chrome_hover;
        let host = self.host.clone();
        let badge = cx.entity_id();
        let value = if hidden {
            masked(self.state.display())
        } else {
            self.state.display().to_owned()
        };

        // One pill: shows the egress IP (or masked), click toggles visibility.
        // `.chrome-chip`, in the monospace font at 11px.
        div()
            .id("ip-badge")
            .debug_selector(|| "ip-badge".into())
            .h(px(28.0))
            .px(px(10.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(5.0))
            .rounded(px(14.0))
            .font_family(MONO)
            .text_size(px(11.0))
            .text_color(theme.text_dim)
            .whitespace_nowrap()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .tooltip(tooltip(self.state.title(look.lang, hidden), &tokens))
            // Not a view listener: the host's toggle may reach back into this badge.
            .on_click(move |_, _, cx| {
                host.toggle_hidden(cx);
                cx.notify(badge);
            })
            // A manual re-check is a right-click (the `contextmenu` event).
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| this.refresh(cx)),
            )
            .child("IP:")
            .child(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(ip: &str) -> Result<IpResult, String> {
        Ok(IpResult {
            ip: Some(ip.to_owned()),
            error: None,
        })
    }

    #[test]
    fn 처음에는_불러오는_중이다() {
        let state = IpState::default();
        assert!(state.is_loading());
        assert_eq!(state.display(), "…");
        assert_eq!(state.error(), None);
    }

    #[test]
    fn 응답이_오면_ip를_보여준다() {
        let mut state = IpState::default();
        let id = state.begin();
        assert!(state.finish(id, ok("203.0.113.7")));
        assert!(!state.is_loading());
        assert_eq!(state.display(), "203.0.113.7");
        assert_eq!(state.error(), None);
    }

    #[test]
    fn 새_요청이_시작되면_늦게_온_앞선_응답은_버린다() {
        let mut state = IpState::default();
        let first = state.begin();
        let second = state.begin();
        assert!(!state.finish(first, ok("198.51.100.1")));
        // Still waiting for the second fetch.
        assert!(state.is_loading());
        assert_eq!(state.display(), "…");
        assert!(state.finish(second, ok("203.0.113.7")));
        assert_eq!(state.display(), "203.0.113.7");
    }

    #[test]
    fn 다시_확인하는_동안에는_말줄임표를_보여준다() {
        let mut state = IpState::default();
        let id = state.begin();
        state.finish(id, ok("203.0.113.7"));
        state.begin();
        assert!(state.is_loading());
        assert_eq!(state.display(), "…");
    }

    #[test]
    fn 오류_응답은_줄표와_오류_문구를_남긴다() {
        let mut state = IpState::default();
        let id = state.begin();
        assert!(state.finish(
            id,
            Ok(IpResult {
                ip: None,
                error: Some("timeout".to_owned())
            })
        ));
        assert_eq!(state.display(), "—");
        assert_eq!(state.error(), Some("timeout"));
    }

    #[test]
    fn ip도_오류도_없으면_unknown으로_친다() {
        let mut state = IpState::default();
        let id = state.begin();
        state.finish(id, Ok(IpResult::default()));
        assert_eq!(state.display(), "—");
        assert_eq!(state.error(), Some("unknown"));
    }

    #[test]
    fn 호출이_실패하면_그_메시지가_오류가_된다() {
        let mut state = IpState::default();
        let id = state.begin();
        state.finish(id, Err("ipc closed".to_owned()));
        assert_eq!(state.display(), "—");
        assert_eq!(state.error(), Some("ipc closed"));
    }

    #[test]
    fn 성공하면_앞선_오류를_지운다() {
        let mut state = IpState::default();
        let id = state.begin();
        state.finish(id, Err("ipc closed".to_owned()));
        let id = state.begin();
        state.finish(id, ok("203.0.113.7"));
        assert_eq!(state.error(), None);
    }

    #[test]
    fn 빈_ip는_그대로_보이지만_실패로_친다() {
        let mut state = IpState::default();
        let id = state.begin();
        state.finish(id, ok(""));
        assert_eq!(state.display(), "");
        assert_eq!(state.error(), Some("unknown"));
    }

    #[test]
    fn ip가_있으면_함께_온_오류는_무시한다() {
        let mut state = IpState::default();
        let id = state.begin();
        state.finish(
            id,
            Ok(IpResult {
                ip: Some("203.0.113.7".to_owned()),
                error: Some("stale".to_owned()),
            }),
        );
        assert_eq!(state.error(), None);
    }

    #[test]
    fn 제목은_숨김_오류_정상_순으로_고른다() {
        let mut state = IpState::default();
        let id = state.begin();
        state.finish(id, Err("timeout".to_owned()));
        assert_eq!(state.title(Lang::En, true), "Show the egress IP");
        assert_eq!(
            state.title(Lang::En, false),
            "Failed to fetch egress IP: timeout — click to retry"
        );
        assert_eq!(
            state.title(Lang::Ko, false),
            "이그레스 IP 조회 실패: timeout — 클릭해 재시도"
        );

        let id = state.begin();
        state.finish(id, ok("203.0.113.7"));
        assert_eq!(
            state.title(Lang::En, false),
            "Egress IP of the active tab (through its proxy, if set) — click to refresh"
        );
    }

    #[test]
    fn 빈_오류_문구는_오류_제목을_만들지_않는다() {
        let mut state = IpState::default();
        let id = state.begin();
        state.finish(
            id,
            Ok(IpResult {
                ip: None,
                error: Some(String::new()),
            }),
        );
        assert_eq!(state.error(), Some(""));
        assert_eq!(state.title(Lang::En, false), Lang::En.t("ip.title"));
    }

    #[test]
    fn 숨기면_글자_수만큼_점으로_가린다() {
        assert_eq!(masked("203.0.113.7"), "•••••••••••");
        assert_eq!(masked("…"), "•");
        assert_eq!(masked(""), "");
    }

    #[test]
    fn 응답은_필드가_빠져도_읽힌다() {
        let result: IpResult = serde_json::from_str(r#"{"ip":"203.0.113.7"}"#).unwrap();
        assert_eq!(result.ip.as_deref(), Some("203.0.113.7"));
        assert_eq!(result.error, None);
        let result: IpResult = serde_json::from_str(r#"{"error":"offline"}"#).unwrap();
        assert_eq!(result.error.as_deref(), Some("offline"));
        assert!(serde_json::from_str::<IpResult>(r#"{"ip":7}"#).is_err());
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::Cell;

    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::ui::testing::{center, look, stage};

    /// Answers every fetch at once with the next address, and counts the fetches.
    #[derive(Default)]
    struct Host {
        fetches: Cell<usize>,
        hidden: Cell<bool>,
    }

    impl IpBadgeHost for Host {
        fn current_ip(&self, _: &mut App) -> Task<Result<IpResult, String>> {
            self.fetches.set(self.fetches.get() + 1);
            Task::ready(Ok(IpResult {
                ip: Some(format!("203.0.113.{}", self.fetches.get())),
                error: None,
            }))
        }

        fn hidden(&self, _: &App) -> bool {
            self.hidden.get()
        }

        fn toggle_hidden(&self, _: &mut App) {
            self.hidden.set(!self.hidden.get());
        }
    }

    fn badge(cx: &mut TestAppContext) -> (Entity<IpBadge>, Rc<Host>, &mut VisualTestContext) {
        let host = Rc::new(Host::default());
        let (badge, _, cx) = stage(cx, {
            let host = host.clone();
            move |cx| IpBadge::new(host, look(), cx)
        });
        cx.run_until_parked();
        (badge, host, cx)
    }

    fn shown(badge: &Entity<IpBadge>, cx: &mut VisualTestContext) -> String {
        badge.read_with(cx, |badge, _| badge.state().display().to_owned())
    }

    #[gpui::test]
    fn 만들어지면_바로_ip를_가져와_보여준다(cx: &mut TestAppContext) {
        let (badge, host, cx) = badge(cx);
        assert_eq!(host.fetches.get(), 1);
        assert_eq!(shown(&badge, cx), "203.0.113.1");
        assert_eq!(cx.debug_bounds("ip-badge").unwrap().size.height, px(28.0));
    }

    #[gpui::test]
    fn 누르면_숨김만_바꾸고_다시_가져오지_않는다(cx: &mut TestAppContext) {
        let (badge, host, cx) = badge(cx);
        let at = center("ip-badge", cx);
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
        assert!(host.hidden.get());
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
        assert!(!host.hidden.get());
        assert_eq!(host.fetches.get(), 1);
        assert_eq!(shown(&badge, cx), "203.0.113.1");
    }

    #[gpui::test]
    fn 오른쪽_버튼은_숨김을_건드리지_않고_다시_가져온다(
        cx: &mut TestAppContext,
    ) {
        let (badge, host, cx) = badge(cx);
        let at = center("ip-badge", cx);
        cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(host.fetches.get(), 2);
        assert!(!host.hidden.get());
        assert_eq!(shown(&badge, cx), "203.0.113.2");
    }

    #[gpui::test]
    fn 숨긴_채로도_새로_가져온_값이_반영된다(cx: &mut TestAppContext) {
        let (badge, host, cx) = badge(cx);
        host.hidden.set(true);
        badge.update(cx, |badge, cx| badge.refresh(cx));
        cx.run_until_parked();
        assert_eq!(host.fetches.get(), 2);
        assert_eq!(shown(&badge, cx), "203.0.113.2");
    }
}
