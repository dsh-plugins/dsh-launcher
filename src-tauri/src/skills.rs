//! SKILL management (issue #10): DSH user-level skills live in
//! `<DSH_HOME>/skills` as directory bundles `<name>/SKILL.md` (or flat
//! `<name>.md`). Skills can be installed/updated from source repositories
//! (`https://[user:password@]github.com/user/repo[.git][#/path/to/skill]`),
//! imported from a local SKILL.md, or created from pasted content.
//! Repo-sourced skills record their origin (repo URL + commit/tag) in
//! `.dsh-skill.json` inside the bundle so they can be updated later.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::AppState;

const SKILL_META: &str = ".dsh-skill.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillOrigin {
    /// The repo URL as entered (may include `#/path/to/skill`).
    pub repo: String,
    pub commit: String,
    #[serde(default)]
    pub tag: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    /// "dir" bundle or flat "file" skill.
    pub kind: String,
    /// On-disk entry name in the skills directory (directory name or
    /// `<name>.md`) — may differ from the frontmatter `name` (issue #61).
    pub entry: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<SkillOrigin>,
}

fn home_path_of(state: &AppState, home_id: &str) -> Result<PathBuf, String> {
    state
        .config
        .lock()
        .unwrap()
        .homes
        .iter()
        .find(|h| h.id == home_id)
        .map(crate::wsl::home_fs_path)
        .ok_or_else(|| "DSH_HOME 不存在".to_string())
}

/// Resolves a HOME to `(fs_path, linux_path, distro)` (issue #49 G6).
///
/// `fs_path` is what Windows file APIs use (`\\wsl$\…` for WSL homes);
/// `linux_path` is the in-distro path, only ever handed to wsl.exe. A local
/// HOME returns the same path for both and `distro = None`.
fn home_paths_of(
    state: &AppState,
    home_id: &str,
) -> Result<(PathBuf, PathBuf, Option<String>), String> {
    let cfg = state.config.lock().unwrap();
    let home = cfg
        .homes
        .iter()
        .find(|h| h.id == home_id)
        .ok_or_else(|| "DSH_HOME 不存在".to_string())?;
    Ok((
        crate::wsl::home_fs_path(home),
        home.path.clone(),
        home.wsl.clone(),
    ))
}

/// Runs a command inside the distro through `bash -lc`, returning trimmed
/// stdout. Used for the write-side git work (clone / rev-parse): a Windows
/// `git.exe` writing into `\\wsl$\` is slow, cannot reach the share while the
/// distro is stopped, and applies Windows permission semantics (no exec bit,
/// different symlink handling) to a Linux tree.
async fn wsl_run(distro: &str, script: &str) -> Result<String, String> {
    let out = crate::wsl::wsl_bash(distro, script)
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

/// Script that allocates the in-distro scratch directory used for repo
/// clones. `mktemp -d` keeps concurrent installs from colliding; the caller
/// creates and removes it entirely inside the distro so nothing has to cross
/// the Windows/Linux boundary.
fn wsl_tmp_dir_script() -> &'static str {
    "mktemp -d /tmp/dsh-skill-XXXXXX"
}

/// `git clone` script for the distro (issue #49 G6). Both arguments are
/// shell-quoted: the URL is user input and the directory comes from `mktemp`,
/// so neither may be interpolated raw.
fn wsl_clone_script(clone_url: &str, dir: &str) -> String {
    format!(
        "git clone --depth 1 {url} {dir}",
        url = crate::wsl::sh_quote(clone_url),
        dir = crate::wsl::sh_quote(dir),
    )
}

/// `git rev-parse HEAD` script for an in-distro clone.
fn wsl_rev_parse_script(dir: &str) -> String {
    format!(
        "git -C {dir} rev-parse HEAD",
        dir = crate::wsl::sh_quote(dir)
    )
}

/// `git describe --tags --exact-match` script; the tag is optional, so
/// stderr is discarded and a non-zero exit is not an error.
fn wsl_describe_script(dir: &str) -> String {
    format!(
        "git -C {dir} describe --tags --exact-match 2>/dev/null",
        dir = crate::wsl::sh_quote(dir),
    )
}

/// `rm -rf` script for the scratch clone (always run, success or not).
fn wsl_rm_script(dir: &str) -> String {
    format!("rm -rf {}", crate::wsl::sh_quote(dir))
}

fn skills_dir(home: &Path) -> PathBuf {
    home.join("skills")
}

