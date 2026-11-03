//! System tray (issue #72).
//!
//! Two kinds of tray icons live side by side:
//!
//! - the **launcher tray** (`main`, always present) with the launcher menu, and
//! - one **instance tray per running instance**, carrying that instance's own
//!   icon, a tooltip with its name / profile / active-conversation count, a
//!   left click that opens or focuses its window, and a right-click menu with
//!   "open" / "stop".
//!
//! Two Tauri constraints shape the design:
//!
//! 1. `TrayIconBuilder::on_menu_event` appends to the app's **global** menu
//!    listener list, and removing a tray does not remove its listener. So the
//!    handlers are registered exactly once, from [`build_tray`], and route by
//!    the menu-item id prefix instead of per-tray closures.
//! 2. A `TrayIcon` handle outliving `remove_tray_by_id` keeps the native icon
//!    alive. The instance registry therefore stores only ids; handles are
//!    never kept.

use std::collections::{HashMap, HashSet};

use crate::{process, AppState};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

/// The launcher's own tray icon (always present).
const MAIN_TRAY_ID: &str = "main";
/// Prefix of an instance tray's id: `instance::<instance_id>`.
const INSTANCE_TRAY_PREFIX: &str = "instance::";

const MENU_OPEN_LAUNCHER: &str = "open-launcher";
const MENU_QUIT: &str = "quit";
const MENU_RUNNING_SUB: &str = "running-profiles";
const MENU_OPEN_PREFIX: &str = "open::";
const MENU_STOP_PREFIX: &str = "stop::";

/// Windows `NOTIFYICONDATAW.szTip` is 128 UTF-16 code units and tray-icon
/// copies at most that many without guaranteeing a terminator, so a longer
/// tooltip is truncated by the shell. Cap it well below the limit.
const TOOLTIP_MAX_CHARS: usize = 120;

/// How often per-instance tooltips are refreshed for the active-conversation
/// count. Instance start/stop re-syncs immediately; this only covers
/// conversations opening and closing inside an already-running instance.
const ACTIVITY_REFRESH: std::time::Duration = std::time::Duration::from_secs(15);

/// (instance_id, instance_name, profile)
type RunningItem = (String, String, String);

struct TrayMenus {
    running: Submenu<tauri::Wry>,
    entries: HashMap<String, (String, MenuItem<tauri::Wry>)>,
    titles: std::collections::HashMap<String, (String, MenuItem<tauri::Wry>)>,
    revision: u64,
}

fn running_menu_items(snapshot: &[RunningItem]) -> Vec<(String, String)> {
    snapshot
        .iter()
        .flat_map(|(id, name, profile)| {
            [
                (
                    format!("{MENU_OPEN_PREFIX}{id}"),
                    format!("打开：{name}（{profile}）"),
                ),
                (
                    format!("{MENU_STOP_PREFIX}{id}"),
                    format!("停止：{name}（{profile}）"),
                ),
            ]
        })
        .collect()
}

// 每完成一个原生操作就保存状态，后续操作失败后重试不会重复创建或移除。
fn sync_menu_items<T>(
    entries: &mut HashMap<String, (String, T)>,
    desired: &[(String, String)],
    mut create: impl FnMut(&str, &str) -> Result<T, String>,
    mut remove: impl FnMut(&T) -> Result<(), String>,
    mut rename: impl FnMut(&T, &str) -> Result<(), String>,
) -> Result<(), String> {
    let removed: Vec<String> = entries
        .keys()
        .filter(|id| !desired.iter().any(|(wanted, _)| wanted == *id))
        .cloned()
        .collect();
    for id in removed {
        remove(&entries[&id].1).map_err(|e| format!("移除菜单项 {id} 失败: {e}"))?;
        entries.remove(&id);
    }
    for (id, text) in desired {
        if let Some((old_text, item)) = entries.get_mut(id) {
            if old_text != text {
                rename(item, text).map_err(|e| format!("更新菜单项 {id} 失败: {e}"))?;
                *old_text = text.clone();
            }
        } else {
            let item = create(id, text).map_err(|e| format!("添加菜单项 {id} 失败: {e}"))?;
            entries.insert(id.clone(), (text.clone(), item));
        }
    }
    Ok(())
}
fn desired_trays(snapshot: &[RunningItem], enabled: bool) -> HashSet<String> {
    snapshot
        .iter()
        .filter(|_| enabled)
        .map(|(id, _, _)| id.clone())
        .collect()
}

