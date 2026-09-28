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

use std::collections::HashSet;

use crate::{process, AppState};
use tauri::image::Image;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
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

fn instance_tray_id(instance_id: &str) -> String {
    format!("{INSTANCE_TRAY_PREFIX}{instance_id}")
}

fn instance_id_of_tray(tray_id: &str) -> Option<&str> {
    tray_id.strip_prefix(INSTANCE_TRAY_PREFIX)
}

/// The app's bundled icon as an owned image, so the fallback outlives the
/// borrow of the app handle.
fn default_icon(app: &AppHandle) -> Option<Image<'static>> {
    app.default_window_icon().cloned().map(Image::to_owned)
}

/// Builds the launcher tray and registers the single global menu / tray event
/// handlers. Called from setup with no running instances yet.
pub fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_launcher_menu(app, &[])?;
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
            sync_tray_icons(&app).await;
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

/// Brings the instance trays in line with the running set and rebuilds the
/// launcher tray's menu. Must be called from an async context.
///
/// Applying tray changes is serialized (and re-checks existence inside the
/// lock). Two overlapping runs could otherwise both observe "no tray for
/// instance X" and both build one; `tray_by_id` would then only ever see the
/// first and `remove_tray_by_id` would only remove the first, leaving a ghost
/// icon that never disappears. Icon resolution (a network download or a UNC
/// read, both slow) deliberately runs *outside* the lock so a slow fetch never
/// delays stopping an instance.
pub async fn sync_tray_icons(app: &AppHandle) {
    let enabled = app
        .state::<AppState>()
        .config
        .lock()
        .unwrap()
        .settings
        .per_instance_tray;
    let snapshot = if enabled {
        running_snapshot(app).await
    } else {
        Vec::new()
    };
    let desired: HashSet<String> = snapshot.iter().map(|(id, _, _)| id.clone()).collect();

    // Retract stale trays first: this must not wait behind an icon download,
    // and stopping an instance should look immediate.
    {
        let sync_lock = app.state::<AppState>().tray_sync_lock.clone();
        let _guard = sync_lock.lock().await;
        rebuild_launcher_menu(app, &snapshot);
        let stale: Vec<String> = {
            let state = app.state::<AppState>();
            let trays = state.instance_trays.lock().unwrap();
            trays.difference(&desired).cloned().collect()
        };
        for instance_id in stale {
            remove_instance_tray(app, &instance_id);
        }
    }

    // Slow part, outside the lock.
    let mut icons = Vec::with_capacity(snapshot.len());
    for (instance_id, _, _) in &snapshot {
        icons.push(resolve_instance_icon(app, instance_id).await);
    }

    {
        let sync_lock = app.state::<AppState>().tray_sync_lock.clone();
        let _guard = sync_lock.lock().await;
        for ((instance_id, name, profile), icon) in snapshot.iter().zip(icons) {
            // The snapshot was taken before the slow icon resolution, so an
            // instance may have stopped meanwhile. A later sync (from its
            // waiter) would have removed its tray already; re-creating one here
            // would leave a ghost icon for a stopped instance.
            if !is_live(app, instance_id).await {
                continue;
            }
            if let Err(e) = apply_instance_tray(app, instance_id, name, profile, icon).await {
                crate::log_warn!("创建实例 {instance_id} 的托盘图标失败: {e}");
            }
        }
    }
}

/// Whether an instance is still running (web process or TUI session). Used to
/// re-validate a snapshot taken before a slow icon resolution.
async fn is_live(app: &AppHandle, instance_id: &str) -> bool {
    let state = app.state::<AppState>();
    if state.running.lock().await.contains_key(instance_id) {
        return true;
    }
    let tui = state.tui_sessions.lock().await;
    let live = tui.contains_key(instance_id);
    drop(tui);
    live
}

/// Rebuilds the launcher tray's dynamic menu. The "running profiles" submenu
/// stays useful even with per-instance trays, and is the only route to a
/// running instance when they are disabled.
fn rebuild_launcher_menu(app: &AppHandle, snapshot: &[RunningItem]) {
    let Some(tray) = app.tray_by_id(MAIN_TRAY_ID) else {
        return;
    };
    match build_launcher_menu(app, snapshot) {
        Ok(menu) => {
            let _ = tray.set_menu(Some(menu));
        }
        Err(e) => crate::log_warn!("重建启动器托盘菜单失败: {e}"),
    }
}

