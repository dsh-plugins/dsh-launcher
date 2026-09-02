use crate::config::{
    new_id, sanitize_name, DshHome, DshInstance, DshVersion, LauncherSettings, NewInstanceInput,
    RemoteVersion, SettingsPatch,
};
use crate::{process, AppState};
use std::collections::BTreeMap;
use tauri::{AppHandle, State};

// ---------------------------------------------------------------------------
// DSH_HOME
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_homes(state: State<'_, AppState>) -> Result<Vec<DshHome>, String> {
    Ok(state.config.lock().unwrap().homes.clone())
}

#[tauri::command]
pub fn create_home(
    state: State<'_, AppState>,
    name: String,
    path: String,
) -> Result<DshHome, String> {
    create_home_record(&state, &name, &path)
}

/// Shared helper: validates + creates a DSH_HOME record. If a HOME with the
/// same (case-insensitively on Windows) path already exists, the existing
/// record is returned instead of creating a duplicate (prevents duplicate
/// same-name HOMEs when a dedicated HOME is requested repeatedly).
pub(crate) fn create_home_record(
    state: &State<'_, AppState>,
    name: &str,
    path: &str,
) -> Result<DshHome, String> {
    let name = name.trim();
    let path = path.trim();
    if name.is_empty() {
        return Err("名称不能为空".to_string());
    }
    if path.is_empty() {
        return Err("路径不能为空".to_string());
    }
    let path_buf = std::path::PathBuf::from(path);

    // Reuse an existing HOME with the same normalized path.
    {
        let cfg = state.config.lock().unwrap();
        if let Some(existing) = cfg
            .homes
            .iter()
            .find(|h| crate::config::paths_equal(&h.path, &path_buf))
        {
            return Ok(existing.clone());
        }
    }

    std::fs::create_dir_all(&path_buf).map_err(|e| format!("创建目录失败: {e}"))?;
    let home = DshHome {
        id: new_id("h"),
        name: name.to_string(),
        path: path_buf,
    };
    let mut cfg = state.config.lock().unwrap();
    cfg.homes.push(home.clone());
    save_state(state, &cfg)?;
    Ok(home)
}

#[tauri::command]
pub fn default_dedicated_home_path(
    state: State<'_, AppState>,
    name: String,
) -> Result<String, String> {
    Ok(state
        .data_dir
        .join("homes")
        .join(sanitize_name(&name))
        .to_string_lossy()
        .to_string())
}

#[tauri::command]
pub fn remove_home(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let mut cfg = state.config.lock().unwrap();
    if cfg.instances.iter().any(|i| i.home_id == id) {
        return Err("该 DSH_HOME 仍被实例引用，无法删除".to_string());
    }
    cfg.homes.retain(|h| h.id != id);
    save_state(&state, &cfg)
}

// ---------------------------------------------------------------------------
// DSH versions
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_versions(state: State<'_, AppState>) -> Result<Vec<DshVersion>, String> {
    Ok(state.config.lock().unwrap().versions.clone())
}

/// Queries the npm registry for available @deepseek-ai/dsh versions with
/// their publish dates, then merges in GitHub-only `dsh-v*` release tags
/// (alpha builds are tagged on GitHub but not published to npm) marked with
/// `source: "github"`.
#[tauri::command]
pub async fn fetch_available_versions() -> Result<Vec<RemoteVersion>, String> {
    let versions_json = run_npm_view("@deepseek-ai/dsh", "versions").await?;
    let versions: Vec<String> =
        serde_json::from_str(&versions_json).map_err(|e| format!("解析版本列表失败: {e}"))?;

    let time_json = run_npm_view("@deepseek-ai/dsh", "time").await?;
    // npm >= 9 returns `time` as `[{ "created": ..., "<version>": "<date>" }]`
    // (array wrapping one object); older npm returned the object directly.
    let time_map: BTreeMap<String, serde_json::Value> =
        match serde_json::from_str::<BTreeMap<String, serde_json::Value>>(&time_json) {
            Ok(map) => map,
            Err(_) => {
                let arr: Vec<BTreeMap<String, serde_json::Value>> =
                    serde_json::from_str(&time_json)
                        .map_err(|e| format!("解析发布时间失败: {e}"))?;
                arr.into_iter().next().unwrap_or_default()
            }
        };

    let mut out = Vec::with_capacity(versions.len());
    for v in versions {
        let released_at = time_map
            .get(&v)
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        out.push(RemoteVersion {
            version: v,
            released_at,
            source: None,
        });
    }

    // GitHub-only tags (e.g. dsh-v0.1.2-alpha.1). A release listing failure
    // never aborts the npm listing.
    match fetch_github_tag_versions().await {
        Ok(git_versions) => {
            for gv in git_versions {
                if out.iter().any(|r| r.version == gv.version) {
                    continue;
                }
                out.push(gv);
            }
        }
        Err(e) => crate::log_warn!("获取 GitHub dsh-v* 标签失败，忽略: {e}"),
    }
    Ok(out)
}

