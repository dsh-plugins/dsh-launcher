use crate::{config, cua_helper, AppState};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tauri::{
    ipc::{InvokeBody, Request},
    State,
};

#[derive(Serialize)]
pub(crate) struct CuaSettings {
    auto_complete: bool,
    root: PathBuf,
    default_root: PathBuf,
    target: Option<String>,
}

fn settings(state: &AppState, target: Option<cua_helper::Target>) -> CuaSettings {
    let config = state.config.lock().unwrap();
    CuaSettings {
        auto_complete: config.settings.cua_auto_complete,
        root: cua_helper::root(&state.data_dir, &config.settings),
        default_root: state.data_dir.join("cua"),
        target: target.map(cua_helper::Target::name),
    }
}

#[tauri::command]
pub(crate) async fn get_cua_settings(state: State<'_, AppState>) -> Result<CuaSettings, String> {
    Ok(settings(&state, cua_helper::target_for_node().await?))
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SettingsPatch {
    auto_complete: Option<bool>,
    #[serde(deserialize_with = "present")]
    root: Option<Option<String>>,
}

fn present<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(de).map(Some)
}

#[tauri::command]
pub(crate) async fn update_cua_settings(
    state: State<'_, AppState>,
    request: Request<'_>,
) -> Result<CuaSettings, String> {
    let patch: SettingsPatch = match request.body() {
        InvokeBody::Json(json) => {
            serde_json::from_value(json.clone()).map_err(|e| format!("CUA 设置参数无效：{e}"))?
        }
        InvokeBody::Raw(_) => return Err("CUA 设置需要 JSON 参数".into()),
    };
    let target = cua_helper::target_for_node().await?;
    // 自动补全开关独立于下载，目录更新和助手写入共用同一把锁。
    let _operation = if patch.root.is_some() {
        Some(state.cua_operation.lock().await)
    } else {
        None
    };
    let new_root = if let Some(input) = patch.root {
        let configured = input
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        let old = cua_helper::root(&state.data_dir, &state.config.lock().unwrap().settings);
        let new = configured
            .clone()
            .unwrap_or_else(|| state.data_dir.join("cua"));
        let configured = tokio::task::spawn_blocking(move || {
            cua_helper::copy_root(&old, &new)?;
            Ok::<_, String>(configured)
        })
        .await
        .map_err(|e| e.to_string())??;
        Some(configured)
    } else {
        None
    };
    {
        let mut config = state.config.lock().unwrap();
        let mut next = config.clone();
        if let Some(enabled) = patch.auto_complete {
            next.settings.cua_auto_complete = enabled;
        }
        if let Some(root) = new_root {
            next.settings.cua_root = root;
        }
        config::save_config(&state.config_path, &next)?;
        *config = next;
    }
    Ok(settings(&state, target))
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    state: String,
}

#[derive(Serialize)]
pub(crate) struct CuaRelease {
    version: String,
    prerelease: bool,
    installed: bool,
}

fn matching_release(release: &Release, target: cua_helper::Target) -> Option<Version> {
    let version = release.tag_name.strip_prefix("dsh-v")?;
    if release.draft || !cua_helper::release_version(version) {
        return None;
    }
    let (_, archive) = cua_helper::release_asset(version, target);
    let has_asset = |name: &str| {
        release
            .assets
            .iter()
            .any(|asset| asset.name == name && asset.state == "uploaded")
    };
    (has_asset(&archive) && has_asset("SHA256SUMS"))
        .then(|| Version::parse(version).expect("CUA 发布版本已经校验"))
}

fn mark_installed(
    root: &Path,
    versions: BTreeMap<Version, bool>,
    target: cua_helper::Target,
) -> Vec<CuaRelease> {
    versions
        .into_iter()
        .rev()
        .map(|(version, prerelease)| {
            let version = version.to_string();
            CuaRelease {
                installed: cua_helper::installed(root, &version, target),
                version,
                prerelease,
            }
        })
        .collect()
}

#[tauri::command]
pub(crate) async fn list_cua_releases(
    state: State<'_, AppState>,
) -> Result<Vec<CuaRelease>, String> {
    let Some(target) = cua_helper::target_for_node().await? else {
        return Ok(Vec::new());
    };
    const PAGE_SIZE: usize = 100;
    let mut versions = BTreeMap::new();
    for page in 1.. {
        let url = crate::plugins::github_api_url(&format!(
            "/repos/dsh-plugins/dsh-cua-helper/releases?per_page={PAGE_SIZE}&page={page}"
        ));
        let response = crate::plugins::fetch_json_pub(&url, 16 * 1024 * 1024).await?;
        let releases: Vec<Release> =
            serde_json::from_value(response).map_err(|e| format!("无法读取 CUA 发布列表：{e}"))?;
        for release in &releases {
            if let Some(version) = matching_release(release, target) {
                let prerelease = release.prerelease || !version.pre.is_empty();
                versions.insert(version, prerelease);
            }
        }
        if releases.len() < PAGE_SIZE {
            break;
        }
    }
    let root = cua_helper::root(&state.data_dir, &state.config.lock().unwrap().settings);
    tokio::task::spawn_blocking(move || mark_installed(&root, versions, target))
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_directory_is_distinct_from_restoring_the_default() {
        let toggle: SettingsPatch = serde_json::from_str(r#"{"autoComplete":false}"#).unwrap();
        assert_eq!(toggle.auto_complete, Some(false));
        assert_eq!(toggle.root, None);
        let restore: SettingsPatch = serde_json::from_str(r#"{"root":null}"#).unwrap();
        assert_eq!(restore.root, Some(None));
    }

    #[test]
    fn releases_require_an_uploaded_archive_for_the_exact_platform() {
        let mut release: Release = serde_json::from_str(
            r#"{
            "tag_name":"dsh-v0.2.3-alpha.2",
            "draft":false,
            "prerelease":true,
            "assets":[
              {"name":"cua-helper-dsh-v0.2.3-alpha.2-win-x64.zip","state":"uploaded"},
              {"name":"SHA256SUMS","state":"uploaded"}
            ]
        }"#,
        )
        .unwrap();
        let win = cua_helper::Target::from_name("win-x64").unwrap();
        let mac = cua_helper::Target::from_name("darwin-arm64").unwrap();
        assert!(matching_release(&release, win).is_some());
        assert!(matching_release(&release, mac).is_none());
        release.assets[1].state = "new".into();
        assert!(matching_release(&release, win).is_none());
        release.assets[1].state = "uploaded".into();
        release.draft = true;
        assert!(matching_release(&release, win).is_none());
    }
}
