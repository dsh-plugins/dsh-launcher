//! Modpack (整合包) export/import — issue #5.
//!
//! Current format (pack-structure v2 / manifest v4): a `.dspack` is a plain
//! ZIP with a root `dspack.json` marker (`{"format":"dspack","version":2}`),
//! a root `manifest.json` (v4: adds `type: "profile"` and an optional
//! `files[]` download manifest), optional `package.json` / pnpm files, and an
//! `overrides/` directory whose user files are copied over the profile root
//! on import. Heavy content stays out of the archive: `files[]` entries carry
//! `path + sha256 + size + urls[]` and are fetched on demand, each verified
//! before the import completes.
//!
//! Legacy `.tgz` packs (flat layout, manifest v2/v3) are still accepted on
//! import.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::config::new_id;
use crate::AppState;

/// Manifest version the launcher writes.
pub const MANIFEST_VERSION: u32 = 5;

/// Exports stay on manifest v4 + pack-structure v2 for now (PackForge
/// tooling still generates v3/v4 packs); import accepts up to v5 / v3.
const EXPORT_MANIFEST_VERSION: u32 = 4;

/// `dspack.json` marker at the ZIP root (pack-structure v2). Export keeps
/// writing v2; import accepts v2 and v3 (see `validate_dspack_marker`).
const DSPACK_MARKER: &str = r#"{"format":"dspack","version":2}"#;

/// A profile unit inside a manifest v5 `type:"dshhome"` pack: the v4
/// single-profile contract minus `profileName` (the map key is the name).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileUnit {
    #[serde(default)]
    pub bundles: Vec<String>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub patch: Option<String>,
}

/// A preset index entry (manifest v5 dshhome form); the preset itself ships
/// as plain files under `overrides/.agent-presets/<id>/`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PresetUnit {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// A skill index entry (manifest v5 dshhome form). Small skills ship inside
/// `overrides/skills/` and only carry `path`; heavy skills add the pointer
/// triple for on-demand download.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillRef {
    pub path: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub urls: Vec<String>,
}

/// A `files[]` download entry (manifest v4): heavy content fetched on demand.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModpackFileEntry {
    /// Destination path relative to the profile root (`/` separated).
    pub path: String,
    /// Lowercase hex sha256 of the file content.
    pub sha256: String,
    /// Exact byte size.
    pub size: u64,
    /// Download mirrors, tried in order.
    pub urls: Vec<String>,
}

/// Modpack manifest. `display_name` / `description` stay untyped: v3+ allows
/// either a string or a `{locale: text}` map, and both round-trip verbatim.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModpackManifest {
    #[serde(rename = "manifestVersion")]
    pub manifest_version: u32,
    /// v4+: "profile" (v5 adds "dshhome"; "collection" stays reserved).
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub pack_type: Option<String>,
    pub name: String,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<serde_json::Value>,
    pub version: String,
    #[serde(default)]
    pub description: Option<serde_json::Value>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default, rename = "dshVersion")]
    pub dsh_version: Option<String>,
    #[serde(default, rename = "profileName")]
    pub profile_name: Option<String>,
    #[serde(default)]
    pub bundles: Vec<String>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub patch: Option<String>,
    /// v4: heavy content download manifest (not used by legacy v2/v3 packs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ModpackFileEntry>,
    // --- manifest v5 dshhome form (whole-DSH_HOME snapshot) ---------------
    #[serde(default, rename = "defaultProfile")]
    pub default_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profiles: Option<BTreeMap<String, ProfileUnit>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presets: Option<BTreeMap<String, PresetUnit>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<SkillRef>>,
    #[serde(default)]
    pub instructions: Option<String>,
}

/// Exportable content selection; `Default` exports the standard set (patch,
/// lockfile, workspace settings, icon) and skips extra user files.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct ExportContents {
    /// cordis.patch.yml patch layer, carried via `overrides/`.
    #[serde(default = "default_include")]
    pub patch: bool,
    /// pnpm-lock.yaml (frozen install on import).
    #[serde(default = "default_include")]
    pub lockfile: bool,
    /// pnpm-workspace.yaml.
    #[serde(default = "default_include")]
    pub workspace: bool,
    /// Instance icon bundled as icon.png.
    #[serde(default = "default_include")]
    pub icon: bool,
    /// Other user files in the profile, safety-filtered into `overrides/`.
    #[serde(default)]
    pub extra_files: bool,
}

fn default_include() -> bool {
    true
}

impl Default for ExportContents {
    fn default() -> Self {
        Self {
            patch: true,
            lockfile: true,
            workspace: true,
            icon: true,
            extra_files: false,
        }
    }
}

/// Export overrides: every field falls back to a sensible default derived
/// from the profile.
#[derive(Clone, Debug, Deserialize)]
pub struct ExportModpackInput {
    pub home_id: String,
    pub profile: String,
    /// Full output file path chosen via a save dialog; a missing or wrong
    /// extension gets `.dspack` appended.
    pub out_file: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<serde_json::Value>,
    #[serde(default)]
    pub description: Option<serde_json::Value>,
    #[serde(default)]
    pub author: Option<String>,
    /// Content selection; absent exports the default set.
    #[serde(default)]
    pub contents: Option<ExportContents>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ImportModpackInput {
    /// Local `.dspack` / legacy `.tgz` path or an http(s) URL.
    pub source: String,
    /// Replace an existing profile with the same name inside the new HOME.
    #[serde(default)]
    pub force: bool,
    /// Instance to create; defaults to the manifest's localized display name
    /// (frontend picks the current locale) or `name`.
    #[serde(default)]
    pub instance_name: Option<String>,
    /// Profile to create; defaults to the manifest's `profileName`, then
    /// `pack` (keeping `web` clean).
    #[serde(default)]
    pub profile_name: Option<String>,
    /// Import into an existing instance (issue #11): the pack profile is
    /// created in that instance's HOME instead of a new dedicated one. The
    /// instance's DSH version must share the manifest's version line.
    #[serde(default)]
    pub existing_instance_id: Option<String>,
}

/// Maximum accepted modpack size (64 MiB).
const MODPACK_MAX_BYTES: usize = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

/// The current OS username (`USERNAME` on Windows, `USER` elsewhere), used as
/// the default modpack author.
fn os_username() -> Option<String> {
    std::env::var("USERNAME")
        .ok()
        .or_else(|| std::env::var("USER").ok())
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
}

/// Whether a pnpm dependency spec points at a git host (GitHub).
fn is_git_spec(spec: &str) -> bool {
    spec.starts_with("git+") || spec.starts_with("github:") || spec.contains("github.com")
}

/// Extracts `owner/repo` from a git-ish spec (`git+https://github.com/o/r.git#ref`,
/// `github:o/r#ref`, `https://github.com/o/r`).
fn github_repo_from_spec(spec: &str) -> Option<(String, Option<String>, Option<String>)> {
    // Returns (repo, subpath, ref).
    let body = spec
        .strip_prefix("git+")
        .unwrap_or(spec)
        .trim_end_matches(".git");
    if let Some((repo, sub)) = crate::plugins::parse_github_id(body) {
        return Some((repo, sub, None));
    }
    let path = body
        .strip_prefix("https://github.com/")
        .or_else(|| body.strip_prefix("http://github.com/"))
        .or_else(|| body.strip_prefix("ssh://git@github.com/"))?;
    let (base, frag) = path.split_once('#').unwrap_or((path, ""));
    let mut seg = base.trim_matches('/').split('/');
    let repo = format!("{}/{}", seg.next()?, seg.next()?);
    // pnpm's `#<committish>&path:<sub>` fragment form.
    let mut git_ref = None;
    let mut sub = None;
    for part in frag.split('&') {
        if part.is_empty() {
            continue;
        }
        if let Some(p) = part.strip_prefix("path:") {
            sub = Some(p.trim_matches('/').to_string());
        } else {
            git_ref = Some(part.to_string());
        }
    }
    Some((repo, sub.filter(|s| !s.is_empty()), git_ref))
}

/// The commit a git dependency resolved to, read from the profile's
/// pnpm-lock.yaml (`importers..dependencies.<pkg>.version` looks like
/// `name@https://codeload.github.com/owner/repo/tar.gz/<sha>` or
/// `github.com/owner/repo/<sha>`).
fn locked_git_commit(lock_text: &str, pkg: &str) -> Option<String> {
    let doc: serde_yaml::Value = serde_yaml::from_str(lock_text).ok()?;
    let importers = doc.get("importers")?;
    for (_path, importer) in importers.as_mapping()? {
        for section in ["dependencies", "devDependencies"] {
            let entry = importer.get(section)?.get(pkg)?;
            let version = entry.get("version")?.as_str()?;
            // version: "<pkg>(<peer>)?@<resolved>" — take the part after '@'.
            let resolved = version.rsplit('@').next()?;
            let sha = resolved.trim_end_matches(')').rsplit('/').next()?;
            if sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
                return Some(sha.to_string());
            }
        }
    }
    None
}

/// The installed version of an npm dependency, from its package.json.
fn installed_npm_version(profile: &Path, pkg: &str) -> Option<String> {
    let raw = std::fs::read_to_string(profile.join("node_modules").join(pkg).join("package.json"))
        .ok()?;
    let doc: serde_json::Value = serde_json::from_str(&raw).ok()?;
    doc.get("version")?.as_str().map(|s| s.to_string())
}

