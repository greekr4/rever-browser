//! Port of `src/renderer/src/stores/chat-history.ts` — saved chat
//! conversations, newest first.

use std::io;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::constants::AcpAgentId;
use crate::storage::{Storage, list_field, load_persisted, save_persisted};
use crate::{js_trim, random_suffix, to_base36, utf16_cut};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatConversation {
    pub id: String,
    pub agent_id: AcpAgentId,
    /// The chat's `UIMessage` objects, kept as the JSON they arrive in so a
    /// round trip through storage loses no part type or field.
    pub messages: Vec<Value>,
    pub title: String,
    pub created_at: u64,
    pub updated_at: u64,
}

// Bundle-heavy tool outputs live in messages, so cap the count to keep the
// serialized store small (the browser version had a ~5MB storage quota).
const MAX_CONVERSATIONS: usize = 30;

const TITLE_MAX_UNITS: usize = 60;

const NAME: &str = "rev:chat-history";

/// `now_ms` stands in for `Date.now()` and `random` for `Math.random()` (a
/// value in `[0, 1)`).
pub fn new_conversation_id(now_ms: u64, random: f64) -> String {
    format!("c{}{}", to_base36(now_ms), random_suffix(random))
}

fn text_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

// The first user message's text parts, cut to 60 UTF-16 units.
fn derive_title(messages: &[Value]) -> String {
    let text = messages
        .iter()
        .find(|m| text_of(m, "role") == Some("user"))
        .and_then(|m| m.get("parts"))
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|p| text_of(p, "type") == Some("text"))
                .map(|p| text_of(p, "text").unwrap_or(""))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let text = js_trim(&text);
    if text.is_empty() {
        return "New conversation".to_owned();
    }
    match utf16_cut(text, TITLE_MAX_UNITS) {
        Some(head) => format!("{head}…"),
        None => text.to_owned(),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ChatHistory {
    conversations: Vec<ChatConversation>,
}

impl ChatHistory {
    pub fn load(storage: &dyn Storage) -> Self {
        let state = load_persisted(storage, NAME);
        Self {
            conversations: list_field(&state, "conversations").unwrap_or_default(),
        }
    }

    pub fn conversations(&self) -> &[ChatConversation] {
        &self.conversations
    }

    /// Insert or update a conversation, keeping the list newest-first.
    /// `now_ms` stands in for `Date.now()`. The state changes even when the
    /// write fails, as with `persist`.
    pub fn save(
        &mut self,
        storage: &dyn Storage,
        id: &str,
        agent_id: AcpAgentId,
        messages: Vec<Value>,
        now_ms: u64,
    ) -> io::Result<()> {
        // An empty conversation is not kept, but `persist` still wrote the
        // unchanged state on that call, so this does too.
        if !messages.is_empty() {
            let existing = self.conversations.iter().position(|c| c.id == id);
            let created_at = existing
                .and_then(|at| self.conversations.get(at))
                .map_or(now_ms, |c| c.created_at);
            self.conversations.retain(|c| c.id != id);
            self.conversations.insert(
                0,
                ChatConversation {
                    id: id.to_owned(),
                    agent_id,
                    title: derive_title(&messages),
                    messages,
                    created_at,
                    updated_at: now_ms,
                },
            );
            self.conversations.truncate(MAX_CONVERSATIONS);
        }
        save_persisted(storage, NAME, self)
    }

    pub fn remove(&mut self, storage: &dyn Storage, id: &str) -> io::Result<()> {
        self.conversations.retain(|c| c.id != id);
        save_persisted(storage, NAME, self)
    }

    pub fn clear(&mut self, storage: &dyn Storage) -> io::Result<()> {
        self.conversations.clear();
        save_persisted(storage, NAME, self)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::storage::MemoryStorage;

    fn user(text: &str) -> Value {
        json!({ "id": "m1", "role": "user", "parts": [{ "type": "text", "text": text }] })
    }

    fn titled(messages: Vec<Value>) -> String {
        let storage = MemoryStorage::new();
        let mut history = ChatHistory::load(&storage);
        history
            .save(&storage, "c1", AcpAgentId::ClaudeCode, messages, 1)
            .unwrap();
        history.conversations()[0].title.clone()
    }

    fn ids(history: &ChatHistory) -> Vec<&str> {
        history
            .conversations()
            .iter()
            .map(|c| c.id.as_str())
            .collect()
    }

    #[test]
    fn 대화_id는_c와_36진수_시각과_난수_여섯_자리다() {
        // `c${(1700000000000).toString(36)}${(0.123456789).toString(36).slice(2, 8)}`
        assert_eq!(
            new_conversation_id(1_700_000_000_000, 0.123456789),
            "cloyw3v284fzzzx"
        );
        // A random value with few base-36 digits gives a shorter id.
        assert_eq!(new_conversation_id(0, 0.5), "c0i");
    }

    #[test]
    fn 새_대화를_맨_앞에_넣고_persist_형식으로_저장한다() {
        let storage = MemoryStorage::new();
        let mut history = ChatHistory::load(&storage);
        history
            .save(
                &storage,
                "c1",
                AcpAgentId::ClaudeCode,
                vec![user("hello")],
                100,
            )
            .unwrap();
        history
            .save(&storage, "c2", AcpAgentId::Codex, vec![user("second")], 200)
            .unwrap();
        assert_eq!(ids(&history), ["c2", "c1"]);
        assert_eq!(
            history.conversations()[1],
            ChatConversation {
                id: "c1".to_owned(),
                agent_id: AcpAgentId::ClaudeCode,
                messages: vec![user("hello")],
                title: "hello".to_owned(),
                created_at: 100,
                updated_at: 100,
            }
        );
        assert_eq!(ChatHistory::load(&storage), history);

        let stored: Value =
            serde_json::from_str(&storage.get("rev:chat-history").unwrap()).unwrap();
        assert_eq!(stored["version"], 0);
        assert_eq!(
            stored["state"]["conversations"][1],
            json!({
                "id": "c1",
                "agentId": "claude-code",
                "messages": [{ "id": "m1", "role": "user", "parts": [{ "type": "text", "text": "hello" }] }],
                "title": "hello",
                "createdAt": 100,
                "updatedAt": 100
            })
        );
    }

    #[test]
    fn 같은_id로_다시_저장하면_맨_앞으로_옮기고_생성_시각은_유지한다() {
        let storage = MemoryStorage::new();
        let mut history = ChatHistory::load(&storage);
        history
            .save(
                &storage,
                "c1",
                AcpAgentId::ClaudeCode,
                vec![user("first")],
                100,
            )
            .unwrap();
        history
            .save(
                &storage,
                "c2",
                AcpAgentId::ClaudeCode,
                vec![user("other")],
                200,
            )
            .unwrap();
        history
            .save(
                &storage,
                "c1",
                AcpAgentId::Codex,
                vec![user("first"), json!({ "role": "assistant", "parts": [] })],
                300,
            )
            .unwrap();
        assert_eq!(ids(&history), ["c1", "c2"]);
        let c1 = &history.conversations()[0];
        assert_eq!((c1.created_at, c1.updated_at), (100, 300));
        assert_eq!(c1.agent_id, AcpAgentId::Codex);
        assert_eq!(c1.messages.len(), 2);
    }

    #[test]
    fn 메시지가_없으면_대화를_저장하지_않는다() {
        let storage = MemoryStorage::new();
        let mut history = ChatHistory::load(&storage);
        history
            .save(
                &storage,
                "c1",
                AcpAgentId::ClaudeCode,
                vec![user("keep")],
                100,
            )
            .unwrap();
        history
            .save(&storage, "c1", AcpAgentId::ClaudeCode, vec![], 200)
            .unwrap();
        history
            .save(&storage, "c2", AcpAgentId::ClaudeCode, vec![], 200)
            .unwrap();
        assert_eq!(ids(&history), ["c1"]);
        assert_eq!(history.conversations()[0].updated_at, 100);

        // The unchanged state is still written, as `persist` did on every set.
        let fresh = MemoryStorage::new();
        ChatHistory::load(&fresh)
            .save(&fresh, "c1", AcpAgentId::ClaudeCode, vec![], 1)
            .unwrap();
        assert_eq!(
            fresh.get("rev:chat-history").as_deref(),
            Some(r#"{"state":{"conversations":[]},"version":0}"#)
        );
    }

    #[test]
    fn 대화는_30개까지만_남기고_가장_오래된_것을_버린다() {
        let storage = MemoryStorage::new();
        let mut history = ChatHistory::load(&storage);
        for i in 0..32u64 {
            history
                .save(
                    &storage,
                    &format!("c{i}"),
                    AcpAgentId::ClaudeCode,
                    vec![user("x")],
                    i,
                )
                .unwrap();
        }
        assert_eq!(history.conversations().len(), 30);
        assert_eq!(history.conversations().first().unwrap().id, "c31");
        assert_eq!(history.conversations().last().unwrap().id, "c2");
        // Re-saving a kept conversation reorders without dropping another one.
        history
            .save(&storage, "c2", AcpAgentId::ClaudeCode, vec![user("x")], 99)
            .unwrap();
        assert_eq!(history.conversations().len(), 30);
        assert_eq!(history.conversations().last().unwrap().id, "c3");
    }

    #[test]
    fn remove와_clear는_저장본에도_반영된다() {
        let storage = MemoryStorage::new();
        let mut history = ChatHistory::load(&storage);
        for id in ["c1", "c2", "c3"] {
            history
                .save(&storage, id, AcpAgentId::ClaudeCode, vec![user("x")], 1)
                .unwrap();
        }
        history.remove(&storage, "c2").unwrap();
        history.remove(&storage, "missing").unwrap();
        assert_eq!(ids(&history), ["c3", "c1"]);
        assert_eq!(ids(&ChatHistory::load(&storage)), ["c3", "c1"]);
        history.clear(&storage).unwrap();
        assert_eq!(
            storage.get("rev:chat-history").as_deref(),
            Some(r#"{"state":{"conversations":[]},"version":0}"#)
        );
    }

    #[test]
    fn 제목은_첫_사용자_메시지의_텍스트_파트를_공백으로_이어_만든다() {
        let messages = vec![
            json!({ "role": "assistant", "parts": [{ "type": "text", "text": "ignored" }] }),
            json!({ "role": "user", "parts": [
                { "type": "text", "text": "  find the" },
                { "type": "file", "url": "x" },
                { "type": "text", "text": "login API  " }
            ] }),
            user("later message"),
        ];
        assert_eq!(titled(messages), "find the login API");
    }

    #[test]
    fn 텍스트가_빠진_텍스트_파트는_빈_문자열로_이어진다() {
        // Array.prototype.join writes undefined and null as "", so the
        // separator stays: ["a", undefined, "b"].join(" ") === "a  b".
        let messages = vec![json!({ "role": "user", "parts": [
            { "type": "text", "text": "a" },
            { "type": "text" },
            { "type": "text", "text": null },
            { "type": "text", "text": "b" }
        ] })];
        assert_eq!(titled(messages), "a   b");
        // Only the first user message counts, and the role is case-sensitive.
        assert_eq!(
            titled(vec![
                json!({ "role": "USER", "parts": [{ "type": "text", "text": "x" }] })
            ]),
            "New conversation"
        );
    }

    #[test]
    fn 사용자_텍스트가_없으면_제목은_new_conversation이다() {
        for messages in [
            vec![json!({ "role": "assistant", "parts": [{ "type": "text", "text": "hi" }] })],
            vec![json!({ "role": "user", "parts": [] })],
            vec![json!({ "role": "user", "parts": [{ "type": "file", "url": "x" }] })],
            vec![user(" \n\t\u{FEFF} ")],
            // Malformed messages must not panic.
            vec![json!({ "role": "user" })],
            vec![json!({ "role": "user", "parts": "text" })],
            vec![json!(null), json!("user"), json!(5)],
        ] {
            assert_eq!(titled(messages.clone()), "New conversation", "{messages:?}");
        }
    }

    #[test]
    fn 제목은_utf16_60단위를_넘으면_잘라_말줄임표를_붙인다() {
        let sixty = "a".repeat(60);
        assert_eq!(titled(vec![user(&sixty)]), sixty);
        assert_eq!(titled(vec![user(&"a".repeat(61))]), format!("{sixty}…"));
        // Hangul is one UTF-16 unit per syllable, three bytes in UTF-8.
        let hangul = "가".repeat(61);
        assert_eq!(titled(vec![user(&hangul)]), format!("{}…", "가".repeat(60)));
        // An emoji is two units: 30 fit exactly, a 31st makes it too long.
        assert_eq!(titled(vec![user(&"😀".repeat(30))]), "😀".repeat(30));
        assert_eq!(
            titled(vec![user(&"😀".repeat(31))]),
            format!("{}…", "😀".repeat(30))
        );
    }

    #[test]
    fn 형식이_맞지_않는_저장_대화는_버리고_나머지는_불러온다() {
        let storage = MemoryStorage::new();
        storage
            .set(
                "rev:chat-history",
                r#"{"state":{"conversations":[{"id":"ok","agentId":"grok","messages":[],"title":"t","createdAt":1,"updatedAt":2},{"id":"bad","agentId":"gemini","messages":[],"title":"t","createdAt":1,"updatedAt":2},{"id":"no-fields"},null]},"version":0}"#,
            )
            .unwrap();
        let history = ChatHistory::load(&storage);
        assert_eq!(ids(&history), ["ok"]);
        assert_eq!(history.conversations()[0].agent_id, AcpAgentId::Grok);

        storage.set("rev:chat-history", "{broken").unwrap();
        assert!(ChatHistory::load(&storage).conversations().is_empty());
    }
}
