//! Port of `src/renderer/src/components/chat/ModelMenu.tsx` — the composer's model chip
//! for ONE agent (the tab's): search, pick a model, and — when the agent reports them —
//! reasoning effort and fast mode.
//!
//! `onModel`, `onEffort` and `onFastMode` are the [`ModelMenuEvent`]s.

use gpui::{
    App, Context, Corner, Entity, EventEmitter, Focusable, FontWeight, MouseButton, MouseDownEvent,
    Rgba, Subscription, Window, anchored, deferred, div, point, prelude::*, px, rgb,
};
use rever_ui_kit::text_input::TextInput;
use rever_ui_menus::ui::LookFn;
use rever_ui_state::constants::AcpAgentId;
use serde::{Deserialize, Serialize};

use crate::model_catalog::LiveModel;
use crate::provider_icon::provider_icon;
use crate::ui::{
    BoundsCell, Rect, Span, Tokens, Z_POPOVER, caret, ellipsis, field, js_trim, natural_width,
    search_icon, shadow, single_line, text, tooltip, track_bounds,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MenuModel {
    pub model_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_levels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_fast_mode: Option<bool>,
}

impl From<&LiveModel> for MenuModel {
    fn from(model: &LiveModel) -> Self {
        Self {
            model_id: model.model_id.clone(),
            name: model.name.clone(),
            description: model.description.clone(),
            effort_levels: model.effort_levels.clone(),
            supports_fast_mode: model.supports_fast_mode,
        }
    }
}

/// What the React component takes as props, minus the callbacks.
#[derive(Clone, Debug)]
pub struct ModelMenuProps {
    pub agent_id: AcpAgentId,
    pub models: Vec<MenuModel>,
    pub current_model_id: Option<String>,
    /// Chip text: the model name (falls back to the agent name).
    pub label: String,
    pub effort: Option<String>,
    pub fast_mode: bool,
    /// Effort / fast mode can only change on a running session.
    pub live: bool,
    pub disabled: bool,
    /// The composer the menu lines up with; the chip itself when `None`.
    pub panel: Option<Span>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelMenuEvent {
    Model(MenuModel),
    Effort(String),
    FastMode(bool),
}

/// The dictionary key of a reasoning level the interface has a name for.
pub fn effort_key(level: &str) -> Option<&'static str> {
    match level {
        "low" => Some("model.effort.low"),
        "medium" => Some("model.effort.medium"),
        "high" => Some("model.effort.high"),
        "xhigh" => Some("model.effort.xhigh"),
        "max" => Some("model.effort.max"),
        _ => None,
    }
}

/// The models the search leaves: those whose name or description contains the query.
pub fn shown_models<'a>(models: &'a [MenuModel], query: &str) -> Vec<&'a MenuModel> {
    let q = js_trim(query).to_lowercase();
    models
        .iter()
        .filter(|m| {
            q.is_empty()
                || format!("{} {}", m.name, m.description.as_deref().unwrap_or(""))
                    .to_lowercase()
                    .contains(&q)
        })
        .collect()
}

/// Where the menu goes: its bottom-right corner, as distances from the window's bottom
/// and right edges, and its width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MenuPlacement {
    pub bottom: f32,
    pub right: f32,
    pub width: f32,
}

/// Fixed, not absolute: the chat column clips overflow, which cut the old popover off.
/// The menu opens upward from the chip and ends at the composer's right edge.
pub fn menu_placement(trigger: Rect, panel: Option<Span>, viewport: (f32, f32)) -> MenuPlacement {
    MenuPlacement {
        bottom: viewport.1 - trigger.top + 6.0,
        right: viewport.0 - panel.map_or(trigger.right, |panel| panel.right),
        width: panel
            .map_or(340.0, |panel| panel.right - panel.left)
            .min(340.0),
    }
}

/// `.chrome-menu` is a content-box with `min-width: 260px` and a 1px border.
fn outer_width(width: f32) -> f32 {
    width.max(260.0) + 2.0
}

const CHIP: &str = "model-chip";

