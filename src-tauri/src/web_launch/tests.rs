use super::*;
use crate::web_access::TlsFiles;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{timeout, Duration};

const FLAGS: &str = "--no-open --trusted-host <authority...> --public-url <url> --host <host> isWildcardHost --tls-cert <file> --tls-key <file>";

fn proxy() -> WebAccess {
    WebAccess {
        mode: WebMode::Proxy,
        public_url: Some("https://dsh.example:8443/ui".into()),
        trusted_hosts: vec!["second.example:443".into()],
        ..Default::default()
    }
}

#[tokio::test]
async fn proxy_uses_distinct_authenticated_roots_and_only_displays_the_public_root() {
    let mut web = WebLaunch::prepare(proxy(), Some(3080), FLAGS, None)
        .await
        .unwrap();
    assert!(web
        .args
        .windows(2)
        .any(|p| p == ["--public-url", "https://dsh.example:8443/ui/"]));
    assert!(web
        .args
        .windows(3)
        .any(|p| p == ["--trusted-host", "dsh.example:8443", "second.example:443"]));
    for nonce in ["first", "second"] {
        let links = web
            .ready(&format!("https://dsh.example:8443/ui/?token={nonce}"))
            .unwrap();
        assert_eq!(
            links.desktop.as_str(),
            format!("http://127.0.0.1:3080/?token={nonce}")
        );
        assert_eq!(links.display(), "https://dsh.example:8443/ui/");
        assert!(links.share.as_str().ends_with(nonce));
    }
    assert!(web.ready("https://other.example/ui/?token=foo").is_err());
    assert!(web.ready("https://dsh.example:8443/ui/").is_err());
}

#[tokio::test]
async fn public_modes_require_the_specific_installed_capabilities() {
    let old = "--host <host> --trusted-host <authority...> --no-open";
    assert!(WebLaunch::prepare(proxy(), Some(3080), old, None)
        .await
        .is_err());
    let direct = WebAccess {
        mode: WebMode::Direct,
        address: Some("192.0.2.1".parse().unwrap()),
        ..Default::default()
    };
    assert!(WebLaunch::prepare(direct.clone(), None, old, None)
        .await
        .is_err());
    let mut web = WebLaunch::prepare(direct, None, FLAGS, None).await.unwrap();
    assert!(web.args.windows(2).any(|p| p == ["--host", "192.0.2.1"]));
    assert!(web.ready("http://192.0.2.1:4321/").is_err());
    let links = web.ready("http://192.0.2.1:4321/?token=example").unwrap();
    assert_eq!(links.desktop, links.share);
    let tls = WebAccess {
        tls: Some(TlsFiles {
            cert: "missing.crt".into(),
            key: "missing.key".into(),
        }),
        ..Default::default()
    };
    assert!(WebLaunch::prepare(tls.clone(), None, old, None)
        .await
        .is_err());
    assert!(WebLaunch::prepare(tls, None, FLAGS, None).await.is_err());
    let mut local = WebLaunch::prepare(WebAccess::default(), None, "", None)
        .await
        .unwrap();
    assert!(local.ready("http://127.0.0.1:4321/").is_ok());
}

