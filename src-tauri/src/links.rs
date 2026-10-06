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
    ("AGENTS.md", false),
    ("settings.yaml", false),
    (".credentials.yaml", false),
    ("cordis.patch.yml", false),
];

/// Issue #95: the AGENTS.md entry may be redirected to a file that does not
/// exist yet (dotfiles-repo workflow — the file appears on the next clone).
/// DSH treats a missing AGENTS.md as absent, so a dangling link is safe.
const AGENTS_MD: &str = "AGENTS.md";

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

/// One candidate **root directory** offered as a preset for a redirection
/// target (issue #65). `path` is the root only: the frontend joins the entry
/// name onto it, so a single suggestion set serves every entry. The candidate
/// set is advisory — the user can always type a path by hand.
#[derive(Clone, Debug, serde::Serialize)]
pub struct HomeLinkSuggestion {
    /// Stable id: `launcher-data` | `same-drive` | `last-used`.
    pub id: String,
    /// Frontend i18n key for the label. The backend never emits prose, so the
    /// UI stays translated without a backend language switch.
    pub label_key: String,
    /// Candidate root directory, absolute.
    pub path: String,
    /// Whether the root exists **right now**. Purely a read-only probe: this
    /// command never creates a directory and never writes configuration.
    pub exists: bool,
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
    if let (Ok(resolved), Ok(want)) = (std::fs::canonicalize(path), std::fs::canonicalize(target)) {
        return crate::config::paths_equal(&resolved, &want);
    }
    // Issue #95: canonicalize follows the link, so it fails while the target
    // is still missing (AGENTS.md before the first dotfiles clone). Compare
    // the raw link target instead — links are created from absolute paths, so
    // the stored value is the config target string itself.
    let Ok(raw) = std::fs::read_link(path) else {
        return false;
    };
    let raw = if raw.is_absolute() {
        raw
    } else {
        match path.parent() {
            Some(parent) => parent.join(raw),
            None => return false,
        }
    };
    crate::config::paths_equal(&strip_verbatim(&raw), &strip_verbatim(target))
}

/// Drops the Windows `\\?\` (and `\\?\UNC\`) verbatim prefix so a
/// canonicalized path compares equal to the hand-written config value.
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match s.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => p.to_path_buf(),
    }
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
        match std::os::windows::fs::symlink_file(target, link) {
            Ok(()) => Ok(()),
            Err(err) => {
                if !target.exists() {
                    // Issue #95: a hard link cannot anchor to a file that does
                    // not exist yet, so the dangling symlink is the only
                    // option — surface its error directly.
                    crate::log_warn!("文件链接：目标尚不存在且符号链接创建失败: {err}");
                    Err(err)
                } else {
                    crate::log_warn!(
                        "文件符号链接创建失败，退化为硬链接（DSH 原子替换写入可能使其失效）: {err}"
                    );
                    // Fallback: hard link (same volume only).
                    std::fs::hard_link(target, link)
                }
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(std::io::Error::other("该平台不支持文件链接"))
    }
}

/// Removes a link entry (junction / symlink) without touching its target.
fn remove_link(path: &Path, is_dir: bool) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        // unlink(2) removes the symlink itself regardless of the target's
        // type; remove_dir would fail with "not a directory".
        let _ = is_dir;
        std::fs::remove_file(path)
    }
    #[cfg(windows)]
    {
        if is_dir {
            // remove_dir on a junction removes the link, not the target tree.
            std::fs::remove_dir(path)
        } else {
            std::fs::remove_file(path)
        }
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
    let home_path = home.path.clone();
    let links = home.links.clone();
    // Symlink/canonicalize probes are blocking fs calls: keep them off the
    // async executor like every other launcher fs walk (wsl::run_blocking).
    crate::wsl::run_blocking(move || {
        let mut out = Vec::new();
        for (entry, is_dir) in REDIRECTABLE {
            let target = links.get(*entry).cloned().unwrap_or_default();
            let active =
                !target.is_empty() && link_points_to(&home_path.join(entry), Path::new(&target));
            out.push(HomeLinkInfo {
                entry: entry.to_string(),
                is_dir: *is_dir,
                target,
                active,
            });
        }
        out
    })
    .await
}

