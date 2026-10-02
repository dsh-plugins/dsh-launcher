/// Provider 配置相关的 Tauri 命令 (Issue #76)
///
/// 为前端提供 provider 路由的 CRUD、预设导入、验证与凭证管理。
///
/// 路由存储在 patch 层（见 [`crate::provider_patch`]）：全局为
/// `<DSH_HOME>/cordis.patch.yml`，profile 作用域为
/// `<DSH_HOME>/profiles/<profile>/cordis.patch.yml`。所有命令都接受
/// `profile: Option<String>`，`None` 即全局作用域。
use crate::credential_manager::{self, CredentialSource};
use crate::provider_config::ProviderRoute;
use crate::provider_patch;
use crate::provider_presets;
use crate::provider_validator::{self, ValidationReport};
use crate::AppState;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tauri::State;

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 从 instance_id 获取对应的 DshHome 路径
fn get_home_path(state: &State<'_, AppState>, instance_id: &str) -> Result<PathBuf, String> {
    let cfg = state.config.lock().unwrap();
    let instance = cfg
        .instances
        .iter()
        .find(|i| i.id == instance_id)
        .ok_or_else(|| format!("Instance '{}' 不存在", instance_id))?;

    let home = cfg
        .homes
        .iter()
        .find(|h| h.id == instance.home_id)
        .ok_or_else(|| format!("Home '{}' 不存在", instance.home_id))?;

    Ok(home.path.clone())
}

/// instance 的环境变量覆盖（用于凭证解析）。
fn env_overrides_of(state: &State<'_, AppState>, instance_id: &str) -> BTreeMap<String, String> {
    let cfg = state.config.lock().unwrap();
    cfg.instances
        .iter()
        .find(|i| i.id == instance_id)
        .map(|i| i.env_overrides.clone())
        .unwrap_or_default()
}

/// 解析作用域对应的 patch 文件路径。
fn scope_path(
    state: &State<'_, AppState>,
    instance_id: &str,
    profile: Option<&str>,
) -> Result<PathBuf, String> {
    let home = get_home_path(state, instance_id)?;
    provider_patch::patch_path(&home, profile)
}

/// 凭证解析的作用域基准目录。
///
/// 与 `validate_providers` 透传给 [`crate::provider_validator::validate_routes`] 的
/// `profile_path` 保持一致：全局作用域返回 `home`，profile 作用域返回
/// `profiles/<p>` 目录。这样「凭据状态列 / 读取」与「校验报告」使用**完全相同**的
/// 解析作用域，避免出现"状态列报缺失、校验报告却说已配置（来源: Profile .env）"
/// 的自相矛盾（issue #76 PR 评审 [Medium]）。
fn credential_scope_path(home: &Path, profile: Option<&str>) -> PathBuf {
    match profile {
        None => home.to_path_buf(),
        Some(p) => crate::plugins::profile_dir_pub(home, p),
    }
}

/// 读取作用域内的全部路由。
fn read_routes(
    state: &State<'_, AppState>,
    instance_id: &str,
    profile: Option<&str>,
) -> Result<Vec<ProviderRoute>, String> {
    let path = scope_path(state, instance_id, profile)?;
    provider_patch::parse_providers(&provider_patch::read_patch(&path)?)
}

// ---------------------------------------------------------------------------
// Provider 配置 CRUD
// ---------------------------------------------------------------------------

/// 获取某个作用域内的 provider 路由。
#[tauri::command]
pub fn get_provider_routes(
    state: State<'_, AppState>,
    instance_id: String,
    profile: Option<String>,
) -> Result<Vec<ProviderRoute>, String> {
    read_routes(&state, &instance_id, profile.as_deref())
}

/// 新增一条 provider route。
#[tauri::command]
pub fn add_provider_route(
    state: State<'_, AppState>,
    instance_id: String,
    profile: Option<String>,
    route: ProviderRoute,
) -> Result<Vec<ProviderRoute>, String> {
    let path = scope_path(&state, &instance_id, profile.as_deref())?;
    let raw = provider_patch::read_patch(&path)?;
    let mut routes = provider_patch::parse_providers(&raw)?;

    crate::provider_config::validate_route(&route)?;
    if routes.iter().any(|r| r.name == route.name) {
        return Err(format!("路由名称 '{}' 已存在", route.name));
    }
    routes.push(route);
    routes.sort_by(|a, b| a.name.cmp(&b.name));

    provider_patch::write_patch(&path, &provider_patch::render_providers(&raw, &routes)?)?;
    Ok(routes)
}

/// 更新一条 provider route；`old_name` 指明被编辑的路由。
#[tauri::command]
pub fn update_provider_route(
    state: State<'_, AppState>,
    instance_id: String,
    profile: Option<String>,
    old_name: String,
    route: ProviderRoute,
) -> Result<Vec<ProviderRoute>, String> {
    let path = scope_path(&state, &instance_id, profile.as_deref())?;
    let raw = provider_patch::read_patch(&path)?;
    let mut routes = provider_patch::parse_providers(&raw)?;

    crate::provider_config::validate_route(&route)?;

    let index = routes
        .iter()
        .position(|r| r.name == old_name)
        .ok_or_else(|| format!("路由 '{}' 不存在", old_name))?;

    // 改名时检查冲突。
    if old_name != route.name && routes.iter().any(|r| r.name == route.name) {
        return Err(format!("路由名称 '{}' 已存在", route.name));
    }

    routes[index] = route;
    routes.sort_by(|a, b| a.name.cmp(&b.name));

    provider_patch::write_patch(&path, &provider_patch::render_providers(&raw, &routes)?)?;
    Ok(routes)
}

