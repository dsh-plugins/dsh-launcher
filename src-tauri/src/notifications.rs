use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use tauri::{AppHandle, Manager};

use crate::AppState;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Plan,
    Question,
    Completed,
    Running,
    Resolved,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Event {
    session_id: String,
    id: String,
    kind: Kind,
}

struct Alert {
    kind: Kind,
    unread: bool,
}

#[derive(Default)]
struct InstanceAlerts {
    generation: String,
    events: HashMap<(String, String), Alert>,
}

impl InstanceAlerts {
    fn apply(&mut self, event: Event, foreground: bool) -> bool {
        let key = (event.session_id, event.id);
        match event.kind {
            Kind::Running => {
                self.events.retain(|(session, _), alert| {
                    session != &key.0 || alert.kind != Kind::Completed
                });
                false
            }
            Kind::Resolved => {
                self.events.remove(&key);
                false
            }
            kind => {
                if self.events.contains_key(&key) {
                    return false;
                }
                if kind == Kind::Completed {
                    self.events.retain(|(session, _), alert| {
                        session != &key.0 || alert.kind != Kind::Completed
                    });
                }
                self.events.insert(key, Alert { kind, unread: true });
                !foreground
            }
        }
    }

    fn acknowledge(&mut self, session: &str, through_turn: u64) {
        for ((id, event), alert) in &mut self.events {
            if id == session
                && (alert.kind != Kind::Completed
                    || event
                        .strip_prefix("turn:")
                        .and_then(|turn| turn.parse::<u64>().ok())
                        .is_some_and(|turn| turn <= through_turn))
            {
                alert.unread = false;
            }
        }
    }

    fn pending(&self) -> bool {
        self.events.values().any(|alert| alert.unread)
    }
}

/// 提醒只保存在启动器内存中，实例退出后清除，历史会话不产生新通知。
#[derive(Default)]
pub(crate) struct Notifications(Mutex<HashMap<String, InstanceAlerts>>);

/// 每次启动的独立标记将桥接事件与普通终端输出分开。
pub(crate) struct Bridge {
    pub marker: String,
    pub patch: PathBuf,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        if let Some(dir) = self.patch.parent() {
            if let Err(error) = std::fs::remove_dir_all(dir) {
                crate::log_warn!("清除通知桥接 {} 失败: {error}", dir.display());
            }
        }
    }
}

pub(crate) async fn prepare(data: &Path, distro: Option<&str>) -> Result<Bridge, String> {
    let nonce = uuid::Uuid::new_v4();
    let dir = data.join("notification-bridge").join(nonce.to_string());
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建通知桥接目录失败: {e}"))?;
    let bridge = Bridge {
        marker: format!("dsh-launcher-notify:{nonce}:"),
        patch: dir.join("cordis.patch.yml"),
    };
    let module = dir.join("index.mjs");
    std::fs::write(&module, include_str!("../bridge/notifications.mjs"))
        .map_err(|e| format!("写入通知桥接失败: {e}"))?;
    std::fs::write(
        dir.join("client.js"),
        include_str!("../bridge/generated/client.js"),
    )
    .map_err(|e| format!("写入通知客户端失败: {e}"))?;
    let metadata = serde_json::json!({
        "name": "@dsh-launcher/notifications", "version": "0.0.0", "type": "module",
        "exports": { ".": "./index.mjs", "./client": "./client.js", "./package.json": "./package.json" },
        "dsh": { "client": { "platform": "web", "inject": ["@deepseek-ai/dsh-api-session-controller"] } }
    });
    std::fs::write(dir.join("package.json"), metadata.to_string())
        .map_err(|e| format!("写入通知桥接清单失败: {e}"))?;
    let sidebar = dir.join("sidebar");
    std::fs::create_dir(&sidebar).map_err(|e| format!("创建侧边栏插件目录失败: {e}"))?;
    std::fs::write(sidebar.join("index.mjs"), "export function apply() {}\n")
        .map_err(|e| format!("写入侧边栏宿主入口失败: {e}"))?;
    std::fs::write(
        sidebar.join("client.js"),
        include_str!("../bridge/generated/sidebar.js"),
    )
    .map_err(|e| format!("写入侧边栏客户端失败: {e}"))?;
    let sidebar_metadata = serde_json::json!({
        "name": "@dsh-launcher/sidebar-browser", "version": "0.0.0", "type": "module",
        "exports": { ".": "./index.mjs", "./client": "./client.js", "./package.json": "./package.json" },
        "dsh": { "client": { "platform": "web" } }
    });
    std::fs::write(sidebar.join("package.json"), sidebar_metadata.to_string())
        .map_err(|e| format!("写入侧边栏插件清单失败: {e}"))?;
    let module = match distro {
        Some(distro) => {
            let path = crate::wsl::to_wsl_path(distro, &module.to_string_lossy()).await?;
            let mut url = tauri::Url::parse("file:///").unwrap();
            url.set_path(&path);
            url
        }
        None => tauri::Url::from_file_path(&module)
            .map_err(|_| "通知桥接路径无效")?,
    };
    let sidebar_module = module.join("sidebar/index.mjs").map_err(|e| e.to_string())?;
    let content = serde_json::json!([{
        "insert": [{
            "id": "launcher-notifications",
            "name": module.as_str(),
            "config": { "marker": bridge.marker }
        }, {
            "id": "launcher-sidebar-browser",
            "name": sidebar_module.as_str()
        }]
    }]);
    std::fs::write(&bridge.patch, content.to_string())
        .map_err(|e| format!("写入通知桥接补丁失败: {e}"))?;
    Ok(bridge)
}

