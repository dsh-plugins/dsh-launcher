use if_addrs::{IfAddr, IfOperStatus};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};
use tauri::Url;

/// 实例实际启动的 Web 访问范围。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebMode {
    #[default]
    Local,
    Lan,
    Direct,
    Proxy,
}

/// TLS 材料由实例进程加载，启动器只保存文件路径。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TlsFiles {
    pub cert: String,
    pub key: String,
}

/// 一次保存的 Web 网络设置，端口沿用实例已有的 port 字段。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebAccess {
    pub mode: WebMode,
    pub address: Option<IpAddr>,
    pub networks: Vec<IpNet>,
    pub public_url: Option<String>,
    pub trusted_hosts: Vec<String>,
    pub tls: Option<TlsFiles>,
}

/// 可从其他设备访问的本机网卡地址及其直连子网。
#[derive(Serialize)]
pub struct WebInterface {
    pub name: String,
    pub address: IpAddr,
    pub subnet: String,
}

fn external_address(ip: IpAddr) -> bool {
    let ip = ip.to_canonical();
    !ip.is_loopback()
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && ip != Ipv4Addr::BROADCAST
        && !matches!(ip, IpAddr::V6(v6) if v6.is_unicast_link_local())
}

#[tauri::command]
pub fn list_web_interfaces() -> Result<Vec<WebInterface>, String> {
    let interfaces = if_addrs::get_if_addrs().map_err(|e| format!("读取网卡地址失败: {e}"))?;
    let mut result = Vec::new();
    for interface in interfaces {
        let address = interface.ip();
        if !external_address(address)
            || !matches!(
                interface.oper_status,
                IfOperStatus::Up | IfOperStatus::Unknown
            )
        {
            continue;
        }
        let prefix = match interface.addr {
            IfAddr::V4(v4) => v4.prefixlen,
            IfAddr::V6(v6) => v6.prefixlen,
        };
        let subnet = IpNet::new(address, prefix)
            .map_err(|e| e.to_string())?
            .trunc();
        result.push(WebInterface {
            name: interface.name,
            address,
            subnet: subnet.to_string(),
        });
    }
    result.sort_by(|a, b| a.name.cmp(&b.name).then(a.address.cmp(&b.address)));
    Ok(result)
}

fn public_root(value: &str) -> Result<Url, String> {
    let invalid = || "公开入口必须为含主机名的 HTTP(S) 根地址，可包含路径前缀".to_string();
    let (scheme, rest) = value.split_once("://").ok_or_else(invalid)?;
    let raw_authority = rest.split('/').next().unwrap_or_default();
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") || raw_authority.is_empty()
    {
        return Err(invalid());
    }
    if raw_authority.contains('@')
        || value.contains(['?', '#', '\\'])
        || value.bytes().any(|b| b <= 0x20 || b == 0x7f)
    {
        return Err("公开入口不能包含凭据、查询参数、片段、空白或反斜杠".into());
    }
    let mut url = Url::parse(value).map_err(|_| invalid())?;
    trusted_authority(&authority(&url))?;
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

pub(crate) fn authority(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

fn trusted_authority(raw: &str) -> Result<String, String> {
    let invalid = || format!("可信主机必须为规范的主机名或 host:port（IPv6 加方括号）: {raw}");
    if raw.is_empty()
        || raw.starts_with('-')
        || raw.contains(['/', '\\', '?', '#', '@', '*'])
        || raw.bytes().any(|b| b <= 0x20 || b == 0x7f)
    {
        return Err(invalid());
    }
    // 使用不同默认端口验证显式 :80 和 :443，避免它们变成任意端口授权。
    for scheme in ["http", "https"] {
        let url = Url::parse(&format!("{scheme}://{raw}")).map_err(|_| invalid())?;
        if authority(&url).eq_ignore_ascii_case(raw) {
            return Ok(raw.to_ascii_lowercase());
        }
    }
    Err(invalid())
}

impl WebAccess {
    pub fn normalized(mut self, port: Option<u16>) -> Result<Self, String> {
        if port == Some(0) {
            return Err("固定端口必须在 1 至 65535 之间".into());
        }
        if matches!(self.mode, WebMode::Lan | WebMode::Direct)
            || (self.mode == WebMode::Proxy && self.address.is_some())
        {
            let ip = self
                .address
                .ok_or("请选择具体的本机网卡地址")?
                .to_canonical();
            if !external_address(ip) {
                return Err("请选择具体的非回环网卡地址，IPv6 需使用可路由的地址".into());
            }
            self.address = Some(ip);
        } else {
            self.address = None;
        }
        if self.mode == WebMode::Lan {
            self.networks = IpNet::aggregate(&self.networks);
            if self.networks.is_empty() || self.networks.iter().any(|net| net.prefix_len() == 0) {
                return Err("局域网白名单必须包含明确的 CIDR 网段，不能覆盖整个地址族".into());
            }
        } else {
            self.networks.clear();
        }
        if self.mode == WebMode::Proxy {
            if port.is_none() {
                return Err("反向代理需要固定的后端端口".into());
            }
            let url = public_root(self.public_url.as_deref().unwrap_or_default())?;
            if self.tls.is_some() && url.scheme() != "https" {
                return Err(
                    "后端启用 TLS 时，浏览器的公开入口也必须使用 HTTPS 才能发送会话 Cookie".into(),
                );
            }
            self.public_url = Some(url.into());
        } else {
            self.public_url = None;
        }
        if self.mode == WebMode::Local {
            self.trusted_hosts.clear();
        } else {
            self.trusted_hosts = self
                .trusted_hosts
                .iter()
                .map(|s| trusted_authority(s))
                .collect::<Result<_, _>>()?;
            self.trusted_hosts.sort();
            self.trusted_hosts.dedup();
        }
        if let Some(tls) = &self.tls {
            for path in [&tls.cert, &tls.key] {
                if path.is_empty()
                    || path.contains("-----BEGIN")
                    || path.chars().any(char::is_control)
                    || !(std::path::Path::new(path).is_absolute() || path.starts_with('/'))
                {
                    return Err("TLS 证书链和私钥都需要填写文件的绝对路径".into());
                }
            }
        }
        Ok(self)
    }
}
