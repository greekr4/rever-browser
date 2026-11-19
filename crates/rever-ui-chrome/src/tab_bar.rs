//! Port of `src/renderer/src/components/TabBar.tsx` — the tab strip: one tab per page,
//! the "+" button, and a "⋯ N" menu holding the tabs that do not fit.
//!
//! The tabs come from the host on every render ([`TabBarProps`], what the component
//! read from the tabs and profiles stores); select, close and new-tab go back as
//! [`TabBarEvent`]s.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{
    App, Context, EventEmitter, MouseButton, MouseDownEvent, SharedString, Window, canvas,
    deferred, div, prelude::*, px, rgba, svg,
};
use rever_ui_state::tabs::{ProxyConfig, Tab};

use crate::ui::{
    LookFn, SANS, Tokens, clickable, ellipsis, natural_width, shadow, single_line, tooltip,
};

/// Where the "+" button's new tab starts.
pub const NEW_TAB_URL: &str = "https://www.google.com";

/// The + button lives inside the strip (Chrome puts it right after the last tab).
const PLUS_RESERVE: f32 = 36.0;
/// The width assumed for a tab that has not been on screen yet.
const UNMEASURED: f32 = 140.0;
/// Above the page, below modals: `z-index: 41` among the app's layers.
const Z_MENU: usize = 1;

/// A named profile, for the label on its tabs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabProfile {
    pub id: String,
    pub name: String,
}

/// What the React component reads from the tabs and profiles stores.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabBarProps {
    pub tabs: Vec<Tab>,
    pub active_id: String,
    pub profiles: Vec<TabProfile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabBarEvent {
    /// `selectTab(id)`
    Select(String),
    /// `closeTab(id)`
    Close(String),
    /// `addTab(url)`
    NewTab(String),
}

/// Chrome-style overflow: the ids of the tabs that don't fit a strip `strip_width`
/// wide, in tab order. `widths` are the measured tab widths.
pub fn overflow_ids<'a>(
    tab_ids: impl IntoIterator<Item = &'a str>,
    active_id: &str,
    widths: &HashMap<String, f32>,
    strip_width: f32,
) -> Vec<String> {
    let width = |id: &str| widths.get(id).copied().unwrap_or(UNMEASURED);
    let avail = strip_width - PLUS_RESERVE;
    // The active tab is always kept visible — reserve its width first.
    let mut used = if active_id.is_empty() {
        0.0
    } else {
        width(active_id)
    };
    let mut overflow = Vec::new();
    for id in tab_ids {
        if id == active_id {
            continue;
        }
        let w = width(id);
        if used + w <= avail {
            used += w;
        } else {
            overflow.push(id.to_owned());
        }
    }
    overflow
}

/// Record widths of currently-mounted tabs; overflow tabs are unmounted from the
/// strip, so their last-measured width is kept (prune only closed tabs).
pub fn record_widths<'a>(
    widths: &mut HashMap<String, f32>,
    measured: impl IntoIterator<Item = (String, f32)>,
    live: impl IntoIterator<Item = &'a str>,
) {
    for (id, width) in measured {
        // `offsetWidth` is a whole number.
        widths.insert(id, width.round());
    }
    let live: HashSet<&str> = live.into_iter().collect();
    widths.retain(|id, _| live.contains(id.as_str()));
}

/// The label of a tab's profile: none for the default profile, "Profile" for one the
/// store does not know (yet).
pub fn profile_name(profile_id: Option<&str>, profiles: &[TabProfile]) -> Option<String> {
    let id = profile_id.filter(|id| !id.is_empty() && *id != "default")?;
    let name = profiles
        .iter()
        .find(|profile| profile.id == id)
        .map_or("Profile", |profile| profile.name.as_str());
    // `{label && …}`: an empty name draws no label.
    (!name.is_empty()).then(|| name.to_owned())
}

/// Ephemeral partition (proxy / incognito) — cookies vanish with the app.
/// Persistent profile tabs (persist:…) render like normal tabs.
pub fn is_ephemeral(partition: Option<&str>) -> bool {
    partition.is_some_and(|partition| !partition.is_empty() && !partition.starts_with("persist:"))
}

/// The tooltip of an isolated tab's dot.
pub fn dot_title(proxy: Option<&ProxyConfig>) -> String {
    match proxy.filter(|proxy| proxy.enabled) {
        Some(proxy) => format!(
            "Isolated proxy tab — {}:{}, own cookies",
            proxy.host, proxy.port
        ),
        None => "Isolated tab — own cookies (proxy disabled)".to_owned(),
    }
}

/// `t.title || t.url`
pub fn tab_label(tab: &Tab) -> &str {
    if tab.title.is_empty() {
        &tab.url
    } else {
        &tab.title
    }
}

/// How wide a tab is: its content (12px of padding, the dot, the profile label, the
/// title, the close button, 8px of padding, 8px apart) between 120 and 240px.
/// `label_width` is the label's whole box.
pub fn tab_width(
    title_width: f32,
    label_width: Option<f32>,
    ephemeral: bool,
    closable: bool,
) -> f32 {
    let dot = if ephemeral { 6.0 + 8.0 } else { 0.0 };
    let label = label_width.map_or(0.0, |width| width + 8.0);
    let close = if closable { 8.0 + 18.0 } else { 0.0 };
    (12.0 + dot + label + title_width + close + 8.0).clamp(120.0, 240.0)
}

