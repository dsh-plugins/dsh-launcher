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
        .map(|h| h.path.clone())
        .ok_or_else(|| "DSH_HOME 不存在".to_string())
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
        Some((b, s)) => (b, Some(s.trim_start_matches('/').trim().to_string())),
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
fn collect_bundles(root: &Path, subpath: Option<&str>) -> Result<Vec<PathBuf>, String> {
    let mut bundles: Vec<PathBuf> = Vec::new();
    if let Some(sub) = subpath {
        let dir = root.join(sub);
        if !dir.join("SKILL.md").exists() {
            return Err(format!("子目录 {sub} 中没有 SKILL.md"));
        }
        bundles.push(dir);
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
    let home = home_path_of(&state, &home_id)?;
    let (clone_url, subpath) = parse_skill_repo_url(&url)?;
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
    let names = result?;
    crate::log_info!("已从 {url} 安装 SKILL: {}", names.join(", "));
    Ok(names)
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

        assert!(parse_skill_repo_url("github.com/Gu-ZT/skills").is_err());
        assert!(parse_skill_repo_url("https://github.com/Gu-ZT").is_err());
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
}