/// Upstream repo whose `dsh-v<version>` tags carry releases.
pub(crate) const DSH_REPO: &str = "deepseek-ai/deepseek-harness";

/// Versions tagged on GitHub as `dsh-v*` releases (whether or not they were
/// later published to npm — dedup happens at the caller).
async fn fetch_github_tag_versions() -> Result<Vec<RemoteVersion>, String> {
    let url = crate::plugins::github_api_url(&format!("/repos/{DSH_REPO}/releases?per_page=100"));
    let doc = crate::plugins::fetch_json_pub(&url, 8 * 1024 * 1024).await?;
    let mut out = Vec::new();
    let Some(arr) = doc.as_array() else {
        return Ok(out);
    };
    for rel in arr {
        if rel.get("draft").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }
        let Some(tag) = rel.get("tag_name").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(version) = tag.strip_prefix("dsh-v") else {
            continue;
        };
        if version.is_empty() {
            continue;
        }
        out.push(RemoteVersion {
            version: version.to_string(),
            released_at: rel
                .get("published_at")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            source: Some("github".to_string()),
        });
    }
    Ok(out)
}

async fn run_npm_view(pkg: &str, field: &str) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new(process::npm());
    cmd.args(["view", pkg, field, "--json"]);
    let output = crate::process::hide_console(&mut cmd)
        .output()
        .await
        .map_err(|e| format!("npm 执行失败: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "npm view 失败: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[tauri::command]
pub fn remove_version(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let mut cfg = state.config.lock().unwrap();
    if cfg.instances.iter().any(|i| i.version_id == id) {
        return Err("该版本仍被实例引用，无法删除".to_string());
    }
    let Some(version) = cfg.versions.iter().find(|v| v.id == id).cloned() else {
        return Err("版本不存在".to_string());
    };
    cfg.versions.retain(|v| v.id != id);
    save_state(&state, &cfg)?;
    // Best-effort removal of the install directory.
    let _ = std::fs::remove_dir_all(&version.dir);
    Ok(())
}

// ---------------------------------------------------------------------------
// Instances
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_instances(state: State<'_, AppState>) -> Result<Vec<DshInstance>, String> {
    Ok(state.config.lock().unwrap().instances.clone())
}

#[tauri::command]
pub fn create_instance(
    state: State<'_, AppState>,
    input: NewInstanceInput,
) -> Result<DshInstance, String> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err("实例名称不能为空".to_string());
    }
    let mut cfg = state.config.lock().unwrap();
    if cfg.instances.iter().any(|i| i.name == name) {
        return Err("同名实例已存在".to_string());
    }
    if !cfg.versions.iter().any(|v| v.id == input.version_id) {
        return Err("DSH 版本不存在".to_string());
    }
    if !cfg.homes.iter().any(|h| h.id == input.home_id) {
        return Err("DSH_HOME 不存在".to_string());
    }
    let inst = DshInstance {
        id: new_id("i"),
        name,
        version_id: input.version_id,
        home_id: input.home_id,
        env_overrides: input.env_overrides,
        default_profile: input.default_profile,
        last_profile: None,
        icon: None,

        port: None,
    };
    cfg.instances.push(inst.clone());
    save_state(&state, &cfg)?;
    Ok(inst)
}

#[tauri::command]
pub fn update_instance(
    state: State<'_, AppState>,
    input: DshInstance,
) -> Result<DshInstance, String> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err("实例名称不能为空".to_string());
    }
    let mut cfg = state.config.lock().unwrap();
    if cfg
        .instances
        .iter()
        .any(|i| i.name == name && i.id != input.id)
    {
        return Err("同名实例已存在".to_string());
    }
    if !cfg.versions.iter().any(|v| v.id == input.version_id) {
        return Err("DSH 版本不存在".to_string());
    }
    if !cfg.homes.iter().any(|h| h.id == input.home_id) {
        return Err("DSH_HOME 不存在".to_string());
    }
    let mut updated = input;
    updated.name = name;
    let Some(pos) = cfg.instances.iter().position(|i| i.id == updated.id) else {
        return Err("实例不存在".to_string());
    };
    // The icon is managed by set/clear_instance_icon and the port by
    // set_instance_port, not by this form payload; preserve whatever is
    // currently stored (the frontend spreads a possibly stale instance).
    updated.icon = cfg.instances[pos].icon.clone();
    updated.port = cfg.instances[pos].port;
    cfg.instances[pos] = updated.clone();
    save_state(&state, &cfg)?;
    Ok(updated)
}

