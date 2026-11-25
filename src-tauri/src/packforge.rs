use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::path::{Component, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};

use crate::{tasks, AppState};

const WORKER: &[u8] = include_bytes!("../packforge/generated/worker.cjs");
const MAX_FRAME: u64 = 12 * 1024 * 1024;
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

#[derive(Deserialize)]
pub(crate) struct Publication {
    pub transaction: String,
    pub baseline: String,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Frame {
    Result {
        value: Value,
    },
    Error {
        message: String,
    },
    Progress {
        message: String,
    },
    Download {
        id: Value,
        url: String,
        path: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(rename = "maxBytes")]
        max_bytes: Option<u64>,
    },
    Publish {
        id: Value,
        #[serde(flatten)]
        publication: Publication,
    },
}

pub(crate) struct Job<'a> {
    pub app: &'a AppHandle,
    pub id: &'a str,
    pub publish: &'a mut (dyn FnMut(Publication) -> PublishResult + Send),
}

pub(crate) type PublishResult = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

struct Workspace {
    fs_path: PathBuf,
    worker_path: String,
}

impl Workspace {
    async fn create(distro: Option<&str>) -> Result<Self, String> {
        let (fs_path, worker_path) = if let Some(distro) = distro {
            let path = crate::wsl::wsl_output(
                distro,
                &[
                    "/usr/bin/mktemp".into(),
                    "-d".into(),
                    "/tmp/dsh-pack-XXXXXXXXXX".into(),
                ],
            )
            .await?;
            if !path.starts_with("/tmp/dsh-pack-") || path[5..].contains('/') {
                return Err("WSL 返回了无效的整合包暂存目录".into());
            }
            (crate::wsl::unc_path(distro, &path), path)
        } else {
            let path = std::env::temp_dir().join(format!("dsh-pack-{}", uuid::Uuid::new_v4()));
            #[cfg(unix)]
            let created = {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new().mode(0o700).create(&path)
            };
            #[cfg(not(unix))]
            let created = std::fs::create_dir(&path);
            created.map_err(|e| format!("创建整合包暂存目录失败：{e}"))?;
            (path.clone(), path.to_string_lossy().into_owned())
        };
        Ok(Self {
            fs_path,
            worker_path,
        })
    }

    fn worker_file(&self, name: &str, wsl: bool) -> String {
        if wsl {
            format!("{}/{name}", self.worker_path)
        } else {
            self.fs_path.join(name).to_string_lossy().into_owned()
        }
    }

    fn download_path(&self, candidate: &str, wsl: bool) -> Result<PathBuf, String> {
        let path = if wsl {
            let relative = candidate
                .strip_prefix(&self.worker_path)
                .and_then(|suffix| suffix.strip_prefix('/'))
                .filter(|relative| !relative.contains('\\'))
                .ok_or("下载目标超出整合包暂存目录")?;
            self.fs_path.join(relative)
        } else {
            PathBuf::from(candidate)
        };
        let relative = path
            .strip_prefix(&self.fs_path)
            .map_err(|_| "下载目标超出整合包暂存目录")?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err("下载目标路径无效".into());
        }
        let parent = path.parent().ok_or("缺少下载目标目录")?;
        let canonical_parent = parent.canonicalize().map_err(|_| "无法解析下载目标目录")?;
        let base = self
            .fs_path
            .canonicalize()
            .map_err(|_| "无法解析整合包暂存目录")?;
        if !canonical_parent.starts_with(base) {
            return Err("下载目标目录被重定向".into());
        }
        Ok(canonical_parent.join(path.file_name().ok_or("下载目标缺少文件名")?))
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.fs_path) {
            if error.kind() != io::ErrorKind::NotFound {
                crate::log_warn!("清理 PackForge 暂存目录失败：{error}");
            }
        }
    }
}

async fn send(input: &mut ChildStdin, value: &Value) -> Result<(), String> {
    let mut data = serde_json::to_vec(value).map_err(|_| "无法编码 PackForge 请求")?;
    if data.len() as u64 >= MAX_FRAME {
        return Err("PackForge 请求过大".into());
    }
    data.push(b'\n');
    input
        .write_all(&data)
        .await
        .map_err(|_| "PackForge 进程已退出".into())
}

