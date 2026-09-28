//! Active-conversation counting for per-instance tray tooltips (issue #72).
//!
//! DSH keeps a projection cache of its own session rows under
//! `<DSH_HOME>/storages/`. Two on-disk layouts exist in the wild and both are
//! read here:
//!
//! - **per-record** (newer): `storages/session_projcache/sessions/<id>.json`,
//!   each file holding `{ version, record: { identity, rows } }`.
//! - **aggregate** (older): `storages/session_projcache.json`, holding
//!   `tables.sessions[<id>] = { identity, rows }`.
//!
//! Only the row we need is inspected: `sessionStats.val.openStep`. A non-null
//! `openStep` means a step is executing *right now*; that alone is not enough
//! to call a conversation active, because a crashed or force-killed DSH leaves
//! the last checkpoint behind with `openStep` still set. So a record only
//! counts as active when it is also *fresh* — either its file was written
//! recently, or its `sessionListMetadata.val.lastPromptAt` is recent.
//!
//! This file is an internal DSH format, so the reader is deliberately
//! structure-agnostic: every lookup goes through `Value::get`, unknown and
//! missing keys are ignored, and any failure (missing directory, unreadable
//! file, malformed JSON) degrades to "no activity" instead of surfacing an
//! error. A tooltip hint must never break the launcher.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// How fresh a record must be for a non-null `openStep` to count as running
/// now. DSH checkpoints as it works, so a live conversation keeps rewriting
/// its record; a stale `openStep` is a leftover from a killed process.
const ACTIVE_WINDOW: Duration = Duration::from_secs(300);

/// Conversation counters for one DSH_HOME.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstanceActivity {
    /// Conversations with a step executing now.
    pub active: usize,
    /// Session records that could be parsed (active or not).
    pub total: usize,
}

impl InstanceActivity {
    /// Whether the counters carry any information at all. A HOME with no
    /// projection cache (a brand-new instance) reports nothing rather than a
    /// misleading `0`.
    pub fn is_known(&self) -> bool {
        self.total > 0
    }
}

/// Where the two layouts live, relative to a DSH_HOME.
fn per_record_dir(home: &Path) -> PathBuf {
    home.join("storages")
        .join("session_projcache")
        .join("sessions")
}

fn aggregate_file(home: &Path) -> PathBuf {
    home.join("storages").join("session_projcache.json")
}

/// Counts conversations for `home`. Blocking: call it from the blocking pool,
/// since a WSL home turns every read into a UNC round trip.
pub fn read_activity(home: &Path) -> InstanceActivity {
    let now = SystemTime::now();
    let mut activity = InstanceActivity::default();

    for path in per_record_files(home) {
        let Some(doc) = read_json(&path) else {
            continue;
        };
        // Each file wraps its row set in `record`.
        let record = doc.get("record").unwrap_or(&doc);
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        activity.total += 1;
        if record_is_active(record, mtime, now) {
            activity.active += 1;
        }
    }

    // The aggregate layout only exists on older versions; when it is present
    // alongside a per-record cache (a HOME that was upgraded) the per-record
    // files are the live ones, so the aggregate is only read when it is the
    // only source available.
    if activity.total == 0 {
        if let Some(aggregate) = read_json(&aggregate_file(home)) {
            let sessions = aggregate
                .get("tables")
                .and_then(|t| t.get("sessions"))
                .and_then(Value::as_object);
            if let Some(sessions) = sessions {
                for record in sessions.values() {
                    activity.total += 1;
                    if record_is_active(record, None, now) {
                        activity.active += 1;
                    }
                }
            }
        }
    }

    activity
}

/// Session record files of the per-record layout, sorted for deterministic
/// counting. A missing directory yields an empty list.
fn per_record_files(home: &Path) -> Vec<PathBuf> {
    let dir = per_record_dir(home);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .map(|e| e.eq_ignore_ascii_case("json"))
                    .unwrap_or(false)
        })
        .collect();
    files.sort();
    files
}

/// Reads and parses a JSON file, logging (never raising) failures.
fn read_json(path: &Path) -> Option<Value> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) => {
            crate::log_debug!("会话索引读取失败 {}: {e}", path.display());
            return None;
        }
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(value) => Some(value),
        Err(e) => {
            crate::log_debug!("会话索引解析失败 {}: {e}", path.display());
            None
        }
    }
}

/// The row set of one session record: `rows.sessionStats.val.openStep`
/// non-null AND the record is fresh.
fn record_is_active(record: &Value, file_mtime: Option<SystemTime>, now: SystemTime) -> bool {
    let rows = record.get("rows").unwrap_or(record);
    let has_open_step = rows
        .get("sessionStats")
        .and_then(|s| s.get("val"))
        .and_then(|v| v.get("openStep"))
        .map(|open| !open.is_null())
        .unwrap_or(false);
    if !has_open_step {
        return false;
    }
    // Freshness: a live conversation keeps rewriting its checkpoint, so either
    // the file mtime or the last prompt time lands in the window. The prompt
    // time alone is not enough for a long turn, and the mtime alone is not
    // available in the aggregate layout.
    if file_mtime.is_some_and(|mtime| within_window(mtime, now)) {
        return true;
    }
    last_prompt_at(rows).is_some_and(|ms| prompt_within_window(ms, now))
}

/// `rows.sessionListMetadata.val.lastPromptAt` (Unix milliseconds).
fn last_prompt_at(rows: &Value) -> Option<i64> {
    rows.get("sessionListMetadata")
        .and_then(|m| m.get("val"))
        .and_then(|v| v.get("lastPromptAt"))
        .and_then(Value::as_i64)
}

