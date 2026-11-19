//! Port of `src/renderer/src/hooks/use-find-in-page.ts` — the Cmd/Ctrl+F find bar. Acts on
//! the active tab's page and closes when the user switches tabs.
//!
//! The hook only holds the state; the bar it drives is drawn in App.tsx, and that markup
//! is ported here with it. The searching itself is the host's, behind [`FindHost`].

use std::rc::Rc;

use gpui::{
    App, Context, Empty, Entity, Focusable, KeyDownEvent, SharedString, Subscription, Window, div,
    prelude::*, px, rgba,
};
use rever_ui_kit::text_input::{SelectAll, Submitted, TextInput};

use crate::ui::{LookFn, Tokens, clickable, shadow, text, tooltip};

/// The second argument of Electron's `webview.findInPage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FindOptions {
    pub forward: bool,
    pub find_next: bool,
}

/// The live match count: which match is selected, out of how many.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FindResult {
    /// `activeMatchOrdinal` of the `found-in-page` event.
    pub active: u32,
    /// `matches` of the same event.
    pub total: u32,
}

/// What the host does for the bar: the tab handles of `tabRefs`. The bar calls
/// `find_in_page` and `stop_find_in_page` only for a tab that `has_page`.
pub trait FindHost {
    /// Whether the tab has a page view that can be searched (`tabRefs.current.get(id)`).
    fn has_page(&self, tab_id: &str, cx: &App) -> bool;
    /// Search the tab's page; `None` options start a new search. Matches come back
    /// through [`FindBar::found_in_page`].
    fn find_in_page(&self, tab_id: &str, query: &str, options: Option<FindOptions>, cx: &mut App);
    /// `stopFindInPage('clearSelection')`: end the search and drop the highlights.
    fn stop_find_in_page(&self, tab_id: &str, cx: &mut App);
}

/// What a state change asks of the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FindCommand {
    Find {
        tab_id: String,
        query: String,
        options: Option<FindOptions>,
    },
    Stop {
        tab_id: String,
    },
}

/// The hook's state. The query outlives closing the bar, as it does in the hook.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindState {
    pub open: bool,
    pub query: String,
    pub result: Option<FindResult>,
    /// The tab the bar was opened on (`findTabRef`).
    pub find_tab: Option<String>,
}

impl FindState {
    /// The `find` shortcut: remember the tab and show the bar.
    pub fn open_on(&mut self, active: Option<&str>) {
        self.find_tab = active.map(str::to_owned);
        self.open = true;
    }

    /// `doFind`. `active` is the active tab, and `None` when it has no page view: then
    /// nothing happens at all, not even clearing the count.
    pub fn do_find(
        &mut self,
        active: Option<&str>,
        query: &str,
        options: Option<FindOptions>,
    ) -> Option<FindCommand> {
        let tab_id = active?.to_owned();
        if query.is_empty() {
            self.result = None;
            return Some(FindCommand::Stop { tab_id });
        }
        Some(FindCommand::Find {
            tab_id,
            query: query.to_owned(),
            options,
        })
    }

    /// `closeFind`. Returns the tab whose highlights are to be cleared: the one the bar
    /// was opened on, or the active one when none was recorded.
    pub fn close(&mut self, active: Option<&str>) -> Option<String> {
        let tab_id = self.find_tab.take().or_else(|| active.map(str::to_owned));
        self.open = false;
        self.result = None;
        tab_id
    }

    /// A `found-in-page` event of tab `tab_id`. Only the active tab is listened to, and
    /// only while the bar is open.
    pub fn found(&mut self, active: Option<&str>, tab_id: &str, result: FindResult) {
        if self.open && active == Some(tab_id) {
            self.result = Some(result);
        }
    }

    /// Switching tabs closes the bar (and clears highlights on the old tab).
    pub fn closes_on_tab(&self, active: Option<&str>) -> bool {
        // An empty id is falsy in the hook: a bar opened on no tab stays open.
        self.open
            && self
                .find_tab
                .as_deref()
                .is_some_and(|tab| !tab.is_empty() && active != Some(tab))
    }