async fn wait_cancel(state: &AppState, task_id: Option<&str>) {
    if let Some(task_id) = task_id {
        loop {
            if !state
                .tasks
                .lock()
                .await
                .get(task_id)
                .is_some_and(|task| task.state == tasks::TaskState::Running)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }
    std::future::pending::<()>().await;
}

async fn download(
    workspace: &Workspace,
    distro: Option<&str>,
    url: &str,
    path: &str,
    headers: BTreeMap<String, String>,
    max_bytes: Option<u64>,
) -> Result<(u16, Option<String>), String> {
    let url = reqwest::Url::parse(url).map_err(|_| "下载地址无效")?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("整合包下载地址必须是无内嵌凭据的 HTTP(S) 地址".into());
    }
    let path = workspace.download_path(path, distro.is_some())?;
    let client = crate::proxy::apply(reqwest::Client::builder())?
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(300))
        .user_agent("dsh-launcher-packforge")
        .build()
        .map_err(|_| "无法创建整合包下载连接")?;
    let mut request = client.get(url.clone());
    for (key, value) in headers {
        if key.eq_ignore_ascii_case("authorization") && url.scheme() == "https" {
            request = request.header(reqwest::header::AUTHORIZATION, value);
        }
    }
    let mut response = request
        .send()
        .await
        .map_err(|error| format!("下载整合包资源失败：{}", error.without_url()))?;
    let status = response.status();
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|raw| raw.to_str().ok())
        .map(str::to_owned);
    if !status.is_success() {
        return Ok((status.as_u16(), location));
    }
    let limit = max_bytes.unwrap_or(MAX_DOWNLOAD).min(MAX_DOWNLOAD);
    if response.content_length().is_some_and(|size| size > limit) {
        return Err("整合包资源超过下载大小限制".into());
    }
    let temporary = path.with_file_name(format!(".download-{}", uuid::Uuid::new_v4()));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .await
            .map_err(|_| "无法写入整合包下载暂存文件")?;
        let mut size = 0u64;
        while let Some(chunk) = response.chunk().await.map_err(|_| "整合包资源下载中断")? {
            size += chunk.len() as u64;
            if size > limit {
                return Err("整合包资源超过下载大小限制");
            }
            file.write_all(&chunk)
                .await
                .map_err(|_| "写入整合包下载暂存文件失败")?;
        }
        file.flush()
            .await
            .map_err(|_| "写入整合包下载暂存文件失败")?;
        drop(file);
        tokio::fs::rename(&temporary, &path)
            .await
            .map_err(|_| "发布整合包下载暂存文件失败")
    }
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error.to_owned());
    }
    Ok((status.as_u16(), location))
}

pub(crate) async fn run(
    state: &AppState,
    distro: Option<&str>,
    operation: &str,
    options: Value,
) -> Result<Value, String> {
    run_job(state, distro, operation, options, None).await
}