/// Writes the modpack `.dspack` (pack-structure v2: plain ZIP) at `out` and
/// returns the path. `files` are `(archive path, bytes)` pairs.
fn write_modpack_dspack(out: &Path, files: &[(String, Vec<u8>)]) -> Result<PathBuf, String> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建输出目录失败: {e}"))?;
    }
    let file = std::fs::File::create(out).map_err(|e| format!("创建整合包文件失败: {e}"))?;
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in files {
        writer
            .start_file(name, options)
            .map_err(|e| format!("写入整合包条目 {name} 失败: {e}"))?;
        std::io::Write::write_all(&mut writer, bytes)
            .map_err(|e| format!("写入整合包条目 {name} 失败: {e}"))?;
    }
    writer
        .finish()
        .map_err(|e| format!("写入整合包失败: {e}"))?;
    Ok(out.to_path_buf())
}

/// Machine files that are rebuilt from the manifest (or handled separately)
/// and therefore never copied into `overrides/`.
const PROFILE_MACHINE_FILES: [&str; 4] = [
    "package.json",
    "cordis.patch.yml",
    "pnpm-lock.yaml",
    "pnpm-workspace.yaml",
];

/// Safety filter for exported overrides (pack-structure v1's rules): skips
/// VCS / dependency dirs, env & credential files, and nested archives.
fn overrides_excluded(lower_name: &str) -> bool {
    lower_name == "node_modules"
        || lower_name == ".git"
        || lower_name.starts_with(".env")
        || lower_name.ends_with(".pem")
        || lower_name.ends_with(".key")
        || lower_name.starts_with("id_rsa")
        || lower_name.starts_with("id_ed25519")
        || [".zip", ".tar", ".tgz", ".gz", ".7z", ".rar", ".dspack"]
            .iter()
            .any(|ext| lower_name.ends_with(ext))
}

/// Collects a profile's remaining user files as `overrides/` entries
/// (relative ZIP paths), skipping machine files and unsafe content.
fn collect_extra_files(profile: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = Vec::new();
    let mut stack = vec![profile.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("读取 profile 目录失败: {e}"))?
        {
            let entry = entry.map_err(|e| format!("读取 profile 条目失败: {e}"))?;
            let path = entry.path();
            let lower = entry.file_name().to_string_lossy().to_lowercase();
            if overrides_excluded(&lower) {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(profile)
                .map_err(|e| format!("解析 profile 相对路径失败: {e}"))?
                .to_string_lossy()
                .replace('\\', "/");
            if !rel.contains('/') && PROFILE_MACHINE_FILES.contains(&lower.as_str()) {
                continue;
            }
            let bytes =
                std::fs::read(&path).map_err(|e| format!("读取 profile 文件 {rel} 失败: {e}"))?;
            out.push((format!("overrides/{rel}"), bytes));
        }
    }
    Ok(out)
}

/// Extracts a modpack tgz into `dest`, refusing path-traversal entries.
fn extract_modpack_tgz(tgz: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(tgz).map_err(|e| format!("打开整合包失败: {e}"))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    std::fs::create_dir_all(dest).map_err(|e| format!("创建解压目录失败: {e}"))?;
    for entry in archive
        .entries()
        .map_err(|e| format!("读取整合包失败: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("读取整合包条目失败: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("读取整合包条目名失败: {e}"))?
            .into_owned();
        // Normalize: strip leading "./" and reject anything escaping dest.
        let clean: PathBuf = path
            .components()
            .filter(|c| matches!(c, std::path::Component::Normal(_)))
            .collect();
        if clean.as_os_str().is_empty() {
            continue;
        }
        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(dest.join(&clean)).ok();
            continue;
        }
        if entry.header().entry_type().is_file() {
            if let Some(parent) = clean.parent() {
                std::fs::create_dir_all(dest.join(parent))
                    .map_err(|e| format!("创建解压目录失败: {e}"))?;
            }
            std::io::copy(
                &mut entry,
                &mut std::fs::File::create(dest.join(&clean))
                    .map_err(|e| format!("创建解压文件失败: {e}"))?,
            )
            .map_err(|e| format!("解压条目失败: {e}"))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Pack-structure v2 container (.dspack = plain ZIP + root dspack.json)
// ---------------------------------------------------------------------------

/// Modpack container flavour, detected from the file magic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModpackContainer {
    /// pack-structure v1: gzipped tar with a flat layout (manifest v2/v3).
    LegacyTgz,
    /// pack-structure v2: plain ZIP with a root `dspack.json` marker
    /// (`format: "dspack", version: 2`) carrying manifest v4.
    Dspack,
}

/// Sniffs the container by magic bytes: ZIP local file header `PK\x03\x04`
/// (or the empty-archive marker `PK\x05\x06`) means `.dspack`; anything else
/// falls back to the legacy tgz reader.
fn detect_container(file: &Path) -> Result<ModpackContainer, String> {
    let mut f = std::fs::File::open(file).map_err(|e| format!("打开整合包失败: {e}"))?;
    let mut magic = [0u8; 2];
    let n = std::io::Read::read(&mut f, &mut magic).map_err(|e| format!("读取整合包失败: {e}"))?;
    if n == 2 && magic == *b"PK" {
        Ok(ModpackContainer::Dspack)
    } else {
        Ok(ModpackContainer::LegacyTgz)
    }
}

/// Validates the root `dspack.json` marker (pack-structure v2/v3 §2.2) and
/// returns the container version.
fn validate_dspack_marker<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<u64, String> {
    let mut entry = archive
        .by_name("dspack.json")
        .map_err(|_| "不是 .dspack 整合包（ZIP 根缺少 dspack.json 标记文件）".to_string())?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut entry, &mut text)
        .map_err(|e| format!("读取 dspack.json 失败: {e}"))?;
    let marker: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("解析 dspack.json 失败: {e}"))?;
    let version = marker.get("version").and_then(|v| v.as_u64());
    if marker.get("format").and_then(|v| v.as_str()) != Some("dspack")
        || !matches!(version, Some(2) | Some(3))
    {
        return Err(format!(
            "不支持该 .dspack 版本（需要 format=dspack, version 2-3；实际 version={}）",
            version.map(|v| v.to_string()).unwrap_or_default()
        ));
    }
    Ok(version.unwrap_or(2))
}

/// Container/manifest pairing (pack-structure v3 §2.2): a `.dspack` carries
/// manifest v4+, and manifest v5 requires the v3 container.
fn check_container_manifest(marker: u64, manifest: &ModpackManifest) -> Result<(), String> {
    if manifest.manifest_version < 4 {
        return Err(format!(
            ".dspack 容器要求 manifestVersion ≥ 4（实际为 {}）",
            manifest.manifest_version
        ));
    }
    if manifest.manifest_version == 5 && marker < 3 {
        return Err("manifest v5 需要 pack-structure v3 容器（dspack.json version 3）".to_string());
    }
    Ok(())
}

/// Extracts a `.dspack` ZIP into `dest`, refusing path-traversal entries, and
/// validates the root `dspack.json` marker before anything is written.
fn extract_modpack_dspack(dspack: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(dspack).map_err(|e| format!("打开整合包失败: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("解析 .dspack ZIP 失败: {e}"))?;
    validate_dspack_marker(&mut archive)?;
    std::fs::create_dir_all(dest).map_err(|e| format!("创建解压目录失败: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("读取 .dspack 条目失败: {e}"))?;
        let Some(name) = entry.enclosed_name().map(|p| p.to_path_buf()) else {
            continue; // absolute / parent-traversal entry: skip
        };
        if name.as_os_str().is_empty() {
            continue;
        }
        if entry.is_dir() {
            std::fs::create_dir_all(dest.join(&name)).ok();
            continue;
        }
        if let Some(parent) = name.parent() {
            std::fs::create_dir_all(dest.join(parent))
                .map_err(|e| format!("创建解压目录失败: {e}"))?;
        }
        std::io::copy(
            &mut entry,
            &mut std::fs::File::create(dest.join(&name))
                .map_err(|e| format!("创建解压文件失败: {e}"))?,
        )
        .map_err(|e| format!("解压条目失败: {e}"))?;
    }
    Ok(())
}

/// Reads just `manifest.json` out of a `.dspack` ZIP.
fn read_manifest_from_dspack(dspack: &Path) -> Result<ModpackManifest, String> {
    let file = std::fs::File::open(dspack).map_err(|e| format!("打开整合包失败: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("解析 .dspack ZIP 失败: {e}"))?;
    let marker = validate_dspack_marker(&mut archive)?;
    let mut entry = archive
        .by_name("manifest.json")
        .map_err(|_| ".dspack 缺少 manifest.json".to_string())?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut entry, &mut text)
        .map_err(|e| format!("读取 manifest.json 失败: {e}"))?;
    let manifest: ModpackManifest =
        serde_json::from_str(&text).map_err(|e| format!("解析 manifest.json 失败: {e}"))?;
    validate_manifest(&manifest)?;
    check_container_manifest(marker, &manifest)?;
    validate_pack_type(&manifest)?;
    validate_dshhome(&manifest)?;
    Ok(manifest)
}

/// Copies an extracted `overrides/` tree over the profile root: file-level
/// replacement, no content merge (pack-structure v2 §5). Returns the number
/// of files copied.
fn apply_overrides(unpacked: &Path, profile: &Path) -> Result<usize, String> {
    copy_tree(&unpacked.join("overrides"), profile, None)
}

/// Copies a directory tree over `dst` (file-level replacement). Entries
/// whose first relative component is in `skip_top` are skipped (the dshhome
/// form applies `overrides/profiles/<name>/` per profile, separately from
/// the home-level rest). Returns the number of files copied.
fn copy_tree(src: &Path, dst: &Path, skip_top: Option<&[&str]>) -> Result<usize, String> {
    if !src.is_dir() {
        return Ok(0);
    }
    let mut count = 0usize;
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("读取目录失败: {e}"))? {
            let entry = entry.map_err(|e| format!("读取目录条目失败: {e}"))?;
            let path = entry.path();
            let rel = path
                .strip_prefix(src)
                .map_err(|e| format!("解析相对路径失败: {e}"))?;
            if let Some(skip) = skip_top {
                if let Some(std::path::Component::Normal(first)) = rel.components().next() {
                    if skip.iter().any(|s| first == std::ffi::OsStr::new(s)) {
                        continue;
                    }
                }
            }
            let target = dst.join(rel);
            if path.is_dir() {
                std::fs::create_dir_all(&target).map_err(|e| format!("创建目录失败: {e}"))?;
                stack.push(path);
            } else {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
                }
                std::fs::copy(&path, &target)
                    .map_err(|e| format!("覆盖 {} 失败: {e}", rel.display()))?;
                count += 1;
            }
        }
    }
    Ok(count)
}

/// Resolves a `files[]` destination inside the profile, rejecting absolute
/// paths and parent traversal.
fn files_target(profile: &Path, rel: &str) -> Result<PathBuf, String> {
    let p = Path::new(rel);
    if rel.is_empty()
        || !p
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "files[] 路径非法（必须是不含 .. 的相对路径）: {rel}"
        ));
    }
    Ok(profile.join(p))
}

