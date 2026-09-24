//! Instance icons (issue #8): an instance may carry a custom icon — an
//! http(s) URL or a local image copied into `<home>/icons/<id>.png`. Local
//! images are center-cropped to a 1:1 square and re-encoded as PNG. The
//! launcher icon stays the default and is never copied or exported.

use std::path::{Path, PathBuf};

use crate::AppState;
use tauri::{Manager, State};

const ICON_MAX_BYTES: usize = 16 * 1024 * 1024;
const ICON_SIZE: u32 = 256;

/// Where a local icon lives inside the instance's HOME.
pub(crate) fn local_icon_path(home: &Path, instance_id: &str) -> PathBuf {
    home.join("icons").join(format!("{instance_id}.png"))
}

/// Crops the image to a centered 1:1 square and encodes it as PNG.
pub(crate) fn crop_square_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("解析图像失败: {e}"))?;
    let (w, h) = (img.width(), img.height());
    let side = w.min(h);
    let cropped = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    let resized = if side > ICON_SIZE {
        cropped.resize_exact(ICON_SIZE, ICON_SIZE, image::imageops::FilterType::Lanczos3)
    } else {
        cropped
    };
    let mut out = std::io::Cursor::new(Vec::new());
    resized
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(|e| format!("编码 PNG 失败: {e}"))?;
    Ok(out.into_inner())
}

/// Downloads an icon URL with a size cap.
async fn fetch_icon(url: &str) -> Result<Vec<u8>, String> {
    let client = crate::proxy::apply(reqwest::Client::builder())
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("dsh-launcher")
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("下载图标失败 {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载图标失败 {url}: HTTP {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取图标失败: {e}"))?;
    if bytes.len() > ICON_MAX_BYTES {
        return Err("图标文件过大（超过 16 MiB）".to_string());
    }
    Ok(bytes.to_vec())
}

/// Validates that bytes decode as an image (used to sanity-check remote URLs
/// that stay remote).
fn ensure_decodable(bytes: &[u8]) -> Result<(), String> {
    image::load_from_memory(bytes)
        .map(|_| ())
        .map_err(|e| format!("不是有效的图像文件: {e}"))
}

/// Downloads a remote icon and returns it as a cropped square PNG.
pub(crate) async fn fetch_square_icon_png(url: &str) -> Result<Vec<u8>, String> {
    let bytes = fetch_icon(url).await?;
    crop_square_png(&bytes)
}

/// Encodes PNG bytes as a Windows .ico (issue #14): .url shortcuts only
/// accept ICO icons. The image is fitted into 256×256 (the ICO ceiling).
pub(crate) fn png_to_ico_bytes(png: &[u8]) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let img = image::load_from_memory(png).map_err(|e| format!("解析图像失败: {e}"))?;
    let img = if img.width() > ICON_SIZE || img.height() > ICON_SIZE {
        img.resize(ICON_SIZE, ICON_SIZE, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut out = Vec::new();
    image::codecs::ico::IcoEncoder::new(&mut out)
        .write_image(&rgba, w, h, image::ExtendedColorType::Rgba8)
        .map_err(|e| format!("编码 ICO 失败: {e}"))?;
    Ok(out)
}

fn instance_home(state: &AppState, instance_id: &str) -> Result<PathBuf, String> {
    let cfg = state.config.lock().unwrap();
    let inst = cfg
        .instances
        .iter()
        .find(|i| i.id == instance_id)
        .ok_or_else(|| "实例不存在".to_string())?;
    cfg.homes
        .iter()
        .find(|h| h.id == inst.home_id)
        .map(crate::wsl::home_fs_path)
        .ok_or_else(|| "DSH_HOME 不存在".to_string())
}

/// Sets an instance icon from a local image file or an http(s) URL. Local
/// files are cropped to a square PNG inside the HOME; URLs are stored as-is
/// after a decode sanity check.
#[tauri::command]
pub async fn set_instance_icon(
    state: State<'_, AppState>,
    instance_id: String,
    source: String,
) -> Result<(), String> {
    let source = source.trim().to_string();
    if source.is_empty() {
        return Err("图标来源不能为空".to_string());
    }
    let home = instance_home(&state, &instance_id)?;

    let icon = if source.starts_with("https://") || source.starts_with("http://") {
        ensure_decodable(&fetch_icon(&source).await?)?;
        source
    } else {
        let src = PathBuf::from(&source);
        let bytes =
            std::fs::read(&src).map_err(|e| format!("读取图标文件失败 {}: {e}", src.display()))?;
        if bytes.len() > ICON_MAX_BYTES {
            return Err("图标文件过大（超过 16 MiB）".to_string());
        }
        let png = crop_square_png(&bytes)?;
        let dest = local_icon_path(&home, &instance_id);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建图标目录失败: {e}"))?;
        }
        std::fs::write(&dest, png).map_err(|e| format!("写入图标失败: {e}"))?;
        "local".to_string()
    };

    let mut cfg = state.config.lock().unwrap();
    let inst = cfg
        .instances
        .iter_mut()
        .find(|i| i.id == instance_id)
        .ok_or_else(|| "实例不存在".to_string())?;
    inst.icon = Some(icon);
    crate::commands::save_state(&state, &cfg)?;
    Ok(())
}