pub struct ModelMenu {
    /// Read on every render, as re-rendered props are.
    props: Box<dyn Fn(&App) -> ModelMenuProps>,
    look: LookFn,
    open: bool,
    query: Entity<TextInput>,
    trigger: BoundsCell,
    escape: Option<Subscription>,
}

impl EventEmitter<ModelMenuEvent> for ModelMenu {}

impl ModelMenu {
    pub fn new(
        props: impl Fn(&App) -> ModelMenuProps + 'static,
        look: LookFn,
        cx: &mut Context<Self>,
    ) -> Self {
        // The placeholder is drawn by `field`, in the language of each render.
        let query = cx.new(|cx| TextInput::new("", cx));
        // Typing in the field changes the list.
        cx.observe(&query, |_, _, cx| cx.notify()).detach();
        Self {
            props: Box::new(props),
            look,
            open: false,
            query,
            trigger: BoundsCell::default(),
            escape: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opening clears the search and puts the cursor in it (`autoFocus`).
    pub fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if open && !self.open {
            self.query.update(cx, |query, cx| query.set_text("", cx));
            window.focus(&self.query.focus_handle(cx));
        }
        self.open = open;
        // Window-level, so Esc works wherever the focus is. Kept only while open.
        self.escape = open.then(|| {
            cx.observe_keystrokes(|this, event, window, cx| {
                if event.keystroke.key == "escape" {
                    this.set_open(false, window, cx);
                }
            })
        });
        cx.notify();
    }
}

/// `input.switch`
fn switch(on: bool, disabled: bool, tokens: &Tokens) -> impl IntoElement {
    div()
        .w(px(30.0))
        .h(px(18.0))
        .flex_none()
        .relative()
        .rounded(px(9.0))
        .bg(if on {
            tokens.theme.accent
        } else {
            tokens.border_3
        })
        .when(disabled, |switch| switch.opacity(0.4))
        .child(
            div()
                .absolute()
                .top(px(2.0))
                .left(px(if on { 14.0 } else { 2.0 }))
                .size(px(14.0))
                .rounded_full()
                .bg(rgb(0xffffff)),
        )
}

impl Render for ModelMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let lang = look.lang;
        let props = (self.props)(cx);
        let open = self.open;
        let hover = theme.chrome_hover;
        let dim = theme.text_dim;
        let color = if open { theme.text } else { dim };

        let effort_text = props
            .effort
            .as_deref()
            .and_then(effort_key)
            .map(|key| lang.t(key));
        let label = single_line(&props.label);
        let label_width = natural_width(&label, 12.0, FontWeight::NORMAL, window);
        let agent = props.agent_id.as_str();
        // An image does not take the chip's hover color, so the hovered icon is a second
        // image shown in place of the first.
        let icon = |color: Rgba| provider_icon(agent, 13.0, None, color);

