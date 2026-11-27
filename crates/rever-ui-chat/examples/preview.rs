//! Opens this crate's views alone with fixture data. See rever_ui_kit::preview.
//!
//! Three chat columns, each with one list open. Left: the session tabs (a busy tab with
//! a long title, a second agent's tab, an untitled one) and the "+" list of agents.
//! Middle: the model picker with its full list — ready agents with their models, the
//! others with an install command, a sign-in command or a key field. Right: the history
//! menu over saved conversations, and at the bottom a composer with the model chip's
//! menu open upward.
//!
//! The permission prompt covers the window, so it comes up after three seconds
//! (`REVER_PREVIEW_PROMPT=1` shows it from the start, `=0` never): two requests wait in
//! a real permission queue, the first for a risky tool. The line at the top names the
//! last thing a view asked the host to do, and the tool profile the profile store holds.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{AnyView, App, Context, Entity, Task, Window, div, prelude::*, px};
use rever_chat_core::acp_permission::{ApproveOptions, PermissionStore};
use rever_chat_core::red_team_mode::RedTeamMode;
use rever_tool_profile::tool_profile_line::ToolProfileId;
use rever_ui_chat::chat_history_menu::{
    ChatHistoryEvent, ChatHistoryMenu, ChatHistoryMenuProps, HistoryRow,
};
use rever_ui_chat::chat_tabs::{ChatTabMeta, ChatTabs, ChatTabsEvent, PanelSpec};
use rever_ui_chat::model_catalog::{LiveModel, ModelCatalog, ModelCatalogStore, ModelsHost};
use rever_ui_chat::model_menu::{MenuModel, ModelMenu, ModelMenuEvent, ModelMenuProps};
use rever_ui_chat::model_picker::{
    AgentHealth, AgentHealthStatus, KeyProvider, ModelPicker, ModelPickerEvent, ModelPickerHost,
    ModelPickerProps,
};
use rever_ui_chat::permission_prompt::{
    PermissionPrompt, PermissionPromptEvent, PermissionPromptProps, ToolRules,
};
use rever_ui_chat::provider_icon::provider_icon;
use rever_ui_chat::tool_profile::{ToolProfileHost, ToolProfileStore};
use rever_ui_chat::ui::Span;
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};
use rever_ui_menus::ui::{Look, LookFn};
use rever_ui_state::chat_history::ChatHistory;
use rever_ui_state::constants::{ACP_AGENTS, AcpAgentId};
use rever_ui_state::{MemoryStorage, Storage};
use serde_json::{Value, json};

const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 860.0;
const BAR: f32 = 30.0;
const COLUMN: f32 = 400.0;
const GAP: f32 = 20.0;

fn column(index: usize) -> Span {
    let left = GAP + index as f32 * (COLUMN + GAP);
    Span {
        left,
        right: left + COLUMN,
    }
}

const MINUTE: u64 = 60_000;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

/// (id, agent, age in milliseconds, first user message), oldest first.
const CONVERSATIONS: &[(&str, AcpAgentId, u64, &str)] = &[
    (
        "c-old",
        AcpAgentId::Grok,
        16 * DAY,
        "Which requests carry the CSRF token?",
    ),
    (
        "c-week",
        AcpAgentId::Openai,
        DAY + 2 * HOUR,
        "List every GraphQL operation the dashboard sends and group them by the screen that triggers them",
    ),
    (
        "c-hours",
        AcpAgentId::Codex,
        3 * HOUR,
        "x-signature 헤더가 어떻게 만들어지는지 번들에서 찾아줘",
    ),
    (
        "c-now",
        AcpAgentId::ClaudeCode,
        2 * MINUTE,
        "Reverse the login flow of shop.example",
    ),
];

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// Stands in for `window.rev`: the agent probe, the key store, the model lists and the
/// tool profile.
struct FakeMain {
    health: RefCell<Vec<AgentHealth>>,
    catalog: RefCell<ModelCatalogStore>,
    asked: RefCell<Vec<AcpAgentId>>,
}

fn health(
    id: AcpAgentId,
    status: AgentHealthStatus,
    auto: bool,
    plan: Option<&str>,
) -> AgentHealth {
    AgentHealth {
        id: id.as_str().to_owned(),
        status,
        resolved_path: (id == AcpAgentId::ClaudeCode).then(|| "/opt/homebrew/bin/claude".into()),
        detail: None,
        auto,
        plan: plan.map(str::to_owned),
    }
}

