use crate::{process, AppState};
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

const MENU_OPEN_LAUNCHER: &str = "open-launcher";
const MENU_QUIT: &str = "quit";
const MENU_RUNNING_SUB: &str = "running-profiles";
const MENU_OPEN_PREFIX: &str = "open::";
const MENU_STOP_PREFIX: &str = "stop::";

/// (instance_id, instance_name, profile)
type RunningItem = (String, String, String);

/// Builds the tray icon with its dynamic menu. Called from setup with no
/// running instances yet.
pub fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app, &[])?;
    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().cloned().unwrap())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
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
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { .. } = event {
                handle_double_click(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Rebuilds the tray menu to reflect the current set of running instances.
/// Must be called from an async context.
pub async fn rebuild_tray_menu(app: &AppHandle) {
    let snapshot = running_snapshot(app).await;
    let Some(tray) = app.tray_by_id("main") else {
        return;
    };
    match build_menu(app, &snapshot) {
        Ok(menu) => {
            let _ = tray.set_menu(Some(menu));
        }
        Err(e) => {
            eprintln!("dsh-launcher: rebuild tray menu failed: {e}");
        }
    }
}

async fn running_snapshot(app: &AppHandle) -> Vec<RunningItem> {
    let state = app.state::<AppState>();
    let running = state.running.lock().await;
    let tui = state.tui_sessions.lock().await;
    let cfg = state.config.lock().unwrap();
    let mut items: Vec<RunningItem> = running
        .iter()
        .map(|(id, entry)| {
            let name = cfg
                .instances
                .iter()
                .find(|i| i.id == *id)
                .map(|i| i.name.clone())
                .unwrap_or_else(|| id.clone());
            (id.clone(), name, entry.profile.clone())
        })
        .collect();
    // TUI sessions (issue #31): profile resolved from the instance config.
    for id in tui.keys() {
        let Some(profile) = crate::process::tui_active_profile(&cfg, id) else {
            continue;
        };
        let name = cfg
            .instances
            .iter()
            .find(|i| i.id == *id)
            .map(|i| i.name.clone())
            .unwrap_or_else(|| id.clone());
        items.push((id.clone(), name, profile));
    }
    items
}

fn build_menu(app: &AppHandle, running: &[RunningItem]) -> tauri::Result<Menu<tauri::Wry>> {
    let open_launcher =
        MenuItem::with_id(app, MENU_OPEN_LAUNCHER, "打开启动器", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "退出启动器", true, None::<&str>)?;
    let sep_running = PredefinedMenuItem::separator(app)?;
    let sep_quit = PredefinedMenuItem::separator(app)?;

    // Running profiles live in a second-level submenu: each instance gets an
    // "open window" and a "stop" entry.
    let mut owned: Vec<MenuItem<tauri::Wry>> = Vec::new();
    let mut running_sub = None;
    if !running.is_empty() {
        for (id, name, profile) in running {
            owned.push(MenuItem::with_id(
                app,
                format!("{MENU_OPEN_PREFIX}{id}"),
                format!("打开：{name}（{profile}）"),
                true,
                None::<&str>,
            )?);
            owned.push(MenuItem::with_id(
                app,
                format!("{MENU_STOP_PREFIX}{id}"),
                format!("停止：{name}（{profile}）"),
                true,
                None::<&str>,
            )?);
        }
        let refs: Vec<&dyn IsMenuItem<tauri::Wry>> = owned
            .iter()
            .map(|i| i as &dyn IsMenuItem<tauri::Wry>)
            .collect();
        running_sub = Some(Submenu::with_id_and_items(
            app,
            MENU_RUNNING_SUB,
            "运行中的 Profile",
            true,
            &refs,
        )?);
    }

    let mut items: Vec<&dyn IsMenuItem<tauri::Wry>> = vec![&open_launcher];
    if let Some(sub) = &running_sub {
        items.push(&sep_running);
        items.push(sub);
    }
    items.push(&sep_quit);
    items.push(&quit);

    Menu::with_items(app, &items)
}

fn show_launcher(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

fn quit(app: &AppHandle) {
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
        let url = state.running.lock().await.get(&id).map(|r| r.url.clone());
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
        // TUI instances live in their own session map (issue #31).
        if state.tui_sessions.lock().await.contains_key(&id) {
            let _ = crate::tui::stop_tui_session(&app, &state, &id).await;
        } else {
            let _ = process::stop_instance_process(&app, &state, &id).await;
        }
    });
}

fn handle_double_click(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        // Prefer the profile page the user focused last; fall back to the
        // single running instance; otherwise just show the launcher. TUI
        // sessions count as running too (issue #31).
        let last = state.last_focused_instance.lock().unwrap().clone();
        let running = state.running.lock().await;
        let tui_running = state.tui_sessions.lock().await;
        let is_running = |id: &str| running.contains_key(id) || tui_running.contains_key(id);
        let target_id = last.filter(|id| is_running(id)).or_else(|| {
            let total = running.len() + tui_running.len();
            if total == 1 {
                running
                    .keys()
                    .next()
                    .or_else(|| tui_running.keys().next())
                    .cloned()
            } else {
                None
            }
        });
        // TUI first: no URL window, reopen the terminal window.
        let tui_target = target_id.clone().filter(|id| tui_running.contains_key(id));
        if let Some(id) = tui_target {
            drop(tui_running);
            drop(running);
            let _ = crate::windows::open_tui_window(&app, &id);
            return;
        }
        drop(tui_running);
        let target = target_id.and_then(|id| {
            let entry = running.get(&id)?;
            let url = entry.url.clone()?;
            let name = state
                .config
                .lock()
                .unwrap()
                .instances
                .iter()
                .find(|i| i.id == id)
                .map(|i| i.name.clone())
                .unwrap_or_else(|| id.clone());
            Some((id, name, url))
        });
        drop(running);

        if let Some((id, name, url)) = target {
            let _ = crate::windows::open_instance_window(&app, &id, &name, &url);
        } else {
            show_launcher(&app);
        }
    });
}