/// The profile label's box: its text and 5px of padding each side, at most the 70px
/// of `max-width` plus that padding.
pub fn label_width(text_width: f32) -> f32 {
    (text_width + 10.0).min(80.0)
}

/// The "⋯" menu's content width: its widest row (a title with 8px of padding each
/// side, and the 20px close button 4px away when there is one) between 220 and 320px.
pub fn menu_width(widest_title: f32, closable: bool) -> f32 {
    let close = if closable { 4.0 + 20.0 } else { 0.0 };
    (widest_title + 16.0 + close).clamp(220.0, 320.0)
}

type Measured = Rc<RefCell<Vec<(String, f32)>>>;

/// An invisible child that reports how wide its (relative) parent was laid out.
fn measure(measured: &Measured, id: String) -> impl IntoElement + use<> {
    let measured = measured.clone();
    canvas(
        move |bounds, _, _| measured.borrow_mut().push((id, bounds.size.width.into())),
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .bottom_0()
    .left_0()
    .right_0()
}

pub struct TabBar {
    /// Read on every render, the way the component re-renders on a store change.
    props: Box<dyn Fn(&App) -> TabBarProps>,
    look: LookFn,
    /// Last measured width of each open tab.
    widths: HashMap<String, f32>,
    overflow_ids: Vec<String>,
    menu_open: bool,
    /// The tab the middle button went down on: a middle click closes only when it
    /// comes up on the same tab (`auxclick`).
    middle_down: Option<String>,
    /// Tab widths of the frame being drawn.
    measured: Measured,
}

impl EventEmitter<TabBarEvent> for TabBar {}

impl TabBar {
    pub fn new(props: impl Fn(&App) -> TabBarProps + 'static, look: LookFn) -> Self {
        Self {
            props: Box::new(props),
            look,
            widths: HashMap::new(),
            overflow_ids: Vec::new(),
            menu_open: false,
            middle_down: None,
            measured: Measured::default(),
        }
    }

    /// The tabs currently moved into the "⋯" menu.
    pub fn overflow_ids(&self) -> &[String] {
        &self.overflow_ids
    }

    pub fn is_menu_open(&self) -> bool {
        self.menu_open
    }

    /// `toggleTabMenu`: what a click on the "⋯ N" button does. The menu shows only
    /// while there are hidden tabs to list.
    pub fn toggle_menu(&mut self, cx: &mut Context<Self>) {
        self.menu_open = !self.menu_open;
        cx.notify();
    }

    /// Recompute which tabs fit, from the widths the last frame was drawn with. The
    /// strip shrinks when the "⋯" button appears, so this settles in a frame or two.
    fn fit(&mut self, measured: Vec<(String, f32)>, strip_width: f32, cx: &mut Context<Self>) {
        let props = (self.props)(cx);
        let ids = || props.tabs.iter().map(|tab| tab.id.as_str());
        record_widths(&mut self.widths, measured, ids());
        // `clientWidth` is a whole number.
        let next = overflow_ids(ids(), &props.active_id, &self.widths, strip_width.round());
        if next != self.overflow_ids {
            self.overflow_ids = next;
            cx.notify();
        }
    }

    fn select(&mut self, id: &str, cx: &mut Context<Self>) {
        cx.emit(TabBarEvent::Select(id.to_owned()));
    }

    fn close(&mut self, id: &str, cx: &mut Context<Self>) {
        cx.emit(TabBarEvent::Close(id.to_owned()));
    }

    fn render_tab(
        &self,
        tab: &Tab,
        props: &TabBarProps,
        tokens: &Tokens,
        close_title: &'static str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = tokens.theme;
        let is_active = tab.id == props.active_id;
        let ephemeral = is_ephemeral(tab.partition.as_deref());
        let closable = props.tabs.len() > 1;
        let label = profile_name(tab.profile_id.as_deref(), &props.profiles)
            .map(|label| single_line(&label))
            .map(|label| {
                let width = label_width(natural_width(&label, 10.0, window));
                (label, width)
            });
        let title = single_line(tab_label(tab));
        // GPUI cuts a text with an ellipsis only in a box of a set width, so the tab
        // gets the width CSS would work out from its content.
        let width = tab_width(
            natural_width(&title, 12.0, window),
            label.as_ref().map(|(_, width)| *width),
            ephemeral,
            closable,
        );
        let hover = theme.chrome_hover;
        let id = || tab.id.clone();
        let (click_id, down_id, out_id, up_id, close_id) = (id(), id(), id(), id(), id());

        clickable(SharedString::from(format!("tab-{}", tab.id)))
            .relative()
            .h(px(34.0))
            .w(px(width))
            .pl(px(12.0))
            .pr(px(8.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            // Chrome: the active tab is the toolbar's colour and flows into it.
            .rounded_t(px(8.0))
            .text_size(px(12.0))
            .line_height(px(14.0))
            .map(|el| match (ephemeral, is_active) {
                (true, _) => el
                    .bg(tokens.proxy_tab_bg)
                    .text_color(tokens.proxy_tab_text)
                    .when(!is_active, |el| el.opacity(0.75)),
                (false, true) => el.bg(theme.chrome_toolbar).text_color(theme.text),
                // `.chrome-tab`
                (false, false) => el
                    .text_color(theme.text_2)
                    .hover(move |style| style.bg(hover)),
            })
            .on_click(cx.listener(move |this, _, _, cx| this.select(&click_id, cx)))
            // Middle-click close.
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, _, _| this.middle_down = Some(down_id.clone())),
            )
            .on_mouse_down_out(cx.listener(move |this, _, _, _| {
                if this.middle_down.as_deref() == Some(out_id.as_str()) {
                    this.middle_down = None;
                }
            }))
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _, _, cx| {
                    if this.middle_down.take().as_deref() == Some(up_id.as_str()) {
                        this.close(&up_id, cx);
                    }
                }),
            )
            .when(ephemeral, |el| {
                el.child(
                    clickable(SharedString::from(format!("tab-dot-{}", tab.id)))
                        .size(px(6.0))
                        .flex_none()
                        .rounded(px(3.0))
                        .bg(tokens.proxy_tab_dot)
                        .tooltip(tooltip(dot_title(tab.proxy.as_ref()), tokens)),
                )
            })
            .when_some(label, |el, (label, width)| {
                el.child(
                    clickable(SharedString::from(format!("tab-profile-{}", tab.id)))
                        .flex()
                        .flex_none()
                        .w(px(width))
                        .px(px(5.0))
                        .py(px(1.0))
                        .rounded(px(3.0))
                        .bg(theme.border_2)
                        .text_color(theme.text_dim)
                        .text_size(px(10.0))
                        .line_height(px(12.0))
                        .tooltip(tooltip(format!("Profile: {label}"), tokens))
                        .child(ellipsis(label, 12.0).flex_1()),
                )
            })
            .child(
                ellipsis(title, 14.0)
                    .id("title")
                    .flex_1()
                    .min_w_0()
                    .tooltip(tooltip(tab.url.clone(), tokens)),
            )
            .when(closable, |el| {
                let text = theme.text;
                el.child(
                    // `.chrome-tab-close`
                    clickable(SharedString::from(format!("tab-close-{}", tab.id)))
                        .size(px(18.0))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .text_color(theme.text_dim)
                        .text_size(px(14.0))
                        .line_height(px(14.0))
                        .hover(move |style| style.bg(hover).text_color(text))
                        .tooltip(tooltip(close_title, tokens))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.close(&close_id, cx);
                        }))
                        .child("×"),
                )
            })
            .child(measure(&self.measured, tab.id.clone()))
    }

    /// The list under the "⋯" button.
    fn render_menu(
        &self,
        overflow_tabs: &[&Tab],
        closable: bool,
        tokens: &Tokens,
        close_title: &'static str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = tokens.theme;
        let titles: Vec<String> = overflow_tabs
            .iter()
            .map(|tab| single_line(tab_label(tab)))
            .collect();
        let widest = titles
            .iter()
            .map(|title| natural_width(title, 12.0, window))
            .fold(0.0, f32::max);
        let rows = overflow_tabs.iter().zip(titles).map(|(tab, title)| {
            let (select_id, close_id) = (tab.id.clone(), tab.id.clone());
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(4.0))
                .child(
                    clickable(SharedString::from(format!("tab-menu-item-{}", tab.id)))
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .px(px(8.0))
                        .py(px(6.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .tooltip(tooltip(tab.url.clone(), tokens))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.menu_open = false;
                            this.select(&select_id, cx);
                            cx.notify();
                        }))
                        .child(ellipsis(title, 16.0).flex_1()),
                )
                .when(closable, |row| {
                    row.child(
                        clickable(SharedString::from(format!("tab-menu-close-{}", tab.id)))
                            .size(px(20.0))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(theme.text_dim)
                            .text_size(px(13.0))
                            .tooltip(tooltip(close_title, tokens))
                            // The menu stays open: several tabs can be closed in a row.
                            .on_click(cx.listener(move |this, _, _, cx| this.close(&close_id, cx)))
                            .child("×"),
                    )
                })
        });
        // The CSS boxes are content-box: the 4px padding and the border come on top of
        // the 220–320px width and the 70vh height.
        let chrome = px(10.0);
        clickable("tab-menu")
            .absolute()
            // Under the button (28px tall), right edges aligned: top = bottom + 4.
            .top(px(32.0))
            .right_0()
            .w(px(menu_width(widest, closable)) + chrome)
            .max_h(window.viewport_size().height * 0.7 + chrome)
            // A flex column, not a block: see `ellipsis`.
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .p(px(4.0))
            .rounded(px(6.0))
            .bg(theme.bg)
            .border_1()
            .border_color(theme.border_2)
            // 0 4px 16px rgba(0,0,0,0.3)
            .shadow(vec![shadow(4.0, 16.0, rgba(0x0000004d))])
            .text_color(theme.text)
            .text_size(px(12.0))
            .line_height(px(16.0))
            .occlude()
            // The React menu sits on a full-window layer that takes every click outside
            // it: the click closes the menu and does not reach what is underneath.
            .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Left {
                    this.menu_open = false;
                    cx.notify();
                }
                cx.stop_propagation();
            }))
            .children(rows)
    }
}

