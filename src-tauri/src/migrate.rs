//! Data-directory relocation support (issue #43).
//!
//! Resolution chain (highest priority first):
//!   1. `DSH_LAUNCHER_DATA_HOME` env var   (portable setups; never migrated)
//!   2. pointer file `<default>\data-home.txt`  (written by the settings UI)
//!   3. default app data dir (fallback)
//!
//! When the pointer file names a directory different from the effective one,
//! the launcher runs a one-shot migration at startup (before the log handle
//! is opened): copy -> verify -> switch -> snapshot the old dir. Any failure
//! rolls back to the previous directory and the launcher starts as before.

use crate::AppState;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, State};

pub const POINTER_FILE: &str = "data-home.txt";
pub const MIGRATION_FLAG: &str = "MIGRATION_IN_PROGRESS";
const ENV_DATA_HOME: &str = "DSH_LAUNCHER_DATA_HOME";

/// Where the effective data dir came from (mirrored to the frontend).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DataDirSource {
    /// `DSH_LAUNCHER_DATA_HOME` is set.
    Env,
    /// The pointer file is set and the data dir equals it.
    Pointer,
    /// Default app data dir (no env, no pointer file).
    Default,
}

/// Payload for `get_data_dir_source`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DataDirInfo {
    pub path: String,
    pub source: String,
    /// Non-empty when the resolved directory was unusable and the launcher
    /// fell back to the default one (issue requirement 3); the UI shows it
    /// as a toast.
    pub notice: Option<String>,
}

/// Result of the startup bootstrap: the effective data dir, where it came
/// from, and an optional fallback notice.
pub struct Bootstrap {
    pub data_dir: PathBuf,
    pub source: DataDirSource,
    pub notice: Option<String>,
}

/// Returns the default app data dir (home of the pointer file).
fn default_data_dir<R: tauri::Runtime>(app: &impl tauri::Manager<R>) -> tauri::Result<PathBuf> {
    app.path().app_data_dir()
}

/// Reads the pointer file if present and non-empty; empty otherwise.
fn read_pointer(default_dir: &Path) -> PathBuf {
    let pointer_path = default_dir.join(POINTER_FILE);
    match fs::read_to_string(&pointer_path) {
        Ok(content) => {
            let target = PathBuf::from(content.trim());
            if !target.as_os_str().is_empty() {
                target
            } else {
                PathBuf::new()
            }
        }
        Err(_) => PathBuf::new(),
    }
}

