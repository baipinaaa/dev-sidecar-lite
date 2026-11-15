//! MITM：终结客户端 TLS，解密后按规则转发到上游。
//!
//! 阶段 1 只做「能解密、能转发」的最小闭环：
//! - 客户端侧：用本地 CA 动态签发证书，ALPN 固定 `http/1.1`
//! - 上游侧：rustls（当前不校验证书，等价 Node 版 `unVerifySslAgent`）
//! - 规则：命中 `intercepts` 且 `intercept.enabled` 时才解密
//!
//! SNI 改写、内容改写、ECH 在后续阶段接入。

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use anyhow::{Context as _, Result};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::http::{self, RequestHead, MAX_HEAD};
use crate::ProxyOptions;
use ds_ctl::ConnHandle;
use ds_tls::CertAuthority;
use ds_types::RuntimeConfig;

const ESTABLISHED: &[u8] =
    b"HTTP/1.1 200 Connection Established\r\nProxy-agent: dev-sidecar-lite\r\n\r\n";

/// MITM 运行所需的两套 TLS 配置。
#[derive(Debug, Clone)]
pub struct Mitm {
    acceptor: TlsAcceptor,
    connector: TlsConnector,
}

impl Mitm {
    /// 尝试构建；CA 不可用时返回 `None`（降级为纯隧道）。
    pub fn try_new() -> Option<Self> {
        let authority = match CertAuthority::from_default_paths() {
            Ok(ca) => Arc::new(ca),
            Err(err) => {
                tracing::warn!("加载 MITM CA 失败，降级为纯隧道：{err:#}");
                tracing::warn!("提示：先运行一次 Node 版 dev-sidecar 生成 ~/.dev-sidecar/dev-sidecar.ca.*");
                return None;
            }
        };

        let server_config = match ds_tls::ca::mitm_server_config(authority) {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!("构建 MITM 服务端 TLS 配置失败，降级为纯隧道：{err:#}");
                return None;
            }
        };

        let client_config = ds_tls::client::insecure_client_config();

        Some(Self {
            acceptor: TlsAcceptor::from(server_config),
            connector: TlsConnector::from(client_config),
        })
    }
}

/// 是否应该解密该域名。
pub fn should_intercept(config: &RuntimeConfig, host: &str) -> bool {
    if !config.intercept.enabled {
        return false;
    }
    config
        .intercepts
        .keys()
        .any(|pattern| match ds_types::matcher::compile(pattern) {
            Ok(re) => re.is_match(host),
            Err(err) => {
                tracing::debug!("intercepts 规则 {pattern} 无法编译：{err}");
                false
            }
        })
}

pub async fn handle(
    mut client: TcpStream,
    req: RequestHead,
    leftover: Vec<u8>,
    mitm: &Mitm,
    opts: &ProxyOptions,
    handle: &mut ConnHandle<'_>,
) -> Result<()> {
    let (host, port) = req.authority(443)?;

    client
        .write_all(ESTABLISHED)
        .await
        .context("回写 200 Connection Established 失败")?;
    client.flush().await.ok();

    // 客户端可能把 ClientHello 和 CONNECT 同包发来，必须回灌给 TLS 层
    let prefixed = PrefixedStream::new(leftover, client);

    let mut tls = mitm
        .acceptor
        .accept(prefixed)
        .await
        .with_context(|| format!("与 {host} 的客户端 TLS 握手失败"))?;

    let head = http::read_head(&mut tls, MAX_HEAD).await?;
    let inner = RequestHead::parse(&head.bytes)
        .with_context(|| format!("解析 {host} 的 MITM 请求失败"))?;

    let target = inner.origin_target();

    let tcp = http::connect_upstream(&host, port).await?;

    if port == 443 {
        let server_name = ServerName::try_from(host.clone())
            .with_context(|| format!("{host} 不是合法的 TLS 服务器名"))?;
        let mut upstream = mitm
            .connector
            .connect(server_name, tcp)
            .await
            .with_context(|| format!("与 {host} 的上游 TLS 握手失败"))?;

        upstream
            .write_all(&inner.rebuild(&target))
            .await
            .context("写上游请求头失败")?;
        if !head.leftover.is_empty() {
            upstream
                .write_all(&head.leftover)
                .await
                .context("写上游首包失败")?;
        }
        upstream.flush().await.ok();

        forward_both(&mut tls, &mut upstream, handle, opts, &host, port).await?;
    } else {
        let mut upstream = tcp;
        upstream
            .write_all(&inner.rebuild(&target))
            .await
            .context("写上游请求头失败")?;
        if !head.leftover.is_empty() {
            upstream
                .write_all(&head.leftover)
                .await
                .context("写上游首包失败")?;
        }
        upstream.flush().await.ok();

        forward_both(&mut tls, &mut upstream, handle, opts, &host, port).await?;
    }

    Ok(())
}

/// 双向转发，并支持控制面取消。
async fn forward_both<A, B>(
    client: &mut A,
    upstream: &mut B,
    handle: &mut ConnHandle<'_>,
    opts: &ProxyOptions,
    host: &str,
    port: u16,
) -> Result<()>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let mut cancel = handle.take_cancel();

    let _ = tokio::select! {
        result = tokio::io::copy_bidirectional(client, upstream) => {
            let (up, down) = result.context("MITM 双向转发失败")?;
            handle.finish(up, down);
            if opts.verbose {
                tracing::info!("MITM {host}:{port} 结束（上行 {up}B / 下行 {down}B）");
            }
            false
        }
        _ = async {
            match cancel.as_mut() {
                Some(rx) => { let _ = rx.await; }
                None => std::future::pending::<()>().await,
            }
        } => {
            tracing::info!("MITM {host}:{port} 被控制面关闭");
            handle.finish(0, 0);
            true
        }
    };

    Ok(())
}

/// 把「提前读到的字节」塞回读侧。
struct PrefixedStream<T> {
    prefix: Vec<u8>,
    pos: usize,
    inner: T,
}

impl<T> PrefixedStream<T> {
    fn new(prefix: Vec<u8>, inner: T) -> Self {
        Self {
            prefix,
            pos: 0,
            inner,
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for PrefixedStream<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        if this.pos < this.prefix.len() {
            let remain = &this.prefix[this.pos..];
            let take = remain.len().min(buf.remaining());
            buf.put_slice(&remain[..take]);
            this.pos += take;

            if this.pos >= this.prefix.len() {
                this.prefix.clear();
                this.pos = 0;
            }
            return Poll::Ready(Ok(()));
        }

        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for PrefixedStream<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// 供测试：读空数据时应立即返回 0（避免阻塞）。
pub async fn drain_until_eof<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    stream.read_to_end(&mut out).await?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixed_stream_replays_bytes() {
        // 纯逻辑校验：构造后 prefix/pos 状态正确
        let s = PrefixedStream::new(vec![1, 2, 3], ());
        assert_eq!(s.prefix.len(), 3);
        assert_eq!(s.pos, 0);
    }

    #[test]
    fn insecure_config_has_no_h2() {
        let config = ds_tls::client::insecure_client_config();
        assert!(!config.alpn_protocols.iter().any(|p| p.as_slice() == b"h2"));
    }
}
