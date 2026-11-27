//! Port of `src/renderer/src/components/chat/ChatTabs.tsx` — Orca-style session tabs:
//! each tab is its own chat panel with its own agent process. Inactive tabs stay alive
//! (not drawn) so a running turn keeps going; closing a tab drops its panel, which tears
//! down that agent session.
//!
//! The chat panel itself is not part of this crate: the host hands [`ChatTabs`] a
//! function that makes one for a [`PanelSpec`], and the panel reports its title, agent
//! and busy state through [`ChatTabs::set_meta`] (the component's `onMeta`). The
//! panel's `active` prop is [`ChatTabs::active_id`], with a [`ChatTabsEvent`] when it
//! changes.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyView, App, Context, Entity, EventEmitter, FocusHandle, FontWeight,
    KeyDownEvent, MouseButton, Window, div, prelude::*, px,
};
use rever_ui_menus::ui::LookFn;
use rever_ui_state::constants::AcpAgentId;

use crate::model_picker::{ModelPicker, ModelPickerEvent, ModelPickerHost, ModelPickerProps};
use crate::provider_icon::provider_icon;
use crate::ui::{Span, Tokens, cubic_bezier, ellipsis, natural_width, single_line, tooltip};

/// What a chat panel reports about itself for its tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatTabMeta {
    pub title: String,
    pub agent_id: AcpAgentId,
    pub busy: bool,
}

/// The tabs, the one in front, and what each was opened with. The component's state
/// hooks and the module's `tabSeq` counter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabsState {
    seq: u64,
    first_id: String,
    tabs: Vec<String>,
    // The agent each later tab was opened with (the first tab restores its own).
    launch: HashMap<String, AcpAgentId>,
    active_id: String,
    meta: HashMap<String, ChatTabMeta>,
}

impl Default for TabsState {
    fn default() -> Self {
        Self::new()
    }
}

impl TabsState {
    /// One tab, in front.
    pub fn new() -> Self {
        let mut state = Self {
            seq: 0,
            first_id: String::new(),
            tabs: Vec::new(),
            launch: HashMap::new(),
            active_id: String::new(),
            meta: HashMap::new(),
        };
        let first = state.new_tab_id();
        state.first_id = first.clone();
        state.active_id = first.clone();
        state.tabs.push(first);
        state
    }

    fn new_tab_id(&mut self) -> String {
        self.seq += 1;
        format!("chat-{}", self.seq)
    }

    pub fn tabs(&self) -> &[String] {
        &self.tabs
    }

    pub fn active_id(&self) -> &str {
        &self.active_id
    }

    /// The tab the app started with: the only one that reopens the last conversation.
    pub fn first_id(&self) -> &str {
        &self.first_id
    }

    pub fn meta(&self, id: &str) -> Option<&ChatTabMeta> {
        self.meta.get(id)
    }

    /// The agent a tab was opened with from the "+" list.
    pub fn launch_agent(&self, id: &str) -> Option<AcpAgentId> {
        self.launch.get(id).copied()
    }

    pub fn activate(&mut self, id: &str) {
        self.active_id = id.to_owned();
    }

    /// A panel's report. Returns whether anything changed.
    pub fn on_meta(&mut self, id: &str, meta: ChatTabMeta) -> bool {
        if self.meta.get(id) == Some(&meta) {
            return false;
        }
        self.meta.insert(id.to_owned(), meta);
        true
    }

    /// Open a tab on `agent_id` and bring it to the front. Returns its id.
    pub fn add(&mut self, agent_id: AcpAgentId) -> String {
        let id = self.new_tab_id();
        self.launch.insert(id.clone(), agent_id);
        self.tabs.push(id.clone());
        self.active_id = id.clone();
        id
    }

    /// Close a tab. Closing the one in front brings its left neighbor forward.
    pub fn close(&mut self, id: &str) {
        let at = self.tabs.iter().position(|tab| tab == id);
        let rest: Vec<String> = self.tabs.iter().filter(|tab| *tab != id).cloned().collect();
        if rest.is_empty() {
            // Always keep one session open.
            let fresh = self.new_tab_id();
            self.tabs = vec![fresh.clone()];
            self.active_id = fresh;
        } else {
            if id == self.active_id
                && let Some(next) = rest.get(at.map_or(0, |at| at.saturating_sub(1)))
            {
                self.active_id = next.clone();
            }
            self.tabs = rest;
        }
        self.meta.remove(id);
        self.launch.remove(id);
    }
}

