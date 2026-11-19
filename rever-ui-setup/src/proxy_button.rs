//! Port of `src/renderer/src/components/ProxyButton.tsx` — the toolbar chip that shows
//! the active tab's proxy, and the popover that sets it: a tab that already has its own
//! session gets its proxy changed in place, any other tab is reopened in a new isolated
//! tab routed through the proxy.

use std::hash::{BuildHasher, Hasher};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    App, Context, Entity, FocusHandle, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent,
    SharedString, Subscription, Window, deferred, div, prelude::*, px, rgb,
};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::text_input::TextInput;
use rever_ui_state::tabs::{AddTabOptions, ProxyConfig, ProxyScheme, Tab};

use crate::host::SetupHost;
use crate::ui::{
    Anchor, DISABLED_OPACITY, LookFn, MONO, Tokens, Z_MENU, clickable, field_box, fill, js_trim,
    on_escape, secret_field, select_box, select_list, shadow, text, tooltip,
};

const SCHEMES: [ProxyScheme; 3] = [ProxyScheme::Http, ProxyScheme::Https, ProxyScheme::Socks5];

pub fn scheme_name(scheme: ProxyScheme) -> &'static str {
    match scheme {
        ProxyScheme::Http => "http",
        ProxyScheme::Https => "https",
        ProxyScheme::Socks5 => "socks5",
    }
}

/// What the popover's fields hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub scheme: ProxyScheme,
    pub host: String,
    pub port: String,
    pub username: String,
    pub password: String,
}

/// `EMPTY_DRAFT`: the fields of a tab without a proxy.
impl Default for Draft {
    fn default() -> Self {
        Self {
            scheme: ProxyScheme::Http,
            host: "127.0.0.1".to_owned(),
            port: "8080".to_owned(),
            username: String::new(),
            password: String::new(),
        }
    }
}

pub fn draft_from(proxy: Option<&ProxyConfig>) -> Draft {
    let Some(proxy) = proxy else {
        return Draft::default();
    };
    Draft {
        scheme: proxy.scheme,
        host: proxy.host.clone(),
        port: proxy.port.to_string(),
        username: proxy.username.clone().unwrap_or_default(),
        password: proxy.password.clone().unwrap_or_default(),
    }
}

/// What the port field keeps of what is typed into it: `replace(/[^0-9]/g, '')`.
pub fn digits_only(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).collect()
}

/// `Number(port)` as a port, or `None` when it is not a whole number in 1–65535.
///
/// The port field keeps only digits, but a draft set from code goes through the same
/// check, so the other forms `Number()` reads ("8e1", "0x50", "80.0") are read too.
fn parse_port(port: &str) -> Option<u16> {
    let port = js_trim(port);
    let lower = port.to_ascii_lowercase();
    let prefixed = [("0x", 16), ("0o", 8), ("0b", 2)]
        .into_iter()
        .find_map(|(prefix, radix)| Some((lower.strip_prefix(prefix)?, radix)));
    let number = match prefixed {
        // No sign in these forms, which `from_str_radix` alone would let through.
        Some((digits, radix)) if digits.chars().all(|c| c.is_digit(radix)) => {
            // More digits than fit is far out of range, as it is for `Number()`.
            u64::from_str_radix(digits, radix).ok()? as f64
        }
        Some(_) => return None,
        // Rust's float syntax is JavaScript's decimal one plus "inf" and "nan" in any
        // case: without letters other than the exponent's, the two read alike.
        None if port
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'e' | b'E')) =>
        {
            port.parse::<f64>().ok()?
        }
        None => return None,
    };
    (number.fract() == 0.0 && (1.0..=65535.0).contains(&number)).then_some(number as u16)
}

/// The proxy the draft describes, or the key of the message saying what is wrong with it.
pub fn config_from(draft: &Draft) -> Result<ProxyConfig, &'static str> {
    let host = js_trim(&draft.host);
    if host.is_empty() {
        return Err("proxy.errHost");
    }
    let port = parse_port(&draft.port).ok_or("proxy.errPort")?;
    let username = js_trim(&draft.username);
    Ok(ProxyConfig {
        enabled: true,
        scheme: draft.scheme,
        host: host.to_owned(),
        port,
        username: (!username.is_empty()).then(|| username.to_owned()),
        // The password is sent as typed, spaces included.
        password: (!draft.password.is_empty()).then(|| draft.password.clone()),
    })
}

/// `n.toString(36)` for a non-negative integer.
fn to_base36(mut n: u64) -> String {
    let mut digits = Vec::new();
    loop {
        // n % 36 is always a valid base-36 digit.
        digits.extend(char::from_digit((n % 36) as u32, 36));
        n /= 36;
        if n == 0 {
            break;
        }
    }
    digits.iter().rev().collect()
}

/// The in-memory partition of a new proxy tab: `rever-proxy-`, the time in base 36, and
/// four random base-36 digits (the component's `Math.random().toString(36).slice(2, 6)`).
pub fn proxy_partition(now_ms: u64, random: u64) -> String {
    let suffix = to_base36(random % 36u64.pow(4));
    format!("rever-proxy-{}{suffix:0>4}", to_base36(now_ms))
}

/// Where the new proxy tab goes: the current page when it is a web page (`/^https?:\/\//i`).
pub fn nav_target(url: &str) -> &str {
    let is_web = ["http://", "https://"].iter().any(|prefix| {
        url.get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
    });
    if is_web {
        url
    } else {
        "https://www.google.com"
    }
}

/// The proxy when it is switched on (`on && proxy`).
fn active(proxy: Option<&ProxyConfig>) -> Option<&ProxyConfig> {
    proxy.filter(|proxy| proxy.enabled)
}

