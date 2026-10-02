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

/// Window-scoped compatibility report event name.
pub const EARLY_LOADING_COMPAT_EVENT: &str = "early-loading://compatibility";

/// Window-scoped provider self-check report event name.
pub const EARLY_LOADING_PROVIDER_EVENT: &str = "early-loading://provider";

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
    /// Provider self-check report already computed for this launch, if any.
    ///
    /// Delivered here instead of only by `EARLY_LOADING_PROVIDER_EVENT`:
    /// the report is produced by a fast local-IO check and the webview needs
    /// a moment to mount and register its listener, so an event-only relay
    /// can fire before anyone is listening and be dropped silently (Tauri
    /// does not buffer events for absent subscribers). The frontend seeds its
    /// state from this field and then applies the event, so either arrival
    /// order renders the same report. The event is kept for the case where the
    /// page is already up (e.g. a re-run within a live window).
    pub provider_report: Option<Vec<crate::providers::ProviderRouteReport>>,
}

fn window_label(instance_id: &str) -> String {
    format!("{EARLY_LOADING_LABEL_PREFIX}{instance_id}")
}

/// Opens (or focuses) the frameless early-loading window for one instance.
///
/// Must be an async command: Tauri runs async commands on the runtime where
/// window creation is marshalled to the main thread. A sync command runs on a
/// bare worker thread, and building a WebView2 window there crashes the
/// process on Windows (the exact white-window-and-crash symptom).
#[tauri::command(rename_all = "snake_case")]
pub async fn open_early_loading_window(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    instance_id: String,
) -> Result<(), String> {
    let label = window_label(&instance_id);
    // A new launch invalidates any report left over from an earlier one: the
    // launch driver reports before the webview mounts, so a report can be
    // stashed while no window exists (e.g. the previous open attempt failed).
    // Draining here keeps a stale report from being replayed into this launch.
    state
        .launch_provider_reports
        .lock()
        .unwrap()
        .remove(&instance_id);
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
    let mut builder = WebviewWindowBuilder::new(&app, label.clone(), url)
        .title(format!("正在启动 {name} — DSH Launcher"))
        // Compact by default; the frontend grows the height when an inline
        // compatibility report arrives (see EarlyLoading.vue resizeFor).
        .inner_size(520.0, 320.0)
        .resizable(false)
        .decorations(false)
        .center();
    // Own WebView2 user-data folder: sharing the default (exe-adjacent)
    // folder with the main window wedges the second webview on a white page.
    if let Some(dir) = crate::windows::app_webview_data_dir(&app, &label) {
        builder = builder.data_directory(dir);
    }
    builder.build().map_err(|e| e.to_string())?;
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
        instance_id: instance_id.clone(),
        name: inst.name.clone(),
        profile: inst.last_profile.clone().or(inst.default_profile.clone()),
        // Take the pending report so a later re-open of the window does not
        // replay a stale report; a report produced after this call still
        // arrives by event.
        provider_report: state
            .launch_provider_reports
            .lock()
            .unwrap()
            .remove(&instance_id),
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

/// Forwards the compatibility report to the early-loading window, which
/// renders it inline (instead of the launcher main window's modal). The
/// compatibility check is part of the launch, so its result belongs in the
/// launch window. No-op when the window is closed (launch continues).
///
/// The report travels as a JSON string: `compatibility::Report` is
/// Serialize-only (it is never deserialized anywhere), and command args
/// require Deserialize.
#[tauri::command(rename_all = "snake_case")]
pub fn report_launch_compat(
    app: AppHandle,
    instance_id: String,
    report_json: String,
) -> Result<(), String> {
    let label = window_label(&instance_id);
    if app.get_webview_window(&label).is_none() {
        return Ok(());
    }
    // Validate before relaying so a malformed payload fails loudly here.
    let report: serde_json::Value =
        serde_json::from_str(&report_json).map_err(|e| format!("无效的兼容性报告 JSON: {e}"))?;
    app.emit_to(&label, EARLY_LOADING_COMPAT_EVENT, report)
        .map_err(|e| e.to_string())
}

/// Forwards the provider pre-launch self-check report to the early-loading
/// window, which renders it inline (alongside the compatibility report). The
/// check (`providers::check_provider_routes`) is advisory and never blocks
/// the launch, so this is a no-op when the window is closed (launch
/// continues).
///
/// The report travels as a JSON string: `providers::ProviderRouteReport` is
/// Serialize-only and command args require Deserialize.
#[tauri::command(rename_all = "snake_case")]
pub fn report_launch_provider(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    instance_id: String,
    report_json: String,
) -> Result<(), String> {
    // Validate before relaying so a malformed payload fails loudly here, and
    // parse into the typed report so it can be stashed for the context read.
    let report: Vec<crate::providers::ProviderRouteReport> = serde_json::from_str(&report_json)
        .map_err(|e| format!("无效的供应商自检报告 JSON: {e}"))?;
    // Stash first, unconditionally: the self-check is fast local IO and the
    // window's webview may not have mounted yet, so the event alone can be
    // dropped. `get_early_loading_context` hands the stashed report to the
    // page, which makes both arrival orders work.
    state
        .launch_provider_reports
        .lock()
        .unwrap()
        .insert(instance_id.clone(), report.clone());
    let label = window_label(&instance_id);
    if app.get_webview_window(&label).is_none() {
        // Window already closed (user clicked 关闭): the launch continues
        // silently, the report is a no-op by design.
        return Ok(());
    }
    app.emit_to(&label, EARLY_LOADING_PROVIDER_EVENT, report)
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
