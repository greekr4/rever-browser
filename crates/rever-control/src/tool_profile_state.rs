//! Port of `src/main/mcp/tool-profile-state.ts` — the active tool profile: loaded
//! once from `tool-profile.json`, written back on every change, and pushed to
//! the MCP sessions that follow it.
//!
//! The TypeScript kept this in module-level variables under Electron's userData;
//! here it is one [`ToolProfileState`] per [`ToolContext`], stored next to the
//! other settings in `ToolContext::data_dir`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use rever_tool_profile::tool_profile::{
    TOOL_PROFILE_PROBE_REJECTED, ToolProfile, advertised_names, read_tool_profile,
};
use rever_tools_api::tool::ToolContext;
use serde::Serialize;
use serde_json::json;

use crate::write_private_file;

const TOOL_PROFILE_FILE: &str = "tool-profile.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileSource {
    User,
    Agent,
}

/// What a successful [`ToolProfileState::set`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileChange {
    pub profile: ToolProfile,
    pub advertised: usize,
}

/// An MCP session whose catalog follows the profile. It owns its tool handles,
/// so the state only tells it which profile to apply.
pub trait FollowServer: Send + Sync {
    fn apply_follow_catalog(&self, profile: ToolProfile);
}

type Listener = Arc<dyn Fn(ToolProfile) + Send + Sync>;

struct Inner {
    loaded: bool,
    current: ToolProfile,
    follow_servers: Vec<Arc<dyn FollowServer>>,
    listeners: Vec<Listener>,
}

pub struct ToolProfileState {
    file: PathBuf,
    inner: Mutex<Inner>,
}

/// Where a [`ToolContext`] keeps its one state (`ToolContext::state` needs `Default`).
#[derive(Default)]
struct Slot(OnceLock<Arc<ToolProfileState>>);

/// The address of the session, without the vtable half of the wide pointer:
/// two `Arc<dyn FollowServer>` to the same session must compare equal.
fn identity(server: &Arc<dyn FollowServer>) -> *const () {
    Arc::as_ptr(server).cast()
}

impl ToolProfileState {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            file: data_dir.join(TOOL_PROFILE_FILE),
            inner: Mutex::new(Inner {
                loaded: false,
                current: ToolProfile::Browse,
                follow_servers: Vec::new(),
                listeners: Vec::new(),
            }),
        }
    }

    /// The state every tool and the server share for this context.
    pub fn of(context: &ToolContext) -> Arc<Self> {
        context
            .state::<Slot>()
            .0
            .get_or_init(|| Arc::new(Self::new(&context.data_dir)))
            .clone()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_profile_file(&self, profile: ToolProfile) {
        let text = serde_json::to_string_pretty(&json!({ "version": 1, "profile": profile }));
        let written = text
            .map_err(std::io::Error::other)
            .and_then(|text| write_private_file(&self.file, &text));
        if let Err(e) = written {
            eprintln!("[tool-profile] could not write tool-profile.json: {e}");
        }
    }

    fn ensure_loaded(&self, inner: &mut Inner) {
        if inner.loaded {
            return;
        }
        inner.loaded = true;
        let parsed = std::fs::read_to_string(&self.file)
            .ok()
            .and_then(|text| read_tool_profile(&text));
        if let Some(profile) = parsed {
            inner.current = profile;
            return;
        }
        inner.current = ToolProfile::Browse;
        self.write_profile_file(ToolProfile::Browse);
    }

    pub fn get(&self) -> ToolProfile {
        let mut inner = self.lock();
        self.ensure_loaded(&mut inner);
        inner.current
    }

    pub fn set(
        &self,
        profile: ToolProfile,
        source: ProfileSource,
    ) -> Result<ProfileChange, String> {
        if source == ProfileSource::Agent && profile == ToolProfile::Probe {
            return Err(TOOL_PROFILE_PROBE_REJECTED.to_owned());
        }
        let (servers, listeners) = {
            let mut inner = self.lock();
            self.ensure_loaded(&mut inner);
            inner.current = profile;
            self.write_profile_file(profile);
            (inner.follow_servers.clone(), inner.listeners.clone())
        };
        // Outside the lock: a session or a listener may read the profile back.
        for server in servers {
            server.apply_follow_catalog(profile);
        }
        for listener in listeners {
            listener(profile);
        }
        Ok(ProfileChange {
            profile,
            advertised: advertised_names(profile, []).len(),
        })
    }

    /// Runs on every change, whoever made it: the app's stand-in for the
    /// `tool-profile:changed` message the TypeScript sent to each window.
    pub fn on_change(&self, listener: impl Fn(ToolProfile) + Send + Sync + 'static) {
        self.lock().listeners.push(Arc::new(listener));
    }

    pub fn register_follow_server(&self, server: Arc<dyn FollowServer>) {
        let mut inner = self.lock();
        let id = identity(&server);
        if !inner.follow_servers.iter().any(|s| identity(s) == id) {
            inner.follow_servers.push(server);
        }
    }

    pub fn unregister_follow_server(&self, server: &Arc<dyn FollowServer>) {
        let id = identity(server);
        self.lock().follow_servers.retain(|s| identity(s) != id);
    }

    /// Follower count for tests. Kept out of the docs: it exists so the MCP
    /// adapter's tests can assert on this crate's state.
    #[doc(hidden)]
    pub fn follow_server_count(&self) -> usize {
        self.lock().follow_servers.len()
    }
}