/// Sets an instance's preferred web port (issue #21). Empty / out-of-range
/// input (0, negative, > 65535, non-integer) means "random" — stored as
/// `None`, so launch passes `--port 0`.
#[tauri::command(rename_all = "snake_case")]
pub fn set_instance_port(
    state: State<'_, AppState>,
    instance_id: String,
    port: Option<i64>,
) -> Result<crate::config::DshInstance, String> {
    let port = match port {
        Some(p) if (1..=65535).contains(&p) => Some(p as u16),
        _ => None,
    };
    let mut cfg = state.config.lock().unwrap();
    let Some(inst) = cfg.instances.iter_mut().find(|i| i.id == instance_id) else {
        return Err("实例不存在".to_string());
    };
    inst.port = port;
    let updated = inst.clone();
    save_state(&state, &cfg)?;
    Ok(updated)
}

#[tauri::command]
pub async fn delete_instance(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    // Stop it first if running.
    if state.running.lock().await.contains_key(&id) {
        let _ = process::stop_instance_process(&app, &state, &id).await;
    }
    let mut cfg = state.config.lock().unwrap();
    cfg.instances.retain(|i| i.id != id);
    if cfg.settings.last_instance_id.as_deref() == Some(id.as_str()) {
        cfg.settings.last_instance_id = None;
    }
    save_state(&state, &cfg)
}

/// Input for duplicating an instance.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct CopyInstanceInput {
    /// The source instance id (the "母本").
    pub source_id: String,
    /// Name for the copied instance.
    pub name: String,
    /// When true, create a fresh dedicated DSH_HOME for the copy instead of
    /// reusing the source instance's DSH_HOME.
    pub new_home: bool,
}

/// Copies an instance: creates a new instance record with a new id/name. The
/// copy either reuses the source instance's DSH_HOME (sharing sessions and
/// profiles) or gets a brand-new dedicated DSH_HOME. The DSH version is
/// always reused (the same binary can serve many instances).
#[tauri::command(rename_all = "snake_case")]
pub fn copy_instance(
    state: State<'_, AppState>,
    input: CopyInstanceInput,
) -> Result<DshInstance, String> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err("实例名称不能为空".to_string());
    }

    let mut cfg = state.config.lock().unwrap();
    if cfg.instances.iter().any(|i| i.name == name) {
        return Err("同名实例已存在".to_string());
    }
    let source = cfg
        .instances
        .iter()
        .find(|i| i.id == input.source_id)
        .cloned()
        .ok_or_else(|| "源实例不存在".to_string())?;
    if !cfg.versions.iter().any(|v| v.id == source.version_id) {
        return Err("DSH 版本不存在".to_string());
    }

    // Resolve the DSH_HOME: reuse the source's, or create a dedicated one.
    let home_id = if input.new_home {
        let path = state
            .data_dir
            .join("homes")
            .join(sanitize_name(&name))
            .to_string_lossy()
            .to_string();
        let path_buf = std::path::PathBuf::from(&path);
        // Reuse an existing HOME with the same path (path-based reuse).
        if let Some(existing) = cfg
            .homes
            .iter()
            .find(|h| crate::config::paths_equal(&h.path, &path_buf))
        {
            existing.id.clone()
        } else {
            std::fs::create_dir_all(&path_buf).map_err(|e| format!("创建目录失败: {e}"))?;
            let home = DshHome {
                id: new_id("h"),
                name: name.clone(),
                path: path_buf,
            };
            cfg.homes.push(home.clone());
            home.id
        }
    } else {
        source.home_id.clone()
    };

    let mut inst = DshInstance {
        id: new_id("i"),
        name,
        version_id: source.version_id,
        home_id,
        env_overrides: source.env_overrides.clone(),
        default_profile: source.default_profile.clone(),
        last_profile: None,
        icon: source.icon.clone(),
        port: source.port,
    };
    // A local icon is stored per instance id; copy the file for the clone,
    // falling back to the launcher default when it cannot be carried over.
    if source.icon.as_deref() == Some("local") {
        let src_home = cfg
            .homes
            .iter()
            .find(|h| h.id == source.home_id)
            .map(|h| h.path.clone());
        let dst_home = cfg
            .homes
            .iter()
            .find(|h| h.id == inst.home_id)
            .map(|h| h.path.clone());
        let copied = match (src_home, dst_home) {
            (Some(src_home), Some(dst_home)) => {
                let src_icon = crate::icons::local_icon_path(&src_home, &source.id);
                let dst_icon = crate::icons::local_icon_path(&dst_home, &inst.id);
                if let Some(parent) = dst_icon.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                src_icon.exists() && std::fs::copy(&src_icon, &dst_icon).is_ok()
            }
            _ => false,
        };
        if !copied {
            inst.icon = None;
        }
    }
    cfg.instances.push(inst.clone());
    save_state(&state, &cfg)?;
    Ok(inst)
}

