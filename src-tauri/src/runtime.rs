use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, State};

use crate::AppState;

#[derive(Clone, Debug, Serialize)]
pub struct ToolStatus {
    pub installed: bool,
    pub version: Option<String>,
    pub path: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeStatus {
    pub node: ToolStatus,
    pub pnpm: ToolStatus,
}

async fn probe(program: &str) -> ToolStatus {
    let mut cmd = tokio::process::Command::new(program);
    crate::process::hide_console(&mut cmd);
    let output = cmd.arg("--version").output().await;

    match output {
        Ok(out) if out.status.success() => ToolStatus {
            installed: true,
            version: Some(String::from_utf8_lossy(&out.stdout).trim().to_string()),
            path: None,
        },
        _ => ToolStatus {
            installed: false,
            version: None,
            path: None,
        },
    }
}

#[tauri::command]
pub async fn get_runtime_status(_state: State<'_, AppState>) -> Result<RuntimeStatus, String> {
    let node = probe("node").await;
    let pnpm = probe("pnpm").await;
    Ok(RuntimeStatus { node, pnpm })
}

// ---------------------------------------------------------------------------
// One-click Node.js runtime (issue #23)
// ---------------------------------------------------------------------------

/// The official dist index; npmmirror as the fallback for restricted
/// networks (the configured launcher proxy applies to both).
const NODE_DIST_PRIMARY: &str = "https://nodejs.org/dist";
const NODE_DIST_MIRROR: &str = "https://registry.npmmirror.com/-/binary/node";

/// The managed runtime lives in `<data>/tools/node`.
fn local_node_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("tools").join("node")
}

fn local_node_exe(node_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        node_dir.join("node.exe")
    } else {
        node_dir.join("bin").join("node")
    }
}

/// Directory containing node/npm/npx shims for PATH purposes (on Windows
/// everything sits at the root of the unpacked archive).
fn local_node_bin_dir(node_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        node_dir.to_path_buf()
    } else {
        node_dir.join("bin")
    }
}

/// Appends the managed Node.js bin dir to this process's PATH when a local
/// runtime exists. Appending (not prepending) keeps a system Node preferred;
/// child processes (CLI / npm / pnpm / instances) inherit the value. Called
/// once at startup and after a one-click install.
pub fn ensure_local_node_on_path(data_dir: &Path) {
    let node_dir = local_node_dir(data_dir);
    if !local_node_exe(&node_dir).is_file() {
        return;
    }
    let bin = local_node_bin_dir(&node_dir);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut parts: Vec<PathBuf> = std::env::split_paths(&path).collect();
    if parts.contains(&bin) {
        return;
    }
    parts.push(bin.clone());
    if let Ok(joined) = std::env::join_paths(parts) {
        std::env::set_var("PATH", joined);
        crate::log_info!("已将内置 Node.js 加入 PATH: {}", bin.display());
    }
}

/// dist archive file name for this platform.
fn node_archive_name(version: &str) -> String {
    #[cfg(windows)]
    {
        format!("node-{version}-win-x64.zip")
    }
    #[cfg(target_os = "macos")]
    {
        format!("node-{version}-darwin-arm64.tar.gz")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        format!("node-{version}-linux-x64.tar.gz")
    }
}

/// dist archive file name for a WSL (linux-x64) target (issue #19).
pub(crate) fn node_archive_name_linux(version: &str) -> String {
    format!("node-{version}-linux-x64.tar.gz")
}

/// Latest LTS version (e.g. `v22.14.0`) from the dist index, primary source
/// first with the mirror as fallback.
pub(crate) async fn resolve_node_version() -> Result<String, String> {
    for base in [NODE_DIST_PRIMARY, NODE_DIST_MIRROR] {
        match crate::plugins::fetch_json_pub(&format!("{base}/index.json"), 8 * 1024 * 1024).await {
            Ok(doc) => {
                if let Some(arr) = doc.as_array() {
                    for rel in arr {
                        let is_lts = rel
                            .get("lts")
                            .map(|l| l.is_string() || l.as_bool().unwrap_or(false))
                            .unwrap_or(false);
                        if !is_lts {
                            continue;
                        }
                        if let Some(v) = rel.get("version").and_then(|v| v.as_str()) {
                            return Ok(v.to_string());
                        }
                    }
                }
                crate::log_warn!("Node.js 版本列表格式异常（{base}），尝试镜像");
            }
            Err(e) => crate::log_warn!("获取 Node.js 版本列表失败（{base}）: {e}，尝试镜像"),
        }
    }
    Err("获取 Node.js 版本列表失败（官方源与镜像均不可用）".to_string())
}

