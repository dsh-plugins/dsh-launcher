use crate::config::{Config, InstanceState, InstanceStatus};
use crate::AppState;
use regex::Regex;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, Notify};

pub const STATUS_EVENT: &str = "instance://status";

/// A live instance process.
pub struct RunningInstance {
    /// Stop signal. The waiter task owns the child and `select!`s on this, so
    /// `stop_instance_process` never touches the child handle directly.
    pub kill: Arc<Notify>,
    pub profile: String,
    pub url: Option<String>,
    /// WSL inner process (distro + captured PID) for a clean stop (issue
    /// #19): killing the local wsl.exe alone could orphan the inner node.
    pub wsl: Option<(String, Arc<Mutex<Option<u32>>>)>,
}

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"dsh web: (https?://[^\s]+)").unwrap())
}

pub fn npm() -> &'static str {
    if cfg!(windows) {
        "npm.cmd"
    } else {
        "npm"
    }
}

pub fn pnpm() -> &'static str {
    if cfg!(windows) {
        "pnpm.cmd"
    } else {
        "pnpm"
    }
}

pub fn node() -> &'static str {
    "node"
}

/// Hides the console window on Windows (CREATE_NO_WINDOW) so spawning
/// npm.cmd / pnpm.cmd / node.exe never flashes a terminal next to the
/// launcher GUI. No-op on other platforms.
pub fn hide_console(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// Whether a version directory is a source checkout (GitHub-only tags
/// installed via clone + build) rather than an npm-installed package tree.
/// The marker is the upstream monorepo's CLI package manifest.
fn is_repo_checkout(version_dir: &std::path::Path) -> bool {
    version_dir
        .join("apps")
        .join("cli")
        .join("package.json")
        .exists()
}

pub fn version_bin(version_dir: &std::path::Path) -> PathBuf {
    if is_repo_checkout(version_dir) {
        return version_dir
            .join("apps")
            .join("cli")
            .join("lib")
            .join("bin.js");
    }
    version_dir
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("lib")
        .join("bin.js")
}

/// Checks that a version's bin.js is present and readable. On Windows, pnpm
/// hard-links store files into the version tree and a transient filesystem
/// state (antivirus scan, indexer, post-install flush) can make `exists()`
/// return false once; retry briefly before declaring the install broken.
pub fn version_bin_ready(version_dir: &std::path::Path) -> bool {
    let bin = version_bin(version_dir);
    for _ in 0..5 {
        if bin.exists() {
            if let Ok(meta) = std::fs::metadata(&bin) {
                if meta.len() > 0 {
                    return true;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    bin.exists()
}

/// Builds the effective environment for an instance: DSH_HOME (from the
/// instance's home), the launcher marker, then the user's overrides
/// (DSH_HOME is reserved and never overridden).
pub fn build_env(cfg: &Config, instance_id: &str) -> Result<Vec<(String, String)>, String> {
    let inst = cfg
        .instances
        .iter()
        .find(|i| i.id == instance_id)
        .ok_or_else(|| "实例不存在".to_string())?;
    let home = cfg
        .homes
        .iter()
        .find(|h| h.id == inst.home_id)
        .ok_or_else(|| "DSH_HOME 不存在".to_string())?;

    let mut env: Vec<(String, String)> = Vec::new();
    env.push((
        "DSH_HOME".to_string(),
        home.path.to_string_lossy().to_string(),
    ));
    env.push(("DSH_LAUNCHER_INSTANCE".to_string(), inst.name.clone()));
    for (k, v) in &inst.env_overrides {
        if k == "DSH_HOME" {
            continue; // reserved
        }
        env.push((k.clone(), v.clone()));
    }
    // Launcher proxy applied to dsh: overrides the instance's own proxy vars.
    if cfg.settings.proxy_enabled && cfg.settings.proxy_apply_dsh {
        crate::proxy::override_env(&mut env, &cfg.settings);
    }
    Ok(env)
}

/// Bundle names that classify a profile (issue #31): the web app bundle
/// drives the webview branch, the TUI bundle requires a real TTY (PTY
/// branch). A profile bundling both counts as web.
pub const WEB_BUNDLE: &str = "@deepseek-ai/dsh-web-app";
pub const TUI_BUNDLE: &str = "@deepseek-harness-tui/dsh-tui";

/// What kind of front end a profile runs, derived from its
/// `dsh.profile.bundles` (see `profile_kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstanceKind {
    /// Web GUI profile: spawned with piped stdio, status via the `dsh web:`
    /// URL, displayed in a webview window.
    Web,
    /// Terminal UI profile: needs a real TTY, runs inside a PTY session and
    /// is displayed in an embedded xterm.js window.
    Tui,
    /// Anything else: managed purely as a process (legacy behavior).
    Other,
}

/// Reads the profile's `dsh.profile.bundles` array, `[]` when unreadable
/// (missing package.json / malformed JSON).
fn profile_bundles(home_path: &std::path::Path, profile: &str) -> Vec<String> {
    let pkg = home_path
        .join("profiles")
        .join(profile)
        .join("package.json");
    let Ok(raw) = std::fs::read_to_string(&pkg) else {
        return Vec::new();
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    doc.pointer("/dsh/profile/bundles")
        .and_then(|b| b.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|b| b.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Classifies a profile by its bundles: web app bundle → `Web` (even when a
/// TUI bundle is also present — the web front end wins, matching the pre-TUI
/// behavior), TUI bundle → `Tui`, otherwise `Other`.
pub fn profile_kind(home_path: &std::path::Path, profile: &str) -> InstanceKind {
    let bundles = profile_bundles(home_path, profile);
    if bundles.iter().any(|b| b == WEB_BUNDLE) {
        InstanceKind::Web
    } else if bundles.iter().any(|b| b == TUI_BUNDLE) {
        InstanceKind::Tui
    } else {
        InstanceKind::Other
    }
}

/// Whether a profile is a DSH web application: its package.json
/// `dsh.profile.bundles` includes `@deepseek-ai/dsh-web-app`. Such profiles
/// understand `--host/--port` and get a webview; they must bind a random
/// free port so several instances don't collide.
fn is_web_profile(home_path: &std::path::Path, profile: &str) -> bool {
    profile_kind(home_path, profile) == InstanceKind::Web
}

/// Whether the installed `dsh-web-app` bundle accepts `--no-open` (added in
/// 0.1.0-rc.8). Feature-detect the flag in the bundle's startup script: the
/// flag is a string literal in `lib/startup.js`, so presence is an exact
/// signal that survives pre-release version-number formats. The bundle lives
/// under pnpm's store; we scan `node_modules/.pnpm/**/@deepseek-ai/dsh-web-app/lib/startup.js`.
fn web_app_supports_no_open(version_dir: &std::path::Path) -> bool {
    // Source checkouts keep the bundle at its workspace path; npm trees hoist
    // one canonical copy into the pnpm public store.
    let startup = if is_repo_checkout(version_dir) {
        version_dir
            .join("packages")
            .join("bundle")
            .join("web-app")
            .join("lib")
            .join("startup.js")
    } else {
        // The pnpm hoisted "public" store keeps one canonical copy with a
        // stable path; the hashed `.pnpm/<name>@<ver>_<hash>` layout would
        // need a scan.
        version_dir
            .join("node_modules")
            .join(".pnpm")
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh-web-app")
            .join("lib")
            .join("startup.js")
    };
    let Ok(raw) = std::fs::read_to_string(&startup) else {
        return false;
    };
    raw.contains("--no-open") || raw.contains("no-open")
}

/// Spawns a DSH CLI process for the instance/profile and starts the watchers
/// that parse the web URL, write logs and emit status events.
pub async fn start_instance_process(
    app: &AppHandle,
    state: &State<'_, AppState>,
    instance_id: &str,
    profile: &str,
) -> Result<(), String> {
    let cfg = state.config.lock().unwrap().clone();
    let inst = cfg
        .instances
        .iter()
        .find(|i| i.id == instance_id)
        .cloned()
        .ok_or_else(|| "实例不存在".to_string())?;
    let version = cfg
        .versions
        .iter()
        .find(|v| v.id == inst.version_id)
        .ok_or_else(|| "实例引用的 DSH 版本未安装".to_string())?;

    let home = cfg
        .homes
        .iter()
        .find(|h| h.id == inst.home_id)
        .cloned()
        .ok_or_else(|| "DSH_HOME 不存在".to_string())?;
    let wsl_distro = home.wsl.clone();
    if wsl_distro.is_some() && version.wsl != wsl_distro {
        return Err("实例的 DSH 版本与其 WSL 运行环境（发行版）不匹配".to_string());
    }

    // Guard: already running/starting.
    {
        let running = state.running.lock().await;
        if running.contains_key(instance_id) {
            return Err("实例已在运行".to_string());
        }
    }

    let env = build_env(&cfg, instance_id)?;

    // WSL inner-PID capture (issue #19): shared with the stdout watcher and
    // stop_instance_process.
    let wsl_proc = wsl_distro
        .clone()
        .map(|d| (d, Arc::new(Mutex::new(Option::<u32>::None))));

    // Web-app profiles (their bundle list includes @deepseek-ai/dsh-web-app)
    // get a random free port; other profiles are managed purely as processes
    // (no URL/webview). We detect the web bundle rather than relying on the
    // profile being literally named "web" so user-named web profiles work.
    let mut cmd = if let Some(distro) = &wsl_distro {
        // WSL instance: run the distro-installed CLI through wsl.exe. The
        // wrapper echoes the inner PID (marker), then execs node — stdout /
        // stderr piping, URL capture and log files are unchanged. File
        // probes go through the \\wsl$\ UNC share.
        let bin_linux = format!(
            "{}/node_modules/@deepseek-ai/dsh/lib/bin.js",
            version.dir.to_string_lossy()
        );
        if !crate::wsl::wsl_test(distro, "-f", &bin_linux).await {
            return Err(format!(
                "版本 {} 在 WSL（{distro}）中安装不完整，请重新安装",
                version.version
            ));
        }
        let root = crate::wsl::WslRoot::resolve(distro).await?;
        let home_linux = home.path.to_string_lossy().to_string();
        let is_web =
            is_web_profile(&crate::wsl::unc_path(distro, &home_linux), profile) || profile == "web";
        let mut cli_args = vec!["--profile".to_string(), profile.to_string()];
        if is_web {
            // Issue #21: a pinned port (1-65535) is used verbatim; otherwise
            // 0 binds a random free port.
            let port = inst
                .port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "0".to_string());
            cli_args.extend([
                "--host".to_string(),
                "127.0.0.1".to_string(),
                "--port".to_string(),
                port,
            ]);
            if web_app_supports_no_open(&crate::wsl::unc_path(
                distro,
                &version.dir.to_string_lossy(),
            )) {
                cli_args.push("--no-open".to_string());
            }
        }
        crate::wsl::wsl_bash(
            distro,
            &crate::wsl::launch_script(&home_linux, &root.node_exe(), &bin_linux, &env, &cli_args),
        )
    } else {
        let bin = version_bin(&version.dir);
        if !version_bin_ready(&version.dir) {
            return Err(format!(
                "版本 {} 安装不完整（缺少 {}），请重新安装",
                version.version,
                bin.display()
            ));
        }
        let mut cmd = Command::new(node());
        hide_console(&mut cmd);
        cmd.arg(&bin).arg("--profile").arg(profile);
        let is_web = is_web_profile(&home.path, profile);
        if is_web {
            // Issue #21: a pinned port (1-65535) is used verbatim; otherwise 0
            // binds a random free port so several instances don't collide.
            let port = inst
                .port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "0".to_string());
            cmd.arg("--host").arg("127.0.0.1").arg("--port").arg(port);
            // `--no-open` was added to dsh-web-app in 0.1.0-rc.8: the launcher
            // embeds the UI in its own webview, so the app must not open the
            // system browser. Feature-detect the flag in the installed bundle's
            // startup.js rather than comparing pre-release versions.
            if web_app_supports_no_open(&version.dir) {
                cmd.arg("--no-open");
            }
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd
    };

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("启动进程失败: {e}"))?;
    crate::log_info!("实例 {instance_id} 进程已拉起（profile: {profile}）");

    // Take the pipes before wrapping the child (the waiter takes ownership of
    // the child itself).
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let log_dir = state.data_dir.join("logs");
    std::fs::create_dir_all(&log_dir).ok();
    let log_path = log_dir.join(format!("{instance_id}.log"));
    let log_file = Arc::new(Mutex::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .unwrap_or_else(|_| {
                // Fallback: discard output if logging fails.
                OpenOptions::new()
                    .write(true)
                    .open(std::path::Path::new("NUL"))
                    .unwrap_or_else(|_| panic!("cannot open log file"))
            }),
    ));

    let kill_switch = Arc::new(Notify::new());

    emit_status(
        app,
        &InstanceStatus {
            id: instance_id.to_string(),
            state: InstanceState::Starting,
            url: None,
            profile: Some(profile.to_string()),
            exit_code: None,
        },
    );

    // Register the running entry (stop_instance reaches it through the map).
    state.running.lock().await.insert(
        instance_id.to_string(),
        RunningInstance {
            kill: kill_switch.clone(),
            profile: profile.to_string(),
            url: None,
            wsl: wsl_proc.clone(),
        },
    );
    crate::tray::rebuild_tray_menu(app).await;

    // Waiter: owns the child, awaits exit or a kill request, then cleans up
    // and notifies. It is the single place that removes the map entry and
    // emits the terminal status, so state transitions never diverge.
    {
        let waiter_app = app.clone();
        let waiter_id = instance_id.to_string();
        let waiter_profile = profile.to_string();
        let waiter_kill = kill_switch.clone();
        let mut child = child;
        tauri::async_runtime::spawn(async move {
            let state = waiter_app.state::<AppState>();
            let (code, stopped) = tokio::select! {
                status = child.wait() => (status.ok().and_then(|s| s.code()), false),
                _ = waiter_kill.notified() => {
                    let _ = child.kill().await;
                    let code = child.wait().await.ok().and_then(|s| s.code());
                    (code, true)
                }
            };
            state.running.lock().await.remove(&waiter_id);
            if stopped {
                crate::log_info!("实例 {waiter_id} 已停止（exit code: {code:?}）");
            } else {
                crate::log_warn!("实例 {waiter_id} 意外退出（exit code: {code:?}）");
            }
            emit_status(
                &waiter_app,
                &InstanceStatus {
                    id: waiter_id.clone(),
                    state: if stopped {
                        InstanceState::Stopped
                    } else {
                        InstanceState::Exited
                    },
                    url: None,
                    profile: Some(waiter_profile),
                    exit_code: code,
                },
            );
            crate::tray::rebuild_tray_menu(&waiter_app).await;
            crate::windows::close_instance_window(&waiter_app, &waiter_id);
        });
    }

    // stdout watcher: parse `dsh web: <url>` and emit "running".
    if let Some(out) = stdout {
        let reader_app = app.clone();
        let reader_id = instance_id.to_string();
        let reader_profile = profile.to_string();
        let reader_log = log_file.clone();
        let reader_wsl = wsl_proc.clone();
        tauri::async_runtime::spawn(async move {
            let state = reader_app.state::<AppState>();
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // WSL launch wrapper marker: capture the inner PID, keep it
                // out of the instance log.
                if let Some((_, pid_slot)) = &reader_wsl {
                    if let Some(p) = crate::wsl::parse_pid_marker(&line) {
                        *pid_slot.lock().await = Some(p);
                        continue;
                    }
                }
                log_line(&reader_log, &line).await;
                if let Some(cap) = url_re().captures(&line) {
                    let url = cap[1].to_string();
                    crate::log_info!("实例 {reader_id} 已就绪：{url}");
                    {
                        let mut running = state.running.lock().await;
                        if let Some(entry) = running.get_mut(&reader_id) {
                            entry.url = Some(url.clone());
                        }
                    }
                    emit_status(
                        &reader_app,
                        &InstanceStatus {
                            id: reader_id.clone(),
                            state: InstanceState::Running,
                            url: Some(url),
                            profile: Some(reader_profile.clone()),
                            exit_code: None,
                        },
                    );
                }
            }
        });
    }

    // stderr watcher: forward to the log (diagnostics).
    if let Some(err) = stderr {
        let reader_log = log_file.clone();
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log_line(&reader_log, &line).await;
            }
        });
    }

    Ok(())
}

/// Stops a running instance: signals the waiter task to kill the child and
/// waits for it to reap the process and remove the registry entry. The stop
/// is idempotent: when the entry is already gone (crashed, stopped from the
/// tray, duplicate click) a terminal status is re-emitted so a stale
/// frontend converges instead of erroring with "实例未在运行".
pub async fn stop_instance_process(
    app: &AppHandle,
    state: &State<'_, AppState>,
    instance_id: &str,
) -> Result<(), String> {
    let entry = {
        let running = state.running.lock().await;
        running
            .get(instance_id)
            .map(|r| (r.kill.clone(), r.wsl.clone()))
    };
    let Some((kill, wsl)) = entry else {
        crate::log_debug!("停止实例 {instance_id}：注册表无记录，补发 stopped 状态");
        emit_status(
            app,
            &InstanceStatus {
                id: instance_id.to_string(),
                state: InstanceState::Stopped,
                url: None,
                profile: None,
                exit_code: None,
            },
        );
        return Ok(());
    };
    crate::log_info!("收到停止实例 {instance_id} 的请求");
    // WSL instance: SIGTERM the inner node process first — killing the local
    // wsl.exe wrapper alone could leave it running.
    if let Some((distro, pid_slot)) = &wsl {
        if let Some(pid) = *pid_slot.lock().await {
            let _ = crate::wsl::wsl_output(distro, &["kill".into(), pid.to_string()]).await;
            // Give node a moment to shut down cleanly before the wrapper goes.
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    }
    kill.notify_one();
    // Wait for the waiter task to finish cleanup so callers (e.g. the restart
    // flow) observe a clean registry on return.
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if !state.running.lock().await.contains_key(instance_id) {
            return Ok(());
        }
    }
    crate::log_error!("停止实例 {instance_id} 超时：进程未响应终止信号");
    Err("停止实例超时（进程未响应终止信号）".to_string())
}

/// Kills every running instance (called on launcher exit). Best-effort: the
/// waiter tasks reap the children asynchronously as the runtime shuts down.
pub fn kill_all(state: &AppState) {
    let running = state.running.blocking_lock();
    for entry in running.values() {
        entry.kill.notify_one();
    }
}

pub async fn list_statuses(state: &State<'_, AppState>) -> Vec<InstanceStatus> {
    let running = state.running.lock().await;
    running
        .iter()
        .map(|(id, entry)| InstanceStatus {
            id: id.clone(),
            state: InstanceState::Running,
            url: entry.url.clone(),
            profile: Some(entry.profile.clone()),
            exit_code: None,
        })
        .collect()
}

async fn log_line(log: &Arc<Mutex<std::fs::File>>, line: &str) {
    let mut f = log.lock().await;
    let _ = writeln!(f, "{line}");
    let _ = f.flush();
}

pub fn emit_status(app: &AppHandle, status: &InstanceStatus) {
    let _ = app.emit(STATUS_EVENT, status);
}

/// Resolves the profile a TUI session should run: the launcher-passed
/// `last_profile` when set, else the instance default. TUI windows are only
/// opened right after `start_instance`, so `last_profile` is authoritative;
/// the default is a fallback for reattaching after a launcher restart.
pub fn tui_active_profile(cfg: &Config, instance_id: &str) -> Option<String> {
    let inst = cfg.instances.iter().find(|i| i.id == instance_id)?;
    inst.last_profile
        .clone()
        .or_else(|| inst.default_profile.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_kind_classifies_web_tui_other() {
        let dir = std::env::temp_dir().join(format!("dsh-proc-test-{}", uuid::Uuid::new_v4()));
        let mk = |name: &str, bundles: &str| {
            let p = dir.join("profiles").join(name);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(
                p.join("package.json"),
                format!(
                    r#"{{"name":"dsh-profile-{name}","private":true,"dependencies":{{}},"dsh":{{"profile":{{"bundles":{bundles}}}}}}}"#
                ),
            )
            .unwrap();
        };
        // Missing package.json -> Other.
        std::fs::create_dir_all(dir.join("profiles").join("empty")).unwrap();
        assert_eq!(profile_kind(&dir, "empty"), InstanceKind::Other);
        // Real-world TUI bundle name (issue #31).
        mk(
            "tui",
            r#"["@deepseek-ai/dsh-base","@deepseek-harness-tui/dsh-tui"]"#,
        );
        assert_eq!(profile_kind(&dir, "tui"), InstanceKind::Tui);
        // Web wins when both bundles are present.
        mk(
            "both",
            r#"["@deepseek-ai/dsh-web-app","@deepseek-harness-tui/dsh-tui"]"#,
        );
        assert_eq!(profile_kind(&dir, "both"), InstanceKind::Web);
        mk("web", r#"["@deepseek-ai/dsh-web-app"]"#);
        assert_eq!(profile_kind(&dir, "web"), InstanceKind::Web);
        mk("bot", r#"["@deepseek-ai/dsh-base"]"#);
        assert_eq!(profile_kind(&dir, "bot"), InstanceKind::Other);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn is_web_profile_detects_web_app_bundle() {
        let dir = std::env::temp_dir().join(format!("dsh-proc-test-{}", uuid::Uuid::new_v4()));
        let profile_dir = dir.join("profiles").join("test");
        std::fs::create_dir_all(&profile_dir).unwrap();
        // No package.json -> not a web profile.
        assert!(!is_web_profile(&dir, "test"));
        // Web app bundle -> web profile.
        std::fs::write(
            profile_dir.join("package.json"),
            r#"{"name":"dsh-profile-test","private":true,"dependencies":{},"dsh":{"profile":{"bundles":["@deepseek-ai/dsh-base","@deepseek-ai/dsh-web-app"]}}}"#,
        )
        .unwrap();
        assert!(is_web_profile(&dir, "test"));
        // Non-web profile (no dsh-web-app bundle) -> false.
        let other = dir.join("profiles").join("bot");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("package.json"),
            r#"{"name":"dsh-profile-bot","private":true,"dependencies":{},"dsh":{"profile":{"bundles":["@deepseek-ai/dsh-base"]}}}"#,
        )
        .unwrap();
        assert!(!is_web_profile(&dir, "bot"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn web_app_supports_no_open_feature_detects_flag() {
        let dir = std::env::temp_dir().join(format!("dsh-proc-test-{}", uuid::Uuid::new_v4()));
        let startup_dir = dir
            .join("node_modules")
            .join(".pnpm")
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh-web-app")
            .join("lib");
        // No startup.js yet -> false.
        assert!(!web_app_supports_no_open(&dir));
        std::fs::create_dir_all(&startup_dir).unwrap();
        // Old bundle without the flag (<= 0.1.0-rc.7) -> false.
        std::fs::write(
            startup_dir.join("startup.js"),
            "const p = new Command().option('--host <host>').option('--port <port>')",
        )
        .unwrap();
        assert!(!web_app_supports_no_open(&dir));
        // New bundle with the flag (>= 0.1.0-rc.8) -> true.
        std::fs::write(
            startup_dir.join("startup.js"),
            "const p = new Command().option('--no-open', 'do not open the Web UI in the default browser')",
        )
        .unwrap();
        assert!(web_app_supports_no_open(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn version_bin_detects_source_checkout_layout() {
        let dir = std::env::temp_dir().join(format!("dsh-proc-test-{}", uuid::Uuid::new_v4()));
        // npm layout: node_modules/@deepseek-ai/dsh/lib/bin.js.
        assert!(version_bin(&dir).ends_with(
            std::path::Path::new("node_modules")
                .join("@deepseek-ai")
                .join("dsh")
                .join("lib")
                .join("bin.js")
        ));
        assert!(!version_bin_ready(&dir));
        // Source checkout layout (GitHub-only tags): apps/cli/lib/bin.js.
        let cli = dir.join("apps").join("cli");
        std::fs::create_dir_all(cli.join("lib")).unwrap();
        std::fs::write(cli.join("package.json"), r#"{"name":"@deepseek-ai/dsh"}"#).unwrap();
        assert_eq!(version_bin(&dir), cli.join("lib").join("bin.js"));
        assert!(!version_bin_ready(&dir));
        std::fs::write(cli.join("lib").join("bin.js"), "// bin").unwrap();
        assert!(version_bin_ready(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn web_app_no_open_detects_flag_in_source_checkout() {
        let dir = std::env::temp_dir().join(format!("dsh-proc-test-{}", uuid::Uuid::new_v4()));
        let cli = dir.join("apps").join("cli");
        std::fs::create_dir_all(&cli).unwrap();
        std::fs::write(cli.join("package.json"), r#"{"name":"@deepseek-ai/dsh"}"#).unwrap();
        let lib = dir
            .join("packages")
            .join("bundle")
            .join("web-app")
            .join("lib");
        assert!(!web_app_supports_no_open(&dir));
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(
            lib.join("startup.js"),
            "const p = new Command().option('--no-open', 'x')",
        )
        .unwrap();
        assert!(web_app_supports_no_open(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }
}