/// Downloads one `files[]` entry to `target`, trying each mirror in order
/// and verifying size + sha256. A failed mirror's partial file is removed
/// before the next one is tried.
async fn download_file_entry(entry: &ModpackFileEntry, target: &Path) -> Result<(), String> {
    let client = crate::proxy::apply(reqwest::Client::builder())
        .timeout(std::time::Duration::from_secs(600))
        .user_agent("dsh-launcher")
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    let mut last_err = "无可用下载地址".to_string();
    for url in &entry.urls {
        last_err = match try_download_file_url(&client, url, entry, target).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                std::fs::remove_file(target).ok();
                crate::log_warn!("files[] {} 从 {url} 下载失败: {e}", entry.path);
                e
            }
        };
    }
    Err(format!("{} 下载失败: {last_err}", entry.path))
}

async fn try_download_file_url(
    client: &reqwest::Client,
    url: &str,
    entry: &ModpackFileEntry,
    target: &Path,
) -> Result<(), String> {
    use sha2::Digest;
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("仅支持 http(s) 下载地址: {url}"));
    }
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    let mut file = std::fs::File::create(target).map_err(|e| format!("创建文件失败: {e}"))?;
    let mut hasher = sha2::Sha256::new();
    let mut size: u64 = 0;
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("下载中断: {e}"))? {
        std::io::Write::write_all(&mut file, &chunk).map_err(|e| format!("写入文件失败: {e}"))?;
        hasher.update(&chunk);
        size += chunk.len() as u64;
    }
    drop(file);
    if size != entry.size {
        return Err(format!(
            "大小不符（期望 {} 字节，实际 {size} 字节）",
            entry.size
        ));
    }
    let hash = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if !hash.eq_ignore_ascii_case(entry.sha256.trim()) {
        return Err("sha256 校验失败".to_string());
    }
    Ok(())
}

/// Fetches every `files[]` entry into the profile (manifest v4 §3). Any
/// failure removes the files downloaded so far and rolls the profile back.
async fn download_modpack_files(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    files: &[ModpackFileEntry],
    profile: &Path,
) -> Result<(), String> {
    let mut downloaded: Vec<PathBuf> = Vec::new();
    for entry in files {
        let target = files_target(profile, &entry.path)?;
        crate::tasks::push_task_log_pub(
            app,
            state,
            task_id,
            &format!("下载 files[]: {}（{} 字节）", entry.path, entry.size),
        )
        .await;
        if let Err(e) = download_file_entry(entry, &target).await {
            for f in &downloaded {
                std::fs::remove_file(f).ok();
            }
            return Err(format!("files[] 下载失败，已回滚: {e}"));
        }
        downloaded.push(target);
    }
    Ok(())
}

/// Reads just `manifest.json` out of a modpack tgz.
fn read_manifest_from_tgz(tgz: &Path) -> Result<ModpackManifest, String> {
    let file = std::fs::File::open(tgz).map_err(|e| format!("打开整合包失败: {e}"))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    for entry in archive
        .entries()
        .map_err(|e| format!("读取整合包失败: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("读取整合包条目失败: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("读取整合包条目名失败: {e}"))?
            .into_owned();
        let clean: PathBuf = path
            .components()
            .filter(|c| matches!(c, std::path::Component::Normal(_)))
            .collect();
        if clean == Path::new("manifest.json") {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut entry, &mut text)
                .map_err(|e| format!("读取 manifest.json 失败: {e}"))?;
            let manifest: ModpackManifest =
                serde_json::from_str(&text).map_err(|e| format!("解析 manifest.json 失败: {e}"))?;
            validate_manifest(&manifest)?;
            return Ok(manifest);
        }
    }
    Err("整合包缺少 manifest.json".to_string())
}

fn validate_manifest(manifest: &ModpackManifest) -> Result<(), String> {
    if !(2..=MANIFEST_VERSION).contains(&manifest.manifest_version) {
        return Err(format!(
            "不支持的 manifestVersion {}（支持 2-{MANIFEST_VERSION}）",
            manifest.manifest_version
        ));
    }
    Ok(())
}

/// v4+ `type` check: absent and "profile" are always accepted; v5 adds
/// "dshhome" (whole-DSH_HOME snapshot); "collection" is reserved and
/// rejected for now.
fn validate_pack_type(manifest: &ModpackManifest) -> Result<(), String> {
    match manifest.pack_type.as_deref() {
        None | Some("profile") => Ok(()),
        Some("dshhome") if manifest.manifest_version >= 5 => Ok(()),
        Some("dshhome") => Err("dshhome 形态需要 manifestVersion 5".to_string()),
        Some("collection") => Err("整合包集合（collection）暂未支持".to_string()),
        Some(other) => Err(format!("未知的整合包类型: {other}")),
    }
}

/// manifest v5 `type:"dshhome"` shape checks (manifest v5 §4): `profiles`
/// non-empty without the baseline `web` / `headless` templates, and
/// `defaultProfile` pointing at an existing key.
fn validate_dshhome(manifest: &ModpackManifest) -> Result<(), String> {
    if manifest.pack_type.as_deref() != Some("dshhome") {
        return Ok(());
    }
    let profiles = manifest
        .profiles
        .as_ref()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "dshhome 形态的整合包必须包含至少一个 profile".to_string())?;
    for (name, unit) in profiles {
        if name == "web" || name == "headless" {
            return Err(format!(
                "dshhome 形态的 profiles 不得包含基线模板 profile「{name}」"
            ));
        }
        if crate::config::sanitize_name(name) != name.trim() || name.trim().is_empty() {
            return Err(format!("dshhome 形态的 profile 名「{name}」无效"));
        }
        if unit.bundles.is_empty() {
            return Err(format!("dshhome 形态的 profile「{name}」缺少 bundles"));
        }
    }
    let default = manifest
        .default_profile
        .as_deref()
        .filter(|d| !d.trim().is_empty())
        .ok_or_else(|| "dshhome 形态缺少 defaultProfile".to_string())?;
    if !profiles.contains_key(default) {
        return Err(format!(
            "defaultProfile「{default}」不在 dshhome 形态的 profiles 中"
        ));
    }
    Ok(())
}

