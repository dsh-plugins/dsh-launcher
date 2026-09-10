use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::AppState;

/// Remembers the instance whose window was interacted with most recently so
/// the tray double-click can reopen exactly that profile page.
fn record_focus(app: &AppHandle, instance_id: &str) {
    if let Some(state) = app.try_state::<AppState>() {
        *state.last_focused_instance.lock().unwrap() = Some(instance_id.to_string());
    }
}

/// Hides the launcher main window when the user enabled
/// "hide launcher on window open" in the launch-behavior settings.
fn maybe_hide_main(app: &AppHandle) {
    let hide = app
        .try_state::<AppState>()
        .map(|state| {
            state
                .config
                .lock()
                .unwrap()
                .settings
                .hide_launcher_on_window_open
        })
        .unwrap_or(false);
    if hide {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.hide();
        }
    }
}

/// Opens (or focuses) the webview window hosting the instance's DSH Web GUI.
pub fn open_instance_window(
    app: &AppHandle,
    instance_id: &str,
    name: &str,
    url: &str,
) -> Result<(), String> {
    let label = format!("instance-{instance_id}");
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        record_focus(app, instance_id);
        maybe_hide_main(app);
        return Ok(());
    }
    let parsed = WebviewUrl::External(url.parse().map_err(|e| format!("无效的 URL {url}: {e}"))?);
    let win = WebviewWindowBuilder::new(app, label, parsed)
        .title(format!("{name} — DSH"))
        .inner_size(1024.0, 576.0)
        .min_inner_size(800.0, 500.0)
        .center()
        .build()
        .map_err(|e| e.to_string())?;
    record_focus(app, instance_id);
    maybe_hide_main(app);

    // Track focus so the tray knows which profile page the user used last.
    let handle = app.clone();
    let id = instance_id.to_string();
    win.on_window_event(move |event| {
        if let WindowEvent::Focused(true) = event {
            record_focus(&handle, &id);
        }
    });
    Ok(())
}

/// Closes the instance's webview window if it is open.
pub fn close_instance_window(app: &AppHandle, instance_id: &str) {
    let label = format!("instance-{instance_id}");
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.close();
    }
}

/// Opens (or focuses) the terminal window hosting a TUI instance's PTY
/// session. The window loads the app's own `/terminal/:id` route; its
/// frontend starts / reattaches the PTY session. Closing the window only
/// detaches the view — the session keeps running in the background.
pub fn open_tui_window(app: &AppHandle, instance_id: &str) -> Result<(), String> {
    let label = format!("tui-{instance_id}");
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        record_focus(app, instance_id);
        maybe_hide_main(app);
        return Ok(());
    }
    let name = app
        .try_state::<AppState>()
        .and_then(|state| {
            state
                .config
                .lock()
                .unwrap()
                .instances
                .iter()
                .find(|i| i.id == instance_id)
                .map(|i| i.name.clone())
        })
        .unwrap_or_else(|| instance_id.to_string());
    // Hash router: the route must arrive in the fragment, or the window
    // would land on `/` and show the launcher home instead of the terminal.
    let url = WebviewUrl::App(format!("/index.html#/terminal/{instance_id}").into());
    let win = WebviewWindowBuilder::new(app, label, url)
        .title(format!("{name} — DSH TUI"))
        .inner_size(900.0, 560.0)
        .min_inner_size(560.0, 320.0)
        .center()
        .build()
        .map_err(|e| e.to_string())?;
    record_focus(app, instance_id);
    maybe_hide_main(app);

    let handle = app.clone();
    let id = instance_id.to_string();
    win.on_window_event(move |event| {
        if let WindowEvent::Focused(true) = event {
            record_focus(&handle, &id);
        }
    });
    Ok(())
}

/// Closes a TUI instance's terminal window if it is open (detach only; the
/// PTY session is owned by the backend, not the window).
pub fn close_tui_window(app: &AppHandle, instance_id: &str) {
    let label = format!("tui-{instance_id}");
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.close();
    }
}
