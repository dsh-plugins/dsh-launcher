//! TUI profile sessions: profiles whose `dsh.profile.bundles` contain
//! `@deepseek-harness-tui/dsh-tui` need a real TTY — the CLI's interactive
//! terminal guard refuses to start the TUI when stdout/stderr are pipes
//! (issue #31), so those profiles cannot use the piped `node` process used
//! by web profiles. Instead they run inside a PTY (ConPTY on Windows) and
//! the raw output is forwarded to the frontend (`tui://data`) for xterm.js
//! rendering, mirrored into the instance log.
//!
//! Lifecycle differs from web instances:
//! - `start_instance` (web path) validates and opens the terminal window;
//!   the window's frontend then calls `start_tui_session` which spawns the
//!   PTY and registers the instance as Starting.
//! - The first PTY output flips the instance to Running (a TUI has no URL;
//!   "alive and producing output" is the running signal).
//! - Exiting (user `exit`, crash) or a launcher-driven stop tears the
//!   session down and emits Stopped/Exited like the web path does.
//! - Closing the terminal window only detaches the view; the instance keeps
//!   running and reopening the window reattaches to the live output stream.

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;
use std::io::{Read, Write};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{Mutex, Notify};

use crate::config::{InstanceState, InstanceStatus};
use crate::AppState;

pub const DATA_EVENT: &str = "tui://data";

/// Default PTY size until the frontend sends its first resize.
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 30;

/// Input for starting / resizing a TUI session.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiSessionInput {
    pub instance_id: String,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
}

/// Input for writing to a live TUI session.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiWriteInput {
    pub instance_id: String,
    /// base64 of raw bytes to feed the PTY master.
    pub data: String,
}

/// A live PTY-backed TUI session for one instance.
pub struct TuiSession {
    /// PTY master: used for resize.
    pub master: Box<dyn portable_pty::MasterPty + Send>,
    /// Writable handle to the PTY master (taken once at spawn), shared with
    /// the reader task.
    pub writer: Arc<std::sync::Mutex<Box<dyn std::io::Write + Send>>>,
    /// Killer handle split from the child (portable_pty `clone_killer`): the
    /// stop path and launcher shutdown kill through it while the waiter's
    /// blocking task owns the child for `wait()`.
    pub killer: Arc<Mutex<Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>>>,
    /// Kill signal: the stop path notifies it, the waiter selects it against
    /// the child's exit (mirrors `RunningInstance.kill`).
    pub kill: Arc<Notify>,
    /// Resolves with the exit code once the child exits; taken once by the
    /// waiter task. The blocking `wait()` runs on a dedicated thread so the
    /// async waiter can select it against the kill signal.
    pub child_waiter:
        Arc<Mutex<Option<tokio::sync::oneshot::Receiver<Option<i32>>>>>,
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| format!("输入编码无效: {e}"))
}

/// Emits an instance status using the shared event so Home / tray / status
/// surfaces treat TUI instances like any other instance.
fn emit_instance_status(app: &AppHandle, status: &InstanceStatus) {
    crate::process::emit_status(app, status);
}

/// Spawns the instance's DSH CLI inside a fresh PTY and wires the readers
/// that stream output to the frontend and flip the instance status.
#[tauri::command(rename_all = "snake_case")]
pub async fn start_tui_session(
    app: AppHandle,
    state: State<'_, AppState>,
    input: TuiSessionInput,
) -> Result<(), String> {
    let instance_id = input.instance_id.as_str();
    if state.tui_sessions.lock().await.contains_key(instance_id) {
        return Err("TUI 会话已存在".to_string());
    }
    {
        let running = state.running.lock().await;
        if running.contains_key(instance_id) {
            return Err("实例已在运行".to_string());
        }
    }

    let (home_path, version_dir) = crate::plugins::resolve_instance(&state, instance_id)?;
    let cfg = state.config.lock().unwrap().clone();
    let inst = cfg
        .instances
        .iter()
        .find(|i| i.id == instance_id)
        .cloned()
        .ok_or_else(|| "实例不存在".to_string())?;
    let Some(profile) = crate::process::tui_active_profile(&cfg, instance_id) else {
        return Err("实例没有可用的 profile".to_string());
    };
    // The window only opens for TUI profiles, but re-validate here so a
    // stale window cannot spawn a non-TUI profile through this path.
    if crate::process::profile_kind(&home_path, &profile) != crate::process::InstanceKind::Tui {
        return Err("该实例的 profile 不是 TUI 类型".to_string());
    }

    if !crate::process::version_bin_ready(&version_dir) {
        let bin = crate::process::version_bin(&version_dir);
        return Err(format!(
            "版本 {} 安装不完整（缺少 {}），请重新安装",
            cfg.versions
                .iter()
                .find(|v| v.dir == version_dir)
                .map(|v| v.version.clone())
                .unwrap_or_default(),
            bin.display()
        ));
    }

    let mut env = crate::process::build_env(&cfg, instance_id)?;
    env.push(("DSH_LAUNCHER_INSTANCE".to_string(), inst.name.clone()));

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: input.rows.unwrap_or(DEFAULT_ROWS),
            cols: input.cols.unwrap_or(DEFAULT_COLS),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("创建 PTY 失败: {e}"))?;

    let mut cmd = CommandBuilder::new(crate::process::node());
    cmd.arg(crate::process::version_bin(&version_dir));
    cmd.arg("--profile");
    cmd.arg(&profile);
    cmd.cwd(home_path.as_os_str());
    for (k, v) in &env {
        cmd.env(k, v);
    }

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("启动 TUI 进程失败: {e}"))?;
    // Split a killer handle before the waiter owns the child: the stop path
    // kills through it without touching the child being waited on.
    let killer = child.clone_killer();
    drop(pair.slave); // Close our copy of the slave so EOF propagates.

    // Blocking wait() on a dedicated thread; the async waiter selects the
    // oneshot against the kill signal.
    let (wait_tx, child_waiter) = tokio::sync::oneshot::channel::<Option<i32>>();
    std::thread::spawn(move || {
        let code = child.wait().ok().map(|st| st.exit_code() as i32);
        let _ = wait_tx.send(code);
    });

    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("获取 PTY 写入通道失败: {e}"))?;

    let session = TuiSession {
        master: pair.master,
        writer: Arc::new(std::sync::Mutex::new(writer)),
        killer: Arc::new(Mutex::new(Some(killer))),
        kill: Arc::new(Notify::new()),
        child_waiter: Arc::new(Mutex::new(Some(child_waiter))),
    };

    // Register before the reader spawns so early input finds the session.
    state
        .tui_sessions
        .lock()
        .await
        .insert(instance_id.to_string(), session);

    emit_instance_status(
        &app,
        &InstanceStatus {
            id: instance_id.to_string(),
            state: InstanceState::Starting,
            url: None,
            profile: Some(profile.clone()),
            exit_code: None,
        },
    );
    crate::log_info!("实例 {instance_id} TUI 会话已启动（profile: {profile}）");

    spawn_reader(&app, &state, instance_id).await?;
    spawn_waiter(&app, instance_id, &profile).await;
    Ok(())
}

