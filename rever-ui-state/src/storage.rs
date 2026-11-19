//! The key-value storage the stores persist to, standing in for the browser's
//! `localStorage`, plus the envelope zustand's `persist` middleware wraps a
//! store's state in.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// String values by key, like `localStorage`. Methods take `&self` so a store
/// can be handed a shared `&dyn Storage` for both loading and saving.
pub trait Storage {
    fn get(&self, key: &str) -> Option<String>;
    /// Fails when the value could not be stored (`setItem` throwing); the
    /// previous value is then still in place.
    fn set(&self, key: &str, value: &str) -> io::Result<()>;
    fn remove(&self, key: &str) -> io::Result<()>;
}

// A panic while the lock was held must not make every later read panic too.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Storage that lives only in memory, for tests.
#[derive(Debug, Default)]
pub struct MemoryStorage {
    entries: Mutex<BTreeMap<String, String>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Storage for MemoryStorage {
    fn get(&self, key: &str) -> Option<String> {
        lock(&self.entries).get(key).cloned()
    }

    fn set(&self, key: &str, value: &str) -> io::Result<()> {
        lock(&self.entries).insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn remove(&self, key: &str) -> io::Result<()> {
        lock(&self.entries).remove(key);
        Ok(())
    }
}

/// Storage kept in one JSON file: an object mapping each key to its string
/// value. Every change rewrites the file through a temporary file and a
/// rename, so a crash mid-write leaves the previous file intact.
// shortcut: one process owns the file (no locking, no re-read), upgrade if a
// second process ever writes it.
#[derive(Debug)]
pub struct FileStorage {
    path: PathBuf,
    entries: Mutex<BTreeMap<String, String>>,
}

impl FileStorage {
    /// Opens the file, starting empty when it does not exist yet. A file that
    /// exists but is not a JSON object of strings is an error rather than an
    /// empty store, because the first write would otherwise replace it.
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let entries = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            path,
            entries: Mutex::new(entries),
        })
    }

    fn write(&self, entries: &BTreeMap<String, String>) -> io::Result<()> {
        let name = self.path.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "storage path has no file name")
        })?;
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        // Same directory as the target, so the rename stays on one filesystem.
        let mut tmp_name = name.to_owned();
        tmp_name.push(".tmp");
        let tmp = self.path.with_file_name(tmp_name);
        let mut file = fs::File::create(&tmp)?;
        file.write_all(serde_json::to_string(entries)?.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, &self.path)
    }
}

impl Storage for FileStorage {
    fn get(&self, key: &str) -> Option<String> {
        lock(&self.entries).get(key).cloned()
    }

    fn set(&self, key: &str, value: &str) -> io::Result<()> {
        let mut entries = lock(&self.entries);
        let previous = entries.insert(key.to_owned(), value.to_owned());
        let written = self.write(&entries);
        if written.is_err() {
            // Keep memory and file in agreement: the value was not stored.
            match previous {
                Some(old) => entries.insert(key.to_owned(), old),
                None => entries.remove(key),
            };
        }
        written
    }

    fn remove(&self, key: &str) -> io::Result<()> {
        let mut entries = lock(&self.entries);
        let Some(previous) = entries.remove(key) else {
            return Ok(());
        };
        let written = self.write(&entries);
        if written.is_err() {
            entries.insert(key.to_owned(), previous);
        }
        written
    }
}

#[derive(Serialize)]
struct Envelope<'a, T> {
    state: &'a T,
    version: u32,
}

/// Writes a store's state the way zustand's `persist` does:
/// `{"state": …, "version": 0}` under the store's name.
pub(crate) fn save_persisted<T: Serialize>(
    storage: &dyn Storage,
    name: &str,
    state: &T,
) -> io::Result<()> {
    let text = serde_json::to_string(&Envelope { state, version: 0 })?;
    storage.set(name, &text)
}

/// Reads back what `persist` would merge over a store's defaults: the fields
/// of the stored `state` object. Anything unreadable gives no fields, and so
/// does a numeric version other than 0 — none of the stores define a
/// `migrate`, and without one zustand discards the stored state.
pub(crate) fn load_persisted(storage: &dyn Storage, name: &str) -> Map<String, Value> {
    let Some(raw) = storage.get(name) else {
        return Map::new();
    };
    let Ok(Value::Object(mut envelope)) = serde_json::from_str::<Value>(&raw) else {
        return Map::new();
    };
    if let Some(Value::Number(version)) = envelope.get("version")
        && version.as_f64() != Some(0.0)
    {
        return Map::new();
    }
    match envelope.remove("state") {
        Some(Value::Object(state)) => state,
        _ => Map::new(),
    }
}

/// One persisted field, or `None` (keep the default) when it is missing or
/// does not have the expected type.
pub(crate) fn field<T: DeserializeOwned>(state: &Map<String, Value>, key: &str) -> Option<T> {
    state.get(key).and_then(|value| T::deserialize(value).ok())
}