/// Creates or updates one instance's tray icon. The caller must hold the tray
/// sync lock.
async fn apply_instance_tray(
    app: &AppHandle,
    instance_id: &str,
    name: &str,
    profile: &str,
    icon: Option<Image<'static>>,
) -> Result<(), String> {
    let tray_id = instance_tray_id(instance_id);
    let tooltip = tooltip_for(app, instance_id, name, profile).await;
    let menu = build_instance_menu(app, instance_id, name).map_err(|e| e.to_string())?;

    if let Some(tray) = app.tray_by_id(&tray_id) {
        if let Some(icon) = icon {
            let _ = tray.set_icon(Some(icon));
        }
        let _ = tray.set_tooltip(Some(&tooltip));
        let _ = tray.set_menu(Some(menu));
        return Ok(());
    }

    let mut builder = TrayIconBuilder::with_id(tray_id)
        .menu(&menu)
        // Left click opens/focuses, so the menu is right-click only.
        .show_menu_on_left_click(false)
        .tooltip(&tooltip);
    if let Some(icon) = icon {
        builder = builder.icon(icon);
    }
    builder
        .build(app)
        .map_err(|e| format!("创建托盘失败: {e}"))?;

    app.state::<AppState>()
        .instance_trays
        .lock()
        .unwrap()
        .insert(instance_id.to_string());
    crate::log_info!("已为实例 {instance_id} 添加托盘图标（profile: {profile}）");
    Ok(())
}

/// Removes an instance's tray icon and forgets it. The handle returned by
/// `remove_tray_by_id` is discarded immediately — keeping it alive would keep
/// the native icon on screen.
fn remove_instance_tray(app: &AppHandle, instance_id: &str) {
    {
        let state = app.state::<AppState>();
        state.instance_trays.lock().unwrap().remove(instance_id);
    }
    let tray_id = instance_tray_id(instance_id);
    if app.remove_tray_by_id(&tray_id).is_some() {
        crate::log_info!("已移除实例 {instance_id} 的托盘图标");
    }
}

/// The instance's own icon, or the launcher default. Local files live in the
/// HOME (a UNC round trip for WSL homes); remote URLs are downloaded once per
/// session and cached.
async fn resolve_instance_icon(app: &AppHandle, instance_id: &str) -> Option<Image<'static>> {
    let source = {
        let state = app.state::<AppState>();
        let cfg = state.config.lock().unwrap();
        cfg.instances
            .iter()
            .find(|i| i.id == instance_id)
            .and_then(|i| i.icon.clone())
    };
    let Some(source) = source else {
        return default_icon(app);
    };

    {
        let state = app.state::<AppState>();
        let cache = state.instance_icon_cache.lock().unwrap();
        if let Some((cached_source, bytes)) = cache.get(instance_id) {
            if cached_source == &source {
                // `None` bytes cache a *failure* for this source, so a broken
                // icon URL or unreadable file is not retried on every sync.
                return bytes
                    .as_deref()
                    .and_then(decode_icon)
                    .or_else(|| default_icon(app));
            }
        }
    }

    let bytes = if source == "local" {
        read_local_instance_icon(app, instance_id).await
    } else {
        match crate::icons::fetch_square_icon_png(&source).await {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                crate::log_warn!("实例 {instance_id} 的托盘图标下载失败: {e}");
                None
            }
        }
    };

    app.state::<AppState>()
        .instance_icon_cache
        .lock()
        .unwrap()
        .insert(instance_id.to_string(), (source, bytes.clone()));
    match bytes {
        Some(bytes) => decode_icon(&bytes).or_else(|| default_icon(app)),
        None => default_icon(app),
    }
}

/// Clears the cached icon of one instance, so the next sync re-reads it.
pub fn invalidate_instance_icon(app: &AppHandle, instance_id: &str) {
    app.state::<AppState>()
        .instance_icon_cache
        .lock()
        .unwrap()
        .remove(instance_id);
}

async fn read_local_instance_icon(app: &AppHandle, instance_id: &str) -> Option<Vec<u8>> {
    let path = {
        let state = app.state::<AppState>();
        let cfg = state.config.lock().unwrap();
        let inst = cfg.instances.iter().find(|i| i.id == instance_id)?;
        let home = cfg
            .homes
            .iter()
            .find(|h| h.id == inst.home_id)
            .map(crate::wsl::home_fs_path)?;
        crate::icons::local_icon_path(&home, instance_id)
    };
    let read = crate::wsl::run_blocking(move || std::fs::read(path)).await;
    match read {
        Ok(Ok(bytes)) => Some(bytes),
        Ok(Err(e)) => {
            crate::log_warn!("实例 {instance_id} 的本地托盘图标读取失败: {e}");
            None
        }
        Err(e) => {
            crate::log_warn!("实例 {instance_id} 的本地托盘图标读取失败: {e}");
            None
        }
    }
}

fn decode_icon(bytes: &[u8]) -> Option<Image<'static>> {
    match Image::from_bytes(bytes) {
        Ok(img) => Some(img),
        Err(e) => {
            crate::log_warn!("托盘图标解码失败: {e}");
            None
        }
    }
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

/// The launcher tray's menu: open launcher / running profiles / quit.
fn build_launcher_menu(
    app: &AppHandle,
    running: &[RunningItem],
) -> tauri::Result<Menu<tauri::Wry>> {
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

/// One instance tray's menu: open its window, stop it. The (disabled) title
/// row keeps two instance trays apart when their icons look alike.
fn build_instance_menu(
    app: &AppHandle,
    instance_id: &str,
    name: &str,
) -> tauri::Result<Menu<tauri::Wry>> {
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
    Menu::with_items(app, &[&title, &sep, &open, &stop])
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

#[cfg(test)]
mod tests {
    use super::*;

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