/// Restores the default launcher icon and removes any local icon file.
#[tauri::command]
pub fn clear_instance_icon(state: State<'_, AppState>, instance_id: String) -> Result<(), String> {
    let home = instance_home(&state, &instance_id)?;
    let mut cfg = state.config.lock().unwrap();
    let inst = cfg
        .instances
        .iter_mut()
        .find(|i| i.id == instance_id)
        .ok_or_else(|| "实例不存在".to_string())?;
    let was_local = inst.icon.as_deref() == Some("local");
    inst.icon = None;
    crate::commands::save_state(&state, &cfg)?;
    drop(cfg);
    if was_local {
        let _ = std::fs::remove_file(local_icon_path(&home, &instance_id));
    }
    Ok(())
}

/// Resolves an instance icon for display: remote URLs pass through, local
/// files become a `data:` URL, and `None` means the launcher default.
#[tauri::command]
pub fn read_instance_icon(
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<Option<String>, String> {
    let home = instance_home(&state, &instance_id)?;
    let icon = {
        let cfg = state.config.lock().unwrap();
        cfg.instances
            .iter()
            .find(|i| i.id == instance_id)
            .and_then(|i| i.icon.clone())
    };
    match icon.as_deref() {
        None => Ok(None),
        Some("local") => {
            let bytes = std::fs::read(local_icon_path(&home, &instance_id))
                .map_err(|e| format!("读取图标失败: {e}"))?;
            Ok(Some(format!(
                "data:image/png;base64,{}",
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
            )))
        }
        Some(url) => Ok(Some(url.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Launcher icons (issue #59): the window icon (title bar + taskbar share it)
// and the tray icon may be replaced by a local image, stored as cropped
// square PNGs under `<data>/icons/launcher-<kind>.png`. Files on disk are the
// source of truth — startup re-applies them, no config flag to go stale.
// ---------------------------------------------------------------------------

/// Where a launcher icon of `kind` ("window" / "tray") is stored.
fn launcher_icon_path(data_dir: &Path, kind: &str) -> Result<PathBuf, String> {
    match kind {
        "window" => Ok(data_dir.join("icons").join("launcher-window.png")),
        "tray" => Ok(data_dir.join("icons").join("launcher-tray.png")),
        _ => Err(format!("未知的图标类型: {kind}")),
    }
}

/// Applies PNG bytes to the live window / tray icon.
fn apply_launcher_icon(app: &tauri::AppHandle, kind: &str, png: &[u8]) -> Result<(), String> {
    let img = tauri::image::Image::from_bytes(png).map_err(|e| format!("解析 PNG 失败: {e}"))?;
    match kind {
        "window" => {
            let win = app
                .get_webview_window("main")
                .ok_or_else(|| "主窗口不存在".to_string())?;
            win.set_icon(img)
                .map_err(|e| format!("设置窗口图标失败: {e}"))
        }
        "tray" => {
            let tray = app
                .tray_by_id("main")
                .ok_or_else(|| "托盘不存在".to_string())?;
            tray.set_icon(Some(img))
                .map_err(|e| format!("设置托盘图标失败: {e}"))
        }
        _ => Err(format!("未知的图标类型: {kind}")),
    }
}

/// Re-applies the stored launcher icons at startup; missing files keep the
/// defaults. Failures are logged, never fatal — a bad icon file must not
/// block the launcher.
pub fn apply_launcher_icons(app: &tauri::AppHandle, data_dir: &Path) {
    for kind in ["window", "tray"] {
        let Ok(path) = launcher_icon_path(data_dir, kind) else {
            continue;
        };
        match std::fs::read(&path) {
            Ok(bytes) => {
                if let Err(e) = apply_launcher_icon(app, kind, &bytes) {
                    crate::log_warn!("应用自定义启动器图标（{kind}）失败: {e}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => crate::log_warn!("读取自定义启动器图标（{kind}）失败: {e}"),
        }
    }
}

/// Sets a launcher icon from a local image file: decoded, center-cropped to
/// a square PNG, stored, and applied immediately.
#[tauri::command(rename_all = "snake_case")]
pub fn set_launcher_icon(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    kind: String,
    path: String,
) -> Result<(), String> {
    let dest = launcher_icon_path(&state.data_dir, &kind)?;
    let src = PathBuf::from(path.trim());
    let bytes =
        std::fs::read(&src).map_err(|e| format!("读取图标文件失败 {}: {e}", src.display()))?;
    if bytes.len() > ICON_MAX_BYTES {
        return Err("图标文件过大（超过 16 MiB）".to_string());
    }
    let png = crop_square_png(&bytes)?;
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建图标目录失败: {e}"))?;
    }
    std::fs::write(&dest, &png).map_err(|e| format!("写入图标失败: {e}"))?;
    apply_launcher_icon(&app, &kind, &png)?;
    crate::log_info!("已设置自定义启动器图标（{kind}）");
    Ok(())
}

/// Restores a launcher icon to the default and removes the stored file.
#[tauri::command(rename_all = "snake_case")]
pub fn clear_launcher_icon(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    kind: String,
) -> Result<(), String> {
    let dest = launcher_icon_path(&state.data_dir, &kind)?;
    match std::fs::remove_file(&dest) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("删除图标失败: {e}")),
    }
    let default = app.default_window_icon().cloned();
    match (kind.as_str(), default) {
        ("window", Some(icon)) => {
            if let Some(win) = app.get_webview_window("main") {
                win.set_icon(icon)
                    .map_err(|e| format!("恢复窗口图标失败: {e}"))?;
            }
        }
        ("tray", Some(icon)) => {
            if let Some(tray) = app.tray_by_id("main") {
                tray.set_icon(Some(icon))
                    .map_err(|e| format!("恢复托盘图标失败: {e}"))?;
            }
        }
        // No bundled default icon to restore to (should not happen in
        // packaged builds); the file is gone either way.
        _ => crate::log_warn!("恢复默认启动器图标（{kind}）：无内置默认图标可用"),
    }
    Ok(())
}

/// Reads a stored launcher icon as a `data:` URL for the settings preview;
/// `None` means the default is in use.
#[tauri::command(rename_all = "snake_case")]
pub fn read_launcher_icon(
    state: State<'_, AppState>,
    kind: String,
) -> Result<Option<String>, String> {
    let path = launcher_icon_path(&state.data_dir, &kind)?;
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(format!(
            "data:image/png;base64,{}",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("读取图标失败: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_icon_path_validates_kind() {
        let dir = Path::new("data");
        assert!(launcher_icon_path(dir, "window")
            .unwrap()
            .ends_with("launcher-window.png"));
        assert!(launcher_icon_path(dir, "tray")
            .unwrap()
            .ends_with("launcher-tray.png"));
        assert!(launcher_icon_path(dir, "../escape").is_err());
        assert!(launcher_icon_path(dir, "").is_err());
    }

    /// 4x2 red rectangle PNG, generated once for the crop test.
    fn rect_png() -> Vec<u8> {
        let img = image::RgbImage::from_pixel(4, 2, image::Rgb([255, 0, 0]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn crop_square_centers_wide_images() {
        let png = crop_square_png(&rect_png()).unwrap();
        let img = image::load_from_memory(&png).unwrap();
        assert_eq!(img.width(), img.height());
        assert_eq!(img.width(), 2);
    }

    #[test]
    fn png_converts_to_valid_ico() {
        let ico = png_to_ico_bytes(&rect_png()).unwrap();
        // ICO header: reserved=0, type=1 (icon), count=1.
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 1, 0]);
        let img = image::load_from_memory_with_format(&ico, image::ImageFormat::Ico).unwrap();
        assert_eq!(img.width(), 4);
        assert_eq!(img.height(), 2);
    }
}
