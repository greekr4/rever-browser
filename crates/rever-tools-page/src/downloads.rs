//! Port of `src/main/mcp/tools/downloads.ts` — the files the page downloaded.
//!
//! The TypeScript read the download list its Electron session kept, fed by
//! `will-download`. Servo has no download event, so the app detects a download
//! from the network it taps (a top-level navigation answered with
//! `Content-Disposition: attachment`) and records it in [`DownloadState`], which
//! these tools read. Files are saved to `~/Downloads/Rever`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rever_tools_api::tool::{ToolDef, ToolRegistry};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{now_ms, pretty, schema};

const MAX_RECORDS: usize = 100;
/// `wait_for_download` ignores downloads that finished longer ago than this.
const RECENT_MS: u64 = 30_000;

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ListDownloads);
    registry.register(WaitForDownload);
}

/// One download, as the tools print it. Mirrors `DownloadRecord` in downloads.ts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadRecord {
    pub id: u64,
    pub url: String,
    pub filename: String,
    pub mime: String,
    pub total_bytes: u64,
    pub received_bytes: u64,
    /// "completed" or "interrupted" — Servo hands over a finished body.
    pub state: String,
    pub save_path: String,
    /// Epoch milliseconds.
    pub started_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<u64>,
}

/// The downloads the app detected, shared with the tools (`ctx.state::<DownloadState>()`).
pub struct DownloadState {
    records: Mutex<Vec<DownloadRecord>>,
    /// Ids `wait_for_download` has already returned, so each is returned once.
    reported: Mutex<HashSet<u64>>,
    next_id: AtomicU64,
    /// `~/Downloads/Rever`.
    dir: PathBuf,
    /// When on, a download is recorded but not written (no save dialog here yet).
    ask_where_to_save: Mutex<bool>,
}

