use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, State};
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

use crate::config::{Config, DshHome, DshInstance, DshVersion};
use crate::{packforge, tasks, AppState};

const LIFETIME: Duration = Duration::from_secs(45 * 60);
const MAX_OPEN_ARCHIVES: usize = 12;
const MAX_ZIP_ENTRIES: usize = 16_384;
const MAX_UNCOMPRESSED: u64 = 512 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct PackforgeState {
    sessions: Mutex<Sessions>,
    homes: Mutex<HashMap<String, std::sync::Weak<RwLock<()>>>>,
    pub(crate) committing: Mutex<HashSet<String>>,
}

#[derive(Default)]
struct Sessions {
    archives: HashMap<String, Arc<Archive>>,
    plans: HashMap<String, InstallPlan>,
}

impl Sessions {
    fn prune(&mut self) {
        self.archives
            .retain(|_, archive| archive.created.elapsed() < LIFETIME);
        self.plans
            .retain(|_, plan| self.archives.contains_key(&plan.archive.id));
    }
}

struct Archive {
    id: String,
    path: PathBuf,
    _temp: super::TmpDir,
    sha256: String,
    size: u64,
    manifest: Value,
    version: u32,
    market_id: Option<String>,
    created: Instant,
}

#[derive(Deserialize)]
pub(crate) struct ArchiveInput {
    source: String,
    market_id: Option<String>,
    sha256: Option<String>,
    size: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct TargetInput {
    instance_id: Option<String>,
    instance_name: Option<String>,
    profile_name: Option<String>,
    wsl_distro: Option<String>,
    #[serde(default)]
    update: bool,
    #[serde(default)]
    switch_version: bool,
    previous_source: Option<String>,
    previous_sha256: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ReviewInput {
    archive_id: String,
    target: TargetInput,
    #[serde(default)]
    decisions: BTreeMap<String, String>,
    #[serde(default)]
    credentials: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    confirmed_credentials: BTreeMap<String, bool>,
}

#[derive(Deserialize)]
pub(crate) struct ApplyInput {
    plan_id: String,
    #[serde(default)]
    resolutions: BTreeMap<String, String>,
}

#[derive(Deserialize)]
pub(crate) struct ArchiveId {
    archive_id: String,
}

#[derive(Deserialize)]
pub(crate) struct InstanceId {
    instance_id: String,
}

#[derive(Deserialize)]
pub(crate) struct MarketId {
    id: String,
}

struct InstallPlan {
    archive: Arc<Archive>,
    request: TargetInput,
    route: Route,
    options: Value,
    _previous: Option<super::TmpDir>,
}

#[derive(Clone)]
struct Route {
    home: DshHome,
    instance: Option<DshInstance>,
    instance_name: String,
    profile: String,
    whole: bool,
    version: String,
    current_version: Option<String>,
    installed: Option<DshVersion>,
    shared: bool,
    source_identity: String,
}

impl Route {
    fn switch_required(&self) -> bool {
        self.current_version
            .as_ref()
            .is_some_and(|current| current != &self.version)
    }

    fn fs_home(&self) -> PathBuf {
        crate::wsl::home_fs_path(&self.home)
    }
}

fn valid_sha(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn portable_segment(name: &str) -> bool {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
    {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_lowercase();
    !matches!(
        stem.as_str(),
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
    ) && !["com", "lpt"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

fn archive_digest(path: &Path) -> Result<(String, u64), String> {
    let file = File::open(path).map_err(|e| format!("打开整合包失败：{e}"))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > super::MODPACK_MAX_BYTES as u64
    {
        return Err("整合包为空或超过 64 MiB".into());
    }
    let mut hash = Sha256::new();
    let size = std::io::copy(
        &mut file.take(super::MODPACK_MAX_BYTES as u64 + 1),
        &mut hash,
    )
    .map_err(|e| format!("读取整合包失败：{e}"))?;
    if size != metadata.len() {
        return Err("整合包在读取时发生变化".into());
    }
    Ok((format!("{:x}", hash.finalize()), size))
}

// 在交给 Node 内存解压前检查 ZIP 目录、目标名和解压总量。
fn check_zip(path: &Path) -> Result<(), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("无效的整合包 ZIP：{e}"))?;
    if archive.len() > MAX_ZIP_ENTRIES {
        return Err("整合包 ZIP 条目过多".into());
    }
    let mut paths: HashMap<String, (String, bool, bool)> = HashMap::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| format!("读取整合包 ZIP 目录失败：{e}"))?;
        let directory = entry.is_dir();
        let raw = entry.name();
        let name = if directory {
            raw.strip_suffix('/').unwrap_or(raw)
        } else {
            raw
        };
        if entry.enclosed_name().is_none()
            || !name.split('/').all(portable_segment)
            || entry
                .unix_mode()
                .is_some_and(|mode| !matches!(mode & 0o170000, 0 | 0o100000 | 0o040000))
        {
            return Err(format!("整合包含有无效路径或链接：{name}"));
        }
        total = total
            .checked_add(entry.size())
            .filter(|size| *size <= MAX_UNCOMPRESSED)
            .ok_or("整合包解压内容超过 512 MiB")?;
        let prefixes = name.match_indices('/').map(|(at, _)| (&name[..at], false));
        for (name, explicit) in prefixes.chain(std::iter::once((name, true))) {
            let key = name.to_lowercase();
            let is_dir = !explicit || directory;
            let found = paths
                .entry(key)
                .or_insert_with(|| (name.to_owned(), is_dir, false));
            if found.0 != name || found.1 != is_dir || (explicit && found.2) {
                return Err(format!("整合包包含重复或冲突路径：{name}"));
            }
            found.2 |= explicit;
        }
    }
    Ok(())
}

async fn pin_source(
    source: &str,
    expected_sha: Option<&str>,
    expected_size: Option<u64>,
) -> Result<(PathBuf, super::TmpDir, String, u64), String> {
    if expected_sha.is_some_and(|sha| !valid_sha(sha)) || expected_size == Some(0) {
        return Err("整合包摘要或大小无效".into());
    }
    let (source, temporary) = super::fetch_modpack_source(source).await?;
    let pinned = temporary.0.join("verified.pack");
    let path = pinned.clone();
    let (sha256, size) = tokio::task::spawn_blocking(move || {
        let mut input = File::open(&source).map_err(|e| format!("打开整合包失败：{e}"))?;
        if !input.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("整合包来源须为普通文件".into());
        }
        let mut out = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|e| format!("创建整合包副本失败：{e}"))?;
        std::io::copy(
            &mut (&mut input).take(super::MODPACK_MAX_BYTES as u64 + 1),
            &mut out,
        )
        .map_err(|e| format!("复制整合包失败：{e}"))?;
        out.flush().map_err(|e| e.to_string())?;
        drop(out);
        archive_digest(&path)
    })
    .await
    .map_err(|e| e.to_string())??;
    if expected_sha.is_some_and(|sha| !sha.eq_ignore_ascii_case(&sha256))
        || expected_size.is_some_and(|expected| size != expected)
    {
        return Err("整合包与预期大小或 SHA-256 不一致".into());
    }
    Ok((pinned, temporary, sha256, size))
}

fn checked_archive(state: &AppState, id: &str) -> Result<Arc<Archive>, String> {
    let mut sessions = state.packforge.sessions.lock().unwrap();
    sessions.prune();
    sessions
        .archives
        .get(id)
        .cloned()
        .ok_or_else(|| "整合包预览已失效，请重新读取归档".into())
}

fn warn_launcher(manifest: &Value) -> Vec<String> {
    const ID: &str = "dsh-launcher";
    let mut warning = Vec::new();
    let Some(launchers) = manifest.get("launchers") else {
        return warning;
    };
    if let Some(list) = launchers.as_array() {
        if !list.iter().any(|entry| entry.as_str() == Some(ID)) && !list.is_empty() {
            warning.push("此整合包未声明与 DSH Launcher 兼容，可以继续安装。".into());
        }
    } else if let Some(map) = launchers.as_object() {
        match map.get(ID) {
            Some(value)
                if value.as_bool() == Some(false)
                    || value["supported"].as_bool() == Some(false) =>
            {
                let reason = value["reason"]
                    .as_str()
                    .unwrap_or("作者标记为不支持 DSH Launcher");
                warning.push(format!("{reason}。可以继续安装。"));
            }
            Some(value) => {
                let minimum = value.as_str().or_else(|| value["minVersion"].as_str());
                if let Some(minimum) = minimum {
                    if !launcher_version_at_least(env!("CARGO_PKG_VERSION"), minimum) {
                        warning.push(format!(
                            "此包要求 DSH Launcher {minimum} 或更高版本，可以继续安装。"
                        ));
                    }
                }
            }
            None if map.values().any(|value| {
                value.as_bool() == Some(true)
                    || value.is_string()
                    || value["supported"].as_bool() == Some(true)
            }) =>
            {
                warning.push("此整合包未声明与 DSH Launcher 兼容，可以继续安装。".into());
            }
            None => {}
        }
    }
    warning
}

