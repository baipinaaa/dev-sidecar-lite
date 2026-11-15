//! CONNECT 隧道：HTTPS 流量的基础通路。
//!
//! 未命中 `intercepts` 的域名走这里：只把客户端与上游字节流对接，不解密。

use anyhow::{Context, Result};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::http::{connect_upstream, RequestHead};
use crate::ProxyOptions;
use ds_ctl::ConnHandle;

const ESTABLISHED: &[u8] =
    b"HTTP/1.1 200 Connection Established\r\nProxy-agent: dev-sidecar-lite\r\n\r\n";

pub async fn handle(
    mut client: TcpStream,
    req: RequestHead,
    leftover: Vec<u8>,
    opts: &ProxyOptions,
    handle: &mut ConnHandle<'_>,
) -> Result<()> {
    let (host, port) = req.authority(443)?;

    if opts.verbose {
        tracing::info!("CONNECT {host}:{port}");
    }

    let mut upstream = connect_upstream(&host, port).await?;

    client
        .write_all(ESTABLISHED)
        .await
        .context("回写 200 Connection Established 失败")?;
    client.flush().await.ok();

    if !leftover.is_empty() {
        upstream
            .write_all(&leftover)
            .await
            .context("转发 CONNECT 首包失败")?;
        upstream.flush().await.ok();
    }

    let mut cancel = handle.take_cancel();

    let _ = tokio::select! {
        result = tokio::io::copy_bidirectional(&mut client, &mut upstream) => {
            let (up, down) = result.context("隧道双向转发失败")?;
            handle.finish(up, down);
            if opts.verbose {
                tracing::debug!("CONNECT {host}:{port} 结束（上行 {up}B / 下行 {down}B）");
            }
            false
        }
        _ = async {
            match cancel.as_mut() {
                Some(rx) => { let _ = rx.await; }
                None => std::future::pending::<()>().await,
            }
        } => {
            tracing::info!("CONNECT {host}:{port} 被控制面关闭");
            handle.finish(0, 0);
            true
        }
    };

    Ok(())
}