impl ModelPickerHost for FakeMain {
    fn probe(&self, _: &mut App) -> Task<Vec<AgentHealth>> {
        Task::ready(self.health.borrow().clone())
    }

    fn set_api_key(
        &self,
        provider: KeyProvider,
        _key: String,
        cx: &mut App,
    ) -> Task<Result<bool, String>> {
        // Any key is accepted, after a moment in which the button reads "…".
        if let Some(agent) = ACP_AGENTS
            .iter()
            .find(|a| a.id.as_str() == provider.as_str())
        {
            self.health.borrow_mut().push(health(
                agent.id,
                AgentHealthStatus::Ready,
                false,
                Some("api-key"),
            ));
        }
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            Ok(true)
        })
    }

    fn models(&self, _: &App) -> Vec<LiveModel> {
        self.catalog.borrow().all_agent_models()
    }
}

impl ModelsHost for FakeMain {
    fn request_catalog(&self, agent_id: AcpAgentId) {
        self.asked.borrow_mut().push(agent_id);
    }

    fn subscribe_updates(&self) {}
}

impl ToolProfileHost for FakeMain {
    fn get(&self) -> Value {
        json!("reverse")
    }

    fn set(&self, _: ToolProfileId) -> Result<(), String> {
        Err("the preview has no main process".to_owned())
    }
}

/// What Claude Code's own CLI reports: descriptions, reasoning levels, fast mode.
fn claude_catalog() -> Option<ModelCatalog> {
    serde_json::from_value(json!({
        "fetchedAt": "2026-11-13T09:00:00.000Z",
        "models": [
            { "modelId": "default", "name": "Default", "description": "Follows the model set in your Claude Code settings" },
            { "modelId": "opus", "name": "Opus", "description": "Most capable, for long reverse-engineering sessions with many tool calls",
              "effortLevels": ["low", "medium", "high", "xhigh", "max"], "supportsFastMode": true },
            { "modelId": "sonnet", "name": "Sonnet", "description": "Balanced speed and depth",
              "effortLevels": ["low", "medium", "high"] },
            { "modelId": "haiku", "name": "Haiku", "description": null }
        ]
    }))
    .ok()
}

/// Stands in for a chat panel: says what it was made from.
struct FakePanel {
    spec: PanelSpec,
    look: LookFn,
}

impl Render for FakePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::resolve((self.look)(cx).theme_mode, window.appearance());
        div()
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .text_size(px(12.0))
            .line_height(px(16.0))
            .text_color(theme.text_dim)
            .child(format!("chat panel of {}", self.spec.tab_id))
            .child(format!("restore last conversation: {}", self.spec.restore))
            .child(format!(
                "opened with: {}",
                self.spec
                    .initial_agent
                    .map_or("(its own choice)", AcpAgentId::as_str)
            ))
    }
}

/// The composer's model state, changed by the model menu's events.
#[derive(Clone)]
struct ModelState {
    model_id: String,
    effort: Option<String>,
    fast_mode: bool,
}

struct Preview {
    look: LookFn,
    last: Rc<RefCell<String>>,
    profile: ToolProfileId,
    tabs: Entity<ChatTabs>,
    picker: Entity<ModelPicker>,
    history: Entity<ChatHistoryMenu>,
    model_menu: Entity<ModelMenu>,
    prompt: Entity<PermissionPrompt>,
}

impl Preview {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let look: LookFn = {
            let look = Look {
                theme_mode: ThemeMode::from_env(),
                lang: Lang::detect(),
            };
            Rc::new(move |_| look)
        };
        let last = Rc::new(RefCell::new(String::from("(nothing yet)")));
        // Every fake host action lands here.
        let say = {
            let (last, preview) = (last.clone(), cx.entity_id());
            move |text: String, cx: &mut App| {
                *last.borrow_mut() = text;
                cx.notify(preview);
            }
        };