fn launcher_version_at_least(current: &str, minimum: &str) -> bool {
    let parts = |version: &str| {
        version
            .split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .ok()
    };
    let (Some(current), Some(minimum)) = (parts(current), parts(minimum)) else {
        return false;
    };
    let length = current.len().max(minimum.len());
    current
        .into_iter()
        .chain(std::iter::repeat(0))
        .take(length)
        .cmp(minimum.into_iter().chain(std::iter::repeat(0)).take(length))
        .is_ge()
}

#[tauri::command]
pub(crate) async fn inspect_packforge_archive(
    state: State<'_, AppState>,
    input: ArchiveInput,
) -> Result<Value, String> {
    let mut expected_sha = input.sha256;
    let mut expected_size = input.size;
    if let Some(id) = &input.market_id {
        let entry = super::fetch_modpack_market()
            .await?
            .into_iter()
            .find(|entry| &entry.id == id)
            .ok_or("市场已找不到该整合包，请刷新市场")?;
        if entry.download_url != input.source
            || expected_sha
                .as_ref()
                .is_some_and(|sha| entry.sha256.as_ref() != Some(sha))
            || expected_size.is_some_and(|size| entry.size != Some(size))
        {
            return Err("市场中的整合包已变化，请刷新后重试".into());
        }
        expected_sha = entry.sha256;
        expected_size = entry.size;
    }
    let (path, temp, sha256, size) =
        pin_source(&input.source, expected_sha.as_deref(), expected_size).await?;
    let read_path = path.clone();
    let manifest =
        tokio::task::spawn_blocking(move || match super::detect_container(&read_path)? {
            super::ModpackContainer::Dspack => {
                check_zip(&read_path)?;
                super::read_manifest_from_dspack(&read_path)
            }
            super::ModpackContainer::LegacyTgz => super::read_manifest_from_tgz(&read_path),
        })
        .await
        .map_err(|e| e.to_string())??;
    let version = manifest.manifest_version;
    if version == 6
        && input.market_id.is_some()
        && (expected_sha.is_none() || expected_size.is_none())
    {
        return Err("市场 v6 整合包缺少可校验的 SHA-256 或大小".into());
    }
    let (manifest, icons) = if version == 6 {
        let inspected = packforge::run(
            &state,
            None,
            "inspect",
            json!({"source":path, "expectedSha256":sha256,"expectedSize":size}),
        )
        .await?;
        if inspected["valid"].as_bool() != Some(true) {
            return Err(inspected["validation"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("；"));
        }
        (inspected["manifest"].clone(), inspected["icons"].clone())
    } else {
        (
            serde_json::to_value(manifest).map_err(|e| e.to_string())?,
            Value::Null,
        )
    };
    let id = uuid::Uuid::new_v4().simple().to_string();
    let archive = Arc::new(Archive {
        id: id.clone(),
        path,
        _temp: temp,
        sha256: sha256.clone(),
        size,
        manifest: manifest.clone(),
        version,
        market_id: input.market_id,
        created: Instant::now(),
    });
    let mut sessions = state.packforge.sessions.lock().unwrap();
    sessions.prune();
    if sessions.archives.len() >= MAX_OPEN_ARCHIVES {
        return Err("打开的整合包预览过多，请关闭已打开的预览".into());
    }
    sessions.archives.insert(id.clone(), archive);
    Ok(
        json!({"archive_id":id,"manifest":manifest,"sha256":sha256,"size":size,"icons":icons,"warnings":warn_launcher(&manifest)}),
    )
}

#[tauri::command]
pub(crate) fn discard_packforge_archive(state: State<'_, AppState>, input: ArchiveId) {
    let mut sessions = state.packforge.sessions.lock().unwrap();
    sessions.archives.remove(&input.archive_id);
    sessions
        .plans
        .retain(|_, plan| plan.archive.id != input.archive_id);
}

pub(super) fn pinned_legacy(
    state: &AppState,
    id: &str,
) -> Result<(PathBuf, impl Send + Sync), String> {
    let archive = checked_archive(state, id)?;
    if archive.version >= 6 {
        return Err("请用 v6 导入预览安装该整合包".into());
    }
    Ok((archive.path.clone(), archive))
}

#[tauri::command]
pub(crate) async fn get_packforge_market_detail(input: MarketId) -> Result<Value, String> {
    let (owner, repo) = input.id.split_once('.').ok_or("无效的整合包市场 ID")?;
    let valid = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    };
    if !valid(owner) || !valid(repo) || repo == "." || repo == ".." {
        return Err("无效的整合包市场 ID".into());
    }
    let raw = crate::plugins::fetch_json_pub(
        &format!(
            "https://dsh-packforge.github.io/dsh-pack-market/packs/{owner}.{repo}/manifest.json"
        ),
        1024 * 1024,
    )
    .await?;
    let mut detail = serde_json::Map::new();
    for name in [
        "manifestVersion",
        "name",
        "version",
        "type",
        "profileName",
        "defaultProfile",
        "description",
        "displayName",
        "dshVersion",
        "dshVersions",
        "launchers",
    ] {
        if let Some(value) = raw.get(name) {
            detail.insert(name.into(), value.clone());
        }
    }
    if let Some(profiles) = raw["profiles"].as_object() {
        detail.insert(
            "profiles".into(),
            Value::Object(
                profiles
                    .iter()
                    .filter(|(name, _)| portable_segment(name))
                    .map(|(name, profile)| (name.clone(), public_plugin_unit(profile)))
                    .collect(),
            ),
        );
    } else if let Value::Object(unit) = public_plugin_unit(&raw) {
        detail.extend(unit);
    }
    let mut resources = serde_json::Map::new();
    for kind in ["path.json", "provider.json", "mcp.json", "skill.json"] {
        if let Some(items) = raw["resources"][kind].as_array() {
            resources.insert(
                kind.into(),
                Value::Array(
                    items
                        .iter()
                        .filter_map(Value::as_object)
                        .map(|item| {
                            Value::Object(
                                ["id", "name", "desc", "scope", "plugin", "ref", "commit"]
                                    .into_iter()
                                    .filter_map(|key| {
                                        item.get(key).map(|value| (key.into(), value.clone()))
                                    })
                                    .collect(),
                            )
                        })
                        .collect(),
                ),
            );
        }
    }
    detail.insert("resources".into(), Value::Object(resources));
    Ok(Value::Object(detail))
}

fn public_plugin_unit(unit: &Value) -> Value {
    let bundles = unit["bundles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let dependencies: BTreeMap<_, _> = unit["dependencies"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(id, version)| version.as_str().map(|version| (id, version)))
        .collect();
    let plugins: BTreeMap<_, _> = unit["plugins"].as_object().into_iter().flatten().filter_map(|(id, item)| {
        if !dependencies.contains_key(id) && !bundles.contains(&id.as_str()) { return None; }
        let safe_url = |raw: &Value, icon: bool| raw.as_str().and_then(|text| reqwest::Url::parse(text).ok()).filter(|url|
            (url.scheme() == "https" || (!icon && url.scheme() == "http")) && url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none())
            .map(|url| url.to_string());
        let name = localized(&item["name"]);
        let description = localized(&item["desc"]);
        let source = if item["source"]["type"] == "npm" {
            safe_url(&item["source"]["registry"], false).map(|registry| json!({"type":"npm","registry":registry}))
        } else { None };
        Some((id, json!({"name":name,"desc":description,"icon":safe_url(&item["icon"], true),"source":source})))
    }).collect();
    json!({"bundles":bundles,"dependencies":dependencies,"plugins":plugins})
}

fn localized(value: &Value) -> Value {
    match value {
        Value::String(_) => value.clone(),
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(_, value)| value.is_string())
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
        _ => Value::Null,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Baseline {
    name: String,
    version: String,
    profile_name: Option<String>,
    pack_type: Option<String>,
    source_identity: Option<String>,
    archive_sha256: Option<String>,
}

fn load_baselines(home: &Path) -> Result<Vec<Baseline>, String> {
    let directory = home.join(".dsh-pack/updates/baselines");
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("读取整合包安装记录失败：{error}")),
    };
    let mut entries = entries
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    entries.sort_by_key(|entry| entry.file_name());
    entries
        .into_iter()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
                .is_some_and(valid_sha)
        })
        .map(|entry| {
            if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
                return Err("整合包安装记录不是普通文件".into());
            }
            let file = File::open(entry.path()).map_err(|e| e.to_string())?;
            serde_json::from_reader(std::io::BufReader::new(file))
                .map_err(|e| format!("整合包安装记录无效：{e}"))
        })
        .collect()
}