/// Windows-safe path comparison (case-insensitive), plain equality elsewhere.
pub fn paths_equal(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

/// Startup bootstrap: resolve the data dir, run a pending migration when the
/// pointer file names a different directory, and report a fallback notice.
///
/// Must run before `applog::init` (the log file handle would block moving
/// `logs/`) and before `runtime::ensure_local_node_on_path`.
pub fn bootstrap(app: &tauri::App) -> Bootstrap {
    let default_dir = match default_data_dir(app) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("dsh-launcher: 无法解析默认数据目录: {e}");
            return Bootstrap {
                data_dir: std::env::temp_dir().join("dsh-launcher"),
                source: DataDirSource::Default,
                notice: Some(format!("无法解析默认数据目录: {e}")),
            };
        }
    };

    // 1. Environment variable wins and never migrates.
    if let Some(env_dir) = std::env::var_os(ENV_DATA_HOME) {
        let dir = PathBuf::from(env_dir);
        if !dir.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(&dir) {
                eprintln!("dsh-launcher: 环境变量数据目录不可用: {e}");
                let _ = fs::create_dir_all(&default_dir);
                return Bootstrap {
                    data_dir: default_dir,
                    source: DataDirSource::Default,
                    notice: Some(format!("环境变量 DSH_LAUNCHER_DATA_HOME 指向的目录不可用: {e}")),
                };
            }
            return Bootstrap {
                data_dir: dir,
                source: DataDirSource::Env,
                notice: None,
            };
        }
    }

    // 2. Pointer file: either already migrated, needs migration, or stale.
    let pointer_target = read_pointer(&default_dir);
    if !pointer_target.as_os_str().is_empty() && !paths_equal(&pointer_target, &default_dir) {
        // Interrupted previous migration? Drop the half-finished copy.
        if pointer_target.join(MIGRATION_FLAG).exists() {
            remove_tree(&pointer_target);
        }
        // Already migrated on a previous launch (config.json present)?
        if pointer_target.join("config.json").exists() {
            let _ = fs::create_dir_all(&pointer_target);
            // The old default dir is left as the golden backup; snapshot it
            // (keeping the pointer file alive) for the 30-day recovery
            // window when it still holds data.
            snapshot_default_dir(&default_dir, &pointer_target);
            return Bootstrap {
                data_dir: pointer_target,
                source: DataDirSource::Pointer,
                notice: None,
            };
        }
        // Pending migration: copy current default dir into the target.
        let _ = fs::create_dir_all(&default_dir);
        match migrate_data_dir(&default_dir, &pointer_target) {
            Ok(()) => {
                snapshot_default_dir(&default_dir, &pointer_target);
                Bootstrap {
                    data_dir: pointer_target,
                    source: DataDirSource::Pointer,
                    notice: None,
                }
            }
            Err(e) => {
                eprintln!("dsh-launcher: 数据目录迁移失败,回退默认目录: {e}");
                remove_tree(&pointer_target);
                // Restore the pointer file (drop it) so next launch stays
                // on the default directory instead of retrying forever.
                let _ = fs::remove_file(default_dir.join(POINTER_FILE));
                let _ = fs::create_dir_all(&default_dir);
                bootstrap_notice(app, format!("数据目录迁移失败,已回退默认目录: {e}"));
                Bootstrap {
                    data_dir: default_dir,
                    source: DataDirSource::Default,
                    notice: Some(format!("数据目录迁移失败,已回退默认目录: {e}")),
                }
            }
        }
    } else {
        // 3. Default directory.
        let _ = fs::create_dir_all(&default_dir);
        Bootstrap {
            data_dir: default_dir,
            source: DataDirSource::Default,
            notice: None,
        }
    }
}

/// Snapshots the default (old) data dir as `<default>.old-<ts>` when it still
/// holds data, then restores the pointer file inside the fresh default dir
/// so the resolution chain keeps working. Best-effort.
fn snapshot_default_dir(default_dir: &Path, new_dir: &Path) {
    // Keep the pointer content before renaming the old dir away.
    let pointer_content = fs::read_to_string(default_dir.join(POINTER_FILE)).unwrap_or_default();
    if !dir_is_empty(default_dir).unwrap_or(false) {
        snapshot_old_dir(default_dir);
    }
    // The default dir now either does not exist or is an empty shell from a
    // previous snapshot; recreate it and restore the pointer file.
    let _ = fs::create_dir_all(default_dir);
    if !pointer_content.trim().is_empty() {
        let _ = fs::write(
            default_dir.join(POINTER_FILE),
            pointer_content.trim().to_string(),
        );
    }
    let _ = new_dir;
}

/// Recursively copies a directory tree with `std::fs` (no extra deps).
/// Fails on the first error; partial output is left in place (the caller
/// removes it on rollback).
pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if !from.exists() {
        return Ok(());
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if ty.is_dir() {
            copy_tree(&src, &dst)?;
        } else if ty.is_file() {
            fs::copy(&src, &dst)?;
        } else {
            // Symlink or special: copy the target content recursively for
            // dirs/junctions, or the file bytes for files. Most app data is
            // regular files, so this keeps migration safe on systems
            // without symlink privileges.
            if src.is_dir() {
                copy_tree(&src, &dst)?;
            } else {
                fs::copy(&src, &dst)?;
            }
        }
    }
    Ok(())
}

/// Removes a directory tree. Best-effort; used for rollback and cleanup.
pub fn remove_tree(dir: &Path) {
    if !dir.exists() {
        return;
    }
    let _ = fs::remove_dir_all(dir);
}

/// Counts files and total size under `dir` (recursive).
fn tree_stats(dir: &Path) -> std::io::Result<(u64, u64)> {
    let mut files = 0u64;
    let mut bytes = 0u64;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            let (f, b) = tree_stats(&entry.path())?;
            files += f;
            bytes += b;
        } else if ty.is_file() {
            files += 1;
            bytes += entry.metadata()?.len();
        }
    }
    Ok((files, bytes))
}

