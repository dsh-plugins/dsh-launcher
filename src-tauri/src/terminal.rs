//! Embedded instance terminal: a PTY-backed shell per instance, streamed to
//! the frontend over Tauri events and driven by commands. The terminal lives
//! inside the instance settings page (xterm.js), with a `dsh` shim injected
//! into PATH so `dsh ...` drives the current instance's own CLI.
//!
//! Architecture:
//! - One `TerminalSession` per instance, stored in `AppState.terminals`
//!   (Mutex<HashMap<instance_id, session>>). It owns the PTY master (writer +
//!   resize) and the child handle.
//! - A per-session reader task forwards PTY output to the frontend as
//!   `terminal://data` events (base64 of raw bytes) and emits
//!   `terminal://status` when the shell exits.
//! - The shell env carries DSH_HOME / DSH_LAUNCHER_INSTANCE / env_overrides
//!   (same as the instance process) and the shim dir prepended to PATH.

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;
use std::io::{Read, Write};
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::AppState;

pub const DATA_EVENT: &str = "terminal://data";
pub const STATUS_EVENT: &str = "terminal://status";

/// Input for starting / restarting an instance's embedded terminal session.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartTerminalInput {
    pub instance_id: String,
    /// Initial PTY size in columns x rows (from the frontend xterm size).
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}

/// Input for writing / resizing / closing a session.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalIpcInput {
    pub instance_id: String,
    /// For `write`: base64 of raw bytes to feed the PTY master.
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
}

/// Serializable session state for the frontend.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalStatus {
    pub instance_id: String,
    pub running: bool,
    pub exit_code: Option<i32>,
}

/// A live PTY-backed shell session.
pub struct TerminalSession {
    /// PTY master: used for resize.
    pub master: Box<dyn portable_pty::MasterPty + Send>,
    /// Writable handle to the PTY master, taken once at spawn (portable-pty
    /// allows `take_writer` exactly once). All input writes go through this.
    /// Shared with the reader task so it can answer terminal queries (DSR).
    pub writer: std::sync::Arc<std::sync::Mutex<Box<dyn std::io::Write + Send>>>,
    /// The child process handle. Dropping it kills the shell.
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
}

// ---------------------------------------------------------------------------
// Session registry helpers
// ---------------------------------------------------------------------------

/// Drops the session for an instance (drops the child -> kills the shell).
pub async fn drop_session(state: &State<'_, AppState>, instance_id: &str) {
    state.terminals.lock().await.remove(instance_id);
}

/// Kills every session (called on launcher exit).
pub fn kill_all(state: &AppState) {
    let mut map = state.terminals.blocking_lock();
    map.clear(); // dropping the children kills the shells
}

// ---------------------------------------------------------------------------
// Spawn
// ---------------------------------------------------------------------------

/// Builds the effective environment for the terminal shell: DSH_HOME /
/// DSH_LAUNCHER_INSTANCE / env_overrides (same as the instance process),
/// plus a `dsh` shim dir prepended to PATH.
fn terminal_env(
    state: &State<'_, AppState>,
    instance_id: &str,
    shim_dir: Option<&std::path::Path>,
) -> Result<Vec<(String, String)>, String> {
    let cfg = state.config.lock().unwrap();
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

    // WSL terminals (issue #19 follow-up): the Windows `dsh` shim cannot run
    // inside bash, so PATH is left to the distro.
    if let Some(shim_dir) = shim_dir {
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let mut entries = vec![shim_dir.to_path_buf()];
        entries.extend(std::env::split_paths(&existing));
        match std::env::join_paths(entries) {
            Ok(joined) => env.push(("PATH".to_string(), joined.to_string_lossy().to_string())),
            Err(e) => crate::log_warn!("拼接 PATH 失败，沿用系统 PATH: {e}"),
        }
    }
    Ok(env)
}