#[tauri::command]
pub(crate) async fn list_packforge_installations(
    state: State<'_, AppState>,
    input: InstanceId,
) -> Result<Vec<Value>, String> {
    let home = {
        let config = state.config.lock().unwrap();
        configured_home(&config, &input.instance_id)?
    };
    if let Some(distro) = &home.wsl {
        crate::wsl::ensure_distro_running(&state, distro).await?;
    }
    let home = crate::wsl::home_fs_path(&home);
    let baselines = tokio::task::spawn_blocking(move || load_baselines(&home))
        .await
        .map_err(|e| e.to_string())??;
    Ok(baselines
        .into_iter()
        .filter_map(|record| {
            let pack_type = record.pack_type?;
            let sha256 = record.archive_sha256?;
            let market_id = record
                .source_identity
                .as_deref()
                .and_then(|source| source.strip_prefix("market/"));
            Some(
                json!({"name":record.name,"version":record.version,"pack_type":pack_type,
            "profile_name":record.profile_name,"market_id":market_id,"sha256":sha256}),
            )
        })
        .collect())
}

fn configured_home(config: &Config, instance_id: &str) -> Result<DshHome, String> {
    let instance = config
        .instances
        .iter()
        .find(|item| item.id == instance_id)
        .ok_or("实例不存在")?;
    config
        .homes
        .iter()
        .find(|item| item.id == instance.home_id)
        .cloned()
        .ok_or("实例的 DSH_HOME 不存在".into())
}

fn home_key(home: &DshHome) -> String {
    let root = crate::wsl::home_fs_path(home);
    let path = root.canonicalize().unwrap_or(root);
    let path = path.to_string_lossy();
    let path = crate::tui_cwd::strip_verbatim_prefix(&path);
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.into_owned()
    }
}

fn home_lock(state: &AppState, home: &DshHome) -> Arc<RwLock<()>> {
    let key = home_key(home);
    let mut gates = state.packforge.homes.lock().unwrap();
    gates.retain(|_, gate| gate.strong_count() != 0);
    gates
        .get(&key)
        .and_then(std::sync::Weak::upgrade)
        .unwrap_or_else(|| {
            let lock = Arc::new(RwLock::new(()));
            gates.insert(key, Arc::downgrade(&lock));
            lock
        })
}

fn lock_home_update(state: &AppState, home: &DshHome) -> Result<OwnedRwLockWriteGuard<()>, String> {
    home_lock(state, home)
        .try_write_owned()
        .map_err(|_| "此 DSH_HOME 正在启动实例或执行整合包操作，请稍后重试".into())
}

pub(super) fn try_home_guard(
    state: &AppState,
    home: &DshHome,
) -> Result<OwnedRwLockReadGuard<()>, String> {
    let guard = home_lock(state, home)
        .try_read_owned()
        .map_err(|_| "此 DSH_HOME 正在执行整合包操作，请稍后重试")?;
    if pending_home_registration(state, home).is_some() || unfinalized_home(home) {
        return Err("请等待整合包事务恢复完成后再使用此 DSH_HOME".into());
    }
    Ok(guard)
}

fn pending_home_registration(state: &AppState, home: &DshHome) -> Option<(PathBuf, Registration)> {
    std::fs::read_dir(state.data_dir.join("packforge-journal"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("json")
        })
        .filter_map(|entry| {
            let path = entry.path();
            let file = File::open(&path).ok()?;
            let registration =
                serde_json::from_reader::<_, Registration>(file.take(64 * 1024)).ok()?;
            Some((path, registration))
        })
        .find(|(_, entry)| {
            entry.home.id == home.id || entry.home.path == home.path && entry.home.wsl == home.wsl
        })
}

fn unfinalized_home(home: &DshHome) -> bool {
    crate::wsl::home_fs_path(home)
        .join(".dsh-pack/updates/transaction/journal.json")
        .is_file()
}