/// Parses the YAML frontmatter of a SKILL.md: required `name` +
/// `description`. Returns None when there is no valid frontmatter.
fn parse_frontmatter(content: &str) -> Option<(String, String)> {
    let mut lines = content.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut block = String::new();
    for line in lines {
        if line.trim_end().trim() == "---" {
            break;
        }
        block.push_str(line);
        block.push('\n');
    }
    // Full YAML parse so folded/literal multi-line descriptions
    // (`description: >-` + indented lines) load correctly.
    let value: serde_yaml::Value = serde_yaml::from_str(&block).ok()?;
    let map = value.as_mapping()?;
    let get = |key: &str| {
        map.get(serde_yaml::Value::String(key.to_string()))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    Some((get("name")?, get("description").unwrap_or_default()))
}

/// Validates a skill name (used for the on-disk directory).
fn sanitize_skill_name(name: &str) -> Result<String, String> {
    let clean = crate::config::sanitize_name(name);
    if clean.is_empty() {
        return Err(format!("无效的 SKILL 名称: {name}"));
    }
    Ok(clean)
}

/// Parses a skill repo URL into (clone URL, optional `#/sub/path`).
/// Accepts `https://[user:password@]host/owner/repo[.git][#/path/to/skill]`.
pub(crate) fn parse_skill_repo_url(url: &str) -> Result<(String, Option<String>), String> {
    let url = url.trim();
    let (base, sub) = match url.split_once('#') {
        // `#/path/` and `#path` both mean the same sub path; normalize the
        // slashes away (issue #53).
        Some((b, s)) => (b, Some(s.trim().trim_matches('/').to_string())),
        None => (url, None),
    };
    if !(base.starts_with("https://") || base.starts_with("http://")) {
        return Err(format!("SKILL 仓库地址需以 https:// 开头: {url}"));
    }
    let without_scheme = base.split_once("://").map(|x| x.1).unwrap_or("");
    // user:password@host/owner/repo → require at least host + owner + repo.
    let path_part = without_scheme
        .split_once('@')
        .map(|(_, rest)| rest)
        .unwrap_or(without_scheme);
    let segments: Vec<&str> = path_part.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 3 {
        return Err(format!(
            "SKILL 仓库地址不完整（需要 host/owner/repo）: {url}"
        ));
    }
    let clone_url = base
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_string();
    let sub = sub.filter(|s| !s.is_empty());
    Ok((format!("{clone_url}.git"), sub))
}

fn read_origin(skill_dir: &Path) -> Option<SkillOrigin> {
    let raw = std::fs::read_to_string(skill_dir.join(SKILL_META)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn skill_info_from_dir(dir: &Path) -> Option<SkillInfo> {
    let content = std::fs::read_to_string(dir.join("SKILL.md")).ok()?;
    let (name, description) = parse_frontmatter(&content)?;
    Some(SkillInfo {
        name,
        description,
        kind: "dir".to_string(),
        entry: dir.file_name()?.to_string_lossy().to_string(),
        origin: read_origin(dir),
    })
}

/// Lists the skills of an instance's HOME (`<home>/skills`).
#[tauri::command]
pub fn list_instance_skills(
    state: State<'_, AppState>,
    home_id: String,
) -> Result<Vec<SkillInfo>, String> {
    let home = home_path_of(&state, &home_id)?;
    let dir = skills_dir(&home);
    let mut out = Vec::new();
    if dir.exists() {
        let entries = std::fs::read_dir(&dir).map_err(|e| format!("读取 SKILL 目录失败: {e}"))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().map(|n| n == ".system").unwrap_or(false) {
                    continue;
                }
                if let Some(info) = skill_info_from_dir(&path) {
                    out.push(info);
                }
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Some((name, description)) = parse_frontmatter(&content) {
                        out.push(SkillInfo {
                            name,
                            description,
                            kind: "file".to_string(),
                            entry: entry.file_name().to_string_lossy().to_string(),
                            origin: None,
                        });
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Opens the instance's SKILL directory (`<home>/skills`) in the system file
/// manager, creating it when missing (issue #60). WSL homes open through the
/// distro's `\\wsl$\` share after making sure the distro is running. Returns
/// the opened path so the UI can surface it.
#[tauri::command]
pub async fn open_skills_directory(
    state: State<'_, AppState>,
    home_id: String,
) -> Result<String, String> {
    let (fs, _linux, distro) = home_paths_of(&state, &home_id)?;
    if let Some(d) = distro.as_deref() {
        crate::wsl::ensure_distro_running(&state, d).await?;
    }
    let dir = skills_dir(&fs);
    if !dir.is_dir() {
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    open::that(&dir).map_err(|e| format!("打开目录失败: {e}"))?;
    Ok(dir.to_string_lossy().to_string())
}

/// Exports selected skills as one ZIP archive (issue #61). `entries` are
/// on-disk names inside `<home>/skills` (directory names or `<name>.md`
/// files); anything containing path separators is rejected. The archive
/// mirrors the skills directory layout, so it round-trips through
/// `import_skill_zip`.
#[tauri::command(rename_all = "snake_case")]
pub async fn export_skills(
    state: State<'_, AppState>,
    home_id: String,
    entries: Vec<String>,
    target: String,
) -> Result<(), String> {
    if entries.is_empty() {
        return Err("未选择要导出的 SKILL".to_string());
    }
    let fs = home_path_of(&state, &home_id)?;
    // Reading a WSL home goes through \\wsl$\, which can block for
    // milliseconds per call; keep the runtime free (issue #49 G3).
    crate::wsl::run_blocking(move || export_skills_blocking(&fs, &entries, &target)).await?
}

fn export_skills_blocking(home: &Path, entries: &[String], target: &str) -> Result<(), String> {
    let dir = skills_dir(home);
    let file = std::fs::File::create(target).map_err(|e| format!("创建 ZIP 失败 {target}: {e}"))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for entry in entries {
        if entry.is_empty()
            || entry.contains('/')
            || entry.contains('\\')
            || entry == "."
            || entry == ".."
        {
            return Err(format!("非法的 SKILL 条目名: {entry}"));
        }
        let path = dir.join(entry);
        if path.is_dir() {
            zip_skill_dir(&mut zip, &path, entry, opts)?;
        } else if path.is_file() {
            let bytes =
                std::fs::read(&path).map_err(|e| format!("读取 SKILL 失败 {entry}: {e}"))?;
            zip.start_file(entry.as_str(), opts)
                .map_err(|e| format!("写入 ZIP 失败: {e}"))?;
            use std::io::Write;
            zip.write_all(&bytes)
                .map_err(|e| format!("写入 ZIP 失败: {e}"))?;
        } else {
            return Err(format!("SKILL 不存在: {entry}"));
        }
    }
    zip.finish().map_err(|e| format!("写入 ZIP 失败: {e}"))?;
    Ok(())
}

/// Adds one skill directory to the archive under `<prefix>/`, skipping VCS
/// metadata (`.git`).
fn zip_skill_dir(
    zip: &mut zip::ZipWriter<std::fs::File>,
    dir: &Path,
    prefix: &str,
    opts: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    use std::io::Write;
    for item in std::fs::read_dir(dir).map_err(|e| format!("读取 SKILL 目录失败: {e}"))? {
        let item = item.map_err(|e| e.to_string())?;
        if item.file_name() == ".git" {
            continue;
        }
        let name = format!("{}/{}", prefix, item.file_name().to_string_lossy());
        let path = item.path();
        if path.is_dir() {
            zip_skill_dir(zip, &path, &name, opts)?;
        } else {
            let bytes = std::fs::read(&path).map_err(|e| format!("读取文件失败 {name}: {e}"))?;
            zip.start_file(name.as_str(), opts)
                .map_err(|e| format!("写入 ZIP 失败: {e}"))?;
            zip.write_all(&bytes)
                .map_err(|e| format!("写入 ZIP 失败: {e}"))?;
        }
    }
    Ok(())
}

/// Runs git and returns trimmed stdout; errors carry stderr.
async fn git(args: &[&str], cwd: Option<&Path>) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    crate::process::hide_console(&mut cmd);
    cmd.args(args)
        .env("CI", "true")
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("执行 git 失败（未安装？）: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} 失败: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("创建目录失败: {e}"))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("读取目录失败: {e}"))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            if entry.file_name() == ".git" {
                continue;
            }
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| format!("复制文件失败: {e}"))?;
        }
    }
    Ok(())
}

/// Finds skill bundles under a root directory: the `#/sub` path, the root
/// itself when it carries SKILL.md, or every top-level directory that does.
/// A `#/sub` path may name a single skill (SKILL.md inside) or a container
/// directory holding several skills (issue #53, e.g. `#/.agents/skills/`).
fn collect_bundles(root: &Path, subpath: Option<&str>) -> Result<Vec<PathBuf>, String> {
    let mut bundles: Vec<PathBuf> = Vec::new();
    if let Some(sub) = subpath {
        let dir = root.join(sub);
        if dir.join("SKILL.md").exists() {
            bundles.push(dir);
        } else if dir.is_dir() {
            // The sub path is a multi-skill container: collect every direct
            // child that is a skill.
            for entry in std::fs::read_dir(&dir)
                .map_err(|e| e.to_string())?
                .flatten()
            {
                let p = entry.path();
                if p.is_dir() && p.join("SKILL.md").exists() {
                    bundles.push(p);
                }
            }
            if bundles.is_empty() {
                return Err(format!("子目录 {sub} 及其下没有找到 SKILL.md"));
            }
        } else {
            return Err(format!("子目录 {sub} 中没有 SKILL.md"));
        }
    } else if root.join("SKILL.md").exists() {
        bundles.push(root.to_path_buf());
    } else {
        for entry in std::fs::read_dir(root)
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let p = entry.path();
            if p.is_dir() && p.join("SKILL.md").exists() {
                bundles.push(p);
            }
        }
        if bundles.is_empty() {
            return Err("没有找到 SKILL.md".to_string());
        }
    }
    Ok(bundles)
}

/// Copies one bundle into `<home>/skills/<name>`; returns the skill name.
fn install_bundle(
    bundle: &Path,
    dest_root: &Path,
    origin: Option<&SkillOrigin>,
) -> Result<String, String> {
    let content = std::fs::read_to_string(bundle.join("SKILL.md"))
        .map_err(|e| format!("读取 SKILL.md 失败: {e}"))?;
    let (name, _) = parse_frontmatter(&content)
        .ok_or_else(|| format!("{} 的 SKILL.md 缺少有效 frontmatter", bundle.display()))?;
    let dir_name = sanitize_skill_name(&name)?;
    std::fs::create_dir_all(dest_root).map_err(|e| format!("创建 SKILL 目录失败: {e}"))?;
    let dest = dest_root.join(&dir_name);
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| format!("清理旧 SKILL 失败: {e}"))?;
    }
    copy_dir_recursive(bundle, &dest)?;
    if let Some(origin) = origin {
        std::fs::write(
            dest.join(SKILL_META),
            serde_json::to_vec_pretty(origin).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("写入 SKILL 来源信息失败: {e}"))?;
    }
    Ok(name)
}