/// What a tab's chat panel is made from (the component's `ChatPanel` props).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelSpec {
    pub tab_id: String,
    /// Reopen the most recent saved conversation (only the first tab does).
    pub restore: bool,
    /// The agent picked in the "+" list; the first tab and a replacement tab have none.
    pub initial_agent: Option<AcpAgentId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatTabsEvent {
    /// Another tab came to the front.
    Activated(String),
    /// A tab was closed; its panel is gone.
    Closed(String),
}

type MakePanel = Box<dyn Fn(PanelSpec, &mut Window, &mut Context<ChatTabs>) -> AnyView>;

struct Tab {
    panel: AnyView,
    /// `tabIndex={0}`: a tab can hold the keyboard focus.
    focus: FocusHandle,
}

pub struct ChatTabs {
    state: TabsState,
    open: HashMap<String, Tab>,
    make_panel: MakePanel,
    picker: Entity<ModelPicker>,
    /// The "+" list's `agentId` prop: the agent the tab in front was opened with.
    active_agent: Rc<Cell<AcpAgentId>>,
    look: LookFn,
}

impl EventEmitter<ChatTabsEvent> for ChatTabs {}

impl ChatTabs {
    /// `make_panel` builds the chat panel of one tab; it is called once per tab.
    /// `column` gives the chat column's edges, which the "+" list has to stay inside.
    pub fn new(
        make_panel: impl Fn(PanelSpec, &mut Window, &mut Context<Self>) -> AnyView + 'static,
        host: Rc<dyn ModelPickerHost>,
        column: impl Fn(&App) -> Option<Span> + 'static,
        look: LookFn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let active_agent = Rc::new(Cell::new(AcpAgentId::ClaudeCode));
        let agent = active_agent.clone();
        let picker = cx.new(|cx| {
            ModelPicker::new(
                move |cx| ModelPickerProps {
                    agent_id: agent.get(),
                    model_name: None,
                    chip_name: None,
                    disabled: false,
                    new_tab: true,
                    column: column(cx),
                },
                host,
                look.clone(),
                cx,
            )
        });
        cx.subscribe_in(&picker, window, |this, _, event, window, cx| {
            if let ModelPickerEvent::PickedAgent(agent_id) = event {
                this.add(*agent_id, window, cx);
            }
        })
        .detach();
        let mut this = Self {
            state: TabsState::new(),
            open: HashMap::new(),
            make_panel: Box::new(make_panel),
            picker,
            active_agent,
            look,
        };
        let first = this.state.first_id().to_owned();
        this.mount(&first, window, cx);
        this
    }

    pub fn state(&self) -> &TabsState {
        &self.state
    }

    pub fn active_id(&self) -> &str {
        self.state.active_id()
    }

    /// The "+" button and its list of agents.
    pub fn picker(&self) -> &Entity<ModelPicker> {
        &self.picker
    }

    fn mount(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let spec = PanelSpec {
            tab_id: id.to_owned(),
            restore: id == self.state.first_id(),
            initial_agent: self.state.launch_agent(id),
        };
        let tab = Tab {
            panel: (self.make_panel)(spec, window, cx),
            focus: cx.focus_handle().tab_stop(true),
        };
        self.open.insert(id.to_owned(), tab);
    }

    /// `onMeta`: a panel reports its title, agent and busy state.
    pub fn set_meta(&mut self, id: &str, meta: ChatTabMeta, cx: &mut Context<Self>) {
        if self.state.on_meta(id, meta) {
            cx.notify();
        }
    }

    pub fn activate(&mut self, id: &str, cx: &mut Context<Self>) {
        let before = self.state.active_id().to_owned();
        self.state.activate(id);
        self.changed(&before, cx);
    }

    /// Open a tab on `agent_id` and bring it to the front.
    pub fn add(&mut self, agent_id: AcpAgentId, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.state.active_id().to_owned();
        let id = self.state.add(agent_id);
        self.mount(&id, window, cx);
        self.changed(&before, cx);
    }

    pub fn close(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.state.active_id().to_owned();
        self.state.close(id);
        if self.open.remove(id).is_some() {
            cx.emit(ChatTabsEvent::Closed(id.to_owned()));
        }
        // The replacement of the last tab.
        let active = self.state.active_id().to_owned();
        if !self.open.contains_key(&active) {
            self.mount(&active, window, cx);
        }
        self.changed(&before, cx);
    }