#[tauri::command(rename_all = "snake_case")]
pub fn list_profiles(state: State<'_, AppState>, home_id: String) -> Result<Vec<String>, String> {
    let cfg = state.config.lock().unwrap();
    let home = cfg
        .homes
        .iter()
        .find(|h| h.id == home_id)
        .ok_or_else(|| "DSH_HOME 不存在".to_string())?;
    let profiles_dir = home.path.join("profiles");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&profiles_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // Skip the template and non-profile entries.
            if name == "node_modules" || name == "__temp__" {
                continue;
            }
            if entry.path().is_dir() {
                out.push(name);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Creates a new profile by copying the `__temp__` template inside the
/// given HOME. Returns the created profile name.
#[tauri::command(rename_all = "snake_case")]
pub fn create_profile(
    state: State<'_, AppState>,
    home_id: String,
    name: String,
) -> Result<String, String> {
    let name = name.trim().to_string();
    validate_profile_name(&name)?;

    let profiles_dir = {
        let cfg = state.config.lock().unwrap();
        let home = cfg
            .homes
            .iter()
            .find(|h| h.id == home_id)
            .ok_or_else(|| "DSH_HOME 不存在".to_string())?;
        home.path.join("profiles")
    };

    let temp_dir = profiles_dir.join("__temp__");
    if !temp_dir.is_dir() {
        return Err("模板 __temp__ 不存在，请先创建实例以生成模板".to_string());
    }
    let target = profiles_dir.join(&name);
    if target.exists() {
        return Err(format!("Profile「{name}」已存在"));
    }

    copy_dir_recursive(&temp_dir, &target).map_err(|e| format!("创建 Profile 失败: {e}"))?;
    Ok(name)
}

/// Copies an existing profile directory to a new name inside the same HOME.
/// The copy is only materialized after the new name is validated, mirroring
/// create_profile's copy-from-template behavior.
#[tauri::command(rename_all = "snake_case")]
pub fn copy_profile(
    state: State<'_, AppState>,
    home_id: String,
    source: String,
    name: String,
) -> Result<String, String> {
    let name = name.trim().to_string();
    validate_profile_name(&name)?;

    let profiles_dir = {
        let cfg = state.config.lock().unwrap();
        let home = cfg
            .homes
            .iter()
            .find(|h| h.id == home_id)
            .ok_or_else(|| "DSH_HOME 不存在".to_string())?;
        home.path.join("profiles")
    };

    let from = profiles_dir.join(&source);
    if !from.is_dir() {
        return Err(format!("Profile「{source}」不存在"));
    }
    let to = profiles_dir.join(&name);
    if to.exists() {
        return Err(format!("Profile「{name}」已存在"));
    }

    copy_dir_recursive(&from, &to).map_err(|e| format!("复制 Profile 失败: {e}"))?;
    Ok(name)
}

/// Validates a profile name (shared by create/rename).
fn validate_profile_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Profile 名称不能为空".to_string());
    }
    if name == "__temp__" || name == "node_modules" {
        return Err(format!("「{name}」为保留名称，不能使用"));
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err("Profile 名称只能包含字母、数字、-、_、.".to_string());
    }
    Ok(())
}

/// Renames a profile directory inside the given HOME.
#[tauri::command(rename_all = "snake_case")]
pub fn rename_profile(
    state: State<'_, AppState>,
    home_id: String,
    old_name: String,
    new_name: String,
) -> Result<String, String> {
    validate_profile_name(&new_name)?;
    let new_name = new_name.trim().to_string();

    let profiles_dir = {
        let cfg = state.config.lock().unwrap();
        let home = cfg
            .homes
            .iter()
            .find(|h| h.id == home_id)
            .ok_or_else(|| "DSH_HOME 不存在".to_string())?;
        home.path.join("profiles")
    };

    let from = profiles_dir.join(&old_name);
    if !from.is_dir() {
        return Err(format!("Profile「{old_name}」不存在"));
    }
    let to = profiles_dir.join(&new_name);
    if to.exists() {
        return Err(format!("Profile「{new_name}」已存在"));
    }

    std::fs::rename(&from, &to).map_err(|e| format!("重命名 Profile 失败: {e}"))?;

    // Keep the instance's default/last profile references in sync.
    {
        let mut cfg = state.config.lock().unwrap();
        for inst in cfg.instances.iter_mut() {
            if inst.home_id == home_id {
                if inst.default_profile.as_deref() == Some(old_name.as_str()) {
                    inst.default_profile = Some(new_name.clone());
                }
                if inst.last_profile.as_deref() == Some(old_name.as_str()) {
                    inst.last_profile = Some(new_name.clone());
                }
            }
        }
        save_state(&state, &cfg)?;
    }

    Ok(new_name)
}