/// Installs (or reinstalls) skill(s) from a repo clone into the HOME.
/// Returns the installed skill names.
async fn install_from_clone(
    clone_dir: &Path,
    url: &str,
    subpath: Option<&str>,
    home: &Path,
) -> Result<Vec<String>, String> {
    let commit = git(&["rev-parse", "HEAD"], Some(clone_dir)).await?;
    let tag = git(&["describe", "--tags", "--exact-match"], Some(clone_dir))
        .await
        .ok()
        .filter(|t| !t.is_empty());
    let origin = SkillOrigin {
        repo: url.to_string(),
        commit,
        tag,
    };
    let bundles = collect_bundles(clone_dir, subpath)
        .map_err(|e| format!("仓库中没有找到 SKILL.md（{e}）"))?;
    let dest_root = skills_dir(home);
    let mut installed = Vec::new();
    for bundle in bundles {
        installed.push(install_bundle(&bundle, &dest_root, Some(&origin))?);
    }
    Ok(installed)
}

/// Installs skill(s) from a source repository URL into the HOME's skills.
#[tauri::command]
pub async fn install_skill_repo(
    state: State<'_, AppState>,
    home_id: String,
    url: String,
) -> Result<Vec<String>, String> {
    let (home, home_linux, distro) = home_paths_of(&state, &home_id)?;
    let (clone_url, subpath) = parse_skill_repo_url(&url)?;
    // WSL (issue #49 G6): clone *inside* the distro. Cloning from Windows
    // into the `\\wsl$\` share works only while the distro runs, is far
    // slower, and lands Windows permission semantics on a Linux tree. The
    // clone also has to happen where the skills will live, so the copied
    // bundles keep their exec bits and symlinks.
    let names = if let Some(d) = &distro {
        crate::wsl::ensure_distro_running(&state, d).await?;
        let tmp = wsl_run(d, wsl_tmp_dir_script()).await?;
        let tmp = tmp.trim();
        if tmp.is_empty() {
            return Err("无法在发行版内创建临时目录".to_string());
        }
        let result = async {
            wsl_run(d, &wsl_clone_script(&clone_url, tmp)).await?;
            let dest_root = skills_dir(&home_linux);
            install_from_clone_wsl(d, tmp, url.trim(), subpath.as_deref(), &dest_root).await
        }
        .await;
        // Always drop the scratch clone, success or not.
        let _ = wsl_run(d, &wsl_rm_script(tmp)).await;
        result?
    } else {
        let tmp = std::env::temp_dir().join(format!("dsh-skill-{}", uuid::Uuid::new_v4()));
        let result = async {
            git(
                &["clone", "--depth", "1", &clone_url, &tmp.to_string_lossy()],
                None,
            )
            .await?;
            install_from_clone(&tmp, url.trim(), subpath.as_deref(), &home).await
        }
        .await;
        let _ = std::fs::remove_dir_all(&tmp);
        result?
    };
    crate::log_info!("已从 {url} 安装 SKILL: {}", names.join(", "));
    Ok(names)
}

