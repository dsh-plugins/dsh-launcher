use super::WslWeb;
use ipnet::IpNet;
use std::io;
use std::net::SocketAddr;
use std::process::Stdio;
use tokio::io::{copy, copy_bidirectional, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};

/// 监听器和所有已接入的连接随同一个实例关闭。
pub(super) struct Relay {
    pub address: SocketAddr,
    listener: Option<TcpListener>,
    networks: Option<Vec<IpNet>>,
    worker: Option<(oneshot::Sender<()>, JoinHandle<()>)>,
}

impl Relay {
    pub async fn bind(address: SocketAddr, networks: Option<Vec<IpNet>>) -> Result<Self, String> {
        let listener = TcpListener::bind(address)
            .await
            .map_err(|e| format!("无法监听 {address}: {e}"))?;
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        Ok(Self {
            address,
            listener: Some(listener),
            networks,
            worker: None,
        })
    }

    pub fn start(&mut self, upstream: SocketAddr, wsl: Option<WslWeb>) {
        let Some(listener) = self.listener.take() else {
            return;
        };
        let networks = self.networks.take();
        let (stop, mut stopped) = oneshot::channel();
        let worker = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stopped => break,
                    Some(_) = clients.join_next(), if !clients.is_empty() => (),
                    incoming = listener.accept() => {
                        let (stream, peer) = match incoming {
                            Ok(pair) => pair,
                            Err(e) => {
                                crate::log_warn!("网络访问入口接收连接失败: {e}");
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                                continue;
                            }
                        };
                        if networks.as_ref().is_some_and(|nets| !nets.iter().any(|net| net.contains(&peer.ip().to_canonical()))) {
                            continue;
                        }
                        // WSL 的每个桥接连接都需要一个 Node 进程，限制未认证连接的资源占用。
                        if wsl.is_some() && clients.len() >= 16 { continue; }
                        let wsl = wsl.clone();
                        clients.spawn(async move {
                            if let Err(e) = forward(stream, upstream, wsl).await {
                                crate::log_debug!("实例连接 {peer} 转发失败: {e}");
                            }
                        });
                    }
                }
            }
            drop(listener);
            clients.shutdown().await;
        });
        self.worker = Some((stop, worker));
    }

    pub async fn close(&mut self) {
        self.listener.take();
        if let Some((stop, worker)) = self.worker.take() {
            drop(stop);
            if let Err(e) = worker.await {
                crate::log_warn!("关闭实例网络入口失败: {e}");
            }
        }
    }
}

async fn forward(
    mut client: TcpStream,
    upstream: SocketAddr,
    wsl: Option<WslWeb>,
) -> io::Result<()> {
    let Some(guest) = wsl else {
        let mut server = TcpStream::connect(upstream).await?;
        copy_bidirectional(&mut client, &mut server).await?;
        return Ok(());
    };
    // 从发行版内部连接回环地址，避免 Windows 同号端口的其他程序被意外公开。
    let bridge = r#"const socket = require('node:net').connect(Number(process.argv[1]), '127.0.0.1');
process.stdin.pipe(socket);
socket.pipe(process.stdout);
socket.on('error', () => process.exit(1));
socket.on('close', () => process.stdin.destroy());"#;
    let script = format!(
        "exec {} -e {} {}",
        crate::wsl::sh_quote(&guest.node),
        crate::wsl::sh_quote(bridge),
        upstream.port()
    );
    let mut child = crate::wsl::wsl_cmd(&guest.distro, &["bash".into(), "-c".into(), script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child.stdin.take().expect("已配置标准输入管道");
    let mut output = child.stdout.take().expect("已配置标准输出管道");
    let (mut read, mut write) = client.split();
    tokio::try_join!(
        async {
            copy(&mut read, &mut input).await?;
            input.shutdown().await
        },
        async {
            copy(&mut output, &mut write).await?;
            write.shutdown().await
        },
    )?;
    let status = child.wait().await?;
    if !status.success() {
        return Err(io::Error::other(format!("WSL 转发进程退出: {status}")));
    }
    Ok(())
}
