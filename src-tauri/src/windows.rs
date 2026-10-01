use std::path::PathBuf;

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::AppState;

/// A frameless standalone window that renders one of the launcher's own
/// routes must never share the main window's WebView2 user-data folder: the
/// default folder lives next to the exe and is shared by every window that
/// does not set its own, so two live webviews on one folder make WebView2
/// fail (or wedge, showing a blank white page) for the second window. All
/// launcher-owned auxiliary windows go under `<data_dir>/webview/app/<label>`.
pub(crate) fn app_webview_data_dir(app: &AppHandle, label: &str) -> Option<PathBuf> {
    let state = app.try_state::<AppState>()?;
    let dir = state.data_dir.join("webview").join("app").join(label);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        crate::log_warn!("创建窗口 {label} 的 webview 数据目录失败: {e}");
        return None;
    }
    Some(dir)
}

/// Remembers the instance whose window was interacted with most recently so
/// the tray double-click can reopen exactly that profile page.
fn record_focus(app: &AppHandle, instance_id: &str) {
    if let Some(state) = app.try_state::<AppState>() {
        *state.last_focused_instance.lock().unwrap() = Some(instance_id.to_string());
    }
}

/// Hides the launcher main window when the user enabled
/// "hide launcher on window open" in the launch-behavior settings.
fn maybe_hide_main(app: &AppHandle) {
    let hide = app
        .try_state::<AppState>()
        .map(|state| {
            state
                .config
                .lock()
                .unwrap()
                .settings
                .hide_launcher_on_window_open
        })
        .unwrap_or(false);
    if hide {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.hide();
        }
    }
}

/// Gives an instance window its own browser data store.
///
/// Windows/Linux take a data directory; macOS instead takes a stable WKWebView
/// store identifier (macOS 14+ / iOS 17+, and the platform default below that).
/// Both forms aim at the same thing: the DSH browser-session cookie is named
/// after the request authority and lives 30 days, so one shared store grows by
/// one live cookie per port an instance ever binds and eventually pushes the
/// request head past node:http's 16 KiB limit, where every request — the token
/// exchange included — is answered 431 and the store can never be pruned again.
fn apply_window_store<'a>(
    mut builder: WebviewWindowBuilder<'a, tauri::Wry, AppHandle>,
    app: &AppHandle,
    instance_id: &str,
) -> WebviewWindowBuilder<'a, tauri::Wry, AppHandle> {
    if let Some(dir) = webview_data_dir(app, instance_id) {
        builder = builder.data_directory(dir);
    }
    // macOS and iOS only: the method does not exist on the other platforms, so
    // it is compiled out rather than relied on to be ignored at runtime.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        use sha2::{Digest, Sha256};

        // Stable per instance: derived from the id, so a window keeps its own
        // cookies across restarts without the launcher persisting anything.
        let digest = Sha256::digest(instance_id.as_bytes());
        let mut identifier = [0u8; 16];
        identifier.copy_from_slice(&digest[..16]);
        builder = builder.data_store_identifier(identifier);
    }
    builder
}
/// Extracts `http://127.0.0.1:<port>` from an instance URL when its authority
/// is plain loopback HTTP. `None` for WSL forwards and any other origin, which
/// get no cookie handling at all.
fn loopback_origin(url: &str) -> Option<String> {
    let rest = url.strip_prefix("http://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.starts_with("127.0.0.1:") || authority == "127.0.0.1" {
        Some(format!("http://{authority}"))
    } else {
        None
    }
}