    fn changed(&mut self, before: &str, cx: &mut Context<Self>) {
        self.active_agent.set(
            self.state
                .launch_agent(self.state.active_id())
                .unwrap_or(AcpAgentId::ClaudeCode),
        );
        if self.state.active_id() != before {
            cx.emit(ChatTabsEvent::Activated(self.state.active_id().to_owned()));
        }
        cx.notify();
    }
}

/// `chat-tab-pulse`: opacity 1 → 0.3 → 1 over a second, each half `ease-in-out`.
pub fn pulse_opacity(progress: f32) -> f32 {
    let half = if progress < 0.5 {
        progress * 2.0
    } else {
        (1.0 - progress) * 2.0
    };
    1.0 - 0.7 * cubic_bezier(half, 0.42, 0.0, 0.58, 1.0)
}

impl Render for ChatTabs {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let lang = look.lang;
        let hover = theme.chrome_hover;

        let tabs = self.state.tabs().iter().enumerate().map(|(index, id)| {
            let meta = self.state.meta(id);
            let selected = id == self.state.active_id();
            let color = if selected { theme.text } else { theme.text_2 };
            let title = meta.map_or_else(
                || lang.t("chat.newTab").to_owned(),
                |meta| single_line(&meta.title),
            );
            let title_width = natural_width(&title, 12.0, FontWeight::NORMAL, window);
            let (click_id, key_id, aux_id, close_id) =
                (id.clone(), id.clone(), id.clone(), id.clone());
            // `.chat-tab`
            div()
                .id(("chat-tab", index))
                .debug_selector(move || format!("chat-tab-{index}"))
                .when_some(self.open.get(id), |tab, open| tab.track_focus(&open.focus))
                .h(px(30.0))
                .min_w_0()
                .max_w(px(180.0))
                .flex()
                .flex_shrink()
                .items_center()
                .gap(px(6.0))
                .pl(px(10.0))
                .pr(px(6.0))
                .rounded_t(px(8.0))
                .text_size(px(12.0))
                .line_height(px(14.0))
                .text_color(color)
                .when(selected, |tab| tab.bg(theme.surface_3))
                .when(!selected, |tab| tab.hover(move |style| style.bg(hover)))
                .when_some(meta, |tab, meta| {
                    tab.tooltip(tooltip(meta.title.clone(), &tokens))
                })
                .on_click(cx.listener(move |this, _, _, cx| this.activate(&click_id, cx)))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.activate(&key_id, cx);
                    }
                }))
                // `onAuxClick` with the middle button.
                .on_mouse_up(
                    MouseButton::Middle,
                    cx.listener(move |this, _, window, cx| this.close(&aux_id, window, cx)),
                )
                .children(meta.map(|meta| provider_icon(meta.agent_id.as_str(), 12.0, None, color)))
                .child(
                    // `.chat-tab-title`
                    ellipsis(title, 14.0)
                        .w(px(title_width))
                        .min_w_0()
                        .flex_shrink(),
                )
                .when(meta.is_some_and(|meta| meta.busy), |tab| {
                    tab.child(
                        // `.chat-tab-busy`
                        div()
                            .size(px(6.0))
                            .flex_none()
                            .rounded_full()
                            .bg(theme.accent)
                            .with_animation(
                                ("chat-tab-busy", index),
                                Animation::new(Duration::from_secs(1)).repeat(),
                                |dot, progress| dot.opacity(pulse_opacity(progress)),
                            ),
                    )
                })
                .child(
                    // `.chrome-tab-close`
                    div()
                        .id(("chat-tab-close", index))
                        .debug_selector(move || format!("chat-tab-close-{index}"))
                        .size(px(18.0))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .text_size(px(14.0))
                        .line_height(px(14.0))
                        .text_color(theme.text_dim)
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover).text_color(theme.text))
                        .tooltip(tooltip(lang.t("chat.closeTab"), &tokens))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            // Not also a click on the tab.
                            cx.stop_propagation();
                            this.close(&close_id, window, cx);
                        }))
                        .child("×"),
                )
        });

        // `.chat-tabs`
        let strip = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .pt(px(6.0))
            .px(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .overflow_hidden()
            .children(tabs)
            .child(self.picker.clone());

        let panel = self.open.get(self.state.active_id()).map(|tab| {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(tab.panel.clone())
        });

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(strip)
            .children(panel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(title: &str, busy: bool) -> ChatTabMeta {
        ChatTabMeta {
            title: title.to_owned(),
            agent_id: AcpAgentId::ClaudeCode,
            busy,
        }
    }

    #[test]
    fn 처음에는_탭_하나가_앞에_있다() {
        let state = TabsState::new();
        assert_eq!(state.tabs(), ["chat-1"]);
        assert_eq!(state.active_id(), "chat-1");
        assert_eq!(state.first_id(), "chat-1");
        assert_eq!(state.meta("chat-1"), None);
        assert_eq!(state.launch_agent("chat-1"), None);
    }

    #[test]
    fn 탭을_더하면_끝에_붙고_앞으로_나오며_고른_에이전트를_기억한다() {
        let mut state = TabsState::new();
        assert_eq!(state.add(AcpAgentId::Codex), "chat-2");
        assert_eq!(state.add(AcpAgentId::Grok), "chat-3");
        assert_eq!(state.tabs(), ["chat-1", "chat-2", "chat-3"]);
        assert_eq!(state.active_id(), "chat-3");
        assert_eq!(state.launch_agent("chat-2"), Some(AcpAgentId::Codex));
        assert_eq!(state.launch_agent("chat-3"), Some(AcpAgentId::Grok));
    }

    #[test]
    fn 앞에_있는_탭을_닫으면_왼쪽_탭이_앞으로_나온다() {
        let mut state = TabsState::new();
        state.add(AcpAgentId::Codex);
        state.add(AcpAgentId::Grok);
        state.close("chat-3");
        assert_eq!(state.tabs(), ["chat-1", "chat-2"]);
        assert_eq!(state.active_id(), "chat-2");
    }

    #[test]
    fn 맨_왼쪽_탭을_닫으면_그_자리에_온_탭이_앞으로_나온다() {
        let mut state = TabsState::new();
        state.add(AcpAgentId::Codex);
        state.add(AcpAgentId::Grok);
        state.activate("chat-1");
        state.close("chat-1");
        assert_eq!(state.tabs(), ["chat-2", "chat-3"]);
        assert_eq!(state.active_id(), "chat-2");
    }

    #[test]
    fn 가운데_탭이_앞에_있을_때_닫으면_왼쪽_탭이_앞으로_나온다() {
        let mut state = TabsState::new();
        state.add(AcpAgentId::Codex);
        state.add(AcpAgentId::Grok);
        state.activate("chat-2");
        state.close("chat-2");
        assert_eq!(state.tabs(), ["chat-1", "chat-3"]);
        assert_eq!(state.active_id(), "chat-1");
    }

    #[test]
    fn 뒤에_있는_탭을_닫아도_앞의_탭은_그대로다() {
        let mut state = TabsState::new();
        state.add(AcpAgentId::Codex);
        state.add(AcpAgentId::Grok);
        state.close("chat-2");
        assert_eq!(state.tabs(), ["chat-1", "chat-3"]);
        assert_eq!(state.active_id(), "chat-3");
    }

    #[test]
    fn 마지막_탭을_닫으면_새_탭_하나가_대신_열린다() {
        let mut state = TabsState::new();
        state.on_meta("chat-1", meta("hello", false));
        state.close("chat-1");
        // The counter keeps going: ids are never reused.
        assert_eq!(state.tabs(), ["chat-2"]);
        assert_eq!(state.active_id(), "chat-2");
        // The replacement is not the first tab: it does not restore a conversation.
        assert_eq!(state.first_id(), "chat-1");
        assert_eq!(state.launch_agent("chat-2"), None);
        assert_eq!(state.meta("chat-1"), None);
    }

    #[test]
    fn 닫힌_탭의_제목과_에이전트는_잊는다() {
        let mut state = TabsState::new();
        state.add(AcpAgentId::Codex);
        state.on_meta("chat-2", meta("second", true));
        state.close("chat-2");
        assert_eq!(state.meta("chat-2"), None);
        assert_eq!(state.launch_agent("chat-2"), None);
    }

    #[test]
    fn 없는_탭을_닫아도_아무_일도_없다() {
        let mut state = TabsState::new();
        state.add(AcpAgentId::Codex);
        let before = state.clone();
        state.close("chat-9");
        assert_eq!(state, before);
    }

    #[test]
    fn 패널의_보고는_달라졌을_때만_바뀐_것으로_친다() {
        let mut state = TabsState::new();
        assert!(state.on_meta("chat-1", meta("hello", false)));
        assert!(!state.on_meta("chat-1", meta("hello", false)));
        assert!(state.on_meta("chat-1", meta("hello", true)));
        assert!(state.on_meta("chat-1", meta("hello again", true)));
        let other_agent = ChatTabMeta {
            agent_id: AcpAgentId::Codex,
            ..meta("hello again", true)
        };
        assert!(state.on_meta("chat-1", other_agent.clone()));
        assert_eq!(state.meta("chat-1"), Some(&other_agent));
    }

    #[test]
    fn 바쁨_표시는_1에서_0_3까지_갔다가_돌아온다() {
        assert!((pulse_opacity(0.0) - 1.0).abs() < 1e-3);
        assert!((pulse_opacity(0.5) - 0.3).abs() < 1e-3);
        assert!((pulse_opacity(1.0) - 1.0).abs() < 1e-3);
        // Halfway down, and the same on the way back up.
        assert!((pulse_opacity(0.25) - 0.65).abs() < 1e-3);
        assert!((pulse_opacity(0.75) - 0.65).abs() < 1e-3);
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::RefCell;

    use gpui::{Modifiers, Task, TestAppContext, VisualTestContext};

    use super::*;
    use crate::model_catalog::LiveModel;
    use crate::model_picker::{AgentHealth, AgentHealthStatus, KeyProvider};
    use crate::ui::testing::{center, click, look, window};

    struct Host;

    impl ModelPickerHost for Host {
        fn probe(&self, _: &mut App) -> Task<Vec<AgentHealth>> {
            Task::ready(vec![AgentHealth {
                id: "codex".into(),
                status: AgentHealthStatus::Ready,
                resolved_path: None,
                detail: None,
                auto: false,
                plan: None,
            }])
        }

        fn set_api_key(
            &self,
            _: KeyProvider,
            _: String,
            _: &mut App,
        ) -> Task<Result<bool, String>> {
            Task::ready(Ok(true))
        }

        fn models(&self, _: &App) -> Vec<LiveModel> {
            crate::model_catalog::ModelCatalogStore::new().all_agent_models()
        }
    }

    /// Stands in for a chat panel: shows which tab it belongs to.
    struct Panel(String);

    impl Render for Panel {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(self.0.clone())
        }
    }

    struct Fixture {
        tabs: Entity<ChatTabs>,
        specs: Rc<RefCell<Vec<PanelSpec>>>,
        events: Rc<RefCell<Vec<ChatTabsEvent>>>,
    }

    fn tabs(cx: &mut TestAppContext) -> (Fixture, &mut VisualTestContext) {
        let specs: Rc<RefCell<Vec<PanelSpec>>> = Rc::default();
        let events: Rc<RefCell<Vec<ChatTabsEvent>>> = Rc::default();
        let (made, seen) = (specs.clone(), events.clone());
        let (tabs, cx) = window(cx, move |window, cx| {
            cx.subscribe(&cx.entity(), move |_, _, event: &ChatTabsEvent, _| {
                seen.borrow_mut().push(event.clone());
            })
            .detach();
            ChatTabs::new(
                move |spec, _, cx| {
                    made.borrow_mut().push(spec.clone());
                    cx.new(|_| Panel(spec.tab_id)).into()
                },
                Rc::new(Host),
                |_| None,
                look(),
                window,
                cx,
            )
        });
        cx.run_until_parked();
        (
            Fixture {
                tabs,
                specs,
                events,
            },
            cx,
        )
    }

    fn state(fixture: &Fixture, cx: &mut VisualTestContext) -> TabsState {
        fixture.tabs.read_with(cx, |tabs, _| tabs.state().clone())
    }

    fn pick_codex(cx: &mut VisualTestContext) {
        click("chat-tab-new", cx);
        click("new-tab-agent-codex", cx);
    }

    #[gpui::test]
    fn 첫_탭의_패널만_지난_대화를_되살린다(cx: &mut TestAppContext) {
        let (fixture, cx) = tabs(cx);
        pick_codex(cx);
        assert_eq!(
            *fixture.specs.borrow(),
            [
                PanelSpec {
                    tab_id: "chat-1".into(),
                    restore: true,
                    initial_agent: None
                },
                PanelSpec {
                    tab_id: "chat-2".into(),
                    restore: false,
                    initial_agent: Some(AcpAgentId::Codex)
                },
            ]
        );
        assert_eq!(state(&fixture, cx).active_id(), "chat-2");
        assert_eq!(
            *fixture.events.borrow(),
            [ChatTabsEvent::Activated("chat-2".into())]
        );
    }

    #[gpui::test]
    fn 탭을_누르면_그_탭이_앞으로_나온다(cx: &mut TestAppContext) {
        let (fixture, cx) = tabs(cx);
        pick_codex(cx);
        click("chat-tab-0", cx);
        assert_eq!(state(&fixture, cx).active_id(), "chat-1");
        assert_eq!(
            fixture.events.borrow().last(),
            Some(&ChatTabsEvent::Activated("chat-1".into()))
        );
    }

    #[gpui::test]
    fn 닫기를_누르면_탭이_닫히고_탭을_누른_것으로는_치지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (fixture, cx) = tabs(cx);
        pick_codex(cx);
        // The first tab is behind: closing it must not bring it forward first.
        click("chat-tab-close-0", cx);
        let now = state(&fixture, cx);
        assert_eq!(now.tabs(), ["chat-2"]);
        assert_eq!(now.active_id(), "chat-2");
        assert_eq!(
            *fixture.events.borrow(),
            [
                ChatTabsEvent::Activated("chat-2".into()),
                ChatTabsEvent::Closed("chat-1".into()),
            ]
        );
    }

    #[gpui::test]
    fn 가운데_버튼으로_눌러도_탭이_닫힌다(cx: &mut TestAppContext) {
        let (fixture, cx) = tabs(cx);
        pick_codex(cx);
        let at = center("chat-tab-1", cx);
        cx.simulate_mouse_down(at, MouseButton::Middle, Modifiers::none());
        cx.simulate_mouse_up(at, MouseButton::Middle, Modifiers::none());
        cx.run_until_parked();
        let now = state(&fixture, cx);
        assert_eq!(now.tabs(), ["chat-1"]);
        assert_eq!(now.active_id(), "chat-1");
    }

    #[gpui::test]
    fn 마지막_탭을_닫으면_새_패널이_열린다(cx: &mut TestAppContext) {
        let (fixture, cx) = tabs(cx);
        click("chat-tab-close-0", cx);
        assert_eq!(state(&fixture, cx).tabs(), ["chat-2"]);
        assert_eq!(
            fixture.specs.borrow().last(),
            Some(&PanelSpec {
                tab_id: "chat-2".into(),
                restore: false,
                initial_agent: None
            })
        );
        assert_eq!(
            *fixture.events.borrow(),
            [
                ChatTabsEvent::Closed("chat-1".into()),
                ChatTabsEvent::Activated("chat-2".into()),
            ]
        );
    }

    #[gpui::test]
    fn 초점이_있는_탭에서_엔터나_스페이스를_누르면_앞으로_나온다(
        cx: &mut TestAppContext,
    ) {
        let (fixture, cx) = tabs(cx);
        pick_codex(cx);
        let focus_first = |cx: &mut VisualTestContext| {
            fixture.tabs.update_in(cx, |tabs, window, _| {
                window.focus(&tabs.open["chat-1"].focus);
            });
        };
        focus_first(cx);
        cx.simulate_keystrokes("enter");
        assert_eq!(state(&fixture, cx).active_id(), "chat-1");
        click("chat-tab-1", cx);
        assert_eq!(state(&fixture, cx).active_id(), "chat-2");
        focus_first(cx);
        cx.simulate_keystrokes("space");
        assert_eq!(state(&fixture, cx).active_id(), "chat-1");
    }

    #[gpui::test]
    fn 패널이_보고한_제목이_탭에_보이고_탭은_180픽셀을_넘지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (fixture, cx) = tabs(cx);
        fixture.tabs.update(cx, |tabs, cx| {
            tabs.set_meta(
                "chat-1",
                ChatTabMeta {
                    title: "A conversation title that is far too long to fit in one tab".into(),
                    agent_id: AcpAgentId::ClaudeCode,
                    busy: true,
                },
                cx,
            );
        });
        cx.run_until_parked();
        let tab = cx.debug_bounds("chat-tab-0").unwrap();
        assert_eq!(tab.size.width, px(180.0));
        assert_eq!(tab.size.height, px(30.0));
    }
}