fn foreground(app: &AppHandle, id: &str) -> bool {
    let Some(win) = app.get_window(&format!("instance-{id}")) else {
        return false;
    };
    let query = || -> tauri::Result<bool> {
        Ok(win.is_focused()? && win.is_visible()? && !win.is_minimized()?)
    };
    query().unwrap_or_else(|error| {
        crate::log_debug!("查询实例 {id} 的窗口前台状态失败: {error}");
        false
    })
}

pub(crate) fn pending(app: &AppHandle, id: &str) -> bool {
    app.state::<Notifications>()
        .0
        .lock()
        .unwrap()
        .get(id)
        .is_some_and(InstanceAlerts::pending)
}

pub(crate) fn clear(app: &AppHandle, id: &str) {
    app.state::<Notifications>().0.lock().unwrap().remove(id);
}

pub(crate) fn begin(app: &AppHandle, id: &str, generation: &str) {
    app.state::<Notifications>().0.lock().unwrap().insert(
        id.to_string(),
        InstanceAlerts {
            generation: generation.to_string(),
            ..Default::default()
        },
    );
}

pub(crate) fn viewed_navigation(app: &AppHandle, id: &str, url: &tauri::Url) -> bool {
    if url.scheme() != "dsh-launcher-read" {
        return true;
    }
    if foreground(app, id) {
        let query: HashMap<_, _> = url.query_pairs().collect();
        if let (Some(session), Some(turn)) = (
            query.get("id"),
            query
                .get("turn")
                .and_then(|value| value.parse::<u64>().ok()),
        ) {
            if let Some(alerts) = app.state::<Notifications>().0.lock().unwrap().get_mut(id) {
                alerts.acknowledge(session, turn);
            }
        }
    }
    false
}

