//! Exit admission based on fresh, per-process Host evidence, never cache mtimes.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

const PREFIX: &str = "DSH_LAUNCHER_ACTIVITY:";
const MAX_AGE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct ExitProtection {
    approved: AtomicBool,
    pending: AtomicBool,
    decisions: tokio::sync::Mutex<()>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Impact {
    pub active: bool,
    pub scheduled: bool,
    pub unknown: bool,
}
impl Impact {
    fn requires_confirmation(self) -> bool {
        self.active || self.scheduled || self.unknown
    }
    fn merge(&mut self, other: Self) {
        self.active |= other.active;
        self.scheduled |= other.scheduled;
        self.unknown |= other.unknown;
    }
}

pub struct HostActivity {
    pub nonce: String,
    sample: Option<(Instant, Impact)>,
    bridge_dir: Option<PathBuf>,
}
impl HostActivity {
    pub fn new(nonce: String) -> Self {
        Self {
            nonce,
            sample: None,
            bridge_dir: None,
        }
    }
    pub fn receive(&mut self, line: &str) -> bool {
        let Some(body) = line.strip_prefix(&format!("{PREFIX}{}:", self.nonce)) else {
            return false;
        };
        let (impact, age) = parse_sample(body).unwrap_or((
            Impact {
                unknown: true,
                ..Default::default()
            },
            Duration::ZERO,
        ));
        self.sample = Some((Instant::now() - age, impact));
        true
    }
    pub fn impact(&self) -> Impact {
        match self.sample {
            Some((at, impact)) if at.elapsed() <= MAX_AGE => impact,
            _ => Impact {
                unknown: true,
                ..Default::default()
            },
        }
    }
}
impl Drop for HostActivity {
    fn drop(&mut self) {
        if let Some(dir) = &self.bridge_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
fn parse_sample(body: &str) -> Option<(Impact, Duration)> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    let impact = parse_sample_at(body, now)?;
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let age = now.checked_sub(value.get("sampledAt")?.as_u64()?)?;
    Some((impact, Duration::from_millis(age)))
}
fn parse_sample_at(body: &str, now: u64) -> Option<Impact> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    if value.get("version")?.as_u64()? != 1 {
        return None;
    }
    let sampled_at = value.get("sampledAt")?.as_u64()?;
    if now.checked_sub(sampled_at)? > MAX_AGE.as_millis() as u64 {
        return None;
    }
    if value.get("unknown").and_then(|v| v.as_bool()) == Some(true) {
        return Some(Impact {
            unknown: true,
            ..Default::default()
        });
    }
    Some(Impact {
        active: value.get("active")?.as_bool()?,
        scheduled: value.get("scheduled")?.as_bool()?,
        unknown: false,
    })
}

/// Only inject when the actual CLI advertises the known patch-list contract.
/// Unsupported/WSL/TUI carriers stay unknown, without risking their startup.
pub fn prepare_bridge(
    data_dir: &Path,
    version: &Path,
) -> Result<Option<(PathBuf, HostActivity)>, String> {
    let bin = crate::process::version_bin(version);
    let Some(cli) = bin.parent() else {
        return Ok(None);
    };
    let supported = std::fs::read_to_string(cli.join("args.js"))
        .or_else(|_| std::fs::read_to_string(cli.join("args.ts")))
        .is_ok_and(|s| s.contains("--patch <path>") && s.contains("patch-list overlay"));
    if !supported {
        return Ok(None);
    }
    let nonce = uuid::Uuid::new_v4().to_string();
    let dir = data_dir.join("exit-inspection").join(&nonce);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut activity = HostActivity::new(nonce.clone());
    activity.bridge_dir = Some(dir.clone());
    let module = dir.join("inspector.mjs");
    std::fs::write(&module, include_str!("../assets/exit-inspector.mjs"))
        .map_err(|e| e.to_string())?;
    let module_url =
        tauri::Url::from_file_path(&module).map_err(|_| "退出状态桥接需要绝对路径".to_string())?;
    let patch = dir.join("bridge.yml");
    // JSON is YAML-compatible; quoting protects Windows paths and spaces.
    let value = serde_json::json!([{ "insert": [{ "id": format!("launcher-exit-inspector-{nonce}"), "name": module_url.as_str(),
        "config": { "nonce": nonce.clone() } }] }]);
    std::fs::write(&patch, value.to_string()).map_err(|e| e.to_string())?;
    Ok(Some((patch, activity)))
}

async fn impact(app: &AppHandle, target: Option<&str>) -> Impact {
    let state = app.state::<crate::AppState>();
    let mut result = Impact::default();
    for (id, entry) in state.running.lock().await.iter() {
        if target.is_none_or(|target| target == id) {
            result.merge(entry.exit_impact());
        }
    }
    if state
        .tui_sessions
        .lock()
        .await
        .keys()
        .any(|id| target.is_none_or(|target| target == id))
    {
        result.unknown = true;
    }
    if target.is_none()
        && state.tasks.lock().await.values().any(|task| {
            matches!(
                task.state,
                crate::tasks::TaskState::Running | crate::tasks::TaskState::Queued
            )
        })
    {
        result.active = true;
    }
    result
}

async fn confirm(app: &AppHandle, target: Option<&str>) -> bool {
    let impact = impact(app, target).await;
    if !impact.requires_confirmation() {
        return true;
    }
    let english = app
        .state::<crate::AppState>()
        .config
        .lock()
        .unwrap()
        .settings
        .locale
        .starts_with("en");
    let mut lines = Vec::new();
    if impact.active {
        lines.push(if english {
            "Running or queued tasks will be interrupted."
        } else {
            "正在执行或排队的任务将被中断。"
        });
    }
    if impact.scheduled {
        lines.push(if english {
            "Armed scheduled tasks will not run while their instance is closed."
        } else {
            "已启用的定时任务在实例关闭期间不会运行。"
        });
    }
    if impact.unknown {
        lines.push(if english { "Some task states are unknown (unsupported, not ready, or stale). Work may still be running." } else { "部分实例无法确认任务状态（不支持、尚未就绪或响应已过期），可能仍有任务运行。" });
    }
    let (title, action, cancel) = match (english, target.is_some()) {
        (true, true) => ("Stop this instance?", "Stop instance", "Cancel"),
        (true, false) => ("Quit launcher?", "Quit launcher", "Cancel"),
        (false, true) => ("停止此实例？", "停止实例", "取消"),
        (false, false) => ("退出启动器？", "退出启动器", "取消"),
    };
    let app = app.clone();
    let message = lines.join("\n");
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .message(message)
            .title(title)
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                action.into(),
                cancel.into(),
            ))
            .blocking_show()
    })
    .await
    .unwrap_or(false)
}

