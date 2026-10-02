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

    /// The in-archive path of one tar entry with the root component stripped,
    /// or `None` for the root entry itself. Shared by every tar entry kind.
    /// Only the unix tar handling needs it (Windows unpacks zips).
    #[cfg(unix)]
    fn tar_rel(clean: &Path) -> Option<PathBuf> {
        let rel: PathBuf = clean.components().skip(1).collect();
        (!rel.as_os_str().is_empty()).then_some(rel)
    }

    /// Lexically resolves `.` / `..` without touching the filesystem, so a
    /// link target can be bounds-checked before it is created (unix tars).
    #[cfg(unix)]
    fn normalize_lexical(path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for c in path.components() {
            match c {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other.as_os_str()),
            }
        }
        out
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
            let entry_type = entry.header().entry_type();
            if entry_type.is_file() {
                // Issue #73: the tar branch must apply the header mode — a
                // plain File::create lands 0644 and `bin/node` then fails
                // execve with EACCES ("校验 Node.js 失败: Permission
                // denied (os error 13)" on macOS/Linux).
                write_entry(node_dir, &clean, false, &mut entry)?;
                #[cfg(unix)]
                if let Some(rel) = tar_rel(&clean) {
                    use std::os::unix::fs::PermissionsExt;
                    // A zero header mode would render the file inaccessible
                    // (0000); fall back to the default instead of honoring it.
                    let mode = entry.header().mode().unwrap_or(0o644) & 0o777;
                    let mode = if mode == 0 { 0o644 } else { mode };
                    let target = node_dir.join(rel);
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
                        .map_err(|e| format!("设置文件权限失败 {}: {e}", target.display()))?;
                }
            } else if entry_type.is_dir() {
                write_entry(node_dir, &clean, true, &mut entry)?;
            } else if entry_type.is_symlink() {
                // Issue #73: node dist tarballs carry bin/npm, bin/npx and
                // bin/corepack as symlinks into lib/; skipping them leaves
                // the runtime without a working npm for the pnpm bootstrap.
                #[cfg(unix)]
                {
                    let Some(rel) = tar_rel(&clean) else {
                        continue;
                    };
                    let target = node_dir.join(&rel);
                    let link = entry
                        .link_name()
                        .map_err(|e| format!("读取符号链接目标失败: {e}"))?;
                    let Some(link) = link else {
                        continue;
                    };
                    // The link target resolves from the symlink's own
                    // directory; it must stay inside node_dir (a dist tar
                    // only ever links within its own tree).
                    let base = target
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| node_dir.to_path_buf());
                    let resolved = normalize_lexical(&base.join(&link));
                    if !resolved.starts_with(node_dir) {
                        crate::log_warn!("跳过逃逸的符号链接: {}", target.display());
                        continue;
                    }
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| format!("创建目录失败: {e}"))?;
                    }
                    if target.symlink_metadata().is_ok() {
                        std::fs::remove_file(&target).ok();
                    }
                    std::os::unix::fs::symlink(&link, &target)
                        .map_err(|e| format!("创建符号链接失败 {}: {e}", target.display()))?;
                }
            } else if entry_type.is_hard_link() {
                #[cfg(unix)]
                {
                    let Some(rel) = tar_rel(&clean) else {
                        continue;
                    };
                    let target = node_dir.join(&rel);
                    let link = entry
                        .link_name()
                        .map_err(|e| format!("读取硬链接目标失败: {e}"))?;
                    let Some(link) = link else {
                        continue;
                    };
                    // A hard link's target is archive-root-relative, so it
                    // goes through the same strip-and-sanitize as entry names.
                    let src_clean: PathBuf = link
                        .components()
                        .filter(|c| matches!(c, std::path::Component::Normal(_)))
                        .collect();
                    let Some(src_rel) = tar_rel(&src_clean) else {
                        continue;
                    };
                    let src = node_dir.join(src_rel);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| format!("创建目录失败: {e}"))?;
                    }
                    // A missing or not-yet-extracted hard-link target must not
                    // abort the whole install (the pre-#73 code skipped hard
                    // links entirely); node dists only ever use symlinks.
                    if let Err(e) = std::fs::hard_link(&src, &target) {
                        crate::log_warn!("跳过失败的硬链接 {}: {e}", target.display());
                    }
                }
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
        dedicated_home_name: None,
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

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

