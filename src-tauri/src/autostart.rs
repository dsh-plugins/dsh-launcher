//! Silent autostart and instance auto-launch (issue #100).
//!
//! The OS autostart entry registers the launcher with `--autostart` as an
//! extra argument (see `lib.rs`). A launch carrying that flag:
//!
//! - keeps the main window hidden when `settings.silent_autostart` is set,
//!   so boot goes straight to the tray;
//! - starts every instance listed in `settings.autostart_instances`, either
//!   headless or with its window opened, depending on
//!   `settings.autostart_open_windows`.
//!
//! Everything here is best-effort: a failed launch is logged and surfaced to
//! the user later, never a reason to abort startup — a user who enabled
//! autostart must still get their launcher when one instance is broken.

use tauri::{AppHandle, Manager};

use crate::AppState;

/// The marker the autostart entry passes to the launcher. It must be a plain
/// argument (Windows Run keys and macOS LaunchAgents both store an argv), so
/// `tauri-plugin-autostart` is initialized with it.
pub const AUTOSTART_ARG: &str = "--autostart";

/// Whether this process was started by the OS autostart entry.
pub fn launched_by_autostart() -> bool {
    std::env::args().any(|a| a == AUTOSTART_ARG)
}

/// Applies the silent-startup half of issue #100: hides the main window when
/// an autostart launch asked for it. Returns whether the window was hidden.
///
/// Must run after the window exists but before it is first shown; the window
/// is created visible in `tauri.conf.json`, so this hides it immediately and
/// the resulting flash is the price of not knowing the launch source until
/// argv is readable (which it already is at this point).
pub fn hide_main_for_silent_autostart(app: &AppHandle) -> bool {
    if !launched_by_autostart() {
        return false;
    }
    let silent = app
        .state::<AppState>()
        .config
        .lock()
        .unwrap()
        .settings
        .silent_autostart;
    if !silent {
        return false;
    }
    match app.get_webview_window("main") {
        Some(win) => {
            if let Err(e) = win.hide() {
                crate::log_warn!("静默自启隐藏主窗口失败: {e}");
                return false;
            }
            crate::log_info!("静默自启：主窗口保持隐藏，应用仅驻留托盘");
            true
        }
        None => false,
    }
}

/// One instance to start after an autostart launch, as resolved from settings.
#[derive(Clone, Debug)]
struct PendingLaunch {
    instance_id: String,
    name: String,
    profile: String,
}

/// Resolves the configured autostart instances into concrete launches,
/// dropping entries whose instance is gone or has no usable profile.
///
/// Pure over the config so it is testable without a Tauri runtime.
fn resolve_launches(cfg: &crate::config::Config) -> Vec<PendingLaunch> {
    cfg.settings
        .autostart_instances
        .iter()
        .filter_map(|entry| {
            let inst = cfg.instances.iter().find(|i| i.id == entry.instance_id)?;
            // An explicitly pinned profile wins; otherwise fall back the same
            // way a Home-page launch does (default, then last used).
            let profile = entry
                .profile
                .clone()
                .or_else(|| inst.default_profile.clone())
                .or_else(|| inst.last_profile.clone())?;
            Some(PendingLaunch {
                instance_id: inst.id.clone(),
                name: inst.name.clone(),
                profile,
            })
        })
        .collect()
}

fn resolve_pending_launches(state: &AppState) -> Vec<PendingLaunch> {
    resolve_launches(&state.config.lock().unwrap())
}

/// Starts the configured instances after an autostart launch (issue #100).
///
/// Runs detached from startup: each instance is spawned in turn, and when
/// `autostart_open_windows` is on the launcher waits for the instance's ready
/// signal before opening its window, exactly like a Home-page launch.
pub fn spawn_autostart_launches(app: &AppHandle) {
    if !launched_by_autostart() {
        return;
    }
    let pending = {
        let state = app.state::<AppState>();
        resolve_pending_launches(&state)
    };
    if pending.is_empty() {
        return;
    }
    let open_windows = app
        .state::<AppState>()
        .config
        .lock()
        .unwrap()
        .settings
        .autostart_open_windows;
    crate::log_info!(
        "自启后拉起 {} 个实例（{}）",
        pending.len(),
        if open_windows {
            "打开窗口"
        } else {
            "静默运行"
        }
    );
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        for launch in pending {
            if let Err(e) = start_one(&handle, &launch, open_windows).await {
                crate::log_warn!("自启拉起实例「{}」失败: {e}", launch.name);
            }
        }
    });
}

