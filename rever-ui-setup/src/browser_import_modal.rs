//! Port of `src/renderer/src/components/BrowserImportModal.tsx` — one-click "bring your
//! logins over" import, Aside-style. Offered once after first-run onboarding (macOS only)
//! and reopened from the app menu. Cookies go into the shared browsing session; reading
//! the source browser's Safe Storage key makes macOS show its Keychain prompt. Saved
//! passwords are never read.
//!
//! The first-run offer waits until the provider onboarding modal closes: the host
//! notifies this view when that happens (it reads [`SetupHost::onboarded`] on render).

use std::rc::Rc;

use gpui::{
    AnyElement, Context, Empty, FontWeight, SharedString, Window, anchored, deferred, div, point,
    prelude::*, px, rgba,
};
use rever_ui_kit::i18n::Lang;
use rever_ui_state::Storage;

use crate::host::{BrowserId, BrowserImportOptions, BrowserImportResult, BrowserInfo, SetupHost};
use crate::ui::{
    Anchor, DISABLED_OPACITY, LookFn, SANS, Tokens, Z_MODAL, fill, group_thousands, link_button,
    select_box, select_list, shadow, text, use_button,
};

const OFFERED_KEY: &str = "rev:browser-import-offered";

/// `useBrowserImport`'s first value: the offer is open until it has been closed once,
/// and only on a Mac.
pub fn initially_open(storage: &dyn Storage, is_mac: bool) -> bool {
    storage.get(OFFERED_KEY).is_none() && is_mac
}

/// The profile a browser starts on: its `Default` one, else its first, else none.
pub fn default_profile(browser: Option<&BrowserInfo>) -> String {
    let Some(browser) = browser else {
        return String::new();
    };
    browser
        .profiles
        .iter()
        .find(|profile| profile.id == "Default")
        .or_else(|| browser.profiles.first())
        .map(|profile| profile.id.clone())
        .unwrap_or_default()
}

/// The line under the fields once an import has ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportOutcome {
    pub ok: bool,
    pub text: String,
}

