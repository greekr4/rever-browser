//! Port of `src/renderer/src/components/BotCheckButton.tsx` — the bottom-left floating
//! "Bot check" button and its menu of bot-detection test sites.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, App, Context, FontWeight, Subscription, Window, deferred, div,
    prelude::*, px,
};

use crate::ui::{LookFn, Tokens, Z_BOTCHECK, on_escape, shadow, text, tooltip};

pub struct Site {
    pub url: &'static str,
    pub name: &'static str,
    pub desc_key: &'static str,
}

pub const SITES: &[Site] = &[
    Site {
        url: "https://bot.sannysoft.com",
        name: "Sannysoft",
        desc_key: "botcheck.sannysoft.desc",
    },
    Site {
        url: "https://abrahamjuliot.github.io/creepjs/",
        name: "CreepJS",
        desc_key: "botcheck.creepjs.desc",
    },
    Site {
        url: "https://browserleaks.com/",
        name: "BrowserLeaks",
        desc_key: "botcheck.browserleaks.desc",
    },
    Site {
        url: "https://amiunique.org/fingerprint",
        name: "amiunique",
        desc_key: "botcheck.amiunique.desc",
    },
    Site {
        url: "https://pixelscan.net/",
        name: "pixelscan",
        desc_key: "botcheck.pixelscan.desc",
    },
];

/// framer-motion's `{ type: 'spring', stiffness: 320, damping: 32 }`, mass 1.
const STIFFNESS: f32 = 320.0;
const DAMPING: f32 = 32.0;
/// The spring is within 0.2% of rest by now; framer-motion stops it about here.
const SPRING_MS: u64 = 400;
/// The menu starts (and leaves) 8px below its place.
const TRAVEL: f32 = 8.0;

/// How far a spring released at 0 has moved toward 1 after `seconds`. It overshoots
/// slightly: the spring is underdamped (damping ratio 0.89).
pub fn spring(seconds: f32) -> f32 {
    let seconds = seconds.max(0.0);
    let natural = STIFFNESS.sqrt();
    let ratio = DAMPING / (2.0 * natural);
    let decay = ratio * natural;
    let damped = natural * (1.0 - ratio * ratio).sqrt();
    1.0 - (-decay * seconds).exp()
        * ((damped * seconds).cos() + decay / damped * (damped * seconds).sin())
}

/// The menu's opacity and downward shift `progress` (0–1) into its animation:
/// opening runs `initial → animate`, closing runs `animate → exit`.
pub fn menu_frame(opening: bool, progress: f32) -> (f32, f32) {
    let moved = spring(progress * SPRING_MS as f32 / 1000.0);
    let shown = if opening { moved } else { 1.0 - moved };
    // Only the opacity is bounded; the position follows the spring past its target.
    (shown.clamp(0.0, 1.0), TRAVEL * (1.0 - shown))
}

/// Opens the picked address.
type OnNavigate = Rc<dyn Fn(&str, &mut Window, &mut App)>;

pub struct BotCheckButton {
    on_navigate: OnNavigate,
    look: LookFn,
    open: bool,
    /// The menu is still drawn while it animates out (`AnimatePresence`).
    mounted: bool,
    /// Counts open/close changes: names each animation run and lets a newer change
    /// cancel the unmount an older close scheduled.
    generation: u64,
    escape: Option<Subscription>,
}

impl BotCheckButton {
    pub fn new(on_navigate: impl Fn(&str, &mut Window, &mut App) + 'static, look: LookFn) -> Self {
        Self {
            on_navigate: Rc::new(on_navigate),
            look,
            open: false,
            mounted: false,
            generation: 0,
            escape: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if open == self.open {
            return;
        }
        self.open = open;
        self.generation += 1;
        self.escape = open.then(|| on_escape(cx, |this, cx| this.set_open(false, cx)));
        if open {
            self.mounted = true;
        } else {
            let generation = self.generation;
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(SPRING_MS))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.mounted = false;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }
}

impl Render for BotCheckButton {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let open = self.open;
        let hover = tokens.glass_hover;