/// Deletes a profile directory inside the given HOME. The default/last profile
/// references on instances using this HOME are cleared when they point at the
/// removed profile.
#[tauri::command(rename_all = "snake_case")]
pub fn delete_profile(
    state: State<'_, AppState>,
    home_id: String,
    name: String,
) -> Result<(), String> {
    let profiles_dir = {
        let cfg = state.config.lock().unwrap();
        let home = cfg
            .homes
            .iter()
            .find(|h| h.id == home_id)
            .ok_or_else(|| "DSH_HOME 不存在".to_string())?;
        home.path.join("profiles")
    };

    let target = profiles_dir.join(&name);
    if !target.is_dir() {
        return Err(format!("Profile「{name}」不存在"));
    }
    if name == "__temp__" || name == "node_modules" {
        return Err(format!("「{name}」为保留名称，不能删除"));
    }

    std::fs::remove_dir_all(&target).map_err(|e| format!("删除 Profile 失败: {e}"))?;

    {
        let mut cfg = state.config.lock().unwrap();
        for inst in cfg.instances.iter_mut() {
            if inst.home_id == home_id {
                if inst.default_profile.as_deref() == Some(name.as_str()) {
                    inst.default_profile = None;
                }
                if inst.last_profile.as_deref() == Some(name.as_str()) {
                    inst.last_profile = None;
                }
            }
        }
        save_state(&state, &cfg)?;
    }

    Ok(())
}

/// Recursively copies a directory tree.
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ty.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Instance runtime
// ---------------------------------------------------------------------------

/// Resolves (home path, version dir, version string) for an instance.
fn resolve_instance_paths(
    state: &State<'_, AppState>,
    instance_id: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf, String), String> {
    let cfg = state.config.lock().unwrap();
    let inst = cfg
        .instances
        .iter()
        .find(|i| i.id == instance_id)
        .ok_or_else(|| "实例不存在".to_string())?;
    let home = cfg
        .homes
        .iter()
        .find(|h| h.id == inst.home_id)
        .ok_or_else(|| "DSH_HOME 不存在".to_string())?;
    let version = cfg
        .versions
        .iter()
        .find(|v| v.id == inst.version_id)
        .ok_or_else(|| "版本不存在".to_string())?;
    Ok((
        home.path.clone(),
        version.dir.clone(),
        version.version.clone(),
    ))
}

/// Dependency-tree preflight for an instance + profile. Advisory only: the
/// report never blocks a launch, it is logged and handed to the UI.
#[tauri::command(rename_all = "snake_case")]
pub fn check_instance_health(
    state: State<'_, AppState>,
    instance_id: String,
    profile: String,
) -> Result<crate::doctor::DoctorReport, String> {
    let (home_path, version_dir, version) = resolve_instance_paths(&state, &instance_id)?;
    let profile_dir = home_path.join("profiles").join(&profile);
    let report =
        crate::doctor::inspect(&instance_id, &profile, &version_dir, &version, &profile_dir);
    crate::doctor::log_report(&report);
    Ok(report)
}

#[tauri::command]
pub async fn start_instance(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    profile: String,
) -> Result<(), String> {
    // Preflight the dependency tree before spawning: a duplicated core copy
    // in the profile breaks every tool call at runtime with no load-time
    // error, so it is reported up front instead of being debugged later.
    // Findings never block the launch.
    if let Ok((home_path, version_dir, version)) = resolve_instance_paths(&state, &id) {
        let report = crate::doctor::inspect(
            &id,
            &profile,
            &version_dir,
            &version,
            &home_path.join("profiles").join(&profile),
        );
        crate::doctor::log_report(&report);
        if !report.findings.is_empty() {
            use tauri::Emitter;
            let _ = app.emit(crate::doctor::HEALTH_EVENT, &report);
        }
    }

    process::start_instance_process(&app, &state, &id, &profile).await?;
    // Remember the last used profile.
    let mut cfg = state.config.lock().unwrap();
    if let Some(inst) = cfg.instances.iter_mut().find(|i| i.id == id) {
        inst.last_profile = Some(profile);
    }
    save_state(&state, &cfg)
}

