//! Port of `src/renderer/src/components/chat/ModelPicker.tsx` — the unified model
//! picker: one searchable list of every model across providers, grouped by agent.
//! Choosing a model implicitly selects its provider (agent). Providers that need setup
//! show install/sign-in hints or an API-key input inline; only models whose provider is
//! `ready` are selectable.
//!
//! `window.rev.acp.probe`, `window.rev.settings.setApiKey` and the model catalog store
//! are the [`ModelPickerHost`]; `onSelect` and `onPickAgent` are the
//! [`ModelPickerEvent`]s.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, Focusable, FontWeight, MouseDownEvent, Rgba,
    Task, Window, anchored, deferred, div, point, prelude::*, px, rgba, svg,
};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::text_input::{Submitted, TextInput};
use rever_ui_menus::ui::{LookFn, fill};
use rever_ui_state::constants::{ACP_AGENTS, AcpAgentDef, AcpAgentId, AcpProvider};
use serde::{Deserialize, Serialize};

use crate::model_catalog::LiveModel;
use crate::provider_icon::provider_icon;
use crate::ui::{
    BoundsCell, MONO, Rect, Span, Tokens, Z_POPOVER, caret, ellipsis, field, js_trim,
    natural_width, shadow, single_line, text, tooltip, track_bounds,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentHealthStatus {
    Ready,
    NotInstalled,
    NeedsKey,
    NeedsLogin,
    AuthFailed,
    Failed,
}

/// What the main process found out about one agent (`AgentHealth` in the preload).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHealth {
    pub id: String,
    pub status: AgentHealthStatus,
    pub resolved_path: Option<String>,
    pub detail: Option<String>,
    pub auto: bool,
    pub plan: Option<String>,
}

/// The providers that run on an API key pasted into the picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyProvider {
    Anthropic,
    Openai,
    Grok,
    Glm,
}

impl KeyProvider {
    const ALL: [Self; 4] = [Self::Anthropic, Self::Openai, Self::Grok, Self::Glm];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::Grok => "grok",
            Self::Glm => "glm",
        }
    }
}

/// `isKeyProvider`: the agent's key provider, if it is one.
pub fn key_provider(def: &AcpAgentDef) -> Option<KeyProvider> {
    match def.provider? {
        AcpProvider::Anthropic => Some(KeyProvider::Anthropic),
        AcpProvider::Openai => Some(KeyProvider::Openai),
        AcpProvider::Grok => Some(KeyProvider::Grok),
        AcpProvider::Glm => Some(KeyProvider::Glm),
        AcpProvider::Acp | AcpProvider::ClaudeCli => None,
    }
}

/// What a key provider reports in place of a binary's path.
pub const API_SENTINEL: &str = "api";

pub fn resolved_path_for(def: &AcpAgentDef, health: Option<&AgentHealth>) -> String {
    if key_provider(def).is_some() {
        return API_SENTINEL.to_owned();
    }
    health
        .and_then(|h| h.resolved_path.clone())
        .unwrap_or_else(|| def.command.to_owned())
}

/// The probe results by agent id; of two results for one id the later one counts.
pub type Health = HashMap<String, AgentHealth>;

pub fn health_by_id(results: Vec<AgentHealth>) -> Health {
    results.into_iter().map(|r| (r.id.clone(), r)).collect()
}

/// An agent without a probe result counts as failed.
pub fn status_of(health: &Health, id: AcpAgentId) -> AgentHealthStatus {
    health
        .get(id.as_str())
        .map_or(AgentHealthStatus::Failed, |h| h.status)
}

pub fn ready_count(health: &Health) -> usize {
    health
        .values()
        .filter(|h| h.status == AgentHealthStatus::Ready)
        .count()
}

fn status_key(status: AgentHealthStatus) -> &'static str {
    match status {
        AgentHealthStatus::Ready => "onboard.status.ready",
        AgentHealthStatus::NotInstalled => "onboard.status.not-installed",
        AgentHealthStatus::NeedsKey => "onboard.status.needs-key",
        AgentHealthStatus::NeedsLogin => "onboard.status.needs-login",
        AgentHealthStatus::AuthFailed => "onboard.status.auth-failed",
        AgentHealthStatus::Failed => "onboard.status.failed",
    }
}

/// `STATUS_COLOR`
fn status_color(status: AgentHealthStatus, tokens: &Tokens) -> Rgba {
    match status {
        AgentHealthStatus::Ready => tokens.status_ok,
        AgentHealthStatus::NotInstalled => tokens.theme.http_none,
        AgentHealthStatus::NeedsKey | AgentHealthStatus::NeedsLogin => tokens.theme.status_warn,
        AgentHealthStatus::AuthFailed | AgentHealthStatus::Failed => tokens.theme.status_error,
    }
}

/// The line next to an agent's name. The new-tab list only needs to say which agents
/// can be opened.
pub fn status_text(health: Option<&AgentHealth>, new_tab: bool, lang: Lang) -> String {
    let status = health.map_or(AgentHealthStatus::Failed, |h| h.status);
    if new_tab && status == AgentHealthStatus::Ready {
        return lang.t("chat.agentConnected").to_owned();
    }
    let label = if health.is_some_and(|h| h.auto) {
        lang.t("onboard.status.auto")
    } else {
        lang.t(status_key(status))
    };
    match health
        .and_then(|h| h.plan.as_deref())
        .filter(|plan| !plan.is_empty() && *plan != "api-key")
    {
        Some(plan) => format!(
            "{label} · {}",
            fill(lang.t("onboard.plan"), &[("plan", plan)])
        ),
        None => label.to_owned(),
    }
}

/// The command a not-yet-usable agent needs: (dictionary key of its caption, command).
pub fn setup_hint(
    status: AgentHealthStatus,
    def: &AcpAgentDef,
) -> Option<(&'static str, &'static str)> {
    match status {
        AgentHealthStatus::NotInstalled => Some(("onboard.installWith", def.install_hint)),
        AgentHealthStatus::NeedsLogin => def
            .login_hint
            .filter(|hint| !hint.is_empty())
            .map(|hint| ("onboard.signInWith", hint)),
        _ => None,
    }
}

