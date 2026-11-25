use super::{
    direct_dir, is_sha256, locate, mac_app_complete, release_version, supported_version,
    target_for_node, verify_mac_signature, windows_complete, PreparedHelper, Receipt, Staging,
    Target, MAC_APP, MAC_EXE, MAX_ENTRIES, MAX_UNPACKED_BYTES,
};
use crate::{config::LauncherSettings, AppState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MANIFEST: &str = ".cua-files.json";

#[derive(Deserialize, Serialize, Eq, PartialEq)]
struct Manifest {
    files: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WindowsDependencies {
    runtime_target: WindowsRuntimeTarget,
    targets: BTreeMap<String, BTreeMap<String, WindowsAssets>>,
}

#[derive(Deserialize)]
struct WindowsRuntimeTarget {
    name: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct WindowsAssets {
    runtime: BTreeMap<String, WindowsAsset>,
    native: BTreeMap<String, WindowsAsset>,
    resources: BTreeMap<String, WindowsAsset>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WindowsAsset {
    local_path: Option<String>,
}

pub(crate) fn root(data_dir: &Path, settings: &LauncherSettings) -> PathBuf {
    settings
        .cua_root
        .clone()
        .unwrap_or_else(|| data_dir.join("cua"))
}

pub(super) fn canonical_root(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("请输入 CUA 根目录的绝对路径".into());
    }
    fs::create_dir_all(path).map_err(|e| format!("无法创建 CUA 根目录：{e}"))?;
    fs::canonicalize(path).map_err(|e| format!("无法解析 CUA 根目录：{e}"))
}

pub(crate) fn validate_root(path: &Path) -> Result<PathBuf, String> {
    let path = canonical_root(path)?;
    if cfg!(windows) {
        check_length(&path.join("0.2.3-alpha.2/win-arm64/dsh-cua-windows.runtimeconfig.json"))?;
    }
    Ok(path)
}

fn check_length(path: &Path) -> Result<(), String> {
    let plain = path.to_string_lossy();
    let plain = crate::tui_cwd::strip_verbatim_prefix(&plain);
    if plain.encode_utf16().count() >= 260 {
        return Err(format!(
            "CUA 文件路径超过 Windows 兼容长度，请选择更短的 CUA 根目录：{plain}"
        ));
    }
    Ok(())
}

fn plain_path(path: PathBuf) -> PathBuf {
    if cfg!(windows) {
        if let Some(text) = path.to_str() {
            return PathBuf::from(crate::tui_cwd::strip_verbatim_prefix(text).as_ref());
        }
    }
    path
}

fn direct_metadata(path: &Path) -> Result<fs::Metadata, String> {
    let meta =
        fs::symlink_metadata(path).map_err(|e| format!("无法读取 {}：{e}", path.display()))?;
    #[cfg(windows)]
    let redirected = {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let redirected = meta.file_type().is_symlink();
    if redirected {
        return Err(format!("CUA 文件不能重定向：{}", path.display()));
    }
    Ok(meta)
}

fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join(MANIFEST);
    let meta = direct_metadata(&path)?;
    if !meta.is_file() || meta.len() > 2 * 1024 * 1024 {
        return Err("CUA 校验记录无效".into());
    }
    let manifest: Manifest = serde_json::from_reader(File::open(&path).map_err(|e| e.to_string())?)
        .map_err(|e| format!("CUA 校验记录无效：{e}"))?;
    if manifest.files.is_empty() || manifest.files.values().any(|sha| !is_sha256(sha)) {
        return Err("CUA 校验记录缺少有效的文件摘要".into());
    }
    Ok(manifest)
}

fn snapshot(dir: &Path) -> Result<Manifest, String> {
    if !direct_metadata(dir)?.is_dir() {
        return Err(format!("CUA 助手路径不是目录：{}", dir.display()));
    }
    let mut files = BTreeMap::new();
    let mut pending = vec![PathBuf::new()];
    let mut file_count = 0;
    let mut bytes = 0u64;
    while let Some(prefix) = pending.pop() {
        for entry in fs::read_dir(dir.join(&prefix)).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let relative = prefix.join(entry.file_name());
            if relative == Path::new(MANIFEST) {
                continue;
            }
            file_count += 1;
            if file_count > MAX_ENTRIES {
                return Err("CUA 助手文件数超出限制".into());
            }
            let path = entry.path();
            let meta = direct_metadata(&path)?;
            if relative == Path::new("dsh-cua-windows.log") {
                if !meta.is_file() {
                    return Err(format!("CUA 日志路径不是普通文件：{}", path.display()));
                }
                continue;
            }
            if meta.is_dir() {
                pending.push(relative);
                continue;
            }
            if !meta.is_file() {
                return Err(format!("CUA 助手包含非常规文件：{}", path.display()));
            }
            bytes = bytes
                .checked_add(meta.len())
                .ok_or("CUA 助手大小超出限制")?;
            if bytes > MAX_UNPACKED_BYTES {
                return Err("CUA 助手大小超出限制".into());
            }
            let name = relative
                .to_str()
                .ok_or("CUA 助手路径不是有效 Unicode")?
                .replace('\\', "/");
            super::archive_path(&name)?;
            let mut hash = Sha256::new();
            let mut file = File::open(path)
                .map_err(|e| e.to_string())?
                .take(meta.len() + 1);
            if io::copy(&mut file, &mut hash).map_err(|e| e.to_string())? != meta.len() {
                return Err(format!("检查 CUA 文件时大小发生变化：{name}"));
            }
            files.insert(name, format!("{:x}", hash.finalize()));
        }
    }
    Ok(Manifest { files })
}

fn windows_machine(path: &Path) -> Result<u16, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut dos = [0u8; 64];
    file.read_exact(&mut dos).map_err(|e| e.to_string())?;
    if !dos.starts_with(b"MZ") {
        return Err(format!("CUA 文件不是 Windows PE：{}", path.display()));
    }
    let offset = u32::from_le_bytes([dos[60], dos[61], dos[62], dos[63]]);
    file.seek(SeekFrom::Start(u64::from(offset)))
        .map_err(|e| e.to_string())?;
    let mut header = [0u8; 6];
    file.read_exact(&mut header).map_err(|e| e.to_string())?;
    if !header.starts_with(b"PE\0\0") {
        return Err(format!("CUA 文件的 PE 头无效：{}", path.display()));
    }
    Ok(u16::from_le_bytes([header[4], header[5]]))
}

fn mac_has_architecture(mut file: impl Read, expected: u32) -> io::Result<bool> {
    let mut header = [0u8; 8];
    file.read_exact(&mut header)?;
    let magic = u32::from_be_bytes(header[..4].try_into().unwrap());
    let little = matches!(magic, 0xcffaedfe | 0xbebafeca | 0xbfbafeca);
    let number = if little {
        u32::from_le_bytes
    } else {
        u32::from_be_bytes
    };
    match magic {
        0xfeedfacf | 0xcffaedfe => Ok(number(header[4..].try_into().unwrap()) == expected),
        0xcafebabe | 0xbebafeca | 0xcafebabf | 0xbfbafeca => {
            let count = number(header[4..].try_into().unwrap());
            if count > 32 {
                return Ok(false);
            }
            let width = if matches!(magic, 0xcafebabf | 0xbfbafeca) {
                32
            } else {
                20
            };
            let mut arch = [0u8; 32];
            let mut found = false;
            for _ in 0..count {
                file.read_exact(&mut arch[..width])?;
                found |= number(arch[..4].try_into().unwrap()) == expected;
            }
            Ok(found)
        }
        _ => Ok(false),
    }
}

fn executable(dir: &Path, target: Target) -> Result<PathBuf, String> {
    if target.is_mac() {
        let receipt_file = dir.join("current.json");
        let receipt: Receipt = serde_json::from_reader(
            File::open(&receipt_file)
                .map_err(|e| e.to_string())?
                .take(4096),
        )
        .map_err(|e| format!("CUA 应用清单无效：{e}"))?;
        if !is_sha256(&receipt.bundle) {
            return Err("CUA 应用清单中的目录名无效".into());
        }
        let app = dir.join(receipt.bundle).join(MAC_APP);
        if !mac_app_complete(&app) {
            return Err("CUA 助手缺少完整的 macOS 应用".into());
        }
        let executable = app.join(MAC_EXE);
        let expected = if target.arch == "arm64" {
            0x0100000c
        } else {
            0x01000007
        };
        let file = File::open(&executable).map_err(|e| e.to_string())?;
        if !mac_has_architecture(file, expected).map_err(|e| e.to_string())? {
            return Err(format!(
                "CUA 应用与当前架构不匹配：{}",
                executable.display()
            ));
        }
        return Ok(executable);
    }
    if !windows_complete(dir) {
        return Err("CUA 助手缺少 Windows 可执行文件或运行库".into());
    }
    let expected = if target.arch == "arm64" {
        0xaa64
    } else {
        0x8664
    };
    let executable = dir.join("dsh-cua-windows.exe");
    for path in [
        &executable,
        &dir.join("coreclr.dll"),
        &dir.join("hostfxr.dll"),
    ] {
        if windows_machine(path)? != expected {
            return Err(format!("CUA 文件与当前架构不匹配：{}", path.display()));
        }
    }
    Ok(executable)
}

fn verify_paths(manifest: &Manifest, destination: &Path, target: Target) -> Result<(), String> {
    if !target.is_mac() {
        for name in manifest.files.keys() {
            check_length(&destination.join(name))?;
        }
    }
    Ok(())
}

fn verify_windows_dependencies(
    mut source: impl Read,
    manifest: &Manifest,
    target: Target,
) -> Result<(), String> {
    let dependencies: WindowsDependencies = serde_json::from_reader(&mut source)
        .map_err(|error| format!("CUA 的 .NET 依赖清单无效：{error}"))?;
    let name = &dependencies.runtime_target.name;
    if !name.ends_with(&format!("/{}", target.name())) {
        return Err("CUA 的 .NET 依赖清单与当前架构不匹配".into());
    }
    let assets = dependencies
        .targets
        .get(name)
        .ok_or("CUA 的 .NET 依赖清单缺少当前平台")?;
    let require = |name: &str| {
        if manifest.files.contains_key(name) {
            Ok(())
        } else {
            Err(format!("CUA 助手缺少 .NET 运行库：{name}"))
        }
    };
    for name in ["hostpolicy.dll", "clrjit.dll", "System.Private.CoreLib.dll"] {
        require(name)?;
    }
    for library in assets.values() {
        for (name, asset) in library
            .runtime
            .iter()
            .chain(&library.native)
            .chain(&library.resources)
        {
            if name.ends_with("/_._") {
                continue;
            }
            let default = name.rsplit('/').next().unwrap_or(name);
            require(asset.local_path.as_deref().unwrap_or(default))?;
        }
    }
    Ok(())
}

fn verify_payload(dir: &Path, manifest: &Manifest, target: Target) -> Result<(), String> {
    if !target.is_mac() {
        let deps = File::open(dir.join("dsh-cua-windows.deps.json")).map_err(|e| e.to_string())?;
        verify_windows_dependencies(deps.take(2 * 1024 * 1024), manifest, target)?;
    }
    Ok(())
}

fn save_manifest(dir: &Path, manifest: &Manifest) -> Result<(), String> {
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(dir.join(MANIFEST))
        .map_err(|e| format!("无法写入 CUA 校验记录：{e}"))?;
    serde_json::to_writer(file, manifest).map_err(|e| e.to_string())
}

pub(super) fn register_payload(
    dir: &Path,
    destination: &Path,
    target: Target,
) -> Result<(), String> {
    executable(dir, target)?;
    let manifest = snapshot(dir)?;
    verify_payload(dir, &manifest, target)?;
    verify_paths(&manifest, destination, target)?;
    save_manifest(dir, &manifest)
}

pub(super) fn verify_installed(dir: &Path, target: Target) -> Result<PathBuf, String> {
    if !direct_metadata(dir)?.is_dir() {
        return Err("CUA 安装位置不是目录".into());
    }
    let expected = read_manifest(dir)?;
    if expected != snapshot(dir)? {
        return Err("CUA 助手文件内容与校验记录不一致".into());
    }
    verify_paths(&expected, dir, target)?;
    verify_payload(dir, &expected, target)?;
    executable(dir, target).map(plain_path)
}

pub(super) fn verify_replacement(dir: &Path) -> Result<(), String> {
    let conflict = || format!("目标已有其他 CUA 文件，请先移出该目录：{}", dir.display());
    if !direct_metadata(dir)?.is_dir() {
        return Err(conflict());
    }
    let managed = read_manifest(dir).map_err(|_| conflict())?;
    if snapshot(dir)?
        .files
        .keys()
        .any(|entry| !managed.files.contains_key(entry))
    {
        return Err(conflict());
    }
    Ok(())
}

pub(super) fn installed(root: &Path, version: &str, target: Target) -> bool {
    if !release_version(version) {
        return false;
    }
    let Ok(root) = fs::canonicalize(root) else {
        return false;
    };
    let parent = root.join(version);
    if !direct_metadata(&parent).is_ok_and(|meta| meta.is_dir()) {
        return false;
    }
    verify_installed(&parent.join(target.name()), target).is_ok()
}

fn copy_payload(source: &Path, dest: &Path, expected: &Manifest) -> Result<(), String> {
    fs::create_dir(dest).map_err(|e| e.to_string())?;
    for name in expected.files.keys() {
        let path = dest.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::copy(source.join(name), &path).map_err(|e| format!("无法复制 CUA 文件 {name}：{e}"))?;
    }
    if snapshot(dest)? != *expected {
        return Err("复制后的 CUA 文件校验失败".into());
    }
    save_manifest(dest, expected)
}

fn adopt_legacy(
    location: super::HelperLocation,
    root: &Path,
    version: &str,
    target: Target,
) -> Result<Option<PathBuf>, String> {
    if location.target != target {
        return Err("旧 CUA 助手与本次启动的 Node 架构不一致".into());
    }
    let name = target.name();
    let source = location.provider.join("native/bin").join(&name);
    if !source.try_exists().map_err(|e| e.to_string())? {
        return Ok(None);
    }
    if fs::canonicalize(&source).map_err(|e| e.to_string())? != source {
        return Err("旧 CUA 助手目录被重定向".into());
    }
    let expected = snapshot(&source)?;
    let binary = executable(&source, target)?;
    verify_payload(&source, &expected, target)?;
    if target.is_mac() {
        let app = binary
            .ancestors()
            .nth(3)
            .ok_or("找不到旧 CUA 的 macOS 应用目录")?;
        verify_mac_signature(app)?;
    }
    let base = direct_dir(root, &[version])?;
    let destination = base.join(name);
    verify_paths(&expected, &destination, target)?;
    let staging = Staging(base.join(format!(".cua-{}", uuid::Uuid::new_v4().simple())));
    fs::create_dir(&staging.0).map_err(|e| e.to_string())?;
    copy_payload(&source, &staging.0.join("payload"), &expected)?;
    if destination.try_exists().map_err(|e| e.to_string())? {
        direct_metadata(&destination)?;
    }
    let installed = PreparedHelper {
        staging,
        destination,
        target,
    }
    .commit()?;
    verify_installed(&installed, target).map(Some)
}

/// 启动时只访问本机文件，完整的旧版本可以迁移到统一目录。
pub(crate) async fn resolve_for_launch(
    state: &AppState,
    dir: &Path,
    version: &str,
) -> Result<Option<PathBuf>, String> {
    if !release_version(version) {
        return Ok(None);
    }
    let Some(target) = target_for_node().await? else {
        return Ok(None);
    };
    let cached_root = root(&state.data_dir, &state.config.lock().unwrap().settings);
    let cached_version = version.to_owned();
    let cached = tokio::task::spawn_blocking(move || {
        let root = fs::canonicalize(cached_root).ok()?;
        let parent = root.join(cached_version);
        if !direct_metadata(&parent).is_ok_and(|meta| meta.is_dir()) {
            return None;
        }
        verify_installed(&parent.join(target.name()), target).ok()
    })
    .await
    .map_err(|e| e.to_string())?;
    if cached.is_some() || !supported_version(version) {
        return Ok(cached);
    }
    let _operation = state
        .cua_operation
        .try_lock()
        .map_err(|_| "CUA 下载或目录切换进行中，请完成后重启该实例")?;
    let root = root(&state.data_dir, &state.config.lock().unwrap().settings);
    let Some(location) = locate(dir, version).await? else {
        return Ok(None);
    };
    let version = version.to_owned();
    tokio::task::spawn_blocking(move || {
        let root = canonical_root(&root)?;
        adopt_legacy(location, &root, &version, target)
    })
    .await
    .map_err(|e| e.to_string())?
}

pub(crate) fn copy_root(source: &Path, destination: &Path) -> Result<(), String> {
    let destination = validate_root(destination)?;
    let source = match fs::canonicalize(source) {
        Ok(source) => source,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("无法读取原 CUA 根目录：{e}")),
    };
    if source == destination {
        return Ok(());
    }
    if source.starts_with(&destination) || destination.starts_with(&source) {
        return Err("新旧 CUA 根目录不能互相包含".into());
    }
    let staging = Staging(destination.join(format!(".cua-copy-{}", uuid::Uuid::new_v4().simple())));
    fs::create_dir(&staging.0).map_err(|e| e.to_string())?;
    let mut transfers = Vec::new();
    for child in fs::read_dir(&source).map_err(|e| e.to_string())? {
        let version_dir = child.map_err(|e| e.to_string())?.path();
        let Some(version) = version_dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !release_version(version) {
            continue;
        }
        if !direct_metadata(&version_dir)?.is_dir() {
            return Err(format!("CUA 版本路径不是目录：{}", version_dir.display()));
        }
        for child in fs::read_dir(&version_dir).map_err(|e| e.to_string())? {
            let old = child.map_err(|e| e.to_string())?.path();
            let Some(name) = old.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(target) = Target::from_name(name) else {
                continue;
            };
            verify_installed(&old, target)?;
            let manifest = read_manifest(&old)?;
            let base = direct_dir(&destination, &[version])?;
            let new = base.join(name);
            verify_paths(&manifest, &new, target)?;
            if new.try_exists().map_err(|e| e.to_string())? {
                let same = verify_installed(&new, target)
                    .and_then(|_| read_manifest(&new))
                    .is_ok_and(|receipt| receipt == manifest);
                if !same {
                    return Err(format!("目标已有不同的 CUA 文件：{}", new.display()));
                }
                continue;
            }
            let stage = staging.0.join(transfers.len().to_string());
            copy_payload(&old, &stage, &manifest)?;
            transfers.push((stage, new));
        }
    }
    let mut published = Vec::new();
    for (stage, new) in transfers {
        if let Err(error) = fs::rename(&stage, &new) {
            for path in published.iter().rev() {
                if let Err(cleanup) = fs::remove_dir_all(path) {
                    crate::log_warn!("回收未完成的 CUA 目录复制失败：{cleanup}");
                }
            }
            return Err(format!("无法发布 CUA 目录 {}：{error}", new.display()));
        }
        published.push(new);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn windows_fixture(dir: &Path) -> Target {
        fs::create_dir_all(dir).unwrap();
        let mut pe = [0u8; 70];
        pe[..2].copy_from_slice(b"MZ");
        pe[60] = 64;
        pe[64..].copy_from_slice(b"PE\0\0\x64\x86");
        for name in super::super::WINDOWS_FILES {
            fs::write(dir.join(name), pe).unwrap();
        }
        for name in ["hostpolicy.dll", "clrjit.dll", "System.Private.CoreLib.dll"] {
            fs::write(dir.join(name), pe).unwrap();
        }
        fs::write(
            dir.join("dsh-cua-windows.deps.json"),
            br#"{"runtimeTarget":{"name":"test/win-x64"},"targets":{"test/win-x64":{"helper":{"runtime":{"dsh-cua-windows.dll":{}}}}}}"#,
        )
        .unwrap();
        Target::from_name("win-x64").unwrap()
    }

    #[test]
    fn rejects_missing_windows_dependencies_before_recording_payload() {
        let temp = Staging(std::env::temp_dir().join(format!("cua-{}", uuid::Uuid::new_v4())));
        let target = windows_fixture(&temp.0);
        fs::remove_file(temp.0.join("hostpolicy.dll")).unwrap();
        assert!(register_payload(&temp.0, &temp.0, target)
            .unwrap_err()
            .contains("hostpolicy.dll"));
        assert!(!temp.0.join(MANIFEST).exists());
        assert!(verify_replacement(&temp.0).is_err());
    }

    #[test]
    fn copies_verified_helpers_and_leaves_conflicting_contents_intact() {
        let temp = Staging(std::env::temp_dir().join(format!("cua-{}", uuid::Uuid::new_v4())));
        let source = temp.0.join("old");
        let destination = temp.0.join("new");
        let suffix = Path::new("0.2.3-alpha.2/win-x64");
        let old = source.join(suffix);
        let new = destination.join(suffix);
        let target = windows_fixture(&old);
        register_payload(&old, &old, target).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("unmanaged"), b"existing file").unwrap();
        assert!(copy_root(&source, &destination)
            .unwrap_err()
            .contains("不同"));
        assert_eq!(fs::read(new.join("unmanaged")).unwrap(), b"existing file");
        verify_installed(&old, target).unwrap();
        fs::remove_file(new.join("unmanaged")).unwrap();
        fs::remove_dir(&new).unwrap();
        copy_root(&source, &destination).unwrap();
        copy_root(&source, &destination).unwrap();
        verify_installed(&old, target).unwrap();
        verify_installed(&new, target).unwrap();
        fs::write(new.join("hostpolicy.dll"), b"corrupt").unwrap();
        assert!(verify_installed(&new, target).is_err());
        assert!(verify_replacement(&new).is_ok());
        fs::write(new.join("unmanaged"), b"existing file").unwrap();
        assert!(verify_replacement(&new).is_err());
    }

    #[test]
    fn mac_mach_o_requires_the_target_architecture() {
        let arm64 = 0x0100000cu32;
        let x64 = 0x01000007u32;
        let mut thin = 0xfeedfacfu32.to_le_bytes().to_vec();
        thin.extend(arm64.to_le_bytes());
        assert!(mac_has_architecture(thin.as_slice(), arm64).unwrap());
        assert!(!mac_has_architecture(thin.as_slice(), x64).unwrap());

        let mut fat = 0xcafebabeu32.to_be_bytes().to_vec();
        fat.extend(2u32.to_be_bytes());
        for arch in [arm64, x64] {
            fat.extend(arch.to_be_bytes());
            fat.extend([0; 16]);
        }
        assert!(mac_has_architecture(fat.as_slice(), x64).unwrap());
        assert!(mac_has_architecture(fat.as_slice(), arm64).unwrap());
        assert!(mac_has_architecture(&fat[..fat.len() - 1], x64).is_err());
    }

    #[test]
    fn windows_path_limit_counts_utf16_units() {
        assert!(check_length(Path::new(&"a".repeat(259))).is_ok());
        assert!(check_length(Path::new(&"a".repeat(260))).is_err());
        assert!(check_length(Path::new(&"😀".repeat(130))).is_err());
    }
}
