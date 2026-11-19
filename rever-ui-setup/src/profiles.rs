//! Port of `src/renderer/src/stores/profiles.ts` — named browsing profiles, mirrored from
//! the main side through the host. Persistent profiles survive restarts and back cookie
//! import; incognito profiles are ephemeral.
//!
//! The list is not persisted here (the TypeScript store has no storage key either): the
//! main side owns it, and [`ProfilesStore::load`] reads it back.
//!
//! Every method that awaits the host returns a `Task`. Unlike a promise, a dropped task
//! stops where it is: detach it to let it run to the end.

use std::rc::Rc;

use gpui::{App, Context, Task};
use rever_ui_state::tabs::AddTabOptions;
use serde::{Deserialize, Serialize};

use crate::host::{
    BrowserImportOptions, BrowserInfo, BrowserProfile, BrowserProfileInfo, ProfileKind, SetupHost,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProfileResult {
    pub ok: bool,
    pub imported: u64,
    pub total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub const DEFAULT_PROFILE_ID: &str = "default";
const NEW_TAB_URL: &str = "https://www.google.com";

/// The app going away while a call was in flight.
fn app_gone<E>(_: E) -> String {
    "app was released".to_owned()
}

/// The options of the tab [`ProfilesStore::open_tab`] opens: the Default profile browses
/// in the shared partition, every other one in its own.
pub fn tab_options(profile: &BrowserProfile) -> AddTabOptions {
    AddTabOptions {
        partition: (profile.id != DEFAULT_PROFILE_ID).then(|| profile.partition.clone()),
        profile_id: Some(profile.id.clone()),
        ..AddTabOptions::default()
    }
}

pub struct ProfilesStore {
    host: Rc<dyn SetupHost>,
    profiles: Vec<BrowserProfile>,
    loaded: bool,
}

impl ProfilesStore {
    pub fn new(host: Rc<dyn SetupHost>) -> Self {
        Self {
            host,
            profiles: Vec::new(),
            loaded: false,
        }
    }

    pub fn profiles(&self) -> &[BrowserProfile] {
        &self.profiles
    }

    pub fn loaded(&self) -> bool {
        self.loaded
    }

    pub fn load(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        let listed = self.host.list_profiles(cx);
        cx.spawn(async move |this, cx| {
            let profiles = listed.await?;
            this.update(cx, |this, cx| {
                this.profiles = profiles;
                this.loaded = true;
                cx.notify();
            })
            .map_err(app_gone)
        })
    }

    pub fn create(
        &mut self,
        name: String,
        kind: ProfileKind,
        source: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<BrowserProfile, String>> {
        let created = self.host.create_profile(name, kind, source, cx);
        cx.spawn(async move |this, cx| {
            let profile = created.await?;
            this.update(cx, |this, cx| {
                this.profiles.push(profile.clone());
                cx.notify();
            })
            .map_err(app_gone)?;
            Ok(profile)
        })
    }

    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        if id == DEFAULT_PROFILE_ID {
            return Task::ready(Ok(()));
        }
        let id = id.to_owned();
        let deleted = self.host.delete_profile(id.clone(), cx);
        cx.spawn(async move |this, cx| {
            deleted.await?;
            this.update(cx, |this, cx| {
                this.profiles.retain(|profile| profile.id != id);
                cx.notify();
            })
            .map_err(app_gone)
        })
    }

    /// Open a new tab that browses under the given profile.
    pub fn open_tab(&self, profile: &BrowserProfile, cx: &mut App) -> Task<Result<(), String>> {
        let id = self.host.add_tab(NEW_TAB_URL, tab_options(profile), cx);
        if profile.id == DEFAULT_PROFILE_ID {
            return Task::ready(Ok(()));
        }
        let (host, partition) = (self.host.clone(), profile.partition.clone());
        cx.spawn(async move |cx| {
            // Register the partition so cookie import / current-ip target this profile,
            // then re-assert the active tab (creation may have fired before this call).
            cx.update(|cx| host.register_tab_partition(id.clone(), partition, cx))
                .map_err(app_gone)?
                .await?;
            cx.update(|cx| host.set_active_tab(id, cx))
                .map_err(app_gone)?
                .await
        })
    }

    /// Create a persistent profile named after a real browser profile, seeded with that
    /// profile's cookies (all hosts), then open a tab in it.
    pub fn import_from_browser(
        &mut self,
        browser: &BrowserInfo,
        src_profile: &BrowserProfileInfo,
        cx: &mut Context<Self>,
    ) -> Task<Result<ImportProfileResult, String>> {
        // New persistent profile named after the source browser profile, sourced from
        // the browser (e.g. name "태균", source "Chrome").
        let created = self.create(
            src_profile.name.clone(),
            ProfileKind::Persistent,
            Some(browser.name.clone()),
            cx,
        );
        let host = self.host.clone();
        let opts = BrowserImportOptions {
            browser: browser.id,
            profile: Some(src_profile.id.clone()),
            hosts: Some(Vec::new()),
            shared: None,
        };
        cx.spawn(async move |this, cx| {
            let profile = created.await?;
            // open_tab makes this profile's partition the active session, so the import
            // below lands in it.
            this.update(cx, |this, cx| this.open_tab(&profile, cx))
                .map_err(app_gone)?
                .await?;
            let result = cx
                .update(|cx| host.browser_import(opts, cx))
                .map_err(app_gone)?
                .await?;
            Ok(ImportProfileResult {
                ok: result.ok,
                imported: result.imported,
                total: result.total,
                error: result.error,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, Entity, TestAppContext};

    use super::*;
    use crate::host::{BrowserId, BrowserImportResult};
    use crate::ui::testing::FakeHost;

    fn profile(id: &str, kind: ProfileKind) -> BrowserProfile {
        BrowserProfile {
            id: id.to_owned(),
            name: id.to_uppercase(),
            kind,
            partition: format!("persist:rever-profile-{id}"),
            source: None,
        }
    }

    fn chrome() -> (BrowserInfo, BrowserProfileInfo) {
        let source = BrowserProfileInfo {
            id: "Profile 1".to_owned(),
            name: "태균".to_owned(),
        };
        let browser = BrowserInfo {
            id: BrowserId::Chrome,
            name: "Chrome".to_owned(),
            profiles: vec![source.clone()],
        };
        (browser, source)
    }

    fn store(cx: &mut TestAppContext) -> (Rc<FakeHost>, Entity<ProfilesStore>) {
        let host = Rc::new(FakeHost::default());
        let store = cx.new({
            let host = host.clone();
            |_| ProfilesStore::new(host)
        });
        (host, store)
    }

    fn ids(store: &Entity<ProfilesStore>, cx: &mut TestAppContext) -> Vec<String> {
        store.read_with(cx, |store, _| {
            store.profiles().iter().map(|p| p.id.clone()).collect()
        })
    }

    #[test]
    fn 기본_프로필_탭은_공유_파티션에서_열린다() {
        let options = tab_options(&profile("default", ProfileKind::Persistent));
        assert_eq!(options.partition, None);
        assert_eq!(options.profile_id.as_deref(), Some("default"));
        assert!(options.activate);
        assert_eq!(options.proxy, None);
    }

    #[test]
    fn 다른_프로필_탭은_그_프로필의_파티션에서_열린다() {
        let options = tab_options(&profile("work", ProfileKind::Incognito));
        assert_eq!(
            options.partition.as_deref(),
            Some("persist:rever-profile-work")
        );
        assert_eq!(options.profile_id.as_deref(), Some("work"));
    }

    #[test]
    fn 가져오기_결과는_카멜_케이스로_직렬화하고_없는_error는_뺀다() {
        let result = ImportProfileResult {
            ok: true,
            imported: 3,
            total: 4,
            error: None,
        };
        assert_eq!(
            serde_json::to_string(&result).unwrap(),
            r#"{"ok":true,"imported":3,"total":4}"#
        );
    }

    #[gpui::test]
    async fn 처음에는_비어_있고_load가_목록을_채운다(cx: &mut TestAppContext) {
        let (host, store) = store(cx);
        *host.profiles.borrow_mut() = vec![
            profile("default", ProfileKind::Persistent),
            profile("work", ProfileKind::Persistent),
        ];
        assert!(ids(&store, cx).is_empty());
        assert!(!store.read_with(cx, |store, _| store.loaded()));

        store.update(cx, |store, cx| store.load(cx)).await.unwrap();
        assert_eq!(ids(&store, cx), ["default", "work"]);
        assert!(store.read_with(cx, |store, _| store.loaded()));
    }

    #[gpui::test]
    async fn create는_만든_프로필을_목록_끝에_붙이고_돌려준다(
        cx: &mut TestAppContext,
    ) {
        let (host, store) = store(cx);
        *host.profiles.borrow_mut() = vec![profile("default", ProfileKind::Persistent)];
        store.update(cx, |store, cx| store.load(cx)).await.unwrap();

        let created = store
            .update(cx, |store, cx| {
                store.create("work".to_owned(), ProfileKind::Incognito, None, cx)
            })
            .await
            .unwrap();
        assert_eq!(created.id, "p-work");
        assert_eq!(created.kind, ProfileKind::Incognito);
        assert_eq!(ids(&store, cx), ["default", "p-work"]);
        assert_eq!(
            host.calls().last().unwrap(),
            r#"create_profile "work" Incognito None"#
        );
    }

    #[gpui::test]
    async fn create가_실패하면_목록은_그대로다(cx: &mut TestAppContext) {
        let (host, store) = store(cx);
        host.failing.borrow_mut().push("create_profile");
        let created = store
            .update(cx, |store, cx| {
                store.create("work".to_owned(), ProfileKind::Persistent, None, cx)
            })
            .await;
        assert_eq!(created, Err("create_profile failed".to_owned()));
        assert!(ids(&store, cx).is_empty());
    }

    #[gpui::test]
    async fn remove는_호스트에서_지운_뒤_목록에서_뺀다(cx: &mut TestAppContext) {
        let (host, store) = store(cx);
        *host.profiles.borrow_mut() = vec![
            profile("default", ProfileKind::Persistent),
            profile("work", ProfileKind::Persistent),
        ];
        store.update(cx, |store, cx| store.load(cx)).await.unwrap();

        store
            .update(cx, |store, cx| store.remove("work", cx))
            .await
            .unwrap();
        assert_eq!(ids(&store, cx), ["default"]);
        assert_eq!(host.calls().last().unwrap(), "delete_profile work");
    }

    #[gpui::test]
    async fn 기본_프로필은_지우지_않는다(cx: &mut TestAppContext) {
        let (host, store) = store(cx);
        *host.profiles.borrow_mut() = vec![profile("default", ProfileKind::Persistent)];
        store.update(cx, |store, cx| store.load(cx)).await.unwrap();

        store
            .update(cx, |store, cx| store.remove("default", cx))
            .await
            .unwrap();
        assert_eq!(ids(&store, cx), ["default"]);
        assert_eq!(host.calls(), ["list_profiles"]);
    }

    #[gpui::test]
    async fn 삭제에_실패하면_목록에_남는다(cx: &mut TestAppContext) {
        let (host, store) = store(cx);
        *host.profiles.borrow_mut() = vec![profile("work", ProfileKind::Persistent)];
        store.update(cx, |store, cx| store.load(cx)).await.unwrap();
        host.failing.borrow_mut().push("delete_profile");

        let removed = store.update(cx, |store, cx| store.remove("work", cx)).await;
        assert_eq!(removed, Err("delete_profile failed".to_owned()));
        assert_eq!(ids(&store, cx), ["work"]);
    }

    #[gpui::test]
    async fn 기본_프로필로_탭을_열면_파티션을_등록하지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (host, store) = store(cx);
        let default = profile("default", ProfileKind::Persistent);
        store
            .update(cx, |store, cx| store.open_tab(&default, cx))
            .await
            .unwrap();
        assert_eq!(
            host.calls(),
            [
                "add_tab t1 https://www.google.com partition=None profile=Some(\"default\") proxy=false"
            ]
        );
    }

    #[gpui::test]
    async fn 다른_프로필로_탭을_열면_파티션을_등록하고_활성_탭을_다시_알린다(
        cx: &mut TestAppContext,
    ) {
        let (host, store) = store(cx);
        let work = profile("work", ProfileKind::Persistent);
        store
            .update(cx, |store, cx| store.open_tab(&work, cx))
            .await
            .unwrap();
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
    async fn 파티션_등록에_실패하면_활성_탭을_알리지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (host, store) = store(cx);
        host.failing.borrow_mut().push("register_tab_partition");
        let work = profile("work", ProfileKind::Persistent);
        let opened = store
            .update(cx, |store, cx| store.open_tab(&work, cx))
            .await;
        assert_eq!(opened, Err("register_tab_partition failed".to_owned()));
        assert!(
            !host
                .calls()
                .iter()
                .any(|call| call.starts_with("set_active_tab"))
        );
    }

    #[gpui::test]
    async fn 브라우저에서_가져오면_프로필을_만들고_탭을_연_뒤_모든_호스트의_쿠키를_가져온다(
        cx: &mut TestAppContext,
    ) {
        let (host, store) = store(cx);
        let (browser, source) = chrome();
        *host.import.borrow_mut() = Some(Ok(BrowserImportResult {
            ok: true,
            imported: 120,
            skipped: 2,
            undecryptable: 5,
            total: 127,
            error: None,
        }));

        let result = store
            .update(cx, |store, cx| {
                store.import_from_browser(&browser, &source, cx)
            })
            .await
            .unwrap();
        assert_eq!(
            result,
            ImportProfileResult {
                ok: true,
                imported: 120,
                total: 127,
                error: None
            }
        );
        // The new profile is named after the source profile and sourced from the browser.
        assert_eq!(
            host.calls(),
            [
                "create_profile \"태균\" Persistent Some(\"Chrome\")",
                "add_tab t1 https://www.google.com partition=Some(\"persist:rever-profile-태균\") profile=Some(\"p-태균\") proxy=false",
                "register_tab_partition t1 persist:rever-profile-태균",
                "set_active_tab t1",
                r#"browser_import {"browser":"chrome","profile":"Profile 1","hosts":[]}"#,
            ]
        );
        assert_eq!(ids(&store, cx), ["p-태균"]);
    }

    #[gpui::test]
    async fn 가져오기가_실패를_돌려주면_그_오류를_그대로_전한다(
        cx: &mut TestAppContext,
    ) {
        let (host, store) = store(cx);
        let (browser, source) = chrome();
        *host.import.borrow_mut() = Some(Ok(BrowserImportResult {
            ok: false,
            error: Some("Keychain access denied".to_owned()),
            ..Default::default()
        }));
        let result = store
            .update(cx, |store, cx| {
                store.import_from_browser(&browser, &source, cx)
            })
            .await
            .unwrap();
        assert!(!result.ok);
        assert_eq!(result.error.as_deref(), Some("Keychain access denied"));
        // The profile and its tab exist all the same, as in the TypeScript.
        assert_eq!(ids(&store, cx), ["p-태균"]);
    }

    #[gpui::test]
    async fn 프로필을_만들지_못하면_탭도_열지_않고_가져오지도_않는다(
        cx: &mut TestAppContext,
    ) {
        let (host, store) = store(cx);
        let (browser, source) = chrome();
        host.failing.borrow_mut().push("create_profile");
        let result = store
            .update(cx, |store, cx| {
                store.import_from_browser(&browser, &source, cx)
            })
            .await;
        assert_eq!(result, Err("create_profile failed".to_owned()));
        assert_eq!(host.calls().len(), 1);
    }
}