impl Render for TabBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let props = (self.props)(cx);
        let close_title = look.lang.t("tab.close");
        let hover = theme.chrome_hover;

        let overflow: HashSet<&str> = self.overflow_ids.iter().map(String::as_str).collect();
        let (overflow_tabs, strip_tabs): (Vec<&Tab>, Vec<&Tab>) = props
            .tabs
            .iter()
            .partition(|tab| overflow.contains(tab.id.as_str()));

        self.measured.borrow_mut().clear();
        let tabs: Vec<_> = strip_tabs
            .iter()
            .map(|tab| self.render_tab(tab, &props, &tokens, close_title, window, cx))
            .collect();

        let (bar, measured) = (cx.entity(), self.measured.clone());
        // Last child: the tabs before it have reported their widths by then.
        let fit = canvas(
            move |bounds, _, cx| {
                let tabs = std::mem::take(&mut *measured.borrow_mut());
                let strip_width: f32 = bounds.size.width.into();
                // After the frame: a view cannot ask for a redraw while it is drawn.
                cx.defer(move |cx| bar.update(cx, |bar, cx| bar.fit(tabs, strip_width, cx)));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .bottom_0()
        .left_0()
        .right_0();

        // Tab strip — clips overflow (no scrollbar); extra tabs go to the ⋯ menu
        let strip = div()
            .debug_selector(|| "tab-strip".into())
            .relative()
            .flex()
            .flex_auto()
            .items_end()
            .gap(px(2.0))
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .children(tabs)
            .child(
                // `.chrome-icon-btn.small`
                clickable("new-tab")
                    .size(px(28.0))
                    .ml(px(4.0))
                    .mb(px(3.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .tooltip(tooltip(look.lang.t("tab.new"), &tokens))
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(TabBarEvent::NewTab(NEW_TAB_URL.to_owned()))
                    }))
                    .child(
                        svg()
                            .path("icons/plus.svg")
                            .size(px(16.0))
                            .flex_none()
                            .text_color(theme.text_2),
                    ),
            )
            .child(fit);

        let root = div()
            .flex()
            .flex_1()
            .items_end()
            .gap(px(2.0))
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .font_family(SANS)
            .child(strip);
        if overflow_tabs.is_empty() {
            return root;
        }

        // `.chrome-chip`
        let chip = clickable("tab-overflow")
            .h(px(28.0))
            .mb(px(3.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .rounded(px(14.0))
            .text_color(theme.text_dim)
            .text_size(px(12.0))
            .line_height(px(14.0))
            .whitespace_nowrap()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .tooltip(tooltip(look.lang.t("tab.overflow"), &tokens))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(cx)))
            .child(format!("⋯ {}", overflow_tabs.len()));
        let wrap = div().relative().flex_none().child(chip);
        if !self.menu_open {
            return root.child(wrap);
        }
        let menu = self.render_menu(
            &overflow_tabs,
            props.tabs.len() > 1,
            &tokens,
            close_title,
            window,
            cx,
        );
        root.child(wrap.child(deferred(menu).with_priority(Z_MENU)))
    }
}