/// Downloads the modpack when `source` is a URL into a temp file; local
/// paths are used as-is. Returns (path, temp dir guard).
async fn fetch_modpack_source(source: &str) -> Result<(PathBuf, TmpDir), String> {
    let tmp = std::env::temp_dir().join(format!("dsh-modpack-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("创建临时目录失败: {e}"))?;
    let guard = TmpDir(tmp.clone());
    if source.starts_with("https://") || source.starts_with("http://") {
        let target = tmp.join("modpack.pack");
        download_modpack(source, &target).await?;
        Ok((target, guard))
    } else {
        let p = PathBuf::from(source);
        if !p.exists() {
            return Err(format!("整合包文件不存在: {}", p.display()));
        }
        Ok((p, guard))
    }
}

/// Pre-reads a modpack's manifest so the UI can show metadata and let the
/// user adjust instance/profile names before starting the install task.
#[tauri::command]
pub async fn read_modpack_manifest(source: String) -> Result<ModpackManifest, String> {
    let (file, _guard) = fetch_modpack_source(&source).await?;
    match detect_container(&file)? {
        ModpackContainer::Dspack => read_manifest_from_dspack(&file),
        ModpackContainer::LegacyTgz => read_manifest_from_tgz(&file),
    }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Exports a profile as a manifest-v4 `.dspack` (pack-structure v2: plain
/// ZIP + root `dspack.json` marker + `overrides/`). Dependencies are pinned:
/// npm specs become the installed version, git specs become
/// `github:owner/repo[#path:/sub]` → resolved commit sha. A custom instance
/// icon (issue #8) is bundled as `icon.png`; the default launcher icon is
/// never exported.
#[tauri::command]
pub async fn export_modpack(
    state: State<'_, AppState>,
    input: ExportModpackInput,
) -> Result<String, String> {
    let contents = input.contents.unwrap_or_default();
    let home = home_path_of(&state, &input.home_id)?;
    let profile_dir = crate::plugins::profile_dir_pub(&home, &input.profile);
    let pkg_path = profile_dir.join("package.json");
    let raw = std::fs::read_to_string(&pkg_path)
        .map_err(|e| format!("读取 profile manifest 失败: {e}"))?;
    let pkg: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("解析 profile manifest 失败: {e}"))?;

    let bundles: Vec<String> = pkg
        .pointer("/dsh/profile/bundles")
        .and_then(|b| b.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|b| b.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let lock_text = std::fs::read_to_string(profile_dir.join("pnpm-lock.yaml")).ok();
    let mut pinned = BTreeMap::new();
    if let Some(deps) = pkg.get("dependencies").and_then(|d| d.as_object()) {
        for (name, spec) in deps {
            let spec = spec.as_str().unwrap_or_default();
            if is_git_spec(spec) {
                let Some((repo, sub, spec_ref)) = github_repo_from_spec(spec) else {
                    crate::log_warn!("整合包导出：无法解析 git 依赖 {name}: {spec}，按原样保留");
                    pinned.insert(name.clone(), spec.to_string());
                    continue;
                };
                let sha = lock_text
                    .as_deref()
                    .and_then(|l| locked_git_commit(l, name))
                    .or(spec_ref)
                    .unwrap_or_else(|| "HEAD".to_string());
                let coord = match &sub {
                    Some(p) => format!("github:{repo}#path:/{p}"),
                    None => format!("github:{repo}"),
                };
                pinned.insert(coord, sha);
            } else {
                let version = installed_npm_version(&profile_dir, name)
                    .unwrap_or_else(|| spec.trim_start_matches(['^', '~']).to_string());
                pinned.insert(name.clone(), version);
            }
        }
    }

    // dshVersion: pinned to the exact version of the first instance bound to
    // this HOME, so import installs the same DSH the pack was built with.
    let dsh_version = {
        let cfg = state.config.lock().unwrap();
        cfg.instances
            .iter()
            .find(|i| i.home_id == input.home_id)
            .and_then(|i| cfg.versions.iter().find(|v| v.id == i.version_id))
            .map(|v| v.version.clone())
            .unwrap_or_else(|| "0.1.0".to_string())
    };

    let name = input
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| input.profile.clone());
    let version = input
        .version
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "1.0.0".to_string());

    let patch = if contents.patch {
        std::fs::read_to_string(profile_dir.join("cordis.patch.yml")).ok()
    } else {
        None
    };

    // Instance metadata (issue #8 icon; issue #12: displayName defaults to
    // the instance name, description to an empty string).
    let instance_info = {
        let cfg = state.config.lock().unwrap();
        cfg.instances
            .iter()
            .find(|i| i.home_id == input.home_id)
            .map(|i| (i.id.clone(), i.name.clone(), i.icon.clone()))
    };
    let instance_icon = instance_info
        .as_ref()
        .map(|(id, _, icon)| (id.clone(), icon.clone()));
    let mut icon_field: Option<String> = None;
    let mut icon_png: Option<Vec<u8>> = None;
    if contents.icon {
        if let Some((inst_id, Some(icon))) = instance_icon {
            if icon == "local" {
                if let Ok(bytes) = std::fs::read(crate::icons::local_icon_path(&home, &inst_id)) {
                    icon_png = Some(bytes);
                    icon_field = Some("icon.png".to_string());
                }
            } else if icon.starts_with("http") {
                match fetch_remote_icon(&icon).await {
                    Some(png) => {
                        icon_png = Some(png);
                        icon_field = Some("icon.png".to_string());
                    }
                    None => icon_field = Some(icon),
                }
            }
        }
    }

    let manifest = ModpackManifest {
        manifest_version: EXPORT_MANIFEST_VERSION,
        pack_type: Some("profile".to_string()),
        name: name.clone(),
        display_name: input
            .display_name
            .filter(|d| d.as_str().map(|s| !s.trim().is_empty()).unwrap_or(true))
            .or_else(|| {
                instance_info
                    .as_ref()
                    .map(|(_, name, _)| serde_json::Value::String(name.clone()))
            }),
        version: version.clone(),
        description: Some(
            input
                .description
                .unwrap_or_else(|| serde_json::Value::String(String::new())),
        ),
        author: input
            .author
            .filter(|a| !a.trim().is_empty())
            .or_else(os_username),
        icon: icon_field,
        dsh_version: Some(dsh_version),
        profile_name: Some(input.profile.clone()),
        bundles: bundles.clone(),
        dependencies: pinned,
        patch,
        files: Vec::new(),
        // dshhome-only fields stay empty: exports are single-profile packs.
        default_profile: None,
        profiles: None,
        presets: None,
        skills: None,
        instructions: None,
    };

    let profile_pkg = serde_json::json!({
        "name": format!("dsh-profile-{}", input.profile),
        "private": true,
        "dependencies": manifest_pkg_deps(&manifest),
        "dsh": { "profile": { "bundles": bundles } },
    });

    let mut files: Vec<(String, Vec<u8>)> = vec![
        ("dspack.json".to_string(), DSPACK_MARKER.as_bytes().to_vec()),
        (
            "manifest.json".to_string(),
            serde_json::to_vec_pretty(&manifest)
                .map_err(|e| format!("序列化 manifest 失败: {e}"))?,
        ),
        (
            "package.json".to_string(),
            serde_json::to_vec_pretty(&profile_pkg)
                .map_err(|e| format!("序列化 package.json 失败: {e}"))?,
        ),
    ];
    // pack-structure v2: user files live under overrides/ (file takes
    // precedence over the manifest's inline patch on import).
    if let Some(p) = &manifest.patch {
        files.push((
            "overrides/cordis.patch.yml".to_string(),
            p.clone().into_bytes(),
        ));
    }
    if contents.lockfile {
        if let Ok(lock) = std::fs::read(profile_dir.join("pnpm-lock.yaml")) {
            files.push(("pnpm-lock.yaml".to_string(), lock));
        }
    }
    if contents.workspace {
        if let Ok(ws) = std::fs::read(profile_dir.join("pnpm-workspace.yaml")) {
            files.push(("pnpm-workspace.yaml".to_string(), ws));
        }
    }
    if let Some(png) = icon_png {
        files.push(("icon.png".to_string(), png));
    }
    if contents.extra_files {
        files.extend(collect_extra_files(&profile_dir)?);
    }

    // The save dialog may return a path without the extension (or with a
    // different one); append `.dspack` instead of rejecting.
    let raw_path = input.out_file.trim();
    let out_path = if raw_path.to_lowercase().ends_with(".dspack") {
        PathBuf::from(raw_path)
    } else {
        PathBuf::from(format!("{raw_path}.dspack"))
    };
    let out = write_modpack_dspack(&out_path, &files)?;
    crate::log_info!("已导出整合包 {}", out.display());
    Ok(out.to_string_lossy().to_string())
}

/// Converts manifest dependencies into pnpm-installable package.json specs.
/// v3 coordinates (`github:owner/repo[#path:/sub]` → ref) become
/// `github:owner/repo#<ref>&path:<sub>`; npm names keep their (pinned)
/// version. v2 values are already pnpm specs and pass through.
fn manifest_pkg_deps(manifest: &ModpackManifest) -> BTreeMap<String, String> {
    pkg_deps_from_coords(&manifest.dependencies)
}

/// Runs `pnpm install` inside a pack profile dir. A shipped lockfile gets a
/// frozen install (exact pins); if pnpm rejects it as outdated relative to
/// our regenerated package.json, fall back to a normal install so the
/// import still succeeds.
async fn pnpm_install_profile(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    dest: &Path,
    has_lock: bool,
) -> Result<(), String> {
    let pnpm_prog = crate::tasks::ensure_pnpm_pub(app, state, task_id).await?;
    let store_dir = state.data_dir.join(".pnpm-store");
    let attempts: &[&[&str]] = if has_lock {
        &[&["--frozen-lockfile"], &["--no-frozen-lockfile"]]
    } else {
        &[&["--no-frozen-lockfile"]]
    };
    let mut last_err = String::new();
    for (i, extra) in attempts.iter().enumerate() {
        if i > 0 {
            crate::tasks::push_task_log_pub(
                app,
                state,
                task_id,
                "锁定文件与依赖清单不完全匹配，改用普通安装（锁定版本仍会被优先采用）…",
            )
            .await;
        }
        let mut cmd = tokio::process::Command::new(&pnpm_prog);
        crate::process::hide_console(&mut cmd);
        cmd.current_dir(dest)
            .arg("install")
            .args(extra.iter().copied())
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
        match crate::tasks::run_streamed_command(app, state, task_id, cmd, "pnpm install（整合包）")
            .await
        {
            Ok(()) => {
                last_err.clear();
                break;
            }
            Err(e) => last_err = e,
        }
    }
    if !last_err.is_empty() {
        return Err(last_err);
    }
    Ok(())
}

/// Converts manifest dependency coordinates into package.json deps: github
/// coords become install specs keyed by a derived package name, everything
/// else passes through.
fn pkg_deps_from_coords(deps: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (coord, version) in deps {
        if let Some((repo, sub)) = crate::plugins::parse_github_id(coord) {
            // package.json key must be a package name; derive it from the repo.
            let pkg_name = coord_to_pkg_name(coord);
            out.insert(
                pkg_name,
                crate::plugins::github_install_spec(&repo, version, sub.as_deref()),
            );
        } else {
            out.insert(coord.clone(), version.clone());
        }
    }
    out
}

/// The manifest-authoritative machine package.json for a pack profile.
fn profile_pkg_json(
    profile_name: &str,
    bundles: &[String],
    deps: &BTreeMap<String, String>,
) -> serde_json::Value {
    serde_json::json!({
        "name": format!("dsh-profile-{profile_name}"),
        "private": true,
        "dependencies": pkg_deps_from_coords(deps),
        "dsh": { "profile": { "bundles": bundles } },
    })
}

/// Derives a package name from a github coordinate: the repo basename, or
/// the subpath basename for monorepo plugins.
fn coord_to_pkg_name(coord: &str) -> String {
    let body = coord.trim_start_matches("github:");
    let last = body
        .rsplit(['#', '/'])
        .find(|s| !s.is_empty() && *s != "path:")
        .unwrap_or(body);
    last.to_string()
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// Starts a background task that installs a modpack: it creates a fresh
/// instance with a dedicated DSH_HOME and the pack's profile as its default
/// profile, keeping the `web` profile pristine.
#[tauri::command]
pub async fn start_import_modpack_task(
    app: AppHandle,
    state: State<'_, AppState>,
    input: ImportModpackInput,
) -> Result<String, String> {
    if input.source.trim().is_empty() {
        return Err("整合包来源不能为空".to_string());
    }

    let task = crate::tasks::TaskInfo {
        id: new_id("t"),
        kind: "import-modpack".to_string(),
        label: format!(
            "导入整合包 {}",
            input.instance_name.as_deref().unwrap_or(&input.source)
        ),
        version: String::new(),
        state: crate::tasks::TaskState::Running,
        percent: 0,
        created_at: crate::tasks::now_millis_pub(),
        message: None,
        instance_id: None,
        instance_name: Some(input.source.clone()),
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
        let result = do_import_modpack(&worker_app, &state, &worker_task_id, &input).await;
        let mut tasks = state.tasks.lock().await;
        if let Some(task) = tasks.get_mut(&worker_task_id) {
            if task.state == crate::tasks::TaskState::Cancelled {
                return;
            }
            match result {
                Ok(imported) => {
                    task.state = crate::tasks::TaskState::Done;
                    task.percent = 100;
                    task.message = Some(format!("已导入实例 {imported}"));
                    crate::tasks::emit_progress_pub(
                        &worker_app,
                        &worker_task_id,
                        crate::tasks::TaskState::Done,
                        100,
                        Some(format!("已导入实例 {imported}")),
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

async fn do_import_modpack(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    input: &ImportModpackInput,
) -> Result<String, String> {
    // 1. Obtain the pack locally, detect the container, and extract it.
    let (pack, guard) = fetch_modpack_source(&input.source).await?;
    let tmp = guard.0.clone();
    let unpacked = tmp.join("pack");
    let container = detect_container(&pack)?;
    let marker = match container {
        ModpackContainer::Dspack => {
            let file = std::fs::File::open(&pack).map_err(|e| format!("打开整合包失败: {e}"))?;
            let mut archive =
                zip::ZipArchive::new(file).map_err(|e| format!("解析 .dspack ZIP 失败: {e}"))?;
            let marker = validate_dspack_marker(&mut archive)?;
            extract_modpack_dspack(&pack, &unpacked)?;
            Some(marker)
        }
        ModpackContainer::LegacyTgz => {
            extract_modpack_tgz(&pack, &unpacked)?;
            None
        }
    };
    let manifest_raw = std::fs::read_to_string(unpacked.join("manifest.json"))
        .map_err(|_| "整合包缺少 manifest.json".to_string())?;
    let manifest: ModpackManifest =
        serde_json::from_str(&manifest_raw).map_err(|e| format!("解析 manifest.json 失败: {e}"))?;
    validate_manifest(&manifest)?;
    validate_pack_type(&manifest)?;
    validate_dshhome(&manifest)?;
    if let Some(marker) = marker {
        check_container_manifest(marker, &manifest)?;
    }

    // 1b. manifest v5 dshhome form: a whole-DSH_HOME snapshot follows its
    //     own multi-profile import flow.
    if manifest.pack_type.as_deref() == Some("dshhome") {
        return do_import_dshhome(app, state, task_id, input, &unpacked, &manifest).await;
    }

    // 2. Resolve names. Profile: input override → manifest profileName →
    //    "pack" (keeping `web` clean). Instance: input override → plain-string
    //    displayName → name.
    let profile_name = input
        .profile_name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| manifest.profile_name.clone())
        .unwrap_or_else(|| "pack".to_string());
    let profile_name = crate::config::sanitize_name(&profile_name);
    if profile_name.is_empty() {
        return Err("整合包的 profileName 无效".to_string());
    }
    let instance_name = input
        .instance_name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| {
            manifest
                .display_name
                .as_ref()
                .and_then(|d| d.as_str().map(|s| s.to_string()))
        })
        .unwrap_or_else(|| manifest.name.clone());
    let instance_name = {
        let cfg = state.config.lock().unwrap();
        dedupe_instance_name(&cfg, &instance_name)
    };
    crate::tasks::push_task_log_pub(
        app,
        state,
        task_id,
        &format!(
            "整合包 {} v{} → profile「{}」",
            manifest.name, manifest.version, profile_name
        ),
    )
    .await;

    // 3. Resolve the target: an existing instance (issue #11) or a fresh
    //    dedicated one. The manifest's pinned dshVersion line constrains
    //    both paths.
    let version_str = manifest
        .dsh_version
        .as_deref()
        .map(|v| {
            v.trim()
                .trim_start_matches(['>', '=', '^', '~', ' '])
                .to_string()
        })
        .filter(|v| !v.is_empty());

    let existing_target = match &input.existing_instance_id {
        Some(id) => {
            let cfg = state.config.lock().unwrap();
            let inst = cfg
                .instances
                .iter()
                .find(|i| i.id == *id)
                .cloned()
                .ok_or_else(|| "目标实例不存在".to_string())?;
            let home = cfg
                .homes
                .iter()
                .find(|h| h.id == inst.home_id)
                .cloned()
                .ok_or_else(|| "DSH_HOME 不存在".to_string())?;
            let ver = cfg
                .versions
                .iter()
                .find(|v| v.id == inst.version_id)
                .cloned()
                .ok_or_else(|| "DSH 版本不存在".to_string())?;
            if let Some(want) = &version_str {
                let base = |v: &str| v.split('-').next().unwrap_or(v).to_string();
                if base(&ver.version) != base(want) {
                    return Err(format!(
                        "实例「{}」的 DSH 版本 {} 与整合包要求的 {} 不在同一版本线",
                        inst.name, ver.version, want
                    ));
                }
            }
            Some((inst, home, ver))
        }
        None => None,
    };

    let (version_record, home, target_instance_id) = match existing_target {
        Some((inst, home, ver)) => {
            crate::tasks::push_task_log_pub(
                app,
                state,
                task_id,
                &format!("导入到现有实例「{}」（DSH {}）", inst.name, ver.version),
            )
            .await;
            (ver, home.path, Some(inst.id))
        }
        None => {
            // Fresh instance: resolve the pinned version (exact), falling
            // back to the newest installed version; install if missing.
            let version_record = {
                let cfg = state.config.lock().unwrap();
                match &version_str {
                    Some(v) => cfg.versions.iter().find(|r| r.version == *v).cloned(),
                    None => cfg.versions.last().cloned(),
                }
            };
            let version_record = match version_record {
                Some(v) => v,
                None => match &version_str {
                    Some(v) => {
                        // A pinned base version (e.g. 0.1.0) may have no
                        // published build at all — only prereleases
                        // (0.1.0-rc.8). Substitute the latest available
                        // version of that line.
                        let target = resolve_version_fallback(v).await;
                        if target != *v {
                            crate::tasks::push_task_log_pub(
                                app,
                                state,
                                task_id,
                                &format!(
                                    "{v} 没有正式发行版本，改用该版本线最新的开发版本 {target}"
                                ),
                            )
                            .await;
                        }
                        crate::tasks::push_task_log_pub(
                            app,
                            state,
                            task_id,
                            &format!("整合包需要 DSH {target}，本机未安装，开始安装…"),
                        )
                        .await;
                        crate::tasks::install_version_streamed_pub(app, state, task_id, &target)
                            .await?
                    }
                    None => {
                        return Err(
                            "整合包未声明 dshVersion 且本机没有已安装的 DSH 版本".to_string()
                        );
                    }
                },
            };

            // Dedicated HOME for the new instance (path-based reuse keeps a
            // retry idempotent), then prepare the pristine web template.
            let home_path = state
                .data_dir
                .join("homes")
                .join(crate::config::sanitize_name(&instance_name));
            let home = crate::commands::create_home_record(
                state,
                &instance_name,
                &home_path.to_string_lossy(),
            )?;
            crate::tasks::ensure_web_profile_template_pub(
                app,
                state,
                task_id,
                &home.path,
                &version_record,
            )
            .await?;
            (version_record, home.path, None)
        }
    };

    // 4. Materialize the pack profile directory inside the HOME.
    let dest = crate::plugins::profile_dir_pub(&home, &profile_name);
    if dest.exists() {
        if !input.force {
            return Err(format!("Profile「{profile_name}」已存在，勾选覆盖后重试"));
        }
        std::fs::remove_dir_all(&dest).map_err(|e| format!("清理旧 profile 失败: {e}"))?;
    }
    std::fs::create_dir_all(&dest).map_err(|e| format!("创建 profile 目录失败: {e}"))?;

    let pkg = profile_pkg_json(&profile_name, &manifest.bundles, &manifest.dependencies);
    std::fs::write(
        dest.join("package.json"),
        serde_json::to_vec_pretty(&pkg).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("写入 package.json 失败: {e}"))?;
    // Prefer the pack's own cordis.patch.yml; fall back to the manifest's
    // inline patch (v3).
    if let Ok(patch) = std::fs::read(unpacked.join("cordis.patch.yml")) {
        std::fs::write(dest.join("cordis.patch.yml"), patch)
            .map_err(|e| format!("写入 cordis.patch.yml 失败: {e}"))?;
    } else if let Some(patch) = &manifest.patch {
        std::fs::write(dest.join("cordis.patch.yml"), patch)
            .map_err(|e| format!("写入 cordis.patch.yml 失败: {e}"))?;
    }
    let has_lock = unpacked.join("pnpm-lock.yaml").exists();
    if has_lock {
        std::fs::copy(unpacked.join("pnpm-lock.yaml"), dest.join("pnpm-lock.yaml"))
            .map_err(|e| format!("写入 pnpm-lock.yaml 失败: {e}"))?;
    }
    if let Ok(ws) = std::fs::read(unpacked.join("pnpm-workspace.yaml")) {
        std::fs::write(dest.join("pnpm-workspace.yaml"), ws)
            .map_err(|e| format!("写入 pnpm-workspace.yaml 失败: {e}"))?;
    }

    // 4. Install dependencies (frozen when the pack ships a lockfile).
    if let Err(e) = pnpm_install_profile(app, state, task_id, &dest, has_lock).await {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }

    // 5. pack-structure v2: overrides/ user files land on the profile root
    //    (file-level overwrite, after pnpm's runtime defaults).
    if container == ModpackContainer::Dspack {
        let count = apply_overrides(&unpacked, &dest)?;
        if count > 0 {
            crate::tasks::push_task_log_pub(
                app,
                state,
                task_id,
                &format!("已应用 overrides/ 的 {count} 个文件"),
            )
            .await;
        }
        // pack-structure v3: a profile-form pack may also carry home-level
        // content in `home/` (global skills, .agent-presets, AGENTS.md),
        // copied onto the $DSH_HOME root with the same overwrite semantics.
        let home_dir = unpacked.join("home");
        if home_dir.is_dir() {
            let count = copy_tree(&home_dir, &home, None)?;
            if count > 0 {
                crate::tasks::push_task_log_pub(
                    app,
                    state,
                    task_id,
                    &format!("已应用 home/ 的 {count} 个文件到 DSH_HOME 根目录"),
                )
                .await;
            }
        }
    }

    // 6. manifest v4 files[]: heavy content fetched on demand, each file
    //    verified by sha256 + size; any failure rolls the profile back.
    if !manifest.files.is_empty() {
        if let Err(e) = download_modpack_files(app, state, task_id, &manifest.files, &dest).await {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(e);
        }
    }

    // 7. Register / update the instance with the pack profile as its default.
    let (instance_id, final_instance_name) = if let Some(id) = target_instance_id {
        let mut cfg = state.config.lock().unwrap();
        let inst = cfg
            .instances
            .iter_mut()
            .find(|i| i.id == id)
            .ok_or_else(|| "目标实例不存在".to_string())?;
        inst.default_profile = Some(profile_name.clone());
        let name = inst.name.clone();
        crate::commands::save_state(state, &cfg)?;
        (id, name)
    } else {
        let mut cfg = state.config.lock().unwrap();
        let inst = crate::config::DshInstance {
            id: new_id("i"),
            name: instance_name.clone(),
            version_id: version_record.id.clone(),
            home_id: home_id_of_path(&cfg, &home).ok_or_else(|| "DSH_HOME 记录缺失".to_string())?,
            env_overrides: Default::default(),
            default_profile: Some(profile_name.clone()),
            last_profile: None,
            icon: None,
            port: None,
        };
        cfg.instances.push(inst.clone());
        crate::commands::save_state(state, &cfg)?;
        (inst.id, instance_name.clone())
    };

    // 8. Modpack icon (issue #8): a bundled icon.png becomes the instance's
    //    local icon; an http(s) manifest icon stays a remote reference. An
    //    existing instance keeps an icon it already has.
    let imported_icon: Option<String> = if unpacked.join("icon.png").exists() {
        let dest = crate::icons::local_icon_path(&home, &instance_id);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::copy(unpacked.join("icon.png"), &dest)
            .map(|_| "local".to_string())
            .ok()
    } else {
        manifest
            .icon
            .clone()
            .filter(|i| i.starts_with("https://") || i.starts_with("http://"))
    };
    if let Some(icon) = imported_icon {
        let mut cfg = state.config.lock().unwrap();
        if let Some(inst) = cfg.instances.iter_mut().find(|i| i.id == instance_id) {
            if inst.icon.is_none() {
                inst.icon = Some(icon);
                crate::commands::save_state(state, &cfg)?;
            }
        }
    }

    crate::log_info!(
        "整合包 {} 已导入为实例「{}」（profile「{}」）",
        manifest.name,
        final_instance_name,
        profile_name
    );
    drop(guard);
    Ok(format!("{final_instance_name}（{instance_id}）"))
}

// ---------------------------------------------------------------------------
// Import: manifest v5 dshhome form (whole-DSH_HOME snapshot, issue #24)
// ---------------------------------------------------------------------------

/// Imports a `type:"dshhome"` pack (pack-structure v3 §9): a fresh instance
/// whose HOME mirrors the whole snapshot — per-profile dependency layers,
/// home-level overrides (presets / skills / AGENTS.md / data), and pointer
/// downloads (`files[]` + heavy `skills[]`). Any failure removes the freshly
/// created HOME entirely (all-or-nothing rollback).
async fn do_import_dshhome(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    input: &ImportModpackInput,
    unpacked: &Path,
    manifest: &ModpackManifest,
) -> Result<String, String> {
    if input.existing_instance_id.is_some() {
        return Err("dshhome 形态是整个 DSH_HOME 的快照，只能导入为新实例".to_string());
    }
    let profiles = manifest
        .profiles
        .clone()
        .ok_or_else(|| "dshhome 形态缺少 profiles".to_string())?;
    let default_profile = manifest
        .default_profile
        .clone()
        .ok_or_else(|| "dshhome 形态缺少 defaultProfile".to_string())?;

    let instance_name = input
        .instance_name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| {
            manifest
                .display_name
                .as_ref()
                .and_then(|d| d.as_str().map(|s| s.to_string()))
        })
        .unwrap_or_else(|| manifest.name.clone());
    let instance_name = {
        let cfg = state.config.lock().unwrap();
        dedupe_instance_name(&cfg, &instance_name)
    };
    crate::tasks::push_task_log_pub(
        app,
        state,
        task_id,
        &format!(
            "整合包 {} v{}（dshhome 快照，{} 个 profile，默认「{}」）",
            manifest.name,
            manifest.version,
            profiles.len(),
            default_profile
        ),
    )
    .await;

    // Resolve the pinned DSH version, installing it when missing (same
    // behaviour as the profile form).
    let version_str = manifest
        .dsh_version
        .as_deref()
        .map(|v| {
            v.trim()
                .trim_start_matches(['>', '=', '^', '~', ' '])
                .to_string()
        })
        .filter(|v| !v.is_empty());
    let version_record = {
        let cfg = state.config.lock().unwrap();
        match &version_str {
            Some(v) => cfg.versions.iter().find(|r| r.version == *v).cloned(),
            None => cfg.versions.last().cloned(),
        }
    };
    let version_record = match version_record {
        Some(v) => v,
        None => match &version_str {
            Some(v) => {
                let target = resolve_version_fallback(v).await;
                crate::tasks::push_task_log_pub(
                    app,
                    state,
                    task_id,
                    &format!("整合包需要 DSH {target}，本机未安装，开始安装…"),
                )
                .await;
                crate::tasks::install_version_streamed_pub(app, state, task_id, &target).await?
            }
            None => {
                return Err("整合包未声明 dshVersion 且本机没有已安装的 DSH 版本".to_string());
            }
        },
    };

    // Fresh dedicated HOME; a leftover directory from a failed earlier
    // attempt is wiped so the snapshot starts clean.
    let home_path = state
        .data_dir
        .join("homes")
        .join(crate::config::sanitize_name(&instance_name));
    if home_path.exists() {
        std::fs::remove_dir_all(&home_path).map_err(|e| format!("清理旧 DSH_HOME 失败: {e}"))?;
    }
    std::fs::create_dir_all(&home_path).map_err(|e| format!("创建 DSH_HOME 失败: {e}"))?;
    let home =
        crate::commands::create_home_record(state, &instance_name, &home_path.to_string_lossy())?;

    let result = import_dshhome_body(
        app,
        state,
        task_id,
        &home.path,
        manifest,
        &profiles,
        unpacked,
        &version_record,
    )
    .await;
    if let Err(e) = result {
        // All-or-nothing: drop the half-written snapshot and its record.
        std::fs::remove_dir_all(&home.path).ok();
        let mut cfg = state.config.lock().unwrap();
        cfg.homes.retain(|h| h.id != home.id);
        crate::commands::save_state(state, &cfg).ok();
        return Err(e);
    }

    // Register the instance with the manifest's defaultProfile.
    let instance_id = {
        let mut cfg = state.config.lock().unwrap();
        let inst = crate::config::DshInstance {
            id: new_id("i"),
            name: instance_name.clone(),
            version_id: version_record.id.clone(),
            home_id: home.id.clone(),
            env_overrides: Default::default(),
            default_profile: Some(default_profile.clone()),
            last_profile: None,
            icon: None,
            port: None,
        };
        cfg.instances.push(inst.clone());
        crate::commands::save_state(state, &cfg)?;
        inst.id
    };

    // Icon: bundled icon.png wins; an http(s) manifest icon stays remote.
    let imported_icon: Option<String> = if unpacked.join("icon.png").exists() {
        let dest = crate::icons::local_icon_path(&home.path, &instance_id);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::copy(unpacked.join("icon.png"), &dest)
            .map(|_| "local".to_string())
            .ok()
    } else {
        manifest
            .icon
            .clone()
            .filter(|i| i.starts_with("https://") || i.starts_with("http://"))
    };
    if let Some(icon) = imported_icon {
        let mut cfg = state.config.lock().unwrap();
        if let Some(inst) = cfg.instances.iter_mut().find(|i| i.id == instance_id) {
            inst.icon = Some(icon);
            crate::commands::save_state(state, &cfg)?;
        }
    }

    crate::log_info!(
        "dshhome 整合包 {} 已导入为实例「{}」（默认 profile「{}」）",
        manifest.name,
        instance_name,
        default_profile
    );
    Ok(format!("{instance_name}（{instance_id}）"))
}

/// The body of a dshhome import, separated so the caller can roll the fresh
/// HOME back on any failure.
#[allow(clippy::too_many_arguments)]
async fn import_dshhome_body(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    home: &Path,
    manifest: &ModpackManifest,
    profiles: &BTreeMap<String, ProfileUnit>,
    unpacked: &Path,
    version_record: &crate::config::DshVersion,
) -> Result<(), String> {
    // 1. Baseline web/headless templates first, so pack content wins.
    crate::tasks::ensure_web_profile_template_pub(app, state, task_id, home, version_record)
        .await?;

    // 2. Per profile: overrides/profiles/<name>/ → profile dir, then the
    //    manifest-authoritative package.json, then pnpm install.
    for (name, unit) in profiles {
        let dest = crate::plugins::profile_dir_pub(home, name);
        std::fs::create_dir_all(&dest)
            .map_err(|e| format!("创建 profile「{name}」目录失败: {e}"))?;
        let profile_overrides = unpacked.join("overrides").join("profiles").join(name);
        if profile_overrides.is_dir() {
            let count = copy_tree(&profile_overrides, &dest, None)?;
            if count > 0 {
                crate::tasks::push_task_log_pub(
                    app,
                    state,
                    task_id,
                    &format!("profile「{name}」: 已应用 overrides 的 {count} 个文件"),
                )
                .await;
            }
        }
        // Manifest wins over any copied machine file.
        let pkg = profile_pkg_json(name, &unit.bundles, &unit.dependencies);
        std::fs::write(
            dest.join("package.json"),
            serde_json::to_vec_pretty(&pkg).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("写入 profile「{name}」package.json 失败: {e}"))?;
        if !dest.join("cordis.patch.yml").exists() {
            if let Some(patch) = &unit.patch {
                std::fs::write(dest.join("cordis.patch.yml"), patch)
                    .map_err(|e| format!("写入 profile「{name}」cordis.patch.yml 失败: {e}"))?;
            }
        }
        crate::tasks::push_task_log_pub(
            app,
            state,
            task_id,
            &format!("profile「{name}」: 安装依赖…"),
        )
        .await;
        let has_lock = dest.join("pnpm-lock.yaml").exists();
        pnpm_install_profile(app, state, task_id, &dest, has_lock).await?;
    }

    // 3. Home-level overrides: everything except the per-profile subtrees.
    let count = copy_tree(&unpacked.join("overrides"), home, Some(&["profiles"]))?;
    if count > 0 {
        crate::tasks::push_task_log_pub(
            app,
            state,
            task_id,
            &format!("已应用 home 级 overrides/ 的 {count} 个文件"),
        )
        .await;
    }

    // 4. Pointer downloads into the HOME root: files[] plus heavy skills[]
    //    (small skills already shipped inside overrides/skills/).
    let mut entries: Vec<ModpackFileEntry> = manifest.files.clone();
    if let Some(skills) = &manifest.skills {
        for skill in skills {
            match (&skill.sha256, skill.size) {
                (Some(sha256), Some(size)) if !skill.urls.is_empty() => {
                    entries.push(ModpackFileEntry {
                        path: skill.path.clone(),
                        sha256: sha256.clone(),
                        size,
                        urls: skill.urls.clone(),
                    });
                }
                _ => {} // pure-file skill: carried by overrides/skills/
            }
        }
    }
    if !entries.is_empty() {
        download_modpack_files(app, state, task_id, &entries, home).await?;
    }
    Ok(())
}

/// Finds a free instance name, appending `-2`, `-3`, … when taken.
fn dedupe_instance_name(cfg: &crate::config::Config, base: &str) -> String {
    let base = if base.trim().is_empty() {
        "modpack"
    } else {
        base.trim()
    };
    if !cfg.instances.iter().any(|i| i.name == base) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !cfg.instances.iter().any(|i| i.name == candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// When a pinned base version (say `0.1.0`) was never released as-is, falls
/// back to the newest available version of the same line (e.g. `0.1.0-rc.8`)
/// from npm + GitHub tags. Already-prerelease or exactly-available versions
/// pass through unchanged; network failure also passes through (the install
/// path reports the real error).
async fn resolve_version_fallback(requested: &str) -> String {
    if requested.contains('-') {
        return requested.to_string();
    }
    let Ok(available) = crate::commands::fetch_available_versions().await else {
        return requested.to_string();
    };
    let req_base = requested.split('-').next().unwrap_or(requested);
    let best = available
        .iter()
        .filter_map(|v| {
            let parsed = semver::Version::parse(&v.version).ok()?;
            let base = v.version.split('-').next().unwrap_or(&v.version);
            (base == req_base).then_some(parsed)
        })
        .max();
    match best {
        Some(v) => v.to_string(),
        None => requested.to_string(),
    }
}

/// Downloads and square-crops a remote icon for bundling; `None` on failure
/// (the exporter then falls back to referencing the URL).
async fn fetch_remote_icon(url: &str) -> Option<Vec<u8>> {
    match crate::icons::fetch_square_icon_png(url).await {
        Ok(png) => Some(png),
        Err(e) => {
            crate::log_warn!("导出整合包：下载实例图标失败 {url}: {e}");
            None
        }
    }
}

/// HOME id for a path (the dedicated HOME was just created or reused).
fn home_id_of_path(cfg: &crate::config::Config, path: &Path) -> Option<String> {
    cfg.homes
        .iter()
        .find(|h| crate::config::paths_equal(&h.path, path))
        .map(|h| h.id.clone())
}

/// Downloads a modpack URL to `target` with a size cap.
async fn download_modpack(url: &str, target: &Path) -> Result<(), String> {
    let client = crate::proxy::apply(reqwest::Client::builder())
        .timeout(std::time::Duration::from_secs(300))
        .user_agent("dsh-launcher")
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("下载整合包失败 {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载整合包失败 {url}: HTTP {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取整合包失败: {e}"))?;
    if bytes.len() > MODPACK_MAX_BYTES {
        return Err("整合包过大（超过 64 MiB）".to_string());
    }
    std::fs::write(target, &bytes).map_err(|e| format!("保存整合包失败: {e}"))?;
    Ok(())
}

/// Best-effort temp dir cleanup.
struct TmpDir(PathBuf);

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_repo_from_spec_parses_common_forms() {
        assert_eq!(
            github_repo_from_spec("git+https://github.com/DViridescent/dafy-whale-theme.git"),
            Some(("DViridescent/dafy-whale-theme".to_string(), None, None))
        );
        assert_eq!(
            github_repo_from_spec("github:owner/repo#path:/packages/x"),
            Some((
                "owner/repo".to_string(),
                Some("packages/x".to_string()),
                None
            ))
        );
        assert_eq!(
            github_repo_from_spec("https://github.com/owner/repo#abc1234&path:sub/dir"),
            Some((
                "owner/repo".to_string(),
                Some("sub/dir".to_string()),
                Some("abc1234".to_string())
            ))
        );
        assert_eq!(github_repo_from_spec("^1.2.3"), None);
    }

    #[test]
    fn locked_git_commit_reads_importers() {
        let lock = r#"
lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      dafy-whale-theme:
        specifier: github:DViridescent/dafy-whale-theme
        version: github.com/DViridescent/dafy-whale-theme/99e8c57654f2c6394d515589a16b2a2a15c0a5f1
"#;
        assert_eq!(
            locked_git_commit(lock, "dafy-whale-theme"),
            Some("99e8c57654f2c6394d515589a16b2a2a15c0a5f1".to_string())
        );
        assert_eq!(locked_git_commit(lock, "missing"), None);
    }

    #[test]
    fn manifest_pkg_deps_converts_v3_coords() {
        let manifest = ModpackManifest {
            manifest_version: 3,
            pack_type: None,
            name: "x".to_string(),
            display_name: None,
            version: "1.0.0".to_string(),
            description: None,
            author: None,
            icon: None,
            dsh_version: None,
            profile_name: None,
            bundles: vec![],
            dependencies: BTreeMap::from([
                ("github:owner/repo".to_string(), "abc1234".to_string()),
                (
                    "github:owner/mono#path:/pkg".to_string(),
                    "def5678".to_string(),
                ),
                ("dsh-pet".to_string(), "0.2.0".to_string()),
            ]),
            patch: None,
            files: vec![],
            default_profile: None,
            profiles: None,
            presets: None,
            skills: None,
            instructions: None,
        };
        let deps = manifest_pkg_deps(&manifest);
        assert_eq!(deps["repo"], "github:owner/repo#abc1234");
        assert_eq!(deps["pkg"], "github:owner/mono#def5678&path:pkg");
        assert_eq!(deps["dsh-pet"], "0.2.0");
    }

    #[test]
    fn dspack_round_trip() {
        let dir = std::env::temp_dir().join(format!("dsh-modpack-test-{}", uuid::Uuid::new_v4()));
        let files: Vec<(String, Vec<u8>)> = vec![
            ("dspack.json".to_string(), DSPACK_MARKER.as_bytes().to_vec()),
            ("manifest.json".to_string(), b"{}".to_vec()),
            ("overrides/cordis.patch.yml".to_string(), b"[]".to_vec()),
        ];
        let dspack = write_modpack_dspack(&dir.join("x-1.0.0.dspack"), &files).unwrap();
        assert_eq!(detect_container(&dspack).unwrap(), ModpackContainer::Dspack);
        let out = dir.join("out");
        extract_modpack_dspack(&dspack, &out).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("manifest.json")).unwrap(),
            "{}"
        );
        assert_eq!(
            std::fs::read_to_string(out.join("overrides/cordis.patch.yml")).unwrap(),
            "[]"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dspack_marker_is_required() {
        let dir = std::env::temp_dir().join(format!("dsh-modpack-test-{}", uuid::Uuid::new_v4()));
        // A plain ZIP without dspack.json must be rejected as a dspack.
        let files: Vec<(String, Vec<u8>)> = vec![("manifest.json".to_string(), b"{}".to_vec())];
        let zip_path = write_modpack_dspack(&dir.join("plain.zip"), &files).unwrap();
        assert_eq!(
            detect_container(&zip_path).unwrap(),
            ModpackContainer::Dspack
        );
        let err = extract_modpack_dspack(&zip_path, &dir.join("out")).unwrap_err();
        assert!(err.contains("dspack.json"), "unexpected error: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pack_type_validation() {
        let mut manifest = ModpackManifest {
            manifest_version: 4,
            pack_type: None,
            name: "x".to_string(),
            display_name: None,
            version: "1.0.0".to_string(),
            description: None,
            author: None,
            icon: None,
            dsh_version: None,
            profile_name: None,
            bundles: vec![],
            dependencies: BTreeMap::new(),
            patch: None,
            files: vec![],
            default_profile: None,
            profiles: None,
            presets: None,
            skills: None,
            instructions: None,
        };
        assert!(validate_pack_type(&manifest).is_ok());
        manifest.pack_type = Some("profile".to_string());
        assert!(validate_pack_type(&manifest).is_ok());
        manifest.pack_type = Some("collection".to_string());
        assert!(validate_pack_type(&manifest)
            .unwrap_err()
            .contains("暂未支持"));
        manifest.pack_type = Some("wat".to_string());
        assert!(validate_pack_type(&manifest).is_err());
    }

    #[test]
    fn files_target_rejects_traversal() {
        let profile = Path::new("/profile");
        assert!(files_target(profile, "data/models/x.bin").is_ok());
        assert!(files_target(profile, "../escape").is_err());
        assert!(files_target(profile, "/absolute").is_err());
        assert!(files_target(profile, "").is_err());
    }

    /// A valid manifest v5 dshhome baseline for shape tests.
    fn dshhome_manifest() -> ModpackManifest {
        ModpackManifest {
            manifest_version: 5,
            pack_type: Some("dshhome".to_string()),
            name: "x".to_string(),
            display_name: None,
            version: "1.0.0".to_string(),
            description: None,
            author: None,
            icon: None,
            dsh_version: None,
            profile_name: None,
            bundles: vec![],
            dependencies: BTreeMap::new(),
            patch: None,
            files: vec![],
            default_profile: Some("main".to_string()),
            profiles: Some(BTreeMap::from([(
                "main".to_string(),
                ProfileUnit {
                    bundles: vec!["@deepseek-ai/dsh-base".to_string()],
                    dependencies: BTreeMap::new(),
                    patch: None,
                },
            )])),
            presets: None,
            skills: None,
            instructions: None,
        }
    }

    #[test]
    fn dshhome_type_requires_v5() {
        let mut m = dshhome_manifest();
        m.manifest_version = 4;
        assert!(validate_pack_type(&m)
            .unwrap_err()
            .contains("manifestVersion 5"));
        m.manifest_version = 5;
        assert!(validate_pack_type(&m).is_ok());
    }

    #[test]
    fn dshhome_shape_validation() {
        let ok = dshhome_manifest();
        assert!(validate_dshhome(&ok).is_ok());

        let mut empty = dshhome_manifest();
        empty.profiles = Some(BTreeMap::new());
        assert!(validate_dshhome(&empty).unwrap_err().contains("至少一个"));

        let mut missing = dshhome_manifest();
        missing.profiles = None;
        assert!(validate_dshhome(&missing).is_err());

        let mut web = dshhome_manifest();
        web.profiles.as_mut().unwrap().insert(
            "web".to_string(),
            ProfileUnit {
                bundles: vec!["b".to_string()],
                dependencies: BTreeMap::new(),
                patch: None,
            },
        );
        assert!(validate_dshhome(&web).unwrap_err().contains("web"));

        let mut bad_default = dshhome_manifest();
        bad_default.default_profile = Some("nope".to_string());
        assert!(validate_dshhome(&bad_default).unwrap_err().contains("nope"));

        let mut no_bundles = dshhome_manifest();
        no_bundles
            .profiles
            .as_mut()
            .unwrap()
            .get_mut("main")
            .unwrap()
            .bundles = vec![];
        assert!(validate_dshhome(&no_bundles)
            .unwrap_err()
            .contains("bundles"));
    }

    #[test]
    fn container_manifest_pairing() {
        let mut m = dshhome_manifest();
        // manifest v5 needs the v3 container.
        assert!(check_container_manifest(2, &m).is_err());
        assert!(check_container_manifest(3, &m).is_ok());
        // A dspack never carries a pre-v4 manifest.
        m.manifest_version = 3;
        m.pack_type = None;
        assert!(check_container_manifest(3, &m).is_err());
        // v4 manifest in either container is fine.
        m.manifest_version = 4;
        assert!(check_container_manifest(2, &m).is_ok());
        assert!(check_container_manifest(3, &m).is_ok());
    }

    #[test]
    fn dspack_v3_marker_accepted() {
        let dir = std::env::temp_dir().join(format!("dsh-modpack-test-{}", uuid::Uuid::new_v4()));
        let files: Vec<(String, Vec<u8>)> = vec![
            (
                "dspack.json".to_string(),
                br#"{"format":"dspack","version":3}"#.to_vec(),
            ),
            (
                "manifest.json".to_string(),
                serde_json::to_vec(&dshhome_manifest()).unwrap(),
            ),
        ];
        let dspack = write_modpack_dspack(&dir.join("x-1.0.0.dspack"), &files).unwrap();
        let manifest = read_manifest_from_dspack(&dspack).unwrap();
        assert_eq!(manifest.manifest_version, 5);
        assert_eq!(manifest.pack_type.as_deref(), Some("dshhome"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn copy_tree_skip_top_excludes_subtree() {
        let dir = std::env::temp_dir().join(format!("dsh-modpack-test-{}", uuid::Uuid::new_v4()));
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("profiles/main")).unwrap();
        std::fs::create_dir_all(src.join("skills/x")).unwrap();
        std::fs::write(src.join("AGENTS.md"), "a").unwrap();
        std::fs::write(src.join("profiles/main/cordis.patch.yml"), "p").unwrap();
        std::fs::write(src.join("skills/x/SKILL.md"), "s").unwrap();
        let dst = dir.join("dst");
        let count = copy_tree(&src, &dst, Some(&["profiles"])).unwrap();
        assert_eq!(count, 2);
        assert!(dst.join("AGENTS.md").exists());
        assert!(dst.join("skills/x/SKILL.md").exists());
        assert!(!dst.join("profiles/main/cordis.patch.yml").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