        let main = Rc::new(FakeMain {
            health: RefCell::new(vec![
                health(
                    AcpAgentId::ClaudeCode,
                    AgentHealthStatus::Ready,
                    true,
                    Some("max"),
                ),
                health(
                    AcpAgentId::Openai,
                    AgentHealthStatus::Ready,
                    false,
                    Some("api-key"),
                ),
                health(
                    AcpAgentId::Codex,
                    AgentHealthStatus::NotInstalled,
                    false,
                    None,
                ),
                health(
                    AcpAgentId::Anthropic,
                    AgentHealthStatus::NeedsKey,
                    false,
                    None,
                ),
                health(AcpAgentId::Grok, AgentHealthStatus::NeedsLogin, false, None),
                health(AcpAgentId::Glm, AgentHealthStatus::AuthFailed, false, None),
            ]),
            catalog: RefCell::default(),
            asked: RefCell::default(),
        });
        // The model store asks for the CLI lists; only Claude Code's "arrives".
        main.catalog.borrow_mut().start(main.as_ref());
        if main.asked.borrow().contains(&AcpAgentId::ClaudeCode) {
            main.catalog
                .borrow_mut()
                .put("claude-code", claude_catalog().as_ref());
        }

        // The profile store: starts from the host's profile, and a change the host
        // refuses is rolled back.
        let mut profile = ToolProfileStore::load(main.as_ref());
        let _ = profile.set_profile(main.as_ref(), ToolProfileId::Probe);

        // ── Session tabs ────────────────────────────────────────────────────
        let tabs = cx.new(|cx| {
            let panel_look = look.clone();
            let mut tabs = ChatTabs::new(
                move |spec, _, cx| -> AnyView {
                    let look = panel_look.clone();
                    cx.new(|_| FakePanel { spec, look }).into()
                },
                main.clone(),
                |_| Some(column(0)),
                look.clone(),
                window,
                cx,
            );
            tabs.add(AcpAgentId::Codex, window, cx);
            tabs.add(AcpAgentId::Glm, window, cx);
            tabs.activate("chat-1", cx);
            tabs.set_meta(
                "chat-1",
                ChatTabMeta {
                    title: "Reverse the login flow of shop.example".into(),
                    agent_id: AcpAgentId::ClaudeCode,
                    busy: true,
                },
                cx,
            );
            tabs.set_meta(
                "chat-2",
                ChatTabMeta {
                    title: "Signing key".into(),
                    agent_id: AcpAgentId::Codex,
                    busy: false,
                },
                cx,
            );
            let picker = tabs.picker().clone();
            picker.update(cx, |picker, cx| picker.set_open(true, window, cx));
            tabs
        });
        cx.subscribe(&tabs, {
            let say = say.clone();
            move |_, _, event: &ChatTabsEvent, cx| say(format!("tabs: {event:?}"), cx)
        })
        .detach();

        // ── Model picker ────────────────────────────────────────────────────
        let chosen = Rc::new(RefCell::new((AcpAgentId::ClaudeCode, "Opus".to_owned())));
        let picker = cx.new(|cx| {
            let chosen = chosen.clone();
            let mut picker = ModelPicker::new(
                move |_| {
                    let (agent_id, name) = chosen.borrow().clone();
                    ModelPickerProps {
                        agent_id,
                        chip_name: Some(name.clone()),
                        model_name: Some(name),
                        disabled: false,
                        new_tab: false,
                        column: Some(column(1)),
                    }
                },
                main.clone(),
                look.clone(),
                cx,
            );
            picker.set_open(true, window, cx);
            picker
        });
        cx.subscribe(&picker, {
            let (say, chosen) = (say.clone(), chosen.clone());
            move |_, _, event: &ModelPickerEvent, cx| {
                if let ModelPickerEvent::Selected { agent_id, name, .. } = event {
                    *chosen.borrow_mut() = (*agent_id, name.clone());
                }
                say(format!("picker: {event:?}"), cx);
            }
        })
        .detach();

        // ── History menu ────────────────────────────────────────────────────
        let storage = Rc::new(MemoryStorage::new());
        let saved = Rc::new(RefCell::new(ChatHistory::load(storage.as_ref())));
        let now = now_ms();
        for (id, agent_id, age, text) in CONVERSATIONS {
            let message =
                json!({ "id": "m1", "role": "user", "parts": [{ "type": "text", "text": text }] });
            let _ = saved.borrow_mut().save(
                storage.as_ref(),
                id,
                *agent_id,
                vec![message],
                now.saturating_sub(*age),
            );
        }
        let current = Rc::new(RefCell::new(Some("c-hours".to_owned())));
        let history = cx.new(|cx| {
            let (saved, current) = (saved.clone(), current.clone());
            let mut menu = ChatHistoryMenu::new(
                move |_| ChatHistoryMenuProps {
                    conversations: saved
                        .borrow()
                        .conversations()
                        .iter()
                        .map(HistoryRow::from)
                        .collect(),
                    current_id: current.borrow().clone(),
                    disabled: false,
                    column: Some(column(2)),
                },
                look.clone(),
            );
            menu.set_open(true, cx);
            menu
        });
        cx.subscribe(&history, {
            let say = say.clone();
            let storage: Rc<dyn Storage> = storage.clone();
            move |_, _, event: &ChatHistoryEvent, cx| {
                match event {
                    ChatHistoryEvent::Open(id) => *current.borrow_mut() = Some(id.clone()),
                    ChatHistoryEvent::Remove(id) => {
                        let _ = saved.borrow_mut().remove(storage.as_ref(), id);
                    }
                }
                say(format!("history: {event:?}"), cx);
            }
        })
        .detach();