#[cfg(test)]
mod tests {
    use rever_ui_state::tabs::ProxyScheme;

    use super::*;

    fn widths(entries: &[(&str, f32)]) -> HashMap<String, f32> {
        entries
            .iter()
            .map(|(id, width)| ((*id).to_owned(), *width))
            .collect()
    }

    fn tab(id: &str) -> Tab {
        Tab {
            id: id.into(),
            url: format!("https://{id}.test/"),
            title: format!("Tab {id}"),
            web_contents_id: None,
            initial_url: format!("https://{id}.test/"),
            proxy: None,
            partition: None,
            profile_id: None,
        }
    }

    const IDS: [&str; 4] = ["t1", "t2", "t3", "t4"];

    #[test]
    fn 모두_들어가면_넘치는_탭이_없다() {
        let w = widths(&[("t1", 120.0), ("t2", 120.0), ("t3", 120.0), ("t4", 120.0)]);
        assert!(overflow_ids(IDS, "t1", &w, 600.0).is_empty());
    }

    #[test]
    fn 더하기_버튼_자리_36px는_탭이_쓰지_못한다() {
        let w = widths(&[("t1", 120.0), ("t2", 120.0), ("t3", 120.0), ("t4", 120.0)]);
        // 480 of tabs fit 516 exactly (`<=`), not 515.
        assert!(overflow_ids(IDS, "t1", &w, 516.0).is_empty());
        assert_eq!(overflow_ids(IDS, "t1", &w, 515.0), ["t4"]);
    }

    #[test]
    fn 활성_탭은_맨_뒤에_있어도_항상_보인다() {
        let w = widths(&[("t1", 120.0), ("t2", 120.0), ("t3", 120.0), ("t4", 120.0)]);
        // Room for two tabs: the active one and the first.
        assert_eq!(overflow_ids(IDS, "t4", &w, 290.0), ["t2", "t3"]);
        // Not even room for the active tab alone: everything else overflows, it stays.
        assert_eq!(overflow_ids(IDS, "t4", &w, 100.0), ["t1", "t2", "t3"]);
    }

    #[test]
    fn 넓은_탭이_넘쳐도_뒤의_좁은_탭은_들어갈_수_있다() {
        let w = widths(&[("t1", 120.0), ("t2", 240.0), ("t3", 120.0), ("t4", 240.0)]);
        // 264 available: t1 (active) + t3 fit, the two wide ones do not.
        assert_eq!(overflow_ids(IDS, "t1", &w, 300.0), ["t2", "t4"]);
    }

