//! Port of `src/renderer/src/components/OnboardingModal.tsx` — first-run provider setup.
//! Probes every agent for real (ACP handshake / authenticated API call) so a green row
//! means it actually works, and shows "connected automatically" when an existing CLI
//! login already covers it.
//!
//! The modal covers the whole window wherever the host puts it. It shows on first run
//! only; once an agent is picked or onboarding is skipped it renders nothing.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, ClipboardItem, Context, Empty, Entity, FontWeight, Rgba, Subscription, Task,
    Window, anchored, deferred, div, point, prelude::*, px, rgba,
};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::text_input::{Submitted, TextInput};
use rever_ui_state::constants::{ACP_AGENTS, AcpAgentDef, AcpProvider};

use crate::host::{AgentHealth, AgentHealthStatus, KeyProvider, SetupHost};
use crate::ui::{
    DISABLED_OPACITY, LookFn, MONO, SANS, Tokens, Z_MODAL, clickable, field_box, fill, js_trim,
    link_button, named, provider_icon, secret_field, shadow, text, use_button,
};

/// How long the "Copied ✓" label stays.
const COPIED_MS: u64 = 1500;

/// `STATUS_COLOR`.
pub(crate) fn status_color(status: AgentHealthStatus, tokens: &Tokens) -> Rgba {
    match status {
        AgentHealthStatus::Ready => tokens.status_ok,
        AgentHealthStatus::NotInstalled => tokens.theme.http_none,
        AgentHealthStatus::NeedsKey | AgentHealthStatus::NeedsLogin => tokens.theme.status_warn,
        AgentHealthStatus::AuthFailed | AgentHealthStatus::Failed => tokens.theme.status_error,
    }
}

/// The `onboard.status.<status>` key.
pub fn status_key(status: AgentHealthStatus) -> &'static str {
    match status {
        AgentHealthStatus::Ready => "onboard.status.ready",
        AgentHealthStatus::NotInstalled => "onboard.status.not-installed",
        AgentHealthStatus::NeedsKey => "onboard.status.needs-key",
        AgentHealthStatus::NeedsLogin => "onboard.status.needs-login",
        AgentHealthStatus::AuthFailed => "onboard.status.auth-failed",
        AgentHealthStatus::Failed => "onboard.status.failed",
    }
}

/// `isKeyProvider`: the agents that run on a pasted API key.
pub fn key_provider(def: &AcpAgentDef) -> Option<KeyProvider> {
    match def.provider? {
        AcpProvider::Anthropic => Some(KeyProvider::Anthropic),
        AcpProvider::Openai => Some(KeyProvider::Openai),
        AcpProvider::Grok => Some(KeyProvider::Grok),
        AcpProvider::Glm => Some(KeyProvider::Glm),
        AcpProvider::Acp | AcpProvider::ClaudeCli => None,
    }
}

/// The command line under a row: how to install the agent, or how to sign in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hint {
    pub title_key: &'static str,
    pub cmd: &'static str,
}

/// What one agent's row shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRow {
    /// `failed` until the agent has been probed.
    pub status: AgentHealthStatus,
    pub ready: bool,
    /// The status text's key: "connected automatically" wins over the status.
    pub label_key: &'static str,
    plan: Option<String>,
    detail: Option<String>,
    pub hint: Option<Hint>,
    /// Set when the row has an API key field.
    pub key_provider: Option<KeyProvider>,
    /// Half transparent: the first scan has not reached this agent yet.
    pub dimmed: bool,
}

impl AgentRow {
    /// The line under the name: status, then the plan and the detail when there are any.
    pub fn status_line(&self, lang: Lang) -> String {
        let mut line = lang.t(self.label_key).to_owned();
        // `h.plan && h.plan !== 'api-key'`: an empty plan is no plan.
        if let Some(plan) = self
            .plan
            .as_deref()
            .filter(|plan| !plan.is_empty() && *plan != "api-key")
        {
            line.push_str(" · ");
            line.push_str(&fill(lang.t("onboard.plan"), &[("plan", plan)]));
        }
        if let Some(detail) = self.detail.as_deref().filter(|detail| !detail.is_empty()) {
            line.push_str(" · ");
            line.push_str(detail);
        }
        line
    }
}

/// The modal's state. Kept apart from the view so it can be tested without a window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnboardingState {
    health: HashMap<String, AgentHealth>,
    scanning: bool,
    saving_key: Option<KeyProvider>,
    copied: Option<String>,
}

impl Default for OnboardingState {
    fn default() -> Self {
        Self {
            health: HashMap::new(),
            scanning: true,
            saving_key: None,
            copied: None,
        }
    }
}

impl OnboardingState {
    pub fn is_scanning(&self) -> bool {
        self.scanning
    }

    pub fn begin_scan(&mut self) {
        self.scanning = true;
    }

    /// A probe came back. Replaces every earlier result; of two entries with one id the
    /// later one stays, as with `Object.fromEntries`.
    pub fn scanned(&mut self, results: Vec<AgentHealth>) {
        self.health = results
            .into_iter()
            .map(|health| (health.id.clone(), health))
            .collect();
        self.scanning = false;
    }