/// Validates a redirection target against the HOME. The caller has already
/// created the target (directories) or its parent directory (AGENTS.md), so
/// canonicalization has something concrete to work with.
fn check_target(home: &Path, entry: &str, target_path: &Path, is_dir: bool) -> Result<(), String> {
    // The target must exist with the right shape before we swap — except
    // AGENTS.md (issue #95), which may legitimately not exist yet.
    if is_dir {
        if target_path.exists() && !target_path.is_dir() {
            return Err("目标已存在且不是目录".to_string());
        }
    } else if !target_path.is_file() && entry != AGENTS_MD {
        return Err("目标文件不存在".to_string());
    }
    // Redirecting an entry onto itself is meaningless.
    if let (Ok(a), Some(Ok(parent))) = (
        std::fs::canonicalize(home),
        target_path.parent().map(std::fs::canonicalize),
    ) {
        if crate::config::paths_equal(&a, &parent)
            && target_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                == Some(entry.to_string())
        {
            return Err("目标不能就是 HOME 内的原条目".to_string());
        }
    }
    // Never let a redirected tree contain its own link (recursion trap). A
    // missing AGENTS.md target cannot be canonicalized, so fall back to its
    // (existing) parent directory plus the file name.
    if let Ok(h) = std::fs::canonicalize(home) {
        let canon = match std::fs::canonicalize(target_path) {
            Ok(t) => Some(t),
            Err(_) => match (
                target_path.parent().map(std::fs::canonicalize),
                target_path.file_name(),
            ) {
                (Some(Ok(p)), Some(name)) => Some(p.join(name)),
                _ => None,
            },
        };
        if let Some(t) = canon {
            if crate::config::paths_equal(&t, &h) || t.starts_with(&h) {
                return Err("目标不能位于该 DSH_HOME 内部".to_string());
            }
        }
    }
    Ok(())
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
    let home_path = home.path.clone();
    let entry_fs = entry.clone();
    let target_fs = target_path.clone();
    // Validation, the swap and link creation are all blocking fs work; run
    // them on a blocking thread (wsl::run_blocking) and only leave the config
    // update below on the async executor.
    crate::wsl::run_blocking(move || set_home_link_fs(&home_path, &entry_fs, &target_fs, is_dir))
        .await??;
    record_link(&state, &home_id, &entry, &target_path)
}