        // `.composer-chip`
        let chip = div()
            .id("model-chip")
            .debug_selector(|| "model-chip".into())
            .group(CHIP)
            .relative()
            .h(px(28.0))
            .max_w_full()
            .min_w_0()
            .overflow_hidden()
            .px(px(9.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .rounded(px(14.0))
            .text_size(px(12.0))
            .line_height(px(14.0))
            .text_color(color)
            .whitespace_nowrap()
            .when(open, |chip| chip.bg(hover))
            .when(props.disabled, |chip| chip.opacity(0.42))
            .when(!props.disabled, |chip| {
                chip.cursor_pointer()
                    .hover(move |style| style.bg(hover).text_color(theme.text))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_open(!this.open, window, cx);
                    }))
            })
            .tooltip(tooltip(lang.t("model.chipTitle"), &tokens))
            .child(
                div()
                    .size(px(13.0))
                    .flex_none()
                    .relative()
                    .child(icon(color))
                    .when(!props.disabled && !open, |slot| {
                        slot.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .invisible()
                                .group_hover(CHIP, |style| style.visible())
                                .child(icon(theme.text)),
                        )
                    }),
            )
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .overflow_hidden()
                    .child(
                        ellipsis(label, 14.0)
                            .w(px(label_width))
                            .min_w_0()
                            .flex_shrink(),
                    )
                    .when_some(effort_text, |label, effort| {
                        label.child(
                            div()
                                .flex_none()
                                .text_color(dim)
                                .child(format!("\u{a0}{effort}")),
                        )
                    })
                    .when(props.fast_mode, |label| {
                        label.child(div().flex_none().text_color(dim).child("\u{a0}⚡"))
                    }),
            )
            .child(caret(9.0, 0.7));

        let wrap = div()
            .relative()
            .flex()
            .min_w_0()
            .child(track_bounds(&self.trigger))
            .child(chip);
        if !open {
            return wrap;
        }
        let Some(trigger) = self.trigger.get().map(Rect::from) else {
            window.request_animation_frame();
            return wrap;
        };
        let viewport = window.viewport_size();
        let (viewport_width, viewport_height) = (f32::from(viewport.width), viewport.height.into());
        let place = menu_placement(trigger, props.panel, (viewport_width, viewport_height));

        let current = props
            .models
            .iter()
            .find(|m| Some(m.model_id.as_str()) == props.current_model_id.as_deref());
        let levels: &[String] = current
            .and_then(|m| m.effort_levels.as_deref())
            .unwrap_or_default();
        let fast = current.is_some_and(|m| m.supports_fast_mode == Some(true));
        let has_options = !levels.is_empty() || fast;
        let live = props.live;
        let after_start = lang.t("model.optionsAfterStart");

        let query = self.query.read(cx).text().to_owned();
        let shown = shown_models(&props.models, &query);
        let none_match = shown.is_empty();
        let rows = shown.into_iter().enumerate().map(|(index, m)| {
            let checked = Some(m.model_id.as_str()) == props.current_model_id.as_deref();
            let model = m.clone();
            // `.model-menu-row`: a `button`, so its lines are 16px tall.
            div()
                .id(("model-menu-row", index))
                .debug_selector(move || format!("model-menu-row-{index}"))
                .w_full()
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(10.0))
                .py(px(7.0))
                .rounded(px(8.0))
                .line_height(px(16.0))
                .cursor_pointer()
                .when(checked, |row| row.bg(hover))
                .hover(move |style| style.bg(hover))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.emit(ModelMenuEvent::Model(model.clone()));
                    this.set_open(false, window, cx);
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(div().child(m.name.clone()))
                        .when_some(
                            m.description.clone().filter(|text| !text.is_empty()),
                            |column, description| {
                                // `.composer-menu-hint`
                                column.child(
                                    div().text_size(px(11.0)).text_color(dim).child(description),
                                )
                            },
                        ),
                )
                .when(checked, |row| row.child(div().flex_none().child("✓")))
        });

        // `.model-menu-search`
        let search = div()
            .h(px(32.0))
            .mt(px(2.0))
            .mx(px(8.0))
            .mb(px(6.0))
            .px(px(10.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .rounded(px(8.0))
            .bg(theme.surface_2)
            .child(search_icon(13.0, dim))
            .child(field(
                &self.query,
                lang.t("model.search"),
                false,
                theme.text,
                cx,
            ));

        // `.model-menu-list`
        let list = div()
            .id("model-menu-list")
            .min_h_0()
            .overflow_y_scroll()
            .px(px(6.0))
            .children(rows)
            .when(none_match, |list| {
                list.child(
                    text(11.0)
                        .px(px(14.0))
                        .py(px(8.0))
                        .text_color(dim)
                        .child(lang.t("model.noMatch")),
                )
            });

        // `.model-menu-option`
        let option = |id: &'static str| {
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .justify_between()
                .gap(px(6.0))
                .pt(px(6.0))
                .pr(px(10.0))
                .pb(px(6.0))
                .pl(px(14.0))
                .whitespace_nowrap()
                .text_size(px(12.0))
                .line_height(px(14.0))
                .text_color(theme.text_2)
                .when(!live, |row| row.tooltip(tooltip(after_start, &tokens)))
        };

        let effort_row = (!levels.is_empty()).then(|| {
            let buttons = levels.iter().enumerate().map(|(index, level)| {
                let checked = Some(level.as_str()) == props.effort.as_deref();
                let picked = level.clone();
                // `.effort-seg button`
                div()
                    .id(("model-effort", index))
                    .debug_selector(move || format!("model-effort-{index}"))
                    .px(px(6.0))
                    .py(px(3.0))
                    .rounded(px(6.0))
                    .line_height(px(16.0))
                    .when(checked, |button| {
                        button
                            .bg(theme.surface_3)
                            .text_color(theme.text)
                            .font_weight(FontWeight::SEMIBOLD)
                    })
                    .when(!live, |button| button.opacity(0.42))
                    .when(live, |button| {
                        button
                            .cursor_pointer()
                            // The hover rule outranks the checked one in styles.css, so
                            // the checked level changes its background too.
                            .hover(move |style| style.bg(hover))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(ModelMenuEvent::Effort(picked.clone()));
                            }))
                    })
                    .child(match effort_key(level) {
                        Some(key) => lang.t(key).to_owned(),
                        None => level.clone(),
                    })
            });
            option("model-effort-row")
                .child(lang.t("model.effort"))
                // `.effort-seg`
                .child(div().flex().gap(px(1.0)).children(buttons))
        });

        let fast_mode = props.fast_mode;
        let fast_row = fast.then(|| {
            // A `label`: a click anywhere on the line flips the switch.
            option("model-fast-row")
                .debug_selector(|| "model-fast-row".into())
                .when(live, |row| {
                    row.cursor_pointer()
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(ModelMenuEvent::FastMode(!fast_mode));
                        }))
                })
                .child(lang.t("model.fastMode"))
                .child(switch(fast_mode, !live, &tokens))
        });

        // `.chrome-menu.model-menu`
        let menu = div()
            .id("model-menu")
            .debug_selector(|| "model-menu".into())
            .w(px(outer_width(place.width)))
            .max_h(viewport.height * 0.7)
            .flex()
            .flex_col()
            .py(px(6.0))
            .rounded(px(8.0))
            .bg(tokens.surface)
            .border_1()
            .border_color(theme.border)
            .shadow(vec![shadow(4.0, 16.0, tokens.shadow)])
            .text_size(px(13.0))
            .text_color(theme.text)
            .occlude()
            // The React menu sits on a full-window layer that takes every click outside
            // it: the click closes the menu and does not reach what is underneath.
            .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                if event.button == MouseButton::Left {
                    this.set_open(false, window, cx);
                }
                cx.stop_propagation();
            }))
            .child(search)
            .child(list)
            .when(has_options, |menu| {
                // `.chrome-menu-sep`
                menu.child(div().h(px(1.0)).flex_none().my(px(6.0)).bg(theme.border))
            })
            .children(effort_row)
            .children(fast_row)
            .when(!live && has_options, |menu| {
                // `.chrome-menu-foot`
                menu.child(
                    text(11.0)
                        .flex_none()
                        .pt(px(6.0))
                        .px(px(16.0))
                        .pb(px(2.0))
                        .text_color(dim)
                        .child(after_start),
                )
            });

        wrap.child(
            deferred(
                anchored()
                    .snap_to_window()
                    .anchor(Corner::BottomRight)
                    .position(point(
                        px(viewport_width - place.right),
                        px(viewport_height - place.bottom),
                    ))
                    .child(menu),
            )
            .with_priority(Z_POPOVER),
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn model(name: &str, description: Option<&str>) -> MenuModel {
        MenuModel {
            model_id: name.to_lowercase(),
            name: name.to_owned(),
            description: description.map(str::to_owned),
            ..MenuModel::default()
        }
    }

    fn names(models: &[&MenuModel]) -> Vec<String> {
        models.iter().map(|m| m.name.clone()).collect()
    }

    #[test]
    fn 검색어가_없으면_모든_모델을_보여준다() {
        let models = [model("Opus", None), model("Haiku", Some("Fast"))];
        assert_eq!(names(&shown_models(&models, "")), ["Opus", "Haiku"]);
        assert_eq!(names(&shown_models(&models, "  \t")), ["Opus", "Haiku"]);
    }

    #[test]
    fn 이름이나_설명에_검색어가_들어_있는_모델만_남긴다() {
        let models = [
            model("Opus", Some("Most capable")),
            model("Haiku", Some("Fastest, for quick answers")),
            model("Sonnet", None),
        ];
        assert_eq!(names(&shown_models(&models, "opus")), ["Opus"]);
        assert_eq!(names(&shown_models(&models, " QUICK ")), ["Haiku"]);
        assert_eq!(
            names(&shown_models(&models, "o")),
            ["Opus", "Haiku", "Sonnet"]
        );
        assert!(shown_models(&models, "gpt").is_empty());
    }

    #[test]
    fn 이름과_설명은_빈칸_하나로_이어서_찾는다() {
        let models = [model("Opus", Some("Most capable")), model("Sonnet", None)];
        assert_eq!(names(&shown_models(&models, "opus most")), ["Opus"]);
        // Not the other way round, and not across two models.
        assert!(shown_models(&models, "capable opus").is_empty());
        assert!(shown_models(&models, "capable sonnet").is_empty());
    }

    #[test]
    fn 검색어는_자바스크립트처럼_다듬고_소문자로_바꾼다() {
        let models = [model("Claude Opus", None), model("ÉCLAIR", Some("ΣΑΣ"))];
        // U+FEFF is whitespace for String.prototype.trim, not for Rust's trim.
        assert_eq!(
            names(&shown_models(&models, "\u{feff} OPUS\u{a0}")),
            ["Claude Opus"]
        );
        assert_eq!(names(&shown_models(&models, "éclair")), ["ÉCLAIR"]);
        // 'ΣΑΣ'.toLowerCase() is 'σας': the last sigma is the final form.
        assert_eq!(names(&shown_models(&models, "σας")), ["ÉCLAIR"]);
        assert!(shown_models(&models, "σασ").is_empty());
    }

    #[test]
    fn 아는_추론_단계만_사전_키가_있다() {
        assert_eq!(effort_key("low"), Some("model.effort.low"));
        assert_eq!(effort_key("medium"), Some("model.effort.medium"));
        assert_eq!(effort_key("high"), Some("model.effort.high"));
        assert_eq!(effort_key("xhigh"), Some("model.effort.xhigh"));
        assert_eq!(effort_key("max"), Some("model.effort.max"));
        assert_eq!(effort_key("minimal"), None);
        assert_eq!(effort_key("High"), None);
        assert_eq!(effort_key(""), None);
    }

    fn chip() -> Rect {
        Rect {
            left: 700.0,
            top: 560.0,
            right: 800.0,
            bottom: 588.0,
        }
    }

    #[test]
    fn 메뉴는_칩_위_6픽셀에서_위로_열리고_컴포저의_오른쪽_끝에_맞춘다() {
        let panel = Some(Span {
            left: 612.0,
            right: 988.0,
        });
        assert_eq!(
            menu_placement(chip(), panel, (1000.0, 640.0)),
            MenuPlacement {
                bottom: 86.0,
                right: 12.0,
                width: 340.0
            }
        );
    }

    #[test]
    fn 컴포저가_좁으면_메뉴도_그_너비다() {
        let panel = Some(Span {
            left: 700.0,
            right: 988.0,
        });
        assert_eq!(menu_placement(chip(), panel, (1000.0, 640.0)).width, 288.0);
    }

    #[test]
    fn 컴포저를_모르면_칩의_오른쪽_끝에_맞추고_너비는_340이다() {
        assert_eq!(
            menu_placement(chip(), None, (1000.0, 640.0)),
            MenuPlacement {
                bottom: 86.0,
                right: 200.0,
                width: 340.0
            }
        );
    }

    #[test]
    fn 메뉴_상자는_최소_260에_테두리를_더한_너비다() {
        assert_eq!(outer_width(340.0), 342.0);
        assert_eq!(outer_width(200.0), 262.0);
    }

    #[test]
    fn 모델은_camelcase_json과_오간다() {
        let parsed: MenuModel = serde_json::from_value(json!({
            "modelId": "opus", "name": "Opus", "description": null,
            "effortLevels": ["low", "high"], "supportsFastMode": true
        }))
        .unwrap();
        assert_eq!(
            parsed.effort_levels.as_deref().map(<[String]>::len),
            Some(2)
        );
        assert_eq!(parsed.supports_fast_mode, Some(true));
        assert_eq!(
            json!(model("Opus", None)),
            json!({ "modelId": "opus", "name": "Opus", "description": null })
        );
    }

    #[test]
    fn 라이브_모델에서_메뉴의_모델을_만든다() {
        let live = LiveModel {
            agent_id: AcpAgentId::ClaudeCode,
            model_id: "opus".into(),
            name: "Opus".into(),
            description: Some("Most capable".into()),
            effort_levels: Some(vec!["low".into()]),
            supports_fast_mode: Some(false),
        };
        let menu = MenuModel::from(&live);
        assert_eq!(menu.model_id, "opus");
        assert_eq!(menu.description.as_deref(), Some("Most capable"));
        assert_eq!(menu.effort_levels, Some(vec!["low".to_owned()]));
        assert_eq!(menu.supports_fast_mode, Some(false));
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::ui::testing::{click, look};

    /// The chip near the bottom of a window-filling surface that counts the presses
    /// reaching it — what lies "under" the menu.
    struct Stage {
        menu: Entity<ModelMenu>,
        presses: Rc<Cell<usize>>,
    }

    impl Render for Stage {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let presses = self.presses.clone();
            div()
                .size_full()
                .child(
                    div()
                        .absolute()
                        .size_full()
                        .on_any_mouse_down(move |_, _, _| presses.set(presses.get() + 1)),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(500.0))
                        .top(px(600.0))
                        .flex()
                        .child(self.menu.clone()),
                )
        }
    }

    struct Fixture {
        menu: Entity<ModelMenu>,
        events: Rc<RefCell<Vec<ModelMenuEvent>>>,
        presses: Rc<Cell<usize>>,
    }

    fn models() -> Vec<MenuModel> {
        vec![
            MenuModel {
                model_id: "opus".into(),
                name: "Opus".into(),
                description: Some("Most capable".into()),
                effort_levels: Some(vec!["low".into(), "high".into(), "ultra".into()]),
                supports_fast_mode: Some(true),
            },
            MenuModel {
                model_id: "haiku".into(),
                name: "Haiku".into(),
                ..MenuModel::default()
            },
        ]
    }

    fn stage(
        live: bool,
        disabled: bool,
        cx: &mut TestAppContext,
    ) -> (Fixture, &mut VisualTestContext) {
        let events: Rc<RefCell<Vec<ModelMenuEvent>>> = Rc::default();
        let presses = Rc::new(Cell::new(0));
        let (seen, counted) = (events.clone(), presses.clone());
        let (stage, cx) = cx.add_window_view(move |_, cx| {
            let menu = cx.new(|cx| {
                ModelMenu::new(
                    move |_| ModelMenuProps {
                        agent_id: AcpAgentId::ClaudeCode,
                        models: models(),
                        current_model_id: Some("opus".into()),
                        label: "Opus".into(),
                        effort: Some("high".into()),
                        fast_mode: false,
                        live,
                        disabled,
                        panel: None,
                    },
                    look(),
                    cx,
                )
            });
            cx.subscribe(&menu, move |_, _, event: &ModelMenuEvent, _| {
                seen.borrow_mut().push(event.clone());
            })
            .detach();
            Stage {
                menu,
                presses: counted,
            }
        });
        let menu = stage.read_with(cx, |stage, _| stage.menu.clone());
        (
            Fixture {
                menu,
                events,
                presses,
            },
            cx,
        )
    }

    fn is_open(fixture: &Fixture, cx: &mut VisualTestContext) -> bool {
        fixture.menu.read_with(cx, |menu, _| menu.is_open())
    }

    #[gpui::test]
    fn 칩을_누르면_메뉴가_칩_위로_열린다(cx: &mut TestAppContext) {
        let (fixture, cx) = stage(true, false, cx);
        click("model-chip", cx);
        assert!(is_open(&fixture, cx));
        let chip = cx.debug_bounds("model-chip").unwrap();
        let menu = cx.debug_bounds("model-menu").unwrap();
        assert_eq!(chip.size.height, px(28.0));
        assert_eq!(menu.bottom(), chip.top() - px(6.0));
        assert_eq!(menu.right(), chip.right());
        assert_eq!(menu.size.width, px(342.0));
    }

    #[gpui::test]
    fn 비활성_칩은_눌러도_열리지_않는다(cx: &mut TestAppContext) {
        let (fixture, cx) = stage(true, true, cx);
        click("model-chip", cx);
        assert!(!is_open(&fixture, cx));
    }

    #[gpui::test]
    fn 모델을_누르면_그_모델을_알리고_닫힌다(cx: &mut TestAppContext) {
        let (fixture, cx) = stage(true, false, cx);
        click("model-chip", cx);
        click("model-menu-row-1", cx);
        assert_eq!(
            *fixture.events.borrow(),
            [ModelMenuEvent::Model(models().remove(1))]
        );
        assert!(!is_open(&fixture, cx));
    }

    #[gpui::test]
    fn 검색어를_치면_목록이_줄고_다시_열면_검색어가_비워진다(
        cx: &mut TestAppContext,
    ) {
        let (fixture, cx) = stage(true, false, cx);
        click("model-chip", cx);
        // The search field has the focus as soon as the menu opens.
        cx.simulate_input("hai");
        cx.run_until_parked();
        click("model-menu-row-0", cx);
        assert_eq!(
            *fixture.events.borrow(),
            [ModelMenuEvent::Model(models().remove(1))]
        );
        click("model-chip", cx);
        click("model-menu-row-0", cx);
        assert_eq!(
            fixture.events.borrow().last(),
            Some(&ModelMenuEvent::Model(models().remove(0)))
        );
    }

    #[gpui::test]
    fn 세션이_있으면_추론_단계와_빠른_모드를_바꿀_수_있다(
        cx: &mut TestAppContext,
    ) {
        let (fixture, cx) = stage(true, false, cx);
        click("model-chip", cx);
        click("model-effort-0", cx);
        click("model-effort-2", cx);
        click("model-fast-row", cx);
        assert_eq!(
            *fixture.events.borrow(),
            [
                ModelMenuEvent::Effort("low".into()),
                // A level without a name of its own is still offered, under its id.
                ModelMenuEvent::Effort("ultra".into()),
                ModelMenuEvent::FastMode(true),
            ]
        );
        // The menu stays open for these.
        assert!(is_open(&fixture, cx));
    }

    #[gpui::test]
    fn 세션이_없으면_추론_단계와_빠른_모드는_눌러도_바뀌지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (fixture, cx) = stage(false, false, cx);
        click("model-chip", cx);
        click("model-effort-0", cx);
        click("model-fast-row", cx);
        assert!(fixture.events.borrow().is_empty());
        assert!(is_open(&fixture, cx));
    }

    #[gpui::test]
    fn esc를_누르면_닫힌다(cx: &mut TestAppContext) {
        let (fixture, cx) = stage(true, false, cx);
        click("model-chip", cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!is_open(&fixture, cx));
        assert!(fixture.events.borrow().is_empty());
    }

    #[gpui::test]
    fn 바깥을_누르면_닫히고_그_클릭은_아래로_가지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (fixture, cx) = stage(true, false, cx);
        click("model-chip", cx);
        let before = fixture.presses.get();
        cx.simulate_click(point(px(40.0), px(40.0)), Modifiers::none());
        cx.run_until_parked();
        assert!(!is_open(&fixture, cx));
        assert_eq!(fixture.presses.get(), before);
    }

    #[gpui::test]
    fn 열린_채로_칩을_다시_누르면_닫힌다(cx: &mut TestAppContext) {
        let (fixture, cx) = stage(true, false, cx);
        click("model-chip", cx);
        click("model-chip", cx);
        assert!(!is_open(&fixture, cx));
    }
}