fn instance_tray_id(instance_id: &str) -> String {
    format!("{INSTANCE_TRAY_PREFIX}{instance_id}")
}

fn instance_id_of_tray(tray_id: &str) -> Option<&str> {
    tray_id.strip_prefix(INSTANCE_TRAY_PREFIX)
}

/// Builds the launcher tray and registers the single global menu / tray event
/// handlers. Called from setup with no running instances yet.
pub fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_launcher_menu(app)?;
    TrayIconBuilder::with_id(MAIN_TRAY_ID)
        .icon(app.default_window_icon().cloned().unwrap())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .build(app)?;

    // Handlers are registered once on the app, never per tray: the builder's
    // `on_menu_event` appends to a global list (so N trays would fire N times
    // per click), and its `on_tray_icon_event` is keyed by tray id (so an
    // instance tray built later would have no handler at all). The app-level
    // listeners route by menu-item id / tray id instead.
    app.on_menu_event(|app, event| {
        let id = event.id().as_ref();
        if id == MENU_OPEN_LAUNCHER {
            show_launcher(app);
        } else if id == MENU_QUIT {
            quit(app);
        } else if let Some(instance_id) = id.strip_prefix(MENU_OPEN_PREFIX) {
            open_instance_from_tray(app, instance_id);
        } else if let Some(instance_id) = id.strip_prefix(MENU_STOP_PREFIX) {
            stop_instance_from_tray(app, instance_id);
        }
    });
    app.on_tray_icon_event(|app, event| {
        // Left click opens/focuses: the launcher for the launcher tray, the
        // instance window for an instance tray. Right click is left to the
        // native menu.
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            handle_left_click(app, event.id().as_ref());
        }
    });
    Ok(())
}

/// Spawns the periodic tooltip refresh. It is a no-op while nothing runs.
pub fn spawn_activity_refresh(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(ACTIVITY_REFRESH).await;
            let state = app.state::<AppState>();
            if !state.config.lock().unwrap().settings.per_instance_tray {
                continue;
            }
            if !has_running_instances(&app).await {
                continue;
            }
            refresh_activity(&app).await;
        }
    });
}

async fn has_running_instances(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    if !state.running.lock().await.is_empty() {
        return true;
    }
    let tui = state.tui_sessions.lock().await;
    let count = tui.len();
    drop(tui);
    count > 0
}

/// 菜单只响应实例集合与名称变化，慢速资源读取在同步锁外执行。
pub async fn sync_tray_icons(app: &AppHandle) {
    let sync_lock = app.state::<AppState>().tray_sync_lock.clone();
    let (snapshot, revision) = {
        let _guard = sync_lock.lock().await;
        let snapshot = running_snapshot(app).await;
        let enabled = app
            .state::<AppState>()
            .config
            .lock()
            .unwrap()
            .settings
            .per_instance_tray;
        if let Err(error) = sync_launcher_menu(app, &snapshot) {
            crate::log_warn!("更新启动器托盘菜单失败: {error}");
        }
        let revision = {
            let state = app.state::<std::sync::Mutex<TrayMenus>>();
            let mut menus = state.lock().unwrap();
            menus.revision += 1;
            menus.revision
        };
        let desired = desired_trays(&snapshot, enabled);
        let stale: Vec<String> = app
            .state::<AppState>()
            .instance_trays
            .lock()
            .unwrap()
            .difference(&desired)
            .cloned()
            .collect();
        for id in stale {
            remove_instance_tray(app, &id).await;
        }
        (if enabled { snapshot } else { Vec::new() }, revision)
    };

    let mut resolved = Vec::with_capacity(snapshot.len());
    for (id, name, profile) in &snapshot {
        let icon = crate::icons::resolve_instance_icon(app, id).await;
        let tooltip = tooltip_for(app, id, name, profile).await;
        resolved.push((id, name, profile, icon, tooltip));
    }

    let _guard = sync_lock.lock().await;
    if app
        .state::<std::sync::Mutex<TrayMenus>>()
        .lock()
        .unwrap()
        .revision
        != revision
        || !app
            .state::<AppState>()
            .config
            .lock()
            .unwrap()
            .settings
            .per_instance_tray
    {
        return;
    }
    let current = running_snapshot(app).await;
    for (id, name, profile, icon, tooltip) in resolved {
        if !current.contains(&(id.clone(), name.clone(), profile.clone())) {
            continue;
        }
        if let Err(error) = apply_instance_tray(app, id, name, profile, icon, &tooltip) {
            crate::log_warn!("更新实例 {id} 托盘失败: {error}");
        }
    }
}