/// The blocking filesystem half of `set_home_link`: materializes the anchor
/// the link needs, validates the target, swaps any existing entry out of the
/// way, and creates the replacement link. Config persistence is the caller's
/// job.
fn set_home_link_fs(
    home_path: &Path,
    entry: &str,
    target_path: &Path,
    is_dir: bool,
) -> Result<(), String> {
    // Materialize the anchor the link needs before validation runs: directory
    // targets are created outright; a missing AGENTS.md gets its parent
    // directory so the link has a stable location to hang on.
    if is_dir {
        std::fs::create_dir_all(target_path).map_err(|e| format!("创建目标目录失败: {e}"))?;
    } else if entry == AGENTS_MD && !target_path.exists() {
        let parent = target_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| "目标路径缺少父目录".to_string())?;
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目标目录失败: {e}"))?;
    }
    check_target(home_path, entry, target_path, is_dir)?;

    // Swap the existing entry out of the way.
    let link_path = home_path.join(entry);
    if crate::commands::entry_is_dir_link(&link_path)
        || link_path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
    {
        if !link_points_to(&link_path, target_path) {
            remove_link(&link_path, is_dir).map_err(|e| format!("移除旧链接失败: {e}"))?;
        } else {
            // Already pointing at this target: nothing to swap or re-create.
            return Ok(());
        }
    } else if link_path.exists() {
        if is_dir {
            // Move existing content into the (fresh) target when possible,
            // otherwise keep it as a timestamped backup.
            let target_empty = std::fs::read_dir(target_path)
                .map(|mut it| it.next().is_none())
                .unwrap_or(false);
            if target_empty {
                std::fs::rename(&link_path, target_path)
                    .map_err(|e| format!("迁移现有目录到目标失败: {e}"))?;
            } else {
                let bak = home_path.join(format!(
                    "{entry}.bak-{}",
                    chrono::Local::now().format("%Y%m%d%H%M%S")
                ));
                std::fs::rename(&link_path, &bak).map_err(|e| format!("备份现有目录失败: {e}"))?;
                crate::log_warn!("存储重定向：现有 {entry} 已备份为 {}", bak.display());
            }
        } else {
            let bak = home_path.join(format!(
                "{entry}.bak-{}",
                chrono::Local::now().format("%Y%m%d%H%M%S")
            ));
            std::fs::rename(&link_path, &bak).map_err(|e| format!("备份现有文件失败: {e}"))?;
            crate::log_warn!("存储重定向：现有 {entry} 已备份为 {}", bak.display());
        }
    }

    if is_dir {
        crate::commands::create_dir_link(target_path, &link_path)
            .map_err(|e| format!("创建目录链接失败: {e}"))?;
    } else {
        create_file_link(target_path, &link_path).map_err(|e| format!("创建文件链接失败: {e}"))?;
    }
    Ok(())
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
    // Remember the directory the user redirected into (issue #65 D2) so the
    // next dialog can offer it as a preset root. Advisory only: a bad value
    // costs one useless dropdown entry, never a failed write.
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            cfg.settings.last_link_root = Some(parent.to_string_lossy().to_string());
        }
    }
    save_locked(state, &cfg)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Redirection target presets (issue #65)
// ---------------------------------------------------------------------------

/// Candidate root directories presented as presets for a redirection target.
///
/// Purely advisory and **read-only**: no directory is created, no config is
/// written, and no writability probe is attempted (a temp-file probe would
/// litter the user's directories for no real gain — the backend validation in
/// `set_home_link` stays the only authority).
fn suggest_targets(
    data_dir: &Path,
    home_path: &Path,
    last_used: Option<&str>,
) -> Vec<HomeLinkSuggestion> {
    let mut out: Vec<HomeLinkSuggestion> = Vec::new();
    let mut push = |id: &str, label_key: &str, path: PathBuf| {
        // Same root reached two ways (e.g. last-used equals the launcher
        // default) must appear once.
        if out
            .iter()
            .any(|s| crate::config::paths_equal(Path::new(&s.path), &path))
        {
            return;
        }
        out.push(HomeLinkSuggestion {
            id: id.to_string(),
            label_key: label_key.to_string(),
            exists: path.exists(),
            path: path.to_string_lossy().to_string(),
        });
    };

    push(
        "launcher-data",
        "storagePresetLauncherData",
        data_dir.join("dsh-data"),
    );
    if let Some(root) = same_volume_root(home_path) {
        push("same-drive", "storagePresetSameDrive", root);
    }
    if let Some(last) = last_used.map(str::trim).filter(|s| !s.is_empty()) {
        push("last-used", "storagePresetLastUsed", PathBuf::from(last));
    }
    out
}

/// A `<volume>:\dsh-data` root on the DSH_HOME's own volume, or
/// `<home parent>/dsh-data` where there is no drive letter (unix, UNC, …).
/// Returns `None` when no sensible location exists, rather than proposing a
/// path inside the HOME — which `set_home_link` would reject outright.
fn same_volume_root(home_path: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if let Some(Component::Prefix(prefix)) = home_path.components().next() {
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) {
                // "D:" + "\" → "D:\", then join onto it.
                let root = PathBuf::from(format!("{}\\", prefix.as_os_str().to_string_lossy()));
                return Some(root.join("dsh-data"));
            }
        }
    }
    let parent = home_path.parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    Some(parent.join("dsh-data"))
}