        let menu = self.mounted.then(|| {
            let button = cx.weak_entity();
            let items = SITES.iter().enumerate().map(|(index, site)| {
                let (button, on_navigate) = (button.clone(), self.on_navigate.clone());
                // `.botcheck-item`
                div()
                    .id(("botcheck-item", index))
                    .debug_selector(|| format!("botcheck-item-{index}"))
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(2.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(gpui::transparent_black())
                    // The item is a `button`, and the base `button` rule sets
                    // `line-height: 16px` for both of its lines.
                    .line_height(px(16.0))
                    .cursor_pointer()
                    // The base `button:hover:not(:disabled)` rule outranks `.botcheck-item:hover`
                    // (specificity 0,2,1 over 0,2,0), so the hover fill is `--border-2`; only
                    // the border color comes from the item's own rule.
                    .hover(move |style| style.bg(theme.border_2).border_color(hover))
                    // Not a view listener: the host's navigation may reach back into this
                    // button. A menu that is animating out still takes clicks, as the
                    // node `AnimatePresence` keeps around does.
                    .on_click(move |_, window, cx| {
                        on_navigate(site.url, window, cx);
                        let _ = button.update(cx, |this, cx| this.set_open(false, cx));
                    })
                    .child(
                        // `.botcheck-item-name`
                        div()
                            .text_size(px(12.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(site.name),
                    )
                    .child(
                        // `.botcheck-item-desc`
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.text_dim)
                            .child(look.lang.t(site.desc_key)),
                    )
            });
            // `.botcheck-menu`
            div()
                .id("botcheck-menu")
                .debug_selector(|| "botcheck-menu".into())
                // 280 of content plus padding and border: the CSS box is content-box.
                .min_w(px(298.0))
                .p(px(8.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .rounded(px(14.0))
                .bg(tokens.glass_panel)
                .border_1()
                .border_color(tokens.glass_hairline)
                .shadow(vec![shadow(12.0, 40.0, tokens.shadow)])
                .occlude()
                .child(
                    // `.botcheck-menu-title`
                    text(10.0)
                        .pt(px(6.0))
                        .px(px(10.0))
                        .pb(px(4.0))
                        .text_color(theme.text_dim)
                        .child(look.lang.t("botcheck.menuTitle").to_uppercase()),
                )
                .children(items)
                .with_animation(
                    ("botcheck-menu", self.generation),
                    Animation::new(Duration::from_millis(SPRING_MS)),
                    move |menu, progress| {
                        let (opacity, shift) = menu_frame(open, progress);
                        menu.opacity(opacity).top(px(shift))
                    },
                )
        });

        // `.chip.botcheck-trigger`
        let trigger = div()
            .id("botcheck-trigger")
            .debug_selector(|| "botcheck-trigger".into())
            .relative()
            .h(px(28.0))
            .px(px(14.0))
            .flex()
            .flex_none()
            .items_center()
            .rounded(px(14.0))
            .border_1()
            .border_color(tokens.glass_hairline)
            // A GPUI shadow is painted under the whole box, not only outside it, and
            // would show through the nearly clear hover/active fill: those go without.
            .when(open, |trigger| trigger.bg(hover))
            .when(!open, |trigger| {
                trigger
                    .bg(tokens.glass_panel)
                    .shadow(vec![shadow(6.0, 24.0, tokens.shadow)])
            })
            .text_size(px(11.0))
            .text_color(theme.text)
            .whitespace_nowrap()
            .cursor_pointer()
            // The button floats over the page, which must not take its presses.
            .occlude()
            .hover(move |style| style.bg(hover).shadow(Vec::new()))
            .tooltip(tooltip(look.lang.t("botcheck.triggerTitle"), &tokens))
            .on_click(cx.listener(|this, _, _, cx| this.set_open(!this.open, cx)))
            .when(open, |trigger| {
                // `.chip.active::before`: the accent bar above an active chip. Offsets are
                // from the padding box, which starts inside the 1px border.
                trigger.child(
                    div()
                        .absolute()
                        .top(px(-7.0))
                        .left(px(9.0))
                        .right(px(9.0))
                        .h(px(2.0))
                        .rounded_b(px(2.0))
                        .bg(theme.accent),
                )
            })
            .child(look.lang.t("botcheck.trigger"));

        // `.botcheck`: pinned to the bottom-left of whatever the host puts it in.
        deferred(
            div()
                .id("botcheck")
                .absolute()
                .left(px(12.0))
                .bottom(px(12.0))
                .flex()
                .flex_col()
                .items_start()
                .gap(px(8.0))
                // A press anywhere outside the button and its menu closes the menu, and
                // still reaches what was pressed.
                .when(open, |wrap| {
                    wrap.on_mouse_down_out(cx.listener(|this, _, _, cx| this.set_open(false, cx)))
                })
                .children(menu)
                .child(trigger),
        )
        .with_priority(Z_BOTCHECK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rever_ui_kit::i18n::Lang;

    #[test]
    fn 사이트_목록은_원본과_같은_순서와_주소다() {
        let urls: Vec<&str> = SITES.iter().map(|site| site.url).collect();
        assert_eq!(
            urls,
            vec![
                "https://bot.sannysoft.com",
                "https://abrahamjuliot.github.io/creepjs/",
                "https://browserleaks.com/",
                "https://amiunique.org/fingerprint",
                "https://pixelscan.net/",
            ]
        );
        let names: Vec<&str> = SITES.iter().map(|site| site.name).collect();
        assert_eq!(
            names,
            vec![
                "Sannysoft",
                "CreepJS",
                "BrowserLeaks",
                "amiunique",
                "pixelscan"
            ]
        );
    }

    #[test]
    fn 모든_사이트_설명과_버튼_문구가_사전에_있다() {
        // An unknown key comes back as the key itself.
        for site in SITES {
            assert_ne!(Lang::En.t(site.desc_key), site.desc_key);
            assert_ne!(Lang::Ko.t(site.desc_key), site.desc_key);
        }
        for key in [
            "botcheck.menuTitle",
            "botcheck.trigger",
            "botcheck.triggerTitle",
        ] {
            assert_ne!(Lang::En.t(key), key);
        }
        assert_eq!(
            Lang::En.t("botcheck.sannysoft.desc"),
            "Baseline stealth (webdriver / plugins / UA)"
        );
    }

    #[test]
    fn 스프링은_0에서_출발해_1에_멈춘다() {
        assert!(spring(0.0).abs() < 1e-6);
        assert!(spring(-1.0).abs() < 1e-6);
        assert!((spring(SPRING_MS as f32 / 1000.0) - 1.0).abs() < 0.005);
        assert!((spring(5.0) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn 스프링은_살짝_넘쳤다가_돌아온다() {
        // Underdamped: the peak is just above 1, half a damped period in (π / 8 s).
        let peak = (1..400)
            .map(|step| spring(step as f32 / 1000.0))
            .fold(0.0f32, f32::max);
        assert!(peak > 1.0 && peak < 1.01, "peak {peak}");
        assert!(spring(0.1) > 0.4 && spring(0.1) < 0.6);
    }

    #[test]
    fn 열릴_때는_아래에서_올라오며_나타난다() {
        assert_eq!(menu_frame(true, 0.0), (0.0, TRAVEL));
        let (opacity, shift) = menu_frame(true, 1.0);
        assert!((opacity - 1.0).abs() < 0.005 && shift.abs() < 0.05);
        let (opacity, shift) = menu_frame(true, 0.25);
        assert!(opacity > 0.0 && opacity < 1.0 && shift > 0.0 && shift < TRAVEL);
    }

    #[test]
    fn 닫힐_때는_아래로_내려가며_사라진다() {
        assert_eq!(menu_frame(false, 0.0), (1.0, 0.0));
        let (opacity, shift) = menu_frame(false, 1.0);
        assert!(opacity < 0.005 && (shift - TRAVEL).abs() < 0.05);
    }

    #[test]
    fn 넘치는_구간에서도_투명도는_0과_1_사이다() {
        for step in 0..=100 {
            let progress = step as f32 / 100.0;
            for opening in [true, false] {
                let (opacity, _) = menu_frame(opening, progress);
                assert!((0.0..=1.0).contains(&opacity));
            }
        }
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::{Cell, RefCell};

    use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext};

    use super::*;
    use crate::ui::testing::{center, elsewhere, look, stage};

    /// The button, every URL `on_navigate` was given, the presses outside it, the window.
    type Staged<'a> = (
        Entity<BotCheckButton>,
        Rc<RefCell<Vec<String>>>,
        Rc<Cell<usize>>,
        &'a mut VisualTestContext,
    );

    fn button(cx: &mut TestAppContext) -> Staged<'_> {
        let visited: Rc<RefCell<Vec<String>>> = Rc::default();
        let (button, presses, cx) = stage(cx, {
            let visited = visited.clone();
            move |_| {
                BotCheckButton::new(
                    move |url, _, _| visited.borrow_mut().push(url.to_owned()),
                    look(),
                )
            }
        });
        (button, visited, presses, cx)
    }

    fn is_open(button: &Entity<BotCheckButton>, cx: &mut VisualTestContext) -> bool {
        button.read_with(cx, |button, _| button.is_open())
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let at = center(selector, cx);
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn 처음에는_버튼만_보인다(cx: &mut TestAppContext) {
        let (button, _, _, cx) = button(cx);
        assert!(!is_open(&button, cx));
        assert!(cx.debug_bounds("botcheck-menu").is_none());
        assert_eq!(
            cx.debug_bounds("botcheck-trigger").unwrap().size.height,
            px(28.0)
        );
    }

    #[gpui::test]
    fn 버튼을_누른_것은_아래에_깔린_페이지로_가지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (button, _, presses, cx) = button(cx);
        click("botcheck-trigger", cx);
        assert!(is_open(&button, cx));
        assert_eq!(presses.get(), 0);
    }

    #[gpui::test]
    fn 버튼을_누르면_메뉴가_버튼_위에_왼쪽_끝을_맞춰_열린다(
        cx: &mut TestAppContext,
    ) {
        let (button, _, _, cx) = button(cx);
        click("botcheck-trigger", cx);
        assert!(is_open(&button, cx));
        let trigger = cx.debug_bounds("botcheck-trigger").unwrap();
        let menu = cx.debug_bounds("botcheck-menu").unwrap();
        assert_eq!(menu.left(), trigger.left());
        assert!(menu.bottom() <= trigger.top());
        // 280 of content, 8 of padding and 1 of border on each side.
        assert_eq!(menu.size.width, px(298.0));
        // Every site is a row as wide as the menu's inside.
        for selector in [
            "botcheck-item-0",
            "botcheck-item-1",
            "botcheck-item-2",
            "botcheck-item-3",
            "botcheck-item-4",
        ] {
            assert_eq!(cx.debug_bounds(selector).unwrap().size.width, px(280.0));
        }
        // 1 + 8 + 16 + 2 + 16 + 8 + 1: border, padding, name, gap, description, at the
        // 16px line height of a `button`. Measured on the shortest description: the
        // stage is narrow enough to wrap the long ones.
        assert_eq!(
            cx.debug_bounds("botcheck-item-3").unwrap().size.height,
            px(52.0)
        );
    }

    #[gpui::test]
    fn 사이트를_누르면_그_주소로_이동하고_닫힌다(cx: &mut TestAppContext) {
        let (button, visited, _, cx) = button(cx);
        click("botcheck-trigger", cx);
        click("botcheck-item-1", cx);
        assert_eq!(
            *visited.borrow(),
            vec!["https://abrahamjuliot.github.io/creepjs/".to_owned()]
        );
        assert!(!is_open(&button, cx));
    }

    #[gpui::test]
    fn 바깥을_누르면_닫히고_그_누름은_아래에도_전달된다(
        cx: &mut TestAppContext,
    ) {
        let (button, visited, presses, cx) = button(cx);
        click("botcheck-trigger", cx);
        let before = presses.get();
        cx.simulate_mouse_down(elsewhere(), MouseButton::Right, Modifiers::none());
        cx.run_until_parked();
        assert!(!is_open(&button, cx));
        assert_eq!(presses.get(), before + 1);
        assert!(visited.borrow().is_empty());
    }

    #[gpui::test]
    fn 열린_채로_버튼을_다시_누르면_닫힌다(cx: &mut TestAppContext) {
        let (button, _, _, cx) = button(cx);
        click("botcheck-trigger", cx);
        click("botcheck-trigger", cx);
        assert!(!is_open(&button, cx));
        click("botcheck-trigger", cx);
        assert!(is_open(&button, cx));
    }

    #[gpui::test]
    fn esc를_누르면_닫힌다(cx: &mut TestAppContext) {
        let (button, visited, _, cx) = button(cx);
        click("botcheck-trigger", cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!is_open(&button, cx));
        assert!(visited.borrow().is_empty());
    }

    #[gpui::test]
    fn 닫으면_사라지는_동안은_남아_있다가_치워진다(cx: &mut TestAppContext) {
        let (button, visited, _, cx) = button(cx);
        let mounted = |cx: &mut VisualTestContext| button.read_with(cx, |button, _| button.mounted);
        assert!(!mounted(cx));
        click("botcheck-trigger", cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        // Still drawn while it animates out, and its items still take clicks: the React
        // node kept by `AnimatePresence` keeps its `onClick`.
        assert!(mounted(cx));
        assert!(visited.borrow().is_empty());
        click("botcheck-item-0", cx);
        assert_eq!(
            *visited.borrow(),
            vec!["https://bot.sannysoft.com".to_owned()]
        );
        assert!(!is_open(&button, cx));

        cx.executor()
            .advance_clock(Duration::from_millis(SPRING_MS - 1));
        cx.run_until_parked();
        assert!(mounted(cx));
        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        assert!(!mounted(cx));
    }

    #[gpui::test]
    fn 사라지는_동안_다시_열면_치워지지_않는다(cx: &mut TestAppContext) {
        let (button, _, _, cx) = button(cx);
        click("botcheck-trigger", cx);
        click("botcheck-trigger", cx);
        cx.executor()
            .advance_clock(Duration::from_millis(SPRING_MS / 2));
        click("botcheck-trigger", cx);
        cx.executor()
            .advance_clock(Duration::from_millis(SPRING_MS));
        cx.run_until_parked();
        assert!(is_open(&button, cx));
        assert!(button.read_with(cx, |button, _| button.mounted));
    }
}
