use semver::Version;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const RELEASES: &str = "https://github.com/dsh-plugins/dsh-cua-helper/releases/download";
const MAX_ZIP_BYTES: u64 = 192 * 1024 * 1024;
const MAX_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_SUM_BYTES: u64 = 16 * 1024;
const WINDOWS_FILES: &[&str] = &[
    "dsh-cua-windows.exe",
    "dsh-cua-windows.dll",
    "dsh-cua-windows.deps.json",
    "dsh-cua-windows.runtimeconfig.json",
    "CuaProtocol.dll",
    "coreclr.dll",
    "hostfxr.dll",
];
const MAC_APP: &str = "DSH Computer Use.app";
const MAC_DEV_APP: &str = "DSH Computer Use Dev.app";
const MAC_EXE: &str = "Contents/MacOS/dsh-cua-native";

pub(crate) fn supported_version(version: &str) -> bool {
    Version::parse(version).is_ok_and(|v| {
        v.build.is_empty()
            && v.cmp_precedence(&Version::parse("0.2.3-alpha.2").unwrap())
                .is_ge()
    })
}

#[derive(Clone, Copy)]
struct Target {
    os: &'static str,
    arch: &'static str,
}

impl Target {
    fn for_node(os: &str, arch: &str) -> Option<Self> {
        let os = match os {
            "win32" => "win",
            "darwin" => "darwin",
            _ => return None,
        };
        let arch = match arch {
            "x64" => "x64",
            "arm64" => "arm64",
            _ => return None,
        };
        Some(Self { os, arch })
    }

    fn name(self) -> String {
        format!("{}-{}", self.os, self.arch)
    }

    fn is_mac(self) -> bool {
        self.os == "darwin"
    }

    fn package(self) -> &'static str {
        if self.is_mac() {
            "@deepseek-ai/dsh-cua-macos"
        } else {
            "@deepseek-ai/dsh-cua-windows"
        }
    }
}

fn release_asset(version: &str, target: Target) -> (String, String) {
    let tag = format!("dsh-v{version}");
    (
        format!("{RELEASES}/{tag}"),
        format!("cua-helper-{tag}-{}.zip", target.name()),
    )
}

const RESOLVE_PROVIDER: &str = r#"
const fs = require('node:fs');
const path = require('node:path');
const { createRequire } = require('node:module');
const root = process.cwd();
const pkg = process.platform === 'darwin' ? 'cua-macos' : 'cua-windows';
const name = '@deepseek-ai/dsh-' + pkg;
const source = path.join(root, 'apps', 'cli', 'package.json');
const cli = fs.existsSync(source) ? source : path.join(root, 'node_modules', '@deepseek-ai', 'dsh', 'package.json');
const from = createRequire(fs.realpathSync(cli));
const entry = fs.existsSync(source)
  ? path.join(root, 'packages', 'experimental', pkg, 'package.json')
  : createRequire(from.resolve('@deepseek-ai/dsh-cua-bundle/package.json')).resolve(name + '/package.json');
const manifest = fs.realpathSync(entry);
const data = JSON.parse(fs.readFileSync(manifest, 'utf8'));
console.log(JSON.stringify({ os: process.platform, arch: process.arch, manifest, name: data.name, version: data.version, source: cli === source }));
"#;

#[derive(serde::Deserialize)]
struct ResolvedProvider {
    os: String,
    arch: String,
    manifest: PathBuf,
    name: String,
    version: String,
    source: bool,
}

pub(crate) struct HelperLocation {
    target: Target,
    provider: PathBuf,
    source: bool,
}

