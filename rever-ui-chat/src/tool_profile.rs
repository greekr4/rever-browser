//! Port of `src/renderer/src/stores/tool-profile.ts` — the interface's copy of the tool
//! profile the main process owns.
//!
//! `window.rev.toolProfile` is the [`ToolProfileHost`]. Its `set` is asynchronous in the
//! Electron app, and a change from the main process can land while it is in flight, so
//! besides [`ToolProfileStore::set_profile`] the two halves are there on their own:
//! [`ToolProfileStore::begin_set`] before the call and [`ToolProfileStore::settle`] with
//! its result.

use rever_tool_profile::tool_profile_line::ToolProfileId;
use serde_json::Value;

/// `window.rev.toolProfile`, as far as this store uses it.
pub trait ToolProfileHost {
    /// The profile the main process has now, as it came over IPC.
    fn get(&self) -> Value;
    fn set(&self, profile: ToolProfileId) -> Result<(), String>;
}

fn as_profile(value: &Value) -> Option<ToolProfileId> {
    value.as_str().and_then(ToolProfileId::parse)
}

/// A `set` that was sent and has not been answered yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingSet {
    profile: ToolProfileId,
    prev: ToolProfileId,
}

// Mirror of the main-process profile. Not persisted: main owns the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolProfileStore {
    profile: ToolProfileId,
}

impl Default for ToolProfileStore {
    fn default() -> Self {
        Self {
            profile: ToolProfileId::Browse,
        }
    }
}

impl ToolProfileStore {
    /// Starts on `browse`, then takes the main process's profile when it is a valid one.
    pub fn load(host: &dyn ToolProfileHost) -> Self {
        let mut store = Self::default();
        store.sync(&host.get());
        store
    }

    pub fn profile(&self) -> ToolProfileId {
        self.profile
    }

    /// What the main process reports (the answer to `get`, or an `onChange`). Anything
    /// that is not a profile id is ignored. Returns whether the value was taken.
    pub fn sync(&mut self, value: &Value) -> bool {
        let Some(profile) = as_profile(value) else {
            return false;
        };
        self.profile = profile;
        true
    }

    /// Show the new profile at once; the host's `set` follows.
    pub fn begin_set(&mut self, profile: ToolProfileId) -> PendingSet {
        let prev = std::mem::replace(&mut self.profile, profile);
        PendingSet { profile, prev }
    }

    /// The host's `set` answered. A failure puts the previous profile back, unless the
    /// profile changed again in the meantime.
    pub fn settle<E>(&mut self, pending: PendingSet, result: Result<(), E>) -> Result<(), E> {
        if result.is_err() && self.profile == pending.profile {
            self.profile = pending.prev;
        }
        result
    }

    /// The error is the caller's to log (`console.warn('[tool-profile] set failed:', e)`).
    pub fn set_profile(
        &mut self,
        host: &dyn ToolProfileHost,
        profile: ToolProfileId,
    ) -> Result<(), String> {
        let pending = self.begin_set(profile);
        self.settle(pending, host.set(profile))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use serde_json::json;

    use super::*;

    struct FakeHost {
        current: Value,
        fail: bool,
        sets: RefCell<Vec<ToolProfileId>>,
    }

    impl FakeHost {
        fn new(current: Value, fail: bool) -> Self {
            Self {
                current,
                fail,
                sets: RefCell::default(),
            }
        }
    }

    impl ToolProfileHost for FakeHost {
        fn get(&self) -> Value {
            self.current.clone()
        }

        fn set(&self, profile: ToolProfileId) -> Result<(), String> {
            self.sets.borrow_mut().push(profile);
            if self.fail {
                Err("ipc closed".to_owned())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn 처음에는_browse다() {
        assert_eq!(ToolProfileStore::default().profile(), ToolProfileId::Browse);
    }

    #[test]
    fn 메인_프로세스의_프로파일로_시작한다() {
        let host = FakeHost::new(json!("reverse"), false);
        assert_eq!(
            ToolProfileStore::load(&host).profile(),
            ToolProfileId::Reverse
        );
    }

    #[test]
    fn 프로파일이_아닌_값은_무시한다() {
        for junk in [
            json!("Browse"),
            json!("probe "),
            json!(""),
            json!(null),
            json!(1),
            json!({ "profile": "probe" }),
            json!(["probe"]),
        ] {
            let host = FakeHost::new(junk.clone(), false);
            assert_eq!(
                ToolProfileStore::load(&host).profile(),
                ToolProfileId::Browse,
                "{junk}"
            );
        }
        let mut store = ToolProfileStore::default();
        assert!(store.sync(&json!("probe")));
        assert!(!store.sync(&json!("admin")));
        assert_eq!(store.profile(), ToolProfileId::Probe);
    }

    #[test]
    fn 바꾸면_호스트에_알리고_새_프로파일을_보여준다() {
        let host = FakeHost::new(json!("browse"), false);
        let mut store = ToolProfileStore::load(&host);
        assert_eq!(store.set_profile(&host, ToolProfileId::Probe), Ok(()));
        assert_eq!(store.profile(), ToolProfileId::Probe);
        assert_eq!(*host.sets.borrow(), [ToolProfileId::Probe]);
    }

    #[test]
    fn 호스트가_실패하면_이전_프로파일로_되돌리고_오류를_돌려준다() {
        let host = FakeHost::new(json!("reverse"), true);
        let mut store = ToolProfileStore::load(&host);
        assert_eq!(
            store.set_profile(&host, ToolProfileId::Probe),
            Err("ipc closed".to_owned())
        );
        assert_eq!(store.profile(), ToolProfileId::Reverse);
    }

    #[test]
    fn 응답을_기다리는_동안에는_새_프로파일이_보인다() {
        let mut store = ToolProfileStore::default();
        let pending = store.begin_set(ToolProfileId::Reverse);
        assert_eq!(store.profile(), ToolProfileId::Reverse);
        assert_eq!(store.settle(pending, Ok::<(), String>(())), Ok(()));
        assert_eq!(store.profile(), ToolProfileId::Reverse);
    }

    #[test]
    fn 기다리는_사이에_프로파일이_또_바뀌었으면_실패해도_되돌리지_않는다() {
        let mut store = ToolProfileStore::default();
        let pending = store.begin_set(ToolProfileId::Reverse);
        // The main process announced another profile before the failure came back.
        store.sync(&json!("probe"));
        assert_eq!(store.settle(pending, Err("ipc closed")), Err("ipc closed"));
        assert_eq!(store.profile(), ToolProfileId::Probe);
    }

    #[test]
    fn 같은_프로파일로_바꾸다_실패하면_그대로다() {
        let host = FakeHost::new(json!("probe"), true);
        let mut store = ToolProfileStore::load(&host);
        assert!(store.set_profile(&host, ToolProfileId::Probe).is_err());
        assert_eq!(store.profile(), ToolProfileId::Probe);
    }
}
