//! Port of `src/renderer/src/workflows/core/store.ts` — the saved workflows, newest first.
//!
//! The zustand store becomes [`WorkflowsStore`]; its `persist` middleware becomes a write
//! to the [`Storage`] it was built with, under the same key and in the same JSON shape
//! (`{"state":{"workflows":[…]},"version":0}`).

use std::io;
use std::rc::Rc;

use rever_ui_state::Storage;
use serde_json::{Value, json};

use super::types::{Workflow, now_ms};

/// The `name` of the persisted store.
pub const STORAGE_KEY: &str = "rev:workflows";
pub const MAX_WORKFLOWS: usize = 200;

pub struct WorkflowsStore {
    workflows: Vec<Workflow>,
    storage: Rc<dyn Storage>,
}

impl WorkflowsStore {
    /// Starts from what `storage` holds, as zustand's hydration does: unreadable JSON
    /// and a numeric version other than 0 (there is no `migrate`) give an empty list.
    /// An entry that is not a workflow is skipped rather than losing the list with it.
    pub fn new(storage: Rc<dyn Storage>) -> Self {
        Self {
            workflows: load(storage.as_ref()),
            storage,
        }
    }

    /// Read the list again: something else (the agent's macro tools) wrote the storage.
    pub fn reload(&mut self) {
        self.workflows = load(self.storage.as_ref());
    }

    /// Newest first.
    pub fn workflows(&self) -> &[Workflow] {
        &self.workflows
    }

    /// Insert or replace by id, keeping the list newest-first. The list is changed
    /// even when the write fails (`setItem` throwing), as with `persist`.
    pub fn upsert(&mut self, workflow: Workflow) -> io::Result<()> {
        self.upsert_at(workflow, now_ms())
    }

    /// [`upsert`](Self::upsert) with the clock passed in.
    pub fn upsert_at(&mut self, mut workflow: Workflow, now_ms: u64) -> io::Result<()> {
        self.workflows.retain(|other| other.id != workflow.id);
        workflow.updated_at = now_ms;
        self.workflows.insert(0, workflow);
        self.workflows.truncate(MAX_WORKFLOWS);
        self.persist()
    }

    pub fn remove(&mut self, id: &str) -> io::Result<()> {
        self.workflows.retain(|other| other.id != id);
        self.persist()
    }

    // zustand's persist writes after every `set`, including one that changes nothing.
    fn persist(&self) -> io::Result<()> {
        let envelope = json!({ "state": { "workflows": self.workflows }, "version": 0 });
        self.storage.set(STORAGE_KEY, &envelope.to_string())
    }
}

