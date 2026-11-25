use super::*;
use crate::browser_settings::PreviewConfig;
use sha2::{Digest, Sha256};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;

extern "C" {
    fn dsh_browser_supported() -> bool;
    fn dsh_browser_attach(
        view: *mut c_void,
        port: u16,
        name: *const c_char,
        pass: *const c_char,
        ctx: *mut c_void,
    ) -> bool;
    fn dsh_browser_history(view: *mut c_void, back: bool);
    fn dsh_browser_remove(identifier: *const u8, context: *mut c_void);
}

pub(super) fn supported() -> bool {
    unsafe { dsh_browser_supported() }
}

fn store_id(path: &Path) -> [u8; 16] {
    let digest = Sha256::digest(path.as_os_str().as_encoded_bytes());
    let mut id = [0; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

pub(super) fn builder(builder: WebviewBuilder<Wry>, config: &PreviewConfig) -> WebviewBuilder<Wry> {
    builder.data_store_identifier(store_id(&config.data_dir))
}

pub(super) fn layout(view: &Webview, area: Layout) -> Result<()> {
    native_bounds(view, area)
}

pub(super) fn attach(view: &Webview, config: &PreviewConfig, reporter: Reporter) -> Result<()> {
    let port = config.network.url().port().ok_or("本地代理缺少端口")?;
    let name = CString::new(config.network.username()).map_err(|e| e.to_string())?;
    let pass = CString::new(config.network.password()).map_err(|e| e.to_string())?;
    on_ui(view, move |platform| {
        let ctx = Box::into_raw(Box::new(reporter));
        if unsafe {
            dsh_browser_attach(
                platform.inner(),
                port,
                name.as_ptr(),
                pass.as_ptr(),
                ctx.cast(),
            )
        } {
            Ok(())
        } else {
            unsafe { drop(Box::from_raw(ctx)) }
            Err("未能配置 WebKit 的侧栏代理".into())
        }
    })
}

#[no_mangle]
extern "C" fn dsh_browser_state(
    ctx: *const c_void,
    url: *const c_char,
    back: bool,
    forward: bool,
    error: *const c_char,
) {
    let reporter = unsafe { &*ctx.cast::<Reporter>() };
    let mut update = json!({"canGoBack":back,"canGoForward":forward});
    if !url.is_null() {
        let url = unsafe { CStr::from_ptr(url) }.to_string_lossy();
        if url != "about:blank" {
            update["url"] = json!(url);
        }
    }
    if !error.is_null() {
        update["loading"] = json!(false);
        update["error"] = json!(unsafe { CStr::from_ptr(error) }.to_string_lossy());
    }
    reporter.emit(update);
}

#[no_mangle]
extern "C" fn dsh_browser_drop(ctx: *mut c_void) {
    unsafe { drop(Box::from_raw(ctx.cast::<Reporter>())) }
}

pub(super) fn history(view: &Webview, back: bool) -> Result<()> {
    on_ui(view, move |platform| {
        unsafe { dsh_browser_history(platform.inner(), back) };
        Ok(())
    })
}

pub(super) fn retire(app: &AppHandle, path: &Path) -> Result<()> {
    if !supported() {
        return Ok(());
    }
    let id = store_id(path);
    let (tx, rx) = std::sync::mpsc::channel::<Result<()>>();
    app.run_on_main_thread(move || unsafe {
        dsh_browser_remove(id.as_ptr(), Box::into_raw(Box::new(tx)).cast());
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(std::time::Duration::from_secs(15))
        .map_err(|e| e.to_string())?
}

#[no_mangle]
extern "C" fn dsh_browser_removed(ctx: *mut c_void, error: *const c_char) {
    let tx = unsafe { Box::from_raw(ctx.cast::<std::sync::mpsc::Sender<Result<()>>>()) };
    let result = if error.is_null() {
        Ok(())
    } else {
        Err(unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned())
    };
    let _ = tx.send(result);
}