// The change only touches the tar branch, which is dead code on Windows
// (dist archives for Windows are zips), so the tests are unix-only and the
// CI linux/macos legs exercise them.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn unique_temp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dsh-runtime-test-{tag}-{}", uuid::Uuid::new_v4()))
    }

    /// Builds a minimal node-dist-shaped tar.gz: a root directory, one
    /// executable file, one in-tree symlink, one escaping symlink. Written to
    /// a temp file; the caller gets (archive, node_dir, temp_root).
    fn build_dist_tarball(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = unique_temp(tag);
        std::fs::create_dir_all(&root).unwrap();
        let archive = root.join("node.tar.gz");
        let node_dir = root.join("node");

        let mut builder = tar::Builder::new(Vec::new());
        let top = "node-v24.0.0-darwin-arm64";

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_path(format!("{top}/bin")).unwrap();
        // GNU headers are zero-filled; an untouched size field is 12 NUL bytes,
        // which fails the tar crate's strict octal parse on read-back
        // ("numeric field was not a number"). Real dist tarballs always carry
        // an ASCII "0" here, so spell it out for the synthetic entry too.
        header.set_size(0);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, std::io::empty()).unwrap();

        let data = b"#!/bin/sh\necho node\n";
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_path(format!("{top}/bin/node")).unwrap();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, &data[..]).unwrap();

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_path(format!("{top}/bin/npm")).unwrap();
        header
            .set_link_name("../lib/node_modules/npm/bin/npm-cli.js")
            .unwrap();
        header.set_size(0);
        header.set_cksum();
        builder.append(&header, std::io::empty()).unwrap();

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_path(format!("{top}/bin/evil")).unwrap();
        header.set_link_name("../../../../outside").unwrap();
        header.set_size(0);
        header.set_cksum();
        builder.append(&header, std::io::empty()).unwrap();

        let tar_bytes = builder.into_inner().unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &tar_bytes).unwrap();
        std::fs::write(&archive, encoder.finish().unwrap()).unwrap();
        (archive, node_dir, root)
    }

    #[test]
    fn extract_tar_unpacks_regular_files_on_every_platform() {
        let (archive, node_dir, root) = build_dist_tarball("basic");
        extract_node_archive(&archive, &node_dir).unwrap();
        assert_eq!(
            std::fs::read(node_dir.join("bin").join("node")).unwrap(),
            b"#!/bin/sh\necho node\n"
        );
        // The escaping symlink must never appear, on any platform.
        assert!(!node_dir.join("bin").join("evil").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    /// Issue #73: without the header mode the extracted `bin/node` lands 0644
    /// and the post-install verify spawn fails with EACCES on macOS/Linux.
    #[cfg(unix)]
    #[test]
    fn extract_tar_preserves_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let (archive, node_dir, root) = build_dist_tarball("mode");
        extract_node_archive(&archive, &node_dir).unwrap();
        let mode = std::fs::metadata(node_dir.join("bin").join("node"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "executable bit lost: mode {mode:o}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Issue #73: bin/npm / bin/npx / bin/corepack are symlinks in the dist
    /// tarball; dropping them leaves the runtime without a working npm.
    #[cfg(unix)]
    #[test]
    fn extract_tar_recreates_in_tree_symlinks() {
        let (archive, node_dir, root) = build_dist_tarball("links");
        extract_node_archive(&archive, &node_dir).unwrap();
        let link = node_dir.join("bin").join("npm");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            Path::new("../lib/node_modules/npm/bin/npm-cli.js")
        );
        // The escaping symlink is skipped, not created.
        assert!(node_dir
            .join("bin")
            .join("evil")
            .symlink_metadata()
            .is_err());
        std::fs::remove_dir_all(&root).ok();
    }
}
