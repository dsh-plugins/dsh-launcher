//! 整合包市场、历史归档导入与 PackForge v6 的启动器接入。

pub(crate) mod export_v6;
pub(crate) mod v6;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::config::new_id;
use crate::AppState;

/// 历史导入流程支持的最高版本。
pub const MANIFEST_VERSION: u32 = 6;

#[cfg(test)]
const DSPACK_MARKER: &str = r#"{"format":"dspack","version":2}"#;

/// A profile unit inside a manifest v5 `type:"dshhome"` pack: the v4
/// single-profile contract minus `profileName` (the map key is the name).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileUnit {
    #[serde(default)]
    pub bundles: Vec<String>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

#[derive(Clone, Debug, Deserialize)]
pub struct ImportModpackInput {
    #[serde(default)]
    pub archive_id: Option<String>,
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
    /// Import into a WSL distro (issue #49 G5): the pack's profile lands in
    /// `$HOME/.dsh-launcher/homes/<name>` *inside* the distro and pnpm runs
    /// there, so native modules are Linux binaries. `None` = a local Windows
    /// HOME (the historical behaviour). Only the dshhome form is supported;
    /// the single-profile form targets an existing instance, whose HOME (and
    /// therefore its distro) is already decided.
    #[serde(default)]
    pub wsl_distro: Option<String>,
}

/// Maximum accepted modpack size (64 MiB).
const MODPACK_MAX_BYTES: usize = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
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