async fn recover_if_needed(state: &AppState, home: &DshHome) -> Result<(), String> {
    if pending_home_registration(state, home).is_none() && !unfinalized_home(home) {
        return Ok(());
    }
    let _guard = lock_home_update(state, home)?;
    require_stopped(state, home).await?;
    if let Some((path, registration)) = pending_home_registration(state, home) {
        reconcile_registration(state, &path, &registration).await?;
    } else if unfinalized_home(home) {
        packforge::run(
            state,
            home.wsl.as_deref(),
            "recover",
            json!({"home":home.path}),
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn guard_instance_start(
    state: &State<'_, AppState>,
    instance_id: &str,
) -> Result<OwnedRwLockReadGuard<()>, String> {
    let home = configured_home(&state.config.lock().unwrap(), instance_id)?;
    if let Some(distro) = &home.wsl {
        crate::wsl::ensure_distro_running(state, distro).await?;
    }
    recover_if_needed(state, &home).await?;
    try_home_guard(state, &home)
}

pub(crate) fn launch_guard(
    state: &AppState,
    instance_id: &str,
) -> Result<OwnedRwLockReadGuard<()>, String> {
    let home = configured_home(&state.config.lock().unwrap(), instance_id)?;
    try_home_guard(state, &home)
}

pub(crate) fn guard_home_id(
    state: &AppState,
    home_id: &str,
) -> Result<OwnedRwLockReadGuard<()>, String> {
    let home = state
        .config
        .lock()
        .unwrap()
        .homes
        .iter()
        .find(|home| home.id == home_id)
        .cloned()
        .ok_or("DSH_HOME 不存在")?;
    try_home_guard(state, &home)
}

async fn require_stopped(state: &AppState, home: &DshHome) -> Result<(), String> {
    let ids = {
        let config = state.config.lock().unwrap();
        let key = home_key(home);
        config
            .instances
            .iter()
            .filter(|instance| {
                config
                    .homes
                    .iter()
                    .any(|item| item.id == instance.home_id && home_key(item) == key)
            })
            .map(|instance| instance.id.clone())
            .collect::<Vec<_>>()
    };
    if ids.is_empty() {
        return Ok(());
    }
    let running = state.running.lock().await;
    let tui = state.tui_sessions.lock().await;
    if ids
        .iter()
        .any(|id| running.contains_key(id) || tui.contains_key(id))
    {
        return Err("请先停止所有使用此 DSH_HOME 的实例".into());
    }
    Ok(())
}

fn version_selection(
    manifest: &Value,
    current: Option<&DshVersion>,
    installed: &[DshVersion],
) -> Result<(String, Option<DshVersion>), String> {
    let preferred = manifest["dshVersion"]
        .as_str()
        .filter(|version| !version.is_empty());
    let declared: Vec<_> = manifest["dshVersions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let allowed = |candidate: &str| {
        if declared.is_empty() {
            preferred.is_none_or(|version| version == candidate)
        } else {
            declared.contains(&candidate)
        }
    };
    let newest = installed
        .iter()
        .filter(|version| allowed(&version.version))
        .filter_map(|version| {
            semver::Version::parse(&version.version)
                .ok()
                .map(|parsed| (parsed, version))
        })
        .max_by(|(left, _), (right, _)| left.cmp_precedence(right))
        .map(|(_, version)| version);
    let selected = current
        .filter(|version| allowed(&version.version))
        .or_else(|| {
            preferred.and_then(|preferred| {
                installed
                    .iter()
                    .find(|version| version.version == preferred)
            })
        })
        .or(newest);
    if let Some(selected) = selected {
        return Ok((selected.version.clone(), Some(selected.clone())));
    }
    let version = preferred
        .map(str::to_owned)
        .or_else(|| {
            declared
                .iter()
                .filter_map(|text| semver::Version::parse(text).ok())
                .max_by(semver::Version::cmp_precedence)
                .map(|version| version.to_string())
        })
        .ok_or("整合包未声明 DSH 版本，所选环境中也没有已安装版本")?;
    if semver::Version::parse(&version).is_err() {
        return Err("v6 整合包须使用精确的 DSH 版本号".into());
    }
    Ok((version, None))
}

async fn route(
    state: &State<'_, AppState>,
    archive: &Archive,
    input: &TargetInput,
) -> Result<Route, String> {
    if archive.version != 6 {
        return Err("该操作需要 PackForge v6 整合包".into());
    }
    let whole = archive.manifest["type"] == "dshhome";
    let (config, instance, existing_home) = {
        let cfg = state.config.lock().unwrap();
        let instance = input
            .instance_id
            .as_ref()
            .map(|id| {
                cfg.instances
                    .iter()
                    .find(|item| &item.id == id)
                    .cloned()
                    .ok_or("实例不存在")
            })
            .transpose()?;
        let home = instance
            .as_ref()
            .map(|instance| configured_home(&cfg, &instance.id))
            .transpose()?;
        (cfg.clone(), instance, home)
    };
    if (input.update && instance.is_none()) || (whole && !input.update && instance.is_some()) {
        return Err("请选择正确的整合包目标实例".into());
    }
    let profile = if whole {
        archive.manifest["defaultProfile"]
            .as_str()
            .ok_or("全局目录整合包缺少默认配置档")?
    } else {
        input
            .profile_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                archive.manifest["profileName"].as_str().filter(|name| {
                    !["web", "headless"]
                        .iter()
                        .any(|reserved| name.eq_ignore_ascii_case(reserved))
                })
            })
            .unwrap_or("pack")
    }
    .trim()
    .to_owned();
    if !portable_segment(&profile)
        || ["web", "headless"]
            .iter()
            .any(|name| profile.eq_ignore_ascii_case(name))
    {
        return Err("整合包配置档名称无效或由 DSH 保留".into());
    }
    let name = input
        .instance_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| archive.manifest["displayName"].as_str())
        .or_else(|| archive.manifest["name"].as_str())
        .ok_or("整合包缺少实例名称")?;
    let instance_name = instance
        .as_ref()
        .map(|instance| instance.name.clone())
        .unwrap_or_else(|| super::dedupe_instance_name(&config, name));
    let home = match existing_home {
        Some(home) => home,
        None => {
            let suffix = &archive.id[..12];
            let slug: String = crate::config::sanitize_name(&instance_name)
                .chars()
                .take(28)
                .collect();
            let folder = format!("{}-{suffix}", if slug.is_empty() { "pack" } else { &slug });
            let wsl = input
                .wsl_distro
                .as_deref()
                .filter(|value| !value.trim().is_empty());
            let path = if let Some(distro) = wsl {
                crate::wsl::ensure_distro_running(state, distro).await?;
                crate::wsl::WslRoot::resolve(distro)
                    .await?
                    .home_dir(&folder)
                    .into()
            } else {
                state.data_dir.join("homes").join(folder)
            };
            DshHome {
                id: format!("h-pack-{suffix}"),
                name: instance_name.clone(),
                path,
                wsl: wsl.map(str::to_owned),
                links: BTreeMap::new(),
            }
        }
    };
    if let Some(distro) = &home.wsl {
        crate::wsl::ensure_distro_running(state, distro).await?;
    }
    let path = home_key(&home);
    let shared = config
        .instances
        .iter()
        .filter(|item| {
            config
                .homes
                .iter()
                .any(|candidate| candidate.id == item.home_id && home_key(candidate) == path)
        })
        .count()
        > 1;
    let current = instance
        .as_ref()
        .map(|instance| {
            config
                .versions
                .iter()
                .find(|version| version.id == instance.version_id && version.wsl == home.wsl)
                .ok_or("目标实例的 DSH 版本与运行环境不匹配")
        })
        .transpose()?;
    let versions: Vec<_> = config
        .versions
        .iter()
        .filter(|version| version.wsl == home.wsl)
        .cloned()
        .collect();
    let (version, installed) = version_selection(&archive.manifest, current, &versions)?;
    let source_identity = if input.update {
        let baselines = load_baselines(&crate::wsl::home_fs_path(&home))?;
        let record = baselines.into_iter().find(|record| {
            (whole && record.pack_type.as_deref() == Some("dshhome"))
                || (!whole
                    && record
                        .profile_name
                        .as_deref()
                        .is_some_and(|value| value.eq_ignore_ascii_case(&profile)))
        });
        if let Some(record) = record {
            if archive.manifest["name"].as_str() != Some(&record.name) {
                return Err("目标已绑定其他整合包，请选择同名同类型的安装".into());
            }
            if let Some(source) = record.source_identity {
                if let Some(market_id) = &archive.market_id {
                    if source != format!("market/{market_id}") {
                        return Err("新版整合包的市场来源与安装记录不一致".into());
                    }
                }
                source
            } else {
                archive_identity(archive)
            }
        } else {
            archive_identity(archive)
        }
    } else {
        archive_identity(archive)
    };
    Ok(Route {
        home,
        instance,
        instance_name,
        profile,
        whole,
        version,
        current_version: current.map(|version| version.version.clone()),
        installed,
        shared,
        source_identity,
    })
}

fn archive_identity(archive: &Archive) -> String {
    archive
        .market_id
        .as_ref()
        .map(|id| format!("market/{id}"))
        .unwrap_or_else(|| format!("manual/{}", archive.id))
}

fn options(archive: &Archive, route: &Route, input: &TargetInput) -> Value {
    let profiles_root = if route.home.wsl.is_some() {
        format!("{}/profiles", route.home.path.to_string_lossy())
    } else {
        route
            .home
            .path
            .join("profiles")
            .to_string_lossy()
            .into_owned()
    };
    let anchor = route.installed.as_ref().map(dsh_cli);
    let credentials = route
        .instance
        .as_ref()
        .map(|instance| &instance.env_overrides);
    let cwd = route
        .instance
        .as_ref()
        .and_then(|instance| instance.tui_cwd.as_deref());
    json!({"source":archive.path,"expectedSha256":archive.sha256,"expectedSize":archive.size,
        "profilesRoot":profiles_root,"home":route.home.path,"name":route.profile,"targetProfileName":route.profile,
        "sourceIdentity":route.source_identity,"update":input.update,"selectedDshVersion":route.version,"installedDshVersions":[route.version],
        "installAnchor":anchor,"credentialEnvironment":credentials,"credentialCwd":cwd})
}

fn dsh_cli(version: &DshVersion) -> String {
    if version.wsl.is_some() {
        let root = version.dir.to_string_lossy().replace('\\', "/");
        let name = if crate::process::is_repo_checkout(&crate::wsl::version_fs_path(version)) {
            "apps/cli/lib/bin.js"
        } else {
            "node_modules/@deepseek-ai/dsh/lib/bin.js"
        };
        format!("{}/{name}", root.trim_end_matches('/'))
    } else {
        crate::process::version_bin(&version.dir)
            .to_string_lossy()
            .into_owned()
    }
}

async fn ensure_review_node(
    app: &AppHandle,
    state: &State<'_, AppState>,
    distro: Option<&str>,
) -> Result<(), String> {
    if let Some(distro) = distro {
        let root = crate::wsl::WslRoot::resolve(distro).await?;
        if !crate::wsl::wsl_test(distro, "-x", &root.node_exe()).await {
            let found = crate::wsl::wsl_output(
                distro,
                &["node".into(), "-p".into(), "process.versions.node".into()],
            )
            .await
            .ok()
            .and_then(|version| semver::Version::parse(&version).ok());
            if !found.is_some_and(|version| version.major >= 20) {
                crate::wsl::ensure_node(app, state, "", distro, &root).await?;
            }
        }
    }
    Ok(())
}