/// Test scaffolding shared with the adapter crates that used to hold these
/// modules. `TempDir` is the only export.
#[doc(hidden)]
pub mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A fresh, empty data folder per test; removed when the test ends.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(label: &str) -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "rever-mcp-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct Recorder(Mutex<Vec<ToolProfile>>);

    impl FollowServer for Recorder {
        fn apply_follow_catalog(&self, profile: ToolProfile) {
            self.0.lock().unwrap().push(profile);
        }
    }

    fn file_text(dir: &TempDir) -> String {
        std::fs::read_to_string(dir.0.join("tool-profile.json")).unwrap()
    }

    #[test]
    fn 파일이_없으면_browse로_시작하고_파일을_만든다() {
        let dir = TempDir::new("profile");
        let state = ToolProfileState::new(&dir.0);
        assert_eq!(state.get(), ToolProfile::Browse);
        // The exact bytes JSON.stringify({ version: 1, profile }, null, 2) writes.
        assert_eq!(
            file_text(&dir),
            "{\n  \"version\": 1,\n  \"profile\": \"browse\"\n}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn 프로파일_파일은_소유자만_읽고_쓸_수_있다() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("profile");
        let file = dir.0.join("tool-profile.json");
        // An existing, wider file is tightened too, not only a new one.
        std::fs::write(&file, "{}").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        ToolProfileState::new(&dir.0).get();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn 저장된_프로파일을_읽어_온다() {
        let dir = TempDir::new("profile");
        std::fs::write(
            dir.0.join("tool-profile.json"),
            r#"{"version":1,"profile":"reverse"}"#,
        )
        .unwrap();
        let state = ToolProfileState::new(&dir.0);
        assert_eq!(state.get(), ToolProfile::Reverse);
        // A valid file is left as it was.
        assert_eq!(file_text(&dir), r#"{"version":1,"profile":"reverse"}"#);
    }

    #[test]
    fn 깨진_파일은_browse로_되돌리고_다시_쓴다() {
        for broken in [
            "not json",
            r#"{"version":2,"profile":"reverse"}"#,
            r#"{"version":1,"profile":"full"}"#,
            "",
        ] {
            let dir = TempDir::new("profile");
            std::fs::write(dir.0.join("tool-profile.json"), broken).unwrap();
            let state = ToolProfileState::new(&dir.0);
            assert_eq!(state.get(), ToolProfile::Browse, "{broken}");
            assert_eq!(
                file_text(&dir),
                "{\n  \"version\": 1,\n  \"profile\": \"browse\"\n}"
            );
        }
    }

    #[test]
    fn 파일은_처음_한_번만_읽는다() {
        let dir = TempDir::new("profile");
        let state = ToolProfileState::new(&dir.0);
        assert_eq!(state.get(), ToolProfile::Browse);
        std::fs::write(
            dir.0.join("tool-profile.json"),
            r#"{"version":1,"profile":"reverse"}"#,
        )
        .unwrap();
        assert_eq!(state.get(), ToolProfile::Browse);
    }

    #[test]
    fn 프로파일을_바꾸면_파일에_쓰고_광고하는_도구_수를_돌려준다() {
        let dir = TempDir::new("profile");
        let state = ToolProfileState::new(&dir.0);
        let change = state.set(ToolProfile::Reverse, ProfileSource::Agent);
        assert_eq!(
            change,
            Ok(ProfileChange {
                profile: ToolProfile::Reverse,
                advertised: advertised_names(ToolProfile::Reverse, []).len(),
            })
        );
        assert_eq!(state.get(), ToolProfile::Reverse);
        assert_eq!(
            file_text(&dir),
            "{\n  \"version\": 1,\n  \"profile\": \"reverse\"\n}"
        );
        // A new state over the same folder starts from what was saved.
        assert_eq!(ToolProfileState::new(&dir.0).get(), ToolProfile::Reverse);
    }

    #[test]
    fn 에이전트는_probe를_고를_수_없고_사용자는_고를_수_있다() {
        let dir = TempDir::new("profile");
        let state = ToolProfileState::new(&dir.0);
        assert_eq!(
            state.set(ToolProfile::Probe, ProfileSource::Agent),
            Err(TOOL_PROFILE_PROBE_REJECTED.to_owned())
        );
        // The refusal happens before the file is even loaded.
        assert!(!dir.0.join("tool-profile.json").exists());
        assert_eq!(state.get(), ToolProfile::Browse);

        let change = state.set(ToolProfile::Probe, ProfileSource::User);
        assert_eq!(change.map(|c| c.profile), Ok(ToolProfile::Probe));
        assert_eq!(state.get(), ToolProfile::Probe);
    }

    #[test]
    fn 폴더에_쓸_수_없어도_프로파일은_메모리에서_바뀐다() {
        let dir = TempDir::new("profile");
        let state = ToolProfileState::new(&dir.0.join("missing"));
        assert_eq!(state.get(), ToolProfile::Browse);
        let change = state.set(ToolProfile::Reverse, ProfileSource::User);
        assert_eq!(change.map(|c| c.profile), Ok(ToolProfile::Reverse));
        assert_eq!(state.get(), ToolProfile::Reverse);
    }

    #[test]
    fn 바꿀_때마다_follow_세션과_리스너에_알린다() {
        let dir = TempDir::new("profile");
        let state = Arc::new(ToolProfileState::new(&dir.0));
        let recorder = Arc::new(Recorder::default());
        let server: Arc<dyn FollowServer> = recorder.clone();
        state.register_follow_server(server.clone());
        // Registering the same session twice keeps one entry, like Map.set.
        state.register_follow_server(server.clone());

        let heard = Arc::new(Mutex::new(Vec::new()));
        let sink = heard.clone();
        let reader = state.clone();
        // A listener may read the state back without deadlocking.
        state.on_change(move |profile| sink.lock().unwrap().push((profile, reader.get())));

        state
            .set(ToolProfile::Reverse, ProfileSource::Agent)
            .unwrap();
        // Setting the same profile again still applies and notifies.
        state
            .set(ToolProfile::Reverse, ProfileSource::User)
            .unwrap();
        assert_eq!(
            *recorder.0.lock().unwrap(),
            [ToolProfile::Reverse, ToolProfile::Reverse]
        );
        assert_eq!(
            *heard.lock().unwrap(),
            [(ToolProfile::Reverse, ToolProfile::Reverse); 2]
        );

        assert_eq!(state.follow_server_count(), 1);
        state.unregister_follow_server(&server);
        assert_eq!(state.follow_server_count(), 0);
        state.set(ToolProfile::Browse, ProfileSource::User).unwrap();
        assert_eq!(recorder.0.lock().unwrap().len(), 2);
        assert_eq!(heard.lock().unwrap().len(), 3);
    }

    #[test]
    fn 거절된_변경은_아무에게도_알리지_않는다() {
        let dir = TempDir::new("profile");
        let state = ToolProfileState::new(&dir.0);
        let recorder = Arc::new(Recorder::default());
        state.register_follow_server(recorder.clone());
        assert!(state.set(ToolProfile::Probe, ProfileSource::Agent).is_err());
        assert!(recorder.0.lock().unwrap().is_empty());
    }

    #[test]
    fn 같은_컨텍스트는_상태_하나를_나눠_쓴다() {
        let dir = TempDir::new("profile");
        let context = ToolContext::new(
            Arc::default(),
            Arc::new(rever_tools_api::tool::NoBrowser),
            dir.0.clone(),
        );
        let first = ToolProfileState::of(&context);
        first
            .set(ToolProfile::Reverse, ProfileSource::User)
            .unwrap();
        assert!(Arc::ptr_eq(&first, &ToolProfileState::of(&context)));
        assert_eq!(ToolProfileState::of(&context).get(), ToolProfile::Reverse);
        assert!(dir.0.join("tool-profile.json").exists());
    }

    #[test]
    fn 변경_결과는_camel_case로_직렬화한다() {
        let change = ProfileChange {
            profile: ToolProfile::Reverse,
            advertised: 3,
        };
        assert_eq!(
            serde_json::to_value(change).unwrap(),
            json!({ "profile": "reverse", "advertised": 3 })
        );
    }
}
