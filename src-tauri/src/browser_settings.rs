use crate::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use tauri::{AppHandle, Manager, State};

#[derive(Clone, Copy, Default, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Storage {
    #[default]
    Isolated,
    Session,
    Shared,
}

#[derive(Clone, Copy, Default, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    Direct,
    #[default]
    Launcher,
    Custom,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct BrowserProxy {
    pub mode: ProxyMode,
    pub url: String,
    pub port: u16,
    pub no_proxy: String,
    pub username: String,
}

impl Default for BrowserProxy {
    fn default() -> Self {
        Self {
            mode: ProxyMode::Launcher,
            url: "http://127.0.0.1".into(),
            port: 7890,
            no_proxy: "127.0.0.1,localhost,::1".into(),
            username: String::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct BrowserSettings {
    pub storage: Storage,
    pub user_agent: String,
    pub proxy: BrowserProxy,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct BrowserOverrides {
    pub storage: Option<Storage>,
    pub user_agent: Option<String>,
    pub proxy: Option<BrowserProxy>,
}

impl BrowserOverrides {
    fn resolve(&self, global: &BrowserSettings) -> BrowserSettings {
        let storage = self.storage.unwrap_or(global.storage);
        BrowserSettings {
            storage,
            user_agent: self
                .user_agent
                .as_ref()
                .unwrap_or(&global.user_agent)
                .clone(),
            proxy: if storage == Storage::Shared {
                global.proxy.clone()
            } else {
                self.proxy.clone().unwrap_or_else(|| global.proxy.clone())
            },
        }
    }
}

pub(crate) struct PreviewConfig {
    pub data_dir: PathBuf,
    pub user_agent: Option<String>,
    pub network: Arc<crate::browser_proxy::BrowserNetwork>,
}

#[derive(Default)]
struct Stores {
    sessions: HashMap<String, (String, File)>,
    networks: HashMap<String, Arc<crate::browser_proxy::BrowserNetwork>>,
}

static STORES: LazyLock<Mutex<Stores>> = LazyLock::new(Default::default);

/// 最后一个标签释放网络配置后，下一次打开才读取新代理。
pub(crate) fn preview_config(app: &AppHandle, instance_id: &str) -> Result<PreviewConfig, String> {
    let state = app.state::<AppState>();
    let (global, overrides) = {
        let cfg = state.config.lock().unwrap();
        let instance = cfg
            .instances
            .iter()
            .find(|i| i.id == instance_id)
            .ok_or("实例不存在")?;
        (cfg.settings.clone(), instance.browser.clone())
    };
    let effective = overrides.resolve(&global.browser);
    let data_root = state.data_dir.join("sidebar-browser");
    std::fs::create_dir_all(&data_root)
        .map_err(|error| format!("创建侧边栏数据目录失败: {error}"))?;
    let mut stores = STORES.lock().unwrap();
    let store_key = match effective.storage {
        Storage::Shared => "shared".into(),
        Storage::Isolated => format!("instance-{}", storage_hash(instance_id)),
        Storage::Session => {
            use std::collections::hash_map::Entry;
            match stores.sessions.entry(instance_id.to_owned()) {
                Entry::Occupied(entry) => entry.get().0.clone(),
                Entry::Vacant(entry) => {
                    let key = format!("run-{}", uuid::Uuid::new_v4());
                    let lock = lock_session(&data_root.join(&key))
                        .map_err(|error| format!("锁定临时侧边栏数据失败: {error}"))?;
                    entry.insert((key.clone(), lock));
                    key
                }
            }
        }
    };
    let network = match stores.networks.get(&store_key) {
        Some(network) if Arc::strong_count(network) > 1 => network.clone(),
        previous => {
            let instance_proxy = effective.storage != Storage::Shared && overrides.proxy.is_some();
            let connection = match effective.proxy.mode {
                ProxyMode::Direct => None,
                ProxyMode::Launcher => crate::proxy::launcher_route(&global)?,
                ProxyMode::Custom => {
                    let scope = if instance_proxy {
                        instance_scope(instance_id)
                    } else {
                        "browser".into()
                    };
                    let options = &effective.proxy;
                    Some(crate::proxy::Route::load(
                        &options.url,
                        options.port,
                        &options.no_proxy,
                        &options.username,
                        &scope,
                    )?)
                }
            };
            if let Some(network) = previous {
                network.reconfigure(connection);
                network.clone()
            } else {
                let network = Arc::new(crate::browser_proxy::BrowserNetwork::start(connection)?);
                stores.networks.insert(store_key.clone(), network.clone());
                network
            }
        }
    };
    let data_dir = data_root.join(&store_key);
    std::fs::create_dir_all(&data_dir).map_err(|e| format!("创建侧边栏数据目录失败: {e}"))?;
    Ok(PreviewConfig {
        data_dir,
        network,
        user_agent: (!effective.user_agent.is_empty()).then_some(effective.user_agent),
    })
}

fn storage_hash(value: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn instance_scope(id: &str) -> String {
    format!("instance-{}", storage_hash(id))
}

fn lock_session(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))
        .map_err(|error| error.to_string())?;
    file.try_lock().map_err(|error| error.to_string())?;
    Ok(file)
}

fn clean_session(app: &AppHandle, path: &Path, lock: File) -> Result<(), String> {
    crate::native_browser::retire_store(app, path)?;
    if let Err(error) = std::fs::remove_dir_all(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.to_string());
        }
    }
    drop(lock);
    std::fs::remove_file(path.with_extension("lock")).map_err(|error| error.to_string())
}

pub(crate) fn end_session(app: &AppHandle, instance_id: &str) {
    let session = {
        let mut stores = STORES.lock().unwrap();
        let session = stores.sessions.remove(instance_id);
        if let Some((key, _)) = &session {
            stores.networks.remove(key);
        }
        session
    };
    if let Some((key, lock)) = session {
        let path = app
            .state::<AppState>()
            .data_dir
            .join("sidebar-browser")
            .join(key);
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            if let Err(error) = clean_session(&app, &path, lock) {
                crate::log_warn!("清理本次运行的侧边栏数据失败: {error}");
            }
        });
    }
}

