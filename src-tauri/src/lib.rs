mod applog;
mod commands;
mod config;
mod doctor;
mod icons;
mod mcp;
mod modpack;
mod plugins;
mod process;
mod runtime;
mod skills;
mod tasks;
mod terminal;
mod tray;
mod update;
mod windows;

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use tauri::{Emitter, Manager, WindowEvent};

pub struct AppState {
    pub config_path: std::path::PathBuf,
    pub data_dir: std::path::PathBuf,
    pub config: StdMutex<config::Config>,
    pub running: tokio::sync::Mutex<HashMap<String, process::RunningInstance>>,
    pub tasks: tokio::sync::Mutex<HashMap<String, tasks::TaskInfo>>,
    /// One mutex per profile directory, serializing plugin installs and
    /// removals against that profile. `dsh plugin` (pnpm + the bundle
    /// reconcile) is a read-modify-write cycle over the profile's
    /// package.json, so concurrent runs against one profile overwrite each
    /// other and only the last plugin survives.
    pub profile_locks: tokio::sync::Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    /// Instance whose webview window was opened/focused most recently.
    pub last_focused_instance: StdMutex<Option<String>>,
    /// Embedded PTY terminal sessions per instance id.
    pub terminals: tokio::sync::Mutex<HashMap<String, terminal::TerminalSession>>,
}

/// Extracts a `dsh-launcher://…` deep link from process arguments (Windows
/// protocol activation passes the URL as an argv entry).
pub(crate) fn deep_link_from_args(args: &[String]) -> Option<String> {
    args.iter()
        .find(|a| a.starts_with("dsh-launcher://"))
        .cloned()
}

/// Pending cold-start deep link: the frontend pulls this once the webview is
/// ready (events emitted before that would be lost).
#[tauri::command]
fn pending_deep_link() -> Option<String> {
    deep_link_from_args(&std::env::args().collect::<Vec<_>>())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Single instance first: a second launch (e.g. browser protocol
        // activation) forwards its argv to the running instance and exits.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            let link = deep_link_from_args(&argv);
            // launch links are headless: start the instance without popping
            // the launcher window up (issue #9).
            let is_launch = link
                .as_deref()
                .map(|u| u.starts_with("dsh-launcher://launch"))
                .unwrap_or(false);
            if !is_launch {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.unminimize();
                    let _ = win.set_focus();
                }
            }
            if let Some(url) = link {
                crate::log_info!("单实例转发 deep link: {url}");
                let _ = app.emit("deep-link", url);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            // Register the dsh-launcher:// scheme at runtime (Windows/Linux)
            // and forward every deep link to the frontend; the modpack
            // import flow consumes dsh-launcher://pack?url=<tgz>.
            #[cfg(desktop)]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                if let Err(e) = app.deep_link().register("dsh-launcher") {
                    crate::log_warn!("注册 dsh-launcher:// 协议失败: {e}");
                }
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        crate::log_info!("收到 deep link: {url}");
                        let _ = handle.emit("deep-link", url.to_string());
                    }
                });
            }
            // Cold start from a launch shortcut stays silent: hide the main
            // window and let the frontend start the instance (issue #9).
            if deep_link_from_args(&std::env::args().collect::<Vec<_>>())
                .map(|u| u.starts_with("dsh-launcher://launch"))
                .unwrap_or(false)
            {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.hide();
                }
            }
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let config_path = data_dir.join("config.json");
            let cfg = config::load_config(&config_path);

            // Runtime log: rotate the previous latest.log, then apply the
            // configured level (invalid stored values fall back to info).
            let log_level =
                applog::parse_level(&cfg.settings.log_level).unwrap_or(applog::Level::Info);
            if let Err(e) = applog::init(&data_dir.join("logs"), log_level) {
                eprintln!("dsh-launcher: 初始化运行日志失败: {e}");
            }
            crate::log_info!(
                "启动器已启动，版本 {}，数据目录 {}",
                env!("CARGO_PKG_VERSION"),
                data_dir.display()
            );

            app.manage(AppState {
                config_path,
                data_dir: data_dir.clone(),
                config: StdMutex::new(cfg),
                running: tokio::sync::Mutex::new(HashMap::new()),
                tasks: tokio::sync::Mutex::new(HashMap::new()),
                profile_locks: tokio::sync::Mutex::new(HashMap::new()),
                last_focused_instance: StdMutex::new(None),
                terminals: tokio::sync::Mutex::new(HashMap::new()),
            });

            // System tray with dynamic menu.
            tray::build_tray(app.handle())?;

            // Close-to-tray for the main window.
            if let Some(win) = app.get_webview_window("main") {
                let handle = app.handle().clone();
                let win2 = win.clone();
                win.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        let minimize = handle
                            .state::<AppState>()
                            .config
                            .lock()
                            .unwrap()
                            .settings
                            .minimize_to_tray;
                        if minimize {
                            api.prevent_close();
                            let _ = win2.hide();
                        }
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_homes,
            commands::create_home,
            commands::default_dedicated_home_path,
            commands::remove_home,
            commands::list_versions,
            commands::fetch_available_versions,
            commands::remove_version,
            tasks::start_create_instance_task,
            tasks::list_tasks,
            tasks::remove_task,
            tasks::cancel_task,
            runtime::get_runtime_status,
            commands::list_instances,
            commands::create_instance,
            commands::update_instance,
            commands::delete_instance,
            commands::copy_instance,
            commands::list_profiles,
            commands::create_profile,
            commands::copy_profile,
            commands::rename_profile,
            commands::delete_profile,
            commands::start_instance,
            commands::stop_instance,
            commands::check_instance_health,
            commands::list_instance_status,
            commands::open_instance_window,
            commands::open_external,
            commands::create_launch_shortcut,
            pending_deep_link,
            icons::set_instance_icon,
            icons::clear_instance_icon,
            icons::read_instance_icon,
            skills::list_instance_skills,
            skills::install_skill_repo,
            skills::list_repo_skills,
            skills::check_skill_updates,
            skills::import_skill_zip,
            skills::update_skill,
            skills::delete_skill,
            skills::import_skill_file,
            skills::create_skill,
            mcp::list_mcp_servers,
            mcp::save_mcp_server,
            mcp::delete_mcp_server,
            commands::get_settings,
            commands::update_settings,
            commands::fetch_news,
            update::check_launcher_update,
            plugins::fetch_plugin_market,
            plugins::fetch_plugin_versions,
            plugins::list_installed_plugins,
            plugins::set_plugins_enabled,
            plugins::uninstall_plugin,
            plugins::start_install_plugin_task,
            modpack::export_modpack,
            modpack::read_modpack_manifest,
            modpack::start_import_modpack_task,
            terminal::start_terminal_session,
            terminal::write_terminal_input,
            terminal::resize_terminal_session,
            terminal::close_terminal_session,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Terminate child processes when the launcher exits so no DSH
            // instance is left orphaned.
            if let tauri::RunEvent::Exit = event {
                let state = app_handle.state::<AppState>();
                process::kill_all(&state);
                terminal::kill_all(&state);
            }
        });
}