pub(crate) async fn run_job(
    state: &AppState,
    distro: Option<&str>,
    operation: &str,
    mut options: Value,
    mut job: Option<Job<'_>>,
) -> Result<Value, String> {
    let workspace = Workspace::create(distro).await?;
    tokio::fs::write(workspace.fs_path.join("worker.cjs"), WORKER)
        .await
        .map_err(|_| "无法写入 PackForge 引擎")?;
    options["workspace"] = json!(workspace.worker_path);
    if options["pnpmEnv"].is_null() {
        let mut proxy = Vec::new();
        crate::proxy::override_env(&mut proxy, &state.config.lock().unwrap().settings)?;
        options["pnpmEnv"] = json!(proxy.into_iter().collect::<BTreeMap<_, _>>());
    }
    if let Some(distro) = distro {
        for (key, filename) in [
            ("source", "incoming.pack"),
            ("previousSource", "previous.pack"),
        ] {
            if let Some(path) = options[key].as_str() {
                tokio::fs::copy(path, workspace.fs_path.join(filename))
                    .await
                    .map_err(|_| "无法将已校验的整合包传入 WSL")?;
                options[key] = json!(workspace.worker_file(filename, true));
            }
        }
        // WSL 的包管理与 Git 子进程均从发行版的工具目录启动。
        let root = crate::wsl::WslRoot::resolve(distro).await?;
        options["nodeBin"] = json!(root.node_bin_dir());
    }
    let program = workspace.worker_file("worker.cjs", distro.is_some());
    let mut proc = if let Some(distro) = distro {
        let root = crate::wsl::WslRoot::resolve(distro).await?;
        let executable = if crate::wsl::wsl_test(distro, "-x", &root.node_exe()).await {
            root.node_exe()
        } else {
            "node".to_string()
        };
        crate::wsl::wsl_cmd(
            distro,
            &[
                "/usr/bin/env".into(),
                "-u".into(),
                "NODE_OPTIONS".into(),
                "-u".into(),
                "NODE_PATH".into(),
                executable,
                program,
            ],
        )
    } else {
        let mut proc = Command::new(crate::process::node());
        proc.arg(program)
            .env_remove("NODE_OPTIONS")
            .env_remove("NODE_PATH");
        crate::process::hide_console(&mut proc);
        proc
    };
    let mut child = proc
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "无法启动 PackForge 引擎，请检查 Node.js 是否已安装")?;
    let mut input = child.stdin.take().ok_or("无法向 PackForge 进程发送请求")?;
    let mut output = BufReader::new(child.stdout.take().ok_or("无法读取 PackForge 进程结果")?);
    send(
        &mut input,
        &json!({ "operation": operation, "options": options }),
    )
    .await?;
    let task_id = job.as_ref().map(|job| job.id);
    let mut cancelled = false;
    let mut published = false;
    let mut publication_error = None;
    let mut cancel_deadline = tokio::time::Instant::now() + Duration::from_secs(900);
    let mut bytes = Vec::new();
    loop {
        let mut bounded = (&mut output).take(MAX_FRAME - bytes.len() as u64);
        let count = tokio::select! {
            result = bounded.read_until(b'\n', &mut bytes) => result.map_err(|_| "读取 PackForge 结果失败")?,
            _ = wait_cancel(state, task_id), if !cancelled && !published => {
                cancelled = true;
                cancel_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
                let _ = send(&mut input, &json!({"kind":"cancel"})).await;
                continue;
            }
            _ = tokio::time::sleep_until(cancel_deadline) => {
                let _ = child.kill().await;
                return Err(if cancelled { "整合包任务已取消" } else { "PackForge 操作超时" }.into());
            }
        };
        if count == 0 || bytes.len() as u64 >= MAX_FRAME || !bytes.ends_with(b"\n") {
            return Err("PackForge 进程未返回完整结果".into());
        }
        let frame: Frame =
            serde_json::from_slice(&bytes).map_err(|_| "PackForge 进程返回了无效结果")?;
        bytes.clear();
        match frame {
            Frame::Result { value } => {
                drop(input);
                let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
                    .await
                    .map_err(|_| "等待 PackForge 进程退出超时")?
                    .map_err(|_| "无法确认 PackForge 进程退出状态")?;
                if !status.success() {
                    return Err("PackForge 进程异常退出".into());
                }
                return Ok(value);
            }
            Frame::Error { message } => return Err(publication_error.unwrap_or(message)),
            Frame::Progress { message } => {
                if let Some(job) = job.as_ref() {
                    tasks::push_task_log_pub(job.app, &job.app.state(), job.id, &message).await;
                }
            }
            Frame::Download {
                id,
                url,
                path,
                headers,
                max_bytes,
            } => {
                let pending = download(&workspace, distro, &url, &path, headers, max_bytes);
                let result = if cancelled {
                    Err("整合包任务已取消".into())
                } else {
                    tokio::select! {
                        result = pending => result,
                        _ = wait_cancel(state, task_id), if !published => Err("整合包任务已取消".into()),
                    }
                };
                let reply = match result {
                    Ok((status, location)) => json!({"id":id,"status":status,"location":location}),
                    Err(error) => json!({"id":id,"error":error}),
                };
                send(&mut input, &reply).await?;
            }
            Frame::Publish { id, publication } => {
                let result = if cancelled || publication.transaction.is_empty() {
                    Err("整合包任务已取消或提交记录无效".into())
                } else if let Some(job) = job.as_mut() {
                    (job.publish)(publication).await
                } else {
                    Err("PackForge 提交缺少实例任务".into())
                };
                published = result.is_ok();
                publication_error = result.err();
                send(&mut input, &json!({"id": id, "error":publication_error.as_ref().map(|_| "无法登记整合包实例")})).await?;
            }
        }
    }
}