/// The chip's text.
pub fn chip_label(proxy: Option<&ProxyConfig>, lang: Lang) -> String {
    match active(proxy) {
        Some(proxy) => fill(
            lang.t("proxy.labelActive"),
            &[("host", &proxy.host), ("port", &proxy.port.to_string())],
        ),
        None => lang.t("proxy.label").to_owned(),
    }
}

/// The chip's `title`.
pub fn chip_title(proxy: Option<&ProxyConfig>, lang: Lang) -> String {
    match active(proxy) {
        Some(proxy) => fill(
            lang.t("proxy.editTitle"),
            &[
                ("scheme", scheme_name(proxy.scheme)),
                ("host", &proxy.host),
                ("port", &proxy.port.to_string()),
            ],
        ),
        None => lang.t("proxy.setTitle").to_owned(),
    }
}

/// A tab that already browses in its own session (`tab.partition` is set and not empty).
fn is_isolated(tab: &Tab) -> bool {
    tab.partition.as_deref().is_some_and(|p| !p.is_empty())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// A random number without a dependency: each `RandomState` is seeded differently.
fn random() -> u64 {
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

pub struct ProxyButton {
    host: Rc<dyn SetupHost>,
    /// The active tab, read on every render, as the `tab` prop is.
    tab: Box<dyn Fn(&App) -> Option<Tab>>,
    /// Called after a proxy change is applied so the caller can reload the tab (the
    /// proxy applies to new requests; a reload makes it take full effect for the
    /// visible page).
    on_applied: Rc<dyn Fn(&mut App)>,
    look: LookFn,
    open: bool,
    scheme: ProxyScheme,
    host_field: Entity<TextInput>,
    port_field: Entity<TextInput>,
    username_field: Entity<TextInput>,
    password_field: Entity<TextInput>,
    /// The key of the error line's message.
    error: Option<&'static str>,
    /// The tab id and proxy the draft was loaded from.
    loaded_from: Option<(String, Option<ProxyConfig>)>,
    scheme_open: bool,
    scheme_anchor: Anchor,
    /// `firstFieldRef`: the scheme select takes the focus each time the draft is loaded.
    scheme_focus: FocusHandle,
    /// The draft was loaded since the last render, which is where the window is at hand.
    focus_scheme: bool,
    escape: Option<Subscription>,
    _fields_changed: Vec<Subscription>,
}

impl ProxyButton {
    pub fn new(
        host: Rc<dyn SetupHost>,
        tab: impl Fn(&App) -> Option<Tab> + 'static,
        on_applied: impl Fn(&mut App) + 'static,
        look: LookFn,
        cx: &mut Context<Self>,
    ) -> Self {
        let host_field = cx.new(|cx| TextInput::new("127.0.0.1", cx));
        let port_field = cx.new(|cx| TextInput::new("8080", cx));
        let username_field = cx.new(|cx| TextInput::new("", cx));
        let password_field = cx.new(|cx| TextInput::new("", cx));
        let fields_changed = vec![
            // Only digits stay in the port field.
            cx.observe(&port_field, |_, field, cx| {
                let digits = digits_only(field.read(cx).text());
                if digits != field.read(cx).text() {
                    field.update(cx, |field, cx| field.set_text(digits, cx));
                }
            }),
            // The masked field draws one dot per character of this one.
            cx.observe(&password_field, |_, _, cx| cx.notify()),
        ];
        Self {
            host,
            tab: Box::new(tab),
            on_applied: Rc::new(on_applied),
            look,
            open: false,
            scheme: ProxyScheme::Http,
            host_field,
            port_field,
            username_field,
            password_field,
            error: None,
            loaded_from: None,
            scheme_open: false,
            scheme_anchor: Anchor::default(),
            scheme_focus: cx.focus_handle(),
            focus_scheme: false,
            escape: None,
            _fields_changed: fields_changed,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn error(&self) -> Option<&'static str> {
        self.error
    }

    pub fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.open = open;
        self.scheme_open = false;
        // ESC closes.
        self.escape = open.then(|| on_escape(cx, |this, cx| this.set_open(false, cx)));
        if open {
            // Load the active tab's config each time the popover opens.
            self.load_draft(cx);
        }
        cx.notify();
    }

    pub fn draft(&self, cx: &App) -> Draft {
        let read = |field: &Entity<TextInput>| field.read(cx).text().to_owned();
        Draft {
            scheme: self.scheme,
            host: read(&self.host_field),
            port: read(&self.port_field),
            username: read(&self.username_field),
            password: read(&self.password_field),
        }
    }

    pub fn set_draft(&mut self, draft: Draft, cx: &mut Context<Self>) {
        self.scheme = draft.scheme;
        for (field, value) in [
            (&self.host_field, draft.host),
            (&self.port_field, draft.port),
            (&self.username_field, draft.username),
            (&self.password_field, draft.password),
        ] {
            field.update(cx, |field, cx| field.set_text(value, cx));
        }
        cx.notify();
    }

    fn load_draft(&mut self, cx: &mut Context<Self>) {
        let tab = (self.tab)(cx);
        let proxy = tab.as_ref().and_then(|tab| tab.proxy.clone());
        self.set_draft(draft_from(proxy.as_ref()), cx);
        self.error = None;
        self.loaded_from = tab.map(|tab| (tab.id, proxy));
        self.focus_scheme = true;
    }

    fn fail(&mut self, key: &'static str, cx: &mut Context<Self>) {
        self.error = Some(key);
        cx.notify();
    }

    pub fn apply(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = (self.tab)(cx) else {
            return;
        };
        let config = match config_from(&self.draft(cx)) {
            Ok(config) => config,
            Err(key) => return self.fail(key, cx),
        };
        let host = self.host.clone();

        // Already an isolated proxy tab → edit its proxy in place.
        if is_isolated(&tab) {
            host.set_tab_proxy(&tab.id, Some(config.clone()), cx);
            // The tab's proxy has changed under the open popover: follow it now, so an
            // error arriving below is not wiped by the reload on the next render.
            self.load_draft(cx);
            let set = host.set_proxy(tab.id, Some(config), None, cx);
            let on_applied = self.on_applied.clone();
            cx.spawn(async move |this, cx| match set.await {
                Err(error) => {
                    eprintln!("[proxy] set failed: {error}");
                    let _ = this.update(cx, |this, cx| this.fail("proxy.errApply", cx));
                }
                Ok(()) => {
                    let _ = this.update(cx, |this, cx| this.set_open(false, cx));
                    let _ = cx.update(|cx| on_applied(cx));
                }
            })
            .detach();
            return;
        }

        // Normal tab → open a NEW tab in its own in-memory partition (separate cookie
        // jar, incognito-style) routed through the proxy. The tab starts on about:blank
        // so no request leaves before the proxy is registered, then navigates to the
        // current page.
        let partition = proxy_partition(now_ms(), random());
        let target = nav_target(&tab.url).to_owned();
        let new_id = host.add_tab(
            "about:blank",
            AddTabOptions {
                proxy: Some(config.clone()),
                partition: Some(partition.clone()),
                ..AddTabOptions::default()
            },
            cx,
        );
        let set = host.set_proxy(new_id.clone(), Some(config), Some(partition), cx);
        cx.spawn(async move |this, cx| match set.await {
            Err(error) => {
                eprintln!("[proxy] set failed: {error}");
                let _ = cx.update(|cx| host.close_tab(&new_id, cx));
                let _ = this.update(cx, |this, cx| this.fail("proxy.errApply", cx));
            }
            Ok(()) => {
                let _ = cx.update(|cx| host.request_navigation(&target, cx));
                let _ = this.update(cx, |this, cx| this.set_open(false, cx));
            }
        })
        .detach();
    }

    pub fn disable(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = (self.tab)(cx) else {
            return;
        };
        self.host.set_tab_proxy(&tab.id, None, cx);
        let cleared = self.host.set_proxy(tab.id, None, None, cx);
        let on_applied = self.on_applied.clone();
        cx.spawn(async move |this, cx| {
            if let Err(error) = cleared.await {
                eprintln!("[proxy] clear failed: {error}");
            }
            let _ = this.update(cx, |this, cx| this.set_open(false, cx));
            let _ = cx.update(|cx| on_applied(cx));
        })
        .detach();
    }

    fn pick_scheme(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(scheme) = SCHEMES.get(index) {
            self.scheme = *scheme;
        }
        self.dismiss_scheme(cx);
    }

    fn dismiss_scheme(&mut self, cx: &mut Context<Self>) {
        self.scheme_open = false;
        cx.notify();
    }

    /// The keys of the focused scheme select: the arrows step through the options without
    /// wrapping, as on a closed native select; Space and Enter show or hide the list.
    fn scheme_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let at = SCHEMES.iter().position(|scheme| *scheme == self.scheme);
        let to = match event.keystroke.key.as_str() {
            "up" => at.map(|at| at.saturating_sub(1)),
            "down" => at.map(|at| at + 1),
            "space" | "enter" => {
                self.scheme_open = !self.scheme_open;
                None
            }
            _ => return,
        };
        if let Some(scheme) = to.and_then(|to| SCHEMES.get(to)) {
            self.scheme = *scheme;
        }
        cx.stop_propagation();
        cx.notify();
    }
}

/// A caption over its field: half of a row.
fn labelled(label: impl IntoElement, tokens: &Tokens) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(px(3.0))
        .text_color(tokens.theme.text_dim)
        .child(label)
}