/// Suggests preset root directories for a redirection target (issue #65).
/// Advisory only: the user may ignore every candidate and type a path.
#[tauri::command]
pub async fn suggest_home_link_targets(
    state: State<'_, AppState>,
    home_id: String,
    entry: String,
) -> Result<Vec<HomeLinkSuggestion>, String> {
    if entry_kind(&entry).is_none() {
        return Err(format!("不支持重定向的条目: {entry}"));
    }
    let home = home_of(&state, &home_id)?;
    // Same capability boundary as list/set/clear: WSL HOMEs are refused.
    if home.wsl.is_some() {
        return Err("WSL 实例的 DSH_HOME 暂不支持存储重定向".to_string());
    }
    let data_dir = state.data_dir.clone();
    let last_used = state.config.lock().unwrap().settings.last_link_root.clone();
    Ok(suggest_targets(&data_dir, &home.path, last_used.as_deref()))
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
        assert_eq!(entry_kind("AGENTS.md"), Some(false));
        assert_eq!(entry_kind("profiles"), None);
        assert_eq!(entry_kind("node_modules"), None);
    }

    #[test]
    fn check_target_rejects_missing_target_for_other_files() {
        let root = unique_temp("check-missing");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let target = root.join("missing").join("settings.yaml");
        let err = check_target(&home, "settings.yaml", &target, false).unwrap_err();
        assert!(err.contains("目标文件不存在"), "unexpected: {err}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn check_target_allows_missing_agents_md_target() {
        // Issue #95: redirecting AGENTS.md into a dotfiles repo before the
        // file exists must pass validation once the parent directory is there.
        let root = unique_temp("check-agents");
        let home = root.join("home");
        let repo = root.join("dotfiles");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        let target = repo.join("AGENTS.md");
        check_target(&home, AGENTS_MD, &target, false).unwrap();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn check_target_rejects_agents_md_inside_home_via_parent_fallback() {
        // The file does not exist, so plain canonicalize cannot catch this;
        // the parent fallback must.
        let root = unique_temp("check-inside");
        let home = root.join("home");
        let sub = home.join("notes");
        std::fs::create_dir_all(&sub).unwrap();
        let target = sub.join("AGENTS.md");
        let err = check_target(&home, AGENTS_MD, &target, false).unwrap_err();
        assert!(
            err.contains("目标不能位于该 DSH_HOME 内部"),
            "unexpected: {err}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn check_target_rejects_redirecting_entry_onto_itself() {
        let root = unique_temp("check-self");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        // HOME/AGENTS.md as the target of AGENTS.md — the file may not even
        // exist yet; the parent comparison must still catch it.
        let target = home.join("AGENTS.md");
        let err = check_target(&home, AGENTS_MD, &target, false).unwrap_err();
        assert!(
            err.contains("目标不能就是 HOME 内的原条目"),
            "unexpected: {err}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn link_points_to_recognizes_dangling_file_symlink() {
        // Issue #95: an AGENTS.md link whose target does not exist yet must
        // still show as active; the read_link fallback verifies it.
        let root = unique_temp("dangling");
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("dotfiles").join("AGENTS.md"); // never created
        let link = root.join("AGENTS.md");
        if create_file_link(&target, &link).is_err() {
            eprintln!("skipping: cannot create file symlinks on this platform");
            std::fs::remove_dir_all(&root).ok();
            return;
        }
        assert!(link_points_to(&link, &target));
        assert!(!link_points_to(&link, &root.join("other.md")));
        std::fs::remove_dir_all(&root).ok();
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

    #[test]
    fn suggest_targets_lists_launcher_data_first_and_never_touches_disk() {
        let data_dir = unique_temp("suggest-data");
        let home = unique_temp("suggest-home");
        let out = suggest_targets(&data_dir, &home, None);
        assert_eq!(out[0].id, "launcher-data");
        assert_eq!(out[0].label_key, "storagePresetLauncherData");
        assert_eq!(PathBuf::from(&out[0].path), data_dir.join("dsh-data"));
        assert!(!out[0].exists);
        // Read-only contract: the probe must not materialize anything.
        assert!(!data_dir.exists());
        assert!(!data_dir.join("dsh-data").exists());
    }

    #[test]
    fn suggest_targets_marks_existing_roots() {
        let data_dir = unique_temp("suggest-exists");
        let root = data_dir.join("dsh-data");
        std::fs::create_dir_all(&root).unwrap();
        let out = suggest_targets(&data_dir, &unique_temp("suggest-home2"), None);
        assert!(out[0].exists);
        std::fs::remove_dir_all(&data_dir).ok();
    }

    #[test]
    fn suggest_targets_includes_last_used_and_dedupes() {
        let data_dir = unique_temp("suggest-last");
        let home = unique_temp("suggest-home3");
        let last = unique_temp("suggest-lastused");
        let out = suggest_targets(&data_dir, &home, Some(&last.to_string_lossy()));
        assert!(out.iter().any(|s| s.id == "last-used"));
        assert_eq!(
            out.iter()
                .find(|s| s.id == "last-used")
                .map(|s| PathBuf::from(&s.path)),
            Some(last.clone())
        );
        // A last-used root equal to an earlier candidate must not duplicate.
        let dup = suggest_targets(
            &data_dir,
            &home,
            Some(&data_dir.join("dsh-data").to_string_lossy()),
        );
        assert_eq!(dup.iter().filter(|s| s.id == "launcher-data").count(), 1);
        assert!(!dup.iter().any(|s| s.id == "last-used"));
        // Blank/whitespace last-used is ignored rather than offered.
        let blank = suggest_targets(&data_dir, &home, Some("   "));
        assert!(!blank.iter().any(|s| s.id == "last-used"));
    }

    #[test]
    fn suggest_targets_proposes_a_root_outside_the_home() {
        // `set_home_link` rejects targets inside the HOME, so a suggestion
        // must never be nested under it.
        let data_dir = unique_temp("suggest-outside");
        let home = unique_temp("suggest-home4");
        let out = suggest_targets(&data_dir, &home, None);
        for s in &out {
            assert!(
                !Path::new(&s.path).starts_with(&home),
                "suggestion {} must stay outside the HOME",
                s.path
            );
        }
        if let Some(same) = out.iter().find(|s| s.id == "same-drive") {
            assert!(same.path.ends_with("dsh-data"));
            assert!(PathBuf::from(&same.path).is_absolute());
        }
    }

    #[test]
    fn same_volume_root_is_absolute_and_outside_the_home() {
        let home = unique_temp("suggest-vol").join("homes").join("lab");
        let root = same_volume_root(&home).expect("a volume root exists for a nested path");
        assert!(root.is_absolute());
        assert!(root.ends_with("dsh-data"));
        assert!(!root.starts_with(&home));
        // A bare root has no parent to hang `<parent>/dsh-data` off.
        assert_eq!(same_volume_root(Path::new("/")), None);
    }

    #[test]
    fn last_link_root_is_backward_compatible() {
        // Older configs have no `last_link_root` key at all and must still
        // parse; absent means "never redirected yet".
        let json = r#"{"locale":"zh-CN"}"#;
        let s: crate::config::LauncherSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.last_link_root, None);
        let mut with = s.clone();
        with.last_link_root = Some("D:/dsh-data".to_string());
        let raw = serde_json::to_string(&with).unwrap();
        assert!(raw.contains("last_link_root"));
        // None stays out of the file so the default config is unchanged.
        assert!(!serde_json::to_string(&s)
            .unwrap()
            .contains("last_link_root"));
    }
}