/// WSL counterpart of `install_from_clone` (issue #49 G6): reads the clone
/// through `\\wsl$\` (bundles are just files) but runs git inside the distro
/// and copies into the distro's skills dir, so a Windows-side `git` never
/// touches a Linux tree.
async fn install_from_clone_wsl(
    distro: &str,
    clone_dir_linux: &str,
    url: &str,
    subpath: Option<&str>,
    dest_root_linux: &Path,
) -> Result<Vec<String>, String> {
    let commit = wsl_run(distro, &wsl_rev_parse_script(clone_dir_linux)).await?;
    let tag = wsl_run(distro, &wsl_describe_script(clone_dir_linux))
        .await
        .ok()
        .filter(|t| !t.is_empty());
    let origin = SkillOrigin {
        repo: url.to_string(),
        commit,
        tag,
    };
    // Bundle discovery and the copy both read/write through `\\wsl$\`, so the
    // whole walk runs on the blocking pool (issue #49 G3).
    let clone_fs = crate::wsl::unc_path(distro, clone_dir_linux);
    let dest_root_fs = crate::wsl::unc_path(distro, &dest_root_linux.to_string_lossy());
    let subpath = subpath.map(str::to_string);
    crate::wsl::run_blocking(move || {
        let bundles = collect_bundles(&clone_fs, subpath.as_deref())
            .map_err(|e| format!("仓库中没有找到 SKILL.md（{e}）"))?;
        let mut installed = Vec::new();
        for bundle in bundles {
            installed.push(install_bundle(&bundle, &dest_root_fs, Some(&origin))?);
        }
        Ok::<_, String>(installed)
    })
    .await?
}

