use super::*;
use crate::browser_settings::PreviewConfig;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    take_pwstr, BasicAuthenticationRequestedEventHandler, HistoryChangedEventHandler,
    NavigationCompletedEventHandler,
};
use windows::core::{Interface, BOOL, HSTRING, PWSTR};
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};

pub(super) fn supported() -> bool {
    true
}

pub(super) fn builder(builder: WebviewBuilder<Wry>, config: &PreviewConfig) -> WebviewBuilder<Wry> {
    // Wry 的 additional_browser_args 会覆盖整个默认参数串，代理和旁路必须一起设置。
    builder.additional_browser_args(&format!(
        "--proxy-server={} --proxy-bypass-list=<-loopback>",
        config.network.url().origin().ascii_serialization()
    ))
}

pub(super) fn layout(view: &Webview, area: Layout) -> Result<()> {
    native_bounds(view, area)
}

static BROWSER_PROCESSES: LazyLock<Mutex<HashMap<PathBuf, Arc<OwnedHandle>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn attach(view: &Webview, config: &PreviewConfig, events: Reporter) -> Result<()> {
    let endpoint = config.network.url();
    let username = HSTRING::from(config.network.username());
    let password = HSTRING::from(config.network.password());
    let data_dir = config.data_dir.clone();
    on_ui(view, move |platform| {
        // 事件回调和 COM 对象均在创建该控制器的 UI 线程使用。
        unsafe {
            let core = platform
                .controller()
                .CoreWebView2()
                .map_err(|e| e.to_string())?;
            let mut pid = 0;
            core.BrowserProcessId(&mut pid).map_err(|e| e.to_string())?;
            let process =
                OpenProcess(PROCESS_SYNCHRONIZE, false, pid).map_err(|e| e.to_string())?;
            BROWSER_PROCESSES
                .lock()
                .unwrap()
                .insert(data_dir, Arc::new(OwnedHandle::from_raw_handle(process.0)));
            let auth: ICoreWebView2_10 = core.cast().map_err(|e| e.to_string())?;
            let mut token = 0;
            let proxy_retry = std::rc::Rc::new(std::cell::Cell::new(false));
            let supplied = proxy_retry.clone();
            auth.add_BasicAuthenticationRequested(
                &BasicAuthenticationRequestedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    let uri = take_pwstr(uri);
                    // WebView2 将代理的 URI 作为 407 的请求地址，源站 401 的地址则是源站本身。
                    if Url::parse(&uri).ok().as_ref() != Some(&endpoint) {
                        return Ok(());
                    }
                    let mut challenge = PWSTR::null();
                    args.Challenge(&mut challenge)?;
                    if take_pwstr(challenge).eq_ignore_ascii_case("Basic realm=\"DSH Launcher\"") {
                        let response = args.Response()?;
                        response.SetUserName(&username)?;
                        response.SetPassword(&password)?;
                        supplied.set(true);
                    }
                    Ok(())
                })),
                &mut token,
            )
            .map_err(|e| e.to_string())?;
            let history = events.clone();
            let changed = HistoryChangedEventHandler::create(Box::new(move |sender, _| {
                if let Some(core) = sender {
                    let mut back = BOOL::default();
                    let mut forward = BOOL::default();
                    let mut uri = PWSTR::null();
                    core.CanGoBack(&mut back)?;
                    core.CanGoForward(&mut forward)?;
                    core.Source(&mut uri)?;
                    let url = take_pwstr(uri);
                    if url != "about:blank" {
                        history.emit(json!({
                            "url": url, "canGoBack": back.as_bool(), "canGoForward": forward.as_bool()
                        }));
                    }
                }
                Ok(())
            }));
            core.add_HistoryChanged(&changed, &mut token)
                .map_err(|e| e.to_string())?;
            let completed = NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut ok = BOOL::default();
                    args.IsSuccess(&mut ok)?;
                    if ok.as_bool() {
                        proxy_retry.set(false);
                        events.emit(json!({"loading": false, "error": null}));
                    } else {
                        let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                        args.WebErrorStatus(&mut status)?;
                        if status
                            == COREWEBVIEW2_WEB_ERROR_STATUS_VALID_PROXY_AUTHENTICATION_REQUIRED
                            && proxy_retry.replace(false)
                        {
                            // WebView2 先结束未授权的请求，随后自动携带代理凭据重试。
                            events.emit(json!({"loading": true}));
                        } else if status != COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED {
                            events.emit(json!({
                                "loading": false, "error": format!("WebView2 导航失败（{}）", status.0)
                            }));
                        }
                    }
                }
                Ok(())
            }));
            core.add_NavigationCompleted(&completed, &mut token)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })
}

pub(super) fn history(view: &Webview, back: bool) -> Result<()> {
    on_ui(view, move |platform| unsafe {
        let core = platform
            .controller()
            .CoreWebView2()
            .map_err(|e| e.to_string())?;
        if back {
            core.GoBack()
        } else {
            core.GoForward()
        }
        .map_err(|e| e.to_string())
    })
}

pub(super) fn retire(_app: &AppHandle, path: &Path) -> Result<()> {
    let process = BROWSER_PROCESSES.lock().unwrap().get(path).cloned();
    if let Some(process) = process {
        // 最后一个 WebView 关闭后浏览器仍可能在写 Cookie；进程句柄也可避免 PID 重用。
        let result = unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), 15_000) };
        if result != WAIT_OBJECT_0 {
            return Err("WebView2 尚未释放临时浏览数据".into());
        }
        let mut processes = BROWSER_PROCESSES.lock().unwrap();
        if processes
            .get(path)
            .is_some_and(|current| Arc::ptr_eq(current, &process))
        {
            processes.remove(path);
        }
    }
    Ok(())
}