/// The WebView2 data directory of an instance's DSH page: one directory per
/// instance, so a store never holds the cookies of another instance's origin.
///
/// DSH names each browser-auth cookie after the request authority
/// (`dsh-auth-<base64url(sha256(host:port))>`) and keeps it for 30 days. The
/// isolation this directory buys is per *instance*, not per authority: an
/// instance that restarts on a new port (the default `--port 0`) reuses this
/// same directory, so one dead 30-day cookie per port it ever bound piles up
/// next to the live one; after roughly 80 restarts the request head crosses
/// node:http's 16 KiB `maxHeaderSize`, every request — including the one
/// carrying a fresh `?token=` — is answered `431` before it is parsed, and
/// the window can never refresh the cookie that would prune the store again.
/// `prune_auth_cookies` is what keeps that corpus bounded, by dropping every
/// `dsh-auth-*` cookie belonging to the instance's own host on each reopen.
fn webview_data_dir(app: &AppHandle, instance_id: &str) -> Option<PathBuf> {
    let state = app.try_state::<AppState>()?;
    let dir = state.data_dir.join("webview").join(instance_id);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        crate::log_warn!("创建实例 {instance_id} 的 webview 数据目录失败: {e}");
        return None;
    }
    Some(dir)
}

/// Removes the WebView2 data directory the launcher gave one instance.
///
/// Windows and Linux keep the browser cookies there; macOS does not, so a
/// caller that must also drop the cookies goes through `clear_webview_data`
/// rather than this directory-only helper. macOS therefore has no caller for
/// it, which `-D warnings` would flag as dead code.
#[cfg_attr(any(target_os = "macos", target_os = "ios"), allow(dead_code))]
pub(crate) fn clear_instance_webview_data_from(app: &AppHandle, instance_id: &str) {
    if let Some(state) = app.try_state::<AppState>() {
        clear_instance_webview_data(&state, instance_id);
    }
}

/// Removes an instance's WebView2 data directory (Windows and Linux own the
/// cookie store there). Safe to call on macOS: the directory is still cleaned,
/// it simply is not where the cookies live.
pub fn clear_instance_webview_data(state: &AppState, instance_id: &str) {
    let dir = state.data_dir.join("webview").join(instance_id);
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        if e.kind() != std::io::ErrorKind::NotFound {
            crate::log_warn!("清理实例 {instance_id} 的 webview 数据目录失败: {e}");
        }
    }
}

/// Drops an instance's browser data before the window is sent back to a token
/// URL. Windows/Linux delete the instance's data directory; macOS clears the
fn clear_webview_data(win: &tauri::WebviewWindow, app: &AppHandle, instance_id: &str) {
    // The window's own store is the only place macOS keeps browser data; the
    // data directory below holds none of it, so wiping the file would leave
    // the cookies that caused the 431 exactly where they were.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let _ = (app, instance_id);
        if let Err(e) = win.clear_all_browsing_data() {
            crate::log_warn!("清理实例的 webview 数据失败: {e}");
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        let _ = win;
        clear_instance_webview_data_from(app, instance_id);
    }
}

/// Whether a stored cookie is one of the instance's own dead DSH
/// browser-auth cookies and should therefore be dropped.
///
/// `name` must carry DSH's `dsh-auth-` prefix, which pins the cookie to this
/// launcher's own scheme instead of any other cookie in the store, and
/// `domain` must be the instance's own host, which keeps the blast radius at
/// the current page's origin. The authority — and with it the port — is
/// deliberately *not* compared: DSH puts `host:port` in the cookie name, so
/// matching the one authority the window is heading for now would leave every
/// cookie an earlier port minted in place for its full 30-day life.
fn should_prune(name: &str, domain: Option<&str>, host: &str) -> bool {
    name.starts_with("dsh-auth-") && domain == Some(host)
}

/// Drops the instance's dead DSH browser-auth cookies immediately before the
/// window is pointed at a fresh token URL. DSH signs those cookies with a
/// secret minted per process, so after an instance restart every stored cookie
/// is dead and the leftovers only inflate the request head — which is exactly
/// what `should_prune` decides, for every authority the instance ever bound.
fn prune_auth_cookies(win: &tauri::WebviewWindow, origin: &str, instance_id: &str) {
    let Ok(origin_url) = origin.parse::<tauri::Url>() else {
        return;
    };
    let Some(host) = origin_url.host_str().map(str::to_string) else {
        return;
    };
    // Tauri documents cookies() as deadlocking in a synchronous command or
    // event handler on Windows; the window event hook is exactly that, so the
    // read stays on a plain worker thread.
    let win = win.clone();
    let id = instance_id.to_string();
    std::thread::spawn(move || {
        let cookies = match win.cookies() {
            Ok(cookies) => cookies,
            Err(e) => {
                crate::log_debug!("实例 {id}：读取 webview cookie 失败: {e}");
                return;
            }
        };
        for cookie in cookies {
            // Every dsh-auth-* cookie on the instance's own host is one of its
            // own: the store is private to the instance, and the domain check
            // still keeps other origins out of the blast radius.
            if !should_prune(cookie.name(), cookie.domain(), host.as_str()) {
                continue;
            }
            if let Err(e) = win.delete_cookie(cookie) {
                crate::log_debug!("实例 {id}：删除失效的会话 cookie 失败: {e}");
            }
        }
    });
}