/// A persisted array field. Entries that do not have the expected shape are
/// dropped instead of discarding the whole list with them.
pub(crate) fn list_field<T: DeserializeOwned>(
    state: &Map<String, Value>,
    key: &str,
) -> Option<Vec<T>> {
    let items = state.get(key)?.as_array()?;
    Some(
        items
            .iter()
            .filter_map(|item| T::deserialize(item).ok())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // A fresh directory per test under the system temp dir; no tempfile crate.
    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rever-ui-state-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn 메모리_저장소는_키별로_값을_넣고_읽고_지운다() {
        let storage = MemoryStorage::new();
        assert_eq!(storage.get("a"), None);
        storage.set("a", "1").unwrap();
        storage.set("a", "2").unwrap();
        assert_eq!(storage.get("a").as_deref(), Some("2"));
        storage.remove("a").unwrap();
        assert_eq!(storage.get("a"), None);
    }

    #[test]
    fn 파일_저장소는_모든_키를_한_json_파일에_담고_다시_열면_복원한다() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("nested").join("storage.json");
        let storage = FileStorage::open(&path).unwrap();
        storage.set("rev:notify", "off").unwrap();
        storage.set("rev:tabs", "{\"tabs\":[]}").unwrap();

        let on_disk: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            on_disk,
            serde_json::json!({ "rev:notify": "off", "rev:tabs": "{\"tabs\":[]}" })
        );
        // The temporary file is renamed over the target, not left behind.
        assert!(!path.with_file_name("storage.json.tmp").exists());

        let reopened = FileStorage::open(&path).unwrap();
        assert_eq!(reopened.get("rev:notify").as_deref(), Some("off"));
        reopened.remove("rev:notify").unwrap();
        assert_eq!(FileStorage::open(&path).unwrap().get("rev:notify"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn 파일_저장소는_깨진_파일을_빈_저장소로_덮어쓰지_않고_오류를_낸다() {
        let dir = temp_dir("corrupt");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("storage.json");
        for broken in ["not json", "[1,2]", "{\"a\":1}"] {
            fs::write(&path, broken).unwrap();
            assert!(FileStorage::open(&path).is_err(), "{broken}");
            assert_eq!(fs::read_to_string(&path).unwrap(), broken);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn 파일_저장소는_쓰기에_실패하면_이전_값을_유지한다() {
        let dir = temp_dir("write-fail");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("storage.json");
        let storage = FileStorage::open(&path).unwrap();
        storage.set("a", "1").unwrap();
        // A directory where the temporary file should go makes the write fail.
        fs::create_dir_all(dir.join("storage.json.tmp")).unwrap();
        assert!(storage.set("a", "2").is_err());
        assert!(storage.set("b", "3").is_err());
        assert!(storage.remove("a").is_err());
        assert_eq!(storage.get("a").as_deref(), Some("1"));
        assert_eq!(storage.get("b"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_봉투는_state와_version_0으로_저장된다() {
        let storage = MemoryStorage::new();
        save_persisted(&storage, "k", &serde_json::json!({ "collapsed": true })).unwrap();
        assert_eq!(
            storage.get("k").as_deref(),
            Some(r#"{"state":{"collapsed":true},"version":0}"#)
        );
        assert_eq!(
            field::<bool>(&load_persisted(&storage, "k"), "collapsed"),
            Some(true)
        );
    }

    #[test]
    fn persist_읽기는_깨진_값과_다른_버전을_버린다() {
        let storage = MemoryStorage::new();
        for raw in [
            "not json",
            "null",
            "5",
            r#"{"state":"text","version":0}"#,
            // No migrate function: a different version is discarded.
            r#"{"state":{"collapsed":true},"version":1}"#,
        ] {
            storage.set("k", raw).unwrap();
            assert!(load_persisted(&storage, "k").is_empty(), "{raw}");
        }
        // Only a *numeric* version is compared; a missing one still loads.
        // So does a version that is not a number, and -0 equals 0.
        for raw in [
            r#"{"state":{"collapsed":true}}"#,
            r#"{"state":{"collapsed":true},"version":"1"}"#,
            r#"{"state":{"collapsed":true},"version":null}"#,
            r#"{"state":{"collapsed":true},"version":-0.0}"#,
        ] {
            storage.set("k", raw).unwrap();
            assert_eq!(load_persisted(&storage, "k").len(), 1, "{raw}");
        }
    }

    #[test]
    fn 타입이_맞지_않는_필드는_none이고_목록은_맞는_항목만_남긴다() {
        let storage = MemoryStorage::new();
        storage
            .set(
                "k",
                r#"{"state":{"collapsed":"yes","items":[1,"x",2]},"version":0}"#,
            )
            .unwrap();
        let state = load_persisted(&storage, "k");
        assert_eq!(field::<bool>(&state, "collapsed"), None);
        assert_eq!(field::<bool>(&state, "missing"), None);
        assert_eq!(list_field::<u32>(&state, "items"), Some(vec![1, 2]));
        assert_eq!(list_field::<u32>(&state, "collapsed"), None);
    }
}
