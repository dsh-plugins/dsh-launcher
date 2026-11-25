use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::AppState;

pub(crate) struct LaunchPatch {
    path: PathBuf,
    file: tokio::fs::File,
}

impl LaunchPatch {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(not(windows))]
impl Drop for LaunchPatch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                crate::log_warn!("清理 Computer Use 启动补丁失败: {error}");
            }
        }
    }
}

/// 只对原生启动检查当前 profile。补全失败不影响 DSH 的默认启动流程。
pub(crate) async fn prepare(
    state: &AppState,
    dir: &Path,
    version: &str,
    profile: &str,
    env: &[(String, String)],
    overlays: &[&Path],
    cwd: Option<&Path>,
) -> Option<LaunchPatch> {
    if !cfg!(any(windows, target_os = "macos")) || !crate::cua_helper::release_version(version) {
        return None;
    }
    if !crate::cua_helper::supported_version(version) {
        let root = crate::cua_helper::root(&state.data_dir, &state.config.lock().unwrap().settings);
        let installed = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::fs::metadata(root.join(version)),
        )
        .await;
        if !matches!(installed, Ok(Ok(metadata)) if metadata.is_dir()) {
            return None;
        }
    }
    let mut command = Command::new(crate::process::node());
    crate::process::hide_console(&mut command);
    let bin = crate::process::version_bin(dir);
    command
        .args(["-e", include_str!("cua_launch.cjs"), "--"])
        .arg(bin)
        .arg(profile)
        .args(overlays)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .env_remove("NODE_OPTIONS")
        .env_remove("NODE_PATH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    match prepare_patch(command, state, dir, version).await {
        Ok(patch) => patch,
        Err(error) => {
            crate::log_warn!("Computer Use 启动补丁已跳过: {error}");
            None
        }
    }
}

async fn prepare_patch(
    mut command: Command,
    state: &AppState,
    dir: &Path,
    version: &str,
) -> Result<Option<LaunchPatch>, String> {
    let mut child = command.spawn().map_err(|_| "无法启动本地配置读取器")?;
    let mut output = child.stdout.take().ok_or("无法读取合成配置")?;
    let decision = tokio::time::timeout(Duration::from_secs(15), output.read_u8())
        .await
        .map_err(|_| "读取 DSH 合成配置超时")?
        .map_err(|_| "DSH 合成配置读取失败")?;
    if decision == b'S' {
        return Ok(None);
    }
    if decision != b'N' {
        return Err("DSH 配置读取器返回无效状态".into());
    }
    let helper = crate::cua_helper::resolve_for_launch(state, dir, version)
        .await?
        .ok_or("未找到匹配版本和架构的助手，请在设置的 Computer Use 页下载，或对该 DSH 版本执行「补全文件」")?;
    let helper = helper.to_str().ok_or("助手路径无法交给 Node")?;
    let serialize = async {
        let mut input = child.stdin.take().ok_or("无法传递助手路径")?;
        let encoded = serde_json::to_vec(helper).map_err(|_| "无法编码助手路径")?;
        input
            .write_all(&encoded)
            .await
            .map_err(|_| "传递助手路径失败")?;
        drop(input);
        const MAX_PATCH: usize = 8 * 1024 * 1024;
        let mut bytes = Vec::new();
        output
            .take((MAX_PATCH + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "读取启动补丁失败")?;
        if bytes.len() > MAX_PATCH
            || bytes.is_empty()
            || !child
                .wait()
                .await
                .map_err(|_| "配置读取器异常退出")?
                .success()
        {
            return Err("生成 Computer Use 启动补丁失败");
        }
        Ok(bytes)
    };
    let bytes = tokio::time::timeout(Duration::from_secs(5), serialize)
        .await
        .map_err(|_| "生成 Computer Use 启动补丁超时")??;
    let path = std::env::temp_dir().join(format!(".cua-launch-{}.yml", uuid::Uuid::new_v4()));
    let mut options = tokio::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    #[cfg(windows)]
    {
        // CreateFileW 的删除标记允许 Node 按共享删除方式读取，启动器异常退出也会回收。
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        options.custom_flags(FILE_FLAG_DELETE_ON_CLOSE);
    }
    let file = options
        .open(&path)
        .await
        .map_err(|_| "创建私有启动补丁失败")?;
    let mut patch = LaunchPatch { path, file };
    patch
        .file
        .write_all(&bytes)
        .await
        .map_err(|_| "写入启动补丁失败")?;
    patch.file.flush().await.map_err(|_| "保存启动补丁失败")?;
    Ok(Some(patch))
}
