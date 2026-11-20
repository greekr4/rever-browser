//! Port of `src/renderer/src/components/AppMenu.tsx` — Chrome's ⋮ "Customize and
//! control" menu: settings that don't need a permanent button.

use std::rc::Rc;

use gpui::{
    App, Context, MouseButton, MouseDownEvent, SharedString, Subscription, Window, deferred, div,
    prelude::*, px,
};

use crate::ui::{LookFn, Tokens, Z_MENU, on_escape, shadow, text, tooltip};

pub type MenuAction = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub struct AppMenuItem {
    pub label: SharedString,
    /// Current value shown on the right (e.g. "Dark", "EN").
    pub hint: Option<SharedString>,
    pub on_click: MenuAction,
    pub disabled: bool,
    pub title: Option<SharedString>,
    /// Draw a separator above this item.
    pub sep: bool,
}

impl AppMenuItem {
    pub fn new(
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            hint: None,
            on_click: Rc::new(on_click),
            disabled: false,
            title: None,
            sep: false,
        }
    }

    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn sep(mut self) -> Self {
        self.sep = true;
        self
    }
}

/// What the React component takes as props.
#[derive(Clone)]
pub struct AppMenuProps {
    pub items: Vec<AppMenuItem>,
    pub title: SharedString,
    pub footer: Option<SharedString>,
}

/// One line of the open menu, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuRow {
    Sep,
    /// Index into the items.
    Item(usize),
    Foot,
}

/// A string React would render: `{hint && …}` skips an empty one as well as a missing one.
pub fn shown(value: &Option<SharedString>) -> Option<SharedString> {
    value.clone().filter(|text| !text.is_empty())
}

pub fn menu_rows(items: &[AppMenuItem], footer: &Option<SharedString>) -> Vec<MenuRow> {
    let mut rows = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if item.sep {
            rows.push(MenuRow::Sep);
        }
        rows.push(MenuRow::Item(index));
    }
    if shown(footer).is_some() {
        rows.extend([MenuRow::Sep, MenuRow::Foot]);
    }
    rows
}

pub struct AppMenu {
    /// Read on every render, so labels and hints follow the host's state the way
    /// re-rendered props do.
    props: Box<dyn Fn(&App) -> AppMenuProps>,
    look: LookFn,
    open: bool,
    escape: Option<Subscription>,
}