/// Downloads the dist archive `name` for `version` to `dest`, streaming
/// progress into the task (5% → 80%). Falls back to the mirror on failure.
pub(crate) async fn download_node_archive(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    version: &str,
    name: &str,
    dest: &Path,
) -> Result<(), String> {
    let client = crate::proxy::apply(reqwest::Client::builder())
        .timeout(std::time::Duration::from_secs(900))
        .user_agent("dsh-launcher")
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    let mut last_err = String::new();
    for base in [NODE_DIST_PRIMARY, NODE_DIST_MIRROR] {
        let url = format!("{base}/{version}/{name}");
        crate::tasks::push_task_log_pub(app, state, task_id, &format!("下载 {url}")).await;
        match download_one(&client, &url, dest, app, task_id).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                crate::log_warn!("Node.js 下载失败（{base}）: {e}");
                std::fs::remove_file(dest).ok();
                last_err = e;
            }
        }
    }
    Err(format!("下载 Node.js 失败: {last_err}"))
}

async fn download_one(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    app: &AppHandle,
    task_id: &str,
) -> Result<(), String> {
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(dest).map_err(|e| format!("创建下载文件失败: {e}"))?;
    let mut done: u64 = 0;
    let mut last_pct = 5u32;
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("下载中断: {e}"))? {
        std::io::Write::write_all(&mut file, &chunk)
            .map_err(|e| format!("写入下载文件失败: {e}"))?;
        done += chunk.len() as u64;
        if let Some(ratio) = done.saturating_mul(75).checked_div(total) {
            let pct = 5 + ratio as u32;
            if pct > last_pct {
                last_pct = pct;
                let shown = done.saturating_mul(100).checked_div(total).unwrap_or(0);
                crate::tasks::emit_progress_pub(
                    app,
                    task_id,
                    crate::tasks::TaskState::Running,
                    pct,
                    Some(format!("正在下载 Node.js（{shown}%）")),
                    None,
                );
            }
        }
    }
    Ok(())
}

/// Extracts the dist archive into `node_dir`, stripping the top-level
/// `node-vX-…/` component. Any previous managed runtime is replaced.
fn extract_node_archive(archive: &Path, node_dir: &Path) -> Result<(), String> {
    if node_dir.exists() {
        std::fs::remove_dir_all(node_dir).map_err(|e| format!("清理旧 Node.js 目录失败: {e}"))?;
    }
    std::fs::create_dir_all(node_dir).map_err(|e| format!("创建 Node.js 目录失败: {e}"))?;

    /// Shared per-entry write with the root component stripped.
    fn write_entry(
        node_dir: &Path,
        raw: &Path,
        is_dir: bool,
        reader: &mut dyn std::io::Read,
    ) -> Result<(), String> {
        let rel: PathBuf = raw.components().skip(1).collect();
        if rel.as_os_str().is_empty() {
            return Ok(());
        }
        let target = node_dir.join(rel);
        if is_dir {
            std::fs::create_dir_all(&target).ok();
            return Ok(());
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
        std::io::copy(
            reader,
            &mut std::fs::File::create(&target).map_err(|e| format!("创建文件失败: {e}"))?,
        )
        .map_err(|e| format!("解压条目失败: {e}"))?;
        Ok(())
    }

    if cfg!(windows) {
        let file = std::fs::File::open(archive).map_err(|e| format!("打开安装包失败: {e}"))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("解析安装包失败: {e}"))?;
        for i in 0..zip.len() {
            let mut entry = zip
                .by_index(i)
                .map_err(|e| format!("读取安装包条目失败: {e}"))?;
            let Some(name) = entry.enclosed_name().map(|p| p.to_path_buf()) else {
                continue;
            };
            write_entry(node_dir, &name, entry.is_dir(), &mut entry)?;
        }
    } else {
        let file = std::fs::File::open(archive).map_err(|e| format!("打开安装包失败: {e}"))?;
        let gz = flate2::read::GzDecoder::new(file);
        let mut tar = tar::Archive::new(gz);
        for entry in tar.entries().map_err(|e| format!("读取安装包失败: {e}"))? {
            let mut entry = entry.map_err(|e| format!("读取安装包条目失败: {e}"))?;
            let path = entry
                .path()
                .map_err(|e| format!("读取安装包条目名失败: {e}"))?
                .into_owned();
            let clean: PathBuf = path
                .components()
                .filter(|c| matches!(c, std::path::Component::Normal(_)))
                .collect();
            let is_dir = entry.header().entry_type().is_dir();
            if entry.header().entry_type().is_file() || is_dir {
                write_entry(node_dir, &clean, is_dir, &mut entry)?;
            }
        }
    }
    Ok(())
}