/// Resolve from the CLI dependency graph rather than guessing pnpm store paths.
pub(crate) async fn locate(dir: &Path, version: &str) -> Result<Option<HelperLocation>, String> {
    if !supported_version(version) || !cfg!(any(windows, target_os = "macos")) {
        return Ok(None);
    }
    let root = fs::canonicalize(dir).map_err(|e| format!("无法解析版本目录：{e}"))?;
    let mut node = tokio::process::Command::new(crate::process::node());
    crate::process::hide_console(&mut node);
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        node.args(["-e", RESOLVE_PROVIDER])
            .current_dir(dir)
            .env_remove("NODE_OPTIONS")
            .env_remove("NODE_PATH")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "解析 CUA 后端超时".to_string())?
    .map_err(|e| format!("无法启动 Node 解析 CUA 后端：{e}"))?;
    if !output.status.success() {
        return Err(format!(
            "无法解析 CUA 后端包：{}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .find(|line| line.contains("Error:"))
                .unwrap_or("Node 包解析失败")
        ));
    }
    let resolved: ResolvedProvider = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("无法读取 CUA 后端信息：{e}"))?;
    let Some(target) = Target::for_node(&resolved.os, &resolved.arch) else {
        return Err(format!(
            "没有适用于 {}/{} 的 CUA 助手",
            resolved.os, resolved.arch
        ));
    };
    let manifest = fs::canonicalize(&resolved.manifest).map_err(|e| e.to_string())?;
    if !manifest.starts_with(&root)
        || resolved.name != target.package()
        || resolved.version != version
    {
        return Err("CUA 后端的位置或版本与当前 DSH 安装不一致".into());
    }
    let provider = manifest.parent().ok_or("CUA 后端没有包目录")?.to_path_buf();
    Ok(Some(HelperLocation {
        target,
        provider,
        source: resolved.source,
    }))
}

fn checksum(bytes: &[u8], filename: &str) -> Result<String, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "校验清单不是 UTF-8")?;
    let mut found = None;
    for line in text.lines() {
        let Some((hash, name)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        if name.trim_start().trim_start_matches('*') != filename {
            continue;
        }
        if found.is_some() || !is_sha256(hash) {
            return Err(format!("{filename} 的 SHA-256 记录无效或重复"));
        }
        found = Some(hash.to_ascii_lowercase());
    }
    found.ok_or_else(|| format!("校验清单未包含 {filename}"))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn archive_path(name: &str) -> Result<Option<PathBuf>, String> {
    if name.starts_with('/') {
        return Err(format!("CUA 压缩包包含绝对路径：{name}"));
    }
    let name = name
        .strip_prefix("./")
        .unwrap_or(name)
        .trim_end_matches('/');
    if name.is_empty() || name == "." {
        return Ok(None);
    }
    let mut clean = PathBuf::new();
    for piece in name.split('/') {
        let stem = piece
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if piece.is_empty()
            || piece == "."
            || piece == ".."
            || piece.ends_with([' ', '.'])
            || piece
                .chars()
                .any(|c| c.is_control() || "\\:<>\"|?*".contains(c))
            || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(format!("CUA 压缩包包含不安全的路径：{name}"));
        }
        clean.push(piece);
    }
    Ok(Some(clean))
}

fn validate_entry(
    path: &Path,
    directory: bool,
    names: &mut HashMap<String, (PathBuf, bool, bool)>,
) -> Result<(), String> {
    let parts: Vec<_> = path
        .ancestors()
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    for parent in parts.into_iter().rev() {
        let explicit = parent == path;
        let is_dir = !explicit || directory;
        let key = parent.to_string_lossy().to_lowercase();
        if let Some((original, existing_dir, seen)) = names.get_mut(&key) {
            if original != parent || *existing_dir != is_dir || (explicit && *seen) {
                return Err(format!("CUA 压缩包包含冲突的条目：{}", path.display()));
            }
            *seen |= explicit;
        } else {
            names.insert(key, (parent.to_path_buf(), is_dir, explicit));
        }
    }
    Ok(())
}

fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(File::open(archive).map_err(|e| e.to_string())?)
        .map_err(|e| format!("无法读取 CUA ZIP：{e}"))?;
    if zip.len() > MAX_ENTRIES {
        return Err("CUA ZIP 文件数超出限制".into());
    }
    let mut total = 0u64;
    let mut names = HashMap::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
        let mode = entry.unix_mode().unwrap_or(0);
        let directory = entry.is_dir();
        if !matches!(mode & 0o170000, 0 | 0o040000 | 0o100000) || entry.is_symlink() {
            return Err("CUA ZIP 包含符号链接或特殊文件".into());
        }
        let Some(relative) = archive_path(entry.name())? else {
            if !directory {
                return Err("CUA ZIP 包含未命名文件".into());
            }
            continue;
        };
        total = total
            .checked_add(entry.size())
            .filter(|n| *n <= MAX_UNPACKED_BYTES)
            .ok_or("CUA ZIP 解压大小超出限制")?;
        validate_entry(&relative, directory, &mut names)?;
        // ditto 附加的 Finder 元数据不是签名应用的一部分。
        if relative.starts_with("__MACOSX") {
            continue;
        }
        let destination = into.join(&relative);
        if directory {
            fs::create_dir_all(destination).map_err(|e| e.to_string())?;
            continue;
        }
        fs::create_dir_all(destination.parent().ok_or("CUA ZIP 文件缺少父目录")?)
            .map_err(|e| e.to_string())?;
        let size = entry.size();
        let mut out = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&destination)
            .map_err(|e| e.to_string())?;
        let copied =
            io::copy(&mut entry.by_ref().take(size + 1), &mut out).map_err(|e| e.to_string())?;
        if copied != size {
            return Err(format!("CUA ZIP 文件大小不符：{}", relative.display()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = if mode & 0o111 != 0 { 0o755 } else { 0o644 };
            out.set_permissions(fs::Permissions::from_mode(perms))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn nonempty_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

fn windows_complete(dir: &Path) -> bool {
    WINDOWS_FILES
        .iter()
        .all(|name| nonempty_file(&dir.join(name)))
}

fn mac_app_complete(app: &Path) -> bool {
    let executable = app.join(MAC_EXE);
    #[cfg(unix)]
    let ready = fs::symlink_metadata(&executable).is_ok_and(|meta| {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.len() > 0 && meta.permissions().mode() & 0o111 != 0
    });
    #[cfg(not(unix))]
    let ready = nonempty_file(&executable);
    ready && nonempty_file(&app.join("Contents/Info.plist"))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    bundle: String,
}

fn mac_complete(dir: &Path) -> bool {
    let receipt = File::open(dir.join("current.json"))
        .ok()
        .and_then(|file| serde_json::from_reader::<_, Receipt>(file.take(4096)).ok());
    receipt.is_some_and(|r| {
        r.bundle.len() == 64
            && r.bundle
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            && mac_app_complete(&dir.join(r.bundle).join(MAC_APP))
    })
}

fn direct_dir(base: &Path, names: &[&str]) -> Result<PathBuf, String> {
    let mut dir = base.to_path_buf();
    for name in names {
        dir.push(name);
        match fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                if fs::canonicalize(&dir).map_err(|e| e.to_string())? != dir {
                    return Err(format!("CUA 目录不能重定向：{}", dir.display()));
                }
            }
            Ok(_) => return Err(format!("CUA 路径不是普通目录：{}", dir.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&dir).map_err(|e| e.to_string())?;
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(dir)
}

struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            crate::log_warn!("清理 CUA 临时目录失败：{error}");
        }
    }
}

pub(crate) struct PreparedHelper {
    staging: Staging,
    destination: PathBuf,
    mac: bool,
}

impl HelperLocation {
    pub(crate) fn target(&self) -> String {
        self.target.name()
    }

    pub(crate) async fn prepare(self, version: &str) -> Result<Option<PreparedHelper>, String> {
        let target_name = self.target.name();
        let bin = direct_dir(&self.provider, &["native", "bin"])?;
        let destination = bin.join(&target_name);
        if destination.exists() {
            direct_dir(&bin, &[&target_name])?;
            if (self.target.is_mac() && mac_complete(&destination))
                || (!self.target.is_mac() && windows_complete(&destination))
            {
                return Ok(None);
            }
        }
        let client = crate::proxy::apply(reqwest::Client::builder())
            .connect_timeout(Duration::from_secs(12))
            .timeout(Duration::from_secs(150))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let url = attempt.url();
                let valid = url.scheme() == "https"
                    && matches!(
                        url.host_str(),
                        Some(
                            "github.com"
                                | "release-assets.githubusercontent.com"
                                | "objects.githubusercontent.com"
                        )
                    );
                if !valid || attempt.previous().len() >= 5 {
                    attempt.error("CUA 下载重定向超出允许范围")
                } else {
                    attempt.follow()
                }
            }))
            .user_agent("dsh-launcher")
            .build()
            .map_err(|e| e.to_string())?;
        let (base, name) = release_asset(version, self.target);
        let mut response = request(&client, &format!("{base}/SHA256SUMS"), MAX_SUM_BYTES).await?;
        let mut sums = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| e.without_url().to_string())?
        {
            if sums.len() as u64 + chunk.len() as u64 > MAX_SUM_BYTES {
                return Err("CUA 校验清单大小超出限制".into());
            }
            sums.extend_from_slice(&chunk);
        }
        let expected = checksum(&sums, &name)?;
        let staging = Staging(bin.join(format!(".cua-install-{}", uuid::Uuid::new_v4())));
        fs::create_dir(&staging.0).map_err(|e| e.to_string())?;
        let archive = staging.0.join("helper.zip");
        let mut file = tokio::fs::File::create(&archive)
            .await
            .map_err(|e| e.to_string())?;
        let mut response = request(&client, &format!("{base}/{name}"), MAX_ZIP_BYTES).await?;
        let mut bytes = 0u64;
        let mut sha = Sha256::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| e.without_url().to_string())?
        {
            bytes += chunk.len() as u64;
            if bytes > MAX_ZIP_BYTES {
                return Err("CUA ZIP 大小超出限制".into());
            }
            sha.update(&chunk);
            file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        }
        file.flush().await.map_err(|e| e.to_string())?;
        drop(file);
        if format!("{:x}", sha.finalize()) != expected {
            return Err(format!("{name} 的 SHA-256 校验失败"));
        }
        tokio::task::spawn_blocking(move || {
            let payload = staging.0.join("payload");
            fs::create_dir(&payload).map_err(|e| e.to_string())?;
            unpack(&archive, &payload)?;
            let mac = self.target.is_mac();
            if mac {
                prepare_mac(&payload, &expected, self.source)?;
            } else if !windows_complete(&payload) {
                return Err("CUA ZIP 缺少 Windows 助手或其运行库".into());
            }
            Ok(Some(PreparedHelper {
                staging,
                destination,
                mac,
            }))
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

async fn request(
    client: &reqwest::Client,
    url: &str,
    max: u64,
) -> Result<reqwest::Response, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("CUA 请求失败：{}", e.without_url()))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(format!("尚无对应的 CUA Release 资产：{url}"));
    }
    if !response.status().is_success() {
        return Err(format!("{} 返回 HTTP {}", url, response.status()));
    }
    if response.content_length().is_some_and(|n| n > max) {
        return Err("CUA 下载大小超出限制".into());
    }
    Ok(response)
}