/// 使用目录对应的排他锁跳过其他启动器进程仍在使用的临时数据。
pub(crate) fn clean_abandoned_sessions(app: &AppHandle, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir.join("sidebar-browser")) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let valid = name
            .to_str()
            .and_then(|s| s.strip_prefix("run-"))
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok());
        if !valid || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        if let Ok(lock) = lock_session(&path) {
            if let Err(error) = clean_session(app, &path, lock) {
                crate::log_warn!("清理上次运行的侧边栏数据失败: {error}");
            }
        }
    }
}

pub(crate) fn remove_instance(app: &AppHandle, id: &str) -> Result<(), String> {
    let key = instance_scope(id);
    STORES.lock().unwrap().networks.remove(&key);
    let path = app
        .state::<AppState>()
        .data_dir
        .join("sidebar-browser")
        .join(&key);
    if path.is_dir() {
        crate::native_browser::retire_store(app, &path)?;
        std::fs::remove_dir_all(path).map_err(|e| format!("删除实例的侧边栏数据失败: {e}"))?;
    }
    crate::proxy::forget_password(&key)
}

fn credential_scope(
    scope: &str,
    instance_id: Option<&str>,
    config: &crate::config::Config,
) -> Result<String, String> {
    match (scope, instance_id) {
        ("launcher" | "browser", None) => Ok(scope.to_owned()),
        ("instance", Some(id)) if config.instances.iter().any(|i| i.id == id) => {
            Ok(instance_scope(id))
        }
        _ => Err("代理凭据范围无效".into()),
    }
}