async fn review_route(
    app: &AppHandle,
    state: &State<'_, AppState>,
    archive: &Archive,
    target: &TargetInput,
) -> Result<Route, String> {
    if let Some(instance_id) = target.instance_id.as_deref() {
        let home = configured_home(&state.config.lock().unwrap(), instance_id)?;
        if let Some(distro) = &home.wsl {
            crate::wsl::ensure_distro_running(state, distro).await?;
        }
        ensure_review_node(app, state, home.wsl.as_deref()).await?;
        recover_if_needed(state, &home).await?;
    }
    let route = route(state, archive, target).await?;
    if route.instance.is_none() {
        ensure_review_node(app, state, route.home.wsl.as_deref()).await?;
    }
    Ok(route)
}

fn check_update_target(route: &Route, target: &TargetInput) -> Result<(), String> {
    if route.switch_required() && (!target.update || route.shared || !target.switch_version) {
        return Err(format!("目标实例须先切换至兼容的 DSH {}", route.version));
    }
    if route.instance.is_none() && route.fs_home().try_exists().map_err(|e| e.to_string())? {
        return Err("新实例的目录已存在，请重新打开整合包预览".into());
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn list_packforge_resources(
    app: AppHandle,
    state: State<'_, AppState>,
    input: ReviewInput,
) -> Result<Value, String> {
    let archive = checked_archive(&state, &input.archive_id)?;
    let route = review_route(&app, &state, &archive, &input.target).await?;
    let _review = try_home_guard(&state, &route.home)?;
    let distro = route.home.wsl.as_deref();
    let preview = packforge::run(
        &state,
        distro,
        "resources",
        options(&archive, &route, &input.target),
    )
    .await?;
    Ok(
        json!({"items":preview["items"],"version":route.version,"current_version":route.current_version,
        "switch_required":route.switch_required(),"shared_home":route.shared,"warnings":warn_launcher(&archive.manifest)}),
    )
}

async fn previous_archive(
    input: &TargetInput,
    archive: &Archive,
) -> Result<Option<(PathBuf, super::TmpDir, String)>, String> {
    let Some(source) = input
        .previous_source
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    let sha = input
        .previous_sha256
        .as_deref()
        .filter(|value| valid_sha(value))
        .ok_or("请输入旧整合包的 SHA-256")?;
    if !input.update {
        return Err("旧版整合包只用于更新基线".into());
    }
    let (path, temp, mut digest, _) = pin_source(source, Some(sha), None).await?;
    let (parsed, path) = match super::detect_container(&path)? {
        super::ModpackContainer::Dspack => {
            check_zip(&path)?;
            (super::read_manifest_from_dspack(&path)?, path)
        }
        super::ModpackContainer::LegacyTgz => {
            let parsed = super::read_manifest_from_tgz(&path)?;
            let converted = temp.0.join("legacy.dspack");
            convert_legacy_baseline(&path, &converted, &parsed, &temp.0)?;
            digest = archive_digest(&converted)?.0;
            (parsed, converted)
        }
    };
    if parsed.manifest_version >= 6
        || archive.manifest["name"].as_str() != Some(&parsed.name)
        || archive.manifest["type"].as_str()
            != Some(parsed.pack_type.as_deref().unwrap_or("profile"))
    {
        return Err("旧包必须与新版整合包的名称和类型一致，并使用历史格式".into());
    }
    Ok(Some((path, temp, digest)))
}

fn convert_legacy_baseline(
    source: &Path,
    destination: &Path,
    manifest: &super::ModpackManifest,
    temp: &Path,
) -> Result<(), String> {
    let unwrapped = temp.join("legacy");
    super::extract_modpack_tgz(source, &unwrapped)?;
    let mut normalized = serde_json::to_value(manifest).map_err(|e| e.to_string())?;
    normalized["manifestVersion"] = json!(4);
    normalized["type"] = json!("profile");
    if manifest
        .dsh_version
        .as_deref()
        .is_some_and(|version| semver::Version::parse(version).is_err())
    {
        if let Some(fields) = normalized.as_object_mut() {
            fields.remove("dshVersion");
        }
    }
    if manifest.manifest_version == 2 {
        // v2 的依赖键是包名，坐标形式的旧版 Git 依赖由 PackForge 的重建过程保留 pnpm spec。
        normalized["dependencies"] =
            serde_json::to_value(super::manifest_pkg_deps(manifest)).map_err(|e| e.to_string())?;
    }
    let mut zip = zip::ZipWriter::new(File::create(destination).map_err(|e| e.to_string())?);
    let option = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("dspack.json", option)
        .map_err(|e| e.to_string())?;
    zip.write_all(br#"{"format":"dspack","version":2}"#)
        .map_err(|e| e.to_string())?;
    zip.start_file("manifest.json", option)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut zip, &normalized).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(&unwrapped).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "manifest.json" {
            continue;
        }
        let target =
            if ["package.json", "pnpm-workspace.yaml", "pnpm-lock.yaml"].contains(&name.as_str()) {
                name.clone()
            } else {
                format!("overrides/{name}")
            };
        add_baseline_entry(&mut zip, &entry.path(), &target, option)?;
    }
    zip.finish().map_err(|e| e.to_string())?;
    check_zip(destination)
}

fn add_baseline_entry(
    zip: &mut zip::ZipWriter<File>,
    path: &Path,
    name: &str,
    option: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    let leaf = name.rsplit('/').next().unwrap_or_default();
    if !portable_segment(leaf)
        || [
            ".npmrc",
            ".credentials.yaml",
            "node_modules",
            ".git",
            ".dsh-pack",
        ]
        .contains(&leaf)
        || leaf.starts_with(".env")
    {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            add_baseline_entry(
                zip,
                &entry.path(),
                &format!("{name}/{}", entry.file_name().to_string_lossy()),
                option,
            )?;
        }
    } else if metadata.is_file() {
        zip.start_file(name, option).map_err(|e| e.to_string())?;
        std::io::copy(&mut File::open(path).map_err(|e| e.to_string())?, zip)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn preview_packforge_install(
    app: AppHandle,
    state: State<'_, AppState>,
    input: ReviewInput,
) -> Result<Value, String> {
    let archive = checked_archive(&state, &input.archive_id)?;
    let route = review_route(&app, &state, &archive, &input.target).await?;
    let _review = try_home_guard(&state, &route.home)?;
    check_update_target(&route, &input.target)?;
    let mut opts = options(&archive, &route, &input.target);
    opts["decisions"] = json!(input.decisions);
    opts["credentials"] = json!(input.credentials);
    opts["confirmedCredentials"] = json!(input.confirmed_credentials);
    let previous = previous_archive(&input.target, &archive).await?;
    if let Some((path, _, digest)) = previous.as_ref() {
        opts["previousSource"] = json!(path);
        opts["previousSha256"] = json!(digest);
    }
    let preview =
        packforge::run(&state, route.home.wsl.as_deref(), "preview", opts.clone()).await?;
    let token = preview["previewToken"]
        .as_str()
        .filter(|value| valid_sha(value))
        .ok_or("PackForge 未生成可提交的预览")?;
    opts["previewToken"] = json!(token);
    let plan_id = uuid::Uuid::new_v4().simple().to_string();
    let mut sessions = state.packforge.sessions.lock().unwrap();
    sessions.prune();
    if !sessions.archives.contains_key(&archive.id) {
        return Err("整合包预览已关闭".into());
    }
    sessions
        .plans
        .retain(|_, plan| plan.archive.id != archive.id);
    sessions.plans.insert(
        plan_id.clone(),
        InstallPlan {
            archive,
            request: input.target,
            route,
            options: opts,
            _previous: previous.map(|(_, temp, _)| temp),
        },
    );
    Ok(
        json!({"plan_id":plan_id,"changes":preview["changes"],"conflicts":preview["conflicts"],"unknown_base":preview["unknownBase"]}),
    )
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct InstanceFields {
    version_id: String,
    default_profile: Option<String>,
    last_profile: Option<String>,
}

impl From<&DshInstance> for InstanceFields {
    fn from(instance: &DshInstance) -> Self {
        Self {
            version_id: instance.version_id.clone(),
            default_profile: instance.default_profile.clone(),
            last_profile: instance.last_profile.clone(),
        }
    }
}

#[derive(Deserialize, Serialize)]
struct Registration {
    transaction: String,
    baseline: PathBuf,
    instance_id: String,
    instance_name: String,
    home: DshHome,
    before: Option<InstanceFields>,
    after: InstanceFields,
    icon: Option<String>,
}

fn journal_path(state: &AppState, task: &str) -> PathBuf {
    state
        .data_dir
        .join("packforge-journal")
        .join(format!("{task}.json"))
}

fn change_registration(
    state: &AppState,
    registration: &Registration,
    commit: bool,
) -> Result<(), String> {
    let mut config = state.config.lock().unwrap();
    let mut next = config.clone();
    let instance = next
        .instances
        .iter_mut()
        .find(|instance| instance.id == registration.instance_id);
    match (&registration.before, commit, instance) {
        (None, true, None) => {
            if !next
                .versions
                .iter()
                .any(|version| version.id == registration.after.version_id)
            {
                return Err("整合包所用 DSH 版本已被移除".into());
            }
            if !next
                .homes
                .iter()
                .any(|home| home.id == registration.home.id)
            {
                next.homes.push(registration.home.clone());
            }
            next.instances.push(DshInstance {
                browser: Default::default(),
                id: registration.instance_id.clone(),
                name: registration.instance_name.clone(),
                version_id: registration.after.version_id.clone(),
                home_id: registration.home.id.clone(),
                env_overrides: BTreeMap::new(),
                default_profile: registration.after.default_profile.clone(),
                last_profile: None,
                icon: registration.icon.clone(),
                port: None,
                web_access: Default::default(),
                tui_cwd: None,
            });
        }
        (None, false, _) => {
            next.instances
                .retain(|instance| instance.id != registration.instance_id);
            if !next
                .instances
                .iter()
                .any(|instance| instance.home_id == registration.home.id)
            {
                next.homes.retain(|home| home.id != registration.home.id);
            }
        }
        (Some(before), _, Some(instance)) => {
            let current = InstanceFields::from(&*instance);
            let (expected, desired) = if commit {
                (before, &registration.after)
            } else {
                (&registration.after, before)
            };
            if &current == desired {
                return Ok(());
            }
            if &current != expected || instance.home_id != registration.home.id {
                return Err("实例已在整合包操作期间变化，请重新预览".into());
            }
            instance.version_id.clone_from(&desired.version_id);
            instance
                .default_profile
                .clone_from(&desired.default_profile);
            instance.last_profile.clone_from(&desired.last_profile);
        }
        (Some(_), true, None) => return Err("目标实例已被删除".into()),
        _ => return Ok(()),
    }
    crate::config::save_config(&state.config_path, &next)?;
    *config = next;
    Ok(())
}

fn record_registration(
    state: &AppState,
    task_id: &str,
    registration: &Registration,
) -> Result<(), String> {
    let path = journal_path(state, task_id);
    write_journal(&path, registration)
}

fn write_journal(path: &Path, value: &impl Serialize) -> Result<(), String> {
    std::fs::create_dir_all(path.parent().ok_or("缺少整合包事务记录目录")?)
        .map_err(|e| e.to_string())?;
    let temporary = path.with_extension("pending");
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer(&file, value).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}

async fn clean_fresh_home(state: &AppState, home: &DshHome) -> Result<(), String> {
    let Some(suffix) = home.id.strip_prefix("h-pack-") else {
        return Err("待恢复目录没有整合包实例标识".into());
    };
    let parent = if let Some(distro) = &home.wsl {
        PathBuf::from(crate::wsl::WslRoot::resolve(distro).await?.home_dir(""))
    } else {
        state.data_dir.join("homes")
    };
    if suffix.len() != 12
        || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        || home.path.parent() != Some(parent.as_path())
        || !home
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(&format!("-{suffix}")))
    {
        return Err("待恢复目录不属于启动器创建的整合包实例".into());
    }
    let path = crate::wsl::home_fs_path(home);
    if path.join(".dsh-pack/updates/transaction").is_dir() {
        packforge::run(
            state,
            home.wsl.as_deref(),
            "recover",
            json!({"home":home.path}),
        )
        .await?;
    }
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            crate::wsl::run_blocking(move || std::fs::remove_dir_all(path))
                .await?
                .map_err(|e| format!("清理未完成的整合包实例失败：{e}"))
        }
        Ok(_) => Err("待恢复的整合包目录已被重定向".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("读取待恢复目录失败：{error}")),
    }
}

