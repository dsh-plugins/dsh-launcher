use crate::proxy::Route;
use base64::{engine::general_purpose::STANDARD, Engine};
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Empty, Full};
use hyper::{
    body::{Bytes, Incoming},
    header,
    http::uri::Authority,
    Method, Request, Response, StatusCode, Uri,
};
use hyper_util::{client::proxy::matcher::Matcher, rt::TokioIo};
use std::{
    convert::Infallible,
    io,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    sync::watch,
};
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
type Socket = Box<dyn Stream>;
type Body = UnsyncBoxBody<Bytes, hyper::Error>;
type Failure = Box<dyn std::error::Error + Send + Sync>;

/// 每个浏览数据分区只接收持有随机口令的本机 WebView 连接。
pub(crate) struct BrowserNetwork {
    address: tauri::Url,
    password: String,
    stop: watch::Sender<bool>,
    routes: watch::Sender<Arc<Router>>,
}

impl BrowserNetwork {
    pub(crate) fn start(route: Option<Route>) -> Result<Self, String> {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .map_err(|e| format!("启动侧边栏网络代理失败: {e}"))?;
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let password = uuid::Uuid::new_v4().simple().to_string();
        let auth = format!("Basic {}", STANDARD.encode(format!("launcher:{password}")));
        let (stop, mut stopping) = watch::channel(false);
        let (routes, active) = watch::channel(Arc::new(Router::new(address, route, auth)));
        tauri::async_runtime::spawn(async move {
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(listener) => listener,
                Err(error) => {
                    crate::log_warn!("侧边栏代理无法监听: {error}");
                    return;
                }
            };
            loop {
                let connection = tokio::select! {
                    _ = stopping.changed() => break,
                    result = listener.accept() => result,
                };
                let Ok((socket, _)) = connection else { break };
                let router = active.borrow().clone();
                let mut closed = router.stop.subscribe();
                tokio::spawn(async move {
                    if *closed.borrow() {
                        return;
                    }
                    let service =
                        hyper::service::service_fn(move |request| router.clone().handle(request));
                    let connection = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(socket), service)
                        .with_upgrades();
                    tokio::select! { _ = connection => (), _ = closed.changed() => () }
                });
            }
        });
        Ok(Self {
            address: tauri::Url::parse(&format!("http://{address}")).unwrap(),
            password,
            stop,
            routes,
        })
    }

    pub(crate) fn reconfigure(&self, route: Option<Route>) {
        let current = self.routes.borrow().clone();
        let next = Router::new(current.address, route, current.auth.clone());
        self.routes
            .send_replace(Arc::new(next))
            .stop
            .send_replace(true);
    }

    pub(crate) fn url(&self) -> tauri::Url {
        self.address.clone()
    }
    pub(crate) fn username(&self) -> &str {
        "launcher"
    }
    pub(crate) fn password(&self) -> &str {
        &self.password
    }
}
impl Drop for BrowserNetwork {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        self.routes.borrow().stop.send_replace(true);
    }
}

struct Router {
    address: SocketAddr,
    route: Option<Route>,
    matcher: Matcher,
    auth: String,
    stop: watch::Sender<bool>,
}

impl Router {
    fn new(address: SocketAddr, route: Option<Route>, auth: String) -> Self {
        let matcher = match &route {
            Some(route) => Matcher::builder()
                .all("http://upstream.invalid")
                .no(&route.bypass)
                .build(),
            None => Matcher::builder().build(),
        };
        Self {
            address,
            route,
            matcher,
            auth,
            stop: watch::channel(false).0,
        }
    }

    async fn handle(
        self: Arc<Self>,
        mut request: Request<Incoming>,
    ) -> Result<Response<Body>, Infallible> {
        let supplied = request.headers_mut().remove(header::PROXY_AUTHORIZATION);
        if supplied.as_ref().map(|v| v.as_bytes()) != Some(self.auth.as_bytes()) {
            let mut challenge = reply(
                StatusCode::PROXY_AUTHENTICATION_REQUIRED,
                "需要本地浏览器认证",
            );
            challenge.headers_mut().insert(
                header::PROXY_AUTHENTICATE,
                header::HeaderValue::from_static("Basic realm=\"DSH Launcher\""),
            );
            return Ok(challenge);
        }
        let result = tokio::time::timeout(Duration::from_secs(60), self.forward(request)).await;
        Ok(match result {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => reply(StatusCode::BAD_GATEWAY, "无法连接目标或完成上游代理认证"),
            Err(_) => reply(StatusCode::GATEWAY_TIMEOUT, "代理连接超时"),
        })
    }