/// One agent's block of the list.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub def: &'static AcpAgentDef,
    pub models: Vec<LiveModel>,
}

/// Group the catalog by agent, preserving catalog order, and drop rows the search query
/// doesn't match (by model name or agent name).
pub fn group_models(catalog: &[LiveModel], query: &str, health: &Health) -> Vec<Group> {
    let q = js_trim(query).to_lowercase();
    let mut groups: Vec<Group> = ACP_AGENTS
        .iter()
        .filter_map(|def| {
            let models: Vec<LiveModel> = catalog
                .iter()
                .filter(|m| m.agent_id == def.id)
                .filter(|m| {
                    q.is_empty()
                        || m.name.to_lowercase().contains(&q)
                        || def.name.to_lowercase().contains(&q)
                })
                .cloned()
                .collect();
            (!models.is_empty()).then_some(Group { def, models })
        })
        .collect();
    // Usable agents first; ones still needing a key or install sink to the bottom (stable within each).
    groups.sort_by_key(|group| status_of(health, group.def.id) != AgentHealthStatus::Ready);
    groups
}

/// Where the popover goes, in window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub top: f32,
    pub left: f32,
    pub width: f32,
    pub max_height: f32,
}

/// Fixed placement inside the chat column: an absolute popover got clipped by its
/// overflow. The popover's left edge lines up with the button's.
pub fn popover_placement(trigger: Rect, column: Option<Span>, viewport: (f32, f32)) -> Placement {
    let col = column.unwrap_or(Span {
        left: 0.0,
        right: viewport.0,
    });
    let width = (col.right - col.left - 16.0).min(320.0);
    Placement {
        top: trigger.bottom + 6.0,
        left: (col.left + 8.0).max(trigger.left.min(col.right - width - 8.0)),
        width,
        max_height: viewport.1 - trigger.bottom - 18.0,
    }
}

/// `window.rev` and the model catalog store, as far as the picker uses them.
pub trait ModelPickerHost {
    /// `acp.probe()`: which agents can run on this machine.
    fn probe(&self, cx: &mut App) -> Task<Vec<AgentHealth>>;
    /// `settings.setApiKey(provider, key)`. An error leaves the typed key in the field.
    fn set_api_key(
        &self,
        provider: KeyProvider,
        key: String,
        cx: &mut App,
    ) -> Task<Result<bool, String>>;
    /// `useAllAgentModels()`: every agent's models.
    fn models(&self, cx: &App) -> Vec<LiveModel>;
}

/// What the React component takes as props, minus the callbacks.
#[derive(Clone, Debug)]
pub struct ModelPickerProps {
    pub agent_id: AcpAgentId,
    /// Display name of the active model, or `None` to fall back to the agent name.
    pub model_name: Option<String>,
    pub disabled: bool,
    /// Render as the chat tabs' "+" button: lists agents only; the model is picked inside the session.
    pub new_tab: bool,
    /// The chat column the popover has to stay inside; the whole window when `None`.
    pub column: Option<Span>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelPickerEvent {
    /// A model of a ready agent was chosen.
    Selected {
        agent_id: AcpAgentId,
        resolved_path: String,
        model_id: String,
        name: String,
    },
    /// newTab mode: a ready agent was clicked.
    PickedAgent(AcpAgentId),
}

pub struct ModelPicker {
    /// Read on every render, as re-rendered props are.
    props: Box<dyn Fn(&App) -> ModelPickerProps>,
    host: Rc<dyn ModelPickerHost>,
    look: LookFn,
    open: bool,
    query: Entity<TextInput>,
    health: Health,
    loading: bool,
    key_inputs: Vec<(KeyProvider, Entity<TextInput>)>,
    saving_key: Option<KeyProvider>,
    copied: Option<String>,
    trigger: BoundsCell,
}

impl EventEmitter<ModelPickerEvent> for ModelPicker {}

impl ModelPicker {
    pub fn new(
        props: impl Fn(&App) -> ModelPickerProps + 'static,
        host: Rc<dyn ModelPickerHost>,
        look: LookFn,
        cx: &mut Context<Self>,
    ) -> Self {
        // The placeholders are drawn by `field`, in the language of each render.
        let query = cx.new(|cx| TextInput::new("", cx));
        cx.observe(&query, |_, _, cx| cx.notify()).detach();
        let key_inputs = KeyProvider::ALL
            .into_iter()
            .map(|provider| {
                let input = cx.new(|cx| TextInput::new("", cx));
                cx.observe(&input, |_, _, cx| cx.notify()).detach();
                cx.subscribe(&input, move |this, _, _: &Submitted, cx| {
                    this.save_key(provider, cx);
                })
                .detach();
                (provider, input)
            })
            .collect();
        let mut picker = Self {
            props: Box::new(props),
            host,
            look,
            open: false,
            query,
            health: Health::new(),
            loading: true,
            key_inputs,
            saving_key: None,
            copied: None,
            trigger: BoundsCell::default(),
        };
        picker.scan(cx);
        picker
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opening puts the cursor in the search field (`autoFocus`), where there is one.
    pub fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if open && !self.open && !(self.props)(cx).new_tab {
            window.focus(&self.query.focus_handle(cx));
        }
        self.open = open;
        cx.notify();
    }

    pub fn health(&self) -> &Health {
        &self.health
    }

    /// Ask the host which agents are usable; the list shows "checking" meanwhile.
    pub fn scan(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        cx.notify();
        let probe = self.host.probe(cx);
        cx.spawn(async move |this, cx| {
            let results = probe.await;
            let _ = this.update(cx, |this, cx| this.probed(results, cx));
        })
        .detach();
    }

    fn probed(&mut self, results: Vec<AgentHealth>, cx: &mut Context<Self>) {
        self.health = health_by_id(results);
        self.loading = false;
        cx.notify();
    }