fn publication_path(
    home: &DshHome,
    publication: &packforge::Publication,
) -> Result<PathBuf, String> {
    uuid::Uuid::parse_str(&publication.transaction).map_err(|_| "整合包事务标识无效")?;
    let path = if let Some(distro) = &home.wsl {
        if !publication.baseline.starts_with('/')
            || publication.baseline.contains("..")
            || publication.baseline.contains('\\')
        {
            return Err("整合包基线路径无效".into());
        }
        crate::wsl::unc_path(distro, &publication.baseline)
    } else {
        PathBuf::from(&publication.baseline)
    };
    let home = crate::wsl::home_fs_path(home);
    let baseline = home.join(".dsh-pack").join("updates").join("baselines");
    let provided = path.parent().ok_or("整合包基线缺少目录")?;
    if provided != baseline
        || !path
            .file_stem()
            .and_then(|name| name.to_str())
            .is_some_and(valid_sha)
        || path.extension().and_then(|extension| extension.to_str()) != Some("json")
    {
        return Err("整合包基线路径超出目标 DSH_HOME".into());
    }
    Ok(path)
}

async fn reconcile_registration(
    state: &AppState,
    path: &Path,
    registration: &Registration,
) -> Result<bool, String> {
    let home = crate::wsl::home_fs_path(&registration.home);
    #[derive(Deserialize)]
    struct CommitMark {
        transaction: Option<String>,
    }
    let transaction = match File::open(&registration.baseline) {
        Ok(file) => {
            serde_json::from_reader::<_, CommitMark>(std::io::BufReader::new(file))
                .map_err(|_| "整合包基线损坏，实例登记暂未恢复")?
                .transaction
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("检查整合包事务结果失败：{error}")),
    };
    let committed = transaction.as_deref() == Some(&registration.transaction);
    if home.join(".dsh-pack/updates/transaction").is_dir() {
        packforge::run(
            state,
            registration.home.wsl.as_deref(),
            "recover",
            json!({"home":registration.home.path}),
        )
        .await?;
    }
    change_registration(state, registration, committed)?;
    if !committed && registration.before.is_none() {
        clean_fresh_home(state, &registration.home).await?;
    }
    std::fs::remove_file(path).map_err(|e| format!("清理整合包实例登记记录失败：{e}"))?;
    Ok(committed)
}