/// 活跃对话数刷新不创建菜单项，避免 Windows 菜单命令标识不断增长。
async fn refresh_activity(app: &AppHandle) {
    let snapshot = running_snapshot(app).await;
    for (id, name, profile) in snapshot {
        let tooltip = tooltip_for(app, &id, &name, &profile).await;
        let lock = app.state::<AppState>().tray_sync_lock.clone();
        let _guard = lock.lock().await;
        if !app
            .state::<AppState>()
            .config
            .lock()
            .unwrap()
            .settings
            .per_instance_tray
            || !running_snapshot(app)
                .await
                .contains(&(id.clone(), name, profile))
        {
            continue;
        }
        if let Some(tray) = app.tray_by_id(&instance_tray_id(&id)) {
            if let Err(error) = tray.set_tooltip(Some(&tooltip)) {
                crate::log_warn!("更新实例 {id} 托盘提示失败: {error}");
            }
        }
    }
}

fn sync_launcher_menu(app: &AppHandle, snapshot: &[RunningItem]) -> Result<(), String> {
    let state = app.state::<std::sync::Mutex<TrayMenus>>();
    let mut menus = state.lock().unwrap();
    let running = menus.running.clone();
    sync_menu_items(
        &mut menus.entries,
        &running_menu_items(snapshot),
        |id, text| {
            let item =
                MenuItem::with_id(app, id, text, true, None::<&str>).map_err(|e| e.to_string())?;
            running.append(&item).map_err(|e| e.to_string())?;
            Ok(item)
        },
        |item| running.remove(item).map_err(|e| e.to_string()),
        |item, text| item.set_text(text).map_err(|e| e.to_string()),
    )?;
    running
        .set_enabled(!snapshot.is_empty())
        .map_err(|e| format!("更新运行实例子菜单状态失败: {e}"))
}
fn apply_instance_tray(
    app: &AppHandle,
    instance_id: &str,
    name: &str,
    profile: &str,
    icon: Option<Image<'static>>,
    tooltip: &str,
) -> Result<(), String> {
    let tray_id = instance_tray_id(instance_id);
    if let Some(tray) = app.tray_by_id(&tray_id) {
        if let Some(icon) = icon {
            tray.set_icon(Some(icon)).map_err(|e| e.to_string())?;
        }
        tray.set_tooltip(Some(tooltip)).map_err(|e| e.to_string())?;
        let state = app.state::<std::sync::Mutex<TrayMenus>>();
        let mut menus = state.lock().unwrap();
        if let Some((old_name, title)) = menus.titles.get_mut(instance_id) {
            if old_name != name {
                title.set_text(name).map_err(|e| e.to_string())?;
                *old_name = name.to_string();
            }
        }
        return Ok(());
    }

    let (menu, title) = build_instance_menu(app, instance_id, name).map_err(|e| e.to_string())?;
    let mut builder = TrayIconBuilder::with_id(tray_id)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip(tooltip);
    if let Some(icon) = icon {
        builder = builder.icon(icon);
    }
    builder
        .build(app)
        .map_err(|e| format!("创建托盘失败: {e}"))?;
    app.state::<std::sync::Mutex<TrayMenus>>()
        .lock()
        .unwrap()
        .titles
        .insert(instance_id.to_string(), (name.to_string(), title));
    app.state::<AppState>()
        .instance_trays
        .lock()
        .unwrap()
        .insert(instance_id.to_string());
    crate::log_info!("已为实例 {instance_id} 添加托盘图标（profile: {profile}）");
    Ok(())
}

