//! Port of `src/renderer/src/components/chat/ChipSelect.tsx` — a flat chip in the
//! composer that opens its options upward (it sits at the bottom of the panel).

use std::rc::Rc;

use gpui::{
    App, Context, MouseButton, MouseDownEvent, SharedString, Subscription, Window, deferred, div,
    prelude::*, px, relative, svg,
};

use crate::ui::{LookFn, MONO, Tokens, Z_MENU, on_escape, shadow, text, tooltip};

#[derive(Clone, Debug, PartialEq)]
pub struct ChipOption {
    pub value: SharedString,
    pub label: SharedString,
    pub hint: Option<SharedString>,
}

impl ChipOption {
    pub fn new(value: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            hint: None,
        }
    }

    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// What the React component takes as props, minus `onChange`.
#[derive(Clone)]
pub struct ChipSelectProps {
    /// Name of a kit icon (`icons/<name>.svg`), drawn at 13px in the chip's text color.
    pub icon: Option<&'static str>,
    pub value: SharedString,
    pub options: Vec<ChipOption>,
    pub title: SharedString,
    pub disabled: bool,
    /// The CSS hides the label while the composer is at most 360px wide (a container
    /// query on `.composer`); the host knows that width, the chip does not.
    pub collapsed: bool,
}

/// The chip's text: the selected option's label, or the raw value when no option matches.
pub fn current_label(options: &[ChipOption], value: &str) -> SharedString {
    options
        .iter()
        .find(|option| option.value.as_ref() == value)
        .map_or_else(|| value.to_owned().into(), |option| option.label.clone())
}

const GROUP: &str = "chip-select";

/// Called with the id of the option the user picked.
type OnChange = Rc<dyn Fn(&str, &mut Window, &mut App)>;

pub struct ChipSelect {
    /// Read on every render, as re-rendered props are.
    props: Box<dyn Fn(&App) -> ChipSelectProps>,
    on_change: OnChange,
    look: LookFn,
    open: bool,
    escape: Option<Subscription>,
}

impl ChipSelect {
    pub fn new(
        props: impl Fn(&App) -> ChipSelectProps + 'static,
        on_change: impl Fn(&str, &mut Window, &mut App) + 'static,
        look: LookFn,
    ) -> Self {
        Self {
            props: Box::new(props),
            on_change: Rc::new(on_change),
            look,
            open: false,
            escape: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.open = open;
        self.escape = open.then(|| on_escape(cx, |this, cx| this.set_open(false, cx)));
        cx.notify();
    }
}

impl Render for ChipSelect {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let props = (self.props)(cx);
        let open = self.open;
        let hover = theme.chrome_hover;
        let label = current_label(&props.options, &props.value);
        let color = if open { theme.text } else { theme.text_dim };

        // `.composer-chip`
        let chip = div()
            .id("chip-select")
            .debug_selector(|| "chip-select".into())
            .group(GROUP)
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
            .text_color(color)
            .whitespace_nowrap()
            .when(open, |chip| chip.bg(hover))
            .when(props.disabled, |chip| chip.opacity(0.42))
            .when(!props.disabled, |chip| {
                chip.cursor_pointer()
                    .hover(move |style| style.bg(hover).text_color(theme.text))
                    .on_click(cx.listener(|this, _, _, cx| this.set_open(!this.open, cx)))
            })
            .tooltip(tooltip(props.title.clone(), &tokens))
            .when_some(props.icon, |chip, icon| {
                chip.child(
                    svg()
                        .path(format!("icons/{icon}.svg"))
                        .size(px(13.0))
                        .flex_none()
                        .text_color(color)
                        // An svg does not inherit the chip's hover color.
                        .when(!props.disabled, |icon| {
                            icon.group_hover(GROUP, move |style| style.text_color(theme.text))
                        }),
                )
            })
            .when(!props.collapsed, |chip| {
                chip.child(div().min_w_0().truncate().child(label))
            })
            .child(
                div()
                    .flex_none()
                    // The system font's fallback draws ▾ as a dot at this size.
                    .font_family(MONO)
                    .text_size(px(9.0))
                    .opacity(0.7)
                    .child("▾"),
            );

        let wrap = div().relative().flex().min_w_0().child(chip);
        if !open {
            return wrap;
        }