    fn key_input(&self, provider: KeyProvider) -> Option<&Entity<TextInput>> {
        self.key_inputs
            .iter()
            .find(|(known, _)| *known == provider)
            .map(|(_, input)| input)
    }

    /// Store the typed key, then probe again: the provider should now be ready.
    fn save_key(&mut self, provider: KeyProvider, cx: &mut Context<Self>) {
        let Some(input) = self.key_input(provider).cloned() else {
            return;
        };
        let key = js_trim(input.read(cx).text()).to_owned();
        if key.is_empty() {
            return;
        }
        self.saving_key = Some(provider);
        cx.notify();
        let saving = self.host.set_api_key(provider, key, cx);
        cx.spawn(async move |this, cx| {
            if saving.await.is_ok() {
                let probe = this.update(cx, |this, cx| {
                    input.update(cx, |input, cx| input.set_text("", cx));
                    this.loading = true;
                    cx.notify();
                    this.host.probe(cx)
                });
                if let Ok(probe) = probe {
                    let results = probe.await;
                    let _ = this.update(cx, |this, cx| this.probed(results, cx));
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.saving_key = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn copy(&mut self, text: &'static str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        self.copied = Some(text.to_owned());
        cx.notify();
        // Not cancelled by a later copy, as the component's timeout is not: the label
        // of a second copy goes back early when the first one's time is up.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.copied = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn choose(&mut self, def: &AcpAgentDef, model: &LiveModel, cx: &mut Context<Self>) {
        if status_of(&self.health, def.id) != AgentHealthStatus::Ready {
            return;
        }
        cx.emit(ModelPickerEvent::Selected {
            agent_id: def.id,
            resolved_path: resolved_path_for(def, self.health.get(def.id.as_str())),
            model_id: model.model_id.clone(),
            name: model.name.clone(),
        });
        self.open = false;
        cx.notify();
    }
}

impl Render for ModelPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let lang = look.lang;
        let props = (self.props)(cx);
        let new_tab = props.new_tab;
        let open = self.open;
        let hover = theme.chrome_hover;

        let toggle = cx.listener(|this, _: &gpui::ClickEvent, window, cx| {
            this.set_open(!this.open, window, cx);
        });
        let trigger = if new_tab {
            // `.chrome-icon-btn.small`
            div()
                .id("chat-tab-new")
                .debug_selector(|| "chat-tab-new".into())
                .relative()
                .size(px(28.0))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded_full()
                .when(open, |button| button.bg(hover))
                .when(props.disabled, |button| button.opacity(0.42))
                .when(!props.disabled, |button| {
                    button
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(toggle)
                })
                .tooltip(tooltip(lang.t("chat.newTab"), &tokens))
                .child(
                    svg()
                        .path("icons/plus.svg")
                        .size(px(14.0))
                        .text_color(theme.text_2),
                )
        } else {
            let selected = ACP_AGENTS
                .iter()
                .find(|agent| agent.id == props.agent_id)
                .or(ACP_AGENTS.first());
            let label = props
                .model_name
                .clone()
                .or_else(|| selected.map(|agent| agent.name.to_owned()))
                .unwrap_or_default();
            let status = selected.map_or(AgentHealthStatus::Failed, |agent| {
                status_of(&self.health, agent.id)
            });
            let label = single_line(&label);
            let label_width = natural_width(&label, 12.0, FontWeight::MEDIUM, window);
            // `triggerStyle`: a `button`, so its lines are 16px tall.
            div()
                .id("model-picker-trigger")
                .debug_selector(|| "model-picker-trigger".into())
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.0))
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .bg(theme.bg_bar)
                .border_1()
                .border_color(theme.border_2)
                .text_size(px(12.0))
                .line_height(px(16.0))
                .text_color(theme.text)
                .when(props.disabled, |button| button.opacity(0.42))
                .when(!props.disabled, |button| {
                    button.cursor_pointer().on_click(toggle)
                })
                .tooltip(tooltip(lang.t("chat.chooseAgent"), &tokens))
                .child(
                    // `iconChip`
                    div()
                        .size(px(18.0))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.0))
                        .bg(theme.surface_3)
                        .children(selected.map(|agent| {
                            provider_icon(agent.id.as_str(), 14.0, Some(agent.icon), theme.text)
                        })),
                )
                .child(
                    ellipsis(label, 16.0)
                        .w(px(label_width.min(130.0)))
                        .flex_none()
                        .font_weight(FontWeight::MEDIUM),
                )
                .child(
                    // `statusDot`
                    div()
                        .size(px(6.0))
                        .flex_none()
                        .rounded_full()
                        .bg(status_color(status, &tokens)),
                )
                .child(caret(10.0, 0.6))
        };

        let wrap = div()
            .relative()
            .flex()
            .flex_none()
            .child(track_bounds(&self.trigger))
            .child(trigger);
        if !open {
            return wrap;
        }
        // The button is painted before a click can open the list; only a list opened
        // from code on the very first frame has to wait for the button's place.
        let Some(trigger) = self.trigger.get().map(Rect::from) else {
            window.request_animation_frame();
            return wrap;
        };
        let viewport = window.viewport_size();
        let place = popover_placement(
            trigger,
            props.column,
            (viewport.width.into(), viewport.height.into()),
        );

        let query = self.query.read(cx).text().to_owned();
        let groups = group_models(&self.host.models(cx), &query, &self.health);
        let no_groups = groups.is_empty();