    #[test]
    fn 아직_재지_않은_탭은_140px로_친다() {
        let w = widths(&[("t1", 120.0)]);
        // 120 + 140 + 140 = 400 fits 436; the fourth does not.
        assert_eq!(overflow_ids(IDS, "t1", &w, 436.0), ["t4"]);
        assert_eq!(overflow_ids(IDS, "t1", &w, 435.0), ["t3", "t4"]);
    }

    #[test]
    fn 활성_id가_비면_자리를_잡지_않고_목록에_없어도_잡는다() {
        let w = widths(&[("t1", 120.0), ("t2", 120.0), ("t3", 120.0), ("t4", 120.0)]);
        assert_eq!(overflow_ids(IDS, "", &w, 396.0), ["t4"]);
        // A stale active id still reserves the unmeasured 140.
        assert_eq!(overflow_ids(IDS, "gone", &w, 396.0), ["t2", "t3", "t4"]);
    }

    #[test]
    fn 탭이_없으면_넘치는_것도_없다() {
        assert!(overflow_ids([], "t1", &HashMap::new(), 0.0).is_empty());
    }

    #[test]
    fn 잰_너비는_정수로_기록하고_닫힌_탭만_지운다() {
        let mut w = widths(&[("t1", 120.0), ("t2", 200.0), ("gone", 150.0)]);
        // t2 is in the overflow menu now (not measured): its last width is kept.
        record_widths(
            &mut w,
            [("t1".to_owned(), 133.4), ("t3".to_owned(), 180.5)],
            ["t1", "t2", "t3"],
        );
        assert_eq!(w, widths(&[("t1", 133.0), ("t2", 200.0), ("t3", 181.0)]));
    }

    #[test]
    fn 기본_프로필과_프로필_없는_탭에는_라벨이_없다() {
        let profiles = [TabProfile {
            id: "p1".into(),
            name: "Work".into(),
        }];
        assert_eq!(profile_name(None, &profiles), None);
        assert_eq!(profile_name(Some(""), &profiles), None);
        assert_eq!(profile_name(Some("default"), &profiles), None);
    }

    #[test]
    fn 프로필_이름을_라벨로_쓰고_모르는_프로필은_profile이다() {
        let profiles = [
            TabProfile {
                id: "p1".into(),
                name: "Work".into(),
            },
            TabProfile {
                id: "p2".into(),
                name: String::new(),
            },
        ];
        assert_eq!(profile_name(Some("p1"), &profiles).as_deref(), Some("Work"));
        assert_eq!(
            profile_name(Some("p9"), &profiles).as_deref(),
            Some("Profile")
        );
        // A profile with an empty name draws no label.
        assert_eq!(profile_name(Some("p2"), &profiles), None);
    }

    #[test]
    fn persist로_시작하지_않는_파티션만_격리_탭이다() {
        assert!(!is_ephemeral(None));
        assert!(!is_ephemeral(Some("")));
        assert!(!is_ephemeral(Some("persist:profile-p1")));
        assert!(is_ephemeral(Some("proxy-t3")));
        assert!(is_ephemeral(Some("incognito:1")));
    }

    #[test]
    fn 점의_툴팁은_프록시가_켜져_있을_때만_주소를_보인다() {
        let mut proxy = ProxyConfig {
            enabled: true,
            scheme: ProxyScheme::Socks5,
            host: "127.0.0.1".into(),
            port: 9050,
            username: None,
            password: None,
        };
        assert_eq!(
            dot_title(Some(&proxy)),
            "Isolated proxy tab — 127.0.0.1:9050, own cookies"
        );
        proxy.enabled = false;
        let disabled = "Isolated tab — own cookies (proxy disabled)";
        assert_eq!(dot_title(Some(&proxy)), disabled);
        assert_eq!(dot_title(None), disabled);
    }

    #[test]
    fn 탭_너비는_내용에_맞추되_120과_240_사이다() {
        // 12 + 60 + 8 + 18 + 8 = 106: under the minimum.
        assert_eq!(tab_width(60.0, None, false, true), 120.0);
        assert_eq!(tab_width(100.0, None, false, true), 146.0);
        // The only tab has no close button.
        assert_eq!(tab_width(100.0, None, false, false), 120.0);
        // The dot adds 6 + 8, the label its box + 8.
        assert_eq!(tab_width(100.0, None, true, true), 160.0);
        assert_eq!(tab_width(100.0, Some(40.0), false, true), 194.0);
        assert_eq!(tab_width(100.0, Some(40.0), true, true), 208.0);
        assert_eq!(tab_width(900.0, Some(80.0), true, true), 240.0);
    }

    #[test]
    fn 프로필_라벨은_패딩을_더해_80px까지다() {
        assert_eq!(label_width(24.0), 34.0);
        assert_eq!(label_width(70.0), 80.0);
        assert_eq!(label_width(300.0), 80.0);
    }

    #[test]
    fn 메뉴_너비는_가장_긴_제목에_맞추되_220과_320_사이다() {
        assert_eq!(menu_width(50.0, true), 220.0);
        // 250 + 16 of padding + 4 + 20 for the close button.
        assert_eq!(menu_width(250.0, true), 290.0);
        assert_eq!(menu_width(250.0, false), 266.0);
        assert_eq!(menu_width(900.0, true), 320.0);
        assert_eq!(menu_width(0.0, false), 220.0);
    }

