//! Port of `src/main/tab-partition.ts` — which browsing session a tab uses, and
//! the named browser profiles.
//!
//! Ordinary tabs share one persistent session, so logging in on one tab keeps
//! you logged in on tabs opened from it. Proxy tabs are the exception: each gets
//! its own in-memory session (no `persist:` prefix, cookies vanish with the
//! app), so its cookie jar and proxy are its own.
//!
//! The TypeScript keeps module-level state; here it is one `TabPartitions`, and
//! the clock and the random part of a new profile id are passed in.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const SHARED_PARTITION: &str = "persist:rever-shared";
pub const DEFAULT_PROFILE_ID: &str = "default";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileKind {
    Persistent,
    Incognito,
}

/// A named browsing identity backed by a session:
///  - "Default" is the shared persistent session (implicit, id `default`).
///  - Persistent profiles survive restarts and are saved to disk.
///  - Incognito profiles are in-memory: cookies vanish with the app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserProfile {
    pub id: String,
    pub name: String,
    pub kind: ProfileKind,
    /// The session this profile browses in; derived from id + kind.
    pub partition: String,
    /// Source browser this profile was seeded from (e.g. "Chrome"), for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

struct Stored {
    name: String,
    kind: ProfileKind,
    source: Option<String>,
}

pub struct TabPartitions {
    // Entries are only added for proxy tabs; every other tab resolves to the
    // shared partition. Not cleaned up on tab close — entries are a few bytes
    // and tab ids are never reused within a run.
    tab_partitions: HashMap<String, String>,
    active: String,
    // In creation order. The Default profile is implicit and not stored.
    profiles: IndexMap<String, Stored>,
    file: PathBuf,
    loaded: bool,
}

fn partition_for_profile(id: &str, kind: ProfileKind) -> String {
    if id == DEFAULT_PROFILE_ID {
        return SHARED_PARTITION.to_owned();
    }
    match kind {
        ProfileKind::Persistent => format!("persist:rever-profile-{id}"),
        ProfileKind::Incognito => format!("rever-incognito-{id}"),
    }
}

fn to_profile(id: &str, stored: &Stored) -> BrowserProfile {
    BrowserProfile {
        id: id.to_owned(),
        name: stored.name.clone(),
        kind: stored.kind,
        partition: partition_for_profile(id, stored.kind),
        source: stored.source.clone(),
    }
}