#[tauri::command]
pub async fn stop_instance(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    process::stop_instance_process(&app, &state, &id).await
}

#[tauri::command]
pub async fn list_instance_status(
    state: State<'_, AppState>,
) -> Result<Vec<crate::config::InstanceStatus>, String> {
    Ok(process::list_statuses(&state).await)
}

#[tauri::command]
pub async fn open_instance_window(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let entry = state.running.lock().await.get(&id).map(|r| r.url.clone());
    let Some(url) = entry.flatten() else {
        return Err("实例未在运行或尚未就绪".to_string());
    };
    let name = state
        .config
        .lock()
        .unwrap()
        .instances
        .iter()
        .find(|i| i.id == id)
        .map(|i| i.name.clone())
        .unwrap_or_else(|| id.clone());
    crate::windows::open_instance_window(&app, &id, &name, &url)
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<LauncherSettings, String> {
    Ok(state.config.lock().unwrap().settings.clone())
}

/// The launcher's data directory (`<data_dir>`); the frontend shows it in
/// the settings page next to the "open directory" button.
#[tauri::command]
pub fn get_launcher_directory(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.data_dir.to_string_lossy().to_string())
}

#[tauri::command]
pub fn update_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: SettingsPatch,
) -> Result<LauncherSettings, String> {
    let mut cfg = state.config.lock().unwrap();
    if let Some(v) = settings.locale {
        cfg.settings.locale = v;
    }
    if let Some(v) = settings.minimize_to_tray {
        cfg.settings.minimize_to_tray = v;
    }
    if let Some(v) = settings.autostart {
        let prev = cfg.settings.autostart;
        cfg.settings.autostart = v;
        if v != prev {
            use tauri_plugin_autostart::ManagerExt;
            let mgr = app.autolaunch();
            let result = if v { mgr.enable() } else { mgr.disable() };
            if let Err(e) = result {
                // Revert the stored flag so the UI stays truthful.
                cfg.settings.autostart = prev;
                return Err(format!("设置开机自启失败: {e}"));
            }
        }
    }
    if let Some(v) = settings.last_instance_id {
        cfg.settings.last_instance_id = Some(v);
    }
    if let Some(v) = settings.news_source {
        cfg.settings.news_source = v.trim().to_string();
    }
    if let Some(v) = settings.theme {
        match v.as_str() {
            "light" | "dark" | "system" => cfg.settings.theme = v,
            _ => return Err(format!("无效的主题: {v}")),
        }
    }
    if let Some(v) = settings.log_level {
        match crate::applog::parse_level(&v) {
            Some(level) => {
                cfg.settings.log_level = v.trim().to_ascii_lowercase();
                crate::applog::set_level(level);
                crate::log_info!("日志等级已切换为 {}", level.as_str());
            }
            None => return Err(format!("无效的日志等级: {v}")),
        }
    }
    if let Some(v) = settings.skill_repos {
        cfg.settings.skill_repos = v
            .into_iter()
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty())
            .collect();
    }
    if let Some(v) = settings.proxy_enabled {
        cfg.settings.proxy_enabled = v;
    }
    if let Some(v) = settings.proxy_url {
        let v = v.trim().trim_end_matches('/').to_string();
        if !v.is_empty() {
            cfg.settings.proxy_url = v;
        }
    }
    if let Some(v) = settings.proxy_port {
        cfg.settings.proxy_port = v;
    }
    if let Some(v) = settings.no_proxy {
        cfg.settings.no_proxy = v.trim().to_string();
    }
    if let Some(v) = settings.proxy_apply_dsh {
        cfg.settings.proxy_apply_dsh = v;
    }
    crate::proxy::sync_from_settings(&cfg.settings);
    let out = cfg.settings.clone();
    save_state(&state, &cfg)?;
    crate::log_debug!("设置已更新并保存");
    Ok(out)
}

// ---------------------------------------------------------------------------
// External links / directories
// ---------------------------------------------------------------------------

/// Opens an http(s) URL in the system browser. The Tauri webview ignores
/// `target="_blank"` anchors, so external links must go through here.
#[tauri::command]
pub fn open_external(url: String) -> Result<(), String> {
    let url = url.trim().to_string();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("仅允许打开 http(s) 链接: {url}"));
    }
    crate::log_info!("在系统浏览器打开 {url}");
    open::that(&url).map_err(|e| format!("打开链接失败: {e}"))
}