/// Called by both the native exit event and the tray. Coalesces repeated quits.
pub fn request_quit(app: &AppHandle) {
    let guard = app.state::<ExitProtection>();
    if guard.pending.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let guard = app.state::<ExitProtection>();
        let _decision = guard.decisions.lock().await;
        if confirm(&app, None).await {
            guard.approved.store(true, Ordering::SeqCst);
            app.exit(0);
        } else {
            guard.pending.store(false, Ordering::SeqCst);
        }
    });
}
pub fn approved(app: &AppHandle) -> bool {
    app.state::<ExitProtection>()
        .approved
        .load(Ordering::SeqCst)
}

fn generation_changed(
    before: &Option<std::sync::Arc<tokio::sync::Notify>>,
    after: Option<&std::sync::Arc<tokio::sync::Notify>>,
) -> bool {
    after.is_some_and(|after| {
        before
            .as_ref()
            .is_none_or(|before| !std::sync::Arc::ptr_eq(before, after))
    })
}

pub async fn stop_instance(app: &AppHandle, id: &str) -> Result<(), String> {
    let protection = app.state::<ExitProtection>();
    let _decision = protection
        .decisions
        .try_lock()
        .map_err(|_| "已有退出或停止确认等待处理".to_string())?;
    let state = app.state::<crate::AppState>();
    let web_generation = state
        .running
        .lock()
        .await
        .get(id)
        .map(|entry| entry.kill.clone());
    let tui_generation = state
        .tui_sessions
        .lock()
        .await
        .get(id)
        .map(|entry| entry.kill.clone());
    if !confirm(app, Some(id)).await {
        return Err("已取消停止实例".into());
    }
    {
        let web = state.running.lock().await;
        let tui = state.tui_sessions.lock().await;
        if generation_changed(&web_generation, web.get(id).map(|entry| &entry.kill))
            || generation_changed(&tui_generation, tui.get(id).map(|entry| &entry.kill))
        {
            return Err("实例状态已改变，请重新确认停止".into());
        }
    }
    if let Some(expected) = tui_generation {
        crate::tui::stop_tui_session(app, &state, id, &expected).await?;
        let wait = async {
            loop {
                let tui = state.tui_sessions.lock().await;
                match tui.get(id) {
                    None => return Ok(()),
                    Some(current) if !std::sync::Arc::ptr_eq(&expected, &current.kill) => {
                        return Err("实例已重新启动，请重新确认停止".to_string())
                    }
                    Some(_) => {}
                }
                drop(tui);
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .map_err(|_| "等待 TUI 实例退出超时".to_string())??;
    }
    if let Some(expected) = web_generation {
        crate::process::stop_instance_process_if(app, &state, id, Some(&expected)).await?;
    }
    let web = state.running.lock().await;
    let tui = state.tui_sessions.lock().await;
    if web.contains_key(id) || tui.contains_key(id) {
        return Err("实例状态已改变，请重新确认停止".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generation_admission_rejects_new_or_replaced_processes() {
        use std::sync::Arc;
        let old = Arc::new(tokio::sync::Notify::new());
        let new = Arc::new(tokio::sync::Notify::new());
        assert!(!generation_changed(&None, None));
        assert!(generation_changed(&None, Some(&new)));
        assert!(!generation_changed(&Some(old.clone()), Some(&old)));
        assert!(generation_changed(&Some(old.clone()), Some(&new)));
        assert!(!generation_changed(&Some(old), None));
    }
    #[test]
    fn strict_schema_never_mistakes_bad_data_for_idle() {
        for bad in [
            "{}",
            "null",
            r#"{"version":2,"active":false,"scheduled":false}"#,
            r#"{"version":1,"active":false}"#,
        ] {
            assert_eq!(parse_sample_at(bad, 1000), None);
        }
        assert_eq!(
            parse_sample_at(
                r#"{"version":1,"sampledAt":1000,"active":false,"scheduled":false}"#,
                1000
            ),
            Some(Impact::default())
        );
    }
    #[test]
    fn delayed_delivery_and_clock_skew_are_unknown() {
        let frame = r#"{"version":1,"sampledAt":1000,"active":false,"scheduled":false}"#;
        assert_eq!(parse_sample_at(frame, 4000), None);
        assert_eq!(parse_sample_at(frame, 999), None);
        assert_eq!(parse_sample_at(frame, 1100), Some(Impact::default()));
    }
    #[test]
    fn samples_are_bound_to_process_and_expire() {
        let mut host = HostActivity::new("fresh".into());
        assert!(host.impact().unknown);
        assert!(!host.receive(
            "DSH_LAUNCHER_ACTIVITY:old:{\"version\":1,\"active\":false,\"scheduled\":false}"
        ));
        assert!(host.impact().unknown);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        assert!(host.receive(&format!("DSH_LAUNCHER_ACTIVITY:fresh:{{\"version\":1,\"sampledAt\":{now},\"active\":false,\"scheduled\":false}}")));
        assert!(!host.impact().requires_confirmation());
        host.sample.as_mut().unwrap().0 = Instant::now() - Duration::from_secs(3);
        assert!(host.impact().unknown);
        host.receive("DSH_LAUNCHER_ACTIVITY:fresh:bad");
        assert!(host.impact().unknown);
    }
    #[test]
    fn multi_instance_aggregation_keeps_all_reasons() {
        let mut total = Impact::default();
        total.merge(Impact {
            active: true,
            ..Default::default()
        });
        total.merge(Impact {
            scheduled: true,
            ..Default::default()
        });
        total.merge(Impact {
            unknown: true,
            ..Default::default()
        });
        total.merge(Impact::default());
        assert_eq!(
            total,
            Impact {
                active: true,
                scheduled: true,
                unknown: true
            }
        );
    }
}
