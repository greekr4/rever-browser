//! Port of `src/main/script-registry.ts` — scriptId → URL for every script the
//! attached tabs have parsed, fed from Debugger.scriptParsed. CDP reports
//! listener and function locations by scriptId only, so tools that point at
//! code (inspect_listeners, execution traces) need this to say *which file* a
//! location is in.
//!
//! Keyed by tab + CDP session + scriptId: script ids are per isolate, so two
//! tabs (or a page and its out-of-process frame) reuse the same ids.

use std::collections::{BTreeMap, HashMap};

const MAX_SCRIPTS: usize = 20_000;

/// The TypeScript kept one insertion-ordered Map for the whole process; here
/// the owner holds a registry. `order` stands in for the Map's insertion order.
#[derive(Debug, Default)]
pub struct ScriptRegistry {
    /// key → (insertion sequence, url)
    scripts: HashMap<String, (u64, String)>,
    /// insertion sequence → key, oldest first
    order: BTreeMap<u64, String>,
    next_seq: u64,
}

fn key(wc_id: u64, session_id: Option<&str>, script_id: &str) -> String {
    format!("{wc_id}:{}:{script_id}", session_id.unwrap_or(""))
}

impl ScriptRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_script(
        &mut self,
        wc_id: u64,
        session_id: Option<&str>,
        script_id: &str,
        url: &str,
    ) {
        let k = key(wc_id, session_id, script_id);
        let seq = self.next_seq;
        self.next_seq += 1;
        // Recording a script again makes it the newest entry.
        if let Some((old_seq, _)) = self.scripts.insert(k.clone(), (seq, url.to_owned())) {
            self.order.remove(&old_seq);
        }
        self.order.insert(seq, k);
        while self.scripts.len() > MAX_SCRIPTS {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            self.scripts.remove(&oldest);
        }
    }

    /// None for unknown ids and for scripts with no URL (inline / eval).
    pub fn script_url(
        &self,
        wc_id: u64,
        session_id: Option<&str>,
        script_id: &str,
    ) -> Option<&str> {
        let (_, url) = self.scripts.get(&key(wc_id, session_id, script_id))?;
        (!url.is_empty()).then_some(url.as_str())
    }

    pub fn clear_scripts_for(&mut self, wc_id: u64) {
        let prefix = format!("{wc_id}:");
        let order = &mut self.order;
        self.scripts.retain(|k, (seq, _)| {
            let keep = !k.starts_with(&prefix);
            if !keep {
                order.remove(seq);
            }
            keep
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 탭_세션_script_id_조합으로_url을_기록하고_찾는다() {
        let mut registry = ScriptRegistry::new();
        registry.record_script(1, None, "42", "https://a.io/app.js");
        registry.record_script(1, Some("oopif-1"), "42", "https://b.io/frame.js");
        registry.record_script(2, None, "42", "https://c.io/other.js");
        assert_eq!(
            registry.script_url(1, None, "42"),
            Some("https://a.io/app.js")
        );
        assert_eq!(
            registry.script_url(1, Some("oopif-1"), "42"),
            Some("https://b.io/frame.js")
        );
        assert_eq!(
            registry.script_url(2, None, "42"),
            Some("https://c.io/other.js")
        );
        assert_eq!(registry.script_url(1, None, "99"), None);
    }

    #[test]
    fn url이_빈_인라인_eval_스크립트는_none으로_돌려준다() {
        let mut registry = ScriptRegistry::new();
        registry.record_script(1, None, "7", "");
        assert_eq!(registry.script_url(1, None, "7"), None);
    }

    #[test]
    fn 상한을_넘으면_가장_오래된_기록부터_지운다() {
        let mut registry = ScriptRegistry::new();
        for i in 0..20_001 {
            registry.record_script(1, None, &i.to_string(), &format!("u{i}"));
        }
        assert_eq!(registry.script_url(1, None, "0"), None);
        assert_eq!(registry.script_url(1, None, "20000"), Some("u20000"));
    }

    #[test]
    fn 탭이_닫히면_그_탭의_기록만_지운다() {
        let mut registry = ScriptRegistry::new();
        registry.record_script(1, None, "1", "a");
        registry.record_script(2, None, "1", "b");
        registry.clear_scripts_for(1);
        assert_eq!(registry.script_url(1, None, "1"), None);
        assert_eq!(registry.script_url(2, None, "1"), Some("b"));
    }

    // Not in the TypeScript suite: the Map's delete-then-set is what this port rebuilt by hand.

    #[test]
    fn 다시_기록한_스크립트는_가장_새_기록이_되어_밀려나지_않는다() {
        let mut registry = ScriptRegistry::new();
        registry.record_script(1, None, "first", "old");
        registry.record_script(1, None, "second", "s");
        registry.record_script(1, None, "first", "new");
        for i in 0..MAX_SCRIPTS - 1 {
            registry.record_script(2, None, &i.to_string(), "x");
        }
        assert_eq!(registry.script_url(1, None, "first"), Some("new"));
        assert_eq!(registry.script_url(1, None, "second"), None);
        assert_eq!(registry.scripts.len(), MAX_SCRIPTS);
        assert_eq!(registry.order.len(), MAX_SCRIPTS);
    }
}
