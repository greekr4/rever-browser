//! Where the stores keep their state between runs.
//!
//! The Electron app persists both stores with zustand's `persist` middleware on
//! `localStorage`. This module has the same contract in two parts: a string key-value
//! [`Storage`] the host provides, and the envelope zustand wraps a store's state in
//! (`{"state": …, "version": 0}`), so data written by one app is readable by the other.

use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// A string key-value store, the part of `localStorage` the stores use.
pub trait Storage {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&mut self, key: &str, value: &str);
}

/// Keeps everything in memory: for tests, previews, and until the shared storage exists.
#[derive(Default, Debug, Clone)]
pub struct MemoryStorage {
    values: HashMap<String, String>,
}

impl Storage for MemoryStorage {
    fn get(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }

    fn set(&mut self, key: &str, value: &str) {
        self.values.insert(key.to_string(), value.to_string());
    }
}

/// zustand's default `version` for a persisted store; neither store sets its own.
const VERSION: u64 = 0;

/// The persisted `state` object under `key`, as zustand's hydration would accept it:
/// unreadable JSON is ignored, and so is a value whose numeric `version` is not ours
/// (there is no `migrate`). A value without a numeric version is taken as it is.
pub(crate) fn load_state(storage: &dyn Storage, key: &str) -> Option<Value> {
    let mut stored: Value = serde_json::from_str(&storage.get(key)?).ok()?;
    if let Some(version) = stored.get("version").filter(|version| version.is_number())
        && version.as_f64() != Some(VERSION as f64)
    {
        return None;
    }
    stored.get_mut("state").map(Value::take)
}

/// The list at `state[field]`. Hydration must not lose the whole list to one damaged
/// item, so an item that does not decode is skipped; anything but a list is empty.
pub(crate) fn decode_list<T: DeserializeOwned>(state: Option<Value>, field: &str) -> Vec<T> {
    let Some(Value::Array(items)) =
        state.and_then(|mut state| state.get_mut(field).map(Value::take))
    else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| serde_json::from_value(item).ok())
        .collect()
}

pub(crate) fn save_state(storage: &mut dyn Storage, key: &str, state: Value) {
    storage.set(
        key,
        &json!({ "state": state, "version": VERSION }).to_string(),
    );
}

/// A storage a test can still read after a store has taken ownership of it.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct SharedStorage(pub std::rc::Rc<std::cell::RefCell<MemoryStorage>>);

#[cfg(test)]
impl Storage for SharedStorage {
    fn get(&self, key: &str) -> Option<String> {
        self.0.borrow().get(key)
    }

    fn set(&mut self, key: &str, value: &str) {
        self.0.borrow_mut().set(key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage_with(key: &str, value: &str) -> MemoryStorage {
        let mut storage = MemoryStorage::default();
        storage.set(key, value);
        storage
    }

    #[test]
    fn 메모리_저장소는_넣은_값을_키로_돌려준다() {
        let mut storage = MemoryStorage::default();
        assert_eq!(storage.get("k"), None);
        storage.set("k", "v1");
        storage.set("k", "v2");
        assert_eq!(storage.get("k"), Some("v2".to_string()));
    }

    #[test]
    fn 저장할_때_state와_version_0으로_감싼다() {
        let mut storage = MemoryStorage::default();
        save_state(&mut storage, "k", json!({ "entries": [] }));
        assert_eq!(
            storage.get("k").as_deref(),
            Some(r#"{"state":{"entries":[]},"version":0}"#)
        );
    }

    #[test]
    fn 저장된_state를_그대로_읽는다() {
        let storage = storage_with("k", r#"{"state":{"a":1},"version":0}"#);
        assert_eq!(load_state(&storage, "k"), Some(json!({ "a": 1 })));
    }

    #[test]
    fn 키가_없거나_json이_깨졌으면_읽지_않는다() {
        assert_eq!(load_state(&MemoryStorage::default(), "k"), None);
        assert_eq!(load_state(&storage_with("k", "{not json"), "k"), None);
        assert_eq!(load_state(&storage_with("k", "null"), "k"), None);
        assert_eq!(load_state(&storage_with("k", ""), "k"), None);
    }

    #[test]
    fn 목록에서_깨진_항목만_건너뛴다() {
        let state = json!({ "items": [1, "x", 2, null, 3] });
        assert_eq!(decode_list::<u32>(Some(state), "items"), vec![1, 2, 3]);
    }

    #[test]
    fn 목록이_아니거나_없으면_빈_목록이다() {
        assert!(decode_list::<u32>(None, "items").is_empty());
        assert!(decode_list::<u32>(Some(json!("text")), "items").is_empty());
        assert!(decode_list::<u32>(Some(json!({ "items": null })), "items").is_empty());
        assert!(decode_list::<u32>(Some(json!({ "other": [1] })), "items").is_empty());
    }

    #[test]
    fn 버전이_다른_숫자면_읽지_않는다() {
        let storage = storage_with("k", r#"{"state":{"a":1},"version":1}"#);
        assert_eq!(load_state(&storage, "k"), None);
    }

    #[test]
    fn 버전이_없거나_숫자가_아니면_읽는다() {
        let storage = storage_with("k", r#"{"state":{"a":1}}"#);
        assert_eq!(load_state(&storage, "k"), Some(json!({ "a": 1 })));
        let storage = storage_with("k", r#"{"state":{"a":1},"version":"1"}"#);
        assert_eq!(load_state(&storage, "k"), Some(json!({ "a": 1 })));
    }
}