fn prepare_mac(payload: &Path, generation: &str, source: bool) -> Result<(), String> {
    let normal = payload.join(MAC_APP);
    let dev = payload.join(MAC_DEV_APP);
    let bundle = if normal.is_dir() { normal } else { dev };
    if !mac_app_complete(&bundle) {
        return Err("CUA ZIP 缺少完整 macOS 应用".into());
    }
    let subdir = payload.join(generation);
    fs::create_dir(&subdir).map_err(|e| e.to_string())?;
    let application = subdir.join(MAC_APP);
    fs::rename(&bundle, &application).map_err(|e| e.to_string())?;
    let output = std::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&application)
        .output()
        .map_err(|e| format!("无法校验 macOS 助手签名：{e}"))?;
    if !output.status.success() {
        return Err(format!(
            "macOS 助手签名校验失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if source {
        crate::log_info!("已准备源码版本的 CUA 文件，源码运行时仍使用自身的构建机制");
    }
    fs::write(
        payload.join("current.json"),
        format!("{{\"bundle\":\"{generation}\"}}\n"),
    )
    .map_err(|e| e.to_string())
}

impl PreparedHelper {
    pub(crate) fn commit(self) -> Result<PathBuf, String> {
        let payload = self.staging.0.join("payload");
        let complete = if self.mac {
            mac_complete(&self.destination)
        } else {
            windows_complete(&self.destination)
        };
        if complete {
            return Ok(self.destination.clone());
        }
        let backup = self.staging.0.join("previous");
        let replaced = self.destination.exists();
        if replaced {
            fs::rename(&self.destination, &backup)
                .map_err(|e| format!("无法暂存不完整的旧助手：{e}"))?;
        }
        if let Err(error) = fs::rename(&payload, &self.destination) {
            if replaced {
                if let Err(restore) = fs::rename(&backup, &self.destination) {
                    let path = self.staging.0.clone();
                    std::mem::forget(self.staging);
                    return Err(format!(
                        "安装失败：{error}；恢复失败：{restore}；旧文件保留在 {}",
                        path.display()
                    ));
                }
            }
            return Err(format!("安装 CUA 助手失败：{error}"));
        }
        Ok(self.destination.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_windows_runtime_files() {
        let temp =
            Staging(std::env::temp_dir().join(format!("dsh-cua-test-{}", uuid::Uuid::new_v4())));
        fs::create_dir(&temp.0).unwrap();
        for name in WINDOWS_FILES {
            fs::write(temp.0.join(name), b"test").unwrap();
        }
        assert!(windows_complete(&temp.0));
        fs::write(temp.0.join("hostfxr.dll"), b"").unwrap();
        assert!(!windows_complete(&temp.0));
    }

    #[test]
    fn exact_version_and_target() {
        for v in ["0.2.3-alpha.2", "0.2.3-alpha.10", "0.2.3", "1.0.0-alpha.1"] {
            assert!(supported_version(v));
        }
        for v in [
            "0.2.3-alpha.1",
            "0.2.2",
            "local",
            "../0.2.3",
            "0.2.3+different",
        ] {
            assert!(!supported_version(v));
        }
        let arm = Target::for_node("darwin", "arm64").unwrap();
        assert_eq!(
            release_asset("0.2.3-alpha.2", arm),
            (
                format!("{RELEASES}/dsh-v0.2.3-alpha.2"),
                "cua-helper-dsh-v0.2.3-alpha.2-darwin-arm64.zip".into()
            )
        );
        assert!(Target::for_node("linux", "x64").is_none());
    }

    #[test]
    fn exact_checksum_and_zip_paths() {
        let hash = "a".repeat(64);
        let list = format!("{hash}  other.zip\r\n{hash} *native.zip\r\n");
        assert_eq!(checksum(list.as_bytes(), "native.zip").unwrap(), hash);
        assert!(checksum(format!("{list}{list}").as_bytes(), "native.zip").is_err());
        assert!(checksum(list.as_bytes(), "missing.zip").is_err());
        assert_eq!(
            archive_path("./dsh-cua-windows.exe").unwrap(),
            Some(PathBuf::from("dsh-cua-windows.exe"))
        );
        for bad in [
            "/root",
            "a/../b",
            "C:/a",
            "a\\b",
            "a//b",
            "aux.txt",
            "foo. /bar",
        ] {
            assert!(archive_path(bad).is_err(), "{bad}");
        }
        let mut names = HashMap::new();
        validate_entry(Path::new("App/Contents"), true, &mut names).unwrap();
        assert!(validate_entry(Path::new("app/else"), false, &mut names).is_err());
        assert!(validate_entry(Path::new("App/Contents"), false, &mut names).is_err());
    }
}
