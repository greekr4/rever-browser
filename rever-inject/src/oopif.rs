//! Port of `src/main/mcp/oopif.ts` — registry of out-of-process iframe (OOPIF) CDP sessions.
//!
//! A cross-SITE iframe runs in its own renderer, so the page's CDP session
//! cannot read it: `Accessibility.getFullAXTree({frameId})` rejects and the
//! frame never appears in `Page.getFrameTree`. `Target.setAutoAttach` with
//! `flatten: true` gives us a sessionId per such frame, and every command for
//! that frame's nodes must carry it.
//!
//! Routing is not optional. A probe against real Chrome showed the parent
//! session answering `DOM.resolveNode` for an OOPIF backendNodeId with a
//! DIFFERENT node rather than an error — backend ids are per-process and
//! collide, so an unrouted click lands on whatever shares the number.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OopifSession {
    pub session_id: String,
    /// CDP targetId, which for an iframe target is also its frameId.
    pub frame_id: String,
    pub url: String,
    /// Session holding this frame's `<iframe>` element — the session the attach
    /// event arrived on. `None` means the page itself. A frame nested inside
    /// another out-of-process frame can only have its owner resolved here, and
    /// asking the page session instead answers about an unrelated node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
}

/// webContentsId → sessions.
///
/// The sessions of a target are a Vec, not a HashMap: the TypeScript keeps them
/// in a JS Map, which lists in insertion order, and callers cut that list
/// (`slice(0, MAX_FRAMES)`), so the order is part of the behavior.
#[derive(Debug, Default)]
pub struct OopifRegistry {
    by_target: HashMap<u64, Vec<OopifSession>>,
}