/// `Number.prototype.toString(36)` for a non-negative integer.
fn base36(mut n: u64) -> String {
    let mut digits = Vec::new();
    loop {
        digits.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    digits.iter().rev().map(|&d| char::from(d)).collect()
}

impl TabPartitions {
    /// `user_data_dir` is `paths::user_data_dir`; profiles are kept in
    /// `browser-profiles.json` under it and read on first use.
    pub fn new(user_data_dir: &Path) -> Self {
        Self {
            tab_partitions: HashMap::new(),
            active: SHARED_PARTITION.to_owned(),
            profiles: IndexMap::new(),
            file: user_data_dir.join("browser-profiles.json"),
            loaded: false,
        }
    }

    pub fn register_tab_partition(&mut self, tab_id: &str, partition: &str) {
        self.tab_partitions
            .insert(tab_id.to_owned(), partition.to_owned());
    }

    pub fn partition_for_tab(&self, tab_id: &str) -> &str {
        self.tab_partitions
            .get(tab_id)
            .map_or(SHARED_PARTITION, String::as_str)
    }

    // Features that act on the browsing session (cookie persistence, cookie
    // import) resolve it through these so they target the visible tab's session.

    pub fn set_active_partition(&mut self, partition: &str) {
        self.active = partition.to_owned();
    }

    pub fn active_partition(&self) -> &str {
        &self.active
    }

    fn ensure_loaded(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        // A missing or unreadable file is an empty list, as in the TypeScript.
        let Ok(text) = fs::read_to_string(&self.file) else {
            return;
        };
        let Ok(Value::Array(raw)) = serde_json::from_str(&text) else {
            return;
        };
        for entry in &raw {
            // An entry without a string id has no usable key; it is skipped.
            let Some(id) = entry.get("id").and_then(Value::as_str) else {
                continue;
            };
            let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_owned);
            self.profiles.insert(
                id.to_owned(),
                Stored {
                    name: text("name").unwrap_or_default(),
                    kind: ProfileKind::Persistent,
                    source: text("source"),
                },
            );
        }
    }

    fn persist(&self) {
        // Only persistent profiles are saved; incognito ones are ephemeral by design.
        let out: Vec<Value> = self
            .profiles
            .iter()
            .filter(|(_, p)| p.kind == ProfileKind::Persistent)
            .map(|(id, p)| match &p.source {
                Some(source) => json!({ "id": id, "name": p.name, "source": source }),
                None => json!({ "id": id, "name": p.name }),
            })
            .collect();
        // A failed save keeps the profile for this run; the TypeScript only logs it.
        if let Ok(text) = serde_json::to_string_pretty(&out) {
            let _ = fs::write(&self.file, text);
        }
    }

    pub fn list_profiles(&mut self) -> Vec<BrowserProfile> {
        self.ensure_loaded();
        let default = Stored {
            name: "Default".to_owned(),
            kind: ProfileKind::Persistent,
            source: None,
        };
        let mut list = vec![to_profile(DEFAULT_PROFILE_ID, &default)];
        list.extend(self.profiles.iter().map(|(id, p)| to_profile(id, p)));
        list
    }

    /// `random` stands in for `Math.random()`: any value, it only keeps two
    /// profiles made in the same millisecond apart.
    pub fn create_profile(
        &mut self,
        name: &str,
        kind: ProfileKind,
        source: Option<&str>,
        now_ms: u64,
        random: u32,
    ) -> BrowserProfile {
        self.ensure_loaded();
        // Four base-36 digits, like `Math.random().toString(36).slice(2, 6)`.
        let suffix = format!("{:0>4}", base36(u64::from(random) % 36u64.pow(4)));
        let id = format!("p{}{suffix}", base36(now_ms));
        let name = match (name.trim(), kind) {
            ("", ProfileKind::Incognito) => "Incognito",
            ("", ProfileKind::Persistent) => "Profile",
            (trimmed, _) => trimmed,
        };
        let stored = Stored {
            name: name.to_owned(),
            kind,
            source: source.map(str::to_owned),
        };
        let profile = to_profile(&id, &stored);
        self.profiles.insert(id, stored);
        if kind == ProfileKind::Persistent {
            self.persist();
        }
        profile
    }

    pub fn delete_profile(&mut self, id: &str) {
        if id == DEFAULT_PROFILE_ID {
            return;
        }
        self.ensure_loaded();
        let removed = self.profiles.shift_remove(id);
        if removed.is_some_and(|p| p.kind == ProfileKind::Persistent) {
            self.persist();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_791_421_323_000;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rever-tab-partition-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn 등록하지_않은_탭은_공유_세션을_쓴다() {
        let mut parts = TabPartitions::new(Path::new("/nowhere"));
        parts.register_tab_partition("t2", "rever-proxy-1");
        assert_eq!(parts.partition_for_tab("t1"), SHARED_PARTITION);
        assert_eq!(parts.partition_for_tab("t2"), "rever-proxy-1");
        assert_eq!(parts.active_partition(), SHARED_PARTITION);
        parts.set_active_partition("rever-proxy-1");
        assert_eq!(parts.active_partition(), "rever-proxy-1");
    }

    #[test]
    fn 프로필_id는_시각과_난수를_36진수로_붙인다() {
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
        let mut parts = TabPartitions::new(&temp("id"));
        let profile = parts.create_profile("Work", ProfileKind::Incognito, None, 36, 37);
        assert_eq!(profile.id, "p100011");
        assert_eq!(profile.partition, "rever-incognito-p100011");
    }

    #[test]
    fn 영구_프로필은_파일에_남고_다시_읽으면_만든_순서로_나온다() {
        let dir = temp("persist");
        let mut parts = TabPartitions::new(&dir);
        let work = parts.create_profile("  Work ", ProfileKind::Persistent, Some("Chrome"), NOW, 1);
        let blank = parts.create_profile(" ", ProfileKind::Persistent, None, NOW, 2);
        let private = parts.create_profile("", ProfileKind::Incognito, None, NOW, 3);
        assert_eq!(
            (work.name.as_str(), blank.name.as_str()),
            ("Work", "Profile")
        );
        assert_eq!(private.name, "Incognito");
        assert_eq!(work.partition, format!("persist:rever-profile-{}", work.id));

        assert_eq!(
            fs::read_to_string(dir.join("browser-profiles.json")).unwrap(),
            format!(
                "[\n  {{\n    \"id\": \"{}\",\n    \"name\": \"Work\",\n    \"source\": \"Chrome\"\n  }},\n  {{\n    \"id\": \"{}\",\n    \"name\": \"Profile\"\n  }}\n]",
                work.id, blank.id
            )
        );

        // A new run: the incognito profile is gone, the default comes first.
        let listed = TabPartitions::new(&dir).list_profiles();
        assert_eq!(listed.len(), 3);
        assert_eq!(
            (
                listed[0].id.as_str(),
                listed[0].name.as_str(),
                listed[0].partition.as_str()
            ),
            (DEFAULT_PROFILE_ID, "Default", SHARED_PARTITION)
        );
        assert_eq!(listed[1..], [work, blank]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn 프로필을_지우면_파일에서도_빠지고_기본_프로필은_지울_수_없다() {
        let dir = temp("delete");
        let mut parts = TabPartitions::new(&dir);
        let work = parts.create_profile("Work", ProfileKind::Persistent, None, NOW, 1);
        parts.delete_profile(DEFAULT_PROFILE_ID);
        parts.delete_profile("missing");
        assert_eq!(parts.list_profiles().len(), 2);
        parts.delete_profile(&work.id);
        assert_eq!(parts.list_profiles().len(), 1);
        assert_eq!(
            fs::read_to_string(dir.join("browser-profiles.json")).unwrap(),
            "[]"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn 깨진_파일은_빈_목록으로_읽는다() {
        let dir = temp("broken");
        for text in ["not json", "{\"id\":\"x\"}", "[{\"name\":\"no id\"}, 3]"] {
            fs::write(dir.join("browser-profiles.json"), text).unwrap();
            assert_eq!(TabPartitions::new(&dir).list_profiles().len(), 1, "{text}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn 프로필은_camel_case_json으로_오간다() {
        let mut parts = TabPartitions::new(&temp("json"));
        let profile = parts.create_profile("Work", ProfileKind::Incognito, None, 0, 0);
        assert_eq!(
            serde_json::to_string(&profile).unwrap(),
            r#"{"id":"p00000","name":"Work","kind":"incognito","partition":"rever-incognito-p00000"}"#
        );
    }
}