#[test]
fn network_rules_preserve_the_actual_allowed_ranges() {
    let mut lan = WebAccess {
        mode: WebMode::Lan,
        address: Some("192.0.2.1".parse().unwrap()),
        ..Default::default()
    };
    assert!(lan.clone().normalized(None).is_err());
    for ip in ["0.0.0.0", "::", "::ffff:0.0.0.0", "127.0.0.1"] {
        let direct = WebAccess {
            mode: WebMode::Direct,
            address: Some(ip.parse().unwrap()),
            ..Default::default()
        };
        assert!(direct.normalized(Some(9000)).is_err());
    }
    lan.networks = ["0.0.0.0/1", "128.0.0.0/1"]
        .map(|n| n.parse().unwrap())
        .to_vec();
    assert!(lan.clone().normalized(None).is_err());
    lan.networks = ["192.0.2.1/24", "2001:db8::1/48"]
        .map(|n| n.parse().unwrap())
        .to_vec();
    let nets = lan.normalized(None).unwrap().networks;
    for ip in ["192.0.2.2", "2001:db8:0:ffff::1"] {
        assert!(nets
            .iter()
            .any(|n| n.contains(&ip.parse::<IpAddr>().unwrap())));
    }
    assert!(!nets
        .iter()
        .any(|n| n.contains(&"2001:db8:1::1".parse::<IpAddr>().unwrap())));
    for url in [
        "ftp://foo/",
        "https:///ui",
        "https://@foo/",
        "https://foo/ui?",
        "https://foo/ui#",
        "https://foo/\n",
        "https://--host/",
        "https://*.example/",
    ] {
        assert!(WebAccess {
            public_url: Some(url.into()),
            ..proxy()
        }
        .normalized(Some(80))
        .is_err());
    }
    for host in ["--host", "--a", "*.example", "example:080", "foo/bar"] {
        assert!(WebAccess {
            trusted_hosts: vec![host.into()],
            ..proxy()
        }
        .normalized(Some(3080))
        .is_err());
    }
    for key in [
        "relative.key",
        "-----BEGIN PRIVATE KEY-----\nsecret",
        "/tmp/-----BEGIN PRIVATE KEY-----",
    ] {
        let tls = TlsFiles {
            cert: "/tmp/chain.pem".into(),
            key: key.into(),
        };
        assert!(WebAccess {
            tls: Some(tls),
            ..proxy()
        }
        .normalized(Some(3080))
        .is_err());
    }
    assert!(proxy().normalized(None).is_err());
    let inst: crate::config::DshInstance = serde_json::from_str(
        r#"{"id":"i","name":"foo","version_id":"v","home_id":"h","port":8080}"#,
    )
    .unwrap();
    assert_eq!(inst.web_access.mode, WebMode::Local);
    assert_eq!(inst.port, Some(8080));
}

#[test]
fn removes_launch_credentials_from_both_urls_in_old_and_new_logs() {
    let old = "dsh web: http://localhost:4321/?token=first (LAN: http://192.0.2.1/?token=second)";
    let masked = redact_tokens(old);
    assert!(!masked.contains("first") && !masked.contains("second"));
    assert!(masked.contains("localhost:4321") && masked.contains("192.0.2.1"));
    assert!(!redact_tokens("http://foo/?%74o%6Ben=sensitive").contains("sensitive"));
}

async fn headers(stream: &mut BufReader<TcpStream>) -> String {
    let mut text = String::new();
    while !text.ends_with("\r\n\r\n") {
        assert_ne!(stream.read_line(&mut text).await.unwrap(), 0);
    }
    text
}

#[tokio::test]
async fn allowed_clients_can_use_http_and_websocket_until_shutdown() {
    for (bind, allowed) in [("127.0.0.1:0", "127.0.0.1/32"), ("[::1]:0", "::1/128")] {
        timeout(Duration::from_secs(8), async {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let nets = vec!["192.0.2.0/24".parse().unwrap(),allowed.parse().unwrap()];
        let mut relay = Relay::bind(bind.parse().unwrap(), Some(nets)).await.unwrap();
        relay.start(upstream.local_addr().unwrap(), None);
        let server = tokio::spawn(async move {
            let mut http = BufReader::new(upstream.accept().await.unwrap().0);
            assert!(headers(&mut http).await.contains("Host: dsh.example"));
            http.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK").await.unwrap();
            drop(http);
            let mut ws = BufReader::new(upstream.accept().await.unwrap().0);
            assert!(headers(&mut ws).await.contains("Upgrade: websocket"));
            ws.get_mut().write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n").await.unwrap();
            let mut frame = [0;7];
            ws.read_exact(&mut frame).await.unwrap();
            assert_eq!(frame,[0x81,0x81,1,2,3,4,b'x'^1]);
            ws.get_mut().write_all(&[0x81,1,b'x']).await.unwrap();
            assert_eq!(ws.read(&mut frame).await.unwrap(),0);
        });
        let mut http = TcpStream::connect(relay.address).await.unwrap();
        http.write_all(b"GET / HTTP/1.1\r\nHost: dsh.example\r\n\r\n").await.unwrap();
        let mut response = String::new();
        http.read_to_string(&mut response).await.unwrap();
        assert!(response.ends_with("\r\n\r\nOK"));
        let mut ws = BufReader::new(TcpStream::connect(relay.address).await.unwrap());
        ws.get_mut().write_all(b"GET /stream HTTP/1.1\r\nHost: dsh.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").await.unwrap();
        assert!(headers(&mut ws).await.starts_with("HTTP/1.1 101"));
        ws.get_mut().write_all(&[0x81,0x81,1,2,3,4,b'x'^1]).await.unwrap();
        let mut echo = [0;3];
        ws.read_exact(&mut echo).await.unwrap();
        assert_eq!(echo,[0x81,1,b'x']);
        relay.close().await;
        assert_eq!(ws.read(&mut echo).await.unwrap(),0);
        assert!(TcpStream::connect(relay.address).await.is_err());
        server.await.unwrap();
    }).await.unwrap();
    }
}

