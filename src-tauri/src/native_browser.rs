use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::webview::{NewWindowResponse, PageLoadEvent, WebviewBuilder};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, Url, Webview, WebviewUrl,
    WebviewWindowBuilder, Wry,
};
use uuid::Uuid;

#[cfg(target_os = "linux")]
#[path = "native_browser/linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "native_browser/macos.rs"]
mod platform;
#[cfg(windows)]
#[path = "native_browser/windows.rs"]
mod platform;

type Result<T> = std::result::Result<T, String>;

#[derive(Default)]
struct Browsers(Mutex<HashMap<String, Arc<Host>>>);

struct Host {
    token: String,
    origin: Mutex<String>,
    generation: AtomicU64,
    tabs: Mutex<HashMap<Uuid, Tab>>,
}

struct Tab {
    view: Webview,
    generation: u64,
    data_dir: std::path::PathBuf,
    // 保持该数据目录的代理监听到最后一个使用它的标签关闭。
    network: Arc<crate::browser_proxy::BrowserNetwork>,
}

#[derive(Clone)]
struct Reporter {
    app: AppHandle,
    owner: String,
    origin: String,
    id: Uuid,
}

impl Reporter {
    fn emit(&self, mut detail: Value) {
        detail["id"] = json!(self.id.to_string());
        let origin = json!(self.origin);
        if let Some(owner) = self.app.get_webview(&self.owner) {
            let script = format!("if(location.origin==={origin})window.dispatchEvent(new CustomEvent('dsh-launcher-browser-state',{{detail:{detail}}}))");
            if let Err(error) = owner.eval(script) {
                crate::log_debug!("发送侧栏网页状态失败: {error}");
            }
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Payload {
    id: Option<Uuid>,
    url: Option<String>,
    session_id: Option<String>,
    turn: Option<u64>,
    #[serde(flatten)]
    bounds: Bounds,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Bounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    viewport_width: f64,
}

#[derive(Clone, Copy)]
struct Layout {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Bounds {
    fn layout(&self, owner: &Webview) -> Result<Layout> {
        let window = owner.window();
        let scale = window.scale_factor().map_err(|e| e.to_string())?;
        let size = window
            .inner_size()
            .map_err(|e| e.to_string())?
            .to_logical::<f64>(scale);
        self.fit(size)
    }

    fn fit(&self, size: LogicalSize<f64>) -> Result<Layout> {
        if ![self.x, self.y, self.width, self.height, self.viewport_width]
            .iter()
            .all(|v| v.is_finite())
            || self.viewport_width <= 0.
            || self.width <= 0.
            || self.height <= 0.
        {
            return Err("网页预览区域无效".into());
        }
        let ratio = size.width / self.viewport_width;
        if !ratio.is_finite() || ratio <= 0. {
            return Err("网页预览缩放比例无效".into());
        }
        let x = (self.x * ratio).clamp(0., size.width);
        let y = (self.y * ratio).clamp(0., size.height);
        let width = ((self.x + self.width) * ratio).clamp(x, size.width) - x;
        let height = ((self.y + self.height) * ratio).clamp(y, size.height) - y;
        if width < 1. || height < 1. {
            return Err("网页预览区域不可见".into());
        }
        Ok(Layout {
            x,
            y,
            width,
            height,
        })
    }
}

fn script(origin: &str, token: &str) -> String {
    include_str!("native_browser/bridge.js")
        .replace("__ORIGIN__", &json!(origin).to_string())
        .replace("__TOKEN__", &json!(token).to_string())
        .replace(
            "__SUPPORTED__",
            if platform::supported() {
                "true"
            } else {
                "false"
            },
        )
}

fn grant(app: &AppHandle, label: &str, origin: &str) -> Result<()> {
    let cap = tauri::ipc::CapabilityBuilder::new(format!("browser-{}", Uuid::new_v4()))
        .local(false)
        .webview(label)
        .remote(format!("{origin}/*"))
        .permission("sidebar-browser:allow-send");
    app.add_capability(cap).map_err(|e| e.to_string())
}

/// 主窗口和远程网页分别使用不同的 WebView 标签，只有实例的 DSH 源可以请求侧栏操作。
pub(crate) fn plugin() -> tauri::plugin::TauriPlugin<Wry> {
    tauri::plugin::Builder::new("sidebar-browser")
        .invoke_handler(tauri::generate_handler![send])
        .setup(|app, _| {
            app.manage(Browsers::default());
            Ok(())
        })
        .build()
}

/// 实例重启可能改用新的监听端口，权限和脚本必须跟随新来源。
pub(crate) fn change_origin(app: &AppHandle, label: &str, url: &Url) -> Result<()> {
    let hosts = app.state::<Browsers>();
    let hosts = hosts.0.lock().unwrap();
    if let Some(host) = hosts.get(label) {
        let mut origin = host.origin.lock().unwrap();
        let new_origin = url.origin().ascii_serialization();
        if *origin != new_origin {
            grant(app, label, &new_origin)?;
            *origin = new_origin;
        }
    }
    Ok(())
}

/// 在实例主页面初始化后开放浏览接口，并在文档重新加载时回收旧标签。
pub(crate) fn configure<'a>(
    app: &AppHandle,
    label: &str,
    initial_url: &Url,
    builder: WebviewWindowBuilder<'a, Wry, AppHandle>,
) -> Result<WebviewWindowBuilder<'a, Wry, AppHandle>> {
    let origin = initial_url.origin().ascii_serialization();
    grant(app, label, &origin)?;
    let host = Arc::new(Host {
        token: Uuid::new_v4().to_string(),
        origin: Mutex::new(origin.clone()),
        generation: AtomicU64::new(0),
        tabs: Mutex::new(HashMap::new()),
    });
    app.state::<Browsers>()
        .0
        .lock()
        .unwrap()
        .insert(label.into(), host.clone());
    let init = script(&origin, &host.token);
    let popup_app = app.clone();
    let popup_owner = label.to_owned();
    Ok(builder
        .initialization_script(init)
        .on_new_window(move |url, _| {
            open_popup(&popup_app, &popup_owner, url);
            NewWindowResponse::Deny
        })
        .on_page_load(move |owner, event| {
            let origin = host.origin.lock().unwrap().clone();
            match event.event() {
                PageLoadEvent::Started => {
                    let generation = host.generation.fetch_add(1, Ordering::SeqCst) + 1;
                    let host = host.clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        host.tabs.lock().unwrap().retain(|_, tab| {
                            if tab.generation >= generation {
                                return true;
                            }
                            if let Err(error) = tab.view.close() {
                                crate::log_debug!("回收网页预览失败: {error}");
                            }
                            false
                        });
                    });
                }
                PageLoadEvent::Finished if event.url().origin().ascii_serialization() == origin => {
                    if let Err(error) = owner.eval(script(&origin, &host.token)) {
                        crate::log_warn!("初始化实例的侧栏浏览器失败: {error}");
                    }
                }
                _ => {}
            }
        }))
}