pub(crate) async fn recover_pending(app: AppHandle) {
    let state = app.state::<AppState>();
    if let Ok(entries) = std::fs::read_dir(state.data_dir.join("packforge-journal")) {
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let path = entry.path();
            let Ok(file) = File::open(&path) else {
                continue;
            };
            let registration = serde_json::from_reader::<_, Registration>(file.take(64 * 1024));
            let result = async {
                let registration = registration.map_err(|_| "无效的实例登记记录")?;
                if let Some(distro) = &registration.home.wsl {
                    crate::wsl::ensure_distro_running(&state, distro).await?;
                }
                let _guard = lock_home_update(&state, &registration.home)?;
                reconcile_registration(&state, &path, &registration).await
            }
            .await;
            if let Err(error) = result {
                crate::log_warn!("恢复整合包实例登记失败：{error}");
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(state.data_dir.join("packforge-journal")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("home")
                || path.with_extension("json").exists()
            {
                continue;
            }
            let Ok(file) = File::open(&path) else {
                continue;
            };
            let Ok(home) = serde_json::from_reader::<_, DshHome>(file.take(64 * 1024)) else {
                continue;
            };
            let result = async {
                let _guard = lock_home_update(&state, &home)?;
                let registered = state
                    .config
                    .lock()
                    .unwrap()
                    .homes
                    .iter()
                    .any(|existing| existing.id == home.id && existing.path == home.path);
                if !registered {
                    if let Some(distro) = &home.wsl {
                        crate::wsl::ensure_distro_running(&state, distro).await?;
                    }
                    clean_fresh_home(&state, &home).await?;
                }
                std::fs::remove_file(path).map_err(|e| e.to_string())
            }
            .await;
            if let Err(error) = result {
                crate::log_warn!("恢复未登记的整合包目录失败：{error}");
            }
        }
    }
    let homes = state.config.lock().unwrap().homes.clone();
    for home in homes {
        if !crate::wsl::home_fs_path(&home)
            .join(".dsh-pack/updates/transaction")
            .is_dir()
        {
            continue;
        }
        let Ok(_guard) = lock_home_update(&state, &home) else {
            continue;
        };
        if let Err(error) = packforge::run(
            &state,
            home.wsl.as_deref(),
            "recover",
            json!({"home":home.path}),
        )
        .await
        {
            crate::log_warn!("恢复整合包文件事务失败：{error}");
        }
    }
}

async fn obtain_version(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task: &str,
    route: &Route,
) -> Result<DshVersion, String> {
    if let Some(version) = &route.installed {
        return Ok(version.clone());
    }
    if let Some(distro) = &route.home.wsl {
        tasks::install_version_streamed_wsl(app, state, task, &route.version, distro).await
    } else {
        tasks::install_version_streamed_pub(app, state, task, &route.version).await
    }
}

async fn configure_worker(
    app: &AppHandle,
    state: &State<'_, AppState>,
    id: &str,
    route: &Route,
    options: &mut Value,
) -> Result<(), String> {
    let (pnpm, store) = if let Some(distro) = &route.home.wsl {
        let root = crate::wsl::WslRoot::resolve(distro).await?;
        crate::wsl::ensure_node(app, state, id, distro, &root).await?;
        (
            crate::wsl::ensure_pnpm(app, state, id, distro, &root).await?,
            root.pnpm_store(),
        )
    } else {
        let pnpm = tasks::ensure_packforge_pnpm(app, state, id).await?;
        (
            pnpm.to_string_lossy().into_owned(),
            state
                .data_dir
                .join(".pnpm-store")
                .to_string_lossy()
                .into_owned(),
        )
    };
    options["pnpm"] = json!(pnpm);
    let mut proxy = Vec::new();
    crate::proxy::override_env(&mut proxy, &state.config.lock().unwrap().settings)?;
    let mut env: BTreeMap<String, String> = proxy.into_iter().collect();
    env.insert("PNPM_CONFIG_STORE_DIR".into(), store);
    env.insert("CI".into(), "true".into());
    options["pnpmEnv"] = json!(env);
    if let Ok(registry) = std::env::var("DSH_NPM_REGISTRY") {
        if !registry.trim().is_empty() {
            options["registry"] = json!(registry.trim());
        }
    }
    Ok(())
}

#[derive(Clone)]
struct InstancePublication {
    task_id: String,
    route: Route,
    version: DshVersion,
    instance_id: String,
    icon: Option<String>,
    update: bool,
}

async fn populate_template_profiles(
    app: &AppHandle,
    state: &State<'_, AppState>,
    request: &InstancePublication,
) {
    let home = &request.route.home;
    let bin = dsh_cli(&request.version);
    let filesystem = request.route.fs_home().join("profiles");
    for profile in ["web", "headless"] {
        if filesystem.join(profile).join("package.json").is_file() {
            continue;
        }
        let mut command = if let Some(distro) = &home.wsl {
            let Ok(root) = crate::wsl::WslRoot::resolve(distro).await else {
                continue;
            };
            crate::wsl::wsl_cmd(
                distro,
                &[
                    "/usr/bin/env".into(),
                    "-u".into(),
                    "NODE_OPTIONS".into(),
                    "-u".into(),
                    "NODE_PATH".into(),
                    format!("DSH_HOME={}", home.path.display()),
                    root.node_exe(),
                    bin.clone(),
                    "--profile".into(),
                    profile.into(),
                    "--dump-default-config".into(),
                ],
            )
        } else {
            let mut command = tokio::process::Command::new(crate::process::node());
            crate::process::hide_console(&mut command);
            command
                .arg(&bin)
                .args(["--profile", profile, "--dump-default-config"])
                .env("DSH_HOME", &home.path)
                .env_remove("NODE_OPTIONS")
                .env_remove("NODE_PATH");
            command
        };
        command
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let _ = tokio::time::timeout(Duration::from_secs(20), command.status()).await;
    }
    let template = filesystem.join("__temp__");
    if !template.exists() && filesystem.join("web/package.json").is_file() {
        let web = filesystem.join("web");
        if crate::wsl::run_blocking(move || crate::commands::copy_dir_recursive(&web, &template))
            .await
            .is_ok_and(|result| result.is_ok())
        {
            return;
        }
    } else if template.is_dir() {
        return;
    }
    tasks::push_task_log_pub(
        app,
        state,
        &request.task_id,
        "已安装整合包，系统配置档模板未生成，可由此版本的 DSH 初始化",
    )
    .await;
}

async fn publish_instance(
    app: AppHandle,
    request: InstancePublication,
    publication: packforge::Publication,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    // 只在专属的新目录补齐系统模板。读取配置不会启动 Web 服务。
    if request.route.instance.is_none() {
        populate_template_profiles(&app, &state, &request).await;
    }
    let InstancePublication {
        task_id,
        route,
        version,
        instance_id,
        icon,
        update,
    } = request;
    {
        let tasks = state.tasks.lock().await;
        if !tasks
            .get(&task_id)
            .is_some_and(|task| task.state == tasks::TaskState::Running)
        {
            return Err("整合包任务已取消".into());
        }
        state
            .packforge
            .committing
            .lock()
            .unwrap()
            .insert(task_id.clone());
    }
    let baseline = publication_path(&route.home, &publication)?;
    let before = route.instance.as_ref().map(InstanceFields::from);
    let keep = |name: &Option<String>| {
        name.as_ref()
            .filter(|name| {
                portable_segment(name)
                    && route
                        .fs_home()
                        .join("profiles")
                        .join(name)
                        .join("package.json")
                        .is_file()
            })
            .cloned()
    };
    let after = InstanceFields {
        version_id: version.id.clone(),
        default_profile: if update {
            before.as_ref().and_then(|instance| {
                if instance.default_profile.is_some() {
                    keep(&instance.default_profile)
                } else {
                    keep(&instance.last_profile)
                }
            })
        } else {
            None
        }
        .or_else(|| Some(route.profile.clone())),
        last_profile: if update {
            before
                .as_ref()
                .and_then(|instance| keep(&instance.last_profile))
        } else {
            None
        },
    };
    let registration = Registration {
        transaction: publication.transaction,
        baseline,
        instance_id,
        instance_name: route.instance_name.clone(),
        home: route.home.clone(),
        before,
        after,
        icon,
    };
    record_registration(&state, &task_id, &registration)?;
    change_registration(&state, &registration, true)
}

async fn apply_plan(
    app: &AppHandle,
    state: &State<'_, AppState>,
    id: &str,
    mut plan: InstallPlan,
    resolutions: BTreeMap<String, String>,
) -> Result<String, String> {
    let home = &plan.route.home;
    let _home_guard = lock_home_update(state, home)?;
    require_stopped(state, home).await?;
    let fresh = route(state, &plan.archive, &plan.request).await?;
    if fresh.home.id != home.id
        || fresh.home.path != home.path
        || fresh.home.wsl != home.wsl
        || fresh.profile != plan.route.profile
        || fresh.version != plan.route.version
        || fresh.source_identity != plan.route.source_identity
        || fresh.instance.as_ref().map(InstanceFields::from)
            != plan.route.instance.as_ref().map(InstanceFields::from)
        || fresh
            .instance
            .as_ref()
            .map(|instance| (&instance.env_overrides, &instance.tui_cwd))
            != plan
                .route
                .instance
                .as_ref()
                .map(|instance| (&instance.env_overrides, &instance.tui_cwd))
    {
        return Err("整合包目标在预览后发生变化，请重新预览".into());
    }
    check_update_target(&fresh, &plan.request)?;
    let version = obtain_version(app, state, id, &fresh).await?;
    let _version_guard = tasks::try_lock_version_for_launch(state, &version).await?;
    let profiles = fresh.fs_home().join("profiles");
    let mut directories = if fresh.whole {
        std::fs::read_dir(&profiles)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|child| child.path())
            .collect::<Vec<_>>()
    } else {
        vec![profiles.join(&fresh.profile)]
    };
    if fresh.whole {
        if let Some(units) = plan.archive.manifest["profiles"].as_object() {
            directories.extend(units.keys().map(|name| profiles.join(name)));
        }
    }
    directories.sort();
    directories.dedup();
    let mut guards = Vec::new();
    for path in directories {
        guards.push(
            crate::plugins::profile_lock(state, &path)
                .await
                .try_lock_owned()
                .map_err(|_| "目标配置档有插件任务执行中，请稍后重新预览")?,
        );
    }
    let opts = &mut plan.options;
    let installed_before_preview = plan.route.installed.is_some();
    if installed_before_preview {
        opts["installAnchor"] = json!(dsh_cli(&version));
    }
    // 尚未安装 DSH 时的预览遵循离线规则，提交前用真实运行时检查生效配置。
    opts["finalInstallAnchor"] = json!(dsh_cli(&version));
    opts["installedDshVersions"] = json!([version.version]);
    opts["resolutions"] = json!(resolutions);
    configure_worker(app, state, id, &fresh, opts).await?;
    if !state
        .tasks
        .lock()
        .await
        .get(id)
        .is_some_and(|task| task.state == tasks::TaskState::Running)
    {
        return Err("整合包任务已取消".into());
    }
    let instance_id = fresh
        .instance
        .as_ref()
        .map(|instance| instance.id.clone())
        .unwrap_or_else(|| crate::config::new_id("i"));
    let task_app = app.clone();
    let captured = InstancePublication {
        task_id: id.to_owned(),
        route: fresh.clone(),
        version,
        instance_id: instance_id.clone(),
        icon: plan.archive.manifest["icon"]
            .as_str()
            .filter(|url| url.starts_with("https://"))
            .map(str::to_owned),
        update: plan.request.update,
    };
    let mut publish = move |publication| -> packforge::PublishResult {
        Box::pin(publish_instance(
            task_app.clone(),
            captured.clone(),
            publication,
        ))
    };
    let job = packforge::Job {
        app,
        id,
        publish: &mut publish,
    };
    let path = journal_path(state, id);
    let intent = (fresh.instance.is_none()).then(|| path.with_extension("home"));
    if let Some(intent) = &intent {
        write_journal(intent, &fresh.home)?;
    }
    let result = packforge::run_job(
        state,
        fresh.home.wsl.as_deref(),
        "apply",
        plan.options,
        Some(job),
    )
    .await;
    if let Ok(file) = File::open(&path) {
        let registration: Registration = serde_json::from_reader(file.take(64 * 1024))
            .map_err(|_| "无法读取整合包实例登记记录")?;
        if reconcile_registration(state, &path, &registration).await? {
            if let Some(intent) = intent {
                let _ = std::fs::remove_file(intent);
            }
            return Ok(instance_id);
        }
    } else {
        if fresh
            .fs_home()
            .join(".dsh-pack/updates/transaction")
            .is_dir()
        {
            packforge::run(
                state,
                fresh.home.wsl.as_deref(),
                "recover",
                json!({"home":fresh.home.path}),
            )
            .await?;
        }
        if fresh.instance.is_none() {
            clean_fresh_home(state, &fresh.home).await?;
        }
    }
    if let Some(intent) = intent {
        let _ = std::fs::remove_file(intent);
    }
    result.and(Err("整合包事务未提交".into()))
}