        let select = cx.weak_entity();
        let on_change = self.on_change.clone();
        let options = props.options.iter().enumerate().map(|(index, option)| {
            let checked = option.value == props.value;
            let value = option.value.clone();
            let (select, on_change) = (select.clone(), on_change.clone());
            // `.chrome-menu-item.composer-menu-item`
            div()
                .id(("chip-option", index))
                .debug_selector(|| format!("chip-option-{index}"))
                .w_full()
                .min_h(px(40.0))
                .px(px(14.0))
                .py(px(6.0))
                .flex()
                .items_center()
                .gap(px(12.0))
                // The option is a `button`, and the base `button` rule sets
                // `line-height: 16px` for everything in it, the 11px hint included.
                .line_height(px(16.0))
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                // Closed before the host hears of it, and not inside a view update: the
                // host's handler may reach back into this chip.
                .on_click(move |_, window, cx| {
                    let _ = select.update(cx, |this, cx| this.set_open(false, cx));
                    on_change(&value, window, cx);
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(div().child(option.label.clone()))
                        .when_some(
                            option.hint.clone().filter(|hint| !hint.is_empty()),
                            |column, hint| {
                                // `.composer-menu-hint`
                                column.child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(theme.text_dim)
                                        .child(hint),
                                )
                            },
                        ),
                )
                .when(checked, |row| {
                    row.child(div().flex_none().text_color(tokens.accent_text).child("✓"))
                })
        });