/// Builds the `dsh` shim script plus a `node` that resolves to the running
/// DSH version's `bin.js`, into `<data_dir>/bin`. Returns the directory to
/// prepend to PATH.
fn prepare_shim(
    data_dir: &std::path::Path,
    version_dir: &std::path::Path,
) -> Result<PathBuf, String> {
    let bin_dir = data_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).map_err(|e| format!("创建 bin 目录失败: {e}"))?;

    let bin_js = crate::process::version_bin(version_dir);
    if !crate::process::version_bin_ready(version_dir) {
        return Err(format!(
            "版本安装不完整（缺少 {}），请重新安装该 DSH 版本",
            bin_js.display()
        ));
    }

    if cfg!(windows) {
        let body = format!(
            "@echo off\r\n\"{}\" \"{}\" %*\r\n",
            crate::process::node(),
            bin_js.display()
        );
        std::fs::write(bin_dir.join("dsh.cmd"), &body)
            .map_err(|e| format!("写入 dsh.cmd 失败: {e}"))?;
        std::fs::write(bin_dir.join("dsh.bat"), &body)
            .map_err(|e| format!("写入 dsh.bat 失败: {e}"))?;
    } else {
        let dsh = bin_dir.join("dsh");
        let body = format!(
            "#!/bin/sh\nexec \"{}\" \"{}\" \"$@\"\n",
            crate::process::node(),
            bin_js.display()
        );
        std::fs::write(&dsh, body).map_err(|e| format!("写入 dsh 失败: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dsh, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("设置 dsh 可执行权限失败: {e}"))?;
        }
    }
    Ok(bin_dir)
}

/// Whether a version's bin.js exists and is non-empty *inside the distro*.
/// The Linux path is not readable from Windows, so probe with `test -s`
/// through wsl.exe — the WSL counterpart of `process::version_bin_ready`.
/// Split out (and taking `distro` rather than `State`) so the WSL-vs-local
/// decision is unit-testable without a live distro.
pub(crate) async fn wsl_version_bin_ready(distro: &str, bin: &std::path::Path) -> bool {
    crate::wsl::wsl_test(distro, "-s", &bin.to_string_lossy()).await
}

/// The shell program for the platform: PowerShell on Windows (pwsh when
/// available, else the built-in Windows PowerShell), the user's $SHELL (or
/// /bin/sh) on unix. portable-pty's CommandBuilder inherits the base env, so
/// the shell resolves through the injected PATH (dsh shim first).
fn shell_program() -> String {
    if cfg!(windows) {
        // Prefer PowerShell Core (pwsh) for modern syntax; fall back to the
        // built-in Windows PowerShell 5.1 (powershell.exe), which ships with
        // every Windows install. Check PATH first (pwsh is commonly added),
        // then the standard Program Files location.
        let in_path = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|dir| dir.join("pwsh.exe").exists()))
            .unwrap_or(false);
        if in_path {
            return "pwsh.exe".to_string();
        }
        if let Some(pf) = std::env::var_os("PROGRAMFILES") {
            let pwsh = std::path::Path::new(&pf)
                .join("PowerShell")
                .join("7")
                .join("pwsh.exe");
            if pwsh.exists() {
                return pwsh.to_string_lossy().to_string();
            }
        }
        "powershell.exe".to_string()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

/// Spawns the shell for an instance inside a fresh PTY, returning the
/// session (master + child) and the cwd it started in.
async fn spawn_session(
    state: &State<'_, AppState>,
    instance_id: &str,
    cols: u16,
    rows: u16,
) -> Result<(TerminalSession, PathBuf), String> {
    // Resolve both path flavours + the distro in one config lock: `_fs` paths
    // go to Windows file APIs, `_linux` paths go inside the distro (see
    // `wsl.rs` for the naming convention).
    let (home_fs, version_fs, wsl_distro, version_linux, version_label) = {
        let cfg = state.config.lock().unwrap();
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
        let version = cfg
            .versions
            .iter()
            .find(|v| v.id == inst.version_id)
            .ok_or_else(|| "版本不存在".to_string())?;
        (
            crate::wsl::home_fs_path(home),
            crate::wsl::version_fs_path(version),
            home.wsl.clone(),
            version.dir.clone(),
            version.version.clone(),
        )
    };
    // WSL instances (issue #19 follow-up): the terminal shell is the distro's
    // bash through wsl.exe. The Windows `dsh` shim cannot run inside bash, so
    // PATH is left to the distro; DSH_HOME is already the Linux path and the
    // session starts in the Linux HOME.
    //
    // The WSL branch deliberately skips `prepare_shim`: that function writes a
    // Windows `dsh.cmd`/`dsh.bat` shim and validates the version with
    // `version_bin_ready`, a Windows `exists()` against the *Linux* version
    // dir that can never succeed. Its validation is replaced by the in-distro
    // `test -s` probe below (issue #49 G1), which checks the same bin.js
    // through the only path that can actually see it.
    let shell: String = match &wsl_distro {
        Some(_) => "wsl.exe".to_string(),
        None => shell_program(),
    };
    let shim_dir = if let Some(distro) = &wsl_distro {
        let bin = crate::process::version_bin(&version_linux);
        if !wsl_version_bin_ready(distro, &bin).await {
            return Err(crate::process::version_missing_message(
                &version_label,
                &bin,
            ));
        }
        None
    } else {
        Some(prepare_shim(&state.data_dir, &version_fs)?)
    };
    let env = terminal_env(state, instance_id, shim_dir.as_deref())?;

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("创建 PTY 失败: {e}"))?;

    let mut cmd = CommandBuilder::new(&shell);
    if let Some(d) = &wsl_distro {
        // UNC paths are not valid Windows process working directories; start
        // wsl.exe from a neutral dir and let bash land in the Linux HOME
        // (wsl.exe -- bash -i already starts in $HOME by default).
        //
        // The env must be written into the script: `Command::env` only sets it
        // on wsl.exe, and WSL does not forward the Windows environment into the
        // distro (that needs WSLENV), so DSH_HOME / DSH_LAUNCHER_INSTANCE /
        // overrides / proxy would silently never reach the shell (issue #49 S2).
        let script = format!("{}; exec bash -i", crate::wsl::env_exports(&env));
        cmd.args(["-d", d, "--", "bash", "-lc", &script]);
        cmd.cwd(std::env::temp_dir().as_os_str());
    } else {
        cmd.cwd(home_fs.as_os_str());
        for (k, v) in &env {
            cmd.env(k, v);
        }
    }

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("启动 shell 失败: {e}"))?;
    drop(pair.slave); // Close our copy of the slave so EOF propagates.

    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("获取 PTY 写入通道失败: {e}"))?;

    Ok((
        TerminalSession {
            master: pair.master,
            writer: std::sync::Arc::new(std::sync::Mutex::new(writer)),
            child,
        },
        home_fs,
    ))
}

/// Starts (or restarts) the embedded terminal session for an instance.
#[tauri::command(rename_all = "snake_case")]
pub async fn start_terminal_session(
    app: AppHandle,
    state: State<'_, AppState>,
    input: StartTerminalInput,
) -> Result<TerminalStatus, String> {
    // Restart semantics: close any existing session first.
    drop_session(&state, &input.instance_id).await;

    let (session, _home) =
        spawn_session(&state, &input.instance_id, input.cols, input.rows).await?;
    let id = input.instance_id.clone();

    // Register the session before spawning the reader so input arriving
    // before the first status event finds the registry entry.
    state.terminals.lock().await.insert(id.clone(), session);

    spawn_reader(&app, &state, &id).await?;

    emit_status(
        &app,
        &TerminalStatus {
            instance_id: id.clone(),
            running: true,
            exit_code: None,
        },
    );
    Ok(TerminalStatus {
        instance_id: id,
        running: true,
        exit_code: None,
    })
}

/// Reader task: forwards PTY output as `terminal://data` events; on EOF marks
/// the session dead, emits `terminal://status` and unregisters it.
///
/// Also answers terminal queries (DSR `ESC[6n` / `ESC[?6n`): console programs
/// like cmd.exe send this on startup and block until the terminal reports the
/// cursor position (`ESC[<row>;<col>R`). Without the answer the shell never
/// prints a prompt and input appears dead.
async fn spawn_reader(
    app: &AppHandle,
    state: &State<'_, AppState>,
    instance_id: &str,
) -> Result<(), String> {
    let (master, writer) = {
        let map = state.terminals.lock().await;
        let session = map
            .get(instance_id)
            .ok_or_else(|| "终端会话不存在".to_string())?;
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

    tauri::async_runtime::spawn(async move {
        let mut reader = master;
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    // cmd.exe and other console programs send a DSR cursor
                    // query on startup and block until answered.
                    if find_dsr(chunk) {
                        answer_dsr(&writer);
                    }
                    let _ = app.emit(
                        DATA_EVENT,
                        TerminalData {
                            instance_id: id.clone(),
                            data: base64_encode(chunk),
                        },
                    );
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        // Shell exited: notify the frontend and unregister the session.
        let _ = app.emit(
            STATUS_EVENT,
            TerminalStatus {
                instance_id: id.clone(),
                running: false,
                exit_code: None,
            },
        );
        // Remove the session from the registry (drops child handle -> kill).
        let state = app.state::<AppState>();
        state.terminals.lock().await.remove(&id);
    });
    Ok(())
}

/// Finds a DSR query (`ESC[6n` / `ESC[?6n`) anywhere in `buf`.
/// Console programs send the query as a standalone sequence, but it can also
/// trail other output, so scan every `ESC[` position and validate the body.
fn find_dsr(buf: &[u8]) -> bool {
    let mut start = 0;
    while let Some(rel) = buf[start..].windows(2).position(|w| w == b"\x1b[") {
        let abs = start + rel;
        let rest = &buf[abs + 2..];
        let rest = rest.strip_prefix(b"?").unwrap_or(rest);
        let digits: usize = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits > 0 && rest.get(digits) == Some(&b'n') {
            return true;
        }
        // Not a DSR here; keep scanning after this escape.
        start = abs + 2;
    }
    false
}

/// Answers a DSR query by writing the cursor position report back to the PTY.
fn answer_dsr(writer: &std::sync::Arc<std::sync::Mutex<Box<dyn std::io::Write + Send>>>) {
    // Report a fixed position (1;1). Real terminals report the actual cursor,
    // but the shell only needs a syntactically valid answer to unblock.
    if let Ok(mut w) = writer.lock() {
        let _ = w.write_all(b"\x1b[1;1R");
        let _ = w.flush();
    }
}

/// Writes raw input to the instance's PTY master. The writer is taken once at
/// spawn and stored in the session, so this is safe to call repeatedly.
#[tauri::command(rename_all = "snake_case")]
pub async fn write_terminal_input(
    state: State<'_, AppState>,
    input: TerminalIpcInput,
) -> Result<(), String> {
    let bytes = base64_decode(input.data.as_deref().unwrap_or(""))?;
    let mut map = state.terminals.lock().await;
    let session = map
        .get_mut(&input.instance_id)
        .ok_or_else(|| "终端会话不存在".to_string())?;
    let mut writer = session
        .writer
        .lock()
        .map_err(|_| "终端写入通道已锁定".to_string())?;
    writer
        .write_all(&bytes)
        .map_err(|e| format!("写入终端失败: {e}"))?;
    writer
        .flush()
        .map_err(|e| format!("刷新终端写入失败: {e}"))?;
    Ok(())
}

/// Resizes the instance's PTY.
#[tauri::command(rename_all = "snake_case")]
pub async fn resize_terminal_session(
    state: State<'_, AppState>,
    input: TerminalIpcInput,
) -> Result<(), String> {
    let cols = input.cols.unwrap_or(80);
    let rows = input.rows.unwrap_or(24);
    let map = state.terminals.lock().await;
    let session = map
        .get(&input.instance_id)
        .ok_or_else(|| "终端会话不存在".to_string())?;
    session
        .master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("调整终端大小失败: {e}"))
}

/// Closes the instance's terminal session (kills the shell).
#[tauri::command(rename_all = "snake_case")]
pub async fn close_terminal_session(
    state: State<'_, AppState>,
    input: TerminalIpcInput,
) -> Result<(), String> {
    drop_session(&state, &input.instance_id).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Data event payload: base64 of raw PTY bytes.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalData {
    instance_id: String,
    data: String,
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

fn emit_status(app: &AppHandle, status: &TerminalStatus) {
    let _ = app.emit(STATUS_EVENT, status);
}

#[cfg(test)]
mod tests {
    use crate::process::node as node_bin;

    #[test]
    fn shim_body_escapes_paths() {
        let body = format!(
            "@echo off\r\n\"{}\" \"{}\" %*\r\n",
            node_bin(),
            "C:\\Program Files\\nodejs\\node.exe"
        );
        assert!(body.contains("\"C:\\Program Files\\nodejs\\node.exe\""));
        assert!(body.ends_with("%*\r\n"));
    }

    #[test]
    fn base64_roundtrip() {
        let raw = b"echo hello\r\n";
        let enc = super::base64_encode(raw);
        assert_eq!(super::base64_decode(&enc).unwrap(), raw);
    }

    #[test]
    fn dsr_queries_are_detected() {
        assert!(super::find_dsr(b"\x1b[6n"));
        assert!(super::find_dsr(b"\x1b[?6n"));
        assert!(super::find_dsr(b"\x1b[1;1R\x1b[6n"));
        assert!(!super::find_dsr(b""));
        assert!(!super::find_dsr(b"hello"));
        assert!(!super::find_dsr(b"\x1b["));
        assert!(!super::find_dsr(b"\x1b[6")); // incomplete
        assert!(!super::find_dsr(b"\x1b[6x")); // not a query
    }

    #[cfg(windows)]
    #[test]
    fn windows_shell_is_powershell_based() {
        let shell = super::shell_program().to_lowercase();
        // Must be PowerShell (Core or Windows PowerShell), never cmd.
        assert!(
            shell.ends_with("pwsh.exe") || shell.ends_with("powershell.exe"),
            "unexpected windows shell: {shell}"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_shell_is_user_shell_or_sh() {
        let shell = super::shell_program();
        assert!(!shell.is_empty());
    }

    /// issue #49 G1: the WSL terminal must not validate the version through
    /// the Windows `exists()` path (a Linux dir can never exist there); the
    /// error message must name the in-distro path the probe actually checked.
    #[test]
    fn wsl_version_missing_message_names_the_linux_bin() {
        let bin = std::path::Path::new(
            "/home/u/.dsh-launcher/versions/0.2.4/node_modules/@deepseek-ai/dsh/lib/bin.js",
        );
        let msg = crate::process::version_missing_message("0.2.4", bin);
        assert!(
            msg.contains("0.2.4"),
            "message must name the version: {msg}"
        );
        assert!(
            msg.contains("/home/u/.dsh-launcher/versions/0.2.4"),
            "message must name the Linux bin path: {msg}"
        );
        // The UNC form would be a path the user cannot see inside the distro.
        assert!(
            !msg.contains(r"\\wsl$"),
            "must not leak the UNC path: {msg}"
        );
    }

    /// The probe flag is the safety net for "install incomplete": `-s` (exists
    /// and is non-empty) rather than `-f`, so a truncated bin.js is caught
    /// the same way `version_bin_ready` catches it on the Windows side.
    #[test]
    fn wsl_probe_uses_non_empty_test() {
        let src = include_str!("terminal.rs");
        let call = src
            .lines()
            .find(|l| l.contains("crate::wsl::wsl_test(distro, "))
            .expect("wsl_version_bin_ready must probe through wsl_test");
        assert!(
            call.contains(r#""-s""#),
            "the in-distro probe must use test -s, got: {call}"
        );
    }
}