/// Whether the instance still has a live registry entry (its child process is
/// running). A URL-less window on a registered instance is merely early; on an
/// unregistered one it is a leftover that can never authenticate again.
fn instance_is_running(app: &AppHandle, instance_id: &str) -> bool {
    app.try_state::<AppState>()
        .and_then(|state| {
            state
                .running
                .try_lock()
                .ok()
                .map(|running| running.contains_key(instance_id))
        })
        .unwrap_or(false)
}

/// Remembers the URL an instance's window was navigated to, so a later reopen
/// still carries this process's token after the registry entry is gone.
fn remember_window_url(app: &AppHandle, instance_id: &str, url: &str) {
    if let Some(state) = app.try_state::<AppState>() {
        state
            .window_urls
            .lock()
            .unwrap()
            .insert(instance_id.to_string(), url.to_string());
    }
}

/// The DSH Web URL to load on the next navigation: the one the instance
/// printed most recently, else the last URL this instance's window was
/// navigated to, else the URL captured at spawn time.
fn current_instance_url(app: &AppHandle, instance_id: &str, fallback: &str) -> String {
    let Some(state) = app.try_state::<AppState>() else {
        return fallback.to_string();
    };
    // Never block a native navigation on the registry lock; a miss just falls
    // through to the URL this call was given.
    state
        .running
        .try_lock()
        .ok()
        .and_then(|running| running.get(instance_id).and_then(|entry| entry.url.clone()))
        .or_else(|| state.window_urls.lock().unwrap().get(instance_id).cloned())
        .unwrap_or_else(|| fallback.to_string())
}

/// Opens (or focuses) the webview window hosting the instance's DSH Web GUI.
pub fn open_instance_window(
    app: &AppHandle,
    instance_id: &str,
    name: &str,
    url: &str,
) -> Result<(), String> {
    let label = format!("instance-{instance_id}");
    // Newest URL for this instance: the printed one, else this window's last
    // page. The caller's URL is the spawn-time fallback (empty after the
    // process exited), which keeps a window-reopen able to re-authenticate.
    let url = current_instance_url(app, instance_id, url);
    // Loopback origin of this instance's DSH page, when it has one.
    let origin = loopback_origin(&url);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        if let Some(origin) = origin.as_deref() {
            prune_auth_cookies(&win, origin, instance_id);
        }
        // Re-enter through the current token URL. The window keeps whatever
        // URL it was built with, which is stale after the instance restarted;
        // reloading a token-less / would only re-render the 401 page and the
        // user would have to copy the URL out of the log by hand.
        if url.is_empty() {
            if instance_is_running(app, instance_id) {
                // Still coming up: this window was opened a moment too early, so
                // leave it alone — a later open picks up the token URL.
                return Ok(());
            }
            // Nothing to authenticate with — the launcher was restarted and this
            // process's token is gone. Scratch the instance's WebView2 store
            // instead of leaving a window that can never recover.
            crate::log_warn!("实例 {instance_id}：没有可用的会话地址，已清理该实例的浏览器数据");
            clear_webview_data(&win, app, instance_id);
            return Err("实例未在运行或尚未就绪".to_string());
        }
        let stale = win
            .url()
            .map(|current| current.as_str() != url)
            .unwrap_or(true);
        if stale {
            crate::log_info!("实例 {instance_id}：窗口已打开，重新导航到当前会话地址");
            let parsed = url.parse().map_err(|e| format!("无效的 URL {url}: {e}"))?;
            if let Err(e) = win.navigate(parsed) {
                crate::log_warn!("实例 {instance_id}：重新导航失败: {e}");
            }
            remember_window_url(app, instance_id, &url);
        }
        record_focus(app, instance_id);
        maybe_hide_main(app);
        return Ok(());
    }
    if url.is_empty() {
        return Err("实例未在运行或尚未就绪".to_string());
    }
    let parsed = WebviewUrl::External(url.parse().map_err(|e| format!("无效的 URL {url}: {e}"))?);
    let builder = WebviewWindowBuilder::new(app, label, parsed)
        .title(format!("{name} — DSH"))
        .inner_size(1024.0, 576.0)
        .min_inner_size(800.0, 500.0)
        .center();
    // Own browser data store: cookies of other instances never reach this
    // window. Earlier ports of *this* instance do share it, which is why
    // prune_auth_cookies drops them on every reopen.
    let win = apply_window_store(builder, app, instance_id)
        .build()
        .map_err(|e| e.to_string())?;
    remember_window_url(app, instance_id, &url);
    record_focus(app, instance_id);
    maybe_hide_main(app);

    // Track focus so the tray knows which profile page the user used last.
    let handle = app.clone();
    let id = instance_id.to_string();
    win.on_window_event(move |event| {
        if let WindowEvent::Focused(true) = event {
            record_focus(&handle, &id);
        }
    });
    Ok(())
}