impl Default for DownloadState {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        Self {
            records: Mutex::new(Vec::new()),
            reported: Mutex::new(HashSet::new()),
            next_id: AtomicU64::new(1),
            dir: PathBuf::from(home).join("Downloads").join("Rever"),
            ask_where_to_save: Mutex::new(false),
        }
    }
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `name`, or `name (n)` before the extension, whichever `taken` does not claim.
pub fn unique_filename(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_owned();
    }
    let ext = Path::new(name)
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    let base = &name[..name.len() - ext.len()];
    for i in 1.. {
        let candidate = format!("{base} ({i}){ext}");
        if !taken(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

/// A safe file name from a `Content-Disposition` value, or the URL's last
/// segment. Only the base name is kept, so a server cannot write outside the
/// download folder with `filename="../.."`.
pub fn download_filename(disposition: &str, url: &str) -> String {
    let from_header = disposition
        .split(';')
        .filter_map(|part| part.trim().strip_prefix("filename="))
        .map(|value| value.trim_matches('"'))
        .next()
        .filter(|value| !value.is_empty());
    let from_url = || {
        url.rsplit('/')
            .next()
            .and_then(|last| last.split(['?', '#']).next())
            .filter(|last| !last.is_empty())
    };
    let name = from_header.or_else(from_url).unwrap_or("download");
    // Base name only: no path separators, never "." or "..".
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base {
        "" | "." | ".." => "download".to_owned(),
        base => base.to_owned(),
    }
}

impl DownloadState {
    pub fn set_ask_where_to_save(&self, enabled: bool) {
        *locked(&self.ask_where_to_save) = enabled;
    }

    /// Record a download and, unless the user asked to be prompted, write its
    /// body to `~/Downloads/Rever`. Returns the record.
    pub fn record(
        &self,
        url: &str,
        filename: &str,
        mime: &str,
        body: &[u8],
        now: u64,
    ) -> DownloadRecord {
        let ask = *locked(&self.ask_where_to_save);
        let (filename, save_path) = if ask {
            (filename.to_owned(), String::new())
        } else {
            let existing: Vec<String> = locked(&self.records)
                .iter()
                .map(|record| record.save_path.clone())
                .collect();
            let name = unique_filename(filename, |candidate| {
                let path = self.dir.join(candidate);
                path.exists()
                    || existing
                        .iter()
                        .any(|taken| *taken == path.to_string_lossy())
            });
            let path = self.dir.join(&name);
            // Best-effort: a failed write still records the attempt, as a browser's would.
            let _ = std::fs::create_dir_all(&self.dir).and_then(|()| std::fs::write(&path, body));
            (name, path.to_string_lossy().into_owned())
        };
        let record = DownloadRecord {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            url: url.to_owned(),
            filename,
            mime: mime.to_owned(),
            total_bytes: body.len() as u64,
            received_bytes: body.len() as u64,
            state: "completed".to_owned(),
            save_path,
            started_at: now,
            ended_at: Some(now),
        };
        let mut records = locked(&self.records);
        records.push(record.clone());
        if records.len() > MAX_RECORDS {
            let dropped = records.remove(0);
            locked(&self.reported).remove(&dropped.id);
        }
        record
    }

    fn list(&self, limit: usize) -> Vec<DownloadRecord> {
        let records = locked(&self.records);
        records[records.len().saturating_sub(limit)..].to_vec()
    }

    /// The next finished download not yet returned that ended in the last 30s.
    fn take_next(&self, now: u64) -> Option<DownloadRecord> {
        let records = locked(&self.records);
        let mut reported = locked(&self.reported);
        let found = records.iter().find(|record| {
            record.state != "progressing"
                && !reported.contains(&record.id)
                && record.ended_at.unwrap_or(0) >= now.saturating_sub(RECENT_MS)
        })?;
        reported.insert(found.id);
        Some(found.clone())
    }
}

tool!(
    ListDownloads,
    ToolDef::new(
        "list_downloads",
        "List files the page downloaded (newest last): filename, url, mime, bytes, state (progressing/completed/cancelled/interrupted) and savePath. Downloads are saved to ~/Downloads/Rever without a save dialog unless the user turned on \"ask where to save\".",
        schema(
            json!({
                "limit": {
                    "description": "Default 20",
                    "type": "integer",
                    "exclusiveMinimum": 0,
                    "maximum": 100
                }
            }),
            &[]
        )
    ),
    |ctx, args| {
        let limit = args.0.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
        let state = ctx.state::<DownloadState>();
        Ok(pretty(&json!({
            "askWhereToSave": *locked(&state.ask_where_to_save),
            "downloads": state.list(limit),
        })))
    }
);

tool!(
    WaitForDownload,
    ToolDef::new(
        "wait_for_download",
        "Wait until a download finishes and return it (savePath, bytes, state). Call right after the click that starts a download. Returns a download that finished in the last 30s if you have not seen it yet, so calling it after a fast download is fine. Each download is returned once.",
        schema(
            json!({
                "timeoutMs": {
                    "description": "Default 30000",
                    "type": "integer",
                    "exclusiveMinimum": 0,
                    "maximum": 120000
                }
            }),
            &[]
        )
    ),
    |ctx, args| {
        let timeout = args
            .0
            .get("timeoutMs")
            .and_then(Value::as_u64)
            .unwrap_or(30_000);
        let state = ctx.state::<DownloadState>();
        let deadline = Instant::now() + Duration::from_millis(timeout);
        loop {
            if let Some(download) = state.take_next(now_ms()) {
                return Ok(pretty(&json!(download)));
            }
            if Instant::now() >= deadline {
                return Ok("no download finished before the timeout".to_owned());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{
        FakeBrowser, call, context, invalid, not_int, rejected, too_big, too_small,
    };

    const NOW: u64 = 1_791_421_323_000;

    fn temp() -> DownloadState {
        use std::sync::atomic::AtomicU32;
        static N: AtomicU32 = AtomicU32::new(0);
        let unique = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("rever-dl-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        DownloadState {
            dir,
            ..DownloadState::default()
        }
    }

    #[test]
    fn 겹치지_않으면_이름을_그대로_겹치면_번호를_붙인다() {
        assert_eq!(unique_filename("a.zip", |_| false), "a.zip");
        let taken = ["a.zip", "a (1).zip"];
        assert_eq!(
            unique_filename("a.zip", |name| taken.contains(&name)),
            "a (2).zip"
        );
        // No extension.
        assert_eq!(
            unique_filename("README", |name| name == "README"),
            "README (1)"
        );
    }

    #[test]
    fn content_disposition에서_파일명을_뽑고_경로_탈출을_막는다() {
        assert_eq!(
            download_filename("attachment; filename=\"report.zip\"", "http://x/d"),
            "report.zip"
        );
        // A traversal attempt keeps only the base name.
        assert_eq!(
            download_filename("attachment; filename=\"../../etc/passwd\"", "http://x/d"),
            "passwd"
        );
        // No usable filename: the URL's last segment, else "download".
        assert_eq!(
            download_filename("attachment", "http://x/a/b/file.bin?q=1"),
            "file.bin"
        );
        assert_eq!(
            download_filename("attachment; filename=\"..\"", "http://x/"),
            "download"
        );
    }

    #[test]
    fn 다운로드를_기록하고_파일로_저장하며_목록을_최신순으로_돌려준다() {
        let state = temp();
        let a = state.record("http://x/a.zip", "a.zip", "application/zip", b"AAA", NOW);
        assert_eq!((a.id, a.total_bytes, a.state.as_str()), (1, 3, "completed"));
        assert_eq!(std::fs::read(&a.save_path).unwrap(), b"AAA");
        // A second download of the same name does not overwrite the first.
        let b = state.record(
            "http://x/a.zip",
            "a.zip",
            "application/zip",
            b"BBBB",
            NOW + 1,
        );
        assert!(b.save_path.ends_with("a (1).zip"));
        assert_eq!(std::fs::read(&a.save_path).unwrap(), b"AAA");
        assert_eq!(
            state.list(10).iter().map(|d| d.id).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(state.list(1).iter().map(|d| d.id).collect::<Vec<_>>(), [2]);
        let _ = std::fs::remove_dir_all(&state.dir);
    }

    #[test]
    fn 대기는_끝난_다운로드를_한_번씩만_돌려주고_오래된_것은_거른다() {
        let state = temp();
        state.record("http://x/a.zip", "a.zip", "application/zip", b"A", NOW);
        // Returned once, then not again.
        assert_eq!(state.take_next(NOW).map(|d| d.id), Some(1));
        assert_eq!(state.take_next(NOW).map(|d| d.id), None);
        // A download that ended more than 30s ago is not returned.
        state.record("http://x/b.zip", "b.zip", "application/zip", b"B", NOW);
        assert_eq!(state.take_next(NOW + 31_000).map(|d| d.id), None);
        let _ = std::fs::remove_dir_all(&state.dir);
    }

    #[tokio::test]
    async fn 목록은_빈_상태를_ask_설정과_함께_돌려준다() {
        let (_, ctx) = context(FakeBrowser::default());
        for (limit, issue) in [
            (json!(0), too_small("limit")),
            (json!(101), too_big("limit", 100)),
            (json!(2.5), not_int("limit")),
        ] {
            assert_eq!(
                call(&ListDownloads, &ctx, json!({ "limit": limit })).await,
                rejected("list_downloads", json!([issue]))
            );
        }
        assert_eq!(
            call(&ListDownloads, &ctx, json!({ "limit": "20" })).await,
            invalid("list_downloads", "limit", "number", "string")
        );
        let listed = call(&ListDownloads, &ctx, json!({})).await.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&listed).unwrap(),
            json!({ "askWhereToSave": false, "downloads": [] })
        );
    }

    #[tokio::test]
    async fn 대기는_인자를_검사하고_없으면_시간_안에_없다고_답한다() {
        let (_, ctx) = context(FakeBrowser::default());
        for (timeout, issue) in [
            (json!(0), too_small("timeoutMs")),
            (json!(120_001), too_big("timeoutMs", 120_000)),
            (json!(-1), too_small("timeoutMs")),
        ] {
            assert_eq!(
                call(&WaitForDownload, &ctx, json!({ "timeoutMs": timeout })).await,
                rejected("wait_for_download", json!([issue]))
            );
        }
        assert_eq!(
            call(&WaitForDownload, &ctx, json!({ "timeoutMs": 1 })).await,
            Ok("no download finished before the timeout".to_owned())
        );
    }
}