pub(crate) fn receive(app: &AppHandle, id: &str, generation: &str, payload: &str) {
    let event: Event = match serde_json::from_str(payload) {
        Ok(event) => event,
        Err(error) => {
            crate::log_warn!("实例 {id} 通知事件解析失败: {error}");
            return;
        }
    };
    let focused = foreground(app, id);
    let kind = event.kind;
    let event_key = (event.session_id.clone(), event.id.clone());
    let notify = {
        let state = app.state::<Notifications>();
        let mut instances = state.0.lock().unwrap();
        let Some(alerts) = instances
            .get_mut(id)
            .filter(|alerts| alerts.generation == generation)
        else {
            return;
        };
        alerts.apply(event, focused)
    };
    if !notify {
        return;
    }
    let state = app.state::<AppState>();
    let (settings, name) = {
        let config = state.config.lock().unwrap();
        let Some(instance) = config.instances.iter().find(|instance| instance.id == id) else {
            return;
        };
        (config.settings.clone(), instance.name.clone())
    };
    let message = match (kind, settings.locale.as_str()) {
        (Kind::Plan, "en-US") => "Plan awaiting review",
        (Kind::Question, "en-US") => "Answer needed",
        (Kind::Completed, "en-US") => "Completed",
        (Kind::Plan, _) => "计划待审",
        (Kind::Question, _) => "需要回答问题",
        (Kind::Completed, _) => "已完成",
        _ => return,
    };
    if settings.notification_system {
        let app = app.clone();
        let id = id.to_string();
        let title = format!("{name} · {message}");
        let generation = generation.to_string();
        let event_key = event_key.clone();
        std::thread::spawn(move || {
            if !alert_current(&app, &id, &generation, &event_key) {
                return;
            }
            if let Err(error) = show_system(&app, &id, &title) {
                crate::log_warn!("实例 {id} 系统提醒失败: {error}");
            }
        });
    }
    if settings.notification_sound {
        let app = app.clone();
        let id = id.to_string();
        let generation = generation.to_string();
        std::thread::spawn(move || {
            if !alert_current(&app, &id, &generation, &event_key) {
                return;
            }
            if let Err(error) = play_sound() {
                crate::log_warn!("播放通知声音失败: {error}");
            }
        });
    }
}

fn alert_current(app: &AppHandle, id: &str, generation: &str, key: &(String, String)) -> bool {
    if foreground(app, id) {
        return false;
    }
    app.state::<Notifications>()
        .0
        .lock()
        .unwrap()
        .get(id)
        .filter(|alerts| alerts.generation == generation)
        .and_then(|alerts| alerts.events.get(key))
        .is_some_and(|alert| alert.unread)
}

fn play_sound() -> Result<(), String> {
    use rodio::{source::SineWave, OutputStreamBuilder, Sink, Source};
    let mut stream = OutputStreamBuilder::open_default_stream().map_err(|e| e.to_string())?;
    stream.log_on_drop(false);
    let sink = Sink::connect_new(stream.mixer());
    for frequency in [660_f32, 880_f32] {
        sink.append(
            SineWave::new(frequency)
                .take_duration(Duration::from_millis(140))
                .amplify(0.15)
                .fade_in(Duration::from_millis(10)),
        );
    }
    sink.sleep_until_end();
    Ok(())
}

#[cfg(windows)]
fn show_system(app: &AppHandle, id: &str, title: &str) -> Result<(), String> {
    use windows::{
        core::HSTRING,
        Data::Xml::Dom::XmlDocument,
        Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
        UI::Notifications::{ToastNotification, ToastNotificationManager},
    };
    // 此函数由独立后台线程调用，WinRT 初始化与释放必须位于同一线程。
    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(|e| e.to_string())?;
    let result = (|| -> windows::core::Result<()> {
        let mut launch = tauri::Url::parse("dsh-launcher://notification").unwrap();
        launch.query_pairs_mut().append_pair("instance", id);
        let escape = |text: &str| {
            text.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&apos;")
        };
        let xml = XmlDocument::new()?;
        xml.LoadXml(&HSTRING::from(format!(
            "<toast activationType=\"protocol\" launch=\"{}\"><visual><binding template=\"ToastGeneric\"><text>{}</text></binding></visual><audio silent=\"true\"/></toast>",
            escape(launch.as_str()), escape(title)
        )))?;
        let notification = ToastNotification::CreateToastNotification(&xml)?;
        notification.SetTag(&HSTRING::from(id.chars().take(16).collect::<String>()))?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
            &app.config().identifier,
        ))?
        .Show(&notification)
    })();
    unsafe { RoUninitialize() };
    result.map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn show_system(app: &AppHandle, id: &str, title: &str) -> Result<(), String> {
    let mut builder = notify_rust::Notification::new();
    builder
        .summary(title)
        .appname("DSH Launcher")
        .action("default", "打开实例");
    #[cfg(not(target_os = "macos"))]
    builder.hint(notify_rust::Hint::SuppressSound(true));
    let notification = builder.show().map_err(|e| e.to_string())?;
    notification.wait_for_action(|action| {
        if action == "default" || action == "打开实例" {
            crate::tray::open_instance_from_tray(app, id);
        }
    });
    Ok(())
}