/// A skill discovered in a source repository (not yet installed).
#[derive(Clone, Debug, Serialize)]
pub struct RepoSkillInfo {
    pub name: String,
    pub description: String,
    /// Top-level path inside the repo; None when the repo root is the skill.
    pub subpath: Option<String>,
}

/// Lists the skills a source repository offers (for the install picker).
/// Clones shallowly, reads frontmatter only, and cleans up.
#[tauri::command]
pub async fn list_repo_skills(url: String) -> Result<Vec<RepoSkillInfo>, String> {
    let (clone_url, subpath) = parse_skill_repo_url(&url)?;
    let tmp = std::env::temp_dir().join(format!("dsh-skill-{}", uuid::Uuid::new_v4()));
    let result = async {
        git(
            &["clone", "--depth", "1", &clone_url, &tmp.to_string_lossy()],
            None,
        )
        .await?;
        let bundles = collect_bundles(&tmp, subpath.as_deref())?;
        let mut out = Vec::new();
        for bundle in &bundles {
            let content = std::fs::read_to_string(bundle.join("SKILL.md"))
                .map_err(|e| format!("读取 SKILL.md 失败: {e}"))?;
            if let Some((name, description)) = parse_frontmatter(&content) {
                let sub = if bundle == &tmp {
                    None
                } else {
                    bundle
                        .strip_prefix(&tmp)
                        .ok()
                        .map(|p| p.to_string_lossy().replace('\\', "/"))
                };
                out.push(RepoSkillInfo {
                    name,
                    description,
                    subpath: sub,
                });
            }
        }
        Ok::<_, String>(out)
    }
    .await;
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// A repo-sourced skill whose remote HEAD moved past the recorded commit.
#[derive(Clone, Debug, Serialize)]
pub struct SkillUpdateInfo {
    pub name: String,
    pub current: String,
    pub latest: String,
}

/// Checks repo-sourced skills for updates: compares the recorded commit with
/// the remote default-branch HEAD (`git ls-remote`, one call per repo).
#[tauri::command]
pub async fn check_skill_updates(
    state: State<'_, AppState>,
    home_id: String,
) -> Result<Vec<SkillUpdateInfo>, String> {
    let home = home_path_of(&state, &home_id)?;
    let dir = skills_dir(&home);
    let mut origins: Vec<(String, SkillOrigin)> = Vec::new();
    if dir.exists() {
        for entry in std::fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let p = entry.path();
            if p.is_dir() {
                if let Some(origin) = read_origin(&p) {
                    if let Some(info) = skill_info_from_dir(&p) {
                        origins.push((info.name, origin));
                    }
                }
            }
        }
    }
    // One ls-remote per distinct repo (the #/sub path shares the clone URL).
    let mut head_cache: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    let mut updates = Vec::new();
    for (name, origin) in origins {
        let clone_url = match parse_skill_repo_url(&origin.repo) {
            Ok((u, _)) => u,
            Err(_) => continue,
        };
        let head = match head_cache.get(&clone_url) {
            Some(h) => h.clone(),
            None => {
                let h = git(&["ls-remote", &clone_url, "HEAD"], None)
                    .await
                    .ok()
                    .and_then(|out| out.split_whitespace().next().map(|s| s.to_string()));
                head_cache.insert(clone_url.clone(), h.clone());
                h
            }
        };
        if let Some(latest) = head {
            if !latest.is_empty() && latest != origin.commit {
                updates.push(SkillUpdateInfo {
                    name,
                    current: origin.commit.chars().take(7).collect(),
                    latest: latest.chars().take(7).collect(),
                });
            }
        }
    }
    Ok(updates)
}