impl AppMenu {
    pub fn new(props: impl Fn(&App) -> AppMenuProps + 'static, look: LookFn) -> Self {
        Self {
            props: Box::new(props),
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

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        self.set_open(!self.open, cx);
    }
}

/// The three dots of the trigger: circles of radius 1.8 at y = 5, 12, 19 in a 24-unit
/// box, drawn at 18px.
fn dots(color: gpui::Rgba) -> impl IntoElement {
    div()
        .size(px(18.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(2.55))
        .children((0..3).map(move |_| div().size(px(2.7)).rounded_full().bg(color)))
}

impl Render for AppMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let props = (self.props)(cx);
        let open = self.open;
        let hover = theme.chrome_hover;

        // `.chrome-icon-btn`
        let button = div()
            .id("app-menu-button")
            .debug_selector(|| "app-menu-button".into())
            .size(px(32.0))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .when(open, |button| button.bg(hover))
            .hover(move |style| style.bg(hover))
            .tooltip(tooltip(props.title.clone(), &tokens))
            .on_click(cx.listener(|this, _, _, cx| this.toggle(cx)))
            .child(dots(theme.text_2));

        let wrap = div().relative().flex_none().child(button);
        if !open {
            return wrap;
        }

        let menu_id = cx.entity_id();
        let rows = menu_rows(&props.items, &props.footer)
            .into_iter()
            .filter_map(|row| match row {
                // `.chrome-menu-sep`
                MenuRow::Sep => Some(
                    div()
                        .h(px(1.0))
                        .my(px(6.0))
                        .bg(theme.border)
                        .into_any_element(),
                ),
                // `.chrome-menu-foot`
                MenuRow::Foot => Some(
                    text(11.0)
                        .pt(px(6.0))
                        .px(px(16.0))
                        .pb(px(2.0))
                        .text_color(theme.text_dim)
                        .child(shown(&props.footer)?)
                        .into_any_element(),
                ),
                // `.chrome-menu-item`
                MenuRow::Item(index) => {
                    let item = props.items.get(index)?;
                    let on_click = item.on_click.clone();
                    Some(
                        div()
                            // Index ids: labels change with the language.
                            .id(("app-menu-item", index))
                            .debug_selector(|| format!("app-menu-item-{index}"))
                            .w_full()
                            .h(px(32.0))
                            .px(px(16.0))
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .when(item.disabled, |row| row.opacity(0.42))
                            .when(!item.disabled, |row| {
                                row.cursor_pointer()
                                    .hover(move |style| style.bg(hover))
                                    // Menu stays open so cycling items (theme, language) can be
                                    // clicked repeatedly. Not a view listener: the host's action
                                    // may reach back into this menu.
                                    .on_click(move |_, window, cx| {
                                        on_click(window, cx);
                                        cx.notify(menu_id);
                                    })
                            })
                            .when_some(shown(&item.title), |row, title| {
                                row.tooltip(tooltip(title, &tokens))
                            })
                            .child(item.label.clone())
                            .when_some(shown(&item.hint), |row, hint| {
                                row.child(
                                    div()
                                        .ml_auto()
                                        .text_size(px(12.0))
                                        .text_color(theme.text_dim)
                                        .child(hint),
                                )
                            })
                            .into_any_element(),
                    )
                }
            });

        // `.chrome-menu`: under the button, right edges aligned (top = bottom + 4).
        let menu = div()
            .id("app-menu")
            .debug_selector(|| "app-menu".into())
            .absolute()
            .top(px(36.0))
            .right_0()
            // 260 of content plus the border: the CSS box is content-box.
            .min_w(px(262.0))
            .py(px(6.0))
            .rounded(px(8.0))
            .bg(tokens.surface)
            .border_1()
            .border_color(theme.border)
            .shadow(vec![shadow(4.0, 16.0, tokens.shadow)])
            .text_size(px(13.0))
            .text_color(theme.text)
            .whitespace_nowrap()
            .occlude()
            // The React menu sits on a full-window layer that takes every click outside it:
            // the click closes the menu and does not reach what is underneath.
            .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Left {
                    this.set_open(false, cx);
                }
                cx.stop_propagation();
            }))
            .children(rows);

        wrap.child(deferred(menu).with_priority(Z_MENU))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &'static str) -> AppMenuItem {
        AppMenuItem::new(label, |_, _| {})
    }

    #[test]
    fn 항목은_순서대로_한_줄씩_나온다() {
        let rows = menu_rows(&[item("Theme"), item("Language")], &None);
        assert_eq!(rows, vec![MenuRow::Item(0), MenuRow::Item(1)]);
    }

    #[test]
    fn sep이_켜진_항목_위에_구분선을_그린다() {
        let rows = menu_rows(&[item("Theme"), item("Viewport").sep()], &None);
        assert_eq!(rows, vec![MenuRow::Item(0), MenuRow::Sep, MenuRow::Item(1)]);
    }

    #[test]
    fn 첫_항목의_sep도_구분선을_그린다() {
        let rows = menu_rows(&[item("Theme").sep()], &None);
        assert_eq!(rows, vec![MenuRow::Sep, MenuRow::Item(0)]);
    }

    #[test]
    fn 푸터가_있으면_구분선과_푸터_줄을_끝에_붙인다() {
        let rows = menu_rows(&[item("Theme")], &Some("Rever Browser v1.2.3".into()));
        assert_eq!(rows, vec![MenuRow::Item(0), MenuRow::Sep, MenuRow::Foot]);
    }

    #[test]
    fn 빈_푸터는_그리지_않는다() {
        assert_eq!(
            menu_rows(&[item("Theme")], &Some("".into())),
            vec![MenuRow::Item(0)]
        );
        assert_eq!(menu_rows(&[], &None), vec![]);
    }

    #[test]
    fn 빈_힌트와_없는_힌트는_보이지_않는다() {
        assert_eq!(shown(&None), None);
        assert_eq!(shown(&Some("".into())), None);
        assert_eq!(shown(&Some("Dark".into())), Some("Dark".into()));
    }

    #[test]
    fn 항목_빌더는_기본값에서_필요한_것만_바꾼다() {
        let plain = item("Theme");
        assert!(!plain.disabled && !plain.sep);
        assert!(plain.hint.is_none() && plain.title.is_none());

        let full = item("View source")
            .hint("EN")
            .title("Open the page source")
            .disabled(true)
            .sep();
        assert_eq!(full.hint, Some("EN".into()));
        assert_eq!(full.title, Some("Open the page source".into()));
        assert!(full.disabled && full.sep);
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::Cell;

    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::ui::testing::{center, elsewhere, look, stage};

    /// A menu of an enabled item, a disabled one and a separated one; the counter
    /// holds how often each was chosen.
    /// The menu, how often each item was chosen, the presses outside it, the window.
    type Staged<'a> = (
        Entity<AppMenu>,
        Rc<[Cell<usize>; 3]>,
        Rc<Cell<usize>>,
        &'a mut VisualTestContext,
    );

    fn menu(cx: &mut TestAppContext) -> Staged<'_> {
        let chosen: Rc<[Cell<usize>; 3]> = Rc::default();
        let (menu, presses, cx) = stage(cx, {
            let chosen = chosen.clone();
            move |_| {
                AppMenu::new(
                    move |_| {
                        let count = |index: usize| {
                            let chosen = chosen.clone();
                            move |_: &mut Window, _: &mut App| {
                                chosen[index].set(chosen[index].get() + 1)
                            }
                        };
                        AppMenuProps {
                            title: "Customize".into(),
                            footer: Some("Rever Browser v1.0.0".into()),
                            items: vec![
                                AppMenuItem::new("Theme", count(0)).hint("Light"),
                                AppMenuItem::new("View source", count(1)).disabled(true),
                                AppMenuItem::new("Viewport", count(2)).sep(),
                            ],
                        }
                    },
                    look(),
                )
            }
        });
        (menu, chosen, presses, cx)
    }

    fn is_open(menu: &Entity<AppMenu>, cx: &mut VisualTestContext) -> bool {
        menu.read_with(cx, |menu, _| menu.is_open())
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let at = center(selector, cx);
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn 처음에는_닫혀_있고_버튼만_보인다(cx: &mut TestAppContext) {
        let (menu, _, _, cx) = menu(cx);
        assert!(!is_open(&menu, cx));
        assert!(cx.debug_bounds("app-menu").is_none());
        let button = cx.debug_bounds("app-menu-button").unwrap();
        assert_eq!(button.size, gpui::size(px(32.0), px(32.0)));
    }

    #[gpui::test]
    fn 버튼을_누르면_버튼_아래에_오른쪽_끝을_맞춰_열린다(
        cx: &mut TestAppContext,
    ) {
        let (menu, _, _, cx) = menu(cx);
        click("app-menu-button", cx);
        assert!(is_open(&menu, cx));
        let button = cx.debug_bounds("app-menu-button").unwrap();
        let popup = cx.debug_bounds("app-menu").unwrap();
        assert_eq!(popup.top(), button.bottom() + px(4.0));
        assert_eq!(popup.right(), button.right());
        // The 260px minimum plus the border.
        assert_eq!(popup.size.width, px(262.0));
        let item = cx.debug_bounds("app-menu-item-0").unwrap();
        assert_eq!(item.size.height, px(32.0));
        assert_eq!(item.size.width, px(260.0));
    }

    #[gpui::test]
    fn 항목을_눌러도_메뉴는_열린_채로_남는다(cx: &mut TestAppContext) {
        let (menu, chosen, _, cx) = menu(cx);
        click("app-menu-button", cx);
        click("app-menu-item-0", cx);
        click("app-menu-item-0", cx);
        click("app-menu-item-2", cx);
        assert_eq!(chosen[0].get(), 2);
        assert_eq!(chosen[2].get(), 1);
        assert!(is_open(&menu, cx));
    }

    #[gpui::test]
    fn 비활성_항목은_눌러도_아무_일도_없다(cx: &mut TestAppContext) {
        let (menu, chosen, _, cx) = menu(cx);
        click("app-menu-button", cx);
        click("app-menu-item-1", cx);
        assert_eq!(chosen[1].get(), 0);
        assert!(is_open(&menu, cx));
    }

    #[gpui::test]
    fn 바깥을_누르면_닫히고_그_클릭은_아래로_가지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (menu, _, presses, cx) = menu(cx);
        click("app-menu-button", cx);
        let before = presses.get();
        cx.simulate_click(elsewhere(), Modifiers::none());
        cx.run_until_parked();
        assert!(!is_open(&menu, cx));
        assert_eq!(presses.get(), before);
        // Closed, the same click goes through again.
        cx.simulate_click(elsewhere(), Modifiers::none());
        assert_eq!(presses.get(), before + 1);
    }

    #[gpui::test]
    fn 열린_채로_버튼을_다시_누르면_닫힌다(cx: &mut TestAppContext) {
        let (menu, _, _, cx) = menu(cx);
        click("app-menu-button", cx);
        click("app-menu-button", cx);
        assert!(!is_open(&menu, cx));
        click("app-menu-button", cx);
        assert!(is_open(&menu, cx));
    }

    #[gpui::test]
    fn 바깥의_오른쪽_버튼은_닫지_않지만_아래로_가지도_않는다(
        cx: &mut TestAppContext,
    ) {
        let (menu, _, presses, cx) = menu(cx);
        click("app-menu-button", cx);
        let before = presses.get();
        cx.simulate_mouse_down(elsewhere(), MouseButton::Right, Modifiers::none());
        cx.run_until_parked();
        assert!(is_open(&menu, cx));
        assert_eq!(presses.get(), before);
    }

    #[gpui::test]
    fn 메뉴_안의_클릭은_아래로_가지_않는다(cx: &mut TestAppContext) {
        let (_, _, presses, cx) = menu(cx);
        click("app-menu-button", cx);
        let before = presses.get();
        click("app-menu-item-0", cx);
        assert_eq!(presses.get(), before);
    }

    #[gpui::test]
    fn esc를_누르면_닫히고_닫힌_뒤에는_esc를_듣지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (menu, _, _, cx) = menu(cx);
        click("app-menu-button", cx);
        cx.simulate_keystrokes("a");
        assert!(is_open(&menu, cx));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!is_open(&menu, cx));
        assert!(menu.read_with(cx, |menu, _| menu.escape.is_none()));
    }
}
