mod relay;
#[cfg(test)]
mod tests;

use crate::web_access::{authority, WebAccess, WebMode};
use ipnet::IpNet;
use regex::Regex;
use relay::Relay;
use std::borrow::Cow;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::Url;

/// 宿主机对外端口和发行版内部端口分别分配，适用于 WSL 的 NAT 与镜像网络。
#[derive(Clone)]
pub struct WslWeb {
    distro: String,
    node: String,
    proxy_port: u16,
}

impl WslWeb {
    pub async fn resolve(
        distro: String,
        node: String,
        access: &WebAccess,
        external_port: Option<u16>,
    ) -> Result<Self, String> {
        let proxy_port = if access.mode == WebMode::Proxy {
            // DSH 设置 public-url 后只公布代理根地址，因此必须在启动前得知发行版内的端口。
            let script = r#"const net = require('node:net');
const externalPort = Number(process.argv[1]);
function selectPort() {
    const listener = net.createServer();
    listener.listen(0, '127.0.0.1', () => {
        const port = listener.address().port;
        listener.close(() => {
            if (port === externalPort) {
                selectPort();
            } else {
                console.log(port);
            }
        });
    });
}
selectPort();"#;
            let command = format!(
                "exec {} -e {} -- {}",
                crate::wsl::sh_quote(&node),
                crate::wsl::sh_quote(script),
                external_port.unwrap_or(0)
            );
            let out =
                crate::wsl::wsl_output(&distro, &["bash".into(), "-c".into(), command]).await?;
            out.trim()
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0 && Some(*p) != external_port)
                .ok_or("无法分配 WSL 回环端口")?
        } else {
            0
        };
        Ok(Self {
            distro,
            node,
            proxy_port,
        })
    }
}

/// 一次启动使用的参数与网络入口，运行期间独立于之后保存的配置。
pub struct WebLaunch {
    pub args: Vec<String>,
    pub mode: WebMode,
    host: IpAddr,
    port: u16,
    tls: bool,
    public: Option<Url>,
    relay: Option<Relay>,
    desktop_relay: Option<Relay>,
    pub networks: Vec<IpNet>,
    wsl: Option<WslWeb>,
}

/// 带本进程凭据的两个入口，只允许在打开窗口与主动分享时读取。
pub struct WebLinks {
    pub desktop: Url,
    pub share: Url,
}

impl WebLinks {
    pub fn local_output(raw: &str) -> Result<Self, String> {
        let url = Url::parse(raw).map_err(|_| "实例输出的本机地址无效")?;
        let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']);
        if !matches!(url.scheme(), "https" | "http")
            || !(host == "localhost"
                || host
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| ip.to_canonical().is_loopback()))
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("实例输出的本机地址无效".into());
        }
        Ok(Self {
            desktop: url.clone(),
            share: url,
        })
    }

    pub fn display(&self) -> String {
        let mut url = self.share.clone();
        url.set_query(None);
        url.set_fragment(None);
        url.into()
    }
}

pub fn redact_tokens(line: &str) -> Cow<'_, str> {
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    TOKEN
        .get_or_init(|| {
            Regex::new(r#"(?i)([?&](?:t|%74)(?:o|%6f)(?:k|%6b)(?:e|%65)(?:n|%6e)=)[^&#\s"'<>]+"#)
                .unwrap()
        })
        .replace_all(line, "${1}[redacted]")
}

pub fn startup_script(version: &Path, profile: &Path) -> String {
    let bundle = "node_modules/@deepseek-ai/dsh-web-app/lib/startup.js";
    [
        version.join("packages/bundle/web-app/lib/startup.js"),
        version.join(bundle),
        version.join("node_modules/.pnpm").join(bundle),
        profile.join(bundle),
    ]
    .iter()
    .find_map(|p| std::fs::read_to_string(p).ok())
    .unwrap_or_default()
}

async fn tls_path(raw: &str, distro: Option<&str>) -> Result<String, String> {
    let path = if let Some(distro) = distro {
        if let Some((owner, linux)) = crate::tui_cwd::parse_wsl_unc(raw) {
            if !owner.eq_ignore_ascii_case(distro) {
                return Err(format!("TLS 文件属于发行版 {owner}，当前实例使用 {distro}"));
            }
            linux
        } else if raw.starts_with('/') && !raw.starts_with("//") {
            raw.to_string()
        } else {
            crate::wsl::to_wsl_path(distro, raw).await?
        }
    } else {
        raw.to_string()
    };
    let file = match distro {
        Some(d) if path.starts_with('/') => crate::wsl::unc_path(d, &path),
        None if Path::new(&path).is_absolute() => PathBuf::from(&path),
        _ => return Err(format!("TLS 文件需要绝对路径: {raw}")),
    };
    let meta = std::fs::File::open(&file)
        .and_then(|f| f.metadata())
        .map_err(|e| format!("无法读取 TLS 文件 {path}: {e}"))?;
    if !meta.is_file() || meta.len() == 0 {
        return Err(format!("TLS 文件为空或不是普通文件: {path}"));
    }
    Ok(path)
}