/// Reader task: forwards PTY output as `tui://data` events and mirrors it
/// into the instance log. The first chunk flips the instance to Running
/// (a TUI has no readiness URL). On EOF the session is torn down: a
/// user-initiated stop already emitted Stopped (the `stopped` flag makes
/// this path stay quiet); a natural exit emits Exited with the code.
async fn spawn_reader(
    app: &AppHandle,
    state: &State<'_, AppState>,
    instance_id: &str,
) -> Result<(), String> {
    let (reader, writer) = {
        let map = state.tui_sessions.lock().await;
        let session = map
            .get(instance_id)
            .ok_or_else(|| "TUI 会话不存在".to_string())?;
        (
            session
                .master
                .try_clone_reader()
                .map_err(|e| format!("读取 PTY 失败: {e}"))?,
            session.writer.clone(),
        )
    };

    let app = app.clone();
    let id = instance_id.to_string();
    let data_dir = state.data_dir.clone();

    tauri::async_runtime::spawn(async move {
        let mut reader = reader;
        let mut buf = [0u8; 8192];
        let mut running_emitted = false;
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    if !running_emitted {
                        running_emitted = true;
                        crate::log_info!("实例 {id} TUI 已就绪");
                        emit_instance_status(
                            &app,
                            &InstanceStatus {
                                id: id.clone(),
                                state: InstanceState::Running,
                                url: None,
                                profile: None,
                                exit_code: None,
                            },
                        );
                    }
                    let _ = app.emit(
                        DATA_EVENT,
                        TuiData {
                            instance_id: id.clone(),
                            data: base64_encode(chunk),
                        },
                    );
                    append_log(&data_dir, &id, chunk);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        // EOF (or a dead PTY): flush and detach. Terminal status emission
        // belongs to the waiter task, which observes the child exit.
        let _ = writer.lock().map(|mut w| w.flush());
    });
    Ok(())
}

