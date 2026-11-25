use super::{BrowserNetwork, Route, STANDARD};
use base64::Engine;
use reqwest::{header, Client, Proxy, StatusCode};
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::timeout,
};

const USER: &str = "user@/ %";
const PASSWORD: &str = "p@:%/?# 密码";
const TIMEOUT: Duration = Duration::from_secs(5);

fn route(address: SocketAddr, protocol: &str, bypass: &str) -> Route {
    Route {
        endpoint: format!("{protocol}://{address}").parse().unwrap(),
        port: address.port(),
        bypass: bypass.into(),
        auth: Some((USER.into(), PASSWORD.into())),
    }
}

fn client(proxy: Proxy) -> Client {
    Client::builder()
        .no_proxy()
        .proxy(proxy)
        .timeout(TIMEOUT)
        .user_agent("DSH-Test/1")
        .build()
        .unwrap()
}

fn sidebar(network: &BrowserNetwork) -> Client {
    client(
        Proxy::all(network.url())
            .unwrap()
            .basic_auth(network.username(), network.password()),
    )
}

async fn listener() -> TcpListener {
    TcpListener::bind(("127.0.0.1", 0)).await.unwrap()
}

async fn read_head(stream: &mut TcpStream) -> String {
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    while !head.ends_with("\r\n\r\n") {
        assert!(head.len() < 16384);
        assert!(reader.read_line(&mut head).await.unwrap() > 0);
    }
    head
}

async fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await.unwrap();
}

async fn http_endpoint(
    status: &'static str,
    body: &'static str,
) -> (SocketAddr, JoinHandle<String>) {
    let server = listener().await;
    let addr = server.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = server.accept().await.unwrap();
        let head = read_head(&mut stream).await;
        respond(&mut stream, status, body).await;
        head
    });
    (addr, task)
}

fn value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
}

fn upstream_basic() -> String {
    format!("Basic {}", STANDARD.encode(format!("{USER}:{PASSWORD}")))
}

#[tokio::test]
async fn connect_authenticates_to_the_upstream_and_relays_bidirectional_bytes() {
    let server = listener().await;
    let upstream = route(server.local_addr().unwrap(), "http", "");
    let task = tokio::spawn(async move {
        let (mut stream, _) = server.accept().await.unwrap();
        let request = read_head(&mut stream).await;
        assert!(request.starts_with("CONNECT example.invalid:443 HTTP/1.1"));
        assert_eq!(
            value(&request, "proxy-authorization"),
            Some(upstream_basic().as_str())
        );
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .unwrap();
        let mut bytes = [0; 4];
        stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"ping");
        stream.write_all(b"pong").await.unwrap();
    });
    let network = BrowserNetwork::start(Some(upstream)).unwrap();
    timeout(TIMEOUT, async {
        let mut stream = TcpStream::connect(network.url().socket_addrs(|| None).unwrap()[0]).await.unwrap();
        let auth = STANDARD.encode(format!("{}:{}", network.username(), network.password()));
        let request = format!("CONNECT example.invalid:443 HTTP/1.1\r\nHost: example.invalid:443\r\nProxy-Authorization: Basic {auth}\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        assert!(read_head(&mut stream).await.starts_with("HTTP/1.1 200"));
        stream.write_all(b"ping").await.unwrap();
        let mut bytes = [0; 4];
        stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"pong");
        task.await.unwrap();
    }).await.unwrap();
}