/// 删除一条 provider route。
#[tauri::command]
pub fn delete_provider_route(
    state: State<'_, AppState>,
    instance_id: String,
    profile: Option<String>,
    name: String,
) -> Result<Vec<ProviderRoute>, String> {
    let path = scope_path(&state, &instance_id, profile.as_deref())?;
    let raw = provider_patch::read_patch(&path)?;
    let mut routes = provider_patch::parse_providers(&raw)?;

    let index = routes
        .iter()
        .position(|r| r.name == name)
        .ok_or_else(|| format!("路由 '{}' 不存在", name))?;
    routes.remove(index);

    provider_patch::write_patch(&path, &provider_patch::render_providers(&raw, &routes)?)?;
    Ok(routes)
}

// ---------------------------------------------------------------------------
// Provider 预设管理
// ---------------------------------------------------------------------------

/// 获取所有可用的 provider 预设 ID。
#[tauri::command]
pub fn list_provider_presets() -> Vec<String> {
    provider_presets::list_presets()
}

/// 从预设导入 routes（追加模式，按名称去重）。
#[tauri::command]
pub fn import_provider_preset(
    state: State<'_, AppState>,
    instance_id: String,
    profile: Option<String>,
    preset_name: String,
) -> Result<Vec<ProviderRoute>, String> {
    let incoming = provider_presets::get_preset(&preset_name)?;
    let path = scope_path(&state, &instance_id, profile.as_deref())?;
    let raw = provider_patch::read_patch(&path)?;
    let mut routes = provider_patch::parse_providers(&raw)?;

    let added = crate::provider_config::merge_templates(&mut routes, incoming);
    if added == 0 {
        return Err("该预设的路由已全部存在".to_string());
    }
    routes.sort_by(|a, b| a.name.cmp(&b.name));

    provider_patch::write_patch(&path, &provider_patch::render_providers(&raw, &routes)?)?;
    Ok(routes)
}

// ---------------------------------------------------------------------------
// Provider 验证
// ---------------------------------------------------------------------------

/// 验证某个作用域内的所有 provider routes。
#[tauri::command]
pub fn validate_providers(
    state: State<'_, AppState>,
    instance_id: String,
    profile: Option<String>,
) -> Result<ValidationReport, String> {
    let home = get_home_path(&state, &instance_id)?;
    let env_overrides = env_overrides_of(&state, &instance_id);
    let routes = read_routes(&state, &instance_id, profile.as_deref())?;

    let profile_dir = profile
        .as_deref()
        .map(|p| crate::plugins::profile_dir_pub(&home, p));
    let profile_path = profile_dir.as_deref().unwrap_or(&home);

    Ok(provider_validator::validate_routes(
        &routes,
        &home,
        profile_path,
        &env_overrides,
    ))
}

// ---------------------------------------------------------------------------
// 凭证管理
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatus {
    pub route_name: String,
    pub env_var: String,
    pub is_set: bool,
}

/// 读取环境变量的值（用于显示已配置的凭证）。
///
/// `profile` 决定凭证解析的作用域：全局作用域只查 `<DSH_HOME>`，profile 作用域
/// 还会查 `profiles/<p>/.env`。必须与 [`validate_providers`] 的解析作用域一致。
#[tauri::command]
pub fn read_credential(
    state: State<'_, AppState>,
    instance_id: String,
    profile: Option<String>,
    env_var: String,
) -> Result<Option<CredentialSource>, String> {
    let home = get_home_path(&state, &instance_id)?;
    let env_overrides = env_overrides_of(&state, &instance_id);
    let base = credential_scope_path(&home, profile.as_deref());

    credential_manager::get_credential(&home, Some(&base), &env_overrides, &env_var)
}

/// 保存凭证到 `<DSH_HOME>/.credentials.yaml`。
#[tauri::command]
pub fn save_credential(
    state: State<'_, AppState>,
    instance_id: String,
    env_var: String,
    value: String,
) -> Result<(), String> {
    let home = get_home_path(&state, &instance_id)?;
    credential_manager::set_credential(&home, &BTreeMap::new(), &env_var, &value)
}

/// 从 `<DSH_HOME>/.credentials.yaml` 删除凭证。
#[tauri::command]
pub fn delete_credential(
    state: State<'_, AppState>,
    instance_id: String,
    env_var: String,
) -> Result<(), String> {
    let home = get_home_path(&state, &instance_id)?;
    credential_manager::delete_credential(&home, &env_var)
}

/// 列出当前作用域内所有路由引用的环境变量及其状态。
#[tauri::command]
pub fn list_credential_status(
    state: State<'_, AppState>,
    instance_id: String,
    profile: Option<String>,
) -> Result<Vec<CredentialStatus>, String> {
    let home = get_home_path(&state, &instance_id)?;
    let env_overrides = env_overrides_of(&state, &instance_id);
    let routes = read_routes(&state, &instance_id, profile.as_deref())?;
    let base = credential_scope_path(&home, profile.as_deref());

    let mut statuses = Vec::new();
    for route in &routes {
        let key = route.api_key_env_str();
        if key.is_empty() {
            continue;
        }
        let is_set =
            credential_manager::get_credential(&home, Some(&base), &env_overrides, key)?.is_some();
        statuses.push(CredentialStatus {
            route_name: route.name.clone(),
            env_var: key.to_string(),
            is_set,
        });
    }
    Ok(statuses)
}