#[tokio::test]
async fn invalid_pem_and_occupied_listener_fail_before_dsh_starts() {
    let dir = std::env::temp_dir().join(format!("dsh-network-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let pem = dir.join("invalid.pem");
    std::fs::write(&pem, b"invalid PEM").unwrap();
    let file = pem.to_string_lossy().into_owned();
    let tls = TlsFiles {
        cert: file.clone(),
        key: file,
    };
    let error = WebLaunch::prepare(
        WebAccess {
            tls: Some(tls),
            ..Default::default()
        },
        None,
        FLAGS,
        None,
    )
    .await
    .err()
    .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    assert!(error.contains("证书链或私钥无效"), "{error}");
    let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let launch = WebLaunch::prepare(
        proxy(),
        Some(occupied.local_addr().unwrap().port()),
        FLAGS,
        None,
    )
    .await
    .unwrap();
    assert!(launch.check_native_port().is_err());
    drop(occupied);
    launch.check_native_port().unwrap();
}

#[tokio::test]
async fn wsl_proxy_owns_the_windows_endpoint_and_releases_it_on_failure_or_exit() {
    let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = occupied.local_addr().unwrap();
    let wsl = Some(WslWeb {
        distro: "example".into(),
        node: "/usr/bin/node".into(),
        proxy_port: if endpoint.port() == 40000 {
            40001
        } else {
            40000
        },
    });
    assert!(
        WebLaunch::prepare(proxy(), Some(endpoint.port()), FLAGS, wsl.clone())
            .await
            .is_err()
    );
    drop(occupied);
    let mut launch = WebLaunch::prepare(proxy(), Some(endpoint.port()), FLAGS, wsl)
        .await
        .unwrap();
    assert_ne!(launch.port, endpoint.port());
    assert!(TcpListener::bind(endpoint).await.is_err());
    let links = launch
        .ready("https://dsh.example:8443/ui/?token=example")
        .unwrap();
    assert_eq!(links.desktop.port(), Some(endpoint.port()));
    launch.close().await;
    assert!(TcpListener::bind(endpoint).await.is_ok());
}

#[tokio::test]
async fn rejects_unlisted_peers_before_forwarding_http_or_upgrades() {
    for bind in ["127.0.0.1:0", "[::1]:0"] {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let nets = vec![
            "192.0.2.0/24".parse().unwrap(),
            "fd00::/64".parse().unwrap(),
        ];
        let mut relay = Relay::bind(bind.parse().unwrap(), Some(nets))
            .await
            .unwrap();
        relay.start(upstream.local_addr().unwrap(), None);
        for upgrade in ["", "Upgrade: websocket\r\nConnection: Upgrade\r\n"] {
            let mut stream = TcpStream::connect(relay.address).await.unwrap();
            let _ = stream.write_all(format!("GET / HTTP/1.1\r\nHost: foo\r\nX-Forwarded-For: 192.0.2.1\r\n{upgrade}\r\n").as_bytes()).await;
            assert!(timeout(Duration::from_secs(2), stream.read_u8())
                .await
                .unwrap()
                .is_err());
        }
        assert!(timeout(Duration::from_millis(80), upstream.accept())
            .await
            .is_err());
        relay.close().await;
    }
}