    /// The "3/12" next to the field; empty while there is no query.
    pub fn count_label(&self) -> String {
        if self.query.is_empty() {
            return String::new();
        }
        let (active, total) = self.result.map_or((0, 0), |r| (r.active, r.total));
        format!("{active}/{total}")
    }
}

pub struct FindBar {
    state: FindState,
    /// The tabs store's `activeId`, pushed by the host.
    active_id: Option<String>,
    host: Rc<dyn FindHost>,
    look: LookFn,
    input: Entity<TextInput>,
    /// Select the field's text once it is on screen.
    select_on_render: bool,
    _subscriptions: [Subscription; 2],
}

impl FindBar {
    /// The host must have called `rever_ui_kit::text_input::bind_keys`.
    pub fn new(host: Rc<dyn FindHost>, look: LookFn, cx: &mut Context<Self>) -> Self {
        // The placeholder is drawn by `render`, in the language of each render; the
        // field would keep the one it was made with.
        let input = cx.new(|cx| TextInput::new("", cx));
        // The field tells of every change, cursor moves included: `onChange` is the text
        // differing from the query.
        let changed = cx.observe(&input, |this, input, cx| {
            let typed = input.read(cx).text().to_owned();
            if typed != this.state.query {
                this.state.query.clone_from(&typed);
                this.do_find(&typed, None, cx);
                cx.notify();
            }
        });
        let submitted = cx.subscribe(&input, |this, _, _: &Submitted, cx| this.step(true, cx));
        Self {
            state: FindState::default(),
            active_id: None,
            host,
            look,
            input,
            select_on_render: false,
            _subscriptions: [changed, submitted],
        }
    }

    pub fn state(&self) -> &FindState {
        &self.state
    }

    /// The field's placeholder, in the language of this render.
    fn placeholder(&self, cx: &App) -> &'static str {
        (self.look)(cx).lang.t("toolbar.findInPage")
    }

    /// The `find` shortcut of App.tsx: show the bar for the active tab, focus the field
    /// and select what is in it. (Whether find is available at all, i.e. the embedded
    /// browser mode, is the host's check.)
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.open_on(self.active_id.as_deref());
        window.focus(&self.input.focus_handle(cx));
        self.select_on_render = true;
        cx.notify();
    }

    /// The hook's `setFindQuery`: put a query in the field without searching for it.
    pub fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        // The state first: the field's change then finds nothing new to search.
        self.state.query = query.to_owned();
        self.input
            .update(cx, |input, cx| input.set_text(query.to_owned(), cx));
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        let active = self.active_id.clone();
        if let Some(tab_id) = self.state.close(active.as_deref())
            && self.host.has_page(&tab_id, cx)
        {
            self.host.stop_find_in_page(&tab_id, cx);
        }
        cx.notify();
    }

    /// The host calls this whenever the active tab changes.
    pub fn set_active_tab(&mut self, active_id: Option<String>, cx: &mut Context<Self>) {
        self.active_id = active_id;
        if self.state.closes_on_tab(self.active_id.as_deref()) {
            self.close(cx);
        }
    }

    /// The host calls this for every `found-in-page` event of any tab.
    pub fn found_in_page(&mut self, tab_id: &str, result: FindResult, cx: &mut Context<Self>) {
        self.state.found(self.active_id.as_deref(), tab_id, result);
        cx.notify();
    }

    fn do_find(&mut self, query: &str, options: Option<FindOptions>, cx: &mut Context<Self>) {
        let active = self
            .active_id
            .clone()
            .filter(|tab_id| self.host.has_page(tab_id, cx));
        match self.state.do_find(active.as_deref(), query, options) {
            Some(FindCommand::Find {
                tab_id,
                query,
                options,
            }) => self.host.find_in_page(&tab_id, &query, options, cx),
            Some(FindCommand::Stop { tab_id }) => self.host.stop_find_in_page(&tab_id, cx),
            None => {}
        }
        cx.notify();
    }

    /// Go to the next or the previous match of the current query.
    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let query = self.state.query.clone();
        let options = FindOptions {
            forward,
            find_next: true,
        };
        self.do_find(&query, Some(options), cx);
    }
}