/// 原生图标必须在主线程销毁，完成后才能重新创建同一实例的托盘。
async fn remove_instance_tray(app: &AppHandle, instance_id: &str) {
    let tray_id = instance_tray_id(instance_id);
    let app_for_main = app.clone();
    let (done, completion) = tokio::sync::oneshot::channel();
    if let Err(error) = app.run_on_main_thread(move || {
        drop(app_for_main.remove_tray_by_id(&tray_id));
        let _ = done.send(());
    }) {
        crate::log_warn!("实例 {instance_id} 的托盘移除未能派发到主线程: {error}");
        return;
    }
    if let Err(error) = completion.await {
        crate::log_warn!("实例 {instance_id} 的托盘移除未完成: {error}");
        return;
    }
    app.state::<AppState>()
        .instance_trays
        .lock()
        .unwrap()
        .remove(instance_id);
    app.state::<std::sync::Mutex<TrayMenus>>()
        .lock()
        .unwrap()
        .titles
        .remove(instance_id);
    crate::log_info!("已移除实例 {instance_id} 的托盘图标");
}

/// `"<name> · <profile> · 运行中（N 个活跃对话）"`, capped for the shell. The
/// count segment is omitted when the instance's session index is unreadable
/// (a HOME where DSH never ran, or a WSL distro that is not up).
async fn tooltip_for(app: &AppHandle, instance_id: &str, name: &str, profile: &str) -> String {
    let activity = instance_activity(app, instance_id).await;
    let mut text = format!("{name} · {profile} · 运行中");
    if activity.is_known() {
        text.push_str(&format!("（{} 个活跃对话）", activity.active));
    }
    truncate_tooltip(&text)
}

fn truncate_tooltip(text: &str) -> String {
    if text.chars().count() <= TOOLTIP_MAX_CHARS {
        return text.to_string();
    }
    let mut out: String = text.chars().take(TOOLTIP_MAX_CHARS - 1).collect();
    out.push('…');
    out
}

/// Counts the instance's active conversations by reading its DSH_HOME session
/// index. WSL homes are only read when the distro is already up: a tooltip is
/// not worth booting a distro for.
async fn instance_activity(
    app: &AppHandle,
    instance_id: &str,
) -> crate::sessions::InstanceActivity {
    let home = {
        let state = app.state::<AppState>();
        let cfg = state.config.lock().unwrap();
        let Some(inst) = cfg.instances.iter().find(|i| i.id == instance_id) else {
            return Default::default();
        };
        match cfg.homes.iter().find(|h| h.id == inst.home_id) {
            Some(home) => home.clone(),
            None => return Default::default(),
        }
    };

    if let Some(distro) = home.wsl.clone() {
        let ready = {
            let state = app.state::<AppState>();
            let cache = state.distro_ready.lock().await;
            cache
                .get(&distro)
                .is_some_and(|at| at.elapsed() < crate::wsl::DISTRO_READY_TTL)
        };
        if !ready {
            return Default::default();
        }
    }

    let path = crate::wsl::home_fs_path(&home);
    let read = crate::wsl::run_blocking(move || crate::sessions::read_activity(&path)).await;
    match read {
        Ok(activity) => activity,
        Err(e) => {
            crate::log_debug!("实例 {instance_id} 活跃对话数读取失败: {e}");
            Default::default()
        }
    }
}

/// Left click: the launcher tray shows the launcher; an instance tray shows
/// that instance's window (web GUI or TUI terminal).
fn handle_left_click(app: &AppHandle, tray_id: &str) {
    match instance_id_of_tray(tray_id) {
        Some(instance_id) => open_instance_from_tray(app, instance_id),
        None => show_launcher(app),
    }
}

async fn running_snapshot(app: &AppHandle) -> Vec<RunningItem> {
    let state = app.state::<AppState>();
    let running = state.running.lock().await;
    let tui = state.tui_sessions.lock().await;
    let cfg = state.config.lock().unwrap();
    let name_of = |id: &str| {
        cfg.instances
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.name.clone())
            .unwrap_or_else(|| id.to_string())
    };
    let mut items: Vec<RunningItem> = running
        .iter()
        .map(|(id, entry)| (id.clone(), name_of(id), entry.profile.clone()))
        .collect();
    // TUI sessions (issue #31): profile resolved from the instance config.
    for id in tui.keys() {
        let Some(profile) = crate::process::tui_active_profile(&cfg, id) else {
            continue;
        };
        items.push((id.clone(), name_of(id), profile));
    }
    // One instance can be in both maps during a handoff; keep one tray per id.
    items.sort();
    items.dedup_by(|a, b| a.0 == b.0);
    items
}

