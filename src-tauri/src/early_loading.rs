//! Frameless early-loading window for instance launches.
//!
//! Between clicking "Start" (Home page / compatible launch) or a desktop
//! shortcut cold start (`dsh-launcher://launch`) and the DSH window being
//! fully up, a small frameless window (`early-loading-<instance>`) shows the
//! current launch stage and progress. The compatibility check is driven from
//! this window too (the launcher-side preflight stays advisory). The window
//! closes itself once the DSH window opens; its footer offers a plain close
//! (launch continues) and a "cancel launch" button that stops the spawn.
//!
//! Progress is reported by the frontend through `report_launch_stage` and
//! forwarded to this window only, so the normal launcher pages stay silent.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::AppState;

/// Prefix of the early-loading window labels (`early-loading-<instance_id>`).
const EARLY_LOADING_LABEL_PREFIX: &str = "early-loading-";

/// Window-scoped progress event name.
pub const EARLY_LOADING_EVENT: &str = "early-loading://progress";

/// Launch stages in display order, mirrored by the frontend's pipeline.
pub const STAGES: [&str; 4] = ["preflight", "spawning", "waiting-ready", "opening-window"];

/// Terminal stages: the window shows the outcome briefly, then closes.
const TERMINAL_STAGES: [&str; 3] = ["done", "cancelled", "failed"];

#[derive(Clone, Debug, Serialize)]
pub struct EarlyLoadingProgress {
    pub instance_id: String,
    /// One of STAGES, or a terminal state: done | cancelled | failed.
    pub stage: String,
    /// 0-100 progress within the stage; `None` renders indeterminate.
    pub percent: Option<u8>,
    /// Optional human-readable detail (error message, check summary, …).
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EarlyLoadingContext {
    pub instance_id: String,
    pub name: String,
    pub profile: Option<String>,
}

fn window_label(instance_id: &str) -> String {
    format!("{EARLY_LOADING_LABEL_PREFIX}{instance_id}")
}

/// Opens (or focuses) the frameless early-loading window for one instance.
#[tauri::command(rename_all = "snake_case")]
pub fn open_early_loading_window(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    instance_id: String,
) -> Result<(), String> {
    let label = window_label(&instance_id);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        return Ok(());
    }
    let name = state
        .config
        .lock()
        .unwrap()
        .instances
        .iter()
        .find(|i| i.id == instance_id)
        .map(|i| i.name.clone())
        .unwrap_or_else(|| instance_id.clone());
    // Hash router: the route must arrive in the fragment (see windows.rs).
    let url = WebviewUrl::App(format!("/index.html#/early-loading/{instance_id}").into());
    WebviewWindowBuilder::new(&app, label, url)
        .title(format!("正在启动 {name} — DSH Launcher"))
        .inner_size(480.0, 340.0)
        .resizable(false)
        .decorations(false)
        .center()
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Closes the early-loading window if it is open.
#[tauri::command(rename_all = "snake_case")]
pub fn close_early_loading_window(app: AppHandle, instance_id: String) {
    close_early_loading(&app, &instance_id);
}

/// Non-command close helper for internal call sites (process waiter).
pub(crate) fn close_early_loading(app: &AppHandle, instance_id: &str) {
    if let Some(win) = app.get_webview_window(&window_label(instance_id)) {
        let _ = win.close();
    }
}

/// Title/profile context for the early-loading window's frontend.
#[tauri::command(rename_all = "snake_case")]
pub fn get_early_loading_context(
    state: tauri::State<'_, AppState>,
    instance_id: String,
) -> Result<EarlyLoadingContext, String> {
    let cfg = state.config.lock().unwrap();
    let inst = cfg
        .instances
        .iter()
        .find(|i| i.id == instance_id)
        .ok_or_else(|| "实例不存在".to_string())?;
    Ok(EarlyLoadingContext {
        instance_id,
        name: inst.name.clone(),
        profile: inst.last_profile.clone().or(inst.default_profile.clone()),
    })
}

/// Forwards a launch-stage update to the early-loading window. The frontend
/// (the page driving the launch) owns the pipeline; the backend only relays.
#[tauri::command(rename_all = "snake_case")]
pub fn report_launch_stage(
    app: AppHandle,
    instance_id: String,
    stage: String,
    percent: Option<u8>,
    detail: Option<String>,
) -> Result<(), String> {
    // Invoke is not constrained by the frontend's TS union type; reject
    // anything outside the known pipeline/terminal stages.
    if !STAGES.contains(&stage.as_str()) && !TERMINAL_STAGES.contains(&stage.as_str()) {
        return Err(format!("无效的启动阶段: {stage}"));
    }
    let label = window_label(&instance_id);
    if app.get_webview_window(&label).is_none() {
        // Window already closed (user clicked 关闭): the launch continues
        // silently, the report is a no-op by design.
        return Ok(());
    }
    app.emit_to(
        &label,
        EARLY_LOADING_EVENT,
        EarlyLoadingProgress {
            instance_id,
            stage,
            percent,
            detail,
        },
    )
    .map_err(|e| e.to_string())
}

/// Cancels a launch in progress: records the cancel intent (so a late
/// `open_instance_window` from the launch driver is refused), then stops the
/// instance if it is already spawned. The early-loading window stays open —
/// the process waiter closes it once the stop has actually converged, and
/// its own close button remains available as an escape hatch.
#[tauri::command(rename_all = "snake_case")]
pub async fn cancel_instance_launch(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    instance_id: String,
) -> Result<(), String> {
    crate::log_info!("用户取消启动实例 {instance_id}");
    state
        .launch_cancels
        .lock()
        .unwrap()
        .insert(instance_id.clone());
    // Forget any pending compatibility-TUI handoff, then stop the process
    // (idempotent: a not-yet-spawned instance just logs a debug line and
    // re-emits `stopped`, which the waiter-style cleanup handles).
    state.tui_compat.lock().await.remove(&instance_id);
    crate::process::stop_instance_process(&app, &state, &instance_id).await
}
