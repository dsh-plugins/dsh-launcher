use crate::config::LauncherSettings;
use std::sync::RwLock;
use tauri::Url;

/// 上游代理的账号与密码只在构建网络请求时进入内存。
#[derive(Clone)]
pub(crate) struct Route {
    pub endpoint: Url,
    pub port: u16,
    pub bypass: String,
    pub auth: Option<(String, String)>,
}

pub(crate) fn validate_address(address: &str, port: u16) -> Result<Url, String> {
    let invalid = "请输入有效的代理地址与端口，账号和密码请填在独立字段中";
    let mut url = Url::parse(address.trim()).map_err(|_| invalid)?;
    if port == 0
        || !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid.into());
    }
    url.set_port(Some(port)).map_err(|_| invalid)?;
    Ok(url)
}

fn entry(scope: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new("in.dsh-plug.dsh-launcher.proxy", scope)
        .map_err(|_| "系统凭据存储不可用".into())
}

fn read_record(scope: &str) -> Result<Option<String>, String> {
    match entry(scope)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err("读取代理密码失败，请检查系统凭据存储是否已解锁".into()),
    }
}

// 代理主机或账号变更后，不向新地址发送旧凭据。
pub(crate) fn password(
    scope: &str,
    endpoint: &Url,
    username: &str,
) -> Result<Option<String>, String> {
    let Some(record) = read_record(scope)? else {
        return Ok(None);
    };
    let (address, account, value): (String, String, String) =
        serde_json::from_str(&record).map_err(|_| "代理凭据格式无效，请重新保存密码")?;
    Ok((address == endpoint.as_str() && account == username).then_some(value))
}

pub(crate) fn forget_password(scope: &str) -> Result<(), String> {
    set_password(scope, None)
}

fn set_password(scope: &str, value: Option<&str>) -> Result<(), String> {
    let entry = entry(scope)?;
    let result = match value {
        Some(value) if !value.is_empty() => entry.set_password(value),
        _ => match entry.delete_credential() {
            Err(keyring::Error::NoEntry) => Ok(()),
            result => result,
        },
    };
    result.map_err(|_| "写入系统代理凭据失败".into())
}

pub(crate) fn with_password<R>(
    scope: &str,
    address: &str,
    port: u16,
    user: &str,
    update: Option<&str>,
    commit: impl FnOnce() -> Result<R, String>,
) -> Result<R, String> {
    let Some(value) = update else { return commit() };
    let record = if value.is_empty() {
        None
    } else {
        let url = validate_address(address, port)?;
        Some(serde_json::to_string(&(url.as_str(), user, value)).map_err(|_| "无法保存代理凭据")?)
    };
    let previous = read_record(scope)?;
    set_password(scope, record.as_deref())?;
    match commit() {
        Ok(value) => Ok(value),
        Err(err) => {
            set_password(scope, previous.as_deref())
                .map_err(|e| format!("{err}；恢复代理凭据失败: {e}"))?;
            Err(err)
        }
    }
}

impl Route {
    pub(crate) fn load(
        address: &str,
        port: u16,
        bypass: &str,
        username: &str,
        scope: &str,
    ) -> Result<Self, String> {
        let endpoint = validate_address(address, port)?;
        if matches!(endpoint.scheme(), "http" | "https") && username.contains(':') {
            return Err("HTTP 代理用户名不能包含冒号".into());
        }
        let auth = if username.is_empty() {
            None
        } else {
            let pass = password(scope, &endpoint, username)?.ok_or("代理密码尚未配置")?;
            Some((username.into(), pass))
        };
        Ok(Self {
            endpoint,
            port,
            bypass: bypass.into(),
            auth,
        })
    }

    pub(crate) fn http_client_proxy(&self) -> Result<reqwest::Proxy, String> {
        Ok(reqwest::Proxy::all(self.authenticated_url()?)
            .map_err(|_| "无效的启动器代理配置")?
            .no_proxy(reqwest::NoProxy::from_string(&self.bypass)))
    }

    fn authenticated_url(&self) -> Result<Url, String> {
        let mut url = self.endpoint.clone();
        if let Some((user, pass)) = &self.auth {
            let user =
                percent_encoding::utf8_percent_encode(user, percent_encoding::NON_ALPHANUMERIC)
                    .to_string();
            let pass =
                percent_encoding::utf8_percent_encode(pass, percent_encoding::NON_ALPHANUMERIC)
                    .to_string();
            url.set_username(&user).map_err(|_| "无法设置代理用户名")?;
            url.set_password(Some(&pass))
                .map_err(|_| "无法设置代理密码")?;
        }
        Ok(url)
    }
}

pub(crate) fn launcher_route(s: &LauncherSettings) -> Result<Option<Route>, String> {
    if !s.proxy_enabled {
        return Ok(None);
    }
    Route::load(
        &s.proxy_url,
        s.proxy_port,
        &s.no_proxy,
        &s.proxy_username,
        "launcher",
    )
    .map(Some)
}

static CURRENT: RwLock<Result<Option<Route>, String>> = RwLock::new(Ok(None));

pub fn sync_from_settings(settings: &LauncherSettings) {
    *CURRENT.write().unwrap() = launcher_route(settings);
}

pub fn apply(builder: reqwest::ClientBuilder) -> Result<reqwest::ClientBuilder, String> {
    let active = CURRENT.read().unwrap();
    let Some(route) = active.as_ref().map_err(Clone::clone)? else {
        return Ok(builder);
    };
    let proxy = route.http_client_proxy()?;
    Ok(builder.proxy(proxy))
}

const PROXY_KEYS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
];

pub fn override_env(
    env: &mut Vec<(String, String)>,
    settings: &LauncherSettings,
) -> Result<(), String> {
    let Some(route) = launcher_route(settings)? else {
        return Ok(());
    };
    let server = route
        .authenticated_url()?
        .as_str()
        .trim_end_matches('/')
        .to_owned();
    env.retain(|(key, _)| !PROXY_KEYS.iter().any(|p| p.eq_ignore_ascii_case(key)));
    for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
        env.push((key.into(), server.clone()));
    }
    for key in ["NO_PROXY", "no_proxy"] {
        env.push((key.into(), route.bypass.clone()));
    }
    Ok(())
}

/// 子进程的诊断可能直接打印代理 URL。
pub(crate) fn redact_credentials(line: &str) -> std::borrow::Cow<'_, str> {
    static URL_CREDENTIALS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)((?:https?|socks5h?)://)[^/@\s"'<>]+:[^/@\s"'<>]+@"#).unwrap()
    });
    URL_CREDENTIALS.replace_all(line, "${1}[redacted]@")
}