impl Render for FindBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.state.open {
            return Empty.into_any_element();
        }
        if std::mem::take(&mut self.select_on_render) {
            // Dispatched once this frame is drawn, when the field is there to take it.
            window.dispatch_action(Box::new(SelectAll), cx);
        }
        let tokens = Tokens::resolve((self.look)(cx).theme_mode, window);
        let theme = tokens.theme;

        // `.toolbar-btn` on a plain `button`.
        let button = |id: &'static str, label: &'static str, title: &'static str| {
            let hover = theme.border_2;
            clickable(id)
                .h(px(28.0))
                .min_w(px(32.0))
                .px(px(10.0))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .bg(theme.surface_3)
                .text_size(px(12.0))
                .line_height(px(12.0))
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .tooltip(tooltip(SharedString::new_static(title), &tokens))
                .child(label)
        };

        div()
            .id("find-bar")
            .debug_selector(|| "find-bar".into())
            .absolute()
            .top(px(8.0))
            .right(px(12.0))
            .flex()
            .items_center()
            .gap(px(4.0))
            .px(px(6.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .bg(theme.bg_bar)
            .border_1()
            .border_color(theme.border)
            // 0 2px 10px rgba(0,0,0,0.25)
            .shadow(vec![shadow(2.0, 10.0, rgba(0x00000040))])
            .text_color(theme.text)
            .font_family(".SystemUIFont")
            .occlude()
            // Plain Enter is the field's own key: it comes back as `Submitted`.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let keystroke = &event.keystroke;
                match keystroke.key.as_str() {
                    // Whatever else is held, `shiftKey` alone picks the direction.
                    "enter" => this.step(!keystroke.modifiers.shift, cx),
                    "escape" => this.close(cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(
                div()
                    .debug_selector(|| "find-input".into())
                    .relative()
                    .w(px(180.0))
                    .h(px(24.0))
                    .px(px(8.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .rounded(px(4.0))
                    .bg(tokens.surface)
                    .border_1()
                    .border_color(theme.border_2)
                    .text_size(px(13.0))
                    .overflow_hidden()
                    .child(self.input.clone())
                    .when(self.input.read(cx).text().is_empty(), |field| {
                        field.child(
                            // Where and how the field draws its own: on its 20px line,
                            // dimmed, clipped bare when too long.
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(px(8.0))
                                .right(px(8.0))
                                .flex()
                                .items_center()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .line_height(px(20.0))
                                .opacity(0.45)
                                .child(self.placeholder(cx)),
                        )
                    }),
            )
            .child(
                text(11.0)
                    .debug_selector(|| "find-count".into())
                    .min_w(px(42.0))
                    .flex()
                    .flex_none()
                    .justify_center()
                    .text_color(theme.text_dim)
                    .child(self.state.count_label()),
            )
            .child(
                button("find-prev", "↑", "Previous match (Shift+Enter)")
                    .on_click(cx.listener(|this, _, _, cx| this.step(false, cx))),
            )
            .child(
                button("find-next", "↓", "Next match (Enter)")
                    .on_click(cx.listener(|this, _, _, cx| this.step(true, cx))),
            )
            .child(
                button("find-close", "✕", "Close (Esc)")
                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEXT: Option<FindOptions> = Some(FindOptions {
        forward: true,
        find_next: true,
    });

    fn open_on(tab: &str) -> FindState {
        let mut state = FindState::default();
        state.open_on(Some(tab));
        state
    }

    #[test]
    fn 처음에는_닫혀_있고_검색어도_결과도_없다() {
        let state = FindState::default();
        assert!(!state.open);
        assert_eq!(state.query, "");
        assert_eq!(state.result, None);
        assert_eq!(state.find_tab, None);
    }

    #[test]
    fn 열면_그때의_활성_탭을_기억한다() {
        let state = open_on("t1");
        assert!(state.open);
        assert_eq!(state.find_tab.as_deref(), Some("t1"));
    }

    #[test]
    fn 검색어가_있으면_활성_탭에서_찾는다() {
        let mut state = open_on("t1");
        assert_eq!(
            state.do_find(Some("t1"), "rust", None),
            Some(FindCommand::Find {
                tab_id: "t1".into(),
                query: "rust".into(),
                options: None,
            })
        );
        assert_eq!(
            state.do_find(Some("t1"), "rust", NEXT),
            Some(FindCommand::Find {
                tab_id: "t1".into(),
                query: "rust".into(),
                options: NEXT,
            })
        );
    }

    #[test]
    fn 빈_검색어는_찾기를_멈추고_결과를_지운다() {
        let mut state = open_on("t1");
        state.result = Some(FindResult {
            active: 1,
            total: 3,
        });
        assert_eq!(
            state.do_find(Some("t1"), "", NEXT),
            Some(FindCommand::Stop {
                tab_id: "t1".into()
            })
        );
        assert_eq!(state.result, None);
    }

    #[test]
    fn 페이지가_없는_탭에서는_결과도_건드리지_않는다() {
        let mut state = open_on("t1");
        let result = Some(FindResult {
            active: 1,
            total: 3,
        });
        state.result = result;
        assert_eq!(state.do_find(None, "", None), None);
        assert_eq!(state.do_find(None, "rust", None), None);
        assert_eq!(state.result, result);
    }

    #[test]
    fn 닫으면_열었던_탭의_강조를_지우고_검색어는_남긴다() {
        let mut state = open_on("t1");
        state.query = "rust".into();
        state.result = Some(FindResult {
            active: 2,
            total: 5,
        });
        // The active tab has moved on; the highlights are on the tab the bar opened on.
        assert_eq!(state.close(Some("t2")).as_deref(), Some("t1"));
        assert!(!state.open);
        assert_eq!(state.result, None);
        assert_eq!(state.find_tab, None);
        assert_eq!(state.query, "rust");
    }

    #[test]
    fn 기억한_탭이_없으면_활성_탭의_강조를_지운다() {
        let mut state = FindState {
            open: true,
            ..Default::default()
        };
        assert_eq!(state.close(Some("t2")).as_deref(), Some("t2"));
        assert_eq!(state.close(None), None);
    }

    #[test]
    fn 활성_탭의_결과만_열려_있는_동안_받는다() {
        let result = FindResult {
            active: 2,
            total: 7,
        };
        let mut state = open_on("t1");
        state.found(Some("t1"), "t2", result);
        assert_eq!(state.result, None);
        state.found(None, "t1", result);
        assert_eq!(state.result, None);
        state.found(Some("t1"), "t1", result);
        assert_eq!(state.result, Some(result));

        let mut closed = FindState::default();
        closed.found(Some("t1"), "t1", result);
        assert_eq!(closed.result, None);
    }

    #[test]
    fn 다른_탭으로_바꾸면_닫아야_한다() {
        let state = open_on("t1");
        assert!(!state.closes_on_tab(Some("t1")));
        assert!(state.closes_on_tab(Some("t2")));
        assert!(state.closes_on_tab(None));
    }

    #[test]
    fn 닫혀_있거나_기억한_탭이_없으면_탭을_바꿔도_그대로다() {
        assert!(!FindState::default().closes_on_tab(Some("t2")));
        let mut no_tab = FindState::default();
        no_tab.open_on(None);
        assert!(!no_tab.closes_on_tab(Some("t2")));
        let mut empty_tab = FindState::default();
        empty_tab.open_on(Some(""));
        assert!(!empty_tab.closes_on_tab(Some("t2")));
    }

    #[test]
    fn 개수_표시는_검색어가_있을_때만_나온다() {
        let mut state = open_on("t1");
        assert_eq!(state.count_label(), "");
        state.result = Some(FindResult {
            active: 3,
            total: 12,
        });
        // A count without a query (left over) is not shown either.
        assert_eq!(state.count_label(), "");
        state.query = "rust".into();
        assert_eq!(state.count_label(), "3/12");
        state.result = None;
        assert_eq!(state.count_label(), "0/0");
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::RefCell;

    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::ui::testing::{center, look};

    /// Records what the bar asks for; only `t1` and `t2` have a page.
    #[derive(Default)]
    struct FakeHost {
        calls: RefCell<Vec<FindCommand>>,
    }

    impl FindHost for FakeHost {
        fn has_page(&self, tab_id: &str, _: &App) -> bool {
            matches!(tab_id, "t1" | "t2")
        }

        fn find_in_page(
            &self,
            tab_id: &str,
            query: &str,
            options: Option<FindOptions>,
            _: &mut App,
        ) {
            self.calls.borrow_mut().push(FindCommand::Find {
                tab_id: tab_id.into(),
                query: query.into(),
                options,
            });
        }

        fn stop_find_in_page(&self, tab_id: &str, _: &mut App) {
            self.calls.borrow_mut().push(FindCommand::Stop {
                tab_id: tab_id.into(),
            });
        }
    }

    fn find(tab_id: &str, query: &str, options: Option<(bool, bool)>) -> FindCommand {
        FindCommand::Find {
            tab_id: tab_id.into(),
            query: query.into(),
            options: options.map(|(forward, find_next)| FindOptions { forward, find_next }),
        }
    }

    fn stop(tab_id: &str) -> FindCommand {
        FindCommand::Stop {
            tab_id: tab_id.into(),
        }
    }

    /// The page box the bar is positioned in.
    struct Stage(Entity<FindBar>);

    impl Render for Stage {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().relative().child(self.0.clone())
        }
    }

    fn closed_bar(
        cx: &mut TestAppContext,
    ) -> (Entity<FindBar>, Rc<FakeHost>, &mut VisualTestContext) {
        cx.update(rever_ui_kit::text_input::bind_keys);
        let host = Rc::new(FakeHost::default());
        let (stage, cx) = cx.add_window_view({
            let host = host.clone();
            move |_, cx| Stage(cx.new(|cx| FindBar::new(host, look(), cx)))
        });
        let bar = stage.read_with(cx, |stage, _| stage.0.clone());
        (bar, host, cx)
    }

    /// A bar on tab `t1`, already open.
    fn bar(cx: &mut TestAppContext) -> (Entity<FindBar>, Rc<FakeHost>, &mut VisualTestContext) {
        let (bar, host, cx) = closed_bar(cx);
        bar.update_in(cx, |bar, window, cx| {
            bar.set_active_tab(Some("t1".into()), cx);
            bar.open(window, cx);
        });
        cx.run_until_parked();
        (bar, host, cx)
    }

    fn calls(host: &FakeHost) -> Vec<FindCommand> {
        host.calls.take()
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let at = center(selector, cx);
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
    }

    fn is_open(bar: &Entity<FindBar>, cx: &mut VisualTestContext) -> bool {
        bar.read_with(cx, |bar, _| bar.state().open)
    }

    #[gpui::test]
    fn 닫혀_있으면_아무것도_그리지_않는다(cx: &mut TestAppContext) {
        let (bar, _, cx) = closed_bar(cx);
        assert!(!is_open(&bar, cx));
        assert!(cx.debug_bounds("find-bar").is_none());
    }

    #[gpui::test]
    fn 열면_오른쪽_위에_놓이고_입력창이_포커스를_받는다(
        cx: &mut TestAppContext,
    ) {
        let (bar, host, cx) = bar(cx);
        let viewport = cx.update(|window, _| window.viewport_size());
        let bounds = cx.debug_bounds("find-bar").unwrap();
        assert_eq!(bounds.top(), px(8.0));
        assert_eq!(bounds.right(), viewport.width - px(12.0));
        let input = cx.debug_bounds("find-input").unwrap();
        assert_eq!(input.size, gpui::size(px(180.0), px(24.0)));
        assert_eq!(cx.debug_bounds("find-next").unwrap().size.height, px(28.0));

        // Typing lands in the field without a click.
        cx.simulate_input("ab");
        cx.run_until_parked();
        assert_eq!(bar.read_with(cx, |bar, _| bar.state().query.clone()), "ab");
        // One search per change of the text, without options.
        assert_eq!(
            calls(&host),
            [find("t1", "a", None), find("t1", "ab", None)]
        );
    }

    #[gpui::test]
    fn 엔터는_다음_시프트_엔터는_이전_일치로_간다(cx: &mut TestAppContext) {
        let (_, host, cx) = bar(cx);
        cx.simulate_input("rust");
        calls(&host);
        cx.simulate_keystrokes("enter");
        assert_eq!(calls(&host), [find("t1", "rust", Some((true, true)))]);
        cx.simulate_keystrokes("shift-enter");
        assert_eq!(calls(&host), [find("t1", "rust", Some((false, true)))]);
    }

    #[gpui::test]
    fn 다른_보조키와_함께_누른_엔터도_시프트만_보고_방향을_정한다(
        cx: &mut TestAppContext,
    ) {
        // `e.key === 'Enter'` whatever else is held; only `shiftKey` picks the direction.
        let (_, host, cx) = bar(cx);
        cx.simulate_input("rust");
        calls(&host);
        cx.simulate_keystrokes("cmd-enter");
        assert_eq!(calls(&host), [find("t1", "rust", Some((true, true)))]);
        cx.simulate_keystrokes("alt-shift-enter");
        assert_eq!(calls(&host), [find("t1", "rust", Some((false, true)))]);
    }

    #[gpui::test]
    fn 자리_표시_글은_그릴_때의_언어를_따른다(cx: &mut TestAppContext) {
        cx.update(rever_ui_kit::text_input::bind_keys);
        let lang = Rc::new(std::cell::Cell::new(rever_ui_kit::i18n::Lang::En));
        let look: LookFn = Rc::new({
            let lang = lang.clone();
            move |_| crate::ui::Look {
                theme_mode: rever_ui_kit::theme::ThemeMode::Light,
                lang: lang.get(),
            }
        });
        let host = Rc::new(FakeHost::default());
        let (stage, cx) =
            cx.add_window_view(move |_, cx| Stage(cx.new(|cx| FindBar::new(host, look, cx))));
        let bar = stage.read_with(cx, |stage, _| stage.0.clone());
        let placeholder =
            |cx: &mut VisualTestContext| bar.read_with(cx, |bar, cx| bar.placeholder(cx));
        assert_eq!(placeholder(cx), "Find in page");
        // The bar lives as long as the window: a language switch must reach it.
        lang.set(rever_ui_kit::i18n::Lang::Ko);
        assert_eq!(placeholder(cx), "페이지에서 찾기");
    }

    #[gpui::test]
    fn 화살표_버튼도_이전과_다음으로_간다(cx: &mut TestAppContext) {
        let (_, host, cx) = bar(cx);
        cx.simulate_input("rust");
        calls(&host);
        click("find-prev", cx);
        click("find-next", cx);
        assert_eq!(
            calls(&host),
            [
                find("t1", "rust", Some((false, true))),
                find("t1", "rust", Some((true, true)))
            ]
        );
    }

    #[gpui::test]
    fn 검색어를_다_지우면_찾기를_멈춘다(cx: &mut TestAppContext) {
        let (bar, host, cx) = bar(cx);
        cx.simulate_input("a");
        bar.update(cx, |bar, cx| {
            bar.found_in_page(
                "t1",
                FindResult {
                    active: 1,
                    total: 4,
                },
                cx,
            )
        });
        calls(&host);
        cx.simulate_keystrokes("backspace");
        assert_eq!(calls(&host), [stop("t1")]);
        assert_eq!(bar.read_with(cx, |bar, _| bar.state().result), None);
        // Enter on an empty field stops again instead of searching for nothing.
        cx.simulate_keystrokes("enter");
        assert_eq!(calls(&host), [stop("t1")]);
    }

    #[gpui::test]
    fn 결과가_오면_개수를_보여_준다(cx: &mut TestAppContext) {
        let (bar, _, cx) = bar(cx);
        cx.simulate_input("rust");
        let count = |bar: &Entity<FindBar>, cx: &mut VisualTestContext| {
            bar.read_with(cx, |bar, _| bar.state().count_label())
        };
        assert_eq!(count(&bar, cx), "0/0");
        bar.update(cx, |bar, cx| {
            bar.found_in_page(
                "t2",
                FindResult {
                    active: 9,
                    total: 9,
                },
                cx,
            );
            bar.found_in_page(
                "t1",
                FindResult {
                    active: 2,
                    total: 5,
                },
                cx,
            );
        });
        assert_eq!(count(&bar, cx), "2/5");
        assert_eq!(cx.debug_bounds("find-count").unwrap().size.width, px(42.0));
    }

    #[gpui::test]
    fn esc와_닫기_버튼은_강조를_지우고_닫는다(cx: &mut TestAppContext) {
        let (bar, host, cx) = bar(cx);
        cx.simulate_input("rust");
        calls(&host);
        cx.simulate_keystrokes("escape");
        assert!(!is_open(&bar, cx));
        assert_eq!(calls(&host), [stop("t1")]);

        bar.update_in(cx, |bar, window, cx| bar.open(window, cx));
        cx.run_until_parked();
        click("find-close", cx);
        assert!(!is_open(&bar, cx));
        assert_eq!(calls(&host), [stop("t1")]);
    }

    #[gpui::test]
    fn 다시_열면_검색어가_남아_있고_전체가_선택돼_있다(
        cx: &mut TestAppContext,
    ) {
        let (bar, host, cx) = bar(cx);
        cx.simulate_input("rust");
        cx.simulate_keystrokes("escape");
        calls(&host);

        bar.update_in(cx, |bar, window, cx| bar.open(window, cx));
        cx.run_until_parked();
        let query = |bar: &Entity<FindBar>, cx: &mut VisualTestContext| {
            bar.read_with(cx, |bar, _| bar.state().query.clone())
        };
        assert_eq!(query(&bar, cx), "rust");
        // Opening alone searches nothing, and there is no count yet.
        assert_eq!(calls(&host), []);
        assert_eq!(bar.read_with(cx, |bar, _| bar.state().count_label()), "0/0");
        // The old query is selected: typing replaces it.
        cx.simulate_input("g");
        cx.run_until_parked();
        assert_eq!(query(&bar, cx), "g");
        assert_eq!(calls(&host), [find("t1", "g", None)]);
    }

    #[gpui::test]
    fn set_query는_입력창을_채우기만_하고_찾지는_않는다(cx: &mut TestAppContext) {
        let (bar, host, cx) = bar(cx);
        bar.update(cx, |bar, cx| bar.set_query("rust", cx));
        cx.run_until_parked();
        assert_eq!(calls(&host), []);
        assert_eq!(bar.read_with(cx, |bar, _| bar.state().count_label()), "0/0");
        // The field holds it: Enter searches for it, typing goes on from its end.
        cx.simulate_keystrokes("enter");
        assert_eq!(calls(&host), [find("t1", "rust", Some((true, true)))]);
        cx.simulate_input("y");
        assert_eq!(calls(&host), [find("t1", "rusty", None)]);
    }

    #[gpui::test]
    fn 탭을_바꾸면_닫히고_열었던_탭의_강조를_지운다(cx: &mut TestAppContext) {
        let (bar, host, cx) = bar(cx);
        cx.simulate_input("rust");
        calls(&host);
        bar.update(cx, |bar, cx| bar.set_active_tab(Some("t1".into()), cx));
        assert!(is_open(&bar, cx));
        bar.update(cx, |bar, cx| bar.set_active_tab(Some("t2".into()), cx));
        assert!(!is_open(&bar, cx));
        assert_eq!(calls(&host), [stop("t1")]);
    }

    #[gpui::test]
    fn 페이지가_없는_탭에서는_호스트를_부르지_않는다(cx: &mut TestAppContext) {
        let (bar, host, cx) = bar(cx);
        cx.simulate_keystrokes("escape");
        calls(&host);
        // `t9` has no page view.
        bar.update_in(cx, |bar, window, cx| {
            bar.set_active_tab(Some("t9".into()), cx);
            bar.open(window, cx);
        });
        cx.run_until_parked();
        cx.simulate_input("x");
        cx.simulate_keystrokes("enter");
        cx.simulate_keystrokes("escape");
        assert!(!is_open(&bar, cx));
        assert_eq!(calls(&host), []);
    }
}
