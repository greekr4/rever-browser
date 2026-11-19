//! Port of `src/renderer/src/components/ProfileMenu.tsx` — Chrome-style profile avatar in
//! the toolbar: open a tab in a profile, create one, import cookies.

use std::rc::Rc;

use gpui::{
    AnyElement, Context, Entity, MouseButton, MouseDownEvent, Subscription, Window, deferred, div,
    prelude::*, px, rgba, svg,
};
use rever_ui_kit::text_input::TextInput;

use crate::host::{
    BrowserId, BrowserInfo, BrowserProfile, BrowserProfileInfo, ProfileKind, SetupHost,
};
use crate::profiles::{DEFAULT_PROFILE_ID, ImportProfileResult, ProfilesStore};
use crate::ui::{LookFn, MONO, Tokens, Z_MENU, clickable, field_box, named, shadow, text, tooltip};

/// The line shown while an import runs.
pub fn import_progress(profile_name: &str) -> String {
    format!("Importing {profile_name}…")
}

/// The line shown once an import has ended. `Err` is the rejected call's message.
pub fn import_message(profile_name: &str, result: Result<ImportProfileResult, String>) -> String {
    match result {
        Ok(result) if result.ok => format!(
            "✓ {profile_name}: imported {}/{} cookies",
            result.imported, result.total
        ),
        // `??`: an empty error stays empty.
        Ok(result) => format!("⚠ {}", result.error.as_deref().unwrap_or("import failed")),
        Err(message) => format!("⚠ {message}"),
    }
}

/// A failed import's line is drawn in the error color.
pub fn is_warning(message: &str) -> bool {
    message.starts_with('⚠')
}

/// The small grey words after a profile's name: where it came from, and "incognito".
pub fn profile_tags(profile: &BrowserProfile) -> Vec<&str> {
    let mut tags = Vec::new();
    if let Some(source) = profile
        .source
        .as_deref()
        .filter(|source| !source.is_empty())
    {
        tags.push(source);
    }
    if profile.kind == ProfileKind::Incognito {
        tags.push("incognito");
    }
    tags
}

pub struct ProfileMenu {
    store: Entity<ProfilesStore>,
    look: LookFn,
    open: bool,
    new_name: Entity<TextInput>,
    browsers: Vec<BrowserInfo>,
    expanded_browser: Option<BrowserId>,
    import_msg: Option<String>,
    _changed: [Subscription; 2],
}

impl ProfileMenu {
    pub fn new(
        store: Entity<ProfilesStore>,
        host: Rc<dyn SetupHost>,
        look: LookFn,
        cx: &mut Context<Self>,
    ) -> Self {
        // The placeholder is drawn by the menu, in the current language.
        let new_name = cx.new(|cx| TextInput::new("", cx));
        store.update(cx, |store, cx| store.load(cx)).detach();
        let listed = host.browsers(cx);
        cx.spawn(async move |this, cx| {
            let browsers = listed.await;
            let _ = this.update(cx, |this, cx| {
                this.browsers = browsers;
                cx.notify();
            });
        })
        .detach();
        Self {
            _changed: [
                cx.observe(&store, |_, _, cx| cx.notify()),
                // The placeholder shows only while the field is empty.
                cx.observe(&new_name, |_, _, cx| cx.notify()),
            ],
            store,
            look,
            open: false,
            new_name,
            browsers: Vec::new(),
            expanded_browser: None,
            import_msg: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.open = open;
        cx.notify();
    }

    pub fn import_msg(&self) -> Option<&str> {
        self.import_msg.as_deref()
    }

    fn open_tab(&mut self, profile: &BrowserProfile, cx: &mut Context<Self>) {
        self.set_open(false, cx);
        self.store
            .update(cx, |store, cx| store.open_tab(profile, cx))
            .detach();
    }

    fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        self.store
            .update(cx, |store, cx| store.remove(id, cx))
            .detach();
    }