/// Imports skills from a ZIP file: either a root-level SKILL.md (single
/// skill) or multiple top-level directories each carrying one. A single
/// wrapping directory (GitHub download style) is stripped.
#[tauri::command]
pub fn import_skill_zip(
    state: State<'_, AppState>,
    home_id: String,
    path: String,
) -> Result<Vec<String>, String> {
    let home = home_path_of(&state, &home_id)?;
    let src = PathBuf::from(path.trim());
    let file =
        std::fs::File::open(&src).map_err(|e| format!("打开 ZIP 失败 {}: {e}", src.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("解析 ZIP 失败: {e}"))?;

    let tmp = std::env::temp_dir().join(format!("dsh-skillzip-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<Vec<String>, String> {
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
            let Some(rel) = entry.enclosed_name() else {
                continue;
            };
            let dest = tmp.join(rel);
            if entry.is_dir() {
                std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
            } else {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut out = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
                std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
            }
        }
        // GitHub-style single wrapper directory (repo-main/…) is stripped.
        let bundles = match collect_bundles(&tmp, None) {
            Ok(b) => b,
            Err(_) => {
                let dirs: Vec<PathBuf> = std::fs::read_dir(&tmp)
                    .map_err(|e| e.to_string())?
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect();
                if dirs.len() == 1 {
                    collect_bundles(&dirs[0], None)?
                } else {
                    return Err("ZIP 中没有找到 SKILL.md（根目录或顶层子目录）".to_string());
                }
            }
        };
        let dest_root = skills_dir(&home);
        let mut installed = Vec::new();
        for bundle in bundles {
            installed.push(install_bundle(&bundle, &dest_root, None)?);
        }
        Ok(installed)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    let names = result?;
    crate::log_info!("已从 ZIP 导入 SKILL: {}", names.join(", "));
    Ok(names)
}

/// Reinstalls a repo-sourced skill from its recorded origin; returns the new
/// commit (and tag when available).
#[tauri::command]
pub async fn update_skill(
    state: State<'_, AppState>,
    home_id: String,
    name: String,
) -> Result<String, String> {
    let home = home_path_of(&state, &home_id)?;
    let dir = skills_dir(&home).join(sanitize_skill_name(&name)?);
    let origin =
        read_origin(&dir).ok_or_else(|| format!("SKILL「{name}」不是从仓库安装的，无法更新"))?;
    let names = install_skill_repo(state, home_id, origin.repo.clone()).await?;
    let new_origin = read_origin(&dir).ok_or_else(|| "更新后来源信息缺失".to_string())?;
    let version = new_origin
        .tag
        .clone()
        .unwrap_or_else(|| new_origin.commit.chars().take(7).collect());
    crate::log_info!(
        "SKILL {} 已更新到 {}（{:?}）",
        names.join(", "),
        version,
        new_origin.tag
    );
    Ok(version)
}

/// Deletes a skill (directory bundle or flat file).
#[tauri::command]
pub fn delete_skill(
    state: State<'_, AppState>,
    home_id: String,
    name: String,
) -> Result<(), String> {
    let home = home_path_of(&state, &home_id)?;
    let dir_name = sanitize_skill_name(&name)?;
    let bundle = skills_dir(&home).join(&dir_name);
    let flat = skills_dir(&home).join(format!("{dir_name}.md"));
    if bundle.exists() {
        std::fs::remove_dir_all(&bundle).map_err(|e| format!("删除 SKILL 失败: {e}"))?;
    } else if flat.exists() {
        std::fs::remove_file(&flat).map_err(|e| format!("删除 SKILL 失败: {e}"))?;
    } else {
        return Err(format!("SKILL「{name}」不存在"));
    }
    Ok(())
}

/// Imports a local SKILL.md file into the HOME's skills directory.
#[tauri::command]
pub fn import_skill_file(
    state: State<'_, AppState>,
    home_id: String,
    path: String,
) -> Result<String, String> {
    let home = home_path_of(&state, &home_id)?;
    let src = PathBuf::from(path.trim());
    let content = std::fs::read_to_string(&src)
        .map_err(|e| format!("读取 SKILL.md 失败 {}: {e}", src.display()))?;
    let (name, _) = parse_frontmatter(&content)
        .ok_or_else(|| "SKILL.md 缺少有效 frontmatter（需要 name 与 description）".to_string())?;
    let dir_name = sanitize_skill_name(&name)?;
    let dest_root = skills_dir(&home);
    std::fs::create_dir_all(&dest_root).map_err(|e| format!("创建 SKILL 目录失败: {e}"))?;
    let dest = dest_root.join(&dir_name);
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| format!("清理旧 SKILL 失败: {e}"))?;
    }
    std::fs::create_dir_all(&dest).map_err(|e| format!("创建 SKILL 目录失败: {e}"))?;
    std::fs::write(dest.join("SKILL.md"), content)
        .map_err(|e| format!("写入 SKILL.md 失败: {e}"))?;
    crate::log_info!("已导入 SKILL {name} <- {}", src.display());
    Ok(name)
}