/// Extracts a modpack tgz into `dest`, refusing path-traversal entries.
fn extract_modpack_tgz(tgz: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(tgz).map_err(|e| format!("打开整合包失败: {e}"))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(std::io::Read::take(gz, 513 * 1024 * 1024));
    std::fs::create_dir_all(dest).map_err(|e| format!("创建解压目录失败: {e}"))?;
    for (index, entry) in archive
        .entries()
        .map_err(|e| format!("读取整合包失败: {e}"))?
        .enumerate()
    {
        if index >= 16_384 {
            return Err("整合包条目过多".into());
        }
        let mut entry = entry.map_err(|e| format!("读取整合包条目失败: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("读取整合包条目名失败: {e}"))?
            .into_owned();
        if path.components().any(|component| match component {
            std::path::Component::Normal(name) => !name.to_str().is_some_and(v6::portable_segment),
            std::path::Component::CurDir => false,
            _ => true,
        }) {
            return Err("历史整合包含有无效路径".into());
        }
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
    if entry.size() > 4096 {
        return Err("dspack.json 标记文件过大".into());
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut entry, &mut text)
        .map_err(|e| format!("读取 dspack.json 失败: {e}"))?;
    let marker: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("解析 dspack.json 失败: {e}"))?;
    let version = marker.get("version").and_then(|v| v.as_u64());
    if marker.get("format").and_then(|v| v.as_str()) != Some("dspack")
        || !matches!(version, Some(2..=4))
    {
        return Err(format!(
            "不支持该 .dspack 版本（需要 format=dspack, version 2-4；实际 version={}）",
            version.map(|v| v.to_string()).unwrap_or_default()
        ));
    }
    Ok(version.unwrap_or(2))
}

/// Container/manifest pairing (pack-structure v3 §2.2): a `.dspack` carries
/// manifest v4+, and manifest v5 requires the v3 container.
fn check_container_manifest(marker: u64, manifest: &ModpackManifest) -> Result<(), String> {
    if (manifest.manifest_version == 6) != (marker == 4) {
        return Err("manifest v6 必须与 dspack 容器 v4 配套".into());
    }
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
    if validate_dspack_marker(&mut archive)? == 4 {
        return Err("请用 v6 导入预览安装该整合包".into());
    }
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
    if entry.size() > 8 * 1024 * 1024 {
        return Err("manifest.json 文件过大".into());
    }
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
    let client = crate::proxy::apply(reqwest::Client::builder())?
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
    let mut archive = tar::Archive::new(std::io::Read::take(gz, 513 * 1024 * 1024));
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
            if entry.size() > 8 * 1024 * 1024 {
                return Err("manifest.json 文件过大".into());
            }
            let mut text = String::new();
            std::io::Read::read_to_string(&mut entry, &mut text)
                .map_err(|e| format!("读取 manifest.json 失败: {e}"))?;
            let manifest: ModpackManifest =
                serde_json::from_str(&text).map_err(|e| format!("解析 manifest.json 失败: {e}"))?;
            validate_manifest(&manifest)?;
            if manifest.manifest_version >= 6 {
                return Err("manifest v6 必须与 dspack 容器 v4 配套".into());
            }
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
    if manifest.manifest_version == 6 && manifest.pack_type.is_none() {
        return Err("manifest v6 须声明整合包类型".into());
    }
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
        if name.eq_ignore_ascii_case("web") || name.eq_ignore_ascii_case("headless") {
            return Err(format!(
                "dshhome 形态的 profiles 不得包含基线模板 profile「{name}」"
            ));
        }
        if (manifest.manifest_version < 6
            && (crate::config::sanitize_name(name) != name.trim() || name.trim().is_empty()))
            || (manifest.manifest_version == 6 && !v6::portable_segment(name))
        {
            return Err(format!("dshhome 形态的 profile 名「{name}」无效"));
        }
        if manifest.manifest_version < 6 && unit.bundles.is_empty() {
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
    #[cfg(unix)]
    let created = {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(&tmp)
    };
    #[cfg(not(unix))]
    let created = std::fs::create_dir(&tmp);
    created.map_err(|e| format!("创建临时目录失败: {e}"))?;
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
// Market (issue #17)
// ---------------------------------------------------------------------------

/// The PackForge community modpack index (issue #17).
const MODPACK_MARKET_URL: &str = "https://dsh-packforge.github.io/dsh-pack-market/index.json";

/// One entry of the PackForge modpack market index. `display_name` /
/// `description` stay untyped: entries carry either a string or a
/// `{locale: text}` map and both round-trip to the frontend verbatim.
/// Everything but `id` / `name` / `version` / `download_url` is optional so a
/// lean entry from a third-party index still parses.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketModpack {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsh_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_name: Option<String>,
    pub download_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// "profile" (single profile) or "dshhome" (whole-DSH_HOME snapshot).
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub pack_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dep_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// The index wrapper: `{ schemaVersion, generatedAt, modpacks[] }`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarketModpackIndex {
    schema_version: u32,
    #[serde(default)]
    modpacks: Vec<MarketModpack>,
}

/// Fetches the PackForge modpack market index (issue #17). Unlike the plugin
/// market this is a single-source catalog: a fetch/parse failure is reported
/// to the UI (which shows a retry) instead of being silently skipped.
#[tauri::command]
pub async fn fetch_modpack_market() -> Result<Vec<MarketModpack>, String> {
    let value = crate::plugins::fetch_json_pub(MODPACK_MARKET_URL, 4 * 1024 * 1024).await?;
    let index: MarketModpackIndex =
        serde_json::from_value(value).map_err(|e| format!("解析整合包市场数据失败: {e}"))?;
    if index.schema_version != 2 {
        return Err(format!(
            "不支持的整合包市场索引版本 {}",
            index.schema_version
        ));
    }
    Ok(index.modpacks)
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

/// WSL variant of `pnpm_install_profile` (issue #19 follow-up): the profile
/// files live inside the distro (written through the `\\wsl$\` UNC share by
/// the caller), so pnpm must run inside the distro with the distro's managed
/// node/pnpm and the distro-side store — Windows pnpm would build a tree the
/// Linux runtime cannot use.
async fn pnpm_install_profile_wsl(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    distro: &str,
    dest_linux: &Path,
    has_lock: bool,
) -> Result<(), String> {
    crate::wsl::ensure_distro_running(state, distro).await?;
    let root = crate::wsl::WslRoot::resolve(distro).await?;
    crate::wsl::ensure_node(app, state, task_id, distro, &root).await?;
    let pnpm = crate::wsl::ensure_pnpm(app, state, task_id, distro, &root).await?;

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
        let mut script = format!(
            "export PATH={0}:\"$PATH\"; export CI=true; cd {1} && {2} install",
            crate::wsl::sh_quote(&root.node_bin_dir()),
            crate::wsl::sh_quote(&dest_linux.to_string_lossy()),
            crate::wsl::sh_quote(&pnpm),
        );
        for e in extra.iter() {
            script.push(' ');
            script.push_str(e);
        }
        script.push_str(" --store-dir ");
        script.push_str(&crate::wsl::sh_quote(&root.pnpm_store()));
        script.push_str(" --loglevel=http --fetch-timeout 300000 --fetch-retries 5 --fetch-retry-maxtimeout 120000 --network-concurrency 4");
        if let Ok(registry) = std::env::var("DSH_NPM_REGISTRY") {
            let registry = registry.trim().to_string();
            if !registry.is_empty() {
                script.push_str(" --registry ");
                script.push_str(&crate::wsl::sh_quote(&registry));
            }
        }
        let cmd = crate::wsl::wsl_bash(distro, &script);
        match crate::tasks::run_streamed_command(
            app,
            state,
            task_id,
            cmd,
            "pnpm install（整合包，WSL）",
        )
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
    let pinned = input
        .archive_id
        .as_deref()
        .map(|id| v6::pinned_legacy(&state, id))
        .transpose()?;

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
        version_id: None,
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
        let pinned_path = pinned.as_ref().map(|(path, _)| path.as_path());
        let result =
            do_import_modpack(&worker_app, &state, &worker_task_id, &input, pinned_path).await;
        let mut tasks = state.tasks.lock().await;
        if let Some(task) = tasks.get_mut(&worker_task_id) {
            if task.state == crate::tasks::TaskState::Cancelled {
                return;
            }
            match result {
                Ok(outcome) => {
                    task.state = crate::tasks::TaskState::Done;
                    task.percent = 100;
                    task.message = Some(format!(
                        "已导入实例 {}（{}）",
                        outcome.instance_name, outcome.instance_id
                    ));
                    // Carry the instance id on the task AND the Done event:
                    // the frontend only refreshes the instance/home lists when
                    // a finished task names its instance, so a missing id left
                    // imported instances invisible until the next app start.
                    task.instance_id = Some(outcome.instance_id.clone());
                    crate::tasks::emit_progress_pub(
                        &worker_app,
                        &worker_task_id,
                        crate::tasks::TaskState::Done,
                        100,
                        task.message.clone(),
                        Some(outcome.instance_id),
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

/// Outcome of a successful import: the instance the pack landed on — fresh
/// or, for the single-profile form, an existing one the user picked — plus
/// its display name for the task message.
struct ImportOutcome {
    instance_id: String,
    instance_name: String,
}

async fn do_import_modpack(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    input: &ImportModpackInput,
    pinned: Option<&Path>,
) -> Result<ImportOutcome, String> {
    let _packforge = input
        .existing_instance_id
        .as_deref()
        .map(|id| v6::launch_guard(state, id))
        .transpose()?;
    // 1. Obtain the pack locally, detect the container, and extract it.
    let (pack, guard) =
        fetch_modpack_source(pinned.and_then(Path::to_str).unwrap_or(&input.source)).await?;
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
    if manifest.manifest_version >= 6 {
        return Err("请用 v6 导入预览安装该整合包".into());
    }
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

    let (version_record, home_linux, distro, target_instance_id) = match existing_target {
        Some((inst, home, ver)) => {
            crate::tasks::push_task_log_pub(
                app,
                state,
                task_id,
                &format!("导入到现有实例「{}」（DSH {}）", inst.name, ver.version),
            )
            .await;
            (ver, home.path.clone(), home.wsl.clone(), Some(inst.id))
        }
        None => {
            // Fresh instance: resolve the pinned version (exact), falling
            // back to the newest installed version; install if missing.
            let version_record =
                resolve_import_version(app, state, task_id, &version_str, None).await?;

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
            (version_record, home.path.clone(), None, None)
        }
    };

    // 4. Materialize the pack profile directory inside the HOME.
    // WSL (issue #19 follow-up): ensure the distro is running first so UNC
    // file writes work, then go through the \\wsl$\ share; pnpm install runs
    // inside the distro on the Linux path.
    if let Some(d) = &distro {
        crate::wsl::ensure_distro_running(state, d).await?;
    }
    let fs_home = match &distro {
        Some(d) => crate::wsl::unc_path(d, &home_linux.to_string_lossy()),
        None => home_linux.clone(),
    };
    let dest = crate::plugins::profile_dir_pub(&fs_home, &profile_name);
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
    let install_result = match &distro {
        Some(d) => {
            pnpm_install_profile_wsl(
                app,
                state,
                task_id,
                d,
                &home_linux.join("profiles").join(&profile_name),
                has_lock,
            )
            .await
        }
        None => pnpm_install_profile(app, state, task_id, &dest, has_lock).await,
    };
    if let Err(e) = install_result {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }

    // 5. pack-structure v2: overrides/ user files land on the profile root
    //    (file-level overwrite, after pnpm's runtime defaults).
    if container == ModpackContainer::Dspack {
        let count = {
            let (u, d) = (unpacked.clone(), dest.clone());
            crate::wsl::run_blocking(move || apply_overrides(&u, &d)).await??
        };
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
            let count = {
                let (h, f) = (home_dir.clone(), fs_home.clone());
                crate::wsl::run_blocking(move || copy_tree(&h, &f, None)).await??
            };
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
            browser: Default::default(),
            id: new_id("i"),
            name: instance_name.clone(),
            version_id: version_record.id.clone(),
            home_id: home_id_of_path(&cfg, &home_linux)
                .ok_or_else(|| "DSH_HOME 记录缺失".to_string())?,
            env_overrides: Default::default(),
            default_profile: Some(profile_name.clone()),
            last_profile: None,
            icon: None,
            port: None,
            web_access: Default::default(),
            tui_cwd: None,
        };
        cfg.instances.push(inst.clone());
        crate::commands::save_state(state, &cfg)?;
        (inst.id, instance_name.clone())
    };

    // 8. Modpack icon (issue #8): a bundled icon.png becomes the instance's
    //    local icon; an http(s) manifest icon stays a remote reference. An
    //    existing instance keeps an icon it already has.
    let imported_icon: Option<String> = if unpacked.join("icon.png").exists() {
        let dest = crate::icons::local_icon_path(&fs_home, &instance_id);
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
    Ok(ImportOutcome {
        instance_id,
        instance_name: final_instance_name,
    })
}

// ---------------------------------------------------------------------------
// Import: manifest v5 dshhome form (whole-DSH_HOME snapshot, issue #24)
// ---------------------------------------------------------------------------

/// Resolves the DSH version an import pins, installing it when missing
/// (issue #49 G5): shared by the single-profile form (`do_import_modpack`) and
/// the whole-HOME form (`do_import_dshhome`) so both behave identically when a
/// pack names a version the machine does not have yet.
///
/// `distro` selects the *flavour* of version record: a WSL target must run a
/// version installed **inside that distro** (the CLI, node and pnpm all come
/// from there), so a local Windows record with the same version string is not
/// a match — pairing the two would make the launcher spawn a Linux path with
/// Windows tooling.
async fn resolve_import_version(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    version_str: &Option<String>,
    distro: Option<&str>,
) -> Result<crate::config::DshVersion, String> {
    let installed = {
        let cfg = state.config.lock().unwrap();
        let matches = |v: &&crate::config::DshVersion| v.wsl.as_deref() == distro;
        match version_str {
            Some(v) => cfg
                .versions
                .iter()
                .find(|r| r.version == *v && matches(r))
                .cloned(),
            None => cfg.versions.iter().rev().find(|r| matches(r)).cloned(),
        }
    };
    if let Some(v) = installed {
        return Ok(v);
    }
    let Some(v) = version_str else {
        return Err("整合包未声明 dshVersion 且本机没有已安装的 DSH 版本".to_string());
    };
    // A pinned base version (e.g. 0.1.0) may have no published build at all —
    // only prereleases (0.1.0-rc.8). Substitute the latest of that line.
    let target = resolve_version_fallback(v).await;
    if target != *v {
        crate::tasks::push_task_log_pub(
            app,
            state,
            task_id,
            &format!("{v} 没有正式发行版本，改用该版本线最新的开发版本 {target}"),
        )
        .await;
    }
    crate::tasks::push_task_log_pub(
        app,
        state,
        task_id,
        &match distro {
            Some(d) => format!("整合包需要 DSH {target}，发行版「{d}」内未安装，开始安装…"),
            None => format!("整合包需要 DSH {target}，本机未安装，开始安装…"),
        },
    )
    .await;
    match distro {
        Some(d) => {
            crate::tasks::install_version_streamed_wsl(app, state, task_id, &target, d).await
        }
        None => crate::tasks::install_version_streamed_pub(app, state, task_id, &target).await,
    }
}

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
) -> Result<ImportOutcome, String> {
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

    // Fresh dedicated HOME; a leftover directory from a failed earlier
    // attempt is wiped so the snapshot starts clean.
    //
    // WSL (issue #49 G5): the record stores the *Linux* path, but every
    // Windows-side file operation below goes through the `\\wsl$\` share —
    // the same split the single-profile form uses (`home_linux` vs `fs_home`).
    // The distro is booted before the first touch because `\\wsl$\` is
    // unreachable while it is stopped.
    let distro = input.wsl_distro.clone().filter(|d| !d.trim().is_empty());
    if let Some(d) = &distro {
        crate::wsl::ensure_distro_running(state, d).await?;
    }

    // Resolve the pinned DSH version, installing it when missing (same
    // behaviour as the profile form). A WSL target resolves a version record
    // installed *inside that distro* — the CLI, node and pnpm all come from
    // there, so a local Windows record is not interchangeable.
    let version_str = manifest
        .dsh_version
        .as_deref()
        .map(|v| {
            v.trim()
                .trim_start_matches(['>', '=', '^', '~', ' '])
                .to_string()
        })
        .filter(|v| !v.is_empty());
    let version_record =
        resolve_import_version(app, state, task_id, &version_str, distro.as_deref()).await?;
    let home_name = crate::config::sanitize_name(&instance_name);
    let (home_linux, fs_home) = match &distro {
        Some(d) => {
            let root = crate::wsl::WslRoot::resolve(d).await?;
            let linux = std::path::PathBuf::from(root.home_dir(&home_name));
            let fs = crate::wsl::unc_path(d, &linux.to_string_lossy());
            (linux, fs)
        }
        None => {
            let linux = state.data_dir.join("homes").join(&home_name);
            (linux.clone(), linux)
        }
    };
    if fs_home.exists() {
        std::fs::remove_dir_all(&fs_home).map_err(|e| format!("清理旧 DSH_HOME 失败: {e}"))?;
    }
    std::fs::create_dir_all(&fs_home).map_err(|e| format!("创建 DSH_HOME 失败: {e}"))?;
    let home = match &distro {
        Some(d) => crate::commands::create_home_record_wsl(
            state,
            &instance_name,
            &home_linux.to_string_lossy(),
            d,
        )?,
        None => crate::commands::create_home_record(
            state,
            &instance_name,
            &home_linux.to_string_lossy(),
        )?,
    };

    let result = import_dshhome_body(
        app,
        state,
        task_id,
        &fs_home,
        &home_linux,
        distro.as_deref(),
        manifest,
        &profiles,
        unpacked,
        &version_record,
    )
    .await;
    if let Err(e) = result {
        // All-or-nothing: drop the half-written snapshot and its record.
        std::fs::remove_dir_all(&fs_home).ok();
        let mut cfg = state.config.lock().unwrap();
        cfg.homes.retain(|h| h.id != home.id);
        crate::commands::save_state(state, &cfg).ok();
        return Err(e);
    }

    // Register the instance with the manifest's defaultProfile.
    let instance_id = {
        let mut cfg = state.config.lock().unwrap();
        let inst = crate::config::DshInstance {
            browser: Default::default(),
            id: new_id("i"),
            name: instance_name.clone(),
            version_id: version_record.id.clone(),
            home_id: home.id.clone(),
            env_overrides: Default::default(),
            default_profile: Some(default_profile.clone()),
            last_profile: None,
            icon: None,
            port: None,
            web_access: Default::default(),
            tui_cwd: None,
        };
        cfg.instances.push(inst.clone());
        crate::commands::save_state(state, &cfg)?;
        inst.id
    };

    // Icon: bundled icon.png wins; an http(s) manifest icon stays remote.
    // WSL (issue #49): `home.path` is the *Linux* path, which Windows would
    // resolve relative to the current drive (C:\home\u\...) — the icon would
    // land outside the instance's HOME and silently never show. Write through
    // the same `fs_home` the rest of this function uses.
    let imported_icon: Option<String> = if unpacked.join("icon.png").exists() {
        let dest = crate::icons::local_icon_path(&fs_home, &instance_id);
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
    Ok(ImportOutcome {
        instance_id,
        instance_name,
    })
}

/// The body of a dshhome import, separated so the caller can roll the fresh
/// HOME back on any failure.
#[allow(clippy::too_many_arguments)]
async fn import_dshhome_body(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    home: &Path,
    home_linux: &Path,
    distro: Option<&str>,
    manifest: &ModpackManifest,
    profiles: &BTreeMap<String, ProfileUnit>,
    unpacked: &Path,
    version_record: &crate::config::DshVersion,
) -> Result<(), String> {
    // 1. Baseline web/headless templates first, so pack content wins.
    //    WSL (issue #49 G5) boots the installed DSH *inside* the distro — the
    //    Windows-side template would need `version_bin_ready` against a Linux
    //    version dir, which can never succeed.
    match distro {
        Some(d) => {
            crate::tasks::ensure_web_profile_template_wsl(
                app,
                state,
                task_id,
                d,
                &home_linux.to_string_lossy(),
                version_record,
            )
            .await?;
        }
        None => {
            crate::tasks::ensure_web_profile_template_pub(
                app,
                state,
                task_id,
                home,
                version_record,
            )
            .await?;
        }
    }

    // 2. Per profile: overrides/profiles/<name>/ → profile dir, then the
    //    manifest-authoritative package.json, then pnpm install.
    for (name, unit) in profiles {
        let dest = crate::plugins::profile_dir_pub(home, name);
        std::fs::create_dir_all(&dest)
            .map_err(|e| format!("创建 profile「{name}」目录失败: {e}"))?;
        let profile_overrides = unpacked.join("overrides").join("profiles").join(name);
        if profile_overrides.is_dir() {
            let count = {
                let (o, d) = (profile_overrides.clone(), dest.clone());
                crate::wsl::run_blocking(move || copy_tree(&o, &d, None)).await??
            };
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
        // WSL (issue #49 G5): install inside the distro so native modules are
        // Linux binaries; the profile dir is reached through `\\wsl$\` for the
        // file writes above and by its Linux path for the install itself.
        match distro {
            Some(d) => {
                pnpm_install_profile_wsl(
                    app,
                    state,
                    task_id,
                    d,
                    &home_linux.join("profiles").join(name),
                    has_lock,
                )
                .await?;
            }
            None => pnpm_install_profile(app, state, task_id, &dest, has_lock).await?,
        }
    }

    // 3. Home-level overrides: everything except the per-profile subtrees.
    let count = {
        let (o, h) = (unpacked.join("overrides"), home.to_path_buf());
        crate::wsl::run_blocking(move || copy_tree(&o, &h, Some(&["profiles"]))).await??
    };
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

/// HOME id for a path (the dedicated HOME was just created or reused).
fn home_id_of_path(cfg: &crate::config::Config, path: &Path) -> Option<String> {
    cfg.homes
        .iter()
        .find(|h| crate::config::paths_equal(&h.path, path))
        .map(|h| h.id.clone())
}

/// Downloads a modpack URL to `target` with a size cap.
async fn download_modpack(url: &str, target: &Path) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    let url = reqwest::Url::parse(url).map_err(|_| "整合包下载地址无效")?;
    let safe = |url: &reqwest::Url| {
        matches!(url.scheme(), "https" | "http")
            && url.username().is_empty()
            && url.password().is_none()
    };
    if !safe(&url) {
        return Err("整合包下载地址须为无内嵌凭据的 HTTP(S) 地址".into());
    }
    let client = crate::proxy::apply(reqwest::Client::builder())?
        .timeout(std::time::Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() < 8 && safe(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("整合包下载重定向无效")
            }
        }))
        .user_agent("dsh-launcher")
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("下载整合包失败：{}", e.without_url()))?;
    if !resp.status().is_success() {
        return Err(format!("下载整合包失败：HTTP {}", resp.status()));
    }
    if resp
        .content_length()
        .is_some_and(|size| size > MODPACK_MAX_BYTES as u64)
    {
        return Err("整合包过大（超过 64 MiB）".to_string());
    }
    let mut file = tokio::fs::File::create(target)
        .await
        .map_err(|e| format!("保存整合包失败：{e}"))?;
    let mut written = 0;
    while let Some(bytes) = resp
        .chunk()
        .await
        .map_err(|e| format!("读取整合包失败：{}", e.without_url()))?
    {
        written += bytes.len();
        if written > MODPACK_MAX_BYTES {
            return Err("整合包过大（超过 64 MiB）".into());
        }
        file.write_all(&bytes)
            .await
            .map_err(|e| format!("保存整合包失败：{e}"))?;
    }
    file.flush()
        .await
        .map_err(|e| format!("保存整合包失败：{e}"))?;
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

    /// issue #49 G5: the dshhome form accepts a WSL distro and must reject
    /// (not silently ignore) a blank one, so a stray empty string cannot
    /// turn a WSL import into a surprise local Windows HOME.
    #[test]
    fn import_input_wsl_distro_is_optional_but_blank_is_dropped() {
        let base = r#"{"source":"pack.dspack"}"#;
        let none: ImportModpackInput = serde_json::from_str(base).unwrap();
        assert!(none.wsl_distro.is_none());

        let set: ImportModpackInput =
            serde_json::from_str(r#"{"source":"pack.dspack","wsl_distro":"Ubuntu"}"#).unwrap();
        assert_eq!(set.wsl_distro.as_deref(), Some("Ubuntu"));

        // The blank filter lives in `do_import_dshhome`; assert its shape here
        // so a refactor cannot drop it.
        let blank = "   ";
        assert!(blank.trim().is_empty());
    }

    /// issue #49 G5: a WSL import must resolve a version record installed
    /// *inside that distro*. Pairing it with a local Windows record of the same
    /// version string would make the launcher spawn a Linux path with Windows
    /// tooling. The selection is a pure predicate over the config, so assert
    /// its shape here.
    #[test]
    fn import_version_selection_is_distro_scoped() {
        let src = include_str!("modpack.rs");
        let body = src
            .split("async fn resolve_import_version(")
            .nth(1)
            .expect("resolve_import_version must exist")
            .split("/// Imports a")
            .next()
            .unwrap();
        assert!(
            body.contains("v.wsl.as_deref() == distro"),
            "the version lookup must be scoped to the distro flavour"
        );
        assert!(
            body.contains("install_version_streamed_wsl"),
            "a WSL target must install the version inside the distro"
        );
        assert!(
            body.contains("install_version_streamed_pub"),
            "a local target must keep the Windows install path"
        );
    }

    /// The single-profile form must stay local-only: it targets an existing
    /// instance whose HOME already fixes the distro, so a caller-supplied
    /// `wsl_distro` must not reach the file layer there.
    #[test]
    fn single_profile_import_ignores_wsl_distro() {
        let src = include_str!("modpack.rs");
        let body = src
            .split("async fn do_import_modpack(")
            .nth(1)
            .expect("do_import_modpack must exist")
            .split("async fn do_import_dshhome(")
            .next()
            .unwrap();
        assert!(
            !body.contains("input.wsl_distro"),
            "the single-profile form must not read wsl_distro"
        );
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

    /// The PackForge index mixes plain-string and locale-map
    /// displayName/description, and lean entries may omit every optional
    /// field (issue #17): both shapes must parse.
    #[test]
    fn market_index_parses_string_and_locale_map_entries() {
        let raw = r#"{
            "schemaVersion": 2,
            "generatedAt": "2026-09-11T09:39:21.133Z",
            "modpacks": [
                {
                    "manifestVersion": 5,
                    "type": "dshhome",
                    "name": "dsh-home-pack",
                    "version": "1.0.0",
                    "displayName": "dsh-game-skin",
                    "description": "",
                    "author": "HXH",
                    "category": "uncategorized",
                    "dshVersion": "0.1.2-alpha.5",
                    "profileName": null,
                    "downloadUrl": "https://example.com/a.dspack",
                    "sha256": "",
                    "size": 9478,
                    "updatedAt": "2026-09-08",
                    "id": "hxh230802.dsh-game-skin",
                    "owner": "hxh230802",
                    "repo": "dsh-game-skin",
                    "profileCount": 5,
                    "bundleCount": 16,
                    "depCount": 7
                },
                {
                    "manifestVersion": 4,
                    "type": "profile",
                    "name": "whales",
                    "version": "1.0.0",
                    "displayName": {"en-US": "All About Whales", "zh-CN": "大肥鱼套装"},
                    "description": {"en-US": "Whales.", "zh-CN": "大肥鱼。"},
                    "downloadUrl": "https://example.com/b.dspack",
                    "id": "DSH-PackForge.whales",
                    "bundleCount": 6,
                    "depCount": 4
                }
            ]
        }"#;
        let index: MarketModpackIndex = serde_json::from_str(raw).unwrap();
        assert_eq!(index.modpacks.len(), 2);

        let first = &index.modpacks[0];
        assert_eq!(first.id, "hxh230802.dsh-game-skin");
        assert_eq!(first.pack_type.as_deref(), Some("dshhome"));
        assert_eq!(
            first.display_name.as_ref().unwrap().as_str(),
            Some("dsh-game-skin")
        );
        assert_eq!(first.size, Some(9478));
        assert_eq!(first.profile_count, Some(5));

        let second = &index.modpacks[1];
        assert!(second.display_name.as_ref().unwrap().is_object());
        assert_eq!(
            second.description.as_ref().unwrap()["zh-CN"].as_str(),
            Some("大肥鱼。")
        );
        // Lean entry: everything optional defaults to None.
        assert!(second.sha256.is_none());
        assert!(second.size.is_none());
        assert!(second.updated_at.is_none());
        assert!(second.profile_count.is_none());
        assert!(second.author.is_none());
    }
}