/// Closes the instance's webview window if it is open.
pub fn close_instance_window(app: &AppHandle, instance_id: &str) {
    let label = format!("instance-{instance_id}");
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.close();
    }
}

/// Opens (or focuses) the terminal window hosting a TUI instance's PTY
/// session. The window loads the app's own `/terminal/:id` route; its
/// frontend starts / reattaches the PTY session. Closing the window only
/// detaches the view — the session keeps running in the background.
pub fn open_tui_window(app: &AppHandle, instance_id: &str) -> Result<(), String> {
    let label = format!("tui-{instance_id}");
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        record_focus(app, instance_id);
        maybe_hide_main(app);
        return Ok(());
    }
    let name = app
        .try_state::<AppState>()
        .and_then(|state| {
            state
                .config
                .lock()
                .unwrap()
                .instances
                .iter()
                .find(|i| i.id == instance_id)
                .map(|i| i.name.clone())
        })
        .unwrap_or_else(|| instance_id.to_string());
    // Hash router: the route must arrive in the fragment, or the window
    // would land on `/` and show the launcher home instead of the terminal.
    let url = WebviewUrl::App(format!("/index.html#/terminal/{instance_id}").into());
    let mut builder = WebviewWindowBuilder::new(app, label.clone(), url)
        .title(format!("{name} — DSH TUI"))
        .inner_size(900.0, 560.0)
        .min_inner_size(560.0, 320.0)
        .center();
    // Own WebView2 user-data folder: sharing the default (exe-adjacent)
    // folder with the main window wedges the second webview on a white page.
    if let Some(dir) = app_webview_data_dir(app, &label) {
        builder = builder.data_directory(dir);
    }
    let win = builder.build().map_err(|e| e.to_string())?;
    record_focus(app, instance_id);
    maybe_hide_main(app);

    let handle = app.clone();
    let id = instance_id.to_string();
    win.on_window_event(move |event| {
        if let WindowEvent::Focused(true) = event {
            record_focus(&handle, &id);
        }
    });
    Ok(())
}