#[tauri::command]
pub(crate) async fn start_packforge_install(
    app: AppHandle,
    state: State<'_, AppState>,
    input: ApplyInput,
) -> Result<String, String> {
    if input.resolutions.values().any(|value| value != "upstream") {
        return Err("无效的整合包冲突处理选项".into());
    }
    let plan = {
        let mut sessions = state.packforge.sessions.lock().unwrap();
        sessions.prune();
        sessions
            .plans
            .remove(&input.plan_id)
            .ok_or("更新预览已失效，请重新预览")?
    };
    let id = crate::config::new_id("t");
    let kind = if plan.request.update {
        "update-modpack"
    } else {
        "import-modpack"
    };
    let label = if plan.request.update {
        "更新整合包"
    } else {
        "导入整合包"
    };
    let name = plan.archive.manifest["name"].as_str().unwrap_or_default();
    state.tasks.lock().await.insert(
        id.clone(),
        tasks::TaskInfo {
            id: id.clone(),
            kind: kind.into(),
            label: format!("{label} {name}"),
            version: plan.route.version.clone(),
            state: tasks::TaskState::Running,
            percent: 0,
            created_at: tasks::now_millis_pub(),
            message: None,
            instance_id: None,
            instance_name: Some(plan.route.instance_name.clone()),
            version_id: None,
            reserved_home_path: None,
            dedicated_home_name: None,
            logs: Vec::new(),
            child: None,
        },
    );
    tasks::emit_progress_pub(&app, &id, tasks::TaskState::Running, 0, None, None);
    let worker_id = id.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        for warning in warn_launcher(&plan.archive.manifest) {
            tasks::push_task_log_pub(&app, &state, &worker_id, &warning).await;
        }
        let result = apply_plan(&app, &state, &worker_id, plan, input.resolutions).await;
        let mut tasks_map = state.tasks.lock().await;
        state
            .packforge
            .committing
            .lock()
            .unwrap()
            .remove(&worker_id);
        if let Some(task) = tasks_map.get_mut(&worker_id) {
            if task.state == tasks::TaskState::Cancelled {
                return;
            }
            match result {
                Ok(instance_id) => {
                    task.state = tasks::TaskState::Done;
                    task.percent = 100;
                    task.instance_id = Some(instance_id);
                    task.message = Some("整合包已应用".into());
                }
                Err(error) => {
                    tasks::push_log_locked_pub(task, &error);
                    task.state = tasks::TaskState::Error;
                    task.message = Some(error);
                }
            }
            tasks::emit_progress_pub(
                &app,
                &worker_id,
                task.state.clone(),
                task.percent,
                task.message.clone(),
                task.instance_id.clone(),
            );
        }
    });
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(version: &str) -> DshVersion {
        DshVersion {
            id: version.into(),
            version: version.into(),
            dir: PathBuf::new(),
            wsl: None,
        }
    }

    #[test]
    fn selects_only_exact_versions_and_keeps_compatible_existing_runtime() {
        let manifest = json!({"dshVersion":"0.2.3-alpha.2","dshVersions":["0.2.3-alpha.2","0.2.3-alpha.10","0.2.3"]});
        let versions = [
            installed("0.2.3-alpha.2"),
            installed("0.2.3-alpha.10"),
            installed("0.2.3"),
            installed("0.2.4"),
        ];
        assert_eq!(
            version_selection(&manifest, None, &versions).unwrap().0,
            "0.2.3-alpha.2"
        );
        assert_eq!(
            version_selection(&manifest, None, &versions[1..])
                .unwrap()
                .0,
            "0.2.3"
        );
        assert_eq!(
            version_selection(&manifest, Some(&versions[1]), &versions)
                .unwrap()
                .0,
            "0.2.3-alpha.10"
        );
        assert!(
            version_selection(&manifest, Some(&versions[3]), &versions[3..])
                .unwrap()
                .1
                .is_none()
        );
        assert_eq!(
            version_selection(
                &json!({"dshVersions":["0.2.3-alpha.2","0.2.3-alpha.10"]}),
                None,
                &[]
            )
            .unwrap()
            .0,
            "0.2.3-alpha.10"
        );
        assert!(version_selection(&json!({}), None, &[]).is_err());
    }

    #[test]
    fn rejects_case_aliases_and_file_directory_collisions_before_unpacking() {
        let dir = super::super::TmpDir(
            std::env::temp_dir().join(format!("packforge-zip-test-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&dir.0).unwrap();
        for (index, (first, second)) in [
            ("home/data/Foo", "home/Data/bar"),
            ("home/file", "home/file/sub"),
            ("home/file", "home/../overrides"),
            ("home/CON.txt", "home/bar"),
        ]
        .into_iter()
        .enumerate()
        {
            let path = dir.0.join(format!("{index}.dspack"));
            super::super::write_modpack_dspack(
                &path,
                &[(first.into(), vec![1]), (second.into(), vec![2])],
            )
            .unwrap();
            assert!(check_zip(&path).is_err());
        }
        let path = dir.0.join("safe.dspack");
        super::super::write_modpack_dspack(
            &path,
            &[
                ("home/技能/foo".into(), vec![1]),
                ("home/技能/bar".into(), vec![2]),
            ],
        )
        .unwrap();
        check_zip(&path).unwrap();
    }
}
