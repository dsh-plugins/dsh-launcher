// DSH_HOME storage redirection (issue #51): replace a whitelisted entry of a
// HOME with a link to an external location, so bulky data (sessions,
// attachments, …) can live on another disk without moving the whole HOME.

use crate::config::DshHome;
use crate::AppState;
use std::path::{Path, PathBuf};
use tauri::State;

/// Whitelisted redirectable entries: name → whether it is a directory.
/// Only the HOME-root global layer is redirectable; per-profile config
/// (profiles/<name>/…) is intentionally not covered.
const REDIRECTABLE: &[(&str, bool)] = &[
    ("sessions", true),
    ("skills", true),
    ("attachments", true),
    ("storages", true),
    ("settings.yaml", false),
    (".credentials.yaml", false),
    ("cordis.patch.yml", false),
];

fn entry_kind(entry: &str) -> Option<bool> {
    REDIRECTABLE
        .iter()
        .find(|(name, _)| *name == entry)
        .map(|(_, is_dir)| *is_dir)
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct HomeLinkInfo {
    pub entry: String,
    pub is_dir: bool,
    /// Configured target (empty = not redirected).
    pub target: String,
    /// The entry currently IS a link resolving to `target`.
    pub active: bool,
}

fn home_of(state: &AppState, home_id: &str) -> Result<DshHome, String> {
    let cfg = state.config.lock().unwrap();
    cfg.homes
        .iter()
        .find(|h| h.id == home_id)
        .cloned()
        .ok_or_else(|| "DSH_HOME 不存在".to_string())
}

/// Rejects changes while any instance using this HOME is running: swapping a
/// live entry out from under a running DSH would corrupt its state.
async fn ensure_no_running_instance(state: &AppState, home_id: &str) -> Result<(), String> {
    let ids: Vec<String> = {
        let cfg = state.config.lock().unwrap();
        cfg.instances
            .iter()
            .filter(|i| i.home_id == home_id)
            .map(|i| i.id.clone())
            .collect()
    };
    let running = state.running.lock().await;
    if ids.iter().any(|id| running.contains_key(id)) {
        return Err("该 DSH_HOME 下有实例正在运行，请先停止后再修改存储重定向".to_string());
    }
    Ok(())
}

fn save_locked(state: &State<'_, AppState>, cfg: &crate::config::Config) -> Result<(), String> {
    crate::config::save_config(&state.config_path, cfg)
}

/// True when `path` is a link (reparse point / symlink) resolving to `target`.
fn link_points_to(path: &Path, target: &Path) -> bool {
    if !crate::commands::entry_is_dir_link(path) {
        return false;
    }
    let Ok(resolved) = std::fs::canonicalize(path) else {
        return false;
    };
    let Ok(want) = std::fs::canonicalize(target) else {
        return false;
    };
    crate::config::paths_equal(&resolved, &want)
}

/// Creates a file link (symlink, hard-link fallback on Windows where file
/// symlinks need privileges). NOTE: a hard-linked file is broken by writers
/// that replace the file atomically — callers must surface this caveat.
fn create_file_link(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_file(target, link).is_ok() {
            Ok(())
        } else {
            // Fallback: hard link (same volume only).
            std::fs::hard_link(target, link)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(std::io::Error::other("该平台不支持文件链接"))
    }
}

/// Removes a link entry (junction / symlink) without touching its target.
fn remove_link(path: &Path, is_dir: bool) -> std::io::Result<()> {
    if is_dir {
        // remove_dir on a junction removes the link, not the target tree.
        std::fs::remove_dir(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[tauri::command]
pub async fn list_home_links(
    state: State<'_, AppState>,
    home_id: String,
) -> Result<Vec<HomeLinkInfo>, String> {
    let home = home_of(&state, &home_id)?;
    if home.wsl.is_some() {
        return Err("WSL 实例的 DSH_HOME 暂不支持存储重定向".to_string());
    }
    let mut out = Vec::new();
    for (entry, is_dir) in REDIRECTABLE {
        let target = home.links.get(*entry).cloned().unwrap_or_default();
        let active =
            !target.is_empty() && link_points_to(&home.path.join(entry), Path::new(&target));
        out.push(HomeLinkInfo {
            entry: entry.to_string(),
            is_dir: *is_dir,
            target,
            active,
        });
    }
    Ok(out)
}

#[tauri::command]
pub async fn set_home_link(
    state: State<'_, AppState>,
    home_id: String,
    entry: String,
    target: String,
) -> Result<(), String> {
    let Some(is_dir) = entry_kind(&entry) else {
        return Err(format!("不支持重定向的条目: {entry}"));
    };
    let home = home_of(&state, &home_id)?;
    if home.wsl.is_some() {
        return Err("WSL 实例的 DSH_HOME 暂不支持存储重定向".to_string());
    }
    ensure_no_running_instance(&state, &home_id).await?;

    let target_path = PathBuf::from(target.trim());
    // The target must exist with the right shape before we swap.
    if is_dir {
        std::fs::create_dir_all(&target_path).map_err(|e| format!("创建目标目录失败: {e}"))?;
    } else if !target_path.is_file() {
        return Err("目标文件不存在".to_string());
    }
    // Redirecting an entry onto itself is meaningless.
    let link_path = home.path.join(&entry);
    if let (Ok(a), Some(Ok(parent))) = (
        std::fs::canonicalize(&home.path),
        target_path.parent().map(std::fs::canonicalize),
    ) {
        if crate::config::paths_equal(&a, &parent)
            && target_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                == Some(entry.clone())
        {
            return Err("目标不能就是 HOME 内的原条目".to_string());
        }
    }
    // Never let a redirected tree contain its own link (recursion trap).
    if let (Ok(t), Ok(h)) = (
        std::fs::canonicalize(&target_path),
        std::fs::canonicalize(&home.path),
    ) {
        if crate::config::paths_equal(&t, &h) || t.starts_with(&h) {
            return Err("目标不能位于该 DSH_HOME 内部".to_string());
        }
    }

    // Swap the existing entry out of the way.
    if crate::commands::entry_is_dir_link(&link_path)
        || link_path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
    {
        if !link_points_to(&link_path, &target_path) {
            remove_link(&link_path, is_dir).map_err(|e| format!("移除旧链接失败: {e}"))?;
        } else {
            // Already pointing at this target: just record it.
            return record_link(&state, &home_id, &entry, &target_path);
        }
    } else if link_path.exists() {
        if is_dir {
            // Move existing content into the (fresh) target when possible,
            // otherwise keep it as a timestamped backup.
            let target_empty = std::fs::read_dir(&target_path)
                .map(|mut it| it.next().is_none())
                .unwrap_or(false);
            if target_empty {
                std::fs::rename(&link_path, &target_path)
                    .map_err(|e| format!("迁移现有目录到目标失败: {e}"))?;
            } else {
                let bak = home.path.join(format!(
                    "{entry}.bak-{}",
                    chrono::Local::now().format("%Y%m%d%H%M%S")
                ));
                std::fs::rename(&link_path, &bak).map_err(|e| format!("备份现有目录失败: {e}"))?;
                crate::log_warn!("存储重定向：现有 {entry} 已备份为 {}", bak.display());
            }
        } else {
            let bak = home.path.join(format!(
                "{entry}.bak-{}",
                chrono::Local::now().format("%Y%m%d%H%M%S")
            ));
            std::fs::rename(&link_path, &bak).map_err(|e| format!("备份现有文件失败: {e}"))?;
            crate::log_warn!("存储重定向：现有 {entry} 已备份为 {}", bak.display());
        }
    }

    if is_dir {
        crate::commands::create_dir_link(&target_path, &link_path)
            .map_err(|e| format!("创建目录链接失败: {e}"))?;
    } else {
        create_file_link(&target_path, &link_path).map_err(|e| format!("创建文件链接失败: {e}"))?;
    }
    record_link(&state, &home_id, &entry, &target_path)
}

fn record_link(
    state: &State<'_, AppState>,
    home_id: &str,
    entry: &str,
    target: &Path,
) -> Result<(), String> {
    let mut cfg = state.config.lock().unwrap();
    let home = cfg
        .homes
        .iter_mut()
        .find(|h| h.id == home_id)
        .ok_or_else(|| "DSH_HOME 不存在".to_string())?;
    home.links
        .insert(entry.to_string(), target.to_string_lossy().to_string());
    save_locked(state, &cfg)?;
    Ok(())
}

#[tauri::command]
pub async fn clear_home_link(
    state: State<'_, AppState>,
    home_id: String,
    entry: String,
) -> Result<(), String> {
    let Some(is_dir) = entry_kind(&entry) else {
        return Err(format!("不支持重定向的条目: {entry}"));
    };
    let home = home_of(&state, &home_id)?;
    ensure_no_running_instance(&state, &home_id).await?;

    let link_path = home.path.join(&entry);
    if crate::commands::entry_is_dir_link(&link_path)
        || link_path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
    {
        // Only the link is removed; the data stays at the target.
        remove_link(&link_path, is_dir).map_err(|e| format!("移除链接失败: {e}"))?;
    }
    let mut cfg = state.config.lock().unwrap();
    if let Some(h) = cfg.homes.iter_mut().find(|h| h.id == home_id) {
        h.links.remove(&entry);
    }
    save_locked(&state, &cfg)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn unique_temp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dsh-links-test-{tag}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn whitelist_rejects_unknown_entries() {
        assert_eq!(entry_kind("sessions"), Some(true));
        assert_eq!(entry_kind("settings.yaml"), Some(false));
        assert_eq!(entry_kind("profiles"), None);
        assert_eq!(entry_kind("node_modules"), None);
    }

    #[test]
    fn link_points_to_verifies_junction_target() {
        let root = unique_temp("verify");
        let target = root.join("target");
        std::fs::create_dir_all(&target).unwrap();
        let link = root.join("link");
        if crate::commands::create_dir_link(&target, &link).is_err() {
            eprintln!("skipping: cannot create directory links on this platform");
            return;
        }
        assert!(link_points_to(&link, &target));
        assert!(!link_points_to(&link, &root));
        // A real directory is not a link.
        assert!(!link_points_to(&target, &target));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn remove_link_keeps_target_content() {
        let root = unique_temp("remove");
        let target = root.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("data.txt"), "keep me").unwrap();
        let link = root.join("link");
        if crate::commands::create_dir_link(&target, &link).is_err() {
            eprintln!("skipping: cannot create directory links on this platform");
            return;
        }
        remove_link(&link, true).unwrap();
        assert!(!link.exists());
        assert_eq!(
            std::fs::read_to_string(target.join("data.txt")).unwrap(),
            "keep me"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn links_map_serializes_empty_by_default() {
        // Old configs (no `links` key) must load, and empty maps stay out of
        // the serialized file.
        let json = r#"{"id":"h1","name":"h","path":"C:/x"}"#;
        let home: DshHome = serde_json::from_str(json).unwrap();
        assert!(home.links.is_empty());
        let mut with = home.clone();
        with.links = BTreeMap::from([("sessions".to_string(), "D:/data/sessions".to_string())]);
        let s = serde_json::to_string(&with).unwrap();
        assert!(s.contains("sessions"));
    }
}