#[tokio::test]
async fn changing_proxy_closes_existing_tunnels_without_changing_the_listener() {
    let origin = listener().await;
    let destination = origin.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.unwrap();
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
    });
    let browser = BrowserNetwork::start(None).unwrap();
    let endpoint = browser.url();
    let address = endpoint.socket_addrs(|| None).unwrap()[0];
    let token = STANDARD.encode(format!("{}:{}", browser.username(), browser.password()));
    timeout(TIMEOUT, async {
        let mut socket = TcpStream::connect(address).await.unwrap();
        let connect = format!(
            "CONNECT {destination} HTTP/1.1\r\nHost: {destination}\r\nProxy-Authorization: Basic {token}\r\n\r\n"
        );
        socket.write_all(connect.as_bytes()).await.unwrap();
        assert!(read_head(&mut socket).await.starts_with("HTTP/1.1 200"));
        browser.reconfigure(None);
        assert_eq!(browser.url(), endpoint);
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
        server.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn http_proxy_receives_upstream_credentials_and_absolute_target() {
    for native_sidebar in [true, false] {
        let (addr, received) = http_endpoint("200 OK", "proxy").await;
        let upstream = route(addr, "http", "");
        let network;
        let http = if native_sidebar {
            network = BrowserNetwork::start(Some(upstream)).unwrap();
            sidebar(&network)
        } else {
            client(upstream.http_client_proxy().unwrap())
        };
        let response = http
            .get("http://example.invalid/path?q=yes")
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "proxy");
        let head = timeout(TIMEOUT, received).await.unwrap().unwrap();
        assert!(head.starts_with("GET http://example.invalid/path?q=yes HTTP/1.1\r\n"));
        assert_eq!(
            value(&head, "proxy-authorization"),
            Some(upstream_basic().as_str())
        );
        assert_eq!(value(&head, "user-agent"), Some("DSH-Test/1"));
    }
}

#[tokio::test]
async fn direct_and_bypassed_requests_keep_credentials_out_of_origin_headers() {
    let unused = listener().await;
    let upstream = route(unused.local_addr().unwrap(), "http", "127.0.0.1");
    let network = BrowserNetwork::start(None).unwrap();
    let browser = sidebar(&network);
    let launcher = client(upstream.http_client_proxy().unwrap());
    for (index, http) in [&browser, &browser, &launcher].into_iter().enumerate() {
        if index == 1 {
            network.reconfigure(Some(upstream.clone()));
        }
        let (dest, received) = http_endpoint("200 OK", "origin").await;
        let response = http
            .get(format!("http://{dest}/private"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "origin");
        let head = timeout(TIMEOUT, received).await.unwrap().unwrap();
        assert!(head.starts_with("GET /private HTTP/1.1\r\n"));
        assert_eq!(value(&head, "proxy-authorization"), None);
        assert_eq!(value(&head, "user-agent"), Some("DSH-Test/1"));
    }
    assert!(timeout(Duration::from_millis(100), unused.accept())
        .await
        .is_err());
}

#[tokio::test]
async fn local_and_upstream_auth_failures_never_contact_the_destination() {
    let dest = listener().await;
    let target = format!("http://{}/", dest.local_addr().unwrap());
    let (upstream, received) = http_endpoint("407 Proxy Authentication Required", PASSWORD).await;
    let network = BrowserNetwork::start(Some(route(upstream, "http", ""))).unwrap();
    let untrusted = client(
        Proxy::all(network.url())
            .unwrap()
            .basic_auth("launcher", "invalid"),
    );
    let local = untrusted.get(&target).send().await.unwrap();
    assert_eq!(local.status(), StatusCode::PROXY_AUTHENTICATION_REQUIRED);
    assert!(local.headers().contains_key(header::PROXY_AUTHENTICATE));

    let remote = sidebar(&network).get(&target).send().await.unwrap();
    assert_eq!(remote.status(), StatusCode::BAD_GATEWAY);
    let body = remote.text().await.unwrap();
    for credential in [USER, PASSWORD, network.password(), &upstream_basic()] {
        assert!(!body.contains(credential));
    }
    let head = timeout(TIMEOUT, received).await.unwrap().unwrap();
    assert_eq!(
        value(&head, "proxy-authorization"),
        Some(upstream_basic().as_str())
    );
    assert!(timeout(Duration::from_millis(100), dest.accept())
        .await
        .is_err());
}

async fn read_bytes(stream: &mut TcpStream) -> Vec<u8> {
    let len = stream.read_u8().await.unwrap();
    let mut bytes = vec![0; usize::from(len)];
    stream.read_exact(&mut bytes).await.unwrap();
    bytes
}

async fn socks_endpoint(accept: bool) -> (SocketAddr, JoinHandle<()>) {
    let server = listener().await;
    let addr = server.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = server.accept().await.unwrap();
        assert_eq!(stream.read_u8().await.unwrap(), 5);
        assert!(read_bytes(&mut stream).await.contains(&2));
        stream.write_all(&[5, 2]).await.unwrap();
        assert_eq!(stream.read_u8().await.unwrap(), 1);
        assert_eq!(read_bytes(&mut stream).await, USER.as_bytes());
        assert_eq!(read_bytes(&mut stream).await, PASSWORD.as_bytes());
        stream.write_all(&[1, u8::from(!accept)]).await.unwrap();
        if !accept {
            return;
        }
        let mut connect = [0; 4];
        stream.read_exact(&mut connect).await.unwrap();
        assert_eq!(&connect[..3], &[5, 1, 0]);
        match connect[3] {
            1 => {
                assert_eq!(
                    stream.read_u32().await.unwrap(),
                    u32::from_be_bytes([127, 0, 0, 1])
                );
            }
            3 => {
                assert_eq!(read_bytes(&mut stream).await, b"127.0.0.1");
            }
            kind => panic!("意外的 SOCKS5 地址类型 {kind}"),
        }
        assert_eq!(stream.read_u16().await.unwrap(), 80);
        stream
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
            .await
            .unwrap();
        let head = read_head(&mut stream).await;
        assert!(head.starts_with("GET /through-socks HTTP/1.1"));
        assert_eq!(value(&head, "proxy-authorization"), None);
        respond(&mut stream, "200 OK", "socks").await;
    });
    (addr, task)
}

#[tokio::test]
async fn socks5_authentication_preserves_special_characters_on_both_clients() {
    for native_sidebar in [true, false] {
        for accept in [true, false] {
            let (addr, task) = socks_endpoint(accept).await;
            let upstream = route(addr, "socks5", "");
            let network;
            let http = if native_sidebar {
                network = BrowserNetwork::start(Some(upstream)).unwrap();
                sidebar(&network)
            } else {
                client(upstream.http_client_proxy().unwrap())
            };
            let response = http.get("http://127.0.0.1/through-socks").send().await;
            timeout(TIMEOUT, task).await.unwrap().unwrap();
            if accept {
                assert_eq!(response.unwrap().text().await.unwrap(), "socks");
            } else if native_sidebar {
                assert_eq!(response.unwrap().status(), StatusCode::BAD_GATEWAY);
            } else {
                assert!(response.is_err());
            }
        }
    }
}