/// Waiter task: owns the exit observation for the TUI session. The child's
/// blocking `wait()` runs on a dedicated thread; its completion races the
/// kill signal in a `select!`. Whichever wins tears the session down and
/// emits the terminal status — the single place TUI instances reach
/// Stopped/Exited, mirroring the web path's waiter.
///
/// This exists because a ConPTY whose child dies abruptly does not
/// reliably deliver EOF to the master reader; waiting on the child handle
/// directly is the dependable signal.
async fn spawn_waiter(app: &AppHandle, instance_id: &str, profile: &str) {
    let app = app.clone();
    let id = instance_id.to_string();
    let profile = profile.to_string();

    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let (killer_slot, kill, waiter_slot) = {
            let map = state.tui_sessions.lock().await;
            let Some(session) = map.get(&id) else {
                return;
            };
            (
                session.killer.clone(),
                session.kill.clone(),
                session.child_waiter.clone(),
            )
        };
        // Only the waiter consumes the child handle for waiting; the stop
        // path kills through the killer handle.
        let mut killer = killer_slot.lock().await.take();
        let child_waiter = waiter_slot.lock().await.take();

        let (exit_code, stopped) = tokio::select! {
            code = async {
                match child_waiter {
                    Some(rx) => rx.await.unwrap_or(None),
                    None => None,
                }
            } => (code, false),
            _ = kill.notified() => {
                if let Some(k) = killer.as_mut() {
                    let _ = k.kill();
                }
                (None, true)
            }
        };

        state.tui_sessions.lock().await.remove(&id);

        if stopped {
            crate::log_info!("实例 {id} TUI 已停止（exit code: {exit_code:?}）");
            let last_profile = state
                .config
                .lock()
                .unwrap()
                .instances
                .iter()
                .find(|i| i.id == id)
                .and_then(|i| i.last_profile.clone());
            emit_instance_status(
                &app,
                &InstanceStatus {
                    id,
                    state: InstanceState::Stopped,
                    url: None,
                    profile: last_profile,
                    exit_code,
                },
            );
        } else {
            crate::log_warn!("实例 {id} TUI 会话退出（exit code: {exit_code:?}）");
            emit_instance_status(
                &app,
                &InstanceStatus {
                    id: id.clone(),
                    state: InstanceState::Exited,
                    url: None,
                    profile: Some(profile),
                    exit_code,
                },
            );
            crate::windows::close_tui_window(&app, &id);
        }
        crate::tray::rebuild_tray_menu(&app).await;
    });
}

/// Stops a TUI instance: signals the waiter (which kills the PTY child and
/// emits Stopped). Idempotent — without a live session it still converges
/// the frontend status, mirroring the web path's stop semantics.
pub async fn stop_tui_session(
    app: &AppHandle,
    state: &State<'_, AppState>,
    instance_id: &str,
) -> Result<(), String> {
    // Notify first, then drop the killer as backup (the waiter takes the
    // killer before selecting, so either path reaches it).
    let kill = {
        let map = state.tui_sessions.lock().await;
        map.get(instance_id).map(|s| s.kill.clone())
    };
    if let Some(kill) = kill {
        kill.notify_one();
        return Ok(());
    }
    // No live session: still converge the frontend to a terminal state.
    let profile = state
        .config
        .lock()
        .unwrap()
        .instances
        .iter()
        .find(|i| i.id == instance_id)
        .and_then(|i| i.last_profile.clone());
    emit_instance_status(
        app,
        &InstanceStatus {
            id: instance_id.to_string(),
            state: InstanceState::Stopped,
            url: None,
            profile,
            exit_code: None,
        },
    );
    Ok(())
}

/// Kills every TUI session (called on launcher exit). Best-effort: notify
/// the waiter and, as a fallback, kill through the killer handle directly
/// (the waiter task may not get to run again during shutdown).
pub fn kill_all(state: &AppState) {
    let mut map = state.tui_sessions.blocking_lock();
    for (_, session) in map.drain() {
        session.kill.notify_one();
        if let Ok(mut slot) = session.killer.try_lock() {
            if let Some(mut k) = slot.take() {
                let _ = k.kill();
            }
        }
    }
}

/// Appends raw PTY output to the instance log (same file as web instances).
fn append_log(data_dir: &std::path::Path, instance_id: &str, bytes: &[u8]) {
    let log_path = data_dir.join("logs").join(format!("{instance_id}.log"));
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = f.write_all(bytes);
    }
}

/// Data event payload: base64 of raw PTY bytes.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TuiData {
    instance_id: String,
    data: String,
}

/// Writes raw input to a live TUI session's PTY master.
#[tauri::command(rename_all = "snake_case")]
pub async fn write_tui_input(
    state: State<'_, AppState>,
    input: TuiWriteInput,
) -> Result<(), String> {
    let bytes = base64_decode(&input.data)?;
    let map = state.tui_sessions.lock().await;
    let session = map
        .get(&input.instance_id)
        .ok_or_else(|| "TUI 会话不存在".to_string())?;
    let mut writer = session
        .writer
        .lock()
        .map_err(|_| "TUI 写入通道已锁定".to_string())?;
    writer
        .write_all(&bytes)
        .map_err(|e| format!("写入 TUI 失败: {e}"))?;
    writer
        .flush()
        .map_err(|e| format!("刷新 TUI 写入失败: {e}"))?;
    Ok(())
}

/// Resizes a live TUI session's PTY.
#[tauri::command(rename_all = "snake_case")]
pub async fn resize_tui_session(
    state: State<'_, AppState>,
    input: TuiSessionInput,
) -> Result<(), String> {
    let cols = input.cols.unwrap_or(DEFAULT_COLS);
    let rows = input.rows.unwrap_or(DEFAULT_ROWS);
    let map = state.tui_sessions.lock().await;
    let session = map
        .get(&input.instance_id)
        .ok_or_else(|| "TUI 会话不存在".to_string())?;
    session
        .master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("调整 TUI 大小失败: {e}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_roundtrip() {
        let raw = b"\x1b[?25lhello\r\n";
        let enc = super::base64_encode(raw);
        assert_eq!(super::base64_decode(&enc).unwrap(), raw);
    }

    #[test]
    fn base64_decode_rejects_garbage() {
        assert!(super::base64_decode("not-base64!").is_err());
    }
}