fn dir_is_empty(dir: &Path) -> std::io::Result<bool> {
    let mut entries = fs::read_dir(dir)?;
    Ok(entries.next().is_none())
}

/// Migrates the data dir from `from` to `to`. Returns an error message on
/// any failure; the caller decides whether to roll back.
pub fn migrate_data_dir(from: &Path, to: &Path) -> Result<(), String> {
    // 1. Refuse to migrate onto an existing populated target.
    if to.exists() && !dir_is_empty(to).unwrap_or(false) {
        return Err(format!("目标目录已有数据,拒绝迁移: {}", to.display()));
    }
    fs::create_dir_all(to).map_err(|e| format!("创建目标目录失败: {}", e))?;

    // 2. Write the in-progress marker (power-failure / kill safety).
    let flag_path = to.join(MIGRATION_FLAG);
    fs::write(&flag_path, from.to_string_lossy().as_bytes())
        .map_err(|e| format!("写入迁移标记失败: {}", e))?;

    // 3. Copy the known payloads.
    for name in [
        "config.json",
        "versions",
        "homes",
        "logs",
        ".pnpm-store",
        "tools",
        "icons",
        "bin",
    ] {
        let src = from.join(name);
        let dst = to.join(name);
        if let Err(e) = copy_tree(&src, &dst) {
            let _ = fs::remove_dir_all(to);
            return Err(format!("复制 {name} 失败: {e}"));
        }
    }

    // 4. Verify: file count + total size + config parse.
    let (src_files, src_bytes) = tree_stats(from).map_err(|e| format!("统计源目录失败: {e}"))?;
    let (dst_files, dst_bytes) = tree_stats(to).map_err(|e| format!("统计目标目录失败: {e}"))?;
    if dst_files < src_files || dst_bytes < src_bytes {
        let _ = fs::remove_dir_all(to);
        return Err(format!(
            "迁移校验失败: 文件数 {dst_files}/{src_files}, 大小 {dst_bytes}/{src_bytes}"
        ));
    }
    let new_config = to.join("config.json");
    if new_config.exists() {
        let _cfg = crate::config::load_config(&new_config);
    }

    // 5. Commit: drop the marker. The pointer file is updated by the caller
    //    after a successful switch.
    let _ = fs::remove_file(&flag_path);
    Ok(())
}

/// Snapshots the old data dir as `<old>.old-<ts>` instead of deleting it
/// (30-day grace window for manual recovery). Best-effort: failure to rename
/// does not fail the migration.
pub fn snapshot_old_dir(old: &Path) {
    let ts = chrono::Utc::now().format("%Y%m%d%H%M%S").to_string();
    let snap = PathBuf::from(format!("{}.old-{}", old.to_string_lossy(), ts));
    if let Err(e) = fs::rename(old, &snap) {
        eprintln!("dsh-launcher: 旧数据目录快照失败: {e}");
    }
}