        // `.chrome-menu.composer-menu`: above the chip, left edges aligned.
        let menu = div()
            .id("chip-select-menu")
            .debug_selector(|| "chip-select-menu".into())
            .absolute()
            // `bottom: calc(100% + 6px)`
            .bottom(relative(1.0))
            .mb(px(6.0))
            .left_0()
            // The CSS box shrinks to its 240px minimum (plus the border), and the hints wrap.
            .w(px(242.0))
            .py(px(6.0))
            .rounded(px(8.0))
            .bg(tokens.surface)
            .border_1()
            .border_color(theme.border)
            .shadow(vec![shadow(4.0, 16.0, tokens.shadow)])
            .text_size(px(13.0))
            .text_color(theme.text)
            .occlude()
            // As in the app menu: a click outside closes the menu and goes no further.
            .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Left {
                    this.set_open(false, cx);
                }
                cx.stop_propagation();
            }))
            .child(
                // `.chrome-menu-foot` with the inline padding of the title line
                text(11.0)
                    .pt(px(2.0))
                    .px(px(14.0))
                    .pb(px(6.0))
                    .text_color(theme.text_dim)
                    .child(props.title.clone()),
            )
            .children(options);

        wrap.child(deferred(menu).with_priority(Z_MENU))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Vec<ChipOption> {
        vec![
            ChipOption::new("auto", "Auto-approve").hint("Runs tools without asking"),
            ChipOption::new("manual", "Ask first"),
        ]
    }

    #[test]
    fn 선택된_값의_라벨을_칩에_보여준다() {
        assert_eq!(current_label(&options(), "manual").as_ref(), "Ask first");
        assert_eq!(current_label(&options(), "auto").as_ref(), "Auto-approve");
    }

    #[test]
    fn 맞는_옵션이_없으면_값을_그대로_보여준다() {
        assert_eq!(current_label(&options(), "recon").as_ref(), "recon");
        assert_eq!(current_label(&[], "").as_ref(), "");
    }

    #[test]
    fn 값이_같은_옵션이_둘이면_앞의_것을_쓴다() {
        let twice = vec![
            ChipOption::new("a", "First"),
            ChipOption::new("a", "Second"),
        ];
        assert_eq!(current_label(&twice, "a").as_ref(), "First");
    }

    #[test]
    fn 옵션_빌더는_힌트를_선택으로_둔다() {
        let all = options();
        assert_eq!(all[0].hint, Some("Runs tools without asking".into()));
        assert_eq!(all[1].hint, None);
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::RefCell;

    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::ui::testing::{center, elsewhere, look, stage};

    /// A select over two options, every value `on_change` was given, the presses outside it, the window.
    type Staged<'a> = (
        Entity<ChipSelect>,
        Rc<RefCell<Vec<String>>>,
        Rc<std::cell::Cell<usize>>,
        &'a mut VisualTestContext,
    );

    fn select(disabled: bool, cx: &mut TestAppContext) -> Staged<'_> {
        let changes: Rc<RefCell<Vec<String>>> = Rc::default();
        let (select, presses, cx) = stage(cx, {
            let changes = changes.clone();
            move |_| {
                ChipSelect::new(
                    move |_| ChipSelectProps {
                        icon: None,
                        value: "auto".into(),
                        options: vec![
                            ChipOption::new("auto", "Auto-approve").hint("Runs tools"),
                            ChipOption::new("manual", "Ask every time"),
                        ],
                        title: "Tool permissions".into(),
                        disabled,
                        collapsed: false,
                    },
                    move |value, _, _| changes.borrow_mut().push(value.to_owned()),
                    look(),
                )
            }
        });
        (select, changes, presses, cx)
    }

    fn is_open(select: &Entity<ChipSelect>, cx: &mut VisualTestContext) -> bool {
        select.read_with(cx, |select, _| select.is_open())
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let at = center(selector, cx);
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn 칩을_누르면_메뉴가_칩_위로_왼쪽_끝을_맞춰_열린다(
        cx: &mut TestAppContext,
    ) {
        let (select, _, _, cx) = select(false, cx);
        assert!(cx.debug_bounds("chip-select-menu").is_none());
        click("chip-select", cx);
        assert!(is_open(&select, cx));
        let chip = cx.debug_bounds("chip-select").unwrap();
        let menu = cx.debug_bounds("chip-select-menu").unwrap();
        assert_eq!(chip.size.height, px(28.0));
        assert_eq!(menu.bottom(), chip.top() - px(6.0));
        assert_eq!(menu.left(), chip.left());
        // The 240px minimum plus the border.
        assert_eq!(menu.size.width, px(242.0));
    }

    #[gpui::test]
    fn 옵션은_높이가_40_이상이고_힌트가_있으면_더_커진다(
        cx: &mut TestAppContext,
    ) {
        let (_, _, _, cx) = select(false, cx);
        click("chip-select", cx);
        let with_hint = cx.debug_bounds("chip-option-0").unwrap();
        let plain = cx.debug_bounds("chip-option-1").unwrap();
        // 6 + 16 + 2 + 16 + 6: padding, label, gap, hint, at the 16px line height of a
        // `button`.
        assert_eq!(with_hint.size.height, px(46.0));
        assert_eq!(plain.size.height, px(40.0));
    }

    #[gpui::test]
    fn 옵션을_누르면_그_값을_알리고_닫힌다(cx: &mut TestAppContext) {
        let (select, changes, _, cx) = select(false, cx);
        click("chip-select", cx);
        click("chip-option-1", cx);
        assert_eq!(*changes.borrow(), vec!["manual".to_owned()]);
        assert!(!is_open(&select, cx));
    }

    #[gpui::test]
    fn 이미_선택된_옵션을_눌러도_값을_알린다(cx: &mut TestAppContext) {
        let (_, changes, _, cx) = select(false, cx);
        click("chip-select", cx);
        click("chip-option-0", cx);
        assert_eq!(*changes.borrow(), vec!["auto".to_owned()]);
    }

    #[gpui::test]
    fn 비활성_칩은_눌러도_열리지_않는다(cx: &mut TestAppContext) {
        let (select, _, _, cx) = select(true, cx);
        click("chip-select", cx);
        assert!(!is_open(&select, cx));
    }

    #[gpui::test]
    fn 바깥을_누르면_닫히고_그_클릭은_아래로_가지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (select, changes, presses, cx) = select(false, cx);
        click("chip-select", cx);
        let before = presses.get();
        cx.simulate_click(elsewhere(), Modifiers::none());
        cx.run_until_parked();
        assert!(!is_open(&select, cx));
        assert_eq!(presses.get(), before);
        assert!(changes.borrow().is_empty());
    }

    #[gpui::test]
    fn 열린_채로_칩을_다시_누르면_닫힌다(cx: &mut TestAppContext) {
        let (select, _, _, cx) = select(false, cx);
        click("chip-select", cx);
        click("chip-select", cx);
        assert!(!is_open(&select, cx));
    }

    #[gpui::test]
    fn esc를_누르면_값을_바꾸지_않고_닫힌다(cx: &mut TestAppContext) {
        let (select, changes, _, cx) = select(false, cx);
        click("chip-select", cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!is_open(&select, cx));
        assert!(changes.borrow().is_empty());
    }
}