    #[test]
    fn 제목이_없으면_주소를_보인다() {
        let mut t = tab("t1");
        assert_eq!(tab_label(&t), "Tab t1");
        t.title.clear();
        assert_eq!(tab_label(&t), "https://t1.test/");
    }
}

#[cfg(test)]
mod view_tests {
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point};

    use super::*;
    use crate::ui::testing::{center, look};

    /// The tab bar in a 40px strip `width` wide, over a surface that counts presses.
    struct Stage {
        bar: Entity<TabBar>,
        width: f32,
        presses: Rc<RefCell<usize>>,
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
                        .on_any_mouse_down(move |_, _, _| *presses.borrow_mut() += 1),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(px(self.width))
                        .h(px(40.0))
                        .flex()
                        .child(self.bar.clone()),
                )
        }
    }

    struct Fixture {
        bar: Entity<TabBar>,
        props: Rc<RefCell<TabBarProps>>,
        events: Rc<RefCell<Vec<TabBarEvent>>>,
        presses: Rc<RefCell<usize>>,
    }

    fn tab(id: &str) -> Tab {
        Tab {
            id: id.into(),
            url: format!("https://{id}.test/"),
            title: id.to_uppercase(),
            web_contents_id: None,
            initial_url: String::new(),
            proxy: None,
            partition: None,
            profile_id: None,
        }
    }

    /// `count` short-titled tabs (120px each), `active` selected, in a bar `width` wide.
    fn bar<'a>(
        count: usize,
        active: &str,
        width: f32,
        cx: &'a mut TestAppContext,
    ) -> (Fixture, &'a mut VisualTestContext) {
        let props = Rc::new(RefCell::new(TabBarProps {
            tabs: (1..=count).map(|n| tab(&format!("t{n}"))).collect(),
            active_id: active.into(),
            profiles: Vec::new(),
        }));
        let events = Rc::new(RefCell::new(Vec::new()));
        let presses = Rc::new(RefCell::new(0));
        let (stage, cx) = cx.add_window_view({
            let (props, events, presses) = (props.clone(), events.clone(), presses.clone());
            move |_, cx| {
                let bar = cx.new(|_| TabBar::new(move |_| props.borrow().clone(), look()));
                cx.subscribe(&bar, move |_, _, event: &TabBarEvent, _| {
                    events.borrow_mut().push(event.clone())
                })
                .detach();
                Stage {
                    bar,
                    width,
                    presses,
                }
            }
        });
        let bar = stage.read_with(cx, |stage, _| stage.bar.clone());
        cx.run_until_parked();
        let fixture = Fixture {
            bar,
            props,
            events,
            presses,
        };
        (fixture, cx)
    }

    impl Fixture {
        fn overflow(&self, cx: &mut VisualTestContext) -> Vec<String> {
            self.bar.read_with(cx, |bar, _| bar.overflow_ids().to_vec())
        }

        fn events(&self) -> Vec<TabBarEvent> {
            self.events.take()
        }

        /// Change what the host's stores hold and let the bar settle.
        fn set(&self, cx: &mut VisualTestContext, change: impl FnOnce(&mut TabBarProps)) {
            change(&mut self.props.borrow_mut());
            self.bar.update(cx, |_, cx| cx.notify());
            cx.run_until_parked();
        }
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let at = center(selector, cx);
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
    }

    fn select(id: &str) -> TabBarEvent {
        TabBarEvent::Select(id.into())
    }

    fn close(id: &str) -> TabBarEvent {
        TabBarEvent::Close(id.into())
    }

    #[gpui::test]
    fn 탭은_34px_높이에_최소_120px이고_아래에_붙는다(cx: &mut TestAppContext) {
        let (f, cx) = bar(2, "t1", 800.0, cx);
        assert!(f.overflow(cx).is_empty());
        let first = cx.debug_bounds("tab-t1").unwrap();
        let second = cx.debug_bounds("tab-t2").unwrap();
        assert_eq!(first.size, gpui::size(px(120.0), px(34.0)));
        assert_eq!(first.bottom(), px(40.0));
        assert_eq!(second.left() - first.right(), px(2.0));
        // The + button follows the last tab: 4px margin after the 2px gap, 3px off the bottom.
        let plus = cx.debug_bounds("new-tab").unwrap();
        assert_eq!(plus.size, gpui::size(px(28.0), px(28.0)));
        assert_eq!(plus.left() - second.right(), px(6.0));
        assert_eq!(plus.bottom(), px(37.0));
        assert!(cx.debug_bounds("tab-overflow").is_none());
    }

    #[gpui::test]
    fn 긴_제목의_탭은_240px에서_멈춘다(cx: &mut TestAppContext) {
        let (f, cx) = bar(2, "t1", 800.0, cx);
        f.set(cx, |props| {
            props.tabs[0].title = "A very long page title ".repeat(8)
        });
        assert_eq!(cx.debug_bounds("tab-t1").unwrap().size.width, px(240.0));
        assert_eq!(cx.debug_bounds("tab-t2").unwrap().size.width, px(120.0));
    }

    #[gpui::test]
    fn 좁으면_들어가지_않는_탭이_메뉴로_가고_활성_탭은_남는다(
        cx: &mut TestAppContext,
    ) {
        // 464 available at first: the active t5 plus t1 and t2; the chip then takes a
        // little more, which still leaves room for those three.
        let (f, cx) = bar(5, "t5", 500.0, cx);
        assert_eq!(f.overflow(cx), ["t3", "t4"]);
        let chip = cx.debug_bounds("tab-overflow").unwrap();
        assert_eq!(chip.size.height, px(28.0));
        assert_eq!(chip.bottom(), px(37.0));
        assert_eq!(chip.right(), px(500.0));
        // The strip ends before the chip, and the visible tabs are inside it.
        let strip = cx.debug_bounds("tab-strip").unwrap();
        assert_eq!(strip.right() + px(2.0), chip.left());
        let active = cx.debug_bounds("tab-t5").unwrap();
        assert!(active.right() <= strip.right());
        assert_eq!(
            active.left(),
            cx.debug_bounds("tab-t2").unwrap().right() + px(2.0)
        );
    }

    #[gpui::test]
    fn 활성_탭이_바뀌면_넘치는_탭을_다시_고른다(cx: &mut TestAppContext) {
        let (f, cx) = bar(5, "t5", 500.0, cx);
        f.set(cx, |props| props.active_id = "t3".into());
        assert_eq!(f.overflow(cx), ["t4", "t5"]);
    }

    #[gpui::test]
    fn 탭을_닫아_자리가_나면_메뉴에서_돌아온다(cx: &mut TestAppContext) {
        let (f, cx) = bar(4, "t1", 500.0, cx);
        assert_eq!(f.overflow(cx), ["t4"]);
        f.set(cx, |props| {
            props.tabs.remove(1);
        });
        assert!(f.overflow(cx).is_empty());
        assert!(f.bar.read_with(cx, |bar, _| !bar.widths.contains_key("t2")));
    }

    #[gpui::test]
    fn 탭을_누르면_선택을_닫기_버튼을_누르면_닫기만_알린다(
        cx: &mut TestAppContext,
    ) {
        let (f, cx) = bar(2, "t1", 800.0, cx);
        click("tab-t2", cx);
        assert_eq!(f.events(), [select("t2")]);
        let button = cx.debug_bounds("tab-close-t2").unwrap();
        assert_eq!(button.size, gpui::size(px(18.0), px(18.0)));
        // 8px of padding to the right of the close button.
        assert_eq!(
            button.right() + px(8.0),
            cx.debug_bounds("tab-t2").unwrap().right()
        );
        click("tab-close-t2", cx);
        assert_eq!(f.events(), [close("t2")]);
    }

    #[gpui::test]
    fn 탭이_하나뿐이면_닫기_버튼이_없다(cx: &mut TestAppContext) {
        let (_, cx) = bar(1, "t1", 800.0, cx);
        assert!(cx.debug_bounds("tab-t1").is_some());
        assert!(cx.debug_bounds("tab-close-t1").is_none());
    }

    #[gpui::test]
    fn 가운데_버튼_클릭은_탭을_닫는다(cx: &mut TestAppContext) {
        let (f, cx) = bar(3, "t1", 800.0, cx);
        let at = center("tab-t2", cx);
        cx.simulate_mouse_down(at, MouseButton::Middle, Modifiers::none());
        cx.simulate_mouse_up(at, MouseButton::Middle, Modifiers::none());
        assert_eq!(f.events(), [close("t2")]);
    }

    #[gpui::test]
    fn 가운데_버튼을_다른_탭에서_떼면_닫지_않는다(cx: &mut TestAppContext) {
        let (f, cx) = bar(3, "t1", 800.0, cx);
        let (second, third) = (center("tab-t2", cx), center("tab-t3", cx));
        cx.simulate_mouse_down(second, MouseButton::Middle, Modifiers::none());
        cx.simulate_mouse_up(third, MouseButton::Middle, Modifiers::none());
        // Pressed outside the bar, released on the tab pressed before.
        cx.simulate_mouse_down(second, MouseButton::Middle, Modifiers::none());
        cx.simulate_mouse_up(
            point(px(700.0), px(300.0)),
            MouseButton::Middle,
            Modifiers::none(),
        );
        cx.simulate_mouse_down(
            point(px(700.0), px(300.0)),
            MouseButton::Middle,
            Modifiers::none(),
        );
        cx.simulate_mouse_up(second, MouseButton::Middle, Modifiers::none());
        assert_eq!(f.events(), []);
    }

    #[gpui::test]
    fn 더하기_버튼은_구글로_새_탭을_연다(cx: &mut TestAppContext) {
        let (f, cx) = bar(1, "t1", 800.0, cx);
        click("new-tab", cx);
        assert_eq!(
            f.events(),
            [TabBarEvent::NewTab("https://www.google.com".into())]
        );
    }

    #[gpui::test]
    fn 넘침_버튼은_그_아래에_오른쪽을_맞춰_메뉴를_연다(
        cx: &mut TestAppContext,
    ) {
        let (f, cx) = bar(5, "t5", 500.0, cx);
        assert!(cx.debug_bounds("tab-menu").is_none());
        click("tab-overflow", cx);
        assert!(f.bar.read_with(cx, |bar, _| bar.is_menu_open()));
        let chip = cx.debug_bounds("tab-overflow").unwrap();
        let menu = cx.debug_bounds("tab-menu").unwrap();
        assert_eq!(menu.top(), chip.bottom() + px(4.0));
        assert_eq!(menu.right(), chip.right());
        // 220 of content, 4px of padding and the border on each side.
        assert_eq!(menu.size.width, px(230.0));
        let row = cx.debug_bounds("tab-menu-item-t3").unwrap();
        assert_eq!(row.size.height, px(28.0));
        assert_eq!(
            cx.debug_bounds("tab-menu-close-t3").unwrap().size,
            gpui::size(px(20.0), px(20.0))
        );
        assert!(cx.debug_bounds("tab-menu-item-t4").unwrap().top() >= row.bottom());
    }

    #[gpui::test]
    fn 메뉴에서_탭을_고르면_메뉴가_닫히고_선택을_알린다(
        cx: &mut TestAppContext,
    ) {
        let (f, cx) = bar(5, "t5", 500.0, cx);
        click("tab-overflow", cx);
        click("tab-menu-item-t4", cx);
        assert_eq!(f.events(), [select("t4")]);
        assert!(!f.bar.read_with(cx, |bar, _| bar.is_menu_open()));
    }

    #[gpui::test]
    fn 메뉴의_닫기는_탭만_닫고_메뉴는_열어_둔다(cx: &mut TestAppContext) {
        let (f, cx) = bar(5, "t5", 500.0, cx);
        click("tab-overflow", cx);
        click("tab-menu-close-t3", cx);
        assert_eq!(f.events(), [close("t3")]);
        assert!(f.bar.read_with(cx, |bar, _| bar.is_menu_open()));
    }

    #[gpui::test]
    fn 메뉴_바깥을_누르면_닫히고_그_클릭은_아래로_가지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (f, cx) = bar(5, "t5", 500.0, cx);
        click("tab-overflow", cx);
        let elsewhere = point(px(40.0), px(400.0));
        let before = *f.presses.borrow();
        cx.simulate_click(elsewhere, Modifiers::none());
        cx.run_until_parked();
        assert!(!f.bar.read_with(cx, |bar, _| bar.is_menu_open()));
        assert_eq!(*f.presses.borrow(), before);
        cx.simulate_click(elsewhere, Modifiers::none());
        assert_eq!(*f.presses.borrow(), before + 1);
    }

    #[gpui::test]
    fn 열린_메뉴_위에서_탭이나_넘침_버튼을_눌러도_닫히기만_한다(
        cx: &mut TestAppContext,
    ) {
        let (f, cx) = bar(5, "t5", 500.0, cx);
        click("tab-overflow", cx);
        click("tab-t1", cx);
        assert!(!f.bar.read_with(cx, |bar, _| bar.is_menu_open()));
        assert_eq!(f.events(), []);
        click("tab-overflow", cx);
        click("tab-overflow", cx);
        assert!(!f.bar.read_with(cx, |bar, _| bar.is_menu_open()));
    }

    #[gpui::test]
    fn 한_탭뿐인_메뉴에는_닫기_버튼이_없다(cx: &mut TestAppContext) {
        // Too narrow even for the single (inactive) tab next to the stale active id.
        let (f, cx) = bar(1, "gone", 200.0, cx);
        assert_eq!(f.overflow(cx), ["t1"]);
        click("tab-overflow", cx);
        assert!(cx.debug_bounds("tab-menu-item-t1").is_some());
        assert!(cx.debug_bounds("tab-menu-close-t1").is_none());
    }

    #[gpui::test]
    fn 격리_탭과_프로필_탭은_점과_라벨을_단다(cx: &mut TestAppContext) {
        let (f, cx) = bar(3, "t1", 800.0, cx);
        f.set(cx, |props| {
            props.tabs[1].partition = Some("proxy-t2".into());
            props.tabs[2].partition = Some("persist:profile-p1".into());
            props.tabs[2].profile_id = Some("p1".into());
            props.profiles = vec![TabProfile {
                id: "p1".into(),
                name: "Work".into(),
            }];
        });
        assert!(cx.debug_bounds("tab-dot-t1").is_none());
        assert!(cx.debug_bounds("tab-profile-t1").is_none());
        // The dot comes first, 12px in and centered.
        let (isolated, dot) = (
            cx.debug_bounds("tab-t2").unwrap(),
            cx.debug_bounds("tab-dot-t2").unwrap(),
        );
        assert_eq!(dot.size, gpui::size(px(6.0), px(6.0)));
        assert_eq!(dot.left(), isolated.left() + px(12.0));
        assert_eq!(dot.center().y, isolated.center().y);
        // A persistent profile's tab has the label and no dot.
        assert!(cx.debug_bounds("tab-dot-t3").is_none());
        let label = cx.debug_bounds("tab-profile-t3").unwrap();
        assert_eq!(
            label.left(),
            cx.debug_bounds("tab-t3").unwrap().left() + px(12.0)
        );
        assert_eq!(label.size.height, px(14.0));
        assert!(label.size.width <= px(80.0));
        // The default profile's name is never shown.
        f.set(cx, |props| {
            props.tabs[0].profile_id = Some("default".into())
        });
        assert!(cx.debug_bounds("tab-profile-t1").is_none());
        assert!(f.overflow(cx).is_empty());
    }
}
