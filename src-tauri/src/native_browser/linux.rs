use super::*;
use crate::browser_settings::PreviewConfig;
use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;
use webkit2gtk::{AuthenticationRequestExt, Credential, CredentialPersistence, WebViewExt};

pub(super) fn supported() -> bool {
    true
}

pub(super) fn builder(builder: WebviewBuilder<Wry>, config: &PreviewConfig) -> WebviewBuilder<Wry> {
    builder.proxy_url(config.network.url())
}

pub(super) fn attach(view: &Webview, config: &PreviewConfig, events: Reporter) -> Result<()> {
    let endpoint = config.network.url();
    let username = config.network.username().to_string();
    let password = config.network.password().to_string();
    on_ui(view, move |platform| {
        let child = platform.inner();
        let container = child
            .parent()
            .and_then(|p| p.downcast::<gtk::Box>().ok())
            .ok_or("找不到预览的 GTK 父容器")?;
        let overlay = container.children().into_iter().find_map(|widget| {
            widget
                .downcast::<gtk::Overlay>()
                .ok()
                .filter(|w| w.widget_name() == "dsh-browser-overlay")
        });
        let overlay = match overlay {
            Some(overlay) => overlay,
            None => {
                let main = container
                    .children()
                    .into_iter()
                    .find_map(|widget| {
                        widget
                            .downcast::<webkit2gtk::WebView>()
                            .ok()
                            .filter(|w| w != &child)
                    })
                    .ok_or("找不到 DSH 主视图")?;
                let overlay = gtk::Overlay::new();
                overlay.set_widget_name("dsh-browser-overlay");
                container.remove(&main);
                overlay.add(&main);
                container.pack_start(&overlay, true, true, 0);
                overlay.show();
                overlay
            }
        };
        container.remove(&child);
        child.set_halign(gtk::Align::Start);
        child.set_valign(gtk::Align::Start);
        overlay.add_overlay(&child);
        child.connect_authenticate(move |_, request| {
            if !request.is_for_proxy()
                || request.host().as_deref() != endpoint.host_str()
                || Some(request.port()) != endpoint.port_or_known_default().map(u32::from)
                || request.realm().as_deref() != Some("DSH Launcher")
            {
                return false;
            }
            if request.is_retry() {
                request.cancel();
                return true;
            }
            let credential = Credential::new(&username, &password, CredentialPersistence::None);
            // gtk-rs 尚未包装 WebKit 的 authenticate 方法；凭据仅传给已核对地址的本地代理。
            unsafe {
                webkit2gtk::ffi::webkit_authentication_request_authenticate(
                    request.to_glib_none().0,
                    credential.to_glib_none().0,
                )
            };
            true
        });
        let history = events.clone();
        child.connect_uri_notify(move |web| {
            if let Some(url) = web.uri().filter(|uri| uri != "about:blank") {
                history.emit(json!({
                    "url": url.as_str(),
                    "canGoBack": web.can_go_back(),
                    "canGoForward": web.can_go_forward(),
                }));
            }
        });
        let history = events.clone();
        child.connect_load_changed(move |web, _| {
            history
                .emit(json!({"canGoBack":web.can_go_back(),"canGoForward":web.can_go_forward()}));
        });
        child.connect_load_failed(move |_, _, _, error| {
            if !error.matches(webkit2gtk::NetworkError::Cancelled) {
                events.emit(json!({"loading":false,"error":error.message()}));
            }
            false
        });
        child.show();
        Ok(())
    })
}

pub(super) fn layout(view: &Webview, area: Layout) -> Result<()> {
    on_ui(view, move |platform| {
        let web = platform.inner();
        web.set_margin_start(area.x.round() as i32);
        web.set_margin_top(area.y.round() as i32);
        web.set_size_request(area.width.round() as i32, area.height.round() as i32);
        Ok(())
    })
}

pub(super) fn history(view: &Webview, back: bool) -> Result<()> {
    on_ui(view, move |platform| {
        if back {
            platform.inner().go_back()
        } else {
            platform.inner().go_forward()
        }
        Ok(())
    })
}