async fn verify_tls(cert: &str, key: &str, wsl: Option<&WslWeb>) -> Result<(), String> {
    // 使用实例的 Node/OpenSSL 提前检查证书链和私钥是否配对，只返回错误代码。
    let check = r#"const fs = require('node:fs');
try {
    require('node:tls').createSecureContext({
        cert: fs.readFileSync(process.argv[1]),
        key: fs.readFileSync(process.argv[2])
    });
} catch (error) {
    console.error(error.code || 'INVALID_PEM');
    process.exitCode = 1;
}"#;
    let mut cmd = if let Some(guest) = wsl {
        let script = [guest.node.as_str(), "-e", check, "--", cert, key]
            .map(crate::wsl::sh_quote)
            .join(" ");
        crate::wsl::wsl_cmd(
            &guest.distro,
            &["bash".into(), "-c".into(), format!("exec {script}")],
        )
    } else {
        let mut cmd = tokio::process::Command::new(crate::process::node());
        crate::process::hide_console(&mut cmd);
        cmd.args(["-e", check, "--", cert, key]);
        cmd
    };
    let output = cmd
        .output()
        .await
        .map_err(|e| format!("无法启动 Node 校验 TLS 材料: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "证书链或私钥无效，请检查 PEM 格式、密钥配对和文件权限（{}）",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

impl WebLaunch {
    pub async fn prepare(
        access: WebAccess,
        port: Option<u16>,
        startup: &str,
        wsl: Option<WslWeb>,
    ) -> Result<Self, String> {
        let access = access.normalized(port)?;
        let mode = access.mode;
        let external_ip = access.address.is_some();
        let relayed = mode == WebMode::Lan || (external_ip && wsl.is_some());
        let mut hosts = access.trusted_hosts;
        let public = access
            .public_url
            .as_deref()
            .map(Url::parse)
            .transpose()
            .map_err(|e| e.to_string())?;
        if let Some(url) = &public {
            hosts.push(authority(url));
        }
        let require = |flag: &str| {
            if startup.contains(&format!("{flag} <")) {
                Ok(())
            } else {
                Err(format!(
                    "此 DSH 版本的 Web 包缺少 {flag}，请升级 DSH 或更改访问方式"
                ))
            }
        };
        if mode != WebMode::Local {
            require("--trusted-host")?;
        }
        if public.is_some() {
            require("--public-url")?;
        }
        if external_ip
            && !relayed
            && !startup.contains("isWildcardHost")
            && !startup.contains("one concrete IPv4")
        {
            return Err("此 DSH 版本不能绑定具体网卡 IP，请升级 DSH 或使用局域网模式".into());
        }
        let mut args = Vec::new();
        if let Some(files) = &access.tls {
            require("--tls-cert")?;
            require("--tls-key")?;
            let distro = wsl.as_ref().map(|guest| guest.distro.as_str());
            let cert = tls_path(&files.cert, distro).await?;
            let key = tls_path(&files.key, distro).await?;
            verify_tls(&cert, &key, wsl.as_ref()).await?;
            args.extend(["--tls-cert".into(), cert, "--tls-key".into(), key]);
        }
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let address = access.address.unwrap_or(loopback);
        let port = port.unwrap_or(0);
        let desktop_relay = if wsl.is_some() {
            // Windows 的本地入口必须由启动器独占，避免把令牌交给同号端口的其他程序。
            let local_port = if matches!(mode, WebMode::Lan | WebMode::Direct) {
                0
            } else {
                port
            };
            Some(Relay::bind(SocketAddr::new(loopback, local_port), None).await?)
        } else {
            None
        };
        let relay = if relayed {
            let networks = (mode == WebMode::Lan).then_some(access.networks.clone());
            let relay = Relay::bind(SocketAddr::new(address, port), networks).await?;
            hosts.push(relay.address.to_string());
            Some(relay)
        } else {
            None
        };
        let host = if relayed { loopback } else { address };
        let port = match &wsl {
            Some(guest) => guest.proxy_port,
            None if relayed => 0,
            None => port,
        };
        args.extend([
            "--host".into(),
            host.to_string(),
            "--port".into(),
            port.to_string(),
        ]);
        if let Some(url) = &public {
            args.extend(["--public-url".into(), url.to_string()]);
        }
        if !hosts.is_empty() {
            hosts.sort();
            hosts.dedup();
            args.push("--trusted-host".into());
            args.extend(hosts);
        }
        if startup.contains("no-open") {
            args.push("--no-open".into());
        }
        Ok(Self {
            args,
            mode,
            host,
            port,
            tls: access.tls.is_some(),
            public,
            relay,
            desktop_relay,
            wsl,
            networks: access.networks,
        })
    }

    pub fn check_native_port(&self) -> Result<(), String> {
        if self.wsl.is_none() && (self.port != 0 || !self.host.is_loopback()) {
            let address = SocketAddr::new(self.host, self.port);
            std::net::TcpListener::bind(address).map_err(|e| format!("无法监听 {address}: {e}"))?;
        }
        Ok(())
    }

    pub fn ready(&mut self, raw: &str) -> Result<WebLinks, String> {
        let announced = Url::parse(raw).map_err(|_| "DSH 输出的启动地址无效")?;
        let scheme = if self.tls || (self.mode == WebMode::Local && announced.scheme() == "https") {
            "https"
        } else {
            "http"
        };
        if !matches!(announced.scheme(), "http" | "https")
            || !announced.username().is_empty()
            || announced.password().is_some()
            || announced.fragment().is_some()
        {
            return Err("DSH 输出的启动地址不是可用的 HTTP(S) 入口".into());
        }
        let mut tokens = announced.query_pairs().filter(|(key, _)| key == "token");
        let token = tokens.next().map(|(_, v)| v.into_owned());
        if self.mode != WebMode::Local
            && (token.as_deref().is_none_or(str::is_empty) || tokens.next().is_some())
        {
            return Err("DSH 未提供有效的登录令牌，已关闭实例网络入口".into());
        }
        let with_token = |socket: SocketAddr| -> Result<Url, String> {
            let mut root =
                Url::parse(&format!("{scheme}://{socket}/")).map_err(|e| e.to_string())?;
            if let Some(token) = &token {
                root.query_pairs_mut().append_pair("token", token);
            }
            Ok(root)
        };
        if let Some(public) = &self.public {
            if announced.origin() != public.origin() || announced.path() != public.path() {
                return Err("DSH 公布的入口与已配置的反向代理地址不符".into());
            }
            let backend = SocketAddr::new(self.host, self.port);
            let desktop = with_token(self.desktop_endpoint(backend))?;
            if let Some(relay) = &mut self.relay {
                relay.start(backend, self.wsl.clone());
            }
            return Ok(WebLinks {
                desktop,
                share: announced,
            });
        }
        let port = announced
            .port_or_known_default()
            .ok_or("DSH 启动地址缺少端口")?;
        let host = announced
            .host_str()
            .unwrap_or_default()
            .trim_matches(['[', ']']);
        let local_match = host
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.to_canonical() == self.host)
            || (host == "localhost" && self.host.is_loopback());
        if !local_match
            || (self.port != 0 && self.port != port)
            || announced.scheme() != scheme
            || announced.path() != "/"
        {
            return Err("DSH 公布的协议、监听地址或端口与本次启动配置不符".into());
        }
        let backend = SocketAddr::new(self.host, port);
        let desktop = if self.desktop_relay.is_some() {
            with_token(self.desktop_endpoint(backend))?
        } else {
            announced
        };
        let share = if let Some(relay) = &mut self.relay {
            let url = with_token(relay.address)?;
            relay.start(SocketAddr::new(self.host, port), self.wsl.clone());
            url
        } else {
            desktop.clone()
        };
        Ok(WebLinks { desktop, share })
    }

    fn desktop_endpoint(&mut self, backend: SocketAddr) -> SocketAddr {
        if let Some(relay) = &mut self.desktop_relay {
            relay.start(backend, self.wsl.clone());
            return relay.address;
        }
        backend
    }

    pub async fn close(&mut self) {
        if let Some(relay) = &mut self.desktop_relay {
            relay.close().await;
        }
        if let Some(relay) = &mut self.relay {
            relay.close().await;
        }
    }
}