    pub fn health(&self, id: &str) -> Option<&AgentHealth> {
        self.health.get(id)
    }

    pub fn ready_count(&self) -> usize {
        self.health
            .values()
            .filter(|health| health.status == AgentHealthStatus::Ready)
            .count()
    }

    /// The line over the list: scanning, or how many agents are ready.
    pub fn summary(&self, lang: Lang) -> String {
        if self.scanning {
            lang.t("onboard.scanning").to_owned()
        } else {
            fill(
                lang.t("onboard.ready"),
                &[("n", &self.ready_count().to_string())],
            )
        }
    }

    pub fn row(&self, def: &AcpAgentDef) -> AgentRow {
        let health = self.health(def.id.as_str());
        let status = health.map_or(AgentHealthStatus::Failed, |health| health.status);
        let hint = match status {
            AgentHealthStatus::NotInstalled => Some(Hint {
                title_key: "onboard.installWith",
                cmd: def.install_hint,
            }),
            AgentHealthStatus::NeedsLogin => {
                def.login_hint
                    .filter(|cmd| !cmd.is_empty())
                    .map(|cmd| Hint {
                        title_key: "onboard.signInWith",
                        cmd,
                    })
            }
            _ => None,
        };
        AgentRow {
            status,
            ready: status == AgentHealthStatus::Ready,
            label_key: if health.is_some_and(|health| health.auto) {
                "onboard.status.auto"
            } else {
                status_key(status)
            },
            plan: health.and_then(|health| health.plan.clone()),
            detail: health.and_then(|health| health.detail.clone()),
            hint,
            key_provider: key_provider(def).filter(|_| status != AgentHealthStatus::Ready),
            dimmed: self.scanning && health.is_none(),
        }
    }

    /// The Save button is off while that key is being saved and while the field holds
    /// nothing but whitespace.
    pub fn save_disabled(&self, provider: KeyProvider, input: &str) -> bool {
        self.saving_key == Some(provider) || js_trim(input).is_empty()
    }

    pub fn save_label(&self, provider: KeyProvider, lang: Lang) -> &'static str {
        if self.saving_key == Some(provider) {
            "…"
        } else {
            lang.t("onboard.save")
        }
    }

    pub fn copy_label(&self, cmd: &str, lang: Lang) -> &'static str {
        if self.copied.as_deref() == Some(cmd) {
            lang.t("onboard.copied")
        } else {
            lang.t("onboard.copy")
        }
    }
}

pub struct OnboardingModal {
    host: Rc<dyn SetupHost>,
    look: LookFn,
    visible: bool,
    state: OnboardingState,
    /// One field per key provider. Its text is the only place a typed key is held, and
    /// it is emptied as soon as the key is saved.
    keys: Vec<(KeyProvider, Entity<TextInput>)>,
    _subscriptions: Vec<Subscription>,
}