/// 通知链接只打开仍运行的已注册实例，不通过链接启动新进程。
pub(crate) fn open_link(app: &AppHandle, link: &str) -> bool {
    let Ok(url) = tauri::Url::parse(link) else {
        return false;
    };
    if url.scheme() != "dsh-launcher" || url.host_str() != Some("notification") {
        return false;
    }
    if let Some((_, id)) = url.query_pairs().find(|(key, _)| key == "instance") {
        crate::tray::open_instance_from_tray(app, &id);
    }
    true
}

pub(crate) fn start(app: &AppHandle) {
    app.manage(Notifications::default());
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(600));
        let mut lit = true;
        let mut flashing = HashMap::<String, tauri::image::Image<'static>>::new();
        let mut previous = std::collections::HashSet::new();
        loop {
            interval.tick().await;
            let ids: Vec<_> = app
                .state::<Notifications>()
                .0
                .lock()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            let enabled = app
                .state::<AppState>()
                .config
                .lock()
                .unwrap()
                .settings
                .notification_tray;
            let targets: Vec<_> = ids
                .into_iter()
                .filter(|id| enabled && pending(&app, id))
                .collect();
            let current: std::collections::HashSet<_> = targets.iter().cloned().collect();
            if current != previous {
                crate::tray::sync_tray_icons(&app).await;
                previous = current;
            }
            for id in &targets {
                if let Some(icon) = crate::icons::resolve_instance_icon(&app, id).await {
                    flashing.insert(id.clone(), icon);
                }
            }
            let lock = app.state::<AppState>().tray_sync_lock.clone();
            let guard = lock.lock().await;
            let blank = tauri::image::Image::new_owned(vec![0; 32 * 32 * 4], 32, 32);
            flashing.retain(|id, icon| {
                let pending = enabled && self::pending(&app, id) && !foreground(&app, id);
                if let Some(tray) = app.tray_by_id(&format!("instance::{id}")) {
                    if let Err(error) = tray.set_icon(Some(if pending && !lit {
                        blank.clone()
                    } else {
                        icon.clone()
                    })) {
                        crate::log_warn!("更新实例 {id} 提醒图标失败: {error}");
                    }
                }
                pending
            });
            drop(guard);
            lit = !lit;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(session: &str, id: &str, kind: Kind) -> Event {
        Event {
            session_id: session.into(),
            id: id.into(),
            kind,
        }
    }

    #[test]
    fn transitions_deduplicate_and_acknowledge_without_realerting() {
        let mut state = InstanceAlerts::default();
        assert!(state.apply(event("a", "q", Kind::Question), false));
        assert!(!state.apply(event("a", "q", Kind::Question), false));
        state.acknowledge("a", 0);
        assert!(!state.pending());
        assert!(!state.apply(event("a", "q", Kind::Question), false));
        assert!(state.apply(event("b", "p", Kind::Plan), false));
        state.apply(event("a", "q", Kind::Resolved), false);
        assert!(state.pending());
        state.apply(event("b", "p", Kind::Resolved), false);
        assert!(!state.pending());
    }

    #[test]
    fn foreground_and_new_turns_clear_only_their_own_completion() {
        let mut state = InstanceAlerts::default();
        assert!(!state.apply(event("a", "turn:1", Kind::Completed), true));
        state.acknowledge("a", 1);
        assert!(!state.pending());
        assert!(state.apply(event("a", "turn:2", Kind::Completed), false));
        assert!(state.apply(event("b", "turn:1", Kind::Completed), false));
        state.apply(event("a", "turn:3", Kind::Running), false);
        assert!(state.pending());
        state.apply(event("b", "turn:2", Kind::Running), false);
        assert!(!state.pending());
    }

    #[test]
    fn viewing_other_sessions_or_older_turns_preserves_unread_results() {
        let mut state = InstanceAlerts::default();
        state.apply(event("a", "turn:3", Kind::Completed), false);
        state.acknowledge("b", 5);
        assert!(state.pending());
        state.acknowledge("a", 2);
        assert!(state.pending());
        state.acknowledge("a", 3);
        assert!(!state.pending());
    }
}