impl OopifRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// A session id that is already registered is replaced where it stands,
    /// like `Map.set` on an existing key.
    pub fn register(&mut self, web_contents_id: u64, session: OopifSession) {
        let sessions = self.by_target.entry(web_contents_id).or_default();
        match sessions
            .iter_mut()
            .find(|known| known.session_id == session.session_id)
        {
            Some(known) => *known = session,
            None => sessions.push(session),
        }
    }

    pub fn unregister(&mut self, web_contents_id: u64, session_id: &str) {
        if let Some(sessions) = self.by_target.get_mut(&web_contents_id) {
            sessions.retain(|known| known.session_id != session_id);
        }
    }

    pub fn clear(&mut self, web_contents_id: u64) {
        self.by_target.remove(&web_contents_id);
    }

    /// An iframe attaches before it navigates, so its URL only arrives with a
    /// later `Target.targetInfoChanged`. The TypeScript handles that by writing
    /// `url` on the object `listOopifSessions` handed out, which is the stored
    /// one; `list` gives copies here, so that write has its own method. Like the
    /// `find` it replaces, it touches the first session with that frame id.
    /// Returns whether one was found.
    pub fn update_frame_url(&mut self, web_contents_id: u64, frame_id: &str, url: &str) -> bool {
        let found = self
            .by_target
            .get_mut(&web_contents_id)
            .and_then(|sessions| sessions.iter_mut().find(|known| known.frame_id == frame_id));
        match found {
            Some(known) => {
                known.url = url.to_owned();
                true
            }
            None => false,
        }
    }

    /// A new list, as in the TypeScript: the caller may register or unregister
    /// while it walks it. The sessions in it are copies too, unlike there; see
    /// `update_frame_url` for the one write that relied on them being shared.
    pub fn list(&self, web_contents_id: u64) -> Vec<OopifSession> {
        self.by_target
            .get(&web_contents_id)
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(session_id: &str, url: &str) -> OopifSession {
        OopifSession {
            session_id: session_id.to_owned(),
            frame_id: format!("frame-{session_id}"),
            url: url.to_owned(),
            parent_session_id: None,
        }
    }

    fn ids(registry: &OopifRegistry, web_contents_id: u64) -> Vec<String> {
        registry
            .list(web_contents_id)
            .into_iter()
            .map(|s| s.session_id)
            .collect()
    }

    #[test]
    fn 등록한_순서대로_나열한다() {
        let mut registry = OopifRegistry::new();
        registry.register(1, session("b", "https://b.test/"));
        registry.register(1, session("a", "https://a.test/"));
        registry.register(1, session("c", "https://c.test/"));
        assert_eq!(ids(&registry, 1), ["b", "a", "c"]);
    }

    #[test]
    fn 모르는_대상은_빈_목록이다() {
        assert!(OopifRegistry::new().list(7).is_empty());
    }

    #[test]
    fn 같은_세션_id로_다시_등록하면_자리를_지킨_채_값을_바꾼다() {
        let mut registry = OopifRegistry::new();
        registry.register(1, session("a", "https://a.test/"));
        registry.register(1, session("b", "https://b.test/"));
        registry.register(1, session("a", "https://a.test/after-navigation"));
        let sessions = registry.list(1);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, "a");
        assert_eq!(sessions[0].url, "https://a.test/after-navigation");
        assert_eq!(sessions[1].session_id, "b");
    }

    #[test]
    fn 해제하면_그_세션만_빠진다() {
        let mut registry = OopifRegistry::new();
        registry.register(1, session("a", "https://a.test/"));
        registry.register(1, session("b", "https://b.test/"));
        registry.unregister(1, "a");
        assert_eq!(ids(&registry, 1), ["b"]);
    }

    #[test]
    fn 해제한_뒤_다시_등록하면_맨_뒤로_간다() {
        let mut registry = OopifRegistry::new();
        registry.register(1, session("a", "https://a.test/"));
        registry.register(1, session("b", "https://b.test/"));
        registry.unregister(1, "a");
        registry.register(1, session("a", "https://a.test/"));
        assert_eq!(ids(&registry, 1), ["b", "a"]);
    }

    #[test]
    fn 모르는_대상이나_세션을_해제해도_아무_일_없다() {
        let mut registry = OopifRegistry::new();
        registry.unregister(9, "nope");
        registry.register(1, session("a", "https://a.test/"));
        registry.unregister(1, "nope");
        assert_eq!(ids(&registry, 1), ["a"]);
        assert!(registry.list(9).is_empty());
    }

    #[test]
    fn 대상끼리_세션이_섞이지_않는다() {
        let mut registry = OopifRegistry::new();
        registry.register(1, session("a", "https://a.test/"));
        registry.register(2, session("a", "https://other.test/"));
        registry.register(2, session("b", "https://b.test/"));
        assert_eq!(ids(&registry, 1), ["a"]);
        assert_eq!(registry.list(2)[0].url, "https://other.test/");
        registry.unregister(2, "a");
        assert_eq!(ids(&registry, 1), ["a"]);
        assert_eq!(ids(&registry, 2), ["b"]);
    }

    #[test]
    fn 비우면_그_대상의_세션만_사라진다() {
        let mut registry = OopifRegistry::new();
        registry.register(1, session("a", "https://a.test/"));
        registry.register(2, session("b", "https://b.test/"));
        registry.clear(1);
        registry.clear(3);
        assert!(registry.list(1).is_empty());
        assert_eq!(ids(&registry, 2), ["b"]);
    }

    #[test]
    fn 나열한_목록은_사본이라_이후_변경이_반영되지_않는다() {
        let mut registry = OopifRegistry::new();
        registry.register(1, session("a", "https://a.test/"));
        let before = registry.list(1);
        registry.unregister(1, "a");
        assert_eq!(before.len(), 1);
        assert!(registry.list(1).is_empty());
    }

    #[test]
    fn 프레임_id로_url을_바꾸면_자리는_그대로고_다음_목록에_보인다() {
        let mut registry = OopifRegistry::new();
        registry.register(1, session("a", ""));
        registry.register(1, session("b", "https://b.test/"));
        assert!(registry.update_frame_url(1, "frame-a", "https://a.test/loaded"));
        let sessions = registry.list(1);
        assert_eq!(ids(&registry, 1), ["a", "b"]);
        assert_eq!(sessions[0].url, "https://a.test/loaded");
        assert_eq!(sessions[1].url, "https://b.test/");
    }

    #[test]
    fn 모르는_프레임이나_대상의_url은_바꾸지_않는다() {
        let mut registry = OopifRegistry::new();
        registry.register(1, session("a", "https://a.test/"));
        // The frame id is the key here, not the session id.
        assert!(!registry.update_frame_url(1, "a", "https://x.test/"));
        assert!(!registry.update_frame_url(2, "frame-a", "https://x.test/"));
        assert_eq!(registry.list(1)[0].url, "https://a.test/");
        assert!(registry.list(2).is_empty());
    }

    #[test]
    fn 프레임_id가_겹치면_먼저_등록된_세션의_url만_바꾼다() {
        let mut registry = OopifRegistry::new();
        for session_id in ["a", "b"] {
            registry.register(
                1,
                OopifSession {
                    frame_id: "shared".to_owned(),
                    ..session(session_id, "")
                },
            );
        }
        assert!(registry.update_frame_url(1, "shared", "https://a.test/"));
        let sessions = registry.list(1);
        assert_eq!(sessions[0].url, "https://a.test/");
        assert_eq!(sessions[1].url, "");
    }

    #[test]
    fn json은_camel_case이고_부모_세션이_없으면_필드를_뺀다() {
        let page_child = session("s1", "https://a.test/");
        assert_eq!(
            serde_json::to_value(&page_child).unwrap(),
            serde_json::json!({ "sessionId": "s1", "frameId": "frame-s1", "url": "https://a.test/" })
        );
        let nested = OopifSession {
            parent_session_id: Some("s1".to_owned()),
            ..session("s2", "https://b.test/")
        };
        assert_eq!(
            serde_json::to_value(&nested).unwrap(),
            serde_json::json!({
                "sessionId": "s2",
                "frameId": "frame-s2",
                "url": "https://b.test/",
                "parentSessionId": "s1"
            })
        );
    }

    #[test]
    fn 부모_세션_필드가_없는_json도_읽는다() {
        let parsed: OopifSession =
            serde_json::from_str(r#"{"sessionId":"s1","frameId":"f1","url":"https://a.test/"}"#)
                .unwrap();
        assert_eq!(parsed.parent_session_id, None);
        assert_eq!(parsed.frame_id, "f1");
        assert!(serde_json::from_str::<OopifSession>(r#"{"sessionId":"s1"}"#).is_err());
    }
}