/// Opens the file manager at a log file with the file selected (Windows
/// Explorer, macOS Finder). Falls back to opening the file itself on other
/// platforms. Returns the resolved log path so the UI can show it.
fn reveal_log_file(
    _app: &tauri::AppHandle,
    log_path: std::path::PathBuf,
) -> Result<String, String> {
    let path_str = log_path.to_string_lossy().to_string();
    if log_path.exists() {
        crate::log_info!("在文件管理器中定位日志文件 {path_str}");
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            let status = std::process::Command::new("explorer.exe")
                .arg(format!("/select,{}", log_path.display()))
                .creation_flags(0x0800_0000)
                .spawn()
                .and_then(|mut c| c.wait());
            if status.is_ok() {
                return Ok(path_str);
            }
            crate::log_warn!("explorer /select 失败，改用默认打开方式");
        }
        #[cfg(target_os = "macos")]
        {
            let status = std::process::Command::new("open")
                .arg("-R")
                .arg(&log_path)
                .spawn()
                .and_then(|mut c| c.wait());
            if status.is_ok() {
                return Ok(path_str);
            }
            crate::log_warn!("open -R 失败，改用默认打开方式");
        }
        // Fallback: open the file with its default application. This also
        // covers Linux (no built-in file-selection reveal).
        open::that(&log_path).map_err(|e| format!("打开日志文件失败: {e}"))?;
        Ok(path_str)
    } else {
        // The log file does not exist yet: open the log directory instead.
        let dir = log_path
            .parent()
            .map(|d| d.to_path_buf())
            .unwrap_or_default();
        crate::log_info!("日志文件不存在，打开日志目录 {}", dir.display());
        open::that(&dir).map_err(|e| format!("打开日志目录失败: {e}"))?;
        Ok(dir.to_string_lossy().to_string())
    }
}

/// Opens the launcher's own data directory (config, homes, logs, …).
#[tauri::command]
pub fn open_launcher_directory(state: State<'_, AppState>) -> Result<String, String> {
    let dir = state.data_dir.clone();
    if !dir.is_dir() {
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建数据目录失败: {e}"))?;
    }
    crate::log_info!("在文件管理器中打开启动器数据目录 {}", dir.display());
    open::that(&dir).map_err(|e| format!("打开目录失败: {e}"))?;
    Ok(dir.to_string_lossy().to_string())
}

/// Reveals the launcher runtime log (`<data_dir>/logs/latest.log`) in the
/// file manager with the file selected, creating the directory when needed.
#[tauri::command]
pub fn open_launcher_log(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let log_dir = state.data_dir.join("logs");
    std::fs::create_dir_all(&log_dir).map_err(|e| format!("创建日志目录失败: {e}"))?;
    reveal_log_file(&app, log_dir.join("latest.log"))
}