/// What an import's answer reads as. `Err` is the rejected call's message.
pub fn import_outcome(result: Result<BrowserImportResult, String>, lang: Lang) -> ImportOutcome {
    let result = match result {
        Ok(result) => result,
        Err(message) => {
            return ImportOutcome {
                ok: false,
                text: message,
            };
        }
    };
    if !result.ok {
        return ImportOutcome {
            ok: false,
            // `??`: an empty error stays an empty line.
            text: result.error.unwrap_or_else(|| "import failed".to_owned()),
        };
    }
    let mut parts = vec![fill(
        lang.t("import.done"),
        &[("n", &group_thousands(result.imported))],
    )];
    if result.undecryptable > 0 {
        parts.push(fill(
            lang.t("import.appBound"),
            &[("n", &group_thousands(result.undecryptable))],
        ));
    }
    ImportOutcome {
        ok: true,
        text: parts.join(" · "),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenSelect {
    Browser,
    Profile,
}

pub struct BrowserImportModal {
    host: Rc<dyn SetupHost>,
    storage: Rc<dyn Storage>,
    look: LookFn,
    /// `useBrowserImport`'s `open`.
    open: bool,
    /// Whether the last render showed the modal: the browsers are listed anew each time
    /// it appears.
    shown: bool,
    /// `None` until the list has arrived.
    browsers: Option<Vec<BrowserInfo>>,
    browser_id: Option<BrowserId>,
    profile: String,
    busy: bool,
    result: Option<ImportOutcome>,
    open_select: Option<OpenSelect>,
    browser_anchor: Anchor,
    profile_anchor: Anchor,
}

impl BrowserImportModal {
    pub fn new(host: Rc<dyn SetupHost>, storage: Rc<dyn Storage>, look: LookFn) -> Self {
        Self {
            // `navigator.userAgent.includes('Mac')`
            open: initially_open(storage.as_ref(), cfg!(target_os = "macos")),
            host,
            storage,
            look,
            shown: false,
            browsers: None,
            browser_id: None,
            profile: String::new(),
            busy: false,
            result: None,
            open_select: None,
            browser_anchor: Anchor::default(),
            profile_anchor: Anchor::default(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open the modal again: the app menu's entry (`useBrowserImport.setState({ open: true })`).
    pub fn open(&mut self, cx: &mut Context<Self>) {
        self.open = true;
        cx.notify();
    }

    pub fn result(&self) -> Option<&ImportOutcome> {
        self.result.as_ref()
    }

    fn selected(&self) -> Option<&BrowserInfo> {
        let id = self.browser_id?;
        self.browsers
            .as_ref()?
            .iter()
            .find(|browser| browser.id == id)
    }

    /// The modal has just appeared: forget the last result and list the browsers.
    fn appeared(&mut self, cx: &mut Context<Self>) {
        self.result = None;
        let listed = self.host.browsers(cx);
        cx.spawn(async move |this, cx| {
            let list = listed.await;
            let _ = this.update(cx, |this, cx| {
                this.browser_id = list.first().map(|browser| browser.id);
                this.profile = default_profile(list.first());
                this.browsers = Some(list);
                cx.notify();
            });
        })
        .detach();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        // A failed write only means the offer comes back on the next start.
        let _ = self.storage.set(OFFERED_KEY, "1");
        self.open = false;
        self.open_select = None;
        cx.notify();
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = self.browser_id else {
            return;
        };
        self.busy = true;
        self.result = None;
        self.open_select = None;
        cx.notify();
        let lang = (self.look)(cx).lang;
        let imported = self.host.browser_import(
            BrowserImportOptions {
                browser,
                profile: Some(self.profile.clone()),
                hosts: None,
                shared: Some(true),
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let outcome = import_outcome(imported.await, lang);
            let _ = this.update(cx, |this, cx| {
                this.result = Some(outcome);
                this.busy = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_select(&mut self, which: OpenSelect, cx: &mut Context<Self>) {
        self.open_select = (self.open_select != Some(which)).then_some(which);
        cx.notify();
    }

    fn dismiss_select(&mut self, cx: &mut Context<Self>) {
        self.open_select = None;
        cx.notify();
    }

    fn pick_browser(&mut self, index: usize, cx: &mut Context<Self>) {
        let browser = self.browsers.as_ref().and_then(|list| list.get(index));
        if let Some(browser) = browser {
            self.browser_id = Some(browser.id);
            self.profile = default_profile(Some(browser));
        }
        self.dismiss_select(cx);
    }

    fn pick_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        let profile = self
            .selected()
            .and_then(|browser| browser.profiles.get(index));
        if let Some(profile) = profile {
            self.profile = profile.id.clone();
        }
        self.dismiss_select(cx);
    }
}

/// `fieldLabel`: a caption over its select.
fn field(label: &'static str, tokens: &Tokens) -> gpui::Div {
    text(11.0)
        .flex()
        .flex_col()
        .flex_1()
        .min_w(px(140.0))
        .gap(px(4.0))
        .text_color(tokens.theme.text_2)
        .child(label)
}

impl Render for BrowserImportModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The first-run offer waits until the provider onboarding modal closes.
        let visible = self.open && self.host.onboarded(cx);
        if visible && !self.shown {
            self.appeared(cx);
        }
        self.shown = visible;
        if !visible {
            return Empty.into_any_element();
        }
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let lang = look.lang;
        let busy = self.busy;
        let done = self.result.as_ref().is_some_and(|result| result.ok);
        let none_found = self.browsers.as_ref().is_some_and(Vec::is_empty);
        let any_found = self.browsers.as_ref().is_some_and(|list| !list.is_empty());

        // `selectStyle`
        let select = |id, label: SharedString, anchor: &Anchor, disabled: bool| {
            select_box(id, label, anchor, disabled)
                .px(px(8.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .bg(theme.bg_bar)
                .border_color(theme.border_2)
                .text_size(px(12.0))
                .line_height(px(15.0))
                .text_color(theme.text)
        };

        let selected = self.selected().cloned();
        let mut list: Option<AnyElement> = None;
        let fields = if none_found {
            div()
                .mt(px(16.0))
                .text_size(px(12.0))
                .line_height(px(14.0))
                .child(lang.t("import.none"))
        } else {
            let browser_disabled = busy || self.browsers.is_none();
            let browser_label = selected
                .as_ref()
                .map(|browser| browser.name.clone())
                .unwrap_or_default();
            let mut fields =
                div().flex().flex_wrap().gap(px(8.0)).mt(px(16.0)).child(
                    field(lang.t("import.browser"), &tokens).child(
                        select(
                            "import-browser",
                            browser_label.into(),
                            &self.browser_anchor,
                            browser_disabled,
                        )
                        .when(!browser_disabled, |select| {
                            select.on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_select(OpenSelect::Browser, cx)
                            }))
                        }),
                    ),
                );
            if self.open_select == Some(OpenSelect::Browser) {
                let browsers = self.browsers.as_deref().unwrap_or_default();
                let options: Vec<SharedString> =
                    browsers.iter().map(|b| b.name.clone().into()).collect();
                list = Some(select_list(
                    "import-browser-list",
                    &options,
                    browsers.iter().position(|b| Some(b.id) == self.browser_id),
                    &self.browser_anchor,
                    &tokens,
                    cx,
                    Self::pick_browser,
                    Self::dismiss_select,
                ));
            }
            // A browser with a single profile needs no second choice.
            if let Some(browser) = selected.as_ref().filter(|b| b.profiles.len() > 1) {
                let at = browser.profiles.iter().position(|p| p.id == self.profile);
                // A value none of the options has shows the first one, as a `<select>` does.
                let label = browser
                    .profiles
                    .get(at.unwrap_or(0))
                    .map(|profile| profile.name.clone())
                    .unwrap_or_default();
                fields = fields.child(field(lang.t("import.profile"), &tokens).child(
                    select("import-profile", label.into(), &self.profile_anchor, busy).when(
                        !busy,
                        |select| {
                            select.on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_select(OpenSelect::Profile, cx)
                            }))
                        },
                    ),
                ));
                if self.open_select == Some(OpenSelect::Profile) {
                    let options: Vec<SharedString> = browser
                        .profiles
                        .iter()
                        .map(|p| p.name.clone().into())
                        .collect();
                    list = Some(select_list(
                        "import-profile-list",
                        &options,
                        at,
                        &self.profile_anchor,
                        &tokens,
                        cx,
                        Self::pick_profile,
                        Self::dismiss_select,
                    ));
                }
            }
            fields
        };

        let result = self.result.as_ref().map(|result| {
            div()
                .debug_selector(|| "import-result".into())
                .mt(px(12.0))
                .text_size(px(12.0))
                .line_height(px(14.0))
                .text_color(if result.ok {
                    tokens.status_ok
                } else {
                    theme.status_error
                })
                .child(result.text.clone())
        });

        let run = (!done && any_found).then(|| {
            let label = if busy {
                lang.t("import.running")
            } else {
                lang.t("import.run")
            };
            use_button("import-run", label, &tokens).map(|button| {
                if busy {
                    button.opacity(DISABLED_OPACITY)
                } else {
                    button
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| this.run(cx)))
                }
            })
        });
        let footer = div()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(12.0))
            .mt(px(16.0))
            .child(
                link_button(
                    "import-close",
                    if done {
                        lang.t("import.close")
                    } else {
                        lang.t("import.later")
                    },
                    busy,
                    &tokens,
                )
                .when(!busy, |button| {
                    button.on_click(cx.listener(|this, _, _, cx| this.close(cx)))
                }),
            )
            .children(run);

        // The onboarding modal's `card`.
        let card = div()
            .id("import-card")
            .debug_selector(|| "import-card".into())
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
            .shadow(vec![shadow(16.0, 48.0, rgba(0x0000008c))])
            .child(
                div()
                    .text_size(px(18.0))
                    .line_height(px(21.0))
                    .font_weight(FontWeight::BOLD)
                    .child(lang.t("import.title")),
            )
            .child(fields)
            .children(result)
            .child(footer);

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
            .child(card)
            // After the card, so the open list covers it.
            .children(list);
        deferred(anchored().position(point(px(0.0), px(0.0))).child(backdrop))
            .with_priority(Z_MODAL)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use rever_ui_state::MemoryStorage;

    use super::*;
    use crate::host::BrowserProfileInfo;

    fn browser(profiles: &[&str]) -> BrowserInfo {
        BrowserInfo {
            id: BrowserId::Chrome,
            name: "Chrome".to_owned(),
            profiles: profiles
                .iter()
                .map(|id| BrowserProfileInfo {
                    id: (*id).to_owned(),
                    name: format!("{id} name"),
                })
                .collect(),
        }
    }

    fn imported(imported: u64, undecryptable: u64) -> Result<BrowserImportResult, String> {
        Ok(BrowserImportResult {
            ok: true,
            imported,
            undecryptable,
            ..Default::default()
        })
    }

    #[test]
    fn 맥에서_한_번도_닫지_않았을_때만_처음부터_열려_있다() {
        let storage = MemoryStorage::new();
        assert!(initially_open(&storage, true));
        assert!(!initially_open(&storage, false));
        storage.set("rev:browser-import-offered", "1").unwrap();
        assert!(!initially_open(&storage, true));
        // Any stored value counts, an empty one too (`getItem(...) === null`).
        storage.set("rev:browser-import-offered", "").unwrap();
        assert!(!initially_open(&storage, true));
    }

    #[test]
    fn 기본_프로필은_default이고_없으면_첫_프로필이다() {
        assert_eq!(
            default_profile(Some(&browser(&["Profile 1", "Default"]))),
            "Default"
        );
        assert_eq!(
            default_profile(Some(&browser(&["Profile 1", "Profile 2"]))),
            "Profile 1"
        );
        assert_eq!(default_profile(Some(&browser(&[]))), "");
        assert_eq!(default_profile(None), "");
    }

    #[test]
    fn 성공하면_가져온_쿠키_수를_천_단위로_끊어_보여준다() {
        assert_eq!(
            import_outcome(imported(12_345, 0), Lang::En),
            ImportOutcome {
                ok: true,
                text: "Imported 12,345 cookies".to_owned()
            }
        );
        assert_eq!(
            import_outcome(imported(7, 0), Lang::Ko).text,
            "쿠키 7개를 가져왔습니다"
        );
    }

    #[test]
    fn 복호화하지_못한_쿠키가_있으면_그_수를_뒤에_붙인다() {
        assert_eq!(
            import_outcome(imported(980, 1_204), Lang::En).text,
            "Imported 980 cookies · 1,204 could not be decrypted and were skipped"
        );
    }

    #[test]
    fn 실패는_오류_문구를_그대로_보여주고_없으면_기본_문구를_쓴다() {
        let failed = |error: Option<&str>| {
            import_outcome(
                Ok(BrowserImportResult {
                    ok: false,
                    imported: 5,
                    error: error.map(str::to_owned),
                    ..Default::default()
                }),
                Lang::En,
            )
        };
        assert_eq!(
            failed(Some("Keychain access denied")),
            ImportOutcome {
                ok: false,
                text: "Keychain access denied".to_owned()
            }
        );
        assert_eq!(failed(None).text, "import failed");
        // `??` keeps an empty string.
        assert_eq!(failed(Some("")).text, "");
    }

    #[test]
    fn 호출_자체가_실패하면_그_메시지를_보여준다() {
        assert_eq!(
            import_outcome(Err("IPC channel closed".to_owned()), Lang::Ko),
            ImportOutcome {
                ok: false,
                text: "IPC channel closed".to_owned()
            }
        );
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::Cell;

    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
    use rever_ui_state::MemoryStorage;

    use super::*;
    use crate::host::BrowserProfileInfo;
    use crate::ui::testing::{FakeHost, click, elsewhere, look, stage};

    fn profile(id: &str, name: &str) -> BrowserProfileInfo {
        BrowserProfileInfo {
            id: id.to_owned(),
            name: name.to_owned(),
        }
    }

    /// Chrome with two profiles (the second is `Default`) and Arc with one.
    fn two_browsers() -> Vec<BrowserInfo> {
        vec![
            BrowserInfo {
                id: BrowserId::Chrome,
                name: "Chrome".to_owned(),
                profiles: vec![profile("Profile 1", "Work"), profile("Default", "Personal")],
            },
            BrowserInfo {
                id: BrowserId::Arc,
                name: "Arc".to_owned(),
                profiles: vec![profile("Default", "Default")],
            },
        ]
    }

    struct Fixture {
        host: Rc<FakeHost>,
        storage: Rc<MemoryStorage>,
    }

    /// A host past onboarding, with the offer already closed once: the tests open the
    /// modal themselves, so they do not depend on the platform they run on.
    fn fixture(browsers: Vec<BrowserInfo>) -> Fixture {
        let host = Rc::new(FakeHost::default());
        host.onboarded.set(true);
        *host.browsers.borrow_mut() = browsers;
        let storage = Rc::new(MemoryStorage::new());
        storage.set(OFFERED_KEY, "1").unwrap();
        Fixture { host, storage }
    }

    fn modal<'a>(
        fixture: &Fixture,
        cx: &'a mut TestAppContext,
    ) -> (
        Entity<BrowserImportModal>,
        Rc<Cell<usize>>,
        &'a mut VisualTestContext,
    ) {
        let (host, storage) = (fixture.host.clone(), fixture.storage.clone());
        let (modal, presses, cx) =
            stage(cx, move |_| BrowserImportModal::new(host, storage, look()));
        modal.update(cx, |modal, cx| modal.open(cx));
        cx.run_until_parked();
        (modal, presses, cx)
    }

    #[gpui::test]
    fn 이미_제안을_닫았으면_처음에는_열리지_않는다(cx: &mut TestAppContext) {
        let fixture = fixture(two_browsers());
        let (host, storage) = (fixture.host.clone(), fixture.storage.clone());
        let (modal, _, cx) = stage(cx, move |_| BrowserImportModal::new(host, storage, look()));
        assert!(!modal.read_with(cx, |modal, _| modal.is_open()));
        assert!(cx.debug_bounds("import-card").is_none());
        assert!(fixture.host.calls().is_empty());
    }

    #[gpui::test]
    fn 온보딩이_끝날_때까지는_열려_있어도_보이지_않는다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(two_browsers());
        fixture.host.onboarded.set(false);
        let (modal, presses, cx) = modal(&fixture, cx);
        assert!(modal.read_with(cx, |modal, _| modal.is_open()));
        assert!(fixture.host.calls().is_empty());
        cx.simulate_click(elsewhere(), Modifiers::none());
        assert_eq!(presses.get(), 1);

        // Onboarding closes: the host notifies the view, which lists the browsers.
        fixture.host.onboarded.set(true);
        modal.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert_eq!(fixture.host.calls(), ["browsers"]);
        assert!(cx.debug_bounds("import-run").is_some());
    }

    #[gpui::test]
    fn 열리면_첫_브라우저와_그_default_프로필이_골라져_있다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(two_browsers());
        let (modal, presses, cx) = modal(&fixture, cx);
        modal.read_with(cx, |modal, _| {
            assert_eq!(modal.browser_id, Some(BrowserId::Chrome));
            assert_eq!(modal.profile, "Default");
        });
        // Two profiles: the second select is there.
        assert!(cx.debug_bounds("import-profile").is_some());
        let card = cx.debug_bounds("import-card").unwrap();
        assert_eq!(card.size.width, px(502.0));
        // Nothing under the modal takes the mouse.
        cx.simulate_click(elsewhere(), Modifiers::none());
        assert_eq!(presses.get(), 0);
    }

    #[gpui::test]
    fn 브라우저를_바꾸면_프로필도_그_브라우저의_기본으로_바뀐다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(two_browsers());
        let (modal, _, cx) = modal(&fixture, cx);
        modal.update(cx, |modal, cx| {
            modal.pick_profile(0, cx);
            assert_eq!(modal.profile, "Profile 1");
        });
        click("import-browser", cx);
        assert_eq!(
            modal.read_with(cx, |modal, _| modal.open_select),
            Some(OpenSelect::Browser)
        );
        // The list opens under the select, as wide as it.
        let select = cx.debug_bounds("import-browser").unwrap();
        let list = cx.debug_bounds("import-browser-list").unwrap();
        // The centered card can sit on a half pixel; the list snaps to a whole one.
        assert!((list.top() - select.bottom() - px(2.0)).abs() < px(1.0));
        assert!((list.left() - select.left()).abs() < px(1.0));
        assert_eq!(list.size.width, select.size.width);

        click("import-browser-list-1", cx);
        modal.read_with(cx, |modal, _| {
            assert_eq!(modal.browser_id, Some(BrowserId::Arc));
            assert_eq!(modal.profile, "Default");
            assert_eq!(modal.open_select, None);
        });
    }

    #[gpui::test]
    fn 프로필이_하나뿐인_브라우저에는_프로필_선택이_없다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(two_browsers().split_off(1));
        let (_, _, cx) = modal(&fixture, cx);
        assert!(cx.debug_bounds("import-browser").is_some());
        assert!(cx.debug_bounds("import-profile").is_none());
    }

    #[gpui::test]
    fn 목록_밖을_누르면_목록만_닫힌다(cx: &mut TestAppContext) {
        let fixture = fixture(two_browsers());
        let (modal, _, cx) = modal(&fixture, cx);
        click("import-profile", cx);
        assert_eq!(
            modal.read_with(cx, |modal, _| modal.open_select),
            Some(OpenSelect::Profile)
        );
        // On the Import button: the press closes the list and goes no further.
        click("import-run", cx);
        modal.read_with(cx, |modal, _| {
            assert_eq!(modal.open_select, None);
            assert!(modal.is_open());
        });
        assert_eq!(fixture.host.calls(), ["browsers"]);
    }

    #[gpui::test]
    fn 가져오기는_고른_브라우저와_프로필을_공유_세션으로_가져온다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(two_browsers());
        *fixture.host.import.borrow_mut() = Some(Ok(BrowserImportResult {
            ok: true,
            imported: 1_500,
            undecryptable: 3,
            total: 1_503,
            ..Default::default()
        }));
        let (modal, _, cx) = modal(&fixture, cx);
        click("import-run", cx);
        assert_eq!(
            fixture.host.calls().last().unwrap(),
            r#"browser_import {"browser":"chrome","profile":"Default","shared":true}"#
        );
        modal.read_with(cx, |modal, _| {
            assert!(!modal.busy);
            assert_eq!(
                modal.result(),
                Some(&ImportOutcome {
                    ok: true,
                    text: "Imported 1,500 cookies · 3 could not be decrypted and were skipped"
                        .to_owned()
                })
            );
        });
        assert!(cx.debug_bounds("import-result").is_some());
    }

    #[gpui::test]
    fn 성공한_뒤에는_가져오기_버튼이_사라지고_닫기만_남는다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(two_browsers());
        let (modal, _, cx) = modal(&fixture, cx);
        click("import-run", cx);
        // Painted bounds outlive the element: a second click must not import again.
        click("import-run", cx);
        let imports = fixture
            .host
            .calls()
            .iter()
            .filter(|call| call.starts_with("browser_import"))
            .count();
        assert_eq!(imports, 1);
        click("import-close", cx);
        assert!(!modal.read_with(cx, |modal, _| modal.is_open()));
    }

    #[gpui::test]
    fn 실패하면_오류를_보여주고_다시_시도할_수_있다(cx: &mut TestAppContext) {
        let fixture = fixture(two_browsers());
        *fixture.host.import.borrow_mut() = Some(Err("Keychain access denied".to_owned()));
        let (modal, _, cx) = modal(&fixture, cx);
        click("import-run", cx);
        assert_eq!(
            modal.read_with(cx, |modal, _| modal.result().cloned()),
            Some(ImportOutcome {
                ok: false,
                text: "Keychain access denied".to_owned()
            })
        );
        *fixture.host.import.borrow_mut() = None;
        click("import-run", cx);
        assert!(modal.read_with(cx, |modal, _| modal.result().is_some_and(|r| r.ok)));
    }

    #[gpui::test]
    fn 닫으면_제안했음을_저장하고_다음에는_처음부터_열리지_않는다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(two_browsers());
        fixture.storage.remove(OFFERED_KEY).unwrap();
        let (modal, presses, cx) = modal(&fixture, cx);
        click("import-close", cx);
        assert!(!modal.read_with(cx, |modal, _| modal.is_open()));
        assert_eq!(fixture.storage.get(OFFERED_KEY).as_deref(), Some("1"));
        assert!(!initially_open(fixture.storage.as_ref(), true));
        cx.simulate_click(elsewhere(), Modifiers::none());
        assert_eq!(presses.get(), 1);
    }

    #[gpui::test]
    fn 다시_열면_지난_결과를_지우고_브라우저를_새로_읽는다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(two_browsers());
        let (modal, _, cx) = modal(&fixture, cx);
        click("import-run", cx);
        click("import-close", cx);
        modal.update(cx, |modal, cx| modal.open(cx));
        cx.run_until_parked();
        assert_eq!(
            modal.read_with(cx, |modal, _| modal.result().cloned()),
            None
        );
        let listed = fixture
            .host
            .calls()
            .iter()
            .filter(|call| *call == "browsers")
            .count();
        assert_eq!(listed, 2);
    }

    #[gpui::test]
    fn 브라우저가_없으면_없다는_문구만_있고_가져오기_버튼이_없다(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture(Vec::new());
        let (modal, _, cx) = modal(&fixture, cx);
        assert!(cx.debug_bounds("import-browser").is_none());
        assert!(cx.debug_bounds("import-run").is_none());
        modal.update(cx, |modal, cx| modal.run(cx));
        cx.run_until_parked();
        assert_eq!(fixture.host.calls(), ["browsers"]);
        click("import-close", cx);
        assert!(!modal.read_with(cx, |modal, _| modal.is_open()));
    }
}