impl OnboardingModal {
    pub fn new(host: Rc<dyn SetupHost>, look: LookFn, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        let keys = ACP_AGENTS
            .iter()
            .filter_map(key_provider)
            .map(|provider| {
                // The placeholder is drawn by the masked field, in the current language.
                let input = cx.new(|cx| TextInput::new("", cx));
                // Typing turns the Save button on and off.
                subscriptions.push(cx.observe(&input, |_, _, cx| cx.notify()));
                subscriptions.push(cx.subscribe(&input, move |this, _, _: &Submitted, cx| {
                    this.save_key(provider, cx)
                }));
                (provider, input)
            })
            .collect();
        let mut modal = Self {
            visible: !host.onboarded(cx),
            host,
            look,
            state: OnboardingState::default(),
            keys,
            _subscriptions: subscriptions,
        };
        if modal.visible {
            modal.scan(cx);
        }
        modal
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn state(&self) -> &OnboardingState {
        &self.state
    }

    fn key_input(&self, provider: KeyProvider) -> Option<&Entity<TextInput>> {
        self.keys
            .iter()
            .find(|(of, _)| *of == provider)
            .map(|(_, input)| input)
    }

    fn begin_scan(&mut self, cx: &mut Context<Self>) -> Task<Vec<AgentHealth>> {
        self.state.begin_scan();
        cx.notify();
        self.host.probe_agents(cx)
    }

    fn scanned(&mut self, results: Vec<AgentHealth>, cx: &mut Context<Self>) {
        self.state.scanned(results);
        cx.notify();
    }

    /// Probe every agent again. Of two scans in flight, the one answering last wins.
    pub fn scan(&mut self, cx: &mut Context<Self>) {
        let probe = self.begin_scan(cx);
        cx.spawn(async move |this, cx| {
            let results = probe.await;
            let _ = this.update(cx, |this, cx| this.scanned(results, cx));
        })
        .detach();
    }

    fn choose(&mut self, def: &'static AcpAgentDef, cx: &mut Context<Self>) {
        let path = self
            .state
            .health(def.id.as_str())
            .and_then(|health| health.resolved_path.clone());
        let host = self.host.clone();
        // After this update: the host's handler may reach back into the modal.
        cx.defer(move |cx| host.pick_agent(def.id, path, cx));
        self.visible = false;
        cx.notify();
    }

    fn skip(&mut self, cx: &mut Context<Self>) {
        let host = self.host.clone();
        cx.defer(move |cx| host.skip_onboarding(cx));
        self.visible = false;
        cx.notify();
    }

    fn save_key(&mut self, provider: KeyProvider, cx: &mut Context<Self>) {
        let Some(input) = self.key_input(provider).cloned() else {
            return;
        };
        let key = js_trim(input.read(cx).text()).to_owned();
        if key.is_empty() {
            return;
        }
        self.state.saving_key = Some(provider);
        cx.notify();
        let saved = self.host.set_api_key(provider, key, cx);
        cx.spawn(async move |this, cx| {
            // A key the host could not store stays in the field, to be sent again.
            if saved.await.is_ok() {
                let probe = this.update(cx, |this, cx| {
                    input.update(cx, |input, cx| input.set_text("", cx));
                    this.begin_scan(cx)
                });
                if let Ok(probe) = probe {
                    let results = probe.await;
                    let _ = this.update(cx, |this, cx| this.scanned(results, cx));
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.state.saving_key = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn copy(&mut self, text: &'static str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        self.state.copied = Some(text.to_owned());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(COPIED_MS))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.state.copied = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn render_row(
        &self,
        def: &'static AcpAgentDef,
        tokens: &Tokens,
        lang: Lang,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = tokens.theme;
        let row = self.state.row(def);
        let id = def.id.as_str();

        let icon = provider_icon(def.id, 16.0, theme.text).unwrap_or_else(|| {
            // `fontSize: size * 0.7`
            div()
                .text_size(px(11.2))
                .font_weight(FontWeight::BOLD)
                .child(def.icon)
                .into_any_element()
        });
        let use_it =
            use_button(named("onboard-use", id), lang.t("onboard.use"), tokens).map(|button| {
                if row.ready {
                    button
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| this.choose(def, cx)))
                } else {
                    // Disabled uses a muted surface rather than a low-opacity accent (which
                    // dropped to ~1.7:1).
                    button
                        .bg(theme.surface_3)
                        .border_color(theme.border)
                        .text_color(theme.text_2)
                        .cursor_not_allowed()
                        .opacity(DISABLED_OPACITY)
                }
            });
        let top = div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .size(px(30.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.0))
                    .bg(theme.surface_3)
                    .child(icon),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(text(13.0).font_weight(FontWeight::MEDIUM).child(def.name))
                    .child(
                        text(11.0)
                            .debug_selector(|| format!("onboard-status-{id}"))
                            .text_color(status_color(row.status, tokens))
                            .child(row.status_line(lang)),
                    ),
            )
            .child(use_it);

        let hint = row.hint.map(|hint| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.0))
                .child(
                    text(10.0)
                        .text_color(theme.text_2)
                        .child(lang.t(hint.title_key)),
                )
                .child(
                    text(10.0)
                        .flex_1()
                        .min_w_0()
                        .px(px(6.0))
                        .py(px(3.0))
                        .rounded(px(4.0))
                        .bg(theme.bg_bar)
                        .border_1()
                        .border_color(theme.border_2)
                        .font_family(MONO)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(hint.cmd),
                )
                .child(
                    link_button(
                        named("onboard-copy", id),
                        self.state.copy_label(hint.cmd, lang),
                        false,
                        tokens,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.copy(hint.cmd, cx))),
                )
        });

        let key_row = row.key_provider.and_then(|provider| {
            let input = self.key_input(provider)?;
            let disabled = self.state.save_disabled(provider, input.read(cx).text());
            let save = clickable(named("onboard-save", id))
                .flex_none()
                .px(px(12.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .bg(theme.surface_3)
                .border_1()
                .border_color(theme.border_2)
                .text_color(theme.text)
                .text_size(px(12.0))
                .line_height(px(16.0))
                .map(|button| {
                    if disabled {
                        button.opacity(DISABLED_OPACITY)
                    } else {
                        button.cursor_pointer().on_click(
                            cx.listener(move |this, _, _, cx| this.save_key(provider, cx)),
                        )
                    }
                })
                .child(self.state.save_label(provider, lang));
            Some(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        field_box(27.0, 8.0)
                            .debug_selector(|| format!("onboard-key-{id}"))
                            .flex_1()
                            .min_w_0()
                            .rounded(px(6.0))
                            .bg(theme.bg_bar)
                            .border_color(theme.border_2)
                            .text_size(px(12.0))
                            .child(secret_field(
                                input,
                                lang.t("onboard.keyPlaceholder"),
                                theme.text,
                                cx,
                            )),
                    )
                    .child(save),
            )
        });

        div()
            .debug_selector(|| format!("onboard-row-{id}"))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(10.0))
            .rounded(px(8.0))
            .bg(theme.surface_2)
            .border_1()
            .border_color(theme.border)
            .when(row.dimmed, |row| row.opacity(0.5))
            .child(top)
            .children(hint)
            .children(key_row)
            .into_any_element()
    }
}