/// Lists `.old-<ts>` snapshots next to the current data dir (for the
/// "restore previous data dir" entry point; UI wiring is a follow-up).
#[allow(dead_code)]
pub fn list_snapshots(data_dir: &Path) -> Vec<PathBuf> {
    let parent = data_dir.parent().unwrap_or(Path::new("."));
    let name = data_dir.file_name().map(|s| s.to_string_lossy().to_string());
    let Ok(entries) = fs::read_dir(parent) else {
        return vec![];
    };
    let mut out = vec![];
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        if let Some(n) = p.file_name().map(|s| s.to_string_lossy().to_string()) {
            if let Some(base) = name.as_deref() {
                if n.starts_with(&format!("{base}.old-")) {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

/// Emits a fallback notice event so an already-running frontend can show it.
fn bootstrap_notice(app: &tauri::App, message: String) {
    use tauri::Emitter;
    let _ = app.emit("data-dir-fallback", message.clone());
    crate::log_warn!("{message}");
}

// ---------------------------------------------------------------------------
// Commands (registered in lib.rs)
// ---------------------------------------------------------------------------

/// Opens a folder picker; returns the picked absolute path, or an empty
/// string when the user cancels.
#[tauri::command]
pub async fn pick_data_dir(app: AppHandle) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app
        .dialog()
        .file()
        .blocking_pick_folder()
        .map(|p| p.to_string())
        .unwrap_or_default();
    Ok(picked)
}

/// Validates the chosen directory and writes the pointer file (pending).
/// No files are moved here; the migration happens on next launch.
#[tauri::command]
pub fn commit_data_dir(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<String, String> {
    let target = PathBuf::from(path.trim());
    if target.as_os_str().is_empty() {
        return Err("目录不能为空".to_string());
    }
    let current = state.data_dir.clone();
    if paths_equal(&target, &current) {
        return Err("新目录与当前数据目录相同".to_string());
    }
    if !target.is_dir() {
        return Err(format!("目录不存在: {}", target.display()));
    }
    if !dir_is_empty(&target).unwrap_or(false) {
        return Err(format!(
            "目标目录已有内容,请选择空目录: {}",
            target.display()
        ));
    }
    // The target must be writable (not read-only).
    if target
        .metadata()
        .map(|m| m.permissions().readonly())
        .unwrap_or(true)
    {
        return Err(format!("目标目录不可写: {}", target.display()));
    }

    let default_dir = default_data_dir(&app).map_err(|e| e.to_string())?;
    fs::create_dir_all(&default_dir).map_err(|e| format!("创建默认目录失败: {e}"))?;
    let pointer_path = default_dir.join(POINTER_FILE);
    let mut f = fs::File::create(&pointer_path).map_err(|e| format!("写入指针文件失败: {e}"))?;
    f.write_all(target.to_string_lossy().as_bytes())
        .map_err(|e| format!("写入指针文件失败: {e}"))?;
    f.flush().map_err(|e| format!("写入指针文件失败: {e}"))?;

    Ok(target.to_string_lossy().to_string())
}

/// Returns the effective data dir, its source, and an optional fallback
/// notice (issue requirement 3 surfaced to the UI).
#[tauri::command]
pub fn get_data_dir_source(state: State<'_, AppState>) -> Result<DataDirInfo, String> {
    let path = state.data_dir.to_string_lossy().to_string();
    let source = match &state.data_dir_source {
        DataDirSource::Env => "env".to_string(),
        DataDirSource::Pointer => "pointer".to_string(),
        DataDirSource::Default => "default".to_string(),
    };
    let notice = state.data_dir_notice.lock().unwrap().clone();
    Ok(DataDirInfo { path, source, notice })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_equal_is_case_insensitive_on_windows() {
        let a = Path::new("C:\\Users\\x\\DATA");
        let b = Path::new("c:\\users\\X\\data");
        #[cfg(windows)]
        assert!(paths_equal(a, b));
        #[cfg(not(windows))]
        assert!(!paths_equal(a, b));
    }

    #[test]
    fn dir_is_empty_detects_contents() {
        let tmp = std::env::temp_dir().join(format!("dsh-migrate-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(dir_is_empty(&tmp).unwrap());
        std::fs::write(tmp.join("a.txt"), "x").unwrap();
        assert!(!dir_is_empty(&tmp).unwrap());
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn copy_tree_copies_recursively() {
        let tmp = std::env::temp_dir().join(format!("dsh-copy-test-{}", uuid::Uuid::new_v4()));
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("sub/deep")).unwrap();
        std::fs::write(src.join("root.txt"), "root").unwrap();
        std::fs::write(src.join("sub/a.txt"), "a").unwrap();
        std::fs::write(src.join("sub/deep/b.txt"), "b").unwrap();
        let dst = tmp.join("dst");
        copy_tree(&src, &dst).unwrap();
        assert!(dst.join("root.txt").exists());
        assert!(dst.join("sub/deep/b.txt").exists());
        assert_eq!(std::fs::read_to_string(dst.join("sub/a.txt")).unwrap(), "a");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn tree_stats_counts_files_and_bytes() {
        let tmp = std::env::temp_dir().join(format!("dsh-stats-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(tmp.join("d")).unwrap();
        std::fs::write(tmp.join("d/a.txt"), "ab").unwrap();
        std::fs::write(tmp.join("b.txt"), "abc").unwrap();
        let (files, bytes) = tree_stats(&tmp).unwrap();
        assert_eq!(files, 2);
        assert_eq!(bytes, 5);
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}