//! WSL execution bridge (issue #19): provisions and runs DSH inside a WSL2
//! distro through `wsl.exe`, keeping the launcher's process/log pipeline
//! (stdout URL capture, log files, status events) unchanged.

use tauri::{AppHandle, State};
use tokio::process::Command;

use crate::AppState;

/// Marker prefix printed by the WSL launch wrapper; the value is the inner
/// process PID (the wrapper `exec`s node, so `$$` IS the node process),
/// captured for a clean stop.
pub const PID_MARKER: &str = "DSH_PID:";

/// Parses the launch wrapper's PID marker line.
pub fn parse_pid_marker(line: &str) -> Option<u32> {
    line.trim()
        .strip_prefix(PID_MARKER)
        .and_then(|rest| rest.trim().parse().ok())
}

/// Builds `wsl.exe -d <distro> -- <argv...>` (no shell, no expansion).
pub fn wsl_cmd(distro: &str, argv: &[String]) -> Command {
    let mut cmd = Command::new("wsl.exe");
    crate::process::hide_console(&mut cmd);
    cmd.arg("-d").arg(distro).arg("--").args(argv);
    cmd
}

/// Builds `wsl.exe -d <distro> -- bash -lc <script>` for shell features
/// ($HOME, PATH export, exec).
pub fn wsl_bash(distro: &str, script: &str) -> Command {
    wsl_cmd(
        distro,
        &["bash".to_string(), "-lc".to_string(), script.to_string()],
    )
}

/// Single-quotes a value for embedding in a bash script.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Runs a wsl command to completion, returning trimmed stdout.
pub async fn wsl_output(distro: &str, argv: &[String]) -> Result<String, String> {
    let out = wsl_cmd(distro, argv)
        .output()
        .await
        .map_err(|e| format!("wsl.exe 执行失败: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            "WSL 命令失败".to_string()
        } else {
            format!("WSL 命令失败: {err}")
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Tests a path inside the distro (`test -e` / `-f` / `-d`).
pub async fn wsl_test(distro: &str, flag: &str, path: &str) -> bool {
    wsl_cmd(distro, &["test".into(), flag.into(), path.into()])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Maps a Linux path inside a distro to its `\\wsl$\` UNC path so plain
/// Windows file APIs (package.json probes, icons, opening the directory)
/// keep working while the distro is running.
pub fn unc_path(distro: &str, linux_path: &str) -> std::path::PathBuf {
    let rest = linux_path.trim_start_matches('/').replace('/', "\\");
    std::path::PathBuf::from(format!(r"\\wsl$\{distro}\{rest}"))
}

/// Lists installed WSL distros (`wsl.exe -l -q`), excluding the internal
/// docker-desktop distros. Empty when WSL is unavailable.
#[cfg(windows)]
pub fn list_distros() -> Vec<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("wsl.exe")
        .args(["-l", "-q"])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output();
    let Ok(out) = out else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    decode_distro_list(&out.stdout)
}

#[cfg(not(windows))]
pub fn list_distros() -> Vec<String> {
    Vec::new()
}

/// wsl.exe prints UTF-16LE when its stdout is piped; decode and clean the
/// distro list.
pub(crate) fn decode_distro_list(bytes: &[u8]) -> Vec<String> {
    let utf16ish = bytes.len() >= 2 && (bytes.starts_with(&[0xFF, 0xFE]) || bytes[1] == 0);
    let text = if utf16ish {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).to_string()
    };
    text.lines()
        .map(|l| l.trim().trim_matches('\0').trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with("docker-desktop"))
        .collect()
}

/// The managed root `~/.dsh-launcher` inside a distro: tools (node/pnpm),
/// versions, and per-instance homes — mirrors the launcher's data dir.
#[derive(Clone, Debug)]
pub struct WslRoot(pub String);

impl WslRoot {
    /// Resolves `$HOME/.dsh-launcher` inside the distro.
    pub async fn resolve(distro: &str) -> Result<Self, String> {
        let home = wsl_output(
            distro,
            &[
                "bash".to_string(),
                "-lc".to_string(),
                "printf %s \"$HOME\"".to_string(),
            ],
        )
        .await?;
        let home = home.trim_end_matches(['/', '\n', '\r']).to_string();
        if home.is_empty() {
            return Err("无法获取 WSL 用户目录".to_string());
        }
        Ok(Self(format!("{home}/.dsh-launcher")))
    }

    pub fn node_bin_dir(&self) -> String {
        format!("{}/tools/node/bin", self.0)
    }
    pub fn node_exe(&self) -> String {
        format!("{}/node", self.node_bin_dir())
    }
    pub fn pnpm_exe(&self) -> String {
        format!("{}/tools/bin/pnpm", self.0)
    }
    pub fn tools_prefix(&self) -> String {
        format!("{}/tools", self.0)
    }
    pub fn version_dir(&self, version: &str) -> String {
        format!("{}/versions/{version}", self.0)
    }
    pub fn home_dir(&self, name: &str) -> String {
        format!("{}/homes/{name}", self.0)
    }
    pub fn pnpm_store(&self) -> String {
        format!("{}/.pnpm-store", self.0)
    }
}

/// Ensures a Node.js runtime exists inside the distro: reuses the managed
/// `tools/node` when present, otherwise downloads the latest LTS linux-x64
/// tarball on the Windows side (proxy + mirror fallback apply) and streams
/// it into the distro through wsl.exe's stdin — no curl/wget needed inside.
pub async fn ensure_node(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    distro: &str,
    root: &WslRoot,
) -> Result<(), String> {
    if let Ok(v) = wsl_output(distro, &[root.node_exe(), "--version".into()]).await {
        if !v.is_empty() {
            crate::tasks::push_task_log_pub(
                app,
                state,
                task_id,
                &format!("WSL Node.js 已就绪（{v}）"),
            )
            .await;
            return Ok(());
        }
    }

    crate::tasks::push_task_log_pub(app, state, task_id, "正在查询 Node.js 最新 LTS 版本…").await;
    let version = crate::runtime::resolve_node_version().await?;
    crate::tasks::push_task_log_pub(
        app,
        state,
        task_id,
        &format!("正在为 WSL（{distro}）安装 Node.js {version}…"),
    )
    .await;
    let name = crate::runtime::node_archive_name_linux(&version);
    let tmp = std::env::temp_dir().join(&name);
    crate::runtime::download_node_archive(app, state, task_id, &version, &name, &tmp).await?;

    // Stream the tarball into the distro and unpack it in one go.
    let dir = format!("{}/tools/node", root.0);
    let script = format!(
        "mkdir -p {dir} && tar -xzf - --strip-components=1 -C {dir}",
        dir = sh_quote(&dir)
    );
    let mut child = wsl_bash(distro, &script)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("WSL 解压进程启动失败: {e}"))?;
    let mut stdin = child.stdin.take().unwrap();
    let mut file = tokio::fs::File::open(&tmp)
        .await
        .map_err(|e| format!("打开下载文件失败: {e}"))?;
    let copy = tokio::io::copy(&mut file, &mut stdin).await;
    drop(stdin); // close the pipe so tar sees EOF
    copy.map_err(|e| format!("传输 Node.js 到 WSL 失败: {e}"))?;
    let status = child
        .wait()
        .await
        .map_err(|e| format!("等待 WSL 解压失败: {e}"))?;
    std::fs::remove_file(&tmp).ok();
    if !status.success() {
        return Err("在 WSL 内解压 Node.js 失败".to_string());
    }

    let v = wsl_output(distro, &[root.node_exe(), "--version".into()]).await?;
    crate::tasks::push_task_log_pub(app, state, task_id, &format!("WSL Node.js {v} 安装完成"))
        .await;
    Ok(())
}

/// Ensures the pinned pnpm major inside the distro (bootstrapped through the
/// managed Node's npm), returning the pnpm executable path.
pub async fn ensure_pnpm(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    distro: &str,
    root: &WslRoot,
) -> Result<String, String> {
    if let Ok(v) = wsl_output(distro, &[root.pnpm_exe(), "--version".into()]).await {
        if crate::tasks::pnpm_major_pub(&v) == Some(crate::tasks::REQUIRED_PNPM_MAJOR) {
            return Ok(root.pnpm_exe());
        }
        crate::tasks::push_task_log_pub(
            app,
            state,
            task_id,
            &format!(
                "WSL pnpm 版本 {v} 与所需主版本 {} 不符，重新安装…",
                crate::tasks::REQUIRED_PNPM_MAJOR
            ),
        )
        .await;
    }

    let spec = format!("pnpm@{}", crate::tasks::REQUIRED_PNPM_MAJOR);
    crate::tasks::push_task_log_pub(app, state, task_id, &format!("正在 WSL 中安装 {spec}…")).await;
    let script = format!(
        "export PATH={}:\"$PATH\"; npm install --global --prefix {} {}",
        sh_quote(&root.node_bin_dir()),
        sh_quote(&root.tools_prefix()),
        sh_quote(&spec),
    );
    let cmd = wsl_bash(distro, &script);
    crate::tasks::run_streamed_command(app, state, task_id, cmd, "pnpm 安装（WSL）").await?;
    if !wsl_test(distro, "-f", &root.pnpm_exe()).await {
        return Err(format!("pnpm 安装完成但未找到: {}", root.pnpm_exe()));
    }
    Ok(root.pnpm_exe())
}

/// The bash wrapper used to run an instance inside WSL: prints the inner PID
/// marker, then execs node with the DSH CLI. Everything is single-quoted so
/// env values and paths with spaces are safe.
pub fn launch_script(
    home: &str,
    node: &str,
    bin: &str,
    env: &[(String, String)],
    cli_args: &[String],
) -> String {
    let env_pairs = env
        .iter()
        .map(|(k, v)| format!("{}={}", k, sh_quote(v)))
        .collect::<Vec<_>>()
        .join(" ");
    let mut cmd = format!("{} {}", sh_quote(node), sh_quote(bin));
    for a in cli_args {
        cmd.push(' ');
        cmd.push_str(&sh_quote(a));
    }
    format!(
        "echo {PID_MARKER}$$; cd {home} && exec env {env_pairs} {cmd}",
        home = sh_quote(home)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pid_marker_lines() {
        assert_eq!(parse_pid_marker("DSH_PID:1234"), Some(1234));
        assert_eq!(parse_pid_marker("  DSH_PID:42  "), Some(42));
        assert_eq!(parse_pid_marker("DSH_PID:"), None);
        assert_eq!(parse_pid_marker("dsh web: http://127.0.0.1:1"), None);
    }

    #[test]
    fn decode_distro_list_handles_utf16() {
        // `wsl.exe -l -q` piped output: UTF-16LE.
        let utf16: Vec<u8> = "Ubuntu\r\nDebian\r\ndocker-desktop\r\n"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert_eq!(decode_distro_list(&utf16), vec!["Ubuntu", "Debian"]);
        assert_eq!(decode_distro_list(b"Ubuntu\n"), vec!["Ubuntu"]);
        assert_eq!(decode_distro_list(b""), Vec::<String>::new());
    }

    #[test]
    fn sh_quote_escapes_single_quotes() {
        assert_eq!(sh_quote("plain"), "'plain'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote("a b"), "'a b'");
    }

    #[test]
    fn launch_script_marks_pid_and_execs() {
        let s = launch_script(
            "/home/u/.dsh-launcher/homes/x",
            "/home/u/.dsh-launcher/tools/node/bin/node",
            "/home/u/.dsh-launcher/versions/0.1.0/bin.js",
            &[
                (
                    "DSH_HOME".to_string(),
                    "/home/u/.dsh-launcher/homes/x".to_string(),
                ),
                ("FOO".to_string(), "it's a value".to_string()),
            ],
            &["--profile".to_string(), "web".to_string()],
        );
        assert!(s.starts_with("echo DSH_PID:$$;"));
        assert!(s.contains("exec env DSH_HOME='/home/u/.dsh-launcher/homes/x'"));
        assert!(s.contains(r"FOO='it'\''s a value'"));
        assert!(s.ends_with("'--profile' 'web'"));
    }

    #[test]
    fn unc_path_maps_linux_to_wsl_share() {
        assert_eq!(
            unc_path("Ubuntu", "/home/u/.dsh-launcher"),
            std::path::PathBuf::from(r"\\wsl$\Ubuntu\home\u\.dsh-launcher")
        );
    }
}