/// Closes a TUI instance's terminal window if it is open (detach only; the
/// PTY session is owned by the backend, not the window).
pub fn close_tui_window(app: &AppHandle, instance_id: &str) {
    let label = format!("tui-{instance_id}");
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Both crates are reached only from here (and, for sha2, from the
    // macOS/iOS store block), so the imports live where they are used rather
    // than in the file header, where a Windows build would flag them unused.
    use base64::Engine;
    use sha2::{Digest, Sha256};

    #[test]
    fn loopback_origin_accepts_only_plain_loopback_http() {
        assert_eq!(
            loopback_origin("http://127.0.0.1:10086/?token=abc"),
            Some("http://127.0.0.1:10086".to_string())
        );
        assert_eq!(
            loopback_origin("http://127.0.0.1:49184"),
            Some("http://127.0.0.1:49184".to_string())
        );
        // WSL forwards, remote hosts and https are deliberately out of scope.
        assert_eq!(loopback_origin("http://172.20.0.5:10086/"), None);
        assert_eq!(loopback_origin("http://localhost:10086/"), None);
        assert_eq!(loopback_origin("https://127.0.0.1:10086/"), None);
    }

    /// The name DSH mints for the `host:port` an instance is served on:
    /// `dsh-auth-<base64url(sha256(authority))>`. The launcher no longer
    /// computes this to match cookies — `should_prune` matches the prefix —
    /// but the naming scheme is still what makes the port part of the cookie
    /// name, so it stays pinned here.
    fn authority_cookie_name(authority: &str) -> String {
        format!(
            "dsh-auth-{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(Sha256::digest(authority.as_bytes()))
        )
    }

    /// Pins the cookie name to the one DSH mints; if DSH ever changes that
    /// scheme the names used below would no longer be the ones the browser
    /// holds, and these tests would stop describing reality.
    #[test]
    fn dsh_auth_cookie_name_follows_the_origin_authority() {
        let origin = loopback_origin("http://127.0.0.1:10086/?token=abc").unwrap();
        let authority = origin.strip_prefix("http://").unwrap();
        assert_eq!(
            authority_cookie_name(authority),
            "dsh-auth-k320QAAWVPdxfnOxhyOWdX-dQ03IEKrIdtzGoIirIUY"
        );
    }

    /// An instance that restarts on a fresh port (the default `--port 0`)
    /// keeps its store but gets a new cookie name, so the store holds the
    /// current authority plus one dead cookie per port it ever bound. All of
    /// them must be pruned on the next open — the exact-name match the
    /// launcher used before only ever removed the current one.
    #[test]
    fn should_prune_hits_every_authority_a_port_ever_minted() {
        let historical = [
            authority_cookie_name("127.0.0.1:10086"),
            authority_cookie_name("127.0.0.1:49184"),
        ];
        let current = authority_cookie_name("127.0.0.1:52311");
        let host = "127.0.0.1";
        let names = [&current, &historical[0], &historical[1]];
        let pruned = names
            .iter()
            .filter(|name| should_prune(name, Some(host), host))
            .count();
        assert_eq!(pruned, 3, "every dsh-auth-* cookie on the host is pruned");
        // Regression contrast: the old rule compared against the single name
        // derived from the authority in the URL, so a restart pruned nothing.
        let exact = names.iter().filter(|name| **name == &current).count();
        assert_eq!(
            exact, 1,
            "exact-name matching could only hit the current one"
        );
    }

    /// `should_prune` must not widen past the launcher's own cookies on the
    /// instance's own host — the store is private to the instance, but it is
    /// still a whole browser profile.
    #[test]
    fn should_prune_leaves_foreign_cookies_alone() {
        let host = "127.0.0.1";
        // Another authority's digest is NOT foreign: it is exactly an earlier
        // port's cookie, i.e. the thing this fix exists to delete.
        assert!(should_prune("dsh-auth-something-else", Some(host), host));
        // Genuinely foreign: wrong prefix entirely. The predicate matches the
        // literal `dsh-auth-`, hyphen included, so a name without it is out.
        assert!(!should_prune("session", Some(host), host));
        assert!(!should_prune("dsh-authX", Some(host), host));
        assert!(!should_prune("Dsh-Auth-abc", Some(host), host));
        // Another origin: the domain check is the second half of the rule.
        assert!(!should_prune("dsh-auth-abc", Some("localhost"), host));
        assert!(!should_prune("dsh-auth-abc", None, host));
        // Bare prefix with an empty suffix (a name DSH does not currently
        // mint): matched on purpose, so a future DSH scheme that drops the
        // digest is still pruned instead of silently piling up. Pinned here so
        // widening the prefix rule stays a deliberate decision.
        assert!(should_prune("dsh-auth-", Some(host), host));
    }
}
