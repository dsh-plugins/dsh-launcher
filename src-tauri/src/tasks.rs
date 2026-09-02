use crate::config::{new_id, DshInstance, DshVersion};
use crate::AppState;
use serde::Serialize;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;

pub const TASK_PROGRESS_EVENT: &str = "task://progress";
pub const TASK_LOG_EVENT: &str = "task://log";

const MAX_LOG_LINES: usize = 1000;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    /// Waiting for a serialized resource (currently: another plugin operation
    /// on the same profile). Not yet doing any work.
    Queued,
    Running,
    Done,
    Error,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
pub struct TaskInfo {
    pub id: String,
    pub kind: String, // "create-instance"
    pub label: String,
    pub version: String,
    pub state: TaskState,
    pub percent: u32,
    pub created_at: i64,
    pub message: Option<String>,
    pub instance_id: Option<String>,
    pub instance_name: Option<String>,
    /// Reserved dedicated HOME path while the task is running; the actual
    /// HOME record is only created when the instance is created, so a
    /// cancelled/failed task never leaves an orphan HOME. Not serialized.
    #[serde(skip)]
    pub reserved_home_path: Option<std::path::PathBuf>,
    pub logs: Vec<String>,
    #[serde(skip)]
    pub child: Option<Arc<Mutex<Option<tokio::process::Child>>>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TaskProgress {
    pub id: String,
    pub state: TaskState,
    pub percent: u32,
    pub message: Option<String>,
    pub instance_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TaskLog {
    pub id: String,
    pub line: String,
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Enqueues a background task that installs the given DSH version (if not
/// installed yet) and then creates the instance. Returns the task id.
#[tauri::command(rename_all = "snake_case")]
pub async fn start_create_instance_task(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    version: String,
    home_id: Option<String>,
    dedicated: bool,
) -> Result<String, String> {
    let name = name.trim().to_string();
    let version = version.trim().to_string();
    if name.is_empty() {
        return Err("实例名称不能为空".to_string());
    }
    if version.is_empty() {
        return Err("版本号不能为空".to_string());
    }

    // Dedicated HOME: reserve the path now (placeholder) but do NOT create the
    // HOME record yet — it is created only once the instance is actually made,
    // so a failed/cancelled task leaves no orphan HOME behind.
    let reserved_home_path: Option<std::path::PathBuf> = if dedicated {
        let path = state
            .data_dir
            .join("homes")
            .join(crate::config::sanitize_name(&name));
        Some(path)
    } else {
        None
    };

    // Validate early so a doomed task is never enqueued.
    {
        let cfg = state.config.lock().unwrap();
        if cfg.instances.iter().any(|i| i.name == name) {
            return Err("同名实例已存在".to_string());
        }
        // For a non-dedicated task the chosen HOME must exist already.
        if !dedicated {
            if let Some(hid) = &home_id {
                if !cfg.homes.iter().any(|h| h.id == *hid) {
                    return Err("DSH_HOME 不存在".to_string());
                }
            }
        }
    }
    // Reject a running/pending task that will create the same instance name
    // once it finishes (prevents duplicate name submissions).
    // Also reject two running tasks reserving the same dedicated HOME path.
    {
        let tasks = state.tasks.lock().await;
        for task in tasks.values() {
            if task.state == TaskState::Running {
                if task.instance_name.as_deref() == Some(name.as_str()) {
                    return Err("同名实例的下载任务已在进行中".to_string());
                }
                if let (Some(a), Some(b)) = (&task.reserved_home_path, &reserved_home_path) {
                    if crate::config::paths_equal(a, b) {
                        return Err("该专属 DSH_HOME 已被其他下载任务占用".to_string());
                    }
                }
            }
        }
    }

    let task = TaskInfo {
        id: new_id("t"),
        kind: "create-instance".to_string(),
        label: format!("下载 DSH {version} 并创建实例「{name}」"),
        version: version.clone(),
        state: TaskState::Running,
        percent: 0,
        created_at: now_millis(),
        message: None,
        instance_id: None,
        instance_name: Some(name.clone()),
        reserved_home_path,
        logs: Vec::new(),
        child: None,
    };
    let task_id = task.id.clone();
    state.tasks.lock().await.insert(task_id.clone(), task);
    emit_progress(&app, &task_id, TaskState::Running, 0, None, None);

    let worker_app = app.clone();
    let worker_task_id = task_id.clone();
    tauri::async_runtime::spawn(async move {
        let state = worker_app.state::<AppState>();
        run_create_instance_task(
            &worker_app,
            &state,
            &worker_task_id,
            &name,
            &version,
            &home_id,
        )
        .await;
    });

    Ok(task_id)
}

#[tauri::command]
pub async fn list_tasks(state: State<'_, AppState>) -> Result<Vec<TaskInfo>, String> {
    let tasks = state.tasks.lock().await;
    let mut out: Vec<TaskInfo> = tasks.values().cloned().collect();
    out.sort_by_key(|t| std::cmp::Reverse(t.created_at));
    Ok(out)
}

#[tauri::command]
pub async fn remove_task(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let mut tasks = state.tasks.lock().await;
    let Some(task) = tasks.get(&id) else {
        return Err("任务不存在".to_string());
    };
    if task.state == TaskState::Running || task.state == TaskState::Queued {
        return Err("任务仍在运行或排队中，请先取消".to_string());
    }
    tasks.remove(&id);
    Ok(())
}

#[tauri::command]
pub async fn cancel_task(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let child = {
        let tasks = state.tasks.lock().await;
        tasks.get(&id).and_then(|t| t.child.clone())
    };
    {
        let mut tasks = state.tasks.lock().await;
        if let Some(task) = tasks.get_mut(&id) {
            task.state = TaskState::Cancelled;
            task.message = Some("已取消".to_string());
        }
    }
    if let Some(child) = child {
        let taken = child.lock().await.take();
        if let Some(mut c) = taken {
            let _ = c.kill().await;
        }
    }
    emit_progress(
        &app,
        &id,
        TaskState::Cancelled,
        0,
        Some("已取消".to_string()),
        None,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

async fn run_create_instance_task(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    name: &str,
    version: &str,
    home_id: &Option<String>,
) {
    // The dedicated HOME path is read from the task's reservation; only then
    // is the actual HOME record created (inside do_create_instance).
    let reserved = {
        let tasks = state.tasks.lock().await;
        tasks
            .get(task_id)
            .and_then(|t| t.reserved_home_path.clone())
    };
    let result = do_create_instance(
        app,
        state,
        task_id,
        name,
        version,
        home_id,
        reserved.as_deref(),
    )
    .await;

    let mut tasks = state.tasks.lock().await;
    if let Some(task) = tasks.get_mut(task_id) {
        if task.state == TaskState::Cancelled {
            return;
        }
        match result {
            Ok(instance_id) => {
                task.state = TaskState::Done;
                task.percent = 100;
                task.instance_id = Some(instance_id.clone());
                crate::log_info!("任务 {task_id} 完成，实例 {instance_id} 已创建");
                // The dedicated HOME now exists for real; release the placeholder.
                task.reserved_home_path = None;
                emit_progress(app, task_id, TaskState::Done, 100, None, Some(instance_id));
            }
            Err(msg) => {
                task.state = TaskState::Error;
                task.message = Some(msg.clone());
                crate::log_error!("任务 {task_id} 失败：{msg}");
                push_log_locked(task, &format!("error: {msg}"));
                emit_progress(
                    app,
                    task_id,
                    TaskState::Error,
                    task.percent,
                    Some(msg),
                    None,
                );
            }
        }
    }
}

async fn do_create_instance(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    name: &str,
    version: &str,
    home_id: &Option<String>,
    reserved_home_path: Option<&std::path::Path>,
) -> Result<String, String> {
    // 1. Install the version if missing.
    let version_record = {
        let cfg = state.config.lock().unwrap();
        cfg.versions.iter().find(|v| v.version == version).cloned()
    };
    let version_record = match version_record {
        Some(v) => v,
        None => install_version_streamed(app, state, task_id, version).await?,
    };

    // 2. Resolve the actual DSH_HOME: for a dedicated task, create the HOME
    //    record now (path-based reuse keeps it idempotent); otherwise the
    //    caller-provided HOME id must already exist.
    let resolved_home_id = match home_id {
        Some(hid) => hid.clone(),
        None => {
            let path = reserved_home_path
                .ok_or_else(|| "缺少专属 DSH_HOME 路径".to_string())?
                .to_string_lossy()
                .to_string();
            crate::commands::create_home_record(state, name, &path)?.id
        }
    };
    let home_path = {
        let cfg = state.config.lock().unwrap();
        cfg.homes
            .iter()
            .find(|h| h.id == resolved_home_id)
            .ok_or_else(|| "DSH_HOME 不存在".to_string())?
            .path
            .clone()
    };

    // 2.5. Ensure the default web profile exists and a `__temp__` template
    // copy is created, so later profiles can be derived from it.
    ensure_web_profile_template(app, state, task_id, &home_path, &version_record).await?;

    // 3. Create the instance record.
    let inst = {
        let mut cfg = state.config.lock().unwrap();
        if cfg.instances.iter().any(|i| i.name == name) {
            return Err("同名实例已存在".to_string());
        }
        if !cfg.homes.iter().any(|h| h.id == resolved_home_id) {
            return Err("DSH_HOME 不存在".to_string());
        }
        let inst = DshInstance {
            id: new_id("i"),
            name: name.to_string(),
            version_id: version_record.id.clone(),
            home_id: resolved_home_id,
            env_overrides: Default::default(),
            default_profile: None,
            last_profile: None,
            icon: None,

            port: None,
        };
        cfg.instances.push(inst.clone());
        crate::commands::save_state(state, &cfg)?;
        inst
    };
    Ok(inst.id)
}

/// Ensures the default `web` profile exists in the given DSH_HOME and that a
/// `__temp__` copy (the template later profiles are derived from) is present.
/// If the template is missing, it boots the installed DSH with
/// `--profile web --port <random>`, waits for the web URL (meaning the profile
/// was materialized), terminates it, then copies `profiles/web` to
/// `profiles/__temp__`. The profile for a fresh HOME is created the first time
/// a DSH process runs with that HOME, so this is a one-time cost per HOME.
async fn ensure_web_profile_template(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    home_path: &std::path::Path,
    version: &DshVersion,
) -> Result<(), String> {
    let profiles = home_path.join("profiles");
    let temp_dir = profiles.join("__temp__");
    if temp_dir.exists() {
        return Ok(());
    }

    let bin = crate::process::version_bin(&version.dir);
    if !crate::process::version_bin_ready(&version.dir) {
        return Err(format!(
            "版本 {} 安装不完整（缺少 {}）",
            version.version,
            bin.display()
        ));
    }

    let port = 20000 + rand_port_offset();
    let msg = format!("正在初始化 web profile（端口 {port}）…");
    push_task_log(app, state, task_id, &msg).await;

    let mut child = crate::process::hide_console(
        tokio::process::Command::new(crate::process::node())
            .arg(&bin)
            .arg("--profile")
            .arg("web")
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .env("DSH_HOME", home_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .spawn()
    .map_err(|e| format!("启动 DSH 生成 profile 失败: {e}"))?;

    // Wait for the web URL to appear (profile has been created), then stop it.
    let mut timer = tokio::time::interval(std::time::Duration::from_millis(300));
    let mut attempts = 0;
    let mut ready = false;
    if let Some(out) = child.stdout.take() {
        let mut reader = BufReader::new(out).lines();
        loop {
            tokio::select! {
                line = reader.next_line() => {
                    match line {
                        Ok(Some(l)) => {
                            let l = l.trim().to_string();
                            if !l.is_empty() {
                                push_task_log(app, state, task_id, &l).await;
                            }
                            if l.contains("dsh web: http") {
                                ready = true;
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                _ = timer.tick() => {
                    attempts += 1;
                    if attempts > 200 { break; } // ~60s safety cap
                }
            }
        }
    }

    // Take ownership of the child handle to kill it (we already removed stdout).
    let _ = child.stderr.take();
    child.kill().await.ok();

    if !ready {
        return Err("生成 web profile 超时或失败".to_string());
    }

    // Copy profiles/web → profiles/__temp__.
    let web_dir = profiles.join("web");
    if !web_dir.exists() {
        return Err("web profile 目录未生成".to_string());
    }
    copy_dir(&web_dir, &temp_dir).map_err(|e| format!("复制 __temp__ profile 失败: {e}"))?;
    push_task_log(app, state, task_id, "web profile 模板 __temp__ 已创建").await;
    Ok(())
}

/// Simple deterministic-ish port offset so multiple homes don't collide often.
fn rand_port_offset() -> u16 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos % 30000) as u16
}

async fn push_task_log(app: &AppHandle, state: &State<'_, AppState>, task_id: &str, line: &str) {
    let mut tasks = state.tasks.lock().await;
    if let Some(task) = tasks.get_mut(task_id) {
        // Cap the retained log (mirrors stream_pipe's MAX_LOG_LINES).
        if task.logs.len() >= MAX_LOG_LINES {
            task.logs.remove(0);
        }
        task.logs.push(line.to_string());
    }
    emit_log(app, task_id, line);
}

/// Recursively copies a directory tree (files only, directories preserved).
fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&from, &to)?;
        } else if ty.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Runs `pnpm install --loglevel=http` for the given version, streaming every
/// output line into the task log (and as events). The pnpm content store is
/// placed under the app data dir (`.pnpm-store`) so versions are installed
/// into the launcher's own storage. Returns the new version record on success.
async fn install_version_streamed(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    version: &str,
) -> Result<DshVersion, String> {
    // Alpha builds ship only as GitHub `dsh-v*` tags, never to npm; route
    // those to the from-source pipeline before touching the version dir.
    if !npm_has_version(version).await {
        push_task_log(
            app,
            state,
            task_id,
            &format!("@deepseek-ai/dsh@{version} 未发布到 npm，检查 GitHub 标签 dsh-v{version}…"),
        )
        .await;
        if !github_tag_exists(version).await? {
            return Err(format!(
                "版本 {version} 既未发布到 npm，也不存在 GitHub 标签 dsh-v{version}"
            ));
        }
        return install_version_from_repo(app, state, task_id, version).await;
    }

    let dir = state.data_dir.join("versions").join(version);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建版本目录失败: {e}"))?;
    let store_dir = state.data_dir.join(".pnpm-store");

    // pnpm (>=10) ignores dependency build scripts by default, which would
    // skip native modules like node-pty / koffi. A workspace manifest inside
    // the install dir opts back into running all build scripts; on pnpm 11
    // this also carries the `allowBuilds` section (see
    // crate::plugins::ensure_build_scripts_allowed).
    crate::plugins::ensure_build_scripts_allowed(&dir)?;

    // Make sure a pnpm executable is available before installing: use the
    // system one if present, otherwise bootstrap the latest pnpm into the
    // launcher's data dir via npm.
    let pnpm_prog = ensure_pnpm(app, state, task_id).await?;

    // pnpm 11 blocks dependency build scripts unless every package with an
    // install script is listed under `allowBuilds`. On the first attempt it
    // writes a `set this to true or false` placeholder into
    // pnpm-workspace.yaml and fails with ERR_PNPM_IGNORED_BUILDS; we convert
    // that placeholder to `true` and retry once so native deps (node-pty,
    // koffi, …) actually build.
    for attempt in 1..=2 {
        let mut cmd = tokio::process::Command::new(&pnpm_prog);
        crate::process::hide_console(&mut cmd);
        // Network robustness: the default fetch timeout (60s) and retries (2)
        // are too tight for large native binaries (e.g. sharp-win32-x64),
        // which fail with "error (23) ... aborted due to timeout" on flaky
        // connections.
        cmd.args(["install", "--prefix"])
            .arg(&dir)
            .arg("--store-dir")
            .arg(&store_dir)
            .args(["--loglevel=http"])
            .args([
                "--fetch-timeout",
                "300000", // 5 min per request
                "--fetch-retries",
                "5",
                "--fetch-retry-maxtimeout",
                "120000",
                "--network-concurrency",
                "4",
            ]);
        // Optional npm registry mirror (e.g. npmmirror) via DSH_NPM_REGISTRY.
        if let Ok(registry) = std::env::var("DSH_NPM_REGISTRY") {
            let registry = registry.trim().to_string();
            if !registry.is_empty() {
                cmd.args(["--registry", &registry]);
            }
        }
        cmd.arg(format!("@deepseek-ai/dsh@{version}"));
        // No TTY under the launcher: keep pnpm non-interactive so a modules
        // purge (store relink) never aborts with
        // ERR_PNPM_ABORTED_REMOVE_MODULES_DIR_NO_TTY.
        cmd.env("CI", "true");

        match run_streamed_command(app, state, task_id, cmd, "pnpm install").await {
            Ok(()) => break,
            Err(_e) if attempt == 1 && task_log_mentions_ignored_builds(state, task_id) => {
                push_task_log(
                    app,
                    state,
                    task_id,
                    "pnpm 11 拦截了构建脚本，正在批准 allowBuilds 后重试…",
                )
                .await;
                crate::plugins::ensure_build_scripts_allowed(&dir)?;
            }
            Err(e) => return Err(e),
        }
    }

    register_version(state, version, dir)
}

/// Records an installed version in the config (idempotent by version
/// string).
fn register_version(
    state: &State<'_, AppState>,
    version: &str,
    dir: std::path::PathBuf,
) -> Result<DshVersion, String> {
    let record = DshVersion {
        id: new_id("v"),
        version: version.to_string(),
        dir,
    };
    let mut cfg = state.config.lock().unwrap();
    if let Some(existing) = cfg.versions.iter().find(|v| v.version == *version) {
        return Ok(existing.clone());
    }
    cfg.versions.push(record.clone());
    crate::commands::save_state(state, &cfg)?;
    Ok(record)
}

/// Whether `@deepseek-ai/dsh@<version>` exists on the npm registry. Network
/// or npm failures keep the classic npm path so its own error surfaces.
async fn npm_has_version(version: &str) -> bool {
    let mut cmd = tokio::process::Command::new(crate::process::npm());
    crate::process::hide_console(&mut cmd);
    cmd.args(["view", &format!("@deepseek-ai/dsh@{version}"), "version"]);
    if let Ok(registry) = std::env::var("DSH_NPM_REGISTRY") {
        let registry = registry.trim().to_string();
        if !registry.is_empty() {
            cmd.args(["--registry", &registry]);
        }
    }
    // An unknown version exits 0 with empty output.
    match cmd.output().await {
        Ok(out) => out.status.success() && !String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        Err(_) => true,
    }
}

/// Whether the upstream repo carries the `dsh-v<version>` tag (GitHub-only
/// alpha builds).
async fn github_tag_exists(version: &str) -> Result<bool, String> {
    let url = crate::plugins::github_api_url(&format!(
        "/repos/{}/git/ref/tags/dsh-v{version}",
        crate::commands::DSH_REPO
    ));
    match crate::plugins::fetch_json_pub(&url, 256 * 1024).await {
        Ok(_) => Ok(true),
        Err(e) if e.contains("HTTP 404") => Ok(false),
        Err(e) => Err(e),
    }
}

/// Installs a GitHub-only version (a `dsh-v<ver>` tag never published to
/// npm) from source, following the upstream README "Run from source" flow:
/// clone the tag → `pnpm install` → `pnpm run build`. The version directory
/// IS the checkout, so the CLI entry is `apps/cli/lib/bin.js` (see
/// `crate::process::version_bin`). This takes much longer than an npm
/// install: a full monorepo dependency install plus a full build.
async fn install_version_from_repo(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    version: &str,
) -> Result<DshVersion, String> {
    let dir = state.data_dir.join("versions").join(version);
    let tag = format!("dsh-v{version}");
    let repo_url = format!("https://github.com/{}.git", crate::commands::DSH_REPO);
    let store_dir = state.data_dir.join(".pnpm-store");

    // 1. Clone the tag. A kept checkout is reused as-is (tags are immutable);
    //    any other leftover directory is a failed attempt and gets cleared.
    if !dir.join("apps").join("cli").join("package.json").exists() {
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| format!("清理残留源码目录失败: {e}"))?;
        }
        push_task_log(
            app,
            state,
            task_id,
            &format!("该版本仅发布在 GitHub，正在克隆 {tag} 源码（浅克隆）…"),
        )
        .await;
        let mut cmd = tokio::process::Command::new("git");
        crate::process::hide_console(&mut cmd);
        cmd.args(["clone", "--depth", "1", "--branch", &tag, &repo_url])
            .arg(&dir)
            // Never prompt for credentials on a public repo.
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("CI", "true");
        run_streamed_command(app, state, task_id, cmd, "git clone")
            .await
            .map_err(|e| {
                if e.contains("程序") || e.contains("program") || e.contains("not found") {
                    format!("源码构建需要安装 Git：{e}")
                } else {
                    e
                }
            })?;
    } else {
        push_task_log(app, state, task_id, "复用已克隆的源码目录").await;
    }

    let pnpm_prog = ensure_pnpm(app, state, task_id).await?;

    // 2. Dependencies. The checkout manages its own workspace manifest
    //    (including build-script policy), so the launcher's allowBuilds
    //    workaround does not apply here.
    push_task_log(
        app,
        state,
        task_id,
        "安装依赖（pnpm install --frozen-lockfile），首次可能需要几分钟…",
    )
    .await;
    let mut cmd = tokio::process::Command::new(&pnpm_prog);
    crate::process::hide_console(&mut cmd);
    cmd.current_dir(&dir)
        .args(["install", "--frozen-lockfile"])
        .arg("--store-dir")
        .arg(&store_dir)
        .args(["--loglevel=http"])
        .args([
            "--fetch-timeout",
            "300000",
            "--fetch-retries",
            "5",
            "--fetch-retry-maxtimeout",
            "120000",
            "--network-concurrency",
            "4",
        ]);
    if let Ok(registry) = std::env::var("DSH_NPM_REGISTRY") {
        let registry = registry.trim().to_string();
        if !registry.is_empty() {
            cmd.args(["--registry", &registry]);
        }
    }
    cmd.env("CI", "true");
    run_streamed_command(app, state, task_id, cmd, "pnpm install（源码）").await?;

    // 3. Build. Per the upstream README, `pnpm run build` prepares every
    //    repository artifact; `pnpm dsh web` then runs without rebuilding.
    push_task_log(app, state, task_id, "构建（pnpm run build）…").await;
    let mut cmd = tokio::process::Command::new(&pnpm_prog);
    crate::process::hide_console(&mut cmd);
    cmd.current_dir(&dir)
        .args(["run", "build"])
        .env("CI", "true");
    run_streamed_command(app, state, task_id, cmd, "pnpm run build").await?;

    // 4. Verify and register.
    if !crate::process::version_bin_ready(&dir) {
        return Err(format!(
            "构建完成后未找到 CLI 入口 {}，请查看任务日志中的构建输出",
            crate::process::version_bin(&dir).display()
        ));
    }
    register_version(state, version, dir)
}

/// Whether the task's streamed log mentions pnpm's ignored-build-scripts
/// failure (ERR_PNPM_IGNORED_BUILDS / "Ignored build scripts").
fn task_log_mentions_ignored_builds(state: &State<'_, AppState>, task_id: &str) -> bool {
    let tasks = state.tasks.try_lock().map(|t| t.clone()).ok();
    tasks
        .and_then(|t| t.get(task_id).map(|t| t.logs.clone()))
        .map(|logs| {
            logs.iter().any(|l| {
                l.contains("ERR_PNPM_IGNORED_BUILDS") || l.contains("Ignored build scripts")
            })
        })
        .unwrap_or(false)
}

/// `pub(crate)` so modpack imports (issue #5) can install the pinned
/// `dshVersion` the same way instance creation does.
pub(crate) async fn install_version_streamed_pub(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    version: &str,
) -> Result<DshVersion, String> {
    install_version_streamed(app, state, task_id, version).await
}

/// `pub(crate)` so modpack imports can prepare the fresh dedicated HOME.
pub(crate) async fn ensure_web_profile_template_pub(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    home_path: &std::path::Path,
    version: &DshVersion,
) -> Result<(), String> {
    ensure_web_profile_template(app, state, task_id, home_path, version).await
}

/// DSH profiles are initialized by pnpm 11, and `dsh plugin` shells out to
/// whatever pnpm is on PATH. A different pnpm major produces trees the CLI
/// does not expect and fails in ways that look unrelated, so the launcher
/// pins the major it drives every install with.
pub(crate) const REQUIRED_PNPM_MAJOR: u32 = 11;

/// Parses the major version out of `pnpm --version` output ("11.17.0\n").
fn pnpm_major(version_output: &str) -> Option<u32> {
    version_output
        .trim()
        .split('.')
        .next()?
        .trim()
        .parse::<u32>()
        .ok()
}

/// Returns a pnpm executable whose major version is [`REQUIRED_PNPM_MAJOR`].
/// Prefers the system pnpm when its major matches; otherwise falls back to a
/// pinned pnpm bootstrapped into the launcher data dir (`tools/`), installing
/// or reinstalling it when missing or on the wrong major.
async fn ensure_pnpm(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
) -> Result<std::path::PathBuf, String> {
    // 1. System pnpm available AND on the required major?
    let mut sys_cmd = tokio::process::Command::new(crate::process::pnpm());
    crate::process::hide_console(&mut sys_cmd);
    let sys = sys_cmd.arg("--version").output().await;
    if let Ok(out) = sys {
        if out.status.success() {
            let raw = String::from_utf8_lossy(&out.stdout).to_string();
            match pnpm_major(&raw) {
                Some(REQUIRED_PNPM_MAJOR) => {
                    return Ok(std::path::PathBuf::from(crate::process::pnpm()))
                }
                other => {
                    let shown = raw.trim().to_string();
                    crate::log_warn!(
                        "系统 pnpm 版本为 {shown}（主版本 {other:?}），DSH profile 需要 pnpm {REQUIRED_PNPM_MAJOR}，改用启动器内置 pnpm"
                    );
                    let msg = format!(
                        "系统 pnpm {shown} 与所需的 pnpm {REQUIRED_PNPM_MAJOR} 不符，使用启动器内置 pnpm"
                    );
                    push_task_log(app, state, task_id, &msg).await;
                }
            }
        }
    }

    // 2. Local pnpm already bootstrapped on the required major?
    let tools_dir = state.data_dir.join("tools");
    let local = local_pnpm_path(&tools_dir);
    if local.exists() {
        let mut probe_cmd = tokio::process::Command::new(&local);
        crate::process::hide_console(&mut probe_cmd);
        let probe = probe_cmd.arg("--version").output().await;
        if let Ok(out) = probe {
            if out.status.success()
                && pnpm_major(&String::from_utf8_lossy(&out.stdout)) == Some(REQUIRED_PNPM_MAJOR)
            {
                return Ok(local);
            }
        }
    }

    // 3. Bootstrap the pinned pnpm major inside the data dir via npm.
    std::fs::create_dir_all(&tools_dir).map_err(|e| format!("创建工具目录失败: {e}"))?;
    let spec = format!("pnpm@{REQUIRED_PNPM_MAJOR}");
    let msg = format!("正在安装 DSH profile 所需的 {spec}…");
    {
        let mut tasks = state.tasks.lock().await;
        if let Some(task) = tasks.get_mut(task_id) {
            task.percent = 5;
            push_log_locked(task, &msg);
        }
    }
    emit_progress(app, task_id, TaskState::Running, 5, None, None);
    emit_log(app, task_id, &msg);
    crate::log_info!("引导安装 {spec} 到 {}", tools_dir.display());

    let child = crate::process::hide_console(
        tokio::process::Command::new(crate::process::npm())
            .args(["install", "--global", "--prefix"])
            .arg(&tools_dir)
            .arg(&spec)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .spawn()
    .map_err(|e| format!("pnpm 安装启动失败: {e}"))?;
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("pnpm 安装等待失败: {e}"))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let last = err.lines().last().unwrap_or(&err).to_string();
        return Err(format!("pnpm 安装失败: {last}"));
    }

    let local = local_pnpm_path(&tools_dir);
    if !local.exists() {
        return Err(format!(
            "pnpm 安装完成但未找到可执行文件: {}",
            local.display()
        ));
    }
    Ok(local)
}

/// Path of the pnpm executable inside a tools dir (Windows uses .cmd).
fn local_pnpm_path(tools_dir: &std::path::Path) -> std::path::PathBuf {
    if cfg!(windows) {
        tools_dir.join("pnpm.cmd")
    } else {
        tools_dir.join("pnpm")
    }
}

// ---------------------------------------------------------------------------

enum StreamPipe {
    Out(tokio::process::ChildStdout),
    Err(tokio::process::ChildStderr),
}

impl tokio::io::AsyncRead for StreamPipe {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            StreamPipe::Out(p) => std::pin::Pin::new(p).poll_read(cx, buf),
            StreamPipe::Err(p) => std::pin::Pin::new(p).poll_read(cx, buf),
        }
    }
}

async fn stream_pipe(app: AppHandle, task_id: String, pipe: StreamPipe) {
    let state = app.state::<AppState>();
    let mut lines = BufReader::new(pipe).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        if line.is_empty() {
            continue;
        }
        let percent = {
            let mut tasks = state.tasks.lock().await;
            match tasks.get_mut(&task_id) {
                Some(task) if task.state == TaskState::Running => {
                    push_log_locked(task, &line);
                    let pct = (task.percent + 1).min(90);
                    task.percent = pct;
                    // Throttle: emit progress roughly every 20 log lines.
                    if task.logs.len() % 20 == 0 {
                        Some(pct)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        };
        emit_log(&app, &task_id, &line);
        if let Some(pct) = percent {
            emit_progress(&app, &task_id, TaskState::Running, pct, None, None);
        }
    }
}

fn push_log_locked(task: &mut TaskInfo, line: &str) {
    if task.logs.len() >= MAX_LOG_LINES {
        task.logs.remove(0);
    }
    task.logs.push(line.to_string());
}

fn emit_progress(
    app: &AppHandle,
    id: &str,
    state: TaskState,
    percent: u32,
    message: Option<String>,
    instance_id: Option<String>,
) {
    let _ = app.emit(
        TASK_PROGRESS_EVENT,
        TaskProgress {
            id: id.to_string(),
            state,
            percent,
            message,
            instance_id,
        },
    );
}

fn emit_log(app: &AppHandle, id: &str, line: &str) {
    let _ = app.emit(
        TASK_LOG_EVENT,
        TaskLog {
            id: id.to_string(),
            line: line.to_string(),
        },
    );
}

// ---------------------------------------------------------------------------
// Shared helpers reused by other modules (e.g. plugins.rs install tasks)
// ---------------------------------------------------------------------------

pub(crate) fn now_millis_pub() -> i64 {
    now_millis()
}

pub(crate) fn emit_progress_pub(
    app: &AppHandle,
    id: &str,
    state: TaskState,
    percent: u32,
    message: Option<String>,
    instance_id: Option<String>,
) {
    emit_progress(app, id, state, percent, message, instance_id);
}

pub(crate) fn push_log_locked_pub(task: &mut TaskInfo, line: &str) {
    push_log_locked(task, line);
}

/// Append a log line to a running task and stream it to the frontend.
pub(crate) async fn push_task_log_pub(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    line: &str,
) {
    {
        let mut tasks = state.tasks.lock().await;
        if let Some(task) = tasks.get_mut(task_id) {
            push_log_locked(task, line);
        }
    }
    emit_log(app, task_id, line);
}

pub(crate) async fn ensure_pnpm_pub(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
) -> Result<std::path::PathBuf, String> {
    ensure_pnpm(app, state, task_id).await
}

/// Runs a piped child command as a task: streams stdout/stderr into the task
/// log, exposes the child for cancellation, and nudges the percent upward
/// while it runs. Returns Err when the command fails or is cancelled.
pub(crate) async fn run_streamed_command(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    mut cmd: tokio::process::Command,
    what: &str,
) -> Result<(), String> {
    // Echo the exact command line into the task log: when a child fails
    // silently (pnpm at --loglevel=warn prints nothing for some errors), the
    // invocation itself is the only clue.
    let cmdline = {
        let std_cmd = cmd.as_std();
        let mut s = std_cmd.get_program().to_string_lossy().to_string();
        for arg in std_cmd.get_args() {
            s.push(' ');
            s.push_str(&arg.to_string_lossy());
        }
        s
    };
    push_task_log_pub(app, state, task_id, &format!("$ {cmdline}")).await;
    crate::log_debug!("run_streamed_command[{what}]: {cmdline}");

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{what} 启动失败: {e}（请确认已安装 Node.js 与 pnpm）"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let shared_child: Arc<Mutex<Option<tokio::process::Child>>> = Arc::new(Mutex::new(Some(child)));

    {
        let mut tasks = state.tasks.lock().await;
        if let Some(task) = tasks.get_mut(task_id) {
            task.child = Some(shared_child.clone());
        }
    }

    let mut pipe_readers = Vec::new();
    for pipe in [stdout.map(StreamPipe::Out), stderr.map(StreamPipe::Err)]
        .into_iter()
        .flatten()
    {
        let app2 = app.clone();
        let tid = task_id.to_string();
        pipe_readers.push(tauri::async_runtime::spawn(async move {
            stream_pipe(app2, tid, pipe).await;
        }));
    }

    // Heartbeat keeps the percent moving while the command is quiet.
    {
        let app2 = app.clone();
        let tid = task_id.to_string();
        let hb_child = shared_child.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                let still_running = {
                    let mut guard = hb_child.lock().await;
                    match guard.as_mut() {
                        Some(c) => matches!(c.try_wait(), Ok(None)),
                        None => false,
                    }
                };
                if !still_running {
                    break;
                }
                let state = app2.state::<AppState>();
                let mut tasks = state.tasks.lock().await;
                let Some(task) = tasks.get_mut(&tid) else {
                    break;
                };
                if task.state != TaskState::Running {
                    break;
                }
                if task.percent < 90 {
                    task.percent = (task.percent + 2).min(90);
                } else if task.percent < 99 {
                    task.percent += 1;
                } else {
                    break;
                }
                let pct = task.percent;
                drop(tasks);
                emit_progress(&app2, &tid, TaskState::Running, pct, None, None);
            }
        });
    }

    let status = loop {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let mut guard = shared_child.lock().await;
        let Some(child) = guard.as_mut() else {
            return Err("任务已取消".to_string());
        };
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => continue,
            Err(e) => return Err(format!("{what} 等待失败: {e}")),
        }
    };

    // Clear the child handle so a later cancel is a no-op.
    {
        let mut tasks = state.tasks.lock().await;
        if let Some(task) = tasks.get_mut(task_id) {
            task.child = None;
        }
    }

    // Drain the pipe readers before judging the outcome: the child has
    // exited but the reader tasks may still be flushing buffered lines into
    // the task log. Reading the log here without waiting races them and
    // loses the very error lines the failure summary is built from.
    for reader in pipe_readers {
        let _ = reader.await;
    }

    if !status.success() {
        let logs = {
            let tasks = state.tasks.lock().await;
            tasks
                .get(task_id)
                .map(|t| t.logs.clone())
                .unwrap_or_default()
        };
        // Prefer a meaningful error line (pnpm error codes, "error:",
        // "aborted", "failed", "timeout") and keep the lines around it —
        // pnpm's remedy ("reinstall your dependencies with pnpm install")
        // sits on the following lines and is exactly what the user needs.
        let is_error_line = |l: &str| {
            let s = l.to_lowercase();
            s.contains("err_pnpm")
                || s.contains("error")
                || s.contains("aborted")
                || s.contains("failed")
                || s.contains("timeout")
        };
        let summary = match logs.iter().rposition(|l| is_error_line(l)) {
            Some(i) => logs[i..]
                .iter()
                .filter(|l| !l.trim().is_empty())
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | "),
            None => {
                let mut tail: Vec<String> = logs
                    .iter()
                    .rev()
                    .filter(|l| !l.trim().is_empty())
                    .take(3)
                    .cloned()
                    .collect();
                tail.reverse();
                tail.join(" | ")
            }
        };
        let summary = if summary.is_empty() {
            format!("{what} 退出码 {status}（子进程未输出任何日志）")
        } else {
            summary
        };
        crate::log_warn!("{what} 失败（{status}）: {summary}");
        return Err(format!("{what} 失败: {summary}"));
    }
    crate::log_debug!("run_streamed_command[{what}]: 完成");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pnpm_major_parses_version_output() {
        assert_eq!(pnpm_major("11.17.0\n"), Some(11));
        assert_eq!(pnpm_major("  10.4.1  "), Some(10));
        assert_eq!(pnpm_major("12.0.0-beta.1"), Some(12));
        assert_eq!(pnpm_major(""), None);
        assert_eq!(pnpm_major("not-a-version"), None);
    }

    #[test]
    fn required_pnpm_major_is_the_profile_toolchain() {
        // DSH profiles are initialized by pnpm 11; changing this constant
        // means the launcher drives installs with a different major.
        assert_eq!(REQUIRED_PNPM_MAJOR, 11);
    }
}