    async fn forward(&self, mut req: Request<Incoming>) -> Result<Response<Body>, Failure> {
        let tunnel = req.method() == Method::CONNECT;
        if !tunnel && req.uri().scheme_str() != Some("http") {
            return Ok(reply(StatusCode::BAD_REQUEST, "无效的代理请求"));
        }
        let authority = req.uri().authority().ok_or("缺少目标地址")?.clone();
        if authority.as_str().contains('@') {
            return Ok(reply(StatusCode::BAD_REQUEST, "无效的目标地址"));
        }
        let host = authority.host().trim_matches(&['[', ']'][..]);
        let port = authority
            .port_u16()
            .unwrap_or(if tunnel { 443 } else { 80 });
        // DNS 别名和其他 127/8 写法也不能把本地代理作为目标。
        if port == self.address.port()
            && tokio::net::lookup_host((host, port))
                .await?
                .any(|addr| addr.ip().is_loopback() || addr.ip().is_unspecified())
        {
            return Ok(reply(
                StatusCode::FORBIDDEN,
                "不能通过侧边栏访问代理监听端口",
            ));
        }
        let destination: Uri =
            format!("{}://{authority}/", if tunnel { "https" } else { "http" }).parse()?;
        let route = self
            .route
            .as_ref()
            .filter(|_| self.matcher.intercept(&destination).is_some());
        let (socket, absolute_form) = self.connect(host, port, &authority, route, tunnel).await?;
        if tunnel {
            let upgrade = hyper::upgrade::on(&mut req);
            self.tunnel(
                async move { Ok((Box::new(TokioIo::new(upgrade.await?)) as Socket, socket)) },
            );
            return Ok(reply(StatusCode::OK, ""));
        }
        let request_upgrade = req
            .headers()
            .contains_key(header::UPGRADE)
            .then(|| hyper::upgrade::on(&mut req));
        req.headers_mut().remove("proxy-connection");
        req.headers_mut().remove(header::PROXY_AUTHENTICATE);
        req.headers_mut()
            .insert(header::HOST, authority.as_str().parse()?);
        if absolute_form {
            if let Some(value) = route.and_then(upstream_auth) {
                req.headers_mut().insert(header::PROXY_AUTHORIZATION, value);
            }
        } else {
            *req.uri_mut() = req
                .uri()
                .path_and_query()
                .map(|p| p.as_str())
                .unwrap_or("/")
                .parse()?;
        }
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(socket)).await?;
        tokio::spawn(connection.with_upgrades());
        let mut response = sender.send_request(req).await?;
        if response.status() == StatusCode::PROXY_AUTHENTICATION_REQUIRED {
            return Ok(reply(StatusCode::BAD_GATEWAY, "上游代理拒绝了账号或密码"));
        }
        if let Some(client) =
            request_upgrade.filter(|_| response.status() == StatusCode::SWITCHING_PROTOCOLS)
        {
            let server = hyper::upgrade::on(&mut response);
            self.tunnel(async move {
                Ok((
                    Box::new(TokioIo::new(client.await?)) as Socket,
                    Box::new(TokioIo::new(server.await?)) as Socket,
                ))
            });
        }
        Ok(response.map(|body| body.boxed_unsync()))
    }

    fn tunnel(
        &self,
        open: impl std::future::Future<Output = Result<(Socket, Socket), Failure>> + Send + 'static,
    ) {
        let mut stop = self.stop.subscribe();
        tokio::spawn(async move {
            if *stop.borrow() {
                return;
            }
            tokio::select! {
                _ = stop.changed() => (),
                _ = async { if let Ok((mut client,mut remote)) = open.await { let _ = tokio::io::copy_bidirectional(&mut client,&mut remote).await; } } => (),
            }
        });
    }

    async fn connect(
        &self,
        host: &str,
        port: u16,
        authority: &Authority,
        route: Option<&Route>,
        tunnel: bool,
    ) -> Result<(Socket, bool), Failure> {
        let Some(route) = route else {
            return Ok((Box::new(TcpStream::connect((host, port)).await?), false));
        };
        let proxy_host = route
            .endpoint
            .host_str()
            .ok_or("无效的代理地址")?
            .trim_matches(&['[', ']'][..]);
        let proxy = (proxy_host, route.port);
        if matches!(route.endpoint.scheme(), "socks5" | "socks5h") {
            let socket = match &route.auth {
                Some((name, pass)) => {
                    tokio_socks::tcp::Socks5Stream::connect_with_password(
                        proxy,
                        (host, port),
                        name,
                        pass,
                    )
                    .await?
                }
                None => tokio_socks::tcp::Socks5Stream::connect(proxy, (host, port)).await?,
            };
            return Ok((Box::new(socket.into_inner()), false));
        }
        let tcp = TcpStream::connect(proxy).await?;
        let socket: Socket = if route.endpoint.scheme() == "https" {
            let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let tls = ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let connector = tokio_rustls::TlsConnector::from(Arc::new(tls));
            Box::new(
                connector
                    .connect(proxy_host.to_owned().try_into()?, tcp)
                    .await?,
            )
        } else {
            Box::new(tcp)
        };
        if !tunnel {
            return Ok((socket, true));
        }
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(socket)).await?;
        tokio::spawn(connection.with_upgrades());
        let mut connect = Request::builder()
            .method(Method::CONNECT)
            .uri(authority.as_str())
            .header(header::HOST, authority.as_str());
        if let Some(value) = upstream_auth(route) {
            connect = connect.header(header::PROXY_AUTHORIZATION, value);
        }
        let mut response = sender
            .send_request(connect.body(Empty::<Bytes>::new())?)
            .await?;
        if !response.status().is_success() {
            return Err(io::Error::other("上游代理拒绝连接").into());
        }
        Ok((
            Box::new(TokioIo::new(hyper::upgrade::on(&mut response).await?)),
            false,
        ))
    }
}

fn upstream_auth(route: &Route) -> Option<header::HeaderValue> {
    route.auth.as_ref().map(|(user, pass)| {
        let mut value: header::HeaderValue =
            format!("Basic {}", STANDARD.encode(format!("{user}:{pass}")))
                .parse()
                .unwrap();
        value.set_sensitive(true);
        value
    })
}

fn reply(status: StatusCode, text: &'static str) -> Response<Body> {
    let body = Full::new(Bytes::from_static(text.as_bytes()))
        .map_err(|never| match never {})
        .boxed_unsync();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(body)
        .unwrap()
}

#[cfg(test)]
mod tests;