impl Render for OnboardingModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return Empty.into_any_element();
        }
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let lang = look.lang;
        let scanning = self.state.is_scanning();

        let mut rows = Vec::with_capacity(ACP_AGENTS.len());
        for def in ACP_AGENTS {
            rows.push(self.render_row(def, &tokens, lang, cx));
        }

        // `card`: 460px of content inside 20px of padding and the border.
        let card = div()
            .id("onboarding-card")
            .debug_selector(|| "onboarding-card".into())
            .w(px(502.0))
            .max_w_full()
            .max_h_full()
            .overflow_y_scroll()
            .p(px(20.0))
            .rounded(px(12.0))
            .bg(theme.bg_bar)
            .border_1()
            .border_color(theme.border)
            .text_color(theme.text)
            // 0 16px 48px rgba(0, 0, 0, 0.55)
            .shadow(vec![shadow(16.0, 48.0, rgba(0x0000008c))])
            .child(
                div()
                    .text_size(px(18.0))
                    .line_height(px(21.0))
                    .font_weight(FontWeight::BOLD)
                    .child(lang.t("onboard.title")),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .text_size(px(12.0))
                    .line_height(px(18.0))
                    .text_color(theme.text_2)
                    .child(lang.t("onboard.desc")),
            )
            .child(
                // `statusRow`
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mt(px(16.0))
                    .pb(px(6.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        text(11.0)
                            .debug_selector(|| "onboard-summary".into())
                            .text_color(theme.text_2)
                            .child(self.state.summary(lang)),
                    )
                    .child(
                        link_button(
                            "onboard-recheck",
                            lang.t("onboard.recheck"),
                            scanning,
                            &tokens,
                        )
                        .when(!scanning, |button| {
                            button.on_click(cx.listener(|this, _, _, cx| this.scan(cx)))
                        }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .mt(px(8.0))
                    .children(rows),
            )
            .child(
                div().flex().justify_end().mt(px(14.0)).child(
                    link_button("onboard-skip", lang.t("onboard.skip"), false, &tokens)
                        .on_click(cx.listener(|this, _, _, cx| this.skip(cx))),
                ),
            );

        // `backdrop`: over the whole window, and nothing under it takes the mouse.
        let viewport = window.viewport_size();
        let backdrop = div()
            .w(viewport.width)
            .h(viewport.height)
            .p(px(16.0))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000099))
            .font_family(SANS)
            .text_size(px(13.0))
            .occlude()
            .child(card);
        deferred(anchored().position(point(px(0.0), px(0.0))).child(backdrop))
            .with_priority(Z_MODAL)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use rever_ui_state::constants::AcpAgentId;

    use super::*;

    fn def(id: AcpAgentId) -> &'static AcpAgentDef {
        ACP_AGENTS.iter().find(|def| def.id == id).unwrap()
    }

    fn health(id: &str, status: AgentHealthStatus) -> AgentHealth {
        AgentHealth {
            id: id.to_owned(),
            status,
            resolved_path: None,
            detail: None,
            auto: false,
            plan: None,
        }
    }

    fn scanned(results: Vec<AgentHealth>) -> OnboardingState {
        let mut state = OnboardingState::default();
        state.scanned(results);
        state
    }

    #[test]
    fn 처음에는_확인_중이고_결과가_없다() {
        let state = OnboardingState::default();
        assert!(state.is_scanning());
        assert_eq!(state.ready_count(), 0);
        assert_eq!(state.summary(Lang::En), "Checking your machine…");
        assert_eq!(state.summary(Lang::Ko), "이 컴퓨터를 확인하는 중…");
    }

    #[test]
    fn 확인이_끝나면_사용_가능한_수를_보여준다() {
        let state = scanned(vec![
            health("claude-code", AgentHealthStatus::Ready),
            health("codex", AgentHealthStatus::NotInstalled),
            health("anthropic", AgentHealthStatus::Ready),
        ]);
        assert!(!state.is_scanning());
        assert_eq!(state.ready_count(), 2);
        assert_eq!(state.summary(Lang::En), "2 ready");
        assert_eq!(state.summary(Lang::Ko), "2개 사용 가능");
    }

    #[test]
    fn 새_결과는_앞선_결과를_통째로_바꾼다() {
        let mut state = scanned(vec![health("codex", AgentHealthStatus::Ready)]);
        state.begin_scan();
        assert!(state.is_scanning());
        // The earlier result stays visible while the new scan runs.
        assert_eq!(state.ready_count(), 1);
        state.scanned(vec![health("openai", AgentHealthStatus::NeedsKey)]);
        assert_eq!(state.health("codex"), None);
        assert_eq!(state.ready_count(), 0);
    }

    #[test]
    fn 같은_id가_두_번_오면_뒤의_것이_남는다() {
        let state = scanned(vec![
            health("codex", AgentHealthStatus::Failed),
            health("codex", AgentHealthStatus::Ready),
        ]);
        assert_eq!(
            state.health("codex").map(|health| health.status),
            Some(AgentHealthStatus::Ready)
        );
        assert_eq!(state.ready_count(), 1);
    }

    #[test]
    fn 카탈로그에_없는_id의_결과도_사용_가능_수에_든다() {
        let state = scanned(vec![health("gemini", AgentHealthStatus::Ready)]);
        assert_eq!(state.ready_count(), 1);
    }

    #[test]
    fn 아직_확인되지_않은_에이전트는_실패로_보이고_확인_중에는_흐리다() {
        let state = OnboardingState::default();
        let row = state.row(def(AcpAgentId::Codex));
        assert_eq!(row.status, AgentHealthStatus::Failed);
        assert!(!row.ready);
        assert!(row.dimmed);
        assert_eq!(row.status_line(Lang::En), "Failed to start");
        assert_eq!(row.hint, None);

        // Once the scan is over a missing result is a plain failure.
        let row = scanned(vec![]).row(def(AcpAgentId::Codex));
        assert!(!row.dimmed);
    }

    #[test]
    fn 다시_확인하는_동안_이미_결과가_있는_줄은_흐려지지_않는다() {
        let mut state = scanned(vec![health("codex", AgentHealthStatus::Ready)]);
        state.begin_scan();
        assert!(!state.row(def(AcpAgentId::Codex)).dimmed);
        assert!(state.row(def(AcpAgentId::Openai)).dimmed);
    }

    #[test]
    fn 설치되지_않은_에이전트는_설치_명령을_보여준다() {
        let state = scanned(vec![health("codex", AgentHealthStatus::NotInstalled)]);
        let row = state.row(def(AcpAgentId::Codex));
        assert_eq!(
            row.hint,
            Some(Hint {
                title_key: "onboard.installWith",
                cmd: "npm i -g @agentclientprotocol/codex-acp"
            })
        );
        assert_eq!(row.status_line(Lang::En), "Not installed");
    }

    #[test]
    fn 로그인이_필요한_에이전트는_로그인_명령을_보여준다() {
        let state = scanned(vec![health("claude-code", AgentHealthStatus::NeedsLogin)]);
        let row = state.row(def(AcpAgentId::ClaudeCode));
        assert_eq!(
            row.hint,
            Some(Hint {
                title_key: "onboard.signInWith",
                cmd: "claude login"
            })
        );
        assert_eq!(row.status_line(Lang::Ko), "로그인 필요");
    }

    #[test]
    fn 로그인_명령이_없는_에이전트는_로그인이_필요해도_명령_줄이_없다() {
        // `anthropic` has no loginHint.
        let state = scanned(vec![health("anthropic", AgentHealthStatus::NeedsLogin)]);
        assert_eq!(state.row(def(AcpAgentId::Anthropic)).hint, None);
    }

    #[test]
    fn 다른_상태에는_명령_줄이_없다() {
        for status in [
            AgentHealthStatus::Ready,
            AgentHealthStatus::NeedsKey,
            AgentHealthStatus::AuthFailed,
            AgentHealthStatus::Failed,
        ] {
            let state = scanned(vec![health("claude-code", status)]);
            assert_eq!(
                state.row(def(AcpAgentId::ClaudeCode)).hint,
                None,
                "{status:?}"
            );
        }
    }

    #[test]
    fn 자동_연결은_상태_문구_대신_자동_연결_문구를_쓴다() {
        let state = scanned(vec![AgentHealth {
            auto: true,
            plan: Some("max".to_owned()),
            ..health("claude-code", AgentHealthStatus::Ready)
        }]);
        let row = state.row(def(AcpAgentId::ClaudeCode));
        assert_eq!(row.label_key, "onboard.status.auto");
        assert_eq!(
            row.status_line(Lang::En),
            "Connected automatically · max plan"
        );
        assert_eq!(row.status_line(Lang::Ko), "자동으로 연결됨 · max 플랜");
    }

    #[test]
    fn api_key_플랜과_빈_플랜은_보여주지_않는다() {
        for plan in ["api-key", ""] {
            let state = scanned(vec![AgentHealth {
                plan: Some(plan.to_owned()),
                ..health("anthropic", AgentHealthStatus::Ready)
            }]);
            assert_eq!(
                state.row(def(AcpAgentId::Anthropic)).status_line(Lang::En),
                "Ready",
                "{plan:?}"
            );
        }
    }

    #[test]
    fn 상세_사유는_상태_뒤에_붙고_빈_사유는_붙지_않는다() {
        let with = |detail: &str| {
            scanned(vec![AgentHealth {
                detail: Some(detail.to_owned()),
                plan: Some("chatgpt".to_owned()),
                ..health("codex", AgentHealthStatus::Failed)
            }])
            .row(def(AcpAgentId::Codex))
            .status_line(Lang::En)
        };
        assert_eq!(
            with("handshake timed out"),
            "Failed to start · chatgpt plan · handshake timed out"
        );
        assert_eq!(with(""), "Failed to start · chatgpt plan");
    }

    #[test]
    fn 상태마다_문구_키가_있다() {
        for (status, text) in [
            (AgentHealthStatus::Ready, "Ready"),
            (AgentHealthStatus::NotInstalled, "Not installed"),
            (AgentHealthStatus::NeedsKey, "Needs API key"),
            (AgentHealthStatus::NeedsLogin, "Sign-in required"),
            (AgentHealthStatus::AuthFailed, "Key rejected"),
            (AgentHealthStatus::Failed, "Failed to start"),
        ] {
            assert_eq!(Lang::En.t(status_key(status)), text);
        }
    }

    #[test]
    fn 상태_색은_컴포넌트의_표와_같다() {
        let tokens = Tokens::light();
        let color = |status| status_color(status, &tokens);
        assert_eq!(color(AgentHealthStatus::Ready), tokens.status_ok);
        assert_eq!(
            color(AgentHealthStatus::NotInstalled),
            tokens.theme.http_none
        );
        assert_eq!(color(AgentHealthStatus::NeedsKey), tokens.theme.status_warn);
        assert_eq!(
            color(AgentHealthStatus::NeedsLogin),
            tokens.theme.status_warn
        );
        assert_eq!(
            color(AgentHealthStatus::AuthFailed),
            tokens.theme.status_error
        );
        assert_eq!(color(AgentHealthStatus::Failed), tokens.theme.status_error);
    }

    #[test]
    fn api_키로_쓰는_에이전트만_키_제공자다() {
        assert_eq!(
            key_provider(def(AcpAgentId::Anthropic)),
            Some(KeyProvider::Anthropic)
        );
        assert_eq!(
            key_provider(def(AcpAgentId::Openai)),
            Some(KeyProvider::Openai)
        );
        assert_eq!(key_provider(def(AcpAgentId::Grok)), Some(KeyProvider::Grok));
        assert_eq!(key_provider(def(AcpAgentId::Glm)), Some(KeyProvider::Glm));
        // claude-cli, and Codex with no provider at all.
        assert_eq!(key_provider(def(AcpAgentId::ClaudeCode)), None);
        assert_eq!(key_provider(def(AcpAgentId::Codex)), None);
    }

    #[test]
    fn 키_입력란은_키_제공자가_준비되지_않았을_때만_나온다() {
        let state = scanned(vec![
            health("anthropic", AgentHealthStatus::NeedsKey),
            health("openai", AgentHealthStatus::Ready),
            health("grok", AgentHealthStatus::AuthFailed),
            health("claude-code", AgentHealthStatus::NeedsLogin),
        ]);
        assert_eq!(
            state.row(def(AcpAgentId::Anthropic)).key_provider,
            Some(KeyProvider::Anthropic)
        );
        assert_eq!(state.row(def(AcpAgentId::Openai)).key_provider, None);
        assert_eq!(
            state.row(def(AcpAgentId::Grok)).key_provider,
            Some(KeyProvider::Grok)
        );
        // Not probed yet counts as failed: the field is there.
        assert_eq!(
            state.row(def(AcpAgentId::Glm)).key_provider,
            Some(KeyProvider::Glm)
        );
        assert_eq!(state.row(def(AcpAgentId::ClaudeCode)).key_provider, None);
    }

    #[test]
    fn 저장_버튼은_빈_입력과_저장_중에_꺼진다() {
        let mut state = OnboardingState::default();
        assert!(state.save_disabled(KeyProvider::Openai, ""));
        assert!(state.save_disabled(KeyProvider::Openai, " \t\u{FEFF}"));
        assert!(!state.save_disabled(KeyProvider::Openai, " sk-1 "));
        assert_eq!(state.save_label(KeyProvider::Openai, Lang::En), "Save");

        state.saving_key = Some(KeyProvider::Openai);
        assert!(state.save_disabled(KeyProvider::Openai, "sk-1"));
        assert_eq!(state.save_label(KeyProvider::Openai, Lang::En), "…");
        // Another provider's button is not affected.
        assert!(!state.save_disabled(KeyProvider::Grok, "xai-1"));
        assert_eq!(state.save_label(KeyProvider::Grok, Lang::Ko), "저장");
    }

    #[test]
    fn 복사한_명령의_버튼만_복사됨으로_바뀐다() {
        let mut state = OnboardingState::default();
        assert_eq!(state.copy_label("claude login", Lang::En), "Copy");
        state.copied = Some("claude login".to_owned());
        assert_eq!(state.copy_label("claude login", Lang::En), "Copied ✓");
        assert_eq!(state.copy_label("codex login", Lang::En), "Copy");
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::Cell;

    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::ui::testing::{FakeHost, center, click, elsewhere, look, stage};

    fn health(id: &str, status: AgentHealthStatus) -> AgentHealth {
        AgentHealth {
            id: id.to_owned(),
            status,
            resolved_path: None,
            detail: None,
            auto: false,
            plan: None,
        }
    }

    /// A first run: Claude Code is signed in, the OpenAI key is missing, Codex is absent.
    fn first_run() -> Rc<FakeHost> {
        let host = Rc::new(FakeHost::default());
        *host.health.borrow_mut() = vec![
            AgentHealth {
                resolved_path: Some("/usr/local/bin/claude".to_owned()),
                auto: true,
                plan: Some("max".to_owned()),
                ..health("claude-code", AgentHealthStatus::Ready)
            },
            health("openai", AgentHealthStatus::NeedsKey),
            health("anthropic", AgentHealthStatus::NeedsKey),
            health("grok", AgentHealthStatus::NeedsKey),
            health("glm", AgentHealthStatus::NeedsKey),
            health("codex", AgentHealthStatus::NotInstalled),
        ];
        host
    }

    fn modal<'a>(
        host: &Rc<FakeHost>,
        cx: &'a mut TestAppContext,
    ) -> (
        Entity<OnboardingModal>,
        Rc<Cell<usize>>,
        &'a mut VisualTestContext,
    ) {
        let host = host.clone();
        stage(cx, move |cx| OnboardingModal::new(host, look(), cx))
    }

    fn type_key(
        modal: &Entity<OnboardingModal>,
        provider: KeyProvider,
        key: &str,
        cx: &mut VisualTestContext,
    ) {
        modal.update(cx, |modal, cx| {
            let input = modal.key_input(provider).unwrap().clone();
            input.update(cx, |input, cx| input.set_text(key.to_owned(), cx));
        });
        cx.run_until_parked();
    }

    fn key_text(
        modal: &Entity<OnboardingModal>,
        provider: KeyProvider,
        cx: &mut VisualTestContext,
    ) -> String {
        modal.read_with(cx, |modal, cx| {
            modal
                .key_input(provider)
                .unwrap()
                .read(cx)
                .text()
                .to_owned()
        })
    }

    #[gpui::test]
    fn 첫_실행에는_창_전체를_덮고_에이전트를_확인한다(cx: &mut TestAppContext) {
        let host = first_run();
        let (modal, _, cx) = modal(&host, cx);
        assert!(modal.read_with(cx, |modal, _| modal.is_visible()));
        assert_eq!(host.calls(), ["probe"]);
        assert_eq!(
            modal.read_with(cx, |modal, _| modal.state().summary(Lang::En)),
            "1 ready"
        );
        // 460px of content, 20px of padding and the border on each side.
        let card = cx.debug_bounds("onboarding-card").unwrap();
        assert_eq!(card.size.width, px(502.0));
        // Centered in the window, not in the corner of the stage it was mounted in.
        let window = cx.update(|window, _| window.viewport_size());
        assert_eq!(card.center().x, window.width / 2.0);
    }

    #[gpui::test]
    fn 이미_온보딩을_마쳤으면_아무것도_그리지_않고_확인도_하지_않는다(
        cx: &mut TestAppContext,
    ) {
        let host = first_run();
        host.onboarded.set(true);
        let (modal, presses, cx) = modal(&host, cx);
        assert!(!modal.read_with(cx, |modal, _| modal.is_visible()));
        assert!(cx.debug_bounds("onboarding-card").is_none());
        assert!(host.calls().is_empty());
        // Nothing covers the window.
        cx.simulate_click(elsewhere(), Modifiers::none());
        assert_eq!(presses.get(), 1);
    }

    #[gpui::test]
    fn 열려_있는_동안_아래로는_클릭이_가지_않는다(cx: &mut TestAppContext) {
        let host = first_run();
        let (modal, presses, cx) = modal(&host, cx);
        cx.simulate_click(elsewhere(), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(presses.get(), 0);
        // A click on the backdrop does not dismiss it either.
        assert!(modal.read_with(cx, |modal, _| modal.is_visible()));
    }

    #[gpui::test]
    fn 준비된_에이전트의_사용_버튼은_그_에이전트와_경로를_고르고_닫는다(
        cx: &mut TestAppContext,
    ) {
        let host = first_run();
        let (modal, _, cx) = modal(&host, cx);
        click("onboard-use-claude-code", cx);
        assert_eq!(
            host.calls().last().unwrap(),
            "pick_agent claude-code Some(\"/usr/local/bin/claude\")"
        );
        assert!(!modal.read_with(cx, |modal, _| modal.is_visible()));
    }

    #[gpui::test]
    fn 준비되지_않은_에이전트의_사용_버튼은_눌러도_아무_일도_없다(
        cx: &mut TestAppContext,
    ) {
        let host = first_run();
        let (modal, _, cx) = modal(&host, cx);
        click("onboard-use-codex", cx);
        click("onboard-use-openai", cx);
        assert_eq!(host.calls(), ["probe"]);
        assert!(modal.read_with(cx, |modal, _| modal.is_visible()));
    }

    #[gpui::test]
    fn 나중에_하기는_건너뛰고_닫는다(cx: &mut TestAppContext) {
        let host = first_run();
        let (modal, presses, cx) = modal(&host, cx);
        click("onboard-skip", cx);
        assert_eq!(host.calls().last().unwrap(), "skip_onboarding");
        assert!(!modal.read_with(cx, |modal, _| modal.is_visible()));
        // The window is free again.
        cx.simulate_click(elsewhere(), Modifiers::none());
        assert_eq!(presses.get(), 1);
    }

    #[gpui::test]
    fn 다시_확인은_에이전트를_한_번_더_확인한다(cx: &mut TestAppContext) {
        let host = first_run();
        let (modal, _, cx) = modal(&host, cx);
        host.health.borrow_mut()[5] = health("codex", AgentHealthStatus::Ready);
        click("onboard-recheck", cx);
        assert_eq!(host.calls(), ["probe", "probe"]);
        assert_eq!(
            modal.read_with(cx, |modal, _| modal.state().ready_count()),
            2
        );
    }

    #[gpui::test]
    fn 키를_저장하면_앞뒤_공백을_떼어_보내고_입력란을_비운_뒤_다시_확인한다(
        cx: &mut TestAppContext,
    ) {
        let host = first_run();
        let (modal, _, cx) = modal(&host, cx);
        type_key(&modal, KeyProvider::Openai, "  sk-test-123  ", cx);
        click("onboard-save-openai", cx);
        assert_eq!(
            host.calls(),
            ["probe", "set_api_key Openai sk-test-123", "probe"]
        );
        // The key is gone from the only place it was held.
        assert_eq!(key_text(&modal, KeyProvider::Openai, cx), "");
        assert_eq!(
            modal.read_with(cx, |modal, _| modal.state().saving_key),
            None
        );
    }

    #[gpui::test]
    fn 엔터로도_키를_저장한다(cx: &mut TestAppContext) {
        let host = first_run();
        let (modal, _, cx) = modal(&host, cx);
        type_key(&modal, KeyProvider::Grok, "xai-9", cx);
        modal.update(cx, |modal, cx| {
            let input = modal.key_input(KeyProvider::Grok).unwrap().clone();
            input.update(cx, |_, cx| cx.emit(Submitted("xai-9".to_owned())));
        });
        cx.run_until_parked();
        assert!(host.calls().contains(&"set_api_key Grok xai-9".to_owned()));
        assert_eq!(key_text(&modal, KeyProvider::Grok, cx), "");
    }

    #[gpui::test]
    fn 빈_키는_저장하지_않는다(cx: &mut TestAppContext) {
        let host = first_run();
        let (modal, _, cx) = modal(&host, cx);
        type_key(&modal, KeyProvider::Openai, "   ", cx);
        click("onboard-save-openai", cx);
        modal.update(cx, |modal, cx| modal.save_key(KeyProvider::Openai, cx));
        cx.run_until_parked();
        assert_eq!(host.calls(), ["probe"]);
    }

    #[gpui::test]
    fn 저장에_실패하면_키를_남겨_두고_다시_확인하지_않는다(
        cx: &mut TestAppContext,
    ) {
        let host = first_run();
        host.failing.borrow_mut().push("set_api_key");
        let (modal, _, cx) = modal(&host, cx);
        type_key(&modal, KeyProvider::Openai, "sk-test-123", cx);
        click("onboard-save-openai", cx);
        assert_eq!(host.calls(), ["probe", "set_api_key Openai sk-test-123"]);
        assert_eq!(key_text(&modal, KeyProvider::Openai, cx), "sk-test-123");
        assert_eq!(
            modal.read_with(cx, |modal, _| modal.state().saving_key),
            None
        );
    }

    #[gpui::test]
    fn 준비된_제공자에는_키_입력란이_없다(cx: &mut TestAppContext) {
        let host = first_run();
        host.health.borrow_mut()[1] = health("openai", AgentHealthStatus::Ready);
        let (_, _, cx) = modal(&host, cx);
        assert!(cx.debug_bounds("onboard-key-openai").is_none());
        assert!(cx.debug_bounds("onboard-key-anthropic").is_some());
        // Claude Code is not a key provider at all.
        assert!(cx.debug_bounds("onboard-key-claude-code").is_none());
    }

    #[gpui::test]
    fn 복사_버튼은_명령을_클립보드에_넣고_잠시_복사됨을_보여준다(
        cx: &mut TestAppContext,
    ) {
        let host = first_run();
        let (modal, _, cx) = modal(&host, cx);
        let codex = "npm i -g @agentclientprotocol/codex-acp";
        click("onboard-copy-codex", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(codex.to_owned())
        );
        assert_eq!(
            modal.read_with(cx, |modal, _| modal.state().copy_label(codex, Lang::En)),
            "Copied ✓"
        );
        cx.executor()
            .advance_clock(Duration::from_millis(COPIED_MS));
        cx.run_until_parked();
        assert_eq!(
            modal.read_with(cx, |modal, _| modal.state().copy_label(codex, Lang::En)),
            "Copy"
        );
    }

    #[gpui::test]
    fn 줄은_아이콘과_이름_상태_사용_버튼을_한_줄에_둔다(
        cx: &mut TestAppContext,
    ) {
        let host = first_run();
        let (_, _, cx) = modal(&host, cx);
        let row = cx.debug_bounds("onboard-row-claude-code").unwrap();
        let card = cx.debug_bounds("onboarding-card").unwrap();
        // The card's 20px padding and border on both sides.
        assert_eq!(row.size.width, px(460.0));
        assert_eq!(row.left(), card.left() + px(21.0));
        let button = cx.debug_bounds("onboard-use-claude-code").unwrap();
        // 5 + 16 + 5 and the border.
        assert_eq!(button.size.height, px(28.0));
        assert_eq!(button.right(), row.right() - px(11.0));
        let at = center("onboard-status-claude-code", cx);
        assert!(row.contains(&at));
    }
}