    /// Create a profile under the typed name (as typed: the main side names an empty
    /// one) and open a tab in it.
    fn create(&mut self, kind: ProfileKind, cx: &mut Context<Self>) {
        let name = self.new_name.read(cx).text().to_owned();
        let created = self
            .store
            .update(cx, |store, cx| store.create(name, kind, None, cx));
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            let Ok(profile) = created.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.new_name.update(cx, |input, cx| input.set_text("", cx));
                this.set_open(false, cx);
            });
            if let Ok(opened) = store.update(cx, |store, cx| store.open_tab(&profile, cx)) {
                let _ = opened.await;
            }
        })
        .detach();
    }

    /// Show or hide a browser's profiles in the import section.
    pub fn toggle_browser(&mut self, id: BrowserId, cx: &mut Context<Self>) {
        self.expanded_browser = (self.expanded_browser != Some(id)).then_some(id);
        cx.notify();
    }

    fn import_browser(
        &mut self,
        browser: &BrowserInfo,
        profile: &BrowserProfileInfo,
        cx: &mut Context<Self>,
    ) {
        self.import_msg = Some(import_progress(&profile.name));
        cx.notify();
        let imported = self.store.update(cx, |store, cx| {
            store.import_from_browser(browser, profile, cx)
        });
        let name = profile.name.clone();
        cx.spawn(async move |this, cx| {
            let message = import_message(&name, imported.await);
            let _ = this.update(cx, |this, cx| {
                this.import_msg = Some(message);
                cx.notify();
            });
        })
        .detach();
    }
}

/// The 1px line between the menu's sections.
fn divider(tokens: &Tokens) -> gpui::Div {
    div().h(px(1.0)).my(px(4.0)).bg(tokens.theme.border_2)
}

/// A section's caption. The CSS uppercases it; its 0.5px letter spacing has no GPUI
/// counterpart.
fn caption(label: &str, tokens: &Tokens) -> gpui::Div {
    text(10.0)
        .px(px(8.0))
        .py(px(4.0))
        .text_color(tokens.theme.text_dim)
        .child(label.to_uppercase())
}