fn load(storage: &dyn Storage) -> Vec<Workflow> {
    let Some(raw) = storage.get(STORAGE_KEY) else {
        return Vec::new();
    };
    let Ok(Value::Object(mut envelope)) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    if let Some(Value::Number(version)) = envelope.get("version")
        && version.as_f64() != Some(0.0)
    {
        return Vec::new();
    }
    let Some(Value::Array(items)) = envelope
        .get_mut("state")
        .and_then(|state| state.get_mut("workflows"))
        .map(Value::take)
    else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| serde_json::from_value(item).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rever_ui_state::MemoryStorage;

    fn workflow(id: &str, name: &str) -> Workflow {
        Workflow {
            id: id.into(),
            kind: "template".into(),
            name: name.into(),
            description: None,
            data: json!({ "body": "" }),
            created_at: 1,
            updated_at: 1,
        }
    }

    fn store() -> (Rc<MemoryStorage>, WorkflowsStore) {
        let storage = Rc::new(MemoryStorage::new());
        (storage.clone(), WorkflowsStore::new(storage))
    }

    fn ids(store: &WorkflowsStore) -> Vec<&str> {
        store.workflows().iter().map(|w| w.id.as_str()).collect()
    }

    #[test]
    fn 다시_읽으면_저장소에_다른_쪽이_쓴_목록으로_바뀐다() {
        let (storage, mut store) = store();
        store.upsert_at(workflow("a", "A"), 10).unwrap();
        // 에이전트의 매크로 도구가 같은 저장소에 쓴 것처럼.
        let (_, mut other) = (0, WorkflowsStore::new(storage.clone()));
        other.upsert_at(workflow("b", "B"), 20).unwrap();
        assert_eq!(ids(&store), ["a"]);
        store.reload();
        assert_eq!(ids(&store), ["b", "a"]);
    }

    #[test]
    fn 처음에는_비어_있다() {
        let (_, store) = store();
        assert!(store.workflows().is_empty());
    }

    #[test]
    fn 새_워크플로는_맨_앞에_들어가고_수정_시각이_지금으로_바뀐다() {
        let (_, mut store) = store();
        store.upsert_at(workflow("a", "A"), 10).unwrap();
        store.upsert_at(workflow("b", "B"), 20).unwrap();
        assert_eq!(ids(&store), ["b", "a"]);
        assert_eq!(store.workflows()[0].updated_at, 20);
        // 만든 시각은 그대로다.
        assert_eq!(store.workflows()[0].created_at, 1);
    }

    #[test]
    fn 같은_id로_다시_넣으면_바꿔_넣고_맨_앞으로_올린다() {
        let (_, mut store) = store();
        store.upsert_at(workflow("a", "A"), 10).unwrap();
        store.upsert_at(workflow("b", "B"), 20).unwrap();
        store.upsert_at(workflow("a", "A2"), 30).unwrap();
        assert_eq!(ids(&store), ["a", "b"]);
        assert_eq!(store.workflows()[0].name, "A2");
        assert_eq!(store.workflows()[0].updated_at, 30);
    }

    #[test]
    fn 이백_개를_넘으면_가장_오래된_것부터_버린다() {
        let (_, mut store) = store();
        for n in 0..=MAX_WORKFLOWS {
            store
                .upsert_at(workflow(&format!("w{n}"), "x"), n as u64)
                .unwrap();
        }
        assert_eq!(store.workflows().len(), MAX_WORKFLOWS);
        assert_eq!(store.workflows()[0].id, "w200");
        assert_eq!(store.workflows()[MAX_WORKFLOWS - 1].id, "w1");
    }

    #[test]
    fn id로_지우고_없는_id는_아무것도_지우지_않는다() {
        let (_, mut store) = store();
        store.upsert_at(workflow("a", "A"), 10).unwrap();
        store.upsert_at(workflow("b", "B"), 20).unwrap();
        store.remove("zzz").unwrap();
        assert_eq!(ids(&store), ["b", "a"]);
        store.remove("b").unwrap();
        assert_eq!(ids(&store), ["a"]);
    }

    #[test]
    fn rev_workflows_키에_persist_형식으로_저장한다() {
        let (storage, mut store) = store();
        store.upsert_at(workflow("a", "A"), 10).unwrap();
        assert_eq!(
            storage.get(STORAGE_KEY).as_deref(),
            Some(
                r#"{"state":{"workflows":[{"id":"a","kind":"template","name":"A","data":{"body":""},"createdAt":1,"updatedAt":10}]},"version":0}"#
            )
        );
        store.remove("a").unwrap();
        assert_eq!(
            storage.get(STORAGE_KEY).as_deref(),
            Some(r#"{"state":{"workflows":[]},"version":0}"#)
        );
    }

    #[test]
    fn 저장된_목록을_다시_읽는다() {
        let (storage, mut store) = store();
        store.upsert_at(workflow("a", "A"), 10).unwrap();
        store.upsert_at(workflow("b", "B"), 20).unwrap();
        let reopened = WorkflowsStore::new(storage);
        assert_eq!(ids(&reopened), ["b", "a"]);
        assert_eq!(reopened.workflows(), store.workflows());
    }

    #[test]
    fn 깨진_값과_다른_버전은_빈_목록이다() {
        for raw in [
            "not json",
            "null",
            "[]",
            r#"{"state":"text","version":0}"#,
            r#"{"state":{"workflows":"x"},"version":0}"#,
            r#"{"state":{"workflows":[{"id":"a"}]},"version":1}"#,
        ] {
            let storage = Rc::new(MemoryStorage::new());
            storage.set(STORAGE_KEY, raw).unwrap();
            assert!(WorkflowsStore::new(storage).workflows().is_empty(), "{raw}");
        }
    }

    #[test]
    fn 버전이_없거나_숫자가_아니면_읽고_깨진_항목만_건너뛴다() {
        for raw in [
            r#"{"state":{"workflows":[{"id":"a"},5,{"id":"b","name":7},{"id":"c"}]}}"#,
            r#"{"state":{"workflows":[{"id":"a"},null,{"id":"c"}]},"version":"1"}"#,
        ] {
            let storage = Rc::new(MemoryStorage::new());
            storage.set(STORAGE_KEY, raw).unwrap();
            assert_eq!(ids(&WorkflowsStore::new(storage)), ["a", "c"], "{raw}");
        }
    }

    /// A storage whose writes fail, as `localStorage.setItem` does over quota.
    struct FullStorage;

    impl Storage for FullStorage {
        fn get(&self, _: &str) -> Option<String> {
            None
        }
        fn set(&self, _: &str, _: &str) -> io::Result<()> {
            Err(io::Error::other("quota exceeded"))
        }
        fn remove(&self, _: &str) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn 저장에_실패해도_목록은_바뀌고_오류를_돌려준다() {
        let mut store = WorkflowsStore::new(Rc::new(FullStorage));
        assert!(store.upsert_at(workflow("a", "A"), 10).is_err());
        assert_eq!(ids(&store), ["a"]);
        assert!(store.remove("a").is_err());
        assert!(store.workflows().is_empty());
    }
}
