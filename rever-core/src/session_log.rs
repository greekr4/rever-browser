//! Port of `src/main/session-log.ts` — an append-only transcript of every prompt
//! and streamed update, one JSON object per line, one file per session. The
//! agent CLI keeps its own jsonl; this one sits where the app controls it, with
//! secrets and PII masked before they reach the disk.
//!
//! The TypeScript appends in the background; a line is small, so this writes it
//! in place. "Now" is passed in, like everywhere else in this crate.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::secret_scan::{redact, redact_deep};
use crate::time::to_iso_string;

pub struct SessionLog {
    dir: PathBuf,
}

impl SessionLog {
    /// `user_data_dir` is `paths::user_data_dir`; the logs go in `session-logs` under it.
    pub fn new(user_data_dir: &Path) -> Self {
        Self {
            dir: user_data_dir.join("session-logs"),
        }
    }

    fn file_for(&self, session_id: &str) -> PathBuf {
        // Session ids carry provider prefixes like "glm:" — keep the path flat/safe.
        // One '_' per UTF-16 unit, as the TypeScript's regex replaces them.
        let safe: String = session_id
            .chars()
            .map(|c| match c {
                'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' => c.to_string(),
                _ => "_".repeat(c.len_utf16()),
            })
            .collect();
        self.dir.join(format!("{safe}.jsonl"))
    }

    fn append(&self, session_id: &str, now_ms: u64, key: &str, value: Value, dir: &str) {
        let line = json!({
            "ts": to_iso_string(now_ms),
            "sessionId": session_id,
            "dir": dir,
            key: value,
        });
        // Logging is best-effort; never break a turn because the disk write failed.
        let _ = fs::create_dir_all(&self.dir).and_then(|()| {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.file_for(session_id))?;
            file.write_all(format!("{line}\n").as_bytes())
        });
    }

    pub fn log_prompt(&self, session_id: &str, text: &str, now_ms: u64) {
        self.append(session_id, now_ms, "text", redact(text).into(), "prompt");
    }

    /// `notification` is the ACP `SessionNotification` as JSON.
    pub fn log_update(&self, session_id: &str, notification: &Value, now_ms: u64) {
        let update = redact_deep(notification);
        self.append(session_id, now_ms, "update", update, "update");
    }

    pub fn log_stop(&self, session_id: &str, stop_reason: &str, now_ms: u64) {
        self.append(session_id, now_ms, "stopReason", stop_reason.into(), "stop");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_791_421_323_000;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rever-session-log-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn 프롬프트_업데이트_종료를_한_줄씩_세션_파일에_덧붙인다() {
        let root = temp("append");
        let log = SessionLog::new(&root);
        log.log_prompt("glm:abc", "hello", NOW);
        log.log_update("glm:abc", &json!({ "update": { "text": "hi" } }), NOW + 1);
        log.log_stop("glm:abc", "end_turn", NOW + 2);

        let text = fs::read_to_string(root.join("session-logs").join("glm_abc.jsonl")).unwrap();
        assert_eq!(
            text,
            concat!(
                r#"{"ts":"2026-10-08T01:02:03.000Z","sessionId":"glm:abc","dir":"prompt","text":"hello"}"#,
                "\n",
                r#"{"ts":"2026-10-08T01:02:03.001Z","sessionId":"glm:abc","dir":"update","update":{"update":{"text":"hi"}}}"#,
                "\n",
                r#"{"ts":"2026-10-08T01:02:03.002Z","sessionId":"glm:abc","dir":"stop","stopReason":"end_turn"}"#,
                "\n",
            )
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 비밀값은_디스크에_쓰기_전에_가린다() {
        let root = temp("redact");
        let log = SessionLog::new(&root);
        let secret = "6c70a033abcd1234efgh5678";
        log.log_prompt("s", &format!("Authorization: Bearer {secret}"), NOW);
        log.log_update("s", &json!({ "text": format!("Bearer {secret}") }), NOW);

        let text = fs::read_to_string(root.join("session-logs").join("s.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(!text.contains(secret));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 세션_id는_안전한_파일_이름으로_바꾼다() {
        let log = SessionLog::new(Path::new("/data"));
        assert_eq!(
            log.file_for("../grok:a b/한😀.x_-"),
            Path::new("/data/session-logs/.._grok_a_b____.x_-.jsonl")
        );
    }

    #[test]
    fn 쓸_수_없는_위치여도_패닉하지_않는다() {
        let root = temp("blocked");
        // A file where the directory should be: create_dir_all fails.
        fs::write(&root, "x").unwrap();
        SessionLog::new(&root).log_stop("s", "end_turn", NOW);
        let _ = fs::remove_file(&root);
    }
}