impl Render for ProfileMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let lang = look.lang;
        let open = self.open;
        let hover = theme.chrome_hover;

        // `.chrome-icon-btn`
        let button = clickable("profile-button")
            .size(px(32.0))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .when(open, |button| button.bg(hover))
            .hover(move |style| style.bg(hover))
            .tooltip(tooltip(lang.t("tab.newInProfile"), &tokens))
            .on_click(cx.listener(|this, _, _, cx| this.set_open(!this.open, cx)))
            .child(
                svg()
                    .path("icons/profile.svg")
                    .size(px(18.0))
                    .text_color(theme.text_2),
            );
        let wrap = div().relative().flex_none().child(button);
        if !open {
            return wrap;
        }

        let profiles = self.store.read(cx).profiles().to_vec();
        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(caption(lang.t("profile.openIn"), &tokens).into_any_element());
        for profile in profiles {
            let id = profile.id.clone();
            let tags = profile_tags(&profile)
                .into_iter()
                .map(|tag| {
                    text(10.0)
                        .ml(px(6.0))
                        .text_color(theme.text_dim)
                        .child(tag.to_owned())
                })
                .collect::<Vec<_>>();
            let delete = (profile.id != DEFAULT_PROFILE_ID).then(|| {
                let id = id.clone();
                clickable(named("profile-delete", &id))
                    .size(px(20.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .text_size(px(13.0))
                    .text_color(theme.text_dim)
                    .cursor_pointer()
                    .tooltip(tooltip(lang.t("profile.delete"), &tokens))
                    .on_click(cx.listener(move |this, _, _, cx| this.remove(&id, cx)))
                    .child("×")
            });
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(
                        clickable(named("profile-open", &id))
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_baseline()
                            .px(px(8.0))
                            .py(px(6.0))
                            .rounded(px(4.0))
                            .text_size(px(12.0))
                            .line_height(px(16.0))
                            .text_color(theme.text)
                            .cursor_pointer()
                            .child(profile.name.clone())
                            .children(tags)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.open_tab(&profile, cx)),
                            ),
                    )
                    .children(delete)
                    .into_any_element(),
            );
        }

        rows.push(divider(&tokens).into_any_element());
        rows.push(
            field_box(29.0, 8.0)
                .debug_selector(|| "profile-new-name".into())
                .w_full()
                .mb(px(4.0))
                .rounded(px(4.0))
                .border_color(theme.border_2)
                .text_size(px(12.0))
                .text_color(theme.text)
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .child(self.new_name.clone())
                        .when(self.new_name.read(cx).text().is_empty(), |field| {
                            field.child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    // The field's own line height.
                                    .line_height(px(20.0))
                                    .opacity(0.45)
                                    .child(lang.t("profile.newName")),
                            )
                        }),
                )
                .into_any_element(),
        );
        let create =
            |id: &'static str, kind: ProfileKind, label: &'static str, hint: &'static str| {
                clickable(id)
                    .flex_1()
                    .flex()
                    .justify_center()
                    .px(px(8.0))
                    .py(px(6.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme.border_2)
                    .text_size(px(11.0))
                    .line_height(px(16.0))
                    .text_color(theme.text_2)
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .tooltip(tooltip(hint, &tokens))
                    .on_click(cx.listener(move |this, _, _, cx| this.create(kind, cx)))
                    .child(label)
            };
        rows.push(
            div()
                .flex()
                .gap(px(4.0))
                .child(create(
                    "profile-create-persistent",
                    ProfileKind::Persistent,
                    lang.t("profile.persistent"),
                    lang.t("profile.persistentHint"),
                ))
                .child(create(
                    "profile-create-incognito",
                    ProfileKind::Incognito,
                    lang.t("profile.incognito"),
                    lang.t("profile.incognitoHint"),
                ))
                .into_any_element(),
        );

        if !self.browsers.is_empty() {
            rows.push(divider(&tokens).into_any_element());
            rows.push(caption(lang.t("profile.importFrom"), &tokens).into_any_element());
            for (index, browser) in self.browsers.iter().enumerate() {
                let expanded = self.expanded_browser == Some(browser.id);
                let browser_id = browser.id;
                rows.push(
                    clickable(("profile-browser", index))
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px(px(8.0))
                        .py(px(6.0))
                        .rounded(px(4.0))
                        .text_size(px(12.0))
                        .line_height(px(16.0))
                        .text_color(theme.text)
                        .cursor_pointer()
                        .tooltip(tooltip(
                            format!("Import a {} profile", browser.name),
                            &tokens,
                        ))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.toggle_browser(browser_id, cx)),
                        )
                        // Written in Korean in the component, whatever the language.
                        .child(format!("{}에서", browser.name))
                        .child(
                            div()
                                // The system font's fallback draws these as a dot at this size.
                                .font_family(MONO)
                                .text_size(px(10.0))
                                .text_color(theme.text_dim)
                                .child(if expanded { "▾" } else { "▸" }),
                        )
                        .into_any_element(),
                );
                if !expanded {
                    continue;
                }
                for (at, profile) in browser.profiles.iter().enumerate() {
                    let (browser, profile) = (browser.clone(), profile.clone());
                    rows.push(
                        clickable(named(&format!("profile-import-{index}"), &at.to_string()))
                            .w_full()
                            .pl(px(20.0))
                            .pr(px(8.0))
                            .py(px(5.0))
                            .rounded(px(4.0))
                            .text_size(px(12.0))
                            .line_height(px(16.0))
                            .text_color(theme.text_2)
                            .cursor_pointer()
                            .tooltip(tooltip(
                                format!(
                                    "New profile \"{}\" seeded with {} cookies",
                                    profile.name, browser.name
                                ),
                                &tokens,
                            ))
                            .child(format!("↓ {}", profile.name))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.import_browser(&browser, &profile, cx)
                            }))
                            .into_any_element(),
                    );
                }
            }
            if let Some(message) = self.import_msg.clone() {
                rows.push(
                    text(10.0)
                        .debug_selector(|| "profile-import-msg".into())
                        .px(px(8.0))
                        .py(px(4.0))
                        .text_color(if is_warning(&message) {
                            theme.status_error
                        } else {
                            theme.text_dim
                        })
                        .child(message)
                        .into_any_element(),
                );
            }
        }

        // Under the button, right edges aligned (top = the button's bottom + 4).
        let menu = clickable("profile-menu")
            .absolute()
            .top(px(36.0))
            .right_0()
            // 200 of content, the 4px padding and the border: the CSS box is content-box.
            .min_w(px(210.0))
            .p(px(4.0))
            .rounded(px(6.0))
            .bg(theme.bg)
            .border_1()
            .border_color(theme.border_2)
            // 0 4px 16px rgba(0, 0, 0, 0.3)
            .shadow(vec![shadow(4.0, 16.0, rgba(0x0000004d))])
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

    fn profile(kind: ProfileKind, source: Option<&str>) -> BrowserProfile {
        BrowserProfile {
            id: "work".to_owned(),
            name: "Work".to_owned(),
            kind,
            partition: "persist:rever-profile-work".to_owned(),
            source: source.map(str::to_owned),
        }
    }

    fn result(ok: bool, error: Option<&str>) -> Result<ImportProfileResult, String> {
        Ok(ImportProfileResult {
            ok,
            imported: 118,
            total: 120,
            error: error.map(str::to_owned),
        })
    }

    #[test]
    fn 가져오는_동안에는_프로필_이름과_함께_진행_중임을_알린다() {
        assert_eq!(import_progress("태균"), "Importing 태균…");
        assert!(!is_warning(&import_progress("태균")));
    }

    #[test]
    fn 성공하면_가져온_수와_전체_수를_보여준다() {
        let message = import_message("태균", result(true, None));
        assert_eq!(message, "✓ 태균: imported 118/120 cookies");
        assert!(!is_warning(&message));
    }

    #[test]
    fn 실패하면_경고_표시와_오류를_보여준다() {
        let message = import_message("태균", result(false, Some("Keychain access denied")));
        assert_eq!(message, "⚠ Keychain access denied");
        assert!(is_warning(&message));
        assert_eq!(
            import_message("태균", result(false, None)),
            "⚠ import failed"
        );
        // `??` keeps an empty error.
        assert_eq!(import_message("태균", result(false, Some(""))), "⚠ ");
    }

    #[test]
    fn 호출이_실패하면_그_메시지를_경고로_보여준다() {
        let message = import_message("태균", Err("profile already exists".to_owned()));
        assert_eq!(message, "⚠ profile already exists");
        assert!(is_warning(&message));
    }

    #[test]
    fn 프로필_이름_뒤에_출처와_시크릿_표시를_붙인다() {
        assert!(profile_tags(&profile(ProfileKind::Persistent, None)).is_empty());
        assert_eq!(
            profile_tags(&profile(ProfileKind::Persistent, Some("Chrome"))),
            ["Chrome"]
        );
        assert_eq!(
            profile_tags(&profile(ProfileKind::Incognito, None)),
            ["incognito"]
        );
        assert_eq!(
            profile_tags(&profile(ProfileKind::Incognito, Some("Arc"))),
            ["Arc", "incognito"]
        );
        // An empty source is no source (`p.source && …`).
        assert!(profile_tags(&profile(ProfileKind::Persistent, Some(""))).is_empty());
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::Cell;

    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::ui::testing::{FakeHost, click, elsewhere, look, stage};

    fn profile(id: &str, name: &str, kind: ProfileKind) -> BrowserProfile {
        BrowserProfile {
            id: id.to_owned(),
            name: name.to_owned(),
            kind,
            partition: format!("persist:rever-profile-{id}"),
            source: None,
        }
    }

    fn host() -> Rc<FakeHost> {
        let host = Rc::new(FakeHost::default());
        *host.profiles.borrow_mut() = vec![
            profile("default", "Default", ProfileKind::Persistent),
            profile("work", "Work", ProfileKind::Persistent),
        ];
        *host.browsers.borrow_mut() = vec![BrowserInfo {
            id: BrowserId::Chrome,
            name: "Chrome".to_owned(),
            profiles: vec![
                BrowserProfileInfo {
                    id: "Default".to_owned(),
                    name: "Personal".to_owned(),
                },
                BrowserProfileInfo {
                    id: "Profile 1".to_owned(),
                    name: "태균".to_owned(),
                },
            ],
        }];
        host
    }

    fn menu<'a>(
        host: &Rc<FakeHost>,
        cx: &'a mut TestAppContext,
    ) -> (
        Entity<ProfileMenu>,
        Entity<ProfilesStore>,
        Rc<Cell<usize>>,
        &'a mut VisualTestContext,
    ) {
        let store = cx.new({
            let host = host.clone();
            |_| ProfilesStore::new(host)
        });
        let (menu, presses, cx) = stage(cx, {
            let (host, store) = (host.clone(), store.clone());
            move |cx| ProfileMenu::new(store, host, look(), cx)
        });
        host.calls.borrow_mut().clear();
        (menu, store, presses, cx)
    }

    fn is_open(menu: &Entity<ProfileMenu>, cx: &mut VisualTestContext) -> bool {
        menu.read_with(cx, |menu, _| menu.is_open())
    }

    #[gpui::test]
    fn 만들어질_때_프로필과_브라우저를_읽어_온다(cx: &mut TestAppContext) {
        let host = host();
        let store = cx.new({
            let host = host.clone();
            |_| ProfilesStore::new(host)
        });
        let (menu, _, cx) = stage(cx, {
            let (host, store) = (host.clone(), store.clone());
            move |cx| ProfileMenu::new(store, host, look(), cx)
        });
        assert_eq!(host.calls(), ["list_profiles", "browsers"]);
        assert_eq!(store.read_with(cx, |store, _| store.profiles().len()), 2);
        assert_eq!(menu.read_with(cx, |menu, _| menu.browsers.len()), 1);
        assert!(!is_open(&menu, cx));
        assert!(cx.debug_bounds("profile-menu").is_none());
    }

    #[gpui::test]
    fn 버튼을_누르면_버튼_아래에_오른쪽_끝을_맞춰_열린다(
        cx: &mut TestAppContext,
    ) {
        let host = host();
        let (menu, _, _, cx) = menu(&host, cx);
        click("profile-button", cx);
        assert!(is_open(&menu, cx));
        let button = cx.debug_bounds("profile-button").unwrap();
        let popup = cx.debug_bounds("profile-menu").unwrap();
        assert_eq!(button.size, gpui::size(px(32.0), px(32.0)));
        assert_eq!(popup.top(), button.bottom() + px(4.0));
        assert_eq!(popup.right(), button.right());
        assert!(popup.size.width >= px(210.0));
        // The Default profile cannot be deleted.
        assert!(cx.debug_bounds("profile-delete-default").is_none());
        assert!(cx.debug_bounds("profile-delete-work").is_some());
    }

    #[gpui::test]
    fn 프로필을_누르면_메뉴를_닫고_그_프로필로_탭을_연다(
        cx: &mut TestAppContext,
    ) {
        let host = host();
        let (menu, _, _, cx) = menu(&host, cx);
        click("profile-button", cx);
        click("profile-open-work", cx);
        assert!(!is_open(&menu, cx));
        assert_eq!(
            host.calls(),
            [
                "add_tab t1 https://www.google.com partition=Some(\"persist:rever-profile-work\") profile=Some(\"work\") proxy=false",
                "register_tab_partition t1 persist:rever-profile-work",
                "set_active_tab t1",
            ]
        );
    }

    #[gpui::test]
    fn 삭제_버튼은_프로필을_지우고_메뉴는_열린_채로_둔다(
        cx: &mut TestAppContext,
    ) {
        let host = host();
        let (menu, store, _, cx) = menu(&host, cx);
        click("profile-button", cx);
        click("profile-delete-work", cx);
        assert_eq!(host.calls(), ["delete_profile work"]);
        assert_eq!(store.read_with(cx, |store, _| store.profiles().len()), 1);
        assert!(is_open(&menu, cx));
    }

    #[gpui::test]
    fn 영구_프로필을_만들면_입력한_이름으로_만들고_그_프로필로_탭을_연다(
        cx: &mut TestAppContext,
    ) {
        let host = host();
        let (menu, store, _, cx) = menu(&host, cx);
        click("profile-button", cx);
        menu.update(cx, |menu, cx| {
            menu.new_name
                .update(cx, |input, cx| input.set_text("Research", cx));
        });
        click("profile-create-persistent", cx);
        assert_eq!(
            host.calls(),
            [
                "create_profile \"Research\" Persistent None",
                "add_tab t1 https://www.google.com partition=Some(\"persist:rever-profile-Research\") profile=Some(\"p-Research\") proxy=false",
                "register_tab_partition t1 persist:rever-profile-Research",
                "set_active_tab t1",
            ]
        );
        assert_eq!(store.read_with(cx, |store, _| store.profiles().len()), 3);
        assert!(!is_open(&menu, cx));
        assert_eq!(
            menu.read_with(cx, |menu, cx| menu.new_name.read(cx).text().to_owned()),
            ""
        );
    }

    #[gpui::test]
    fn 시크릿_프로필은_이름이_비어_있어도_만든다(cx: &mut TestAppContext) {
        let host = host();
        let (_, _, _, cx) = menu(&host, cx);
        click("profile-button", cx);
        click("profile-create-incognito", cx);
        assert_eq!(host.calls()[0], "create_profile \"\" Incognito None");
    }

    #[gpui::test]
    fn 이름_입력란을_누르면_자리표시_글자_위로도_입력할_수_있다(
        cx: &mut TestAppContext,
    ) {
        let host = host();
        let (menu, _, _, cx) = menu(&host, cx);
        click("profile-button", cx);
        // The placeholder lies over the empty field: the press must still reach the field.
        click("profile-new-name", cx);
        cx.simulate_input("Research");
        cx.run_until_parked();
        assert_eq!(
            menu.read_with(cx, |menu, cx| menu.new_name.read(cx).text().to_owned()),
            "Research"
        );
        assert!(is_open(&menu, cx));
    }

    #[gpui::test]
    fn 만들기에_실패하면_이름과_메뉴를_그대로_둔다(cx: &mut TestAppContext) {
        let host = host();
        host.failing.borrow_mut().push("create_profile");
        let (menu, _, _, cx) = menu(&host, cx);
        click("profile-button", cx);
        menu.update(cx, |menu, cx| {
            menu.new_name
                .update(cx, |input, cx| input.set_text("Research", cx));
        });
        click("profile-create-persistent", cx);
        assert_eq!(host.calls().len(), 1);
        assert!(is_open(&menu, cx));
        assert_eq!(
            menu.read_with(cx, |menu, cx| menu.new_name.read(cx).text().to_owned()),
            "Research"
        );
    }

    #[gpui::test]
    fn 브라우저를_누르면_그_프로필들이_펼쳐지고_다시_누르면_접힌다(
        cx: &mut TestAppContext,
    ) {
        let host = host();
        let (menu, _, _, cx) = menu(&host, cx);
        click("profile-button", cx);
        assert_eq!(menu.read_with(cx, |menu, _| menu.expanded_browser), None);
        click("profile-browser-0", cx);
        assert_eq!(
            menu.read_with(cx, |menu, _| menu.expanded_browser),
            Some(BrowserId::Chrome)
        );
        assert!(cx.debug_bounds("profile-import-0-1").is_some());
        click("profile-browser-0", cx);
        assert_eq!(menu.read_with(cx, |menu, _| menu.expanded_browser), None);
    }

    #[gpui::test]
    fn 브라우저_프로필을_누르면_가져오고_결과를_메뉴에_적는다(
        cx: &mut TestAppContext,
    ) {
        let host = host();
        let (menu, _, _, cx) = menu(&host, cx);
        click("profile-button", cx);
        click("profile-browser-0", cx);
        click("profile-import-0-1", cx);
        assert_eq!(
            host.calls()[0],
            "create_profile \"태균\" Persistent Some(\"Chrome\")"
        );
        assert_eq!(
            host.calls().last().unwrap(),
            r#"browser_import {"browser":"chrome","profile":"Profile 1","hosts":[]}"#
        );
        assert_eq!(
            menu.read_with(cx, |menu, _| menu.import_msg().map(str::to_owned)),
            Some("✓ 태균: imported 3/4 cookies".to_owned())
        );
        // The menu stays open, showing the line.
        assert!(is_open(&menu, cx));
        assert!(cx.debug_bounds("profile-import-msg").is_some());
    }

    #[gpui::test]
    fn 가져오기에_실패하면_경고를_적는다(cx: &mut TestAppContext) {
        let host = host();
        *host.import.borrow_mut() = Some(Err("Keychain access denied".to_owned()));
        let (menu, _, _, cx) = menu(&host, cx);
        click("profile-button", cx);
        click("profile-browser-0", cx);
        click("profile-import-0-0", cx);
        assert_eq!(
            menu.read_with(cx, |menu, _| menu.import_msg().map(str::to_owned)),
            Some("⚠ Keychain access denied".to_owned())
        );
    }

    #[gpui::test]
    fn 바깥을_누르면_닫히고_그_클릭은_아래로_가지_않는다(
        cx: &mut TestAppContext,
    ) {
        let host = host();
        let (menu, _, presses, cx) = menu(&host, cx);
        click("profile-button", cx);
        let before = presses.get();
        cx.simulate_click(elsewhere(), Modifiers::none());
        cx.run_until_parked();
        assert!(!is_open(&menu, cx));
        assert_eq!(presses.get(), before);
    }

    #[gpui::test]
    fn 열린_채로_버튼을_다시_누르면_닫힌다(cx: &mut TestAppContext) {
        let host = host();
        let (menu, _, _, cx) = menu(&host, cx);
        click("profile-button", cx);
        click("profile-button", cx);
        assert!(!is_open(&menu, cx));
    }

    #[gpui::test]
    fn 브라우저가_없으면_가져오기_구역이_없다(cx: &mut TestAppContext) {
        let host = host();
        host.browsers.borrow_mut().clear();
        let (_, _, _, cx) = menu(&host, cx);
        click("profile-button", cx);
        assert!(cx.debug_bounds("profile-menu").is_some());
        assert!(cx.debug_bounds("profile-browser-0").is_none());
    }
}