/// The base `button` rule: the secondary action.
fn button(id: &'static str, label: &'static str, tokens: &Tokens) -> gpui::Stateful<gpui::Div> {
    let hover = tokens.theme.border_2;
    clickable(id)
        .flex_none()
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(4.0))
        .bg(tokens.theme.surface_3)
        .text_color(tokens.theme.text)
        .text_size(px(12.0))
        .line_height(px(16.0))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(label)
}

impl Render for ProxyButton {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let lang = look.lang;
        let tab = (self.tab)(cx);
        let proxy = tab.as_ref().and_then(|tab| tab.proxy.clone());
        let on = active(proxy.as_ref()).is_some();
        let hover = theme.chrome_hover;

        // Another tab became active, or this one's proxy changed, under the open popover.
        if self.open {
            let current = tab.as_ref().map(|tab| (tab.id.clone(), proxy.clone()));
            if current != self.loaded_from {
                self.load_draft(cx);
            }
        }
        let open = self.open;
        // `firstFieldRef.current?.focus()`, in the effect that loads the draft.
        if std::mem::take(&mut self.focus_scheme) && open {
            window.focus(&self.scheme_focus);
        }

        // `.chrome-chip`
        let chip = clickable("proxy-button")
            .h(px(28.0))
            .max_w(px(180.0))
            .px(px(10.0))
            .flex()
            .flex_none()
            .items_center()
            .overflow_hidden()
            .rounded(px(14.0))
            .text_size(px(12.0))
            .whitespace_nowrap()
            .text_color(if on { theme.accent } else { theme.text_dim })
            .when(on, |chip| chip.bg(tokens.accent_soft))
            .map(|chip| {
                if tab.is_none() {
                    return chip.opacity(DISABLED_OPACITY);
                }
                chip.cursor_pointer()
                    // An active chip keeps its own fill under the pointer.
                    .when(!on, |chip| chip.hover(move |style| style.bg(hover)))
                    .on_click(cx.listener(|this, _, _, cx| this.set_open(!this.open, cx)))
            })
            .tooltip(tooltip(chip_title(proxy.as_ref(), lang), &tokens))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(chip_label(proxy.as_ref(), lang)),
            );
        let wrap = div().relative().flex_none().child(chip);
        if !open {
            return wrap;
        }

        let isolated = tab.as_ref().is_some_and(is_isolated);
        // An `<input>` does not take the popover's 12px: it keeps the browser's 13.33px.
        let input = |field: &Entity<TextInput>| {
            field_box(26.0, 6.0)
                .w_full()
                .rounded(px(4.0))
                .bg(tokens.surface)
                .border_color(theme.border_2)
                .text_size(px(13.333))
                .text_color(theme.text)
                .child(field.clone())
        };
        let scheme_options: Vec<SharedString> = SCHEMES
            .iter()
            .map(|scheme| scheme_name(*scheme).into())
            .collect();

        let buttons = div()
            .flex()
            .justify_end()
            .gap(px(6.0))
            .mt(px(2.0))
            .when(on, |row| {
                row.child(
                    button("proxy-disable", lang.t("proxy.disable"), &tokens)
                        .on_click(cx.listener(|this, _, _, cx| this.disable(cx))),
                )
                // `margin-right: auto`. As a spacer: with an auto margin GPUI's layout
                // drops the row's gaps between the buttons after it.
                .child(div().flex_1())
            })
            .child(
                button("proxy-cancel", lang.t("common.cancel"), &tokens)
                    .on_click(cx.listener(|this, _, _, cx| this.set_open(false, cx))),
            )
            .child(
                // Its own fill, so the base rule's hover fill does not apply.
                clickable("proxy-apply")
                    .flex_none()
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .bg(theme.accent)
                    .text_color(rgb(0xffffff))
                    .text_size(px(12.0))
                    .line_height(px(16.0))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.apply(cx)))
                    .child(if isolated {
                        lang.t("proxy.apply")
                    } else {
                        lang.t("proxy.openTab")
                    }),
            );

        // Under the chip, right edges aligned (`top: calc(100% + 6px)`).
        let popover = clickable("proxy-popover")
            .absolute()
            .top(px(34.0))
            .right_0()
            // 286 of content, the 12px padding and the border: the CSS box is content-box.
            .w(px(312.0))
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .rounded(px(8.0))
            .bg(tokens.surface)
            .border_1()
            .border_color(theme.border_2)
            .shadow(vec![shadow(8.0, 24.0, tokens.shadow)])
            .text_size(px(12.0))
            .line_height(px(14.0))
            .occlude()
            // click-outside backdrop: the click closes the popover and goes no further.
            .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Left {
                    this.set_open(false, cx);
                }
                cx.stop_propagation();
            }))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(lang.t("proxy.tabProxy")),
            )
            .child(
                labelled(lang.t("proxy.scheme"), &tokens).child(
                    select_box(
                        "proxy-scheme",
                        scheme_name(self.scheme),
                        &self.scheme_anchor,
                        false,
                    )
                    .h(px(26.0))
                    .px(px(6.0))
                    .rounded(px(4.0))
                    .bg(tokens.surface)
                    .border_color(theme.border_2)
                    .text_size(px(13.333))
                    .text_color(theme.text)
                    .track_focus(&self.scheme_focus)
                    .on_key_down(cx.listener(|this, event, _, cx| this.scheme_key(event, cx)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.scheme_open = !this.scheme_open;
                        cx.notify();
                    })),
                ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .child(
                        labelled(lang.t("proxy.host"), &tokens)
                            .child(input(&self.host_field).font_family(MONO)),
                    )
                    .child(
                        labelled(lang.t("proxy.port"), &tokens)
                            .child(input(&self.port_field).font_family(MONO)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(6.0))
                    .child(
                        labelled(
                            div()
                                .flex()
                                .gap(px(3.0))
                                .whitespace_nowrap()
                                .child(lang.t("proxy.user"))
                                .child(div().opacity(0.6).child(lang.t("proxy.optional"))),
                            &tokens,
                        )
                        .child(input(&self.username_field)),
                    )
                    .child(
                        labelled(lang.t("proxy.password"), &tokens).child(
                            field_box(26.0, 6.0)
                                .debug_selector(|| "proxy-password".into())
                                .w_full()
                                .rounded(px(4.0))
                                .bg(tokens.surface)
                                .border_color(theme.border_2)
                                .text_size(px(13.333))
                                .child(secret_field(&self.password_field, "", theme.text, cx)),
                        ),
                    ),
            )
            .children(self.error.map(|key| {
                div()
                    .debug_selector(|| "proxy-error".into())
                    .text_color(theme.status_error)
                    .child(lang.t(key))
            }))
            .child(buttons)
            .child(
                text(12.0)
                    // `line-height: 1.4`
                    .line_height(px(16.8))
                    .text_color(theme.text_dim)
                    .opacity(0.8)
                    .child(if isolated {
                        lang.t("proxy.noteEdit")
                    } else {
                        lang.t("proxy.noteNew")
                    }),
            )
            // Last, so the open list covers the fields under the select.
            .when(self.scheme_open, |popover| {
                popover.child(select_list(
                    "proxy-scheme-list",
                    &scheme_options,
                    SCHEMES.iter().position(|scheme| *scheme == self.scheme),
                    &self.scheme_anchor,
                    &tokens,
                    cx,
                    Self::pick_scheme,
                    Self::dismiss_scheme,
                ))
            });

        wrap.child(deferred(popover).with_priority(Z_MENU))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy() -> ProxyConfig {
        ProxyConfig {
            enabled: true,
            scheme: ProxyScheme::Socks5,
            host: "10.0.0.2".to_owned(),
            port: 1080,
            username: Some("tk".to_owned()),
            password: Some("s3cret".to_owned()),
        }
    }

    fn draft(host: &str, port: &str) -> Draft {
        Draft {
            host: host.to_owned(),
            port: port.to_owned(),
            ..Draft::default()
        }
    }

    #[test]
    fn 프록시가_없는_탭의_초안은_로컬_8080이다() {
        assert_eq!(
            draft_from(None),
            Draft {
                scheme: ProxyScheme::Http,
                host: "127.0.0.1".to_owned(),
                port: "8080".to_owned(),
                username: String::new(),
                password: String::new(),
            }
        );
    }

    #[test]
    fn 프록시가_있는_탭의_초안은_그_설정이다() {
        assert_eq!(
            draft_from(Some(&proxy())),
            Draft {
                scheme: ProxyScheme::Socks5,
                host: "10.0.0.2".to_owned(),
                port: "1080".to_owned(),
                username: "tk".to_owned(),
                password: "s3cret".to_owned(),
            }
        );
        let anonymous = ProxyConfig {
            username: None,
            password: None,
            ..proxy()
        };
        assert_eq!(draft_from(Some(&anonymous)).username, "");
        assert_eq!(draft_from(Some(&anonymous)).password, "");
        // A switched-off proxy still fills the fields.
        let off = ProxyConfig {
            enabled: false,
            ..proxy()
        };
        assert_eq!(draft_from(Some(&off)).host, "10.0.0.2");
    }

    #[test]
    fn 포트_입력란은_숫자만_남긴다() {
        assert_eq!(digits_only("80a8 0"), "8080");
        assert_eq!(digits_only("-1.5e3"), "153");
        assert_eq!(digits_only("포트"), "");
        // Only ASCII digits: `[^0-9]` drops the full-width ones.
        assert_eq!(digits_only("８０80"), "80");
    }

    #[test]
    fn 초안은_켜진_프록시_설정이_된다() {
        let config = config_from(&Draft {
            scheme: ProxyScheme::Https,
            host: "  proxy.example.com ".to_owned(),
            port: "3128".to_owned(),
            username: " tk ".to_owned(),
            password: " p w ".to_owned(),
        })
        .unwrap();
        assert_eq!(
            config,
            ProxyConfig {
                enabled: true,
                scheme: ProxyScheme::Https,
                host: "proxy.example.com".to_owned(),
                port: 3128,
                username: Some("tk".to_owned()),
                // Not trimmed.
                password: Some(" p w ".to_owned()),
            }
        );
    }

    #[test]
    fn 빈_사용자와_비밀번호는_설정에_넣지_않는다() {
        let config = config_from(&Draft {
            username: "   ".to_owned(),
            ..Draft::default()
        })
        .unwrap();
        assert_eq!(config.username, None);
        assert_eq!(config.password, None);
        assert_eq!((config.host.as_str(), config.port), ("127.0.0.1", 8080));
    }

    #[test]
    fn 호스트가_비면_호스트_오류다() {
        assert_eq!(config_from(&draft("", "8080")), Err("proxy.errHost"));
        assert_eq!(config_from(&draft(" \t", "8080")), Err("proxy.errHost"));
        // The host is checked first.
        assert_eq!(config_from(&draft("", "")), Err("proxy.errHost"));
    }

    #[test]
    fn 포트는_1에서_65535까지만_받는다() {
        let port = |port: &str| config_from(&draft("h", port)).map(|config| config.port);
        assert_eq!(port("1"), Ok(1));
        assert_eq!(port("65535"), Ok(65535));
        // Number("0080") === 80
        assert_eq!(port("0080"), Ok(80));
        // Number("") === 0
        assert_eq!(port(""), Err("proxy.errPort"));
        assert_eq!(port("0"), Err("proxy.errPort"));
        assert_eq!(port("65536"), Err("proxy.errPort"));
        assert_eq!(port("99999999999999999999999"), Err("proxy.errPort"));
        assert_eq!(port("80a"), Err("proxy.errPort"));
    }

    #[test]
    fn 포트는_number가_읽는_모든_꼴로_읽는다() {
        // Each expectation is what `Number(port)` and the component's range check give (bun).
        let port = |port: &str| {
            config_from(&draft("h", port))
                .ok()
                .map(|config| config.port)
        };
        for (text, expected) in [
            (" 80 ", Some(80)),
            ("\u{FEFF}443\u{A0}", Some(443)),
            ("+80", Some(80)),
            ("8e1", Some(80)),
            ("1e3", Some(1000)),
            ("4e4", Some(40000)),
            ("6.5535e4", Some(65535)),
            ("80.0", Some(80)),
            ("5.", Some(5)),
            (".5e1", Some(5)),
            ("65535.00000000000000001", Some(65535)),
            ("0x50", Some(80)),
            ("0X50", Some(80)),
            ("0b101", Some(5)),
            ("0o17", Some(15)),
            ("80.5", None),
            ("1e-1", None),
            ("7e4", None),
            ("-80", None),
            ("-0", None),
            ("0x", None),
            ("-0x50", None),
            ("+0x50", None),
            ("0xffffffffffffffffffff", None),
            ("1_000", None),
            ("e5", None),
            (".", None),
            ("+", None),
            ("  ", None),
            ("Infinity", None),
            ("inf", None),
            ("nan", None),
            ("NaN", None),
            ("８０", None),
        ] {
            assert_eq!(port(text), expected, "{text:?}");
        }
    }

    #[test]
    fn 새_프록시_탭의_파티션은_시각과_무작위_네_자리로_만든다() {
        // (1700000000000).toString(36) === "loyw3v28"
        assert_eq!(
            proxy_partition(1_700_000_000_000, 0),
            "rever-proxy-loyw3v280000"
        );
        assert_eq!(
            proxy_partition(1_700_000_000_000, 36u64.pow(4) - 1),
            "rever-proxy-loyw3v28zzzz"
        );
        // Only four digits of the random number are used.
        assert_eq!(proxy_partition(0, 36u64.pow(4) + 35), "rever-proxy-0000z");
        assert_ne!(
            proxy_partition(1_700_000_000_000, random()),
            proxy_partition(1_700_000_000_001, random())
        );
    }

    #[test]
    fn 웹_페이지가_아니면_새_프록시_탭은_구글로_간다() {
        assert_eq!(nav_target("https://a.test/x?y=1"), "https://a.test/x?y=1");
        assert_eq!(nav_target("http://a.test"), "http://a.test");
        assert_eq!(nav_target("HTTPS://A.TEST"), "HTTPS://A.TEST");
        assert_eq!(nav_target("about:blank"), "https://www.google.com");
        assert_eq!(
            nav_target("view-source:https://a.test"),
            "https://www.google.com"
        );
        assert_eq!(nav_target("file:///tmp/a.html"), "https://www.google.com");
        assert_eq!(nav_target(""), "https://www.google.com");
        // A multi-byte start must not split a character.
        assert_eq!(nav_target("한글한글한글"), "https://www.google.com");
    }

    #[test]
    fn 칩은_켜진_프록시의_호스트와_포트를_보여준다() {
        assert_eq!(chip_label(None, Lang::En), "Proxy");
        assert_eq!(chip_label(None, Lang::Ko), "프록시");
        assert_eq!(chip_label(Some(&proxy()), Lang::En), "Proxy: 10.0.0.2:1080");
        assert_eq!(
            chip_title(Some(&proxy()), Lang::En),
            "Proxy: socks5://10.0.0.2:1080 — click to edit"
        );
        assert_eq!(chip_title(None, Lang::En), "Set a proxy for the browser");
    }

    #[test]
    fn 꺼진_프록시는_칩에_나타나지_않는다() {
        let off = ProxyConfig {
            enabled: false,
            ..proxy()
        };
        assert_eq!(chip_label(Some(&off), Lang::En), "Proxy");
        assert_eq!(
            chip_title(Some(&off), Lang::En),
            "Set a proxy for the browser"
        );
    }

    #[test]
    fn 스킴_이름은_설정에_쓰는_이름과_같다() {
        for scheme in SCHEMES {
            assert_eq!(serde_json::to_value(scheme).unwrap(), scheme_name(scheme));
        }
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::{Cell, RefCell};

    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::ui::testing::{FakeHost, click, elsewhere, look, stage};

    fn tab(url: &str) -> Tab {
        Tab {
            id: "t7".to_owned(),
            url: url.to_owned(),
            title: "Page".to_owned(),
            web_contents_id: None,
            initial_url: url.to_owned(),
            proxy: None,
            partition: None,
            profile_id: None,
        }
    }

    fn proxied() -> Tab {
        Tab {
            proxy: Some(ProxyConfig {
                enabled: true,
                scheme: ProxyScheme::Socks5,
                host: "10.0.0.2".to_owned(),
                port: 1080,
                username: None,
                password: None,
            }),
            partition: Some("rever-proxy-abc".to_owned()),
            ..tab("https://a.test/")
        }
    }

    struct Fixture {
        host: Rc<FakeHost>,
        tab: Rc<RefCell<Option<Tab>>>,
        applied: Rc<Cell<usize>>,
    }

    fn fixture(tab: Option<Tab>) -> Fixture {
        Fixture {
            host: Rc::new(FakeHost::default()),
            tab: Rc::new(RefCell::new(tab)),
            applied: Rc::default(),
        }
    }

    fn button<'a>(
        fixture: &Fixture,
        cx: &'a mut TestAppContext,
    ) -> (
        Entity<ProxyButton>,
        Rc<Cell<usize>>,
        &'a mut VisualTestContext,
    ) {
        let (host, tab, applied) = (
            fixture.host.clone(),
            fixture.tab.clone(),
            fixture.applied.clone(),
        );
        stage(cx, move |cx| {
            ProxyButton::new(
                host,
                move |_| tab.borrow().clone(),
                move |_| applied.set(applied.get() + 1),
                look(),
                cx,
            )
        })
    }

    fn is_open(button: &Entity<ProxyButton>, cx: &mut VisualTestContext) -> bool {
        button.read_with(cx, |button, _| button.is_open())
    }

    fn draft(button: &Entity<ProxyButton>, cx: &mut VisualTestContext) -> Draft {
        button.read_with(cx, |button, cx| button.draft(cx))
    }

    #[gpui::test]
    fn 칩을_누르면_칩_아래에_오른쪽_끝을_맞춰_열리고_탭의_설정을_채운다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(Some(proxied()));
        let (button, _, cx) = button(&fixture, cx);
        assert!(cx.debug_bounds("proxy-popover").is_none());
        click("proxy-button", cx);
        assert!(is_open(&button, cx));
        let chip = cx.debug_bounds("proxy-button").unwrap();
        let popover = cx.debug_bounds("proxy-popover").unwrap();
        assert_eq!(chip.size.height, px(28.0));
        assert_eq!(popover.top(), chip.bottom() + px(6.0));
        assert_eq!(popover.right(), chip.right());
        assert_eq!(popover.size.width, px(312.0));
        let loaded = draft(&button, cx);
        assert_eq!(loaded.scheme, ProxyScheme::Socks5);
        assert_eq!(
            (loaded.host.as_str(), loaded.port.as_str()),
            ("10.0.0.2", "1080")
        );
        // A proxied tab can be switched off: Disable at the left edge, the other two
        // buttons at the right, 6px apart.
        let disable = cx.debug_bounds("proxy-disable").unwrap();
        let cancel = cx.debug_bounds("proxy-cancel").unwrap();
        let apply = cx.debug_bounds("proxy-apply").unwrap();
        assert_eq!(disable.left(), popover.left() + px(13.0));
        assert_eq!(apply.right(), popover.right() - px(13.0));
        assert_eq!(apply.left(), cancel.right() + px(6.0));
    }

    #[gpui::test]
    fn 프록시가_없는_탭에서는_기본값으로_열리고_해제_버튼이_없다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(Some(tab("https://a.test/")));
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        assert_eq!(draft(&button, cx), Draft::default());
        assert!(cx.debug_bounds("proxy-disable").is_none());
    }

    #[gpui::test]
    fn 탭이_없으면_칩은_눌러도_열리지_않는다(cx: &mut TestAppContext) {
        let fixture = fixture(None);
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        assert!(!is_open(&button, cx));
    }

    #[gpui::test]
    fn 다시_열면_고치다_만_값과_오류를_버리고_탭의_설정을_다시_채운다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(Some(tab("https://a.test/")));
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        button.update(cx, |button, cx| {
            button.set_draft(
                Draft {
                    host: String::new(),
                    ..Draft::default()
                },
                cx,
            );
            button.apply(cx);
        });
        assert_eq!(
            button.read_with(cx, |b, _| b.error()),
            Some("proxy.errHost")
        );
        click("proxy-cancel", cx);
        assert!(!is_open(&button, cx));
        click("proxy-button", cx);
        assert_eq!(draft(&button, cx), Draft::default());
        assert_eq!(button.read_with(cx, |b, _| b.error()), None);
    }

    #[gpui::test]
    fn 포트_입력란에는_숫자만_남는다(cx: &mut TestAppContext) {
        let fixture = fixture(Some(tab("https://a.test/")));
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        button.update(cx, |button, cx| {
            button
                .port_field
                .update(cx, |field, cx| field.set_text("31a28 ", cx));
        });
        cx.run_until_parked();
        assert_eq!(draft(&button, cx).port, "3128");
    }

    #[gpui::test]
    fn 일반_탭에서는_빈_새_탭을_프록시와_함께_열고_등록한_뒤에_현재_페이지로_보낸다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(Some(tab("https://a.test/page")));
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        click("proxy-apply", cx);
        let config = r#"{"enabled":true,"scheme":"http","host":"127.0.0.1","port":8080}"#;
        assert_eq!(
            fixture.host.calls()[1..],
            [
                format!("set_proxy t1 {config} partition=true"),
                "request_navigation https://a.test/page".to_owned(),
            ]
        );
        // The new tab starts blank, in a partition of its own, with the proxy on it.
        let added = &fixture.host.calls()[0];
        assert!(added.starts_with("add_tab t1 about:blank partition=Some(\"rever-proxy-"));
        assert!(added.ends_with("profile=None proxy=true"));
        assert!(!is_open(&button, cx));
        // The current tab was not touched, so there is nothing to reload.
        assert_eq!(fixture.applied.get(), 0);
    }

    #[gpui::test]
    fn 웹_페이지가_아닌_탭에서_연_프록시_탭은_구글로_간다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(Some(tab("about:blank")));
        let (_, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        click("proxy-apply", cx);
        assert_eq!(
            fixture.host.calls().last().unwrap(),
            "request_navigation https://www.google.com"
        );
    }

    #[gpui::test]
    fn 새_탭에_프록시를_걸지_못하면_그_탭을_닫고_오류를_보여준다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(Some(tab("https://a.test/")));
        fixture.host.failing.borrow_mut().push("set_proxy");
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        click("proxy-apply", cx);
        assert_eq!(fixture.host.calls().last().unwrap(), "close_tab t1");
        assert!(is_open(&button, cx));
        assert_eq!(
            button.read_with(cx, |b, _| b.error()),
            Some("proxy.errApply")
        );
        assert!(cx.debug_bounds("proxy-error").is_some());
    }

    #[gpui::test]
    fn 격리된_탭에서는_그_탭의_프록시를_바꾸고_다시_불러오게_한다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(Some(proxied()));
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        button.update(cx, |button, cx| {
            button.set_draft(
                Draft {
                    scheme: ProxyScheme::Https,
                    host: "proxy.example.com".to_owned(),
                    port: "3128".to_owned(),
                    username: "tk".to_owned(),
                    password: "pw".to_owned(),
                },
                cx,
            );
        });
        click("proxy-apply", cx);
        assert_eq!(
            fixture.host.calls(),
            [
                "set_tab_proxy t7 true".to_owned(),
                r#"set_proxy t7 {"enabled":true,"scheme":"https","host":"proxy.example.com","port":3128,"username":"tk","password":"pw"} partition=false"#.to_owned(),
            ]
        );
        assert!(!is_open(&button, cx));
        assert_eq!(fixture.applied.get(), 1);
    }

    #[gpui::test]
    fn 격리된_탭에_프록시를_걸지_못하면_열어_둔_채_오류를_보여준다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(Some(proxied()));
        fixture.host.failing.borrow_mut().push("set_proxy");
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        click("proxy-apply", cx);
        assert!(is_open(&button, cx));
        assert_eq!(
            button.read_with(cx, |b, _| b.error()),
            Some("proxy.errApply")
        );
        assert_eq!(fixture.applied.get(), 0);
    }

    #[gpui::test]
    fn 잘못된_입력은_호스트에_묻지_않고_오류를_보여준다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(Some(tab("https://a.test/")));
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        button.update(cx, |button, cx| {
            button.set_draft(
                Draft {
                    port: "70000".to_owned(),
                    ..Draft::default()
                },
                cx,
            );
        });
        click("proxy-apply", cx);
        assert_eq!(
            button.read_with(cx, |b, _| b.error()),
            Some("proxy.errPort")
        );
        assert!(fixture.host.calls().is_empty());
        assert!(is_open(&button, cx));
    }

    #[gpui::test]
    fn 해제는_탭의_프록시를_지우고_닫은_뒤_다시_불러오게_한다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(Some(proxied()));
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        click("proxy-disable", cx);
        assert_eq!(
            fixture.host.calls(),
            [
                "set_tab_proxy t7 false",
                "set_proxy t7 null partition=false"
            ]
        );
        assert!(!is_open(&button, cx));
        assert_eq!(fixture.applied.get(), 1);
    }

    #[gpui::test]
    fn 해제에_실패해도_닫고_다시_불러오게_한다(cx: &mut TestAppContext) {
        let fixture = fixture(Some(proxied()));
        fixture.host.failing.borrow_mut().push("set_proxy");
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        click("proxy-disable", cx);
        assert!(!is_open(&button, cx));
        assert_eq!(fixture.applied.get(), 1);
    }

    #[gpui::test]
    fn 스킴_목록에서_고르면_초안의_스킴이_바뀐다(cx: &mut TestAppContext) {
        let fixture = fixture(Some(tab("https://a.test/")));
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        click("proxy-scheme", cx);
        let select = cx.debug_bounds("proxy-scheme").unwrap();
        let list = cx.debug_bounds("proxy-scheme-list").unwrap();
        assert_eq!(select.size.height, px(26.0));
        assert_eq!(list.top(), select.bottom() + px(2.0));
        assert_eq!(list.left(), select.left());
        assert_eq!(list.size.width, select.size.width);
        click("proxy-scheme-list-2", cx);
        assert_eq!(draft(&button, cx).scheme, ProxyScheme::Socks5);
        // Picking closes the list, not the popover.
        assert!(is_open(&button, cx));
        assert!(!button.read_with(cx, |button, _| button.scheme_open));
    }

    #[gpui::test]
    fn 열리면_스킴_선택이_포커스를_받고_화살표_키로_스킴을_바꾼다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(Some(tab("https://a.test/")));
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        let focused = button.read_with(cx, |button, _| button.scheme_focus.clone());
        assert!(cx.update(|window, _| focused.is_focused(window)));

        cx.simulate_keystrokes("down");
        assert_eq!(draft(&button, cx).scheme, ProxyScheme::Https);
        // No wrapping at either end.
        cx.simulate_keystrokes("down down");
        assert_eq!(draft(&button, cx).scheme, ProxyScheme::Socks5);
        cx.simulate_keystrokes("up up up");
        assert_eq!(draft(&button, cx).scheme, ProxyScheme::Http);

        cx.simulate_keystrokes("space");
        assert!(button.read_with(cx, |button, _| button.scheme_open));
        cx.simulate_keystrokes("enter");
        assert!(!button.read_with(cx, |button, _| button.scheme_open));
        // The keys change the draft only.
        assert!(is_open(&button, cx));
        assert!(fixture.host.calls().is_empty());
    }

    #[gpui::test]
    fn 열린_채로_칩을_다시_누르면_닫힌다(cx: &mut TestAppContext) {
        let fixture = fixture(Some(tab("https://a.test/")));
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        click("proxy-button", cx);
        assert!(!is_open(&button, cx));
    }

    #[gpui::test]
    fn 열린_동안_다른_탭이_활성이_되면_그_탭의_설정으로_바뀐다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(Some(tab("https://a.test/")));
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        cx.update(|window, _| window.blur());
        *fixture.tab.borrow_mut() = Some(Tab {
            id: "t9".to_owned(),
            ..proxied()
        });
        button.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert_eq!(draft(&button, cx).host, "10.0.0.2");
        // Loading the draft puts the focus back on the first field, as the effect does.
        let focused = button.read_with(cx, |button, _| button.scheme_focus.clone());
        assert!(cx.update(|window, _| focused.is_focused(window)));
    }

    #[gpui::test]
    fn 바깥을_누르면_닫히고_그_클릭은_아래로_가지_않는다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(Some(tab("https://a.test/")));
        let (button, presses, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        let before = presses.get();
        cx.simulate_click(elsewhere(), Modifiers::none());
        cx.run_until_parked();
        assert!(!is_open(&button, cx));
        assert_eq!(presses.get(), before);
        assert!(fixture.host.calls().is_empty());
    }

    #[gpui::test]
    fn esc와_취소는_아무것도_적용하지_않고_닫는다(cx: &mut TestAppContext) {
        let fixture = fixture(Some(tab("https://a.test/")));
        let (button, _, cx) = button(&fixture, cx);
        click("proxy-button", cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!is_open(&button, cx));
        assert!(button.read_with(cx, |button, _| button.escape.is_none()));
        click("proxy-button", cx);
        click("proxy-cancel", cx);
        assert!(!is_open(&button, cx));
        assert!(fixture.host.calls().is_empty());
        assert_eq!(fixture.applied.get(), 0);
    }
}