/// Starts the one-click Node.js install as a background task (issue #23):
/// downloads the latest LTS from the official dist (mirror fallback),
/// unpacks it into `<data>/tools/node`, puts it on PATH, then bootstraps the
/// pinned pnpm through the bundled npm.
#[tauri::command]
pub async fn start_install_node_task(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if probe("node").await.installed {
        return Err("系统已安装 Node.js".to_string());
    }
    {
        let tasks = state.tasks.lock().await;
        if tasks.values().any(|t| {
            t.kind == "install-node"
                && matches!(
                    t.state,
                    crate::tasks::TaskState::Running | crate::tasks::TaskState::Queued
                )
        }) {
            return Err("Node.js 安装任务已在进行".to_string());
        }
    }

    let task = crate::tasks::TaskInfo {
        id: crate::config::new_id("t"),
        kind: "install-node".to_string(),
        label: "一键安装 Node.js 运行时".to_string(),
        version: String::new(),
        state: crate::tasks::TaskState::Running,
        percent: 0,
        created_at: crate::tasks::now_millis_pub(),
        message: None,
        instance_id: None,
        instance_name: None,
        reserved_home_path: None,
        logs: Vec::new(),
        child: None,
    };
    let task_id = task.id.clone();
    state.tasks.lock().await.insert(task_id.clone(), task);
    crate::tasks::emit_progress_pub(
        &app,
        &task_id,
        crate::tasks::TaskState::Running,
        0,
        None,
        None,
    );

    let worker_app = app.clone();
    let worker_task_id = task_id.clone();
    tauri::async_runtime::spawn(async move {
        let state = worker_app.state::<AppState>();
        let result = do_install_node(&worker_app, &state, &worker_task_id).await;
        let mut tasks = state.tasks.lock().await;
        if let Some(task) = tasks.get_mut(&worker_task_id) {
            match result {
                Ok(version) => {
                    task.state = crate::tasks::TaskState::Done;
                    task.percent = 100;
                    task.message = Some(format!("Node.js {version} 已就绪"));
                    crate::tasks::emit_progress_pub(
                        &worker_app,
                        &worker_task_id,
                        crate::tasks::TaskState::Done,
                        100,
                        Some(format!("Node.js {version} 已就绪")),
                        None,
                    );
                }
                Err(msg) => {
                    task.state = crate::tasks::TaskState::Error;
                    task.message = Some(msg.clone());
                    crate::tasks::push_log_locked_pub(task, &format!("error: {msg}"));
                    let pct = task.percent;
                    drop(tasks);
                    crate::tasks::emit_progress_pub(
                        &worker_app,
                        &worker_task_id,
                        crate::tasks::TaskState::Error,
                        pct,
                        Some(msg),
                        None,
                    );
                }
            }
        }
    });

    Ok(task_id)
}

async fn do_install_node(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
) -> Result<String, String> {
    crate::tasks::push_task_log_pub(app, state, task_id, "正在查询 Node.js 最新 LTS 版本…").await;
    let version = resolve_node_version().await?;
    {
        let mut tasks = state.tasks.lock().await;
        if let Some(task) = tasks.get_mut(task_id) {
            task.version = version.clone();
        }
    }
    crate::tasks::push_task_log_pub(app, state, task_id, &format!("目标版本: {version}")).await;

    let tools = state.data_dir.join("tools");
    std::fs::create_dir_all(&tools).map_err(|e| format!("创建工具目录失败: {e}"))?;
    let archive = tools.join(node_archive_name(&version));
    let name = node_archive_name(&version);
    download_node_archive(app, state, task_id, &version, &name, &archive).await?;

    crate::tasks::emit_progress_pub(
        app,
        task_id,
        crate::tasks::TaskState::Running,
        85,
        Some("正在解压 Node.js…".to_string()),
        None,
    );
    let node_dir = local_node_dir(&state.data_dir);
    extract_node_archive(&archive, &node_dir)?;
    std::fs::remove_file(&archive).ok();

    // Put the managed runtime on PATH for everything we spawn from here on.
    ensure_local_node_on_path(&state.data_dir);

    // Verify the fresh runtime runs.
    let exe = local_node_exe(&node_dir);
    let mut verify = tokio::process::Command::new(&exe);
    crate::process::hide_console(&mut verify);
    let out = verify
        .arg("--version")
        .output()
        .await
        .map_err(|e| format!("校验 Node.js 失败: {e}"))?;
    if !out.status.success() {
        return Err("Node.js 安装后无法运行".to_string());
    }
    let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
    crate::tasks::push_task_log_pub(app, state, task_id, &format!("Node.js {got} 安装完成")).await;

    // npm ships with Node; bootstrap the pinned pnpm so the whole
    // environment goes green in one click.
    crate::tasks::emit_progress_pub(
        app,
        task_id,
        crate::tasks::TaskState::Running,
        92,
        Some("正在安装 pnpm…".to_string()),
        None,
    );
    crate::tasks::ensure_pnpm_pub(app, state, task_id).await?;
    crate::tasks::push_task_log_pub(app, state, task_id, "pnpm 已就绪").await;
    Ok(got)
}