/// Reveals one instance's runtime log (`<data_dir>/logs/<instance_id>.log`)
/// in the file manager with the file selected.
#[tauri::command]
pub fn open_instance_log(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<String, String> {
    {
        let cfg = state.config.lock().unwrap();
        if !cfg.instances.iter().any(|i| i.id == instance_id) {
            return Err("实例不存在".to_string());
        }
    }
    let log_dir = state.data_dir.join("logs");
    std::fs::create_dir_all(&log_dir).map_err(|e| format!("创建日志目录失败: {e}"))?;
    reveal_log_file(&app, log_dir.join(format!("{instance_id}.log")))
}

/// Opens the DSH_HOME directory of one instance in the file manager.
#[tauri::command]
pub fn open_instance_directory(
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<String, String> {
    let home = {
        let cfg = state.config.lock().unwrap();
        let inst = cfg
            .instances
            .iter()
            .find(|i| i.id == instance_id)
            .ok_or_else(|| "实例不存在".to_string())?;
        cfg.homes
            .iter()
            .find(|h| h.id == inst.home_id)
            .map(|h| h.path.clone())
            .ok_or_else(|| "DSH_HOME 不存在".to_string())?
    };
    if !home.is_dir() {
        std::fs::create_dir_all(&home).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    crate::log_info!(
        "在文件管理器中打开实例 {} 的 DSH_HOME {}",
        instance_id,
        home.display()
    );
    open::that(&home).map_err(|e| format!("打开目录失败: {e}"))?;
    Ok(home.to_string_lossy().to_string())
}

// ---------------------------------------------------------------------------
// Launch shortcut (issue #9)
// ---------------------------------------------------------------------------

/// Percent-encodes one URI query value (non-ASCII and reserved chars).
fn uri_encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Default shortcut icon when the instance has none (bundled launcher icon).
const DEFAULT_ICON_PNG: &[u8] = include_bytes!("../icons/128x128.png");

/// Writes a Windows Internet Shortcut (.url) that launches an instance via
/// `dsh-launcher://launch?instance=<name>&profile=<profile>`. The shortcut
/// icon is the instance icon converted to .ico (Windows shortcuts only accept
/// ICO); with no icon the launcher icon is used instead (issue #14).
#[tauri::command]
pub async fn create_launch_shortcut(
    state: State<'_, AppState>,
    instance_id: String,
    profile: String,
    dest_path: String,
) -> Result<(), String> {
    let (name, icon, home) = {
        let cfg = state.config.lock().unwrap();
        let inst = cfg
            .instances
            .iter()
            .find(|i| i.id == instance_id)
            .ok_or_else(|| "实例不存在".to_string())?;
        let home = cfg
            .homes
            .iter()
            .find(|h| h.id == inst.home_id)
            .map(|h| h.path.clone())
            .ok_or_else(|| "DSH_HOME 不存在".to_string())?;
        (inst.name.clone(), inst.icon.clone(), home)
    };

    let url = format!(
        "dsh-launcher://launch?instance={}&profile={}",
        uri_encode(&name),
        uri_encode(profile.trim()),
    );

    // Resolve the icon as PNG bytes: local file → remote URL → launcher
    // default. Then convert to ICO — Windows .url shortcuts ignore PNG icons.
    let icon_png: Vec<u8> = match icon.as_deref() {
        Some("local") => std::fs::read(crate::icons::local_icon_path(&home, &instance_id))
            .unwrap_or_else(|_| DEFAULT_ICON_PNG.to_vec()),
        Some(remote) if remote.starts_with("https://") || remote.starts_with("http://") => {
            crate::icons::fetch_square_icon_png(remote)
                .await
                .unwrap_or_else(|_| DEFAULT_ICON_PNG.to_vec())
        }
        _ => DEFAULT_ICON_PNG.to_vec(),
    };
    let icon_file = match crate::icons::png_to_ico_bytes(&icon_png) {
        Ok(ico) => {
            let dir = state.data_dir.join("icons");
            std::fs::create_dir_all(&dir).ok();
            let ico_path = dir.join(format!("shortcut-{instance_id}.ico"));
            std::fs::write(&ico_path, ico)
                .map(|_| ico_path)
                .map_err(|e| format!("写入 ICO 图标失败: {e}"))?
        }
        Err(_) => std::env::current_exe().map_err(|e| format!("获取启动器路径失败: {e}"))?,
    };
    let content = format!(
        "[InternetShortcut]\r\nURL={url}\r\nIconFile={}\r\nIconIndex=0\r\n",
        icon_file.display()
    );
    let dest = std::path::PathBuf::from(dest_path.trim());
    if dest.as_os_str().is_empty() {
        return Err("保存路径不能为空".to_string());
    }
    std::fs::write(&dest, content).map_err(|e| format!("写入快捷方式失败: {e}"))?;
    crate::log_info!("已创建启动快捷方式 {}", dest.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// News feed
// ---------------------------------------------------------------------------

/// Maximum accepted news payload (2 MiB).
const NEWS_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Fetches the raw news content from an http(s) URL or a local .md/.html
/// file path. Rendering and XSS sanitizing happen on the frontend.
#[tauri::command]
pub async fn fetch_news(source: String) -> Result<String, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("新闻来源不能为空".to_string());
    }

    if source.starts_with("http://") || source.starts_with("https://") {
        let client = crate::proxy::apply(reqwest::Client::builder())
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("dsh-launcher")
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
        let resp = client
            .get(source)
            .send()
            .await
            .map_err(|e| format!("请求新闻源失败: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("新闻源返回 HTTP {status}"));
        }
        if let Some(len) = resp.content_length() {
            if len > NEWS_MAX_BYTES {
                return Err("新闻内容超过 2 MiB，已拒绝".to_string());
            }
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("读取新闻内容失败: {e}"))?;
        if bytes.len() as u64 > NEWS_MAX_BYTES {
            return Err("新闻内容超过 2 MiB，已拒绝".to_string());
        }
        Ok(String::from_utf8_lossy(&bytes).to_string())
    } else {
        let path = std::path::PathBuf::from(source);
        let meta = std::fs::metadata(&path).map_err(|e| format!("读取新闻文件失败: {e}"))?;
        if !meta.is_file() {
            return Err("新闻来源不是文件".to_string());
        }
        if meta.len() > NEWS_MAX_BYTES {
            return Err("新闻内容超过 2 MiB，已拒绝".to_string());
        }
        std::fs::read_to_string(&path).map_err(|e| format!("读取新闻文件失败: {e}"))
    }
}

// ---------------------------------------------------------------------------

pub(crate) fn save_state(
    state: &State<'_, AppState>,
    cfg: &crate::config::Config,
) -> Result<(), String> {
    crate::config::save_config(&state.config_path, cfg)
}