        // ── Model menu ──────────────────────────────────────────────────────
        let model = Rc::new(RefCell::new(ModelState {
            model_id: "opus".into(),
            effort: Some("high".into()),
            fast_mode: true,
        }));
        let model_menu = cx.new(|cx| {
            let (model, main) = (model.clone(), main.clone());
            let mut menu = ModelMenu::new(
                move |_| {
                    let state = model.borrow().clone();
                    let models: Vec<MenuModel> = main
                        .catalog
                        .borrow()
                        .agent_models(AcpAgentId::ClaudeCode)
                        .iter()
                        .map(MenuModel::from)
                        .collect();
                    let label = models
                        .iter()
                        .find(|m| m.model_id == state.model_id)
                        .map_or_else(|| "Claude Code".to_owned(), |m| m.name.clone());
                    ModelMenuProps {
                        agent_id: AcpAgentId::ClaudeCode,
                        models,
                        current_model_id: Some(state.model_id),
                        label,
                        effort: state.effort,
                        fast_mode: state.fast_mode,
                        live: true,
                        disabled: false,
                        panel: Some(Span {
                            left: column(2).left + 12.0,
                            right: column(2).right - 12.0,
                        }),
                    }
                },
                look.clone(),
                cx,
            );
            menu.set_open(true, window, cx);
            menu
        });
        cx.subscribe(&model_menu, {
            let say = say.clone();
            move |_, _, event: &ModelMenuEvent, cx| {
                {
                    let mut state = model.borrow_mut();
                    match event {
                        ModelMenuEvent::Model(m) => state.model_id = m.model_id.clone(),
                        ModelMenuEvent::Effort(level) => state.effort = Some(level.clone()),
                        ModelMenuEvent::FastMode(on) => state.fast_mode = *on,
                    }
                }
                say(format!("model menu: {event:?}"), cx);
            }
        })
        .detach();

        // ── Permission prompt, over a real queue ────────────────────────────
        let queue = Rc::new(RefCell::new(PermissionStore::new(ToolRules, |_| {})));
        // Auto-approve off, so the ordinary second request waits behind the first.
        queue.borrow_mut().set_acp_auto_approve(false);
        let prompt = cx.new(|cx| {
            let queue = queue.clone();
            PermissionPrompt::new(
                move |_| {
                    let queue = queue.borrow();
                    PermissionPromptProps {
                        current: queue
                            .current_permission()
                            .map(|pending| (pending.id(), pending.request().clone())),
                        queue_len: queue.permission_queue().len(),
                    }
                },
                look.clone(),
                cx,
            )
        });
        cx.subscribe(&prompt, {
            let (say, queue) = (say.clone(), queue.clone());
            move |_, prompt, event: &PermissionPromptEvent, cx| {
                {
                    let mut queue = queue.borrow_mut();
                    match event {
                        PermissionPromptEvent::Respond {
                            option_id,
                            grant_session,
                        } => queue.respond_to_permission(
                            option_id,
                            ApproveOptions {
                                grant_session: *grant_session,
                            },
                        ),
                        PermissionPromptEvent::Approve { grant_session } => queue
                            .approve_current_permission(ApproveOptions {
                                grant_session: *grant_session,
                            }),
                        PermissionPromptEvent::Reject => queue.reject_current_permission(),
                    }
                }
                prompt.update(cx, |_, cx| cx.notify());
                say(format!("prompt: {event:?}"), cx);
            }
        })
        .detach();
        let delay = match std::env::var("REVER_PREVIEW_PROMPT").as_deref() {
            Ok("0") => None,
            Ok("1") => Some(Duration::ZERO),
            _ => Some(Duration::from_secs(3)),
        };
        if let Some(delay) = delay {
            let prompt = prompt.clone();
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(delay).await;
                for request in permission_requests() {
                    queue.borrow_mut().request_permission_from_user(
                        request,
                        &RedTeamMode::default(),
                        |_| {},
                    );
                }
                let _ = prompt.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }

        Self {
            look,
            last,
            profile: profile.profile(),
            tabs,
            picker,
            history,
            model_menu,
            prompt,
        }
    }
}

fn permission_requests() -> Vec<Value> {
    let options = json!([
        { "optionId": "allow-always", "name": "Always Allow", "kind": "allow_always" },
        { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
        { "optionId": "reject", "name": "Reject", "kind": "reject_once" }
    ]);
    vec![
        // A fuzzing run: risky and gated, so no "Always Allow" and a session checkbox.
        json!({
            "sessionId": "preview",
            "toolCall": {
                "toolCallId": "toolu_01",
                "title": "mcp__rever__intruder_run",
                "rawInput": {
                    "url": "https://shop.example:8443/api/login",
                    "method": "POST",
                    "positions": ["password"],
                    "payloads": ["admin", "letmein", "hunter2", "가나다"],
                    "concurrency": 4
                }
            },
            "options": options.clone()
        }),
        // An ordinary tool, asked about because auto-approve was switched off.
        json!({
            "sessionId": "preview",
            "toolCall": { "toolCallId": "toolu_02", "title": "", "rawInput": null },
            "options": options
        }),
    ]
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = (self.look)(cx);
        let theme = Theme::resolve(look.theme_mode, window.appearance());
        let label = |text: &'static str| {
            div()
                .text_size(px(11.0))
                .line_height(px(14.0))
                .text_color(theme.text_dim)
                .child(text)
        };
        // One chat column (`aside.agent-panel`).
        let aside = |index: usize| {
            div()
                .absolute()
                .left(px(column(index).left))
                .top(px(BAR + 8.0))
                .w(px(COLUMN))
                .bottom(px(GAP))
                .flex()
                .flex_col()
                .bg(theme.bg)
                .border_1()
                .border_color(theme.border)
        };
        // The row above a chat's messages.
        let header = || {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(8.0))
                .px(px(12.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(theme.border)
        };

        // Marks at the three sizes the views use, and the letter for an unknown agent.
        let icons = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(label("ProviderIcon"))
            .children(
                ["claude-code", "codex", "grok", "glm", "gemini"]
                    .into_iter()
                    .flat_map(|id| {
                        [12.0, 16.0, 24.0]
                            .map(|size| provider_icon(id, size, Some("G"), theme.text))
                    }),
            );

        let bar = div()
            .h(px(BAR))
            .px(px(GAP))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(16.0))
            .bg(theme.bg_bar)
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(12.0))
            .text_color(theme.text_dim)
            .child(icons)
            .child(format!("tool profile: {}", self.profile))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(format!("last host call — {}", self.last.borrow())),
            );

        // `.composer`
        let composer = div()
            .mx(px(12.0))
            .mb(px(12.0))
            .pt(px(10.0))
            .px(px(12.0))
            .pb(px(8.0))
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.0))
            .rounded(px(16.0))
            .border_1()
            .border_color(theme.border_2)
            .bg(theme.chrome_toolbar)
            .child(
                div()
                    .text_size(px(14.0))
                    .text_color(theme.text_dim)
                    .child(look.lang.t("chat.placeholder")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(div().flex_1())
                    .child(self.model_menu.clone()),
            );

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme.bg_bar)
            .text_color(theme.text)
            .text_size(px(13.0))
            .font_family(".SystemUIFont")
            .child(bar)
            .child(aside(0).child(self.tabs.clone()))
            .child(
                aside(1)
                    .child(
                        header()
                            .child(label("ModelPicker"))
                            .child(self.picker.clone()),
                    )
                    .child(div().flex_1()),
            )
            .child(
                aside(2)
                    .child(
                        header()
                            .child(label("ChatHistoryMenu"))
                            .child(div().flex_1())
                            .child(self.history.clone()),
                    )
                    .child(div().flex_1())
                    .child(div().px(px(12.0)).pb(px(4.0)).child(label("ModelMenu")))
                    .child(composer),
            )
            .child(self.prompt.clone())
    }
}

fn main() {
    rever_ui_kit::preview::run(WIDTH, HEIGHT, |window, cx| {
        cx.new(|cx| Preview::new(window, cx))
    });
}
