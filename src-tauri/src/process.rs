use crate::config::{Config, InstanceState, InstanceStatus};
use crate::web_launch::{WebLaunch, WebLinks};
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
    pub links: Option<WebLinks>,
    web: Option<WebLaunch>,
    launch_failed: bool,
    stopping: bool,
}

impl RunningInstance {
    pub fn desktop_url(&self) -> Option<String> {
        self.links.as_ref().map(|links| links.desktop.to_string())
    }

    fn status(&self, id: &str) -> InstanceStatus {
        InstanceStatus {
            id: id.to_string(),
            state: if self.web.is_some() && self.links.is_none() {
                InstanceState::Starting
            } else {
                InstanceState::Running
            },
            url: self.links.as_ref().map(WebLinks::display),
            web_mode: self.web.as_ref().map(|web| web.mode),
            allowed_networks: self
                .web
                .as_ref()
                .map(|web| web.networks.clone())
                .unwrap_or_default(),
            profile: Some(self.profile.clone()),
            exit_code: None,
        }
    }
}

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^dsh web: (https?://[^\s]+)").unwrap())
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

/// The "版本安装不完整" error, shared by every caller that validates an
/// installed version (issue #49): the terminal (G1) and the TUI session both
/// need it, and the message must name the path the *probe* used — the Linux
/// path inside the distro for WSL instances, the UNC path otherwise. Naming
/// the UNC path for a WSL instance points at a location the user cannot see
/// from inside the distro.
pub fn version_missing_message(version: &str, bin: &std::path::Path) -> String {
    format!(
        "版本 {version} 安装不完整（缺少 {}），请重新安装",
        bin.display()
    )
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

    let (pid_sender, pid_receiver) = tokio::sync::oneshot::channel::<u32>();
    let wsl_root = if let Some(distro) = &wsl_distro {
        Some(crate::wsl::WslRoot::resolve(distro).await?)
    } else {
        None
    };
    let profile_dir = crate::wsl::home_fs_path(&home);
    let is_web = is_web_profile(&profile_dir, profile)
        || (profile == "web" && !profile_dir.join("profiles/web/package.json").exists());
    let mut cli_args = vec!["--profile".to_string(), profile.to_string()];
    let web = if is_web {
        let startup = crate::web_launch::startup_script(
            &crate::wsl::version_fs_path(version),
            &profile_dir.join("profiles").join(profile),
        );
        let wsl = if let (Some(distro), Some(root)) = (&wsl_distro, &wsl_root) {
            Some(
                crate::web_launch::WslWeb::resolve(
                    distro.clone(),
                    root.node_exe(),
                    &inst.web_access,
                    inst.port,
                )
                .await?,
            )
        } else {
            None
        };
        let plan = WebLaunch::prepare(inst.web_access.clone(), inst.port, &startup, wsl)
            .await
            .map_err(|e| format!("实例 {}（DSH {}）: {e}", inst.name, version.version))?;
        plan.check_native_port()?;
        cli_args.extend_from_slice(&plan.args);
        Some(plan)
    } else {
        None
    };
    let mut cmd = if let (Some(distro), Some(root)) = (&wsl_distro, &wsl_root) {
        let bin_linux = format!(
            "{}/node_modules/@deepseek-ai/dsh/lib/bin.js",
            version.dir.display()
        );
        if !crate::wsl::wsl_test(distro, "-f", &bin_linux).await {
            return Err(format!(
                "版本 {} 在 WSL（{distro}）中安装不完整，请重新安装",
                version.version
            ));
        }
        crate::wsl::wsl_bash(
            distro,
            &crate::wsl::launch_script(
                &home.path.to_string_lossy(),
                &root.node_exe(),
                &bin_linux,
                &env,
                &cli_args,
            ),
        )
    } else {
        let bin = version_bin(&version.dir);
        if !version_bin_ready(&version.dir) {
            return Err(version_missing_message(&version.version, &bin));
        }
        let mut cmd = Command::new(node());
        hide_console(&mut cmd);
        let mut env = env;
        crate::runtime::apply_instance_pnpm_env(&state.data_dir, &mut env)?;
        cmd.arg(bin).args(cli_args).envs(env);
        cmd
    };

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let log_dir = state.data_dir.join("logs");
    std::fs::create_dir_all(&log_dir).map_err(|e| format!("创建实例日志目录失败: {e}"))?;
    let log_file = Arc::new(Mutex::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_dir.join(format!("{instance_id}.log")))
            .map_err(|e| format!("打开实例日志失败: {e}"))?,
    ));
    let kill_switch = Arc::new(Notify::new());
    let mut running = state.running.lock().await;
    if running.contains_key(instance_id) {
        return Err("实例已在运行".into());
    }
    if state.launch_cancels.lock().unwrap().contains(instance_id) {
        return Err("启动已取消".into());
    }
    let mut child = cmd
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("启动进程失败: {e}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    crate::log_info!("实例 {instance_id} 进程已拉起（profile: {profile}）");
    let needs_announcement = web
        .as_ref()
        .is_some_and(|plan| plan.mode != crate::web_access::WebMode::Local);
    let entry = RunningInstance {
        kill: kill_switch.clone(),
        profile: profile.to_string(),
        links: None,
        web,
        launch_failed: false,
        stopping: false,
    };
    let mut status = entry.status(instance_id);
    status.state = InstanceState::Starting;
    running.insert(instance_id.to_string(), entry);
    emit_status(app, &status);
    drop(running);
    crate::tray::sync_tray_icons(app).await;

    // Waiter: owns the child, awaits exit or a kill request, then cleans up
    // and notifies. It is the single place that removes the map entry and
    // emits the terminal status, so state transitions never diverge.
    {
        let waiter_app = app.clone();
        let waiter_id = instance_id.to_string();
        let waiter_profile = profile.to_string();
        let waiter_kill = kill_switch.clone();
        let waiter_wsl = wsl_distro.clone();
        let mut child = child;
        tauri::async_runtime::spawn(async move {
            let state = waiter_app.state::<AppState>();
            let (code, stopped) = tokio::select! {
                status = child.wait() => (status.ok().and_then(|s| s.code()), false),
                _ = waiter_kill.notified() => {
                    if let Some(distro) = &waiter_wsl {
                        if let Ok(Ok(pid)) = tokio::time::timeout(std::time::Duration::from_secs(3), pid_receiver).await {
                            if let Err(e) = crate::wsl::wsl_output(distro, &["kill".into(), pid.to_string()]).await {
                                crate::log_warn!("终止 WSL 实例 {waiter_id} 失败: {e}");
                            }
                        }
                    }
                    let _ = child.kill().await;
                    let code = child.wait().await.ok().and_then(|s| s.code());
                    (code, true)
                }
            };
            let mut running = state.running.lock().await;
            let Some(mut entry) = running.remove(&waiter_id) else {
                return;
            };
            if let Some(web) = &mut entry.web {
                web.close().await;
            }
            let stopped = stopped && !entry.launch_failed;
            state.window_urls.lock().unwrap().remove(&waiter_id);
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
                    web_mode: None,
                    allowed_networks: Vec::new(),
                    profile: Some(waiter_profile),
                    exit_code: code,
                },
            );
            crate::windows::close_instance_window(&waiter_app, &waiter_id);
            // A crash during launch must not strand the early-loading window
            // (a normal launch already closed it when the DSH window opened).
            crate::early_loading::close_early_loading(&waiter_app, &waiter_id);
            drop(running);
            crate::tray::sync_tray_icons(&waiter_app).await;
        });
    }

    // stdout watcher: parse `dsh web: <url>` and emit "running".
    if let Some(out) = stdout {
        let reader_app = app.clone();
        let reader_id = instance_id.to_string();
        let reader_kill = kill_switch.clone();
        let reader_log = log_file.clone();
        let mut reader_pid = wsl_distro.as_ref().map(|_| pid_sender);
        tauri::async_runtime::spawn(async move {
            let state = reader_app.state::<AppState>();
            let mut reader = BufReader::new(out);
            let mut buf = Vec::new();
            let mut waiting = needs_announcement;
            let deadline = tokio::time::sleep(std::time::Duration::from_secs(120));
            tokio::pin!(deadline);
            loop {
                let result = tokio::select! {
                    _ = &mut deadline, if waiting => {
                        fail_web_start(&state, &reader_id, &reader_kill, &reader_log, "等待 DSH 公布启动地址超时，请检查 Web Profile 的 printUrl 设置").await;
                        break;
                    }
                    line = read_line_lossy(&mut reader, &mut buf) => line,
                };
                let line = match result {
                    Ok(Some(line)) => line,
                    Ok(None) => {
                        if waiting {
                            fail_web_start(
                                &state,
                                &reader_id,
                                &reader_kill,
                                &reader_log,
                                "DSH 在公布启动地址前关闭了标准输出",
                            )
                            .await;
                        }
                        break;
                    }
                    Err(e) => {
                        crate::log_warn!("实例 {reader_id} stdout 读取终止: {e}");
                        if waiting {
                            fail_web_start(
                                &state,
                                &reader_id,
                                &reader_kill,
                                &reader_log,
                                "读取 DSH 启动地址失败",
                            )
                            .await;
                        }
                        break;
                    }
                };
                // WSL launch wrapper marker: capture the inner PID, keep it
                // out of the instance log.
                if let Some(pid) = crate::wsl::parse_pid_marker(&line).filter(|p| *p > 1) {
                    if let Some(sender) = reader_pid.take() {
                        let _ = sender.send(pid);
                    }
                    continue;
                }
                log_line(&reader_log, &line).await;
                if let Some(cap) = url_re().captures(&line) {
                    let mut running = state.running.lock().await;
                    let Some(entry) = running.get_mut(&reader_id) else {
                        break;
                    };
                    if !Arc::ptr_eq(&entry.kill, &reader_kill) {
                        break;
                    }
                    if entry.links.is_some() || entry.launch_failed || entry.stopping {
                        continue;
                    }
                    let links = match &mut entry.web {
                        Some(web) => web.ready(&cap[1]),
                        None => WebLinks::local_output(&cap[1]),
                    };
                    match links {
                        Ok(links) => {
                            crate::log_info!("实例 {reader_id} 已就绪：{}", links.display());
                            waiting = false;
                            entry.links = Some(links);
                            emit_status(&reader_app, &entry.status(&reader_id));
                        }
                        Err(error) => {
                            entry.launch_failed = true;
                            entry.kill.notify_one();
                            drop(running);
                            crate::log_error!("实例 {reader_id} 启动失败: {error}");
                            log_line(&reader_log, &error).await;
                        }
                    }
                }
            }
        });
    }

    // stderr watcher: forward to the log (diagnostics).
    if let Some(err) = stderr {
        let reader_log = log_file.clone();
        let reader_id = instance_id.to_string();
        tauri::async_runtime::spawn(async move {
            let mut reader = BufReader::new(err);
            let mut buf = Vec::new();
            loop {
                match read_line_lossy(&mut reader, &mut buf).await {
                    Ok(Some(line)) => log_line(&reader_log, &line).await,
                    Ok(None) => break,
                    Err(e) => {
                        crate::log_warn!("实例 {reader_id} stderr 读取终止: {e}");
                        break;
                    }
                }
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
        let mut running = state.running.lock().await;
        if let Some(entry) = running.get_mut(instance_id) {
            entry.stopping = true;
            entry.links = None;
            state.window_urls.lock().unwrap().remove(instance_id);
            entry.kill.notify_one();
            if let Some(web) = &mut entry.web {
                web.close().await;
            }
            true
        } else {
            false
        }
    };
    if !entry {
        crate::log_debug!("停止实例 {instance_id}：注册表无记录，补发 stopped 状态");
        emit_status(
            app,
            &InstanceStatus {
                id: instance_id.to_string(),
                state: InstanceState::Stopped,
                url: None,
                web_mode: None,
                allowed_networks: Vec::new(),
                profile: None,
                exit_code: None,
            },
        );
        return Ok(());
    }
    crate::log_info!("收到停止实例 {instance_id} 的请求");
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
    let mut running = state.running.blocking_lock();
    for entry in running.values_mut() {
        entry.stopping = true;
        entry.kill.notify_one();
    }
}

pub async fn list_statuses(state: &State<'_, AppState>) -> Vec<InstanceStatus> {
    let running = state.running.lock().await;
    running.iter().map(|(id, entry)| entry.status(id)).collect()
}

async fn fail_web_start(
    state: &AppState,
    id: &str,
    generation: &Arc<Notify>,
    log: &Arc<Mutex<std::fs::File>>,
    message: &str,
) {
    let mut running = state.running.lock().await;
    let Some(entry) = running.get_mut(id) else {
        return;
    };
    if !Arc::ptr_eq(&entry.kill, generation)
        || entry.stopping
        || entry.links.is_some()
        || entry.launch_failed
    {
        return;
    }
    entry.launch_failed = true;
    entry.kill.notify_one();
    drop(running);
    crate::log_error!("实例 {id}: {message}");
    log_line(log, message).await;
}

async fn log_line(log: &Arc<Mutex<std::fs::File>>, line: &str) {
    let mut f = log.lock().await;
    let _ = writeln!(f, "{}", crate::web_launch::redact_tokens(line));
    let _ = f.flush();
}

/// Reads one line from a child-process pipe, tolerating non-UTF-8 bytes
/// (issue #42). `BufRead::lines()` aborts with `InvalidData` on the first
/// non-UTF-8 byte and, in a `while let Ok(Some(..))` loop, silently drops
/// the read end — the next write by the child then hits EPIPE and the
/// instance dies with no trace. Reading raw bytes with `read_until` and
/// lossy-decoding keeps the reader (and the pipe) alive and preserves the
/// line's content (replacement chars for invalid bytes).
///
/// Returns `Ok(None)` on EOF. A genuine IO error is returned to the caller,
/// which must log it — a dying reader that leaves no trace is what made
/// issue #42 so expensive to diagnose.
pub(crate) async fn read_line_lossy<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> std::io::Result<Option<String>> {
    buf.clear();
    let n = reader.read_until(b'\n', buf).await?;
    if n == 0 {
        return Ok(None);
    }
    let line = String::from_utf8_lossy(buf);
    Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
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

    /// Minimal Config with one home + one instance carrying env overrides.
    fn cfg_with_env(overrides: &[(&str, &str)]) -> Config {
        let mut cfg = Config::default();
        cfg.homes.push(crate::config::DshHome {
            id: "h1".to_string(),
            name: "h1".to_string(),
            path: PathBuf::from("C:/homes/h1"),
            wsl: None,
            links: Default::default(),
        });
        cfg.instances.push(crate::config::DshInstance {
            id: "i1".to_string(),
            name: "demo".to_string(),
            version_id: "v1".to_string(),
            home_id: "h1".to_string(),
            env_overrides: overrides
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            default_profile: None,
            last_profile: None,
            icon: None,
            port: None,
            web_access: Default::default(),
            tui_cwd: None,
        });
        cfg
    }

    #[test]
    fn build_env_carries_overrides_and_reserves_dsh_home() {
        let cfg = cfg_with_env(&[
            ("MY_FLAG", "1"),
            ("DSH_HOME", "C:/evil"),
            ("DSH_LAUNCHER_INSTANCE", "spoofed"),
        ]);
        let env = build_env(&cfg, "i1").unwrap();
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        assert_eq!(get("MY_FLAG").as_deref(), Some("1"));
        // DSH_HOME always comes from the instance's home, never the payload.
        assert_eq!(get("DSH_HOME").as_deref(), Some("C:/homes/h1"));
        assert_eq!(get("DSH_LAUNCHER_INSTANCE").as_deref(), Some("demo"));
        // Exactly one DSH_HOME entry.
        assert_eq!(env.iter().filter(|(k, _)| k == "DSH_HOME").count(), 1);
    }

    #[test]
    fn build_env_errors_for_unknown_instance() {
        let cfg = cfg_with_env(&[]);
        assert!(build_env(&cfg, "nope").is_err());
    }

    #[tokio::test]
    async fn read_line_lossy_survives_invalid_utf8() {
        // Issue #42: 'a' + GBK「…」+ 'b' is not valid UTF-8; Lines::next_line
        // would Err out and silently end the loop here, killing the pipe.
        let bytes: &[u8] = b"ascii line OK\na\xa1\xadb\nafter the bad line\n";
        let mut reader = BufReader::new(bytes);
        let mut buf = Vec::new();
        let first = read_line_lossy(&mut reader, &mut buf).await.unwrap();
        assert_eq!(first.as_deref(), Some("ascii line OK"));
        let bad = read_line_lossy(&mut reader, &mut buf).await.unwrap();
        assert_eq!(bad.as_deref(), Some("a\u{FFFD}\u{FFFD}b"));
        // The reader must still be alive for subsequent lines.
        let third = read_line_lossy(&mut reader, &mut buf).await.unwrap();
        assert_eq!(third.as_deref(), Some("after the bad line"));
        let eof = read_line_lossy(&mut reader, &mut buf).await.unwrap();
        assert_eq!(eof, None);
    }

    #[tokio::test]
    async fn read_line_lossy_handles_crlf_and_unterminated_tail() {
        let bytes: &[u8] = b"one\r\ntwo\r\nthree";
        let mut reader = BufReader::new(bytes);
        let mut buf = Vec::new();
        assert_eq!(
            read_line_lossy(&mut reader, &mut buf)
                .await
                .unwrap()
                .as_deref(),
            Some("one")
        );
        assert_eq!(
            read_line_lossy(&mut reader, &mut buf)
                .await
                .unwrap()
                .as_deref(),
            Some("two")
        );
        assert_eq!(
            read_line_lossy(&mut reader, &mut buf)
                .await
                .unwrap()
                .as_deref(),
            Some("three")
        );
        assert_eq!(read_line_lossy(&mut reader, &mut buf).await.unwrap(), None);
    }

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
        assert!(!crate::web_launch::startup_script(&dir, &dir).contains("no-open"));
        std::fs::create_dir_all(&startup_dir).unwrap();
        // Old bundle without the flag (<= 0.1.0-rc.7) -> false.
        std::fs::write(
            startup_dir.join("startup.js"),
            "const p = new Command().option('--host <host>').option('--port <port>')",
        )
        .unwrap();
        assert!(!crate::web_launch::startup_script(&dir, &dir).contains("no-open"));
        // New bundle with the flag (>= 0.1.0-rc.8) -> true.
        std::fs::write(
            startup_dir.join("startup.js"),
            "const p = new Command().option('--no-open', 'do not open the Web UI in the default browser')",
        )
        .unwrap();
        assert!(crate::web_launch::startup_script(&dir, &dir).contains("no-open"));
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
        assert!(!crate::web_launch::startup_script(&dir, &dir).contains("no-open"));
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(
            lib.join("startup.js"),
            "const p = new Command().option('--no-open', 'x')",
        )
        .unwrap();
        assert!(crate::web_launch::startup_script(&dir, &dir).contains("no-open"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