/// Creates a skill from pasted content; a minimal frontmatter (name +
/// description) is prepended when the content lacks one.
#[tauri::command]
pub fn create_skill(
    state: State<'_, AppState>,
    home_id: String,
    name: String,
    description: String,
    content: String,
) -> Result<String, String> {
    let home = home_path_of(&state, &home_id)?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("SKILL 名称不能为空".to_string());
    }
    let dir_name = sanitize_skill_name(&name)?;
    let body = if parse_frontmatter(&content).is_some() {
        content
    } else {
        let desc = if description.trim().is_empty() {
            name.clone()
        } else {
            description.trim().to_string()
        };
        format!("---\nname: {name}\ndescription: {desc}\n---\n\n{content}")
    };
    let dest_root = skills_dir(&home);
    std::fs::create_dir_all(&dest_root).map_err(|e| format!("创建 SKILL 目录失败: {e}"))?;
    let dest = dest_root.join(&dir_name);
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| format!("清理旧 SKILL 失败: {e}"))?;
    }
    std::fs::create_dir_all(&dest).map_err(|e| format!("创建 SKILL 目录失败: {e}"))?;
    std::fs::write(dest.join("SKILL.md"), body).map_err(|e| format!("写入 SKILL.md 失败: {e}"))?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch directory under %TEMP%, removed by the caller.
    fn scratch_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dshl-skill-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn export_skills_blocking_archives_dirs_and_files() {
        let root = scratch_dir("export");
        let skills = skills_dir(&root);
        let dir_skill = skills.join("review");
        std::fs::create_dir_all(dir_skill.join("sub")).unwrap();
        std::fs::write(dir_skill.join("SKILL.md"), "---\nname: review\n---\n").unwrap();
        std::fs::write(dir_skill.join("sub").join("note.md"), "note").unwrap();
        std::fs::create_dir_all(dir_skill.join(".git")).unwrap();
        std::fs::write(dir_skill.join(".git").join("HEAD"), "x").unwrap();
        std::fs::write(skills.join("plain.md"), "---\nname: plain\n---\n").unwrap();
        std::fs::write(skills.join("unselected.md"), "---\nname: unselected\n---\n").unwrap();
        let target = root.join("out.zip");

        export_skills_blocking(
            &root,
            &["review".to_string(), "plain.md".to_string()],
            target.to_str().unwrap(),
        )
        .unwrap();

        let file = std::fs::File::open(&target).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.contains(&"review/SKILL.md".to_string()), "{names:?}");
        assert!(
            names.contains(&"review/sub/note.md".to_string()),
            "{names:?}"
        );
        assert!(names.contains(&"plain.md".to_string()), "{names:?}");
        assert!(!names.iter().any(|n| n.contains(".git")), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("unselected")), "{names:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn export_skills_blocking_rejects_traversal_and_missing() {
        let root = scratch_dir("export-guard");
        std::fs::create_dir_all(skills_dir(&root)).unwrap();
        let target = root.join("out.zip");
        let target_str = target.to_str().unwrap().to_string();
        assert!(export_skills_blocking(&root, &["../evil".to_string()], &target_str).is_err());
        assert!(export_skills_blocking(&root, &["a/b".to_string()], &target_str).is_err());
        assert!(export_skills_blocking(&root, &["ghost".to_string()], &target_str).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn skill_info_reports_on_disk_entry_name() {
        // The frontmatter name and the directory name may differ; the export
        // flow keys on the on-disk entry (issue #61).
        let root = scratch_dir("entry");
        let skill = skills_dir(&root).join("renamed-dir");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: 显示名\ndescription: d\n---\n",
        )
        .unwrap();
        let info = skill_info_from_dir(&skill).unwrap();
        assert_eq!(info.entry, "renamed-dir");
        assert_eq!(info.name, "显示名");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn parse_repo_url_variants() {
        let (clone, sub) = parse_skill_repo_url("https://github.com/Gu-ZT/skills").unwrap();
        assert_eq!(clone, "https://github.com/Gu-ZT/skills.git");
        assert_eq!(sub, None);

        let (clone, sub) =
            parse_skill_repo_url("https://github.com/Gu-ZT/skills.git#/conventional-commits")
                .unwrap();
        assert_eq!(clone, "https://github.com/Gu-ZT/skills.git");
        assert_eq!(sub.as_deref(), Some("conventional-commits"));

        let (clone, _) =
            parse_skill_repo_url("https://user:pass@github.com/Gu-ZT/skills.git").unwrap();
        assert_eq!(clone, "https://user:pass@github.com/Gu-ZT/skills.git");

        // Issue #53: `#/path/` with leading AND trailing slashes normalizes.
        let (_, sub) =
            parse_skill_repo_url("https://git.example.com/user/repo#/.agents/skills/").unwrap();
        assert_eq!(sub.as_deref(), Some(".agents/skills"));

        assert!(parse_skill_repo_url("github.com/Gu-ZT/skills").is_err());
        assert!(parse_skill_repo_url("https://github.com/Gu-ZT").is_err());
    }

    #[test]
    fn collect_bundles_accepts_multi_skill_container_subpath() {
        let tmp = std::env::temp_dir().join(format!("dsh-skill-test-{}", uuid::Uuid::new_v4()));
        let skill_md = "---\nname: x\ndescription: y\n---\nbody";
        // Sub path naming a single skill directly.
        std::fs::create_dir_all(tmp.join("single")).unwrap();
        std::fs::write(tmp.join("single/SKILL.md"), skill_md).unwrap();
        let bundles = collect_bundles(&tmp, Some("single")).unwrap();
        assert_eq!(bundles, vec![tmp.join("single")]);
        // Sub path naming a container of several skills (issue #53).
        std::fs::create_dir_all(tmp.join(".agents/skills/one")).unwrap();
        std::fs::create_dir_all(tmp.join(".agents/skills/two")).unwrap();
        std::fs::create_dir_all(tmp.join(".agents/skills/not-a-skill")).unwrap();
        std::fs::write(tmp.join(".agents/skills/one/SKILL.md"), skill_md).unwrap();
        std::fs::write(tmp.join(".agents/skills/two/SKILL.md"), skill_md).unwrap();
        let mut bundles = collect_bundles(&tmp, Some(".agents/skills")).unwrap();
        bundles.sort();
        assert_eq!(
            bundles,
            vec![
                tmp.join(".agents/skills/one"),
                tmp.join(".agents/skills/two")
            ]
        );
        // Empty container still errors.
        std::fs::create_dir_all(tmp.join("empty")).unwrap();
        assert!(collect_bundles(&tmp, Some("empty")).is_err());
        // Missing dir errors.
        assert!(collect_bundles(&tmp, Some("nope")).is_err());
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn frontmatter_requires_name_and_description_block() {
        assert_eq!(
            parse_frontmatter("---\nname: a\ndescription: b\n---\nbody"),
            Some(("a".to_string(), "b".to_string()))
        );
        assert_eq!(
            parse_frontmatter("---\nname: a\n---\nbody"),
            Some(("a".to_string(), String::new()))
        );
        assert!(parse_frontmatter("no frontmatter").is_none());
        assert!(parse_frontmatter("---\ndescription: b\n---\n").is_none());
    }

    #[test]
    fn frontmatter_parses_folded_multiline_description() {
        let md = "---\nname: demo\ndescription: >-\n  第一段折行，\n  第二行。\nlicense: MIT\n---\n# Body\n";
        let (name, desc) = parse_frontmatter(md).unwrap();
        assert_eq!(name, "demo");
        assert_eq!(desc, "第一段折行， 第二行。");
    }

    /// issue #49 G6: the clone runs inside the distro, so both the URL (user
    /// input) and the temp dir must be single-quoted — raw interpolation would
    /// let a crafted repo URL run arbitrary commands in the user's distro.
    #[test]
    fn wsl_clone_script_quotes_url_and_dir() {
        let script = wsl_clone_script("https://github.com/o/r.git", "/tmp/dsh-skill-abc123");
        assert_eq!(
            script,
            "git clone --depth 1 'https://github.com/o/r.git' '/tmp/dsh-skill-abc123'"
        );
        // A quote in the URL cannot break out of the single-quoted argument.
        let evil = wsl_clone_script("https://h/o/r'; rm -rf / #", "/tmp/d");
        assert!(
            evil.contains(r"'\''"),
            "the embedded quote must be escaped, got: {evil}"
        );
        assert!(
            evil.ends_with("'/tmp/d'"),
            "the directory must stay the last quoted argument: {evil}"
        );
    }

    /// The rev-parse/describe/rm scripts all address the scratch clone by its
    /// in-distro path, which is the only path git inside the distro knows.
    #[test]
    fn wsl_git_scripts_target_the_linux_path() {
        assert_eq!(
            wsl_rev_parse_script("/tmp/dsh-skill-x"),
            "git -C '/tmp/dsh-skill-x' rev-parse HEAD"
        );
        assert!(wsl_describe_script("/tmp/dsh-skill-x")
            .starts_with("git -C '/tmp/dsh-skill-x' describe"));
        assert_eq!(
            wsl_rm_script("/tmp/dsh-skill-x"),
            "rm -rf '/tmp/dsh-skill-x'"
        );
        assert_eq!(wsl_tmp_dir_script(), "mktemp -d /tmp/dsh-skill-XXXXXX");
    }

    /// A WSL home must resolve to a UNC fs path plus its Linux path and
    /// distro; a local home returns the same path twice and no distro.
    #[test]
    fn home_paths_of_separates_fs_and_linux_flavours() {
        let home = crate::config::DshHome {
            id: "h1".into(),
            name: "local".into(),
            path: std::path::PathBuf::from(r"C:\homes\l"),
            wsl: None,
            links: Default::default(),
        };
        assert_eq!(crate::wsl::home_fs_path(&home), home.path);
        assert_eq!(home.wsl, None);

        let wsl_home = crate::config::DshHome {
            id: "h2".into(),
            name: "wsl".into(),
            path: std::path::PathBuf::from("/home/u/.dsh-launcher/homes/w"),
            wsl: Some("Ubuntu".into()),
            links: Default::default(),
        };
        assert_eq!(
            crate::wsl::home_fs_path(&wsl_home),
            std::path::PathBuf::from(r"\\wsl$\Ubuntu\home\u\.dsh-launcher\homes\w")
        );
        assert_eq!(wsl_home.wsl.as_deref(), Some("Ubuntu"));
    }
}