fn build_launcher_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let open = MenuItem::with_id(app, MENU_OPEN_LAUNCHER, "打开启动器", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "退出启动器", true, None::<&str>)?;
    let running =
        Submenu::with_id_and_items(app, MENU_RUNNING_SUB, "运行中的 Profile", false, &[])?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &running, &separator, &quit])?;
    app.manage(std::sync::Mutex::new(TrayMenus {
        running,
        entries: Default::default(),
        titles: Default::default(),
        revision: 0,
    }));
    Ok(menu)
}

/// One instance tray's menu: open its window, stop it. The (disabled) title
/// row keeps two instance trays apart when their icons look alike.
fn build_instance_menu(
    app: &AppHandle,
    instance_id: &str,
    name: &str,
) -> tauri::Result<(Menu<tauri::Wry>, MenuItem<tauri::Wry>)> {
    let title = MenuItem::with_id(
        app,
        format!("{INSTANCE_TRAY_PREFIX}{instance_id}::title"),
        name,
        false,
        None::<&str>,
    )?;
    let sep = PredefinedMenuItem::separator(app)?;
    let open = MenuItem::with_id(
        app,
        format!("{MENU_OPEN_PREFIX}{instance_id}"),
        "打开实例界面",
        true,
        None::<&str>,
    )?;
    let stop = MenuItem::with_id(
        app,
        format!("{MENU_STOP_PREFIX}{instance_id}"),
        "终止实例",
        true,
        None::<&str>,
    )?;
    let menu = Menu::with_items(app, &[&title, &sep, &open, &stop])?;
    Ok((menu, title))
}

fn show_launcher(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

fn quit(app: &AppHandle) {
    crate::log_info!("收到退出启动器的托盘请求");
    let state = app.state::<AppState>();
    process::kill_all(&state);
    crate::tui::kill_all(&state);
    app.exit(0);
}

fn open_instance_from_tray(app: &AppHandle, instance_id: &str) {
    let app = app.clone();
    let id = instance_id.to_string();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        // TUI instances: reopen the terminal window (issue #31).
        if state.tui_sessions.lock().await.contains_key(&id) {
            let _ = crate::windows::open_tui_window(&app, &id);
            return;
        }
        let url = state.running.lock().await.get(&id).map(|r| r.desktop_url());
        let url = match url {
            Some(Some(u)) => u,
            _ => return,
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
        let _ = crate::windows::open_instance_window(&app, &id, &name, &url);
    });
}