fn within_window(at: SystemTime, now: SystemTime) -> bool {
    match now.duration_since(at) {
        Ok(age) => age <= ACTIVE_WINDOW,
        // A timestamp in the future means clock skew, not staleness.
        Err(_) => true,
    }
}

fn prompt_within_window(ms: i64, now: SystemTime) -> bool {
    let now_ms = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let age = now_ms - ms;
    age >= 0 && (age as u64) <= ACTIVE_WINDOW.as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(tag: &str) -> PathBuf {
        let home =
            std::env::temp_dir().join(format!("dsh-sessions-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join("storages")).unwrap();
        home
    }

    /// A session record with the given `openStep` and prompt time.
    fn record(open_step: bool, last_prompt_at: Option<i64>) -> Value {
        serde_json::json!({
            "identity": { "createdAt": 1, "cwd": "C:\\work" },
            "rows": {
                "sessionStats": {
                    "ver": 1,
                    "seq": 7,
                    "val": {
                        "turns": 1,
                        "openStep": if open_step { serde_json::json!({ "turn": 1, "step": 2 }) } else { Value::Null },
                    }
                },
                "sessionListMetadata": {
                    "ver": 1,
                    "seq": 7,
                    "val": { "blank": false, "lastPromptAt": last_prompt_at }
                }
            }
        })
    }

    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    fn write_per_record(home: &Path, id: &str, rec: &Value) -> PathBuf {
        let dir = per_record_dir(home);
        std::fs::create_dir_all(&dir).unwrap();
        let doc = serde_json::json!({ "version": 7, "record": rec });
        let path = dir.join(format!("{id}.json"));
        std::fs::write(&path, doc.to_string()).unwrap();
        path
    }

    #[test]
    fn per_record_layout_counts_only_open_steps() {
        let home = temp_home("per-record");
        write_per_record(&home, "session-a", &record(true, Some(now_ms())));
        write_per_record(&home, "session-b", &record(false, Some(now_ms())));
        write_per_record(&home, "session-c", &record(true, Some(now_ms())));

        let activity = read_activity(&home);
        assert_eq!(activity.active, 2);
        assert_eq!(activity.total, 3);
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn aggregate_layout_is_read_when_no_per_record_cache_exists() {
        let home = temp_home("aggregate");
        let doc = serde_json::json!({
            "unit": { "name": "session_projcache", "version": 3 },
            "tables": {
                "sessions": {
                    "session-a": record(true, Some(now_ms())),
                    "session-b": record(false, None)
                }
            }
        });
        std::fs::write(aggregate_file(&home), doc.to_string()).unwrap();

        let activity = read_activity(&home);
        assert_eq!(activity.active, 1);
        assert_eq!(activity.total, 2);
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn per_record_layout_wins_over_a_stale_aggregate() {
        // A HOME upgraded from the aggregate layout keeps both files; the
        // per-record cache is the live one and must be the only source used.
        let home = temp_home("upgraded");
        write_per_record(&home, "session-a", &record(false, Some(now_ms())));
        let doc = serde_json::json!({
            "tables": { "sessions": { "session-old": record(true, Some(now_ms())) } }
        });
        std::fs::write(aggregate_file(&home), doc.to_string()).unwrap();

        let activity = read_activity(&home);
        assert_eq!(activity.total, 1);
        assert_eq!(activity.active, 0);
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn stale_open_step_is_not_active() {
        // A killed DSH leaves `openStep` set behind. Freshness is what tells it
        // apart from a live conversation, so a record whose file was written
        // long ago and whose last prompt is long past must not count.
        let home = temp_home("stale");
        let stale_ms = now_ms() - (ACTIVE_WINDOW.as_millis() as i64) - 60_000;
        let path = write_per_record(&home, "session-a", &record(true, Some(stale_ms)));
        let stale_at = SystemTime::now() - ACTIVE_WINDOW - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(stale_at)
            .unwrap();

        let activity = read_activity(&home);
        assert_eq!(activity.total, 1);
        assert_eq!(activity.active, 0);
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn a_recently_written_record_is_active_even_without_a_prompt_time() {
        // The per-record cache is rewritten as DSH works, so a fresh mtime
        // alone proves a step is running (the aggregate layout's records have
        // no mtime, which is why `lastPromptAt` is checked as well there).
        let home = temp_home("fresh-mtime");
        write_per_record(&home, "session-a", &record(true, None));

        let activity = read_activity(&home);
        assert_eq!(activity.total, 1);
        assert_eq!(activity.active, 1);
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn missing_directory_and_broken_json_degrade_to_zero() {
        let home = temp_home("broken");
        let activity = read_activity(&home);
        assert_eq!(activity, InstanceActivity::default());
        assert!(!activity.is_known());

        let dir = per_record_dir(&home);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("session-a.json"), "{ not json").unwrap();
        std::fs::write(dir.join("session-b.json"), "").unwrap();
        // Directory where a file is expected: must be skipped, not panic.
        std::fs::create_dir_all(dir.join("session-c.json")).unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();

        assert_eq!(read_activity(&home), InstanceActivity::default());
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn records_without_the_stats_row_are_counted_but_never_active() {
        let home = temp_home("shapeless");
        let dir = per_record_dir(&home);
        std::fs::create_dir_all(&dir).unwrap();
        let doc = serde_json::json!({ "version": 7, "record": { "rows": {} } });
        std::fs::write(dir.join("session-a.json"), doc.to_string()).unwrap();

        let activity = read_activity(&home);
        assert_eq!(activity.total, 1);
        assert_eq!(activity.active, 0);
        std::fs::remove_dir_all(&home).ok();
    }
}