        let blocks = groups.into_iter().map(|Group { def, models }| {
            let health = self.health.get(def.id.as_str());
            let status = health.map_or(AgentHealthStatus::Failed, |h| h.status);
            let ready = status == AgentHealthStatus::Ready;
            let id = def.id.as_str();

            // `groupHeader`'s three children.
            let heading = |row: gpui::Stateful<gpui::Div>| {
                row.flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        // `groupIcon`
                        div()
                            .size(px(16.0))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.0))
                            .bg(theme.surface_3)
                            .child(provider_icon(id, 12.0, Some(def.icon), theme.text)),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(def.name),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(status_color(status, &tokens))
                            // `.new-tab-agent > span:last-child`: status pushed right.
                            // Not with an auto margin: the layout engine then drops the
                            // row's gaps.
                            .when(new_tab && ready, |status| {
                                status.flex_1().flex().justify_end()
                            })
                            .child(status_text(health, new_tab, lang)),
                    )
            };
            let header = if new_tab && ready {
                // `modelRow` + `groupHeader`, as a `button`.
                heading(div().id(("new-tab-agent", def.id as usize)))
                    .debug_selector(move || format!("new-tab-agent-{id}"))
                    .w_full()
                    .p(px(8.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .line_height(px(16.0))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.emit(ModelPickerEvent::PickedAgent(def.id));
                        this.open = false;
                        cx.notify();
                    }))
            } else {
                heading(div().id(("model-group", def.id as usize)))
                    .px(px(2.0))
                    .pb(px(6.0))
                    .line_height(px(13.0))
            };

            let body = if ready {
                (!new_tab).then(|| {
                    let rows = models.into_iter().enumerate().map(|(index, m)| {
                        let active = props.agent_id == def.id
                            && props.model_name.as_deref() == Some(m.name.as_str());
                        let name = m.name.clone();
                        // `modelRow` / `modelRowActive`: a `button`.
                        div()
                            .id((id, index))
                            .debug_selector(move || format!("model-row-{id}-{index}"))
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px(px(10.0))
                            .py(px(7.0))
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .text_size(px(12.0))
                            .line_height(px(16.0))
                            .cursor_pointer()
                            .when(active, |row| {
                                row.bg(tokens.accent_soft)
                                    .border_color(tokens.accent_border)
                                    .text_color(tokens.accent_text)
                                    .font_weight(FontWeight::SEMIBOLD)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| this.choose(def, &m, cx)))
                            .child(name)
                            .when(active, |row| row.child("✓"))
                    });
                    div().flex().flex_col().gap(px(2.0)).children(rows)
                })
            } else {
                let hint = setup_hint(status, def).map(|(caption, command)| {
                    let copied = self.copied.as_deref() == Some(command);
                    // `hintRow`
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(text(10.0).text_color(theme.text_2).child(lang.t(caption)))
                        .child(
                            // `codeChip`
                            div()
                                .id(("model-hint-code", def.id as usize))
                                .flex_1()
                                .min_w_0()
                                .overflow_x_scroll()
                                .px(px(6.0))
                                .py(px(3.0))
                                .rounded(px(4.0))
                                .bg(theme.surface_2)
                                .border_1()
                                .border_color(theme.border_2)
                                .font_family(MONO)
                                .text_size(px(10.0))
                                .line_height(px(12.0))
                                .whitespace_nowrap()
                                .child(command),
                        )
                        .child(
                            // `linkBtn`
                            div()
                                .id(("model-hint-copy", def.id as usize))
                                .debug_selector(move || format!("model-hint-copy-{id}"))
                                .flex_none()
                                .text_size(px(11.0))
                                .line_height(px(16.0))
                                .text_color(theme.text_2)
                                .underline()
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| this.copy(command, cx)))
                                .child(lang.t(if copied {
                                    "onboard.copied"
                                } else {
                                    "onboard.copy"
                                })),
                        )
                });
                let key_row = key_provider(def).and_then(|provider| {
                    let input = self.key_input(provider)?;
                    let saving = self.saving_key == Some(provider);
                    let blocked = saving || js_trim(input.read(cx).text()).is_empty();
                    Some(
                        // `hintRow`
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(
                                // `keyInputStyle`: 27px tall in the browser, the field is 20.
                                div()
                                    .debug_selector(move || format!("model-key-{id}"))
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .px(px(8.0))
                                    .py(px(2.5))
                                    .rounded(px(6.0))
                                    .bg(theme.surface_2)
                                    .border_1()
                                    .border_color(theme.border_2)
                                    .text_size(px(12.0))
                                    .child(field(
                                        input,
                                        lang.t("onboard.keyPlaceholder"),
                                        true,
                                        theme.text,
                                        cx,
                                    )),
                            )
                            .child(
                                // `saveBtn`
                                div()
                                    .id(("model-key-save", def.id as usize))
                                    .debug_selector(move || format!("model-key-save-{id}"))
                                    .flex_none()
                                    .px(px(12.0))
                                    .py(px(5.0))
                                    .rounded(px(6.0))
                                    .bg(theme.surface_3)
                                    .border_1()
                                    .border_color(theme.border_2)
                                    .text_size(px(12.0))
                                    .line_height(px(16.0))
                                    .when(blocked, |button| button.opacity(0.42))
                                    .when(!blocked, |button| {
                                        button.cursor_pointer().on_click(cx.listener(
                                            move |this, _, _, cx| this.save_key(provider, cx),
                                        ))
                                    })
                                    .child(if saving {
                                        "…"
                                    } else {
                                        lang.t("onboard.save")
                                    }),
                            ),
                    )
                });
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .px(px(4.0))
                        .py(px(2.0))
                        .children(hint)
                        .children(key_row),
                )
            };

            div().child(header).children(body)
        });
        let blocks: Vec<_> = blocks.collect();

        // `popoverStyle` + `fixedPos`
        let popover = div()
            .id("model-picker-popover")
            .debug_selector(|| "model-picker-popover".into())
            .w(px(place.width))
            .max_h(px(place.max_height.max(0.0)))
            .overflow_y_scroll()
            .p(px(10.0))
            .rounded(px(8.0))
            .bg(theme.bg_bar)
            .border_1()
            .border_color(theme.border)
            .shadow(vec![shadow(8.0, 24.0, rgba(0x00000080))])
            .text_color(theme.text)
            .occlude()
            // A press anywhere else closes the list and still reaches what is under it,
            // as with the component's document listener. A press on the button is left
            // to the button, which toggles.
            .on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                if !trigger.contains(event.position) {
                    this.open = false;
                    cx.notify();
                }
            }))
            .when(!new_tab, |popover| {
                popover.child(
                    // `searchStyle`: 29px tall in the browser, the field is 20.
                    div()
                        .flex()
                        .px(px(8.0))
                        .py(px(3.5))
                        .rounded(px(6.0))
                        .bg(theme.surface_2)
                        .border_1()
                        .border_color(theme.border_2)
                        .text_size(px(12.0))
                        .child(field(
                            &self.query,
                            lang.t("model.search"),
                            false,
                            theme.text,
                            cx,
                        )),
                )
            })
            .child(
                // `popoverHeader`
                div()
                    .flex()
                    .justify_end()
                    .pt(px(6.0))
                    .px(px(2.0))
                    .pb(px(8.0))
                    .child(text(11.0).opacity(0.6).child(if self.loading {
                        lang.t("onboard.scanning").to_owned()
                    } else {
                        fill(
                            lang.t("onboard.ready"),
                            &[("n", &ready_count(&self.health).to_string())],
                        )
                    })),
            )
            .child(div().flex().flex_col().gap(px(12.0)).children(blocks).when(
                no_groups,
                |list| {
                    list.child(
                        text(11.0)
                            .px(px(4.0))
                            .py(px(8.0))
                            .text_color(theme.text_2)
                            .child(lang.t("model.noMatch")),
                    )
                },
            ));

        wrap.child(
            deferred(
                anchored()
                    .snap_to_window()
                    .position(point(px(place.left), px(place.top)))
                    .child(popover),
            )
            .with_priority(Z_POPOVER),
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn def(id: AcpAgentId) -> &'static AcpAgentDef {
        ACP_AGENTS.iter().find(|agent| agent.id == id).unwrap()
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

    fn catalog() -> Vec<LiveModel> {
        crate::model_catalog::ModelCatalogStore::new().all_agent_models()
    }

    fn ids(groups: &[Group]) -> Vec<&'static str> {
        groups.iter().map(|group| group.def.id.as_str()).collect()
    }

    #[test]
    fn 상태는_케밥_표기로_직렬화된다() {
        let parsed: AgentHealth = serde_json::from_value(json!({
            "id": "claude-code", "status": "needs-login", "resolvedPath": "/usr/bin/claude",
            "detail": null, "auto": false, "plan": "max"
        }))
        .unwrap();
        assert_eq!(parsed.status, AgentHealthStatus::NeedsLogin);
        assert_eq!(parsed.resolved_path.as_deref(), Some("/usr/bin/claude"));
        assert_eq!(
            json!(AgentHealthStatus::NotInstalled),
            json!("not-installed")
        );
        assert_eq!(json!(AgentHealthStatus::AuthFailed), json!("auth-failed"));
        assert_eq!(json!(KeyProvider::Openai), json!("openai"));
        assert_eq!(
            json!(health("codex", AgentHealthStatus::Ready)),
            json!({ "id": "codex", "status": "ready", "resolvedPath": null,
                    "detail": null, "auto": false, "plan": null })
        );
    }

    #[test]
    fn api_키로_도는_에이전트만_키_제공자다() {
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
        assert_eq!(key_provider(def(AcpAgentId::ClaudeCode)), None);
        assert_eq!(key_provider(def(AcpAgentId::Codex)), None);
    }

    #[test]
    fn 실행_경로는_키_제공자면_api고_아니면_찾은_경로나_명령_이름이다() {
        let found = AgentHealth {
            resolved_path: Some("/opt/bin/claude".into()),
            ..health("claude-code", AgentHealthStatus::Ready)
        };
        assert_eq!(
            resolved_path_for(def(AcpAgentId::Anthropic), Some(&found)),
            "api"
        );
        assert_eq!(
            resolved_path_for(def(AcpAgentId::ClaudeCode), Some(&found)),
            "/opt/bin/claude"
        );
        assert_eq!(
            resolved_path_for(
                def(AcpAgentId::ClaudeCode),
                Some(&health("claude-code", AgentHealthStatus::Ready))
            ),
            "claude"
        );
        assert_eq!(resolved_path_for(def(AcpAgentId::Codex), None), "codex-acp");
    }

    #[test]
    fn 검사_결과가_없는_에이전트는_실패로_친다() {
        let by_id = health_by_id(vec![health("codex", AgentHealthStatus::Ready)]);
        assert_eq!(
            status_of(&by_id, AcpAgentId::Codex),
            AgentHealthStatus::Ready
        );
        assert_eq!(
            status_of(&by_id, AcpAgentId::ClaudeCode),
            AgentHealthStatus::Failed
        );
    }

    #[test]
    fn 같은_id의_결과가_둘이면_뒤의_것을_쓰고_준비된_수는_모든_결과에서_센다() {
        let by_id = health_by_id(vec![
            health("codex", AgentHealthStatus::Failed),
            health("codex", AgentHealthStatus::Ready),
            health("claude-code", AgentHealthStatus::NeedsLogin),
            // An id the picker has no row for still counts.
            health("gemini", AgentHealthStatus::Ready),
        ]);
        assert_eq!(
            status_of(&by_id, AcpAgentId::Codex),
            AgentHealthStatus::Ready
        );
        assert_eq!(ready_count(&by_id), 2);
        assert_eq!(ready_count(&Health::new()), 0);
    }

    #[test]
    fn 검색어가_없으면_에이전트_표의_순서로_묶고_준비된_것을_위로_올린다() {
        let none_ready = group_models(&catalog(), "", &Health::new());
        assert_eq!(
            ids(&none_ready),
            ["anthropic", "openai", "grok", "glm", "claude-code", "codex"]
        );
        let by_id = health_by_id(vec![
            health("codex", AgentHealthStatus::Ready),
            health("grok", AgentHealthStatus::Ready),
            health("openai", AgentHealthStatus::NeedsKey),
        ]);
        let groups = group_models(&catalog(), "", &by_id);
        // Stable inside each half.
        assert_eq!(
            ids(&groups),
            ["grok", "codex", "anthropic", "openai", "glm", "claude-code"]
        );
        let codex = &groups[1];
        assert_eq!(
            codex
                .models
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            ["GPT-5 Codex", "GPT-5"]
        );
    }

    #[test]
    fn 검색어는_모델_이름과_에이전트_이름에서_대소문자_없이_찾는다() {
        let health = Health::new();
        // Model names.
        let opus = group_models(&catalog(), " OPUS ", &health);
        assert_eq!(ids(&opus), ["anthropic", "claude-code"]);
        assert_eq!(opus[0].models.len(), 1);
        assert_eq!(opus[0].models[0].name, "Claude Opus 4.8");
        // An agent name keeps all of that agent's models.
        let codex = group_models(&catalog(), "codex", &health);
        assert_eq!(ids(&codex), ["codex"]);
        assert_eq!(codex[0].models.len(), 2);
        // "(api)" is in four agent names.
        assert_eq!(
            ids(&group_models(&catalog(), "(api)", &health)),
            ["anthropic", "openai", "grok", "glm"]
        );
        assert!(group_models(&catalog(), "gemini", &health).is_empty());
    }

    #[test]
    fn 섞여_온_목록도_에이전트별로_묶고_그_안의_순서는_목록의_순서다() {
        let row = |agent_id, name: &str| LiveModel {
            agent_id,
            model_id: name.to_lowercase(),
            name: name.to_owned(),
            description: None,
            effort_levels: None,
            supports_fast_mode: None,
        };
        let mixed = [
            row(AcpAgentId::Codex, "B"),
            row(AcpAgentId::Anthropic, "X"),
            row(AcpAgentId::Codex, "A"),
        ];
        let groups = group_models(&mixed, "", &Health::new());
        assert_eq!(ids(&groups), ["anthropic", "codex"]);
        let names = |group: &Group| -> Vec<String> {
            group.models.iter().map(|m| m.name.clone()).collect()
        };
        assert_eq!(names(&groups[1]), ["B", "A"]);
        // A query that only one row of an agent matches leaves that row alone.
        let only_a = group_models(&mixed, "\u{feff}a ", &Health::new());
        assert_eq!(ids(&only_a), ["anthropic", "codex"]);
        assert_eq!(names(&only_a[0]), ["X"]);
        assert_eq!(names(&only_a[1]), ["A"]);
    }

    #[test]
    fn 상태_문구는_자동_연결과_요금제를_덧붙인다() {
        let en = Lang::En;
        assert_eq!(status_text(None, false, en), "Failed to start");
        let ready = health("claude-code", AgentHealthStatus::Ready);
        assert_eq!(status_text(Some(&ready), false, en), "Ready");
        let auto = AgentHealth {
            auto: true,
            plan: Some("max".into()),
            ..ready.clone()
        };
        assert_eq!(
            status_text(Some(&auto), false, en),
            "Connected automatically · max plan"
        );
        // An API key is not a plan worth naming.
        let keyed = AgentHealth {
            plan: Some("api-key".into()),
            ..ready.clone()
        };
        assert_eq!(status_text(Some(&keyed), false, en), "Ready");
        let blank = AgentHealth {
            plan: Some(String::new()),
            ..ready.clone()
        };
        assert_eq!(status_text(Some(&blank), false, en), "Ready");
        assert_eq!(
            status_text(
                Some(&health("codex", AgentHealthStatus::NeedsLogin)),
                false,
                Lang::Ko
            ),
            Lang::Ko.t("onboard.status.needs-login")
        );
    }

    #[test]
    fn 새_탭_목록에서_준비된_에이전트는_연결됨이라고만_한다() {
        let auto = AgentHealth {
            auto: true,
            plan: Some("max".into()),
            ..health("claude-code", AgentHealthStatus::Ready)
        };
        assert_eq!(status_text(Some(&auto), true, Lang::En), "Connected");
        // One that is not ready reads as in the full list.
        let missing = AgentHealth {
            plan: Some("pro".into()),
            ..health("codex", AgentHealthStatus::NotInstalled)
        };
        assert_eq!(
            status_text(Some(&missing), true, Lang::En),
            "Not installed · pro plan"
        );
    }

    #[test]
    fn 모든_상태에_제_문구가_있다() {
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
    fn 설치가_안_됐으면_설치_명령을_로그인이_필요하면_로그인_명령을_알려준다() {
        let claude = def(AcpAgentId::ClaudeCode);
        assert_eq!(
            setup_hint(AgentHealthStatus::NotInstalled, claude),
            Some(("onboard.installWith", "npm i -g @anthropic-ai/claude-code"))
        );
        assert_eq!(
            setup_hint(AgentHealthStatus::NeedsLogin, claude),
            Some(("onboard.signInWith", "claude login"))
        );
        // No sign-in command to show for this one.
        assert_eq!(
            setup_hint(AgentHealthStatus::NeedsLogin, def(AcpAgentId::Anthropic)),
            None
        );
        for status in [
            AgentHealthStatus::Ready,
            AgentHealthStatus::NeedsKey,
            AgentHealthStatus::AuthFailed,
            AgentHealthStatus::Failed,
        ] {
            assert_eq!(setup_hint(status, claude), None);
        }
    }

    fn trigger(left: f32, right: f32) -> Rect {
        Rect {
            left,
            top: 10.0,
            right,
            bottom: 38.0,
        }
    }

    #[test]
    fn 팝오버는_버튼_아래_6픽셀에_왼쪽_끝을_맞추고_창_아래까지만_자란다() {
        assert_eq!(
            popover_placement(trigger(100.0, 128.0), None, (1000.0, 640.0)),
            Placement {
                top: 44.0,
                left: 100.0,
                width: 320.0,
                max_height: 584.0
            }
        );
    }

    #[test]
    fn 팝오버는_칼럼_안에_8픽셀_여백을_두고_머문다() {
        let column = Some(Span {
            left: 600.0,
            right: 1000.0,
        });
        let viewport = (1000.0, 640.0);
        // The "+" at the end of the tab strip: the popover is pulled back into the column.
        assert_eq!(
            popover_placement(trigger(960.0, 988.0), column, viewport).left,
            672.0
        );
        assert_eq!(
            popover_placement(trigger(590.0, 618.0), column, viewport).left,
            608.0
        );
        assert_eq!(
            popover_placement(trigger(650.0, 678.0), column, viewport).left,
            650.0
        );
    }

    #[test]
    fn 좁은_칼럼에서는_팝오버가_칼럼_너비에서_16을_뺀_만큼이다() {
        let column = Some(Span {
            left: 700.0,
            right: 1000.0,
        });
        let place = popover_placement(trigger(900.0, 928.0), column, (1000.0, 640.0));
        assert_eq!(place.width, 284.0);
        assert_eq!(place.left, 708.0);
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::RefCell;

    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::ui::testing::{click, look, window};

    /// Stands in for `window.rev`: answers at once, and remembers the keys it was given.
    #[derive(Default)]
    struct FakeHost {
        health: RefCell<Vec<AgentHealth>>,
        keys: RefCell<Vec<(KeyProvider, String)>>,
        probes: RefCell<usize>,
        reject_keys: bool,
    }

    impl ModelPickerHost for FakeHost {
        fn probe(&self, _: &mut App) -> Task<Vec<AgentHealth>> {
            *self.probes.borrow_mut() += 1;
            Task::ready(self.health.borrow().clone())
        }

        fn set_api_key(
            &self,
            provider: KeyProvider,
            key: String,
            _: &mut App,
        ) -> Task<Result<bool, String>> {
            self.keys.borrow_mut().push((provider, key));
            if self.reject_keys {
                return Task::ready(Err("keychain locked".to_owned()));
            }
            // The key makes the provider usable.
            self.health.borrow_mut().push(health(
                provider.as_str(),
                AgentHealthStatus::Ready,
                None,
            ));
            Task::ready(Ok(true))
        }

        fn models(&self, _: &App) -> Vec<LiveModel> {
            crate::model_catalog::ModelCatalogStore::new().all_agent_models()
        }
    }

    fn health(id: &str, status: AgentHealthStatus, path: Option<&str>) -> AgentHealth {
        AgentHealth {
            id: id.to_owned(),
            status,
            resolved_path: path.map(str::to_owned),
            detail: None,
            auto: false,
            plan: None,
        }
    }

    struct Fixture {
        picker: Entity<ModelPicker>,
        host: Rc<FakeHost>,
        events: Rc<RefCell<Vec<ModelPickerEvent>>>,
    }

    fn picker(
        new_tab: bool,
        reject_keys: bool,
        cx: &mut TestAppContext,
    ) -> (Fixture, &mut VisualTestContext) {
        let host = Rc::new(FakeHost {
            health: RefCell::new(vec![
                health(
                    "claude-code",
                    AgentHealthStatus::Ready,
                    Some("/opt/bin/claude"),
                ),
                health("codex", AgentHealthStatus::NotInstalled, None),
                health("openai", AgentHealthStatus::NeedsKey, None),
            ]),
            reject_keys,
            ..FakeHost::default()
        });
        let events: Rc<RefCell<Vec<ModelPickerEvent>>> = Rc::default();
        let (seen, fake) = (events.clone(), host.clone());
        let (picker, cx) = window(cx, move |_, cx| {
            cx.subscribe(&cx.entity(), move |_, _, event: &ModelPickerEvent, _| {
                seen.borrow_mut().push(event.clone());
            })
            .detach();
            ModelPicker::new(
                move |_| ModelPickerProps {
                    agent_id: AcpAgentId::ClaudeCode,
                    model_name: Some("Opus".into()),
                    disabled: false,
                    new_tab,
                    column: None,
                },
                fake,
                look(),
                cx,
            )
        });
        cx.run_until_parked();
        (
            Fixture {
                picker,
                host,
                events,
            },
            cx,
        )
    }

    fn is_open(fixture: &Fixture, cx: &mut VisualTestContext) -> bool {
        fixture.picker.read_with(cx, |picker, _| picker.is_open())
    }

    /// Put text in the OpenAI key field, as typing or pasting would.
    fn type_key(fixture: &Fixture, key: &str, cx: &mut VisualTestContext) {
        fixture.picker.update(cx, |picker, cx| {
            let input = picker.key_input(KeyProvider::Openai).unwrap().clone();
            input.update(cx, |input, cx| input.set_text(key.to_owned(), cx));
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn 처음에_한_번_검사하고_그_결과를_갖는다(cx: &mut TestAppContext) {
        let (fixture, cx) = picker(false, false, cx);
        assert_eq!(*fixture.host.probes.borrow(), 1);
        fixture.picker.read_with(cx, |picker, _| {
            assert!(!picker.loading);
            assert_eq!(ready_count(picker.health()), 1);
        });
    }

    #[gpui::test]
    fn 버튼을_누르면_목록이_버튼_아래에_열린다(cx: &mut TestAppContext) {
        let (fixture, cx) = picker(false, false, cx);
        click("model-picker-trigger", cx);
        assert!(is_open(&fixture, cx));
        let trigger = cx.debug_bounds("model-picker-trigger").unwrap();
        let popover = cx.debug_bounds("model-picker-popover").unwrap();
        assert_eq!(popover.top(), trigger.bottom() + px(6.0));
        assert_eq!(popover.left(), px(8.0));
        assert_eq!(popover.size.width, px(320.0));
        click("model-picker-trigger", cx);
        assert!(!is_open(&fixture, cx));
    }

    #[gpui::test]
    fn 준비된_에이전트의_모델을_누르면_그_모델을_알리고_닫힌다(
        cx: &mut TestAppContext,
    ) {
        let (fixture, cx) = picker(false, false, cx);
        click("model-picker-trigger", cx);
        click("model-row-claude-code-2", cx);
        assert_eq!(
            *fixture.events.borrow(),
            [ModelPickerEvent::Selected {
                agent_id: AcpAgentId::ClaudeCode,
                resolved_path: "/opt/bin/claude".into(),
                model_id: "opus".into(),
                name: "Opus".into(),
            }]
        );
        assert!(!is_open(&fixture, cx));
    }

    #[gpui::test]
    fn 준비되지_않은_에이전트의_모델은_목록에_없다(cx: &mut TestAppContext) {
        let (_, cx) = picker(false, false, cx);
        click("model-picker-trigger", cx);
        assert!(cx.debug_bounds("model-row-claude-code-0").is_some());
        assert!(cx.debug_bounds("model-row-codex-0").is_none());
        assert!(cx.debug_bounds("model-row-openai-0").is_none());
        // What they show instead: the install command, and the key field.
        assert!(cx.debug_bounds("model-hint-copy-codex").is_some());
        assert!(cx.debug_bounds("model-key-openai").is_some());
    }

    #[gpui::test]
    fn 검색어를_치면_목록이_줄어든다(cx: &mut TestAppContext) {
        let (fixture, cx) = picker(false, false, cx);
        click("model-picker-trigger", cx);
        // The search field has the focus as soon as the list opens.
        cx.simulate_input("haiku");
        cx.run_until_parked();
        click("model-row-claude-code-0", cx);
        assert!(matches!(
            fixture.events.borrow().as_slice(),
            [ModelPickerEvent::Selected { model_id, .. }] if model_id == "haiku"
        ));
    }

    #[gpui::test]
    fn 복사를_누르면_명령이_클립보드에_가고_1초_반_동안_복사됨이라고_한다(
        cx: &mut TestAppContext,
    ) {
        let (fixture, cx) = picker(false, false, cx);
        click("model-picker-trigger", cx);
        click("model-hint-copy-codex", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("npm i -g @agentclientprotocol/codex-acp".to_owned())
        );
        let copied = |cx: &mut VisualTestContext| {
            fixture
                .picker
                .read_with(cx, |picker, _| picker.copied.clone())
        };
        assert_eq!(
            copied(cx).as_deref(),
            Some("npm i -g @agentclientprotocol/codex-acp")
        );
        cx.executor().advance_clock(Duration::from_millis(1400));
        cx.run_until_parked();
        assert!(copied(cx).is_some());
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        assert_eq!(copied(cx), None);
    }

    #[gpui::test]
    fn 키를_저장하면_다듬어_보내고_칸을_비우고_다시_검사한다(
        cx: &mut TestAppContext,
    ) {
        let (fixture, cx) = picker(false, false, cx);
        click("model-picker-trigger", cx);
        // Nothing typed: the button does nothing.
        click("model-key-save-openai", cx);
        assert!(fixture.host.keys.borrow().is_empty());
        type_key(&fixture, "  sk-test-123 ", cx);
        click("model-key-save-openai", cx);
        assert_eq!(
            *fixture.host.keys.borrow(),
            [(KeyProvider::Openai, "sk-test-123".to_owned())]
        );
        assert_eq!(*fixture.host.probes.borrow(), 2);
        fixture.picker.read_with(cx, |picker, cx| {
            assert_eq!(picker.saving_key, None);
            let input = picker.key_input(KeyProvider::Openai).unwrap();
            assert_eq!(input.read(cx).text(), "");
            assert_eq!(
                status_of(picker.health(), AcpAgentId::Openai),
                AgentHealthStatus::Ready
            );
        });
    }

    #[gpui::test]
    fn 키_칸에서_엔터를_눌러도_저장한다(cx: &mut TestAppContext) {
        let (fixture, cx) = picker(false, false, cx);
        cx.update(|_, cx| rever_ui_kit::text_input::bind_keys(cx));
        click("model-picker-trigger", cx);
        type_key(&fixture, "sk-enter", cx);
        // A click puts the cursor in the field.
        click("model-key-openai", cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            *fixture.host.keys.borrow(),
            [(KeyProvider::Openai, "sk-enter".to_owned())]
        );
    }

    #[gpui::test]
    fn 저장이_실패하면_친_키를_그대로_두고_다시_검사하지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (fixture, cx) = picker(false, true, cx);
        click("model-picker-trigger", cx);
        type_key(&fixture, "sk-bad", cx);
        click("model-key-save-openai", cx);
        assert_eq!(fixture.host.keys.borrow().len(), 1);
        assert_eq!(*fixture.host.probes.borrow(), 1);
        fixture.picker.read_with(cx, |picker, cx| {
            assert_eq!(picker.saving_key, None);
            let input = picker.key_input(KeyProvider::Openai).unwrap();
            assert_eq!(input.read(cx).text(), "sk-bad");
        });
    }

    #[gpui::test]
    fn 새_탭_목록에서는_준비된_에이전트를_누르면_그_에이전트를_알린다(
        cx: &mut TestAppContext,
    ) {
        let (fixture, cx) = picker(true, false, cx);
        click("chat-tab-new", cx);
        // No model rows in this mode.
        assert!(cx.debug_bounds("model-row-claude-code-0").is_none());
        click("new-tab-agent-claude-code", cx);
        assert_eq!(
            *fixture.events.borrow(),
            [ModelPickerEvent::PickedAgent(AcpAgentId::ClaudeCode)]
        );
        assert!(!is_open(&fixture, cx));
    }

    #[gpui::test]
    fn 바깥을_누르면_닫힌다(cx: &mut TestAppContext) {
        let (fixture, cx) = picker(false, false, cx);
        click("model-picker-trigger", cx);
        cx.simulate_click(point(px(900.0), px(700.0)), Modifiers::none());
        cx.run_until_parked();
        assert!(!is_open(&fixture, cx));
        assert!(fixture.events.borrow().is_empty());
    }
}