fn stop_instance_from_tray(app: &AppHandle, instance_id: &str) {
    let app = app.clone();
    let id = instance_id.to_string();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let result = if state.tui_sessions.lock().await.contains_key(&id) {
            crate::tui::stop_tui_session(&app, &state, &id).await
        } else {
            process::stop_instance_process(&app, &state, &id).await
        };
        if let Err(error) = result {
            crate::log_error!("托盘终止实例 {id} 失败: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_sync_reuses_entries_on_refresh_and_rename() {
        let mut entries = HashMap::new();
        let created = std::cell::Cell::new(0);
        let renamed = std::cell::Cell::new(0);
        let removed = std::cell::Cell::new(0);
        let mut sync = |snapshot: &[RunningItem]| {
            sync_menu_items(
                &mut entries,
                &running_menu_items(snapshot),
                |id, _| {
                    created.set(created.get() + 1);
                    Ok(id.to_string())
                },
                |_| {
                    removed.set(removed.get() + 1);
                    Ok(())
                },
                |_, _| {
                    renamed.set(renamed.get() + 1);
                    Ok(())
                },
            )
            .unwrap();
        };
        let mut snapshot = vec![("foo".into(), "示例".into(), "web".into())];
        sync(&snapshot);
        let initial = created.get();
        for _ in 0..10_000 {
            sync(&snapshot);
        }
        assert_eq!(created.get(), initial);
        snapshot[0].1 = "改名".into();
        sync(&snapshot);
        assert_eq!(created.get(), initial);
        assert!(renamed.get() > 0);
        assert!(desired_trays(&snapshot, false).is_empty());
        sync(&snapshot);
        assert_eq!(created.get(), initial);
        assert!(desired_trays(&snapshot, true).contains("foo"));
        snapshot.push(("bar".into(), "新增".into(), "tui".into()));
        sync(&snapshot);
        assert!(created.get() > initial);
        snapshot.remove(0);
        sync(&snapshot);
        assert!(removed.get() > 0);
        assert!(entries.keys().all(|id| id.ends_with("bar")));
    }

    #[test]
    fn partial_menu_failure_keeps_completed_operations() {
        let mut entries = HashMap::new();
        let desired = running_menu_items(&[("foo".into(), "示例".into(), "web".into())]);
        let error = sync_menu_items(
            &mut entries,
            &desired,
            |id, _| {
                if id.starts_with(MENU_STOP_PREFIX) {
                    Err("示例错误".into())
                } else {
                    Ok(id.to_string())
                }
            },
            |_| Ok(()),
            |_, _| Ok(()),
        )
        .unwrap_err();
        assert!(error.contains("stop::foo"));
        let mut created = Vec::new();
        sync_menu_items(
            &mut entries,
            &desired,
            |id, _| {
                created.push(id.to_string());
                Ok(id.to_string())
            },
            |_| Ok(()),
            |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(created, vec!["stop::foo"]);
        let mut removed = Vec::new();
        sync_menu_items(
            &mut entries,
            &[],
            |_, _| unreachable!(),
            |id| {
                if !removed.is_empty() {
                    return Err("示例错误".into());
                }
                removed.push(id.clone());
                Ok(())
            },
            |_, _| Ok(()),
        )
        .unwrap_err();
        assert!(!entries.contains_key(&removed[0]));
        sync_menu_items(
            &mut entries,
            &[],
            |_, _| unreachable!(),
            |id| {
                assert!(!removed.contains(id));
                removed.push(id.clone());
                Ok(())
            },
            |_, _| Ok(()),
        )
        .unwrap();
        assert!(entries.is_empty());
    }
    #[test]
    fn instance_tray_ids_round_trip() {
        let tray_id = instance_tray_id("inst-1");
        assert_eq!(tray_id, "instance::inst-1");
        assert_eq!(instance_id_of_tray(&tray_id), Some("inst-1"));
    }

    #[test]
    fn the_launcher_tray_is_not_an_instance_tray() {
        assert_eq!(instance_id_of_tray(MAIN_TRAY_ID), None);
    }

    #[test]
    fn an_instance_named_like_the_launcher_id_gets_its_own_tray_id() {
        // Instance ids are generated, but nothing stops a future caller from
        // choosing one; a collision would make the launcher tray and an
        // instance tray fight over the same id.
        assert_ne!(instance_tray_id(MAIN_TRAY_ID), MAIN_TRAY_ID);
        assert_eq!(
            instance_id_of_tray(&instance_tray_id(MAIN_TRAY_ID)),
            Some(MAIN_TRAY_ID)
        );
    }

    #[test]
    fn menu_item_ids_route_to_the_launcher_actions() {
        // The launcher's own commands must not be parsed as instance ids, and
        // instance menu ids must survive the round trip (the routing is a
        // prefix match, so order matters).
        assert!(!MENU_OPEN_LAUNCHER.starts_with(MENU_OPEN_PREFIX));
        assert!(!MENU_QUIT.starts_with(MENU_STOP_PREFIX));
        let item = format!("{MENU_OPEN_PREFIX}i-1");
        assert_eq!(item.strip_prefix(MENU_OPEN_PREFIX), Some("i-1"));
        let stop = format!("{MENU_STOP_PREFIX}i-1");
        assert_eq!(stop.strip_prefix(MENU_STOP_PREFIX), Some("i-1"));
    }

    #[test]
    fn tooltips_are_capped_for_the_windows_shell() {
        assert_eq!(truncate_tooltip("短提示"), "短提示");
        let long = "实".repeat(TOOLTIP_MAX_CHARS + 20);
        let capped = truncate_tooltip(&long);
        assert_eq!(capped.chars().count(), TOOLTIP_MAX_CHARS);
        assert!(capped.ends_with('…'));
    }

    #[test]
    fn tooltip_length_counts_characters_not_bytes() {
        // 100 CJK characters are 300 UTF-8 bytes but only 100 UTF-16 code
        // units, so this still fits the shell's limit.
        let text = format!("{} · pr · 运行中", "名".repeat(100));
        assert!(text.len() > TOOLTIP_MAX_CHARS);
        assert_eq!(truncate_tooltip(&text), text);
    }
}
