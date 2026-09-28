//! Frameless update-notice window and the startup update check.
//!
//! The launcher checks GitHub releases on startup without blocking the main
//! window (the check runs in a spawned task after setup). When a newer
//! release exists — and the user has not chosen "never remind" for exactly
//! that version — a small frameless window (`update-notice`) opens with a
//! custom draggable title bar. The window is a real OS window, not an
//! in-app modal: it survives the main window hiding to the tray and can be
//! dragged anywhere on screen.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::AppState;

/// Label of the update-notice webview window.
const UPDATE_WINDOW_LABEL: &str = "update-notice";

/// Opens (or focuses) the frameless update-notice window. The found release
/// travels in the URL query so the window renders immediately instead of
/// re-hitting the GitHub API (and possibly seeing a different version).
pub fn open_update_window(
    app: &AppHandle,
    info: &crate::update::LauncherUpdateInfo,
) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(UPDATE_WINDOW_LABEL) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        return Ok(());
    }
    // Hash router: the route must arrive in the fragment, or the window would
    // land on `/` and render the full launcher shell inside the small window.
    let mut url = String::from("/index.html#/update-notice");
    if let (Some(latest), Some(link)) = (info.latest.as_deref(), info.url.as_deref()) {
        url.push_str(&format!(
            "?version={}&url={}",
            urlencoding(latest),
            urlencoding(link)
        ));
        if let Some(published) = info.published_at.as_deref() {
            url.push_str(&format!("&published={}", urlencoding(published)));
        }
    }
    let url = WebviewUrl::App(url.into());
    WebviewWindowBuilder::new(app, UPDATE_WINDOW_LABEL, url)
        .title("DSH Launcher 更新提醒")
        .inner_size(460.0, 330.0)
        .resizable(false)
        .decorations(false)
        .center()
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Percent-encodes one URL query value.
fn urlencoding(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// "Never remind" on the notice window: remembers the exact version so the
/// startup check skips it (a newer release still prompts).
#[tauri::command]
pub fn dismiss_update_version(
    state: tauri::State<'_, AppState>,
    version: String,
) -> Result<(), String> {
    let version = version.trim().to_string();
    if version.is_empty() {
        return Err("版本号不能为空".to_string());
    }
    let mut cfg = state.config.lock().unwrap();
    if !cfg.settings.update_suppressed.iter().any(|v| v == &version) {
        cfg.settings.update_suppressed.push(version.clone());
    }
    if let Err(e) = crate::commands::save_state(&state, &cfg) {
        // Roll back the in-memory change: a persisted failure must not leave
        // the session believing the version is permanently suppressed.
        cfg.settings.update_suppressed.retain(|v| v != &version);
        return Err(e);
    }
    crate::log_info!("已永久忽略更新版本 {version}");
    Ok(())
}

/// Non-blocking startup update check. Runs after setup; any failure is
/// logged and swallowed — update checks must never break the launch.
pub fn spawn_startup_update_check(app: &AppHandle) {
    let (channel, suppressed) = {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let cfg = state.config.lock().unwrap();
        (
            cfg.settings.update_channel.clone(),
            cfg.settings.update_suppressed.clone(),
        )
    };
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let info = match crate::update::check_launcher_update(Some(channel)).await {
            Ok(info) => info,
            Err(e) => {
                crate::log_warn!("启动时检查更新失败: {e}");
                return;
            }
        };
        if info.up_to_date {
            return;
        }
        // "Never remind" suppresses exactly this version.
        if let Some(latest) = info.latest.as_deref() {
            if suppressed.iter().any(|v| v == latest) {
                crate::log_info!("版本 {latest} 已被用户设为不再提醒");
                return;
            }
        }
        if let Err(e) = open_update_window(&handle, &info) {
            crate::log_warn!("打开更新提醒窗口失败: {e}");
        }
    });
}