/// 窗口关闭后撤销原生接口的会话凭证。
pub(crate) fn forget(app: &AppHandle, label: &str) {
    app.state::<Browsers>().0.lock().unwrap().remove(label);
}

fn open_popup(app: &AppHandle, owner: &str, url: Url) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
    let app = app.clone();
    let owner = owner.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let result =
            http_url(Some(url.as_str())).and_then(|url| crate::commands::open_external(url.into()));
        if let Err(error) = result {
            crate::log_warn!("打开外部网页失败: {error}");
            let mut dialog = app
                .dialog()
                .message(error)
                .title("DSH Launcher")
                .kind(MessageDialogKind::Error);
            if let Some(window) = app.get_window(&owner) {
                dialog = dialog.parent(&window);
            }
            dialog.show(|_| {});
        }
    });
}

fn http_url(text: Option<&str>) -> Result<Url> {
    let url = Url::parse(text.ok_or("缺少网页地址")?).map_err(|e| e.to_string())?;
    if !matches!(url.scheme(), "http" | "https") || !url.has_host() {
        return Err("只允许 HTTP 或 HTTPS 网页地址".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("网页地址不能包含用户名或密码".into());
    }
    Ok(url)
}

fn allowed_preview(url: &Url, app: &AppHandle, proxy: &Url, owner_origin: &str) -> bool {
    let protected_host = url.host_str().is_some_and(|host| {
        host == "tauri.localhost" || host == "ipc.localhost" || host == "asset.localhost"
    });
    matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
        && !protected_host
        && url.origin() != proxy.origin()
        && url.origin().ascii_serialization() != owner_origin
        && !(cfg!(dev)
            && app
                .config()
                .build
                .dev_url
                .as_ref()
                .is_some_and(|dev| url.origin() == dev.origin()))
}

#[tauri::command]
async fn send(webview: Webview, token: String, action: String, payload: Payload) -> Result<()> {
    let host = webview
        .state::<Browsers>()
        .0
        .lock()
        .unwrap()
        .get(webview.label())
        .cloned()
        .ok_or("此窗口没有侧栏浏览器")?;
    if token != host.token {
        return Err("侧栏请求来源无效".into());
    }
    let generation = host.generation.load(Ordering::SeqCst);
    tauri::async_runtime::spawn_blocking(move || {
        let origin = host.origin.lock().unwrap().clone();
        if webview
            .url()
            .map_err(|e| e.to_string())?
            .origin()
            .ascii_serialization()
            != origin
        {
            return Err("侧栏只能由当前 DSH 页面操作".into());
        }
        if action == "acknowledge-session" {
            if generation != host.generation.load(Ordering::SeqCst) {
                return Err("会话页面已切换".into());
            }
            let instance = webview
                .label()
                .strip_prefix("instance-")
                .ok_or("无法识别 DSH 实例")?;
            let session = payload
                .session_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .ok_or("缺少 DSH 会话标识")?;
            let turn = payload.turn.ok_or("缺少已读轮次")?;
            crate::notifications::acknowledge_session(
                webview.app_handle(),
                instance,
                session,
                turn,
            );
            return Ok(());
        }
        if action == "external" {
            return crate::commands::open_external(http_url(payload.url.as_deref())?.into());
        }
        if !platform::supported() {
            return Err("当前系统请使用外部浏览器".into());
        }
        let mut tabs = host.tabs.lock().unwrap();
        if generation != host.generation.load(Ordering::SeqCst) {
            return Err("会话页面已切换".into());
        }
        let id = payload.id.ok_or("缺少网页标签标识")?;
        if action == "close" {
            if let Some(tab) = tabs.get(&id) {
                tab.view.close().map_err(|e| e.to_string())?;
            }
            tabs.remove(&id);
            return Ok(());
        }
        if action == "open" {
            if tabs.contains_key(&id) {
                return Err("网页标签已存在".into());
            }
            let url = http_url(payload.url.as_deref())?;
            let bounds = payload.bounds.layout(&webview)?;
            let tab = open(&webview, id, url, bounds, generation, origin)?;
            tabs.insert(id, tab);
            return Ok(());
        }
        let Some(tab) = tabs.get(&id) else {
            if action == "hide" {
                return Ok(());
            }
            return Err("网页标签已关闭".into());
        };
        match action.as_str() {
            "hide" => tab.view.hide().map_err(|e| e.to_string()),
            "layout" => {
                platform::layout(&tab.view, payload.bounds.layout(&webview)?)?;
                tab.view.show().map_err(|e| e.to_string())
            }
            "navigate" => {
                let url = http_url(payload.url.as_deref())?;
                if !allowed_preview(&url, webview.app_handle(), &tab.network.url(), &origin) {
                    return Err("该地址属于启动器内部服务".into());
                }
                tab.view.navigate(url).map_err(|e| e.to_string())
            }
            "back" | "forward" => platform::history(&tab.view, action == "back"),
            "reload" => tab.view.reload().map_err(|e| e.to_string()),
            _ => Err("不支持的侧栏操作".into()),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

fn open(
    owner: &Webview,
    id: Uuid,
    url: Url,
    bounds: Layout,
    generation: u64,
    origin: String,
) -> Result<Tab> {
    let app = owner.app_handle();
    let instance = owner
        .label()
        .strip_prefix("instance-")
        .ok_or("无法识别 DSH 实例")?;
    let config = crate::browser_settings::preview_config(app, instance)?;
    let proxy = config.network.url();
    if !allowed_preview(&url, app, &proxy, &origin) {
        return Err("该地址属于启动器内部服务".into());
    }
    let reporter = Reporter {
        app: app.clone(),
        owner: owner.label().into(),
        id,
        origin,
    };
    let title = reporter.clone();
    let load = reporter.clone();
    let nav = reporter.clone();
    let popup = reporter.clone();
    let popup_proxy = proxy.clone();
    let mut builder = WebviewBuilder::new(
        format!("preview-{}", Uuid::new_v4()),
        WebviewUrl::External(Url::parse("about:blank").unwrap()),
    )
    .data_directory(config.data_dir.clone())
    .on_navigation(move |url| {
        if url.as_str() == "about:blank" || allowed_preview(url, &nav.app, &proxy, &nav.origin) {
            return true;
        }
        nav.emit(json!({"error":"该地址无法在侧栏打开","loading":false}));
        false
    })
    .on_new_window(move |url, _| {
        if allowed_preview(&url, &popup.app, &popup_proxy, &popup.origin) {
            open_popup(&popup.app, &popup.owner, url);
        } else {
            popup.emit(json!({"error":"该地址无法打开"}));
        }
        NewWindowResponse::Deny
    })
    .on_document_title_changed(move |_, text| title.emit(json!({"title":text})))
    .on_page_load(move |_, event| {
        if event.url().as_str() != "about:blank" {
            let started = matches!(event.event(), PageLoadEvent::Started);
            let mut update = json!({"loading":started,"url":event.url().as_str()});
            if started {
                update["error"] = Value::Null;
            }
            load.emit(update);
        }
    });
    if let Some(agent) = &config.user_agent {
        builder = builder.user_agent(agent);
    }
    builder = platform::builder(builder, &config);
    let view = owner
        .window()
        .add_child(
            builder,
            LogicalPosition::new(bounds.x, bounds.y),
            LogicalSize::new(bounds.width, bounds.height),
        )
        .map_err(|e| e.to_string())?;
    let setup = platform::attach(&view, &config, reporter)
        .and_then(|()| platform::layout(&view, bounds))
        .and_then(|()| view.navigate(url).map_err(|e| e.to_string()));
    if let Err(error) = setup {
        if let Err(close_error) = view.close() {
            crate::log_warn!("回收未能打开的网页预览失败: {close_error}");
        }
        return Err(error);
    }
    Ok(Tab {
        view,
        generation,
        data_dir: config.data_dir,
        network: config.network,
    })
}

/// 原生 WebView 只能在 UI 线程上操作，调用端在后台等待初始化及网络认证配置完成。
fn on_ui<T: Send + 'static>(
    view: &Webview,
    f: impl FnOnce(tauri::webview::PlatformWebview) -> Result<T> + Send + 'static,
) -> Result<T> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    view.with_webview(move |platform| {
        let _ = tx.send(f(platform));
    })
    .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}

#[cfg(not(target_os = "linux"))]
fn native_bounds(view: &Webview, area: Layout) -> Result<()> {
    view.set_bounds(tauri::Rect {
        position: LogicalPosition::new(area.x, area.y).into(),
        size: LogicalSize::new(area.width, area.height).into(),
    })
    .map_err(|e| e.to_string())
}

/// 临时目录由调用方清理，各平台先释放浏览器仍持有的会话资源。
pub(crate) fn retire_store(app: &AppHandle, path: &std::path::Path) -> Result<()> {
    if let Some(state) = app.try_state::<Browsers>() {
        let hosts: Vec<_> = state.0.lock().unwrap().values().cloned().collect();
        for host in hosts {
            let mut tabs = host.tabs.lock().unwrap();
            let ids: Vec<_> = tabs
                .iter()
                .filter_map(|(id, tab)| (tab.data_dir == path).then_some(*id))
                .collect();
            for id in ids {
                if let Some(tab) = tabs.get(&id) {
                    if app.get_webview(tab.view.label()).is_some() {
                        tab.view.close().map_err(|error| error.to_string())?;
                    }
                }
                tabs.remove(&id);
            }
        }
    }
    // 等事件线程消费之前排入的窗口关闭操作后，再触及磁盘或 WebKit 的持久存储。
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(());
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(std::time::Duration::from_secs(15))
        .map_err(|e| e.to_string())?;
    #[cfg(any(windows, target_os = "macos"))]
    {
        platform::retire(app, path)
    }
    #[cfg(target_os = "linux")]
    {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_zoom_and_parent_clipping_use_the_same_scale() {
        let area = Bounds {
            x: 600.,
            y: -8.,
            width: 500.,
            height: 200.,
            viewport_width: 1000.,
        };
        let rect = area.fit(LogicalSize::new(1500., 800.)).unwrap();
        assert_eq!(
            (rect.x, rect.y, rect.width, rect.height),
            (900., 0., 600., 288.)
        );
        let outside = Bounds { x: 1001., ..area };
        assert!(outside.fit(LogicalSize::new(1500., 800.)).is_err());
    }

    #[test]
    fn http_addresses_reject_embedded_credentials_and_non_web_schemes() {
        assert!(http_url(Some("http://localhost:3000/project")).is_ok());
        assert!(http_url(Some("https://alice:password@example.org/")).is_err());
        assert!(http_url(Some("file:///tmp/example")).is_err());
    }
}