async fn start_one(
    app: &AppHandle,
    launch: &PendingLaunch,
    open_window: bool,
) -> Result<(), String> {
    crate::commands::start_instance(
        app.clone(),
        app.state::<AppState>(),
        launch.instance_id.clone(),
        launch.profile.clone(),
    )
    .await?;
    if !open_window {
        return Ok(());
    }
    // `start_instance` returns right after the spawn; the DSH window is only
    // meaningful once the instance reports a URL, so poll like the Home-page
    // launch path does before opening it.
    let id = launch.instance_id.clone();
    for _ in 0..240 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let state = app.state::<AppState>();
        let (published, alive) = {
            let running = state.running.lock().await;
            match running.get(&id) {
                Some(r) => (r.url.clone().filter(|u| !u.is_empty()), true),
                None => (None, false),
            }
        };
        match published {
            Some(url) => {
                let name = launch.name.clone();
                crate::windows::open_instance_window(app, &id, &name, &url)?;
                return Ok(());
            }
            // The entry is gone again: the process died before publishing a
            // URL, so report it instead of waiting out the whole timeout.
            None if !alive => return Err("实例在就绪前已退出".to_string()),
            None => {}
        }
    }
    Err("等待实例就绪超时".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AutostartInstance, Config, DshInstance, LauncherSettings};

    fn instance(id: &str, name: &str, default: Option<&str>, last: Option<&str>) -> DshInstance {
        DshInstance {
            id: id.into(),
            name: name.into(),
            version_id: "v".into(),
            home_id: "h".into(),
            env_overrides: Default::default(),
            default_profile: default.map(str::to_string),
            last_profile: last.map(str::to_string),
            icon: None,
            port: None,
        }
    }

    fn config_with(instances: Vec<DshInstance>, entries: Vec<AutostartInstance>) -> Config {
        Config {
            instances,
            settings: LauncherSettings {
                autostart_instances: entries,
                ..LauncherSettings::default()
            },
            ..Config::default()
        }
    }

    #[test]
    fn autostart_flag_is_matched_exactly() {
        // The marker must not be confused with a prefix argument.
        assert_ne!(AUTOSTART_ARG, "--autostart-silent");
        assert!(AUTOSTART_ARG.starts_with("--"));
    }

    #[test]
    fn resolved_launches_prefer_the_pinned_profile() {
        let cfg = config_with(
            vec![instance("i1", "Main", Some("web"), None)],
            vec![AutostartInstance {
                instance_id: "i1".into(),
                profile: Some("tui".into()),
            }],
        );
        let launches = resolve_launches(&cfg);
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0].profile, "tui");
    }

    #[test]
    fn resolved_launches_fall_back_to_default_then_last_profile() {
        let cfg = config_with(
            vec![
                instance("i1", "Default", Some("web"), Some("other")),
                instance("i2", "LastOnly", None, Some("tui")),
            ],
            vec![
                AutostartInstance {
                    instance_id: "i1".into(),
                    profile: None,
                },
                AutostartInstance {
                    instance_id: "i2".into(),
                    profile: None,
                },
            ],
        );
        let launches = resolve_launches(&cfg);
        assert_eq!(launches[0].profile, "web");
        assert_eq!(launches[1].profile, "tui");
    }

    #[test]
    fn resolved_launches_skip_stale_or_profileless_entries() {
        // A deleted instance, and an instance with no profile recorded at all,
        // must not turn every boot into a failed launch.
        let cfg = config_with(
            vec![instance("i1", "NoProfile", None, None)],
            vec![
                AutostartInstance {
                    instance_id: "deleted".into(),
                    profile: None,
                },
                AutostartInstance {
                    instance_id: "i1".into(),
                    profile: None,
                },
            ],
        );
        assert!(resolve_launches(&cfg).is_empty());
    }
}
