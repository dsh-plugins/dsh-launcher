//! Launcher runtime log.
//!
//! Writes timestamped, level-filtered lines to `<data_dir>/logs/latest.log`.
//! On startup the previous `latest.log` is rotated next to it as
//! `<yyyy>-<MM>-<dd>-<hh>-<mm>-<ss>-<ms>-<num>.log` (the timestamp is the old
//! file's mtime; `num` disambiguates same-millisecond collisions).

use chrono::{DateTime, Local};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// Parses a level name ("debug" | "info" | "warn" | "error"),
/// case-insensitive. Returns `None` for anything else.
pub fn parse_level(raw: &str) -> Option<Level> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "debug" => Some(Level::Debug),
        "info" => Some(Level::Info),
        "warn" => Some(Level::Warn),
        "error" => Some(Level::Error),
        _ => None,
    }
}

static LOG_FILE: Mutex<Option<File>> = Mutex::new(None);
static LOG_LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);

pub fn set_level(level: Level) {
    LOG_LEVEL.store(level as u8, Ordering::Relaxed);
}

/// Rotates any previous `latest.log` inside `log_dir`, then opens a fresh
/// one and applies the level. Safe to call once at startup.
pub fn init(log_dir: &Path, level: Level) -> Result<(), String> {
    std::fs::create_dir_all(log_dir).map_err(|e| format!("创建日志目录失败: {e}"))?;
    rotate_latest(log_dir)?;
    open_log(log_dir, level, "latest.log")
}

/// Dev-build variant (issue: `pnpm tauri-dev` must not disturb the installed
/// launcher): appends to `latest-dev.log` WITHOUT rotating — rotating
/// `latest.log` out from under the running production launcher would strand
/// its open file handle on the rotated file.
pub fn init_dev(log_dir: &Path, level: Level) -> Result<(), String> {
    std::fs::create_dir_all(log_dir).map_err(|e| format!("创建日志目录失败: {e}"))?;
    open_log(log_dir, level, "latest-dev.log")
}

fn open_log(log_dir: &Path, level: Level, name: &str) -> Result<(), String> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join(name))
        .map_err(|e| format!("打开 {name} 失败: {e}"))?;
    *LOG_FILE.lock().unwrap() = Some(file);
    set_level(level);
    Ok(())
}

fn rotate_latest(log_dir: &Path) -> Result<(), String> {
    let latest = log_dir.join("latest.log");
    if !latest.exists() {
        return Ok(());
    }
    let modified = std::fs::metadata(&latest)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::SystemTime::now());
    let dt: DateTime<Local> = modified.into();
    let base = format!(
        "{}-{:03}",
        dt.format("%Y-%m-%d-%H-%M-%S"),
        dt.timestamp_subsec_millis()
    );
    let Some(target) = pick_target(log_dir, &base) else {
        return Err("轮转日志失败：候选文件名均已存在".to_string());
    };
    std::fs::rename(&latest, &target).map_err(|e| format!("轮转日志失败 {}: {e}", target.display()))
}

/// Picks the first free `<base>-<num>.log` path (num starts at 1).
fn pick_target(log_dir: &Path, base: &str) -> Option<std::path::PathBuf> {
    for num in 1..1000u32 {
        let target = log_dir.join(format!("{base}-{num}.log"));
        if !target.exists() {
            return Some(target);
        }
    }
    None
}

/// Writes one line when `level` passes the configured filter. Always mirrors
/// to stderr so `tauri dev` sessions see the log without opening the file.
pub fn write(level: Level, msg: &str) {
    if (level as u8) < LOG_LEVEL.load(Ordering::Relaxed) {
        return;
    }
    let now = Local::now();
    let line = format!(
        "[{}.{:03}] [{}] {}",
        now.format("%Y-%m-%d %H:%M:%S"),
        now.timestamp_subsec_millis(),
        level.as_str(),
        msg
    );
    let mut guard = LOG_FILE.lock().unwrap();
    if let Some(f) = guard.as_mut() {
        let _ = writeln!(f, "{line}");
        let _ = f.flush();
    }
    drop(guard);
    eprintln!("{line}");
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => { $crate::applog::write($crate::applog::Level::Debug, &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::applog::write($crate::applog::Level::Info, &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::applog::write($crate::applog::Level::Warn, &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::applog::write($crate::applog::Level::Error, &format!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_level_accepts_known_names_case_insensitively() {
        assert_eq!(parse_level("debug"), Some(Level::Debug));
        assert_eq!(parse_level("INFO"), Some(Level::Info));
        assert_eq!(parse_level(" Warn "), Some(Level::Warn));
        assert_eq!(parse_level("error"), Some(Level::Error));
        assert_eq!(parse_level("trace"), None);
        assert_eq!(parse_level(""), None);
    }

    #[test]
    fn pick_target_skips_taken_sequence_numbers() {
        let dir = std::env::temp_dir().join(format!("dsh-log-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = "2026-08-27-10-22-14-123";
        assert_eq!(
            pick_target(&dir, base).unwrap().file_name().unwrap(),
            "2026-08-27-10-22-14-123-1.log"
        );
        std::fs::write(dir.join(format!("{base}-1.log")), "x").unwrap();
        std::fs::write(dir.join(format!("{base}-2.log")), "x").unwrap();
        assert_eq!(
            pick_target(&dir, base).unwrap().file_name().unwrap(),
            "2026-08-27-10-22-14-123-3.log"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rotate_latest_renames_to_timestamped_name() {
        let dir = std::env::temp_dir().join(format!("dsh-log-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("latest.log"), "first").unwrap();
        rotate_latest(&dir).unwrap();
        assert!(!dir.join("latest.log").exists());
        let rotated: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(rotated.len(), 1);
        assert!(
            rotated[0].ends_with("-1.log"),
            "unexpected rotated name: {}",
            rotated[0]
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