#[tauri::command]
pub(crate) fn get_proxy_password_status(
    webview: tauri::Webview,
    state: State<'_, AppState>,
    scope: String,
    instance_id: Option<String>,
) -> Result<bool, String> {
    if webview.label() != "main" {
        return Err("仅启动器设置可读取代理凭据状态".into());
    }
    let cfg = state.config.lock().unwrap();
    let account = credential_scope(&scope, instance_id.as_deref(), &cfg)?;
    let custom = match instance_id.as_deref() {
        Some(id) => cfg
            .instances
            .iter()
            .find(|i| i.id == id)
            .and_then(|i| i.browser.proxy.as_ref()),
        None => Some(&cfg.settings.browser.proxy),
    };
    let (url, port, user) = if scope == "launcher" {
        (
            &cfg.settings.proxy_url,
            cfg.settings.proxy_port,
            &cfg.settings.proxy_username,
        )
    } else if let Some(custom) = custom {
        (&custom.url, custom.port, &custom.username)
    } else {
        return Ok(false);
    };
    if user.is_empty() {
        return Ok(false);
    }
    let endpoint = crate::proxy::validate_address(url, port)?;
    Ok(crate::proxy::password(&account, &endpoint, user)?.is_some())
}

#[tauri::command]
pub(crate) fn save_browser_settings(
    webview: tauri::Webview,
    state: State<'_, AppState>,
    instance_id: Option<String>,
    browser: BrowserOverrides,
    password: Option<String>,
) -> Result<(), String> {
    if webview.label() != "main" {
        return Err("仅启动器设置可修改浏览器配置".into());
    }
    if browser
        .user_agent
        .as_ref()
        .is_some_and(|ua| ua.chars().any(char::is_control))
    {
        return Err("UA 不能包含控制字符".into());
    }
    let mut guard = state.config.lock().unwrap();
    let mut next = guard.clone();
    let scope;
    let mut password = password;
    let mut browser = browser;
    if let Some(id) = instance_id {
        let global = &next.settings.browser;
        let instance = next
            .instances
            .iter_mut()
            .find(|i| i.id == id)
            .ok_or("实例不存在")?;
        if browser.storage.unwrap_or(global.storage) == Storage::Shared {
            browser.proxy = instance.browser.proxy.clone();
            password = None;
        }
        scope = instance_scope(&id);
        instance.browser = browser.clone();
    } else {
        scope = "browser".into();
        next.settings.browser = browser.resolve_global();
    }
    if let Some(proxy) = &browser.proxy {
        crate::proxy::validate_address(&proxy.url, proxy.port)?;
    }
    if let Some(custom) = &browser.proxy {
        crate::proxy::with_password(
            &scope,
            &custom.url,
            custom.port,
            &custom.username,
            password.as_deref(),
            || crate::commands::save_state(&state, &next),
        )?;
    } else {
        crate::commands::save_state(&state, &next)?;
    }
    *guard = next;
    Ok(())
}

impl BrowserOverrides {
    fn resolve_global(&self) -> BrowserSettings {
        BrowserSettings {
            storage: self.storage.unwrap_or_default(),
            user_agent: self.user_agent.clone().unwrap_or_default(),
            proxy: self.proxy.clone().unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_shared_storage_overrides_only_the_instance_proxy() {
        let mut global = BrowserSettings {
            storage: Storage::Shared,
            ..Default::default()
        };
        let overrides = BrowserOverrides {
            user_agent: Some("instance-agent".into()),
            proxy: Some(BrowserProxy {
                mode: ProxyMode::Direct,
                ..Default::default()
            }),
            ..Default::default()
        };
        let shared = overrides.resolve(&global);
        assert_eq!(shared.proxy.mode, ProxyMode::Launcher);
        assert_eq!(shared.user_agent, "instance-agent");
        global.storage = Storage::Session;
        let session = overrides.resolve(&global);
        assert_eq!(session.proxy.mode, ProxyMode::Direct);
        assert_eq!(session.storage, Storage::Session);
    }
}
