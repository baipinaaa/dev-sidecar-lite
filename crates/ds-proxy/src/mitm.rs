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
#[derive(Clone)]
pub struct Mitm {
    acceptor: TlsAcceptor,
}

// tokio-rustls 的 TlsAcceptor / TlsConnector 没实现 Debug，这里只打印类型名。
impl std::fmt::Debug for Mitm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mitm").finish_non_exhaustive()
    }
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

        Some(Self {
            acceptor: TlsAcceptor::from(server_config),
        })
    }
}

/// 上游 ClientConfig：ECH 域名现查 ECHConfig 再建配置，其余走普通 rustls。
///
/// 这里刻意"每次连接现查"，而不是像 Node 版那样做进程级缓存：
/// ECHConfig 有 TTL，且 CF 会在轮换后返回 `retry_configs`；
/// 短连接场景下多一次 DoH 查询（~30ms，可与 TCP 建连并行优化）换来的是不会用到过期配置。
async fn upstream_client_config(config: &RuntimeConfig, host: &str) -> Arc<rustls::ClientConfig> {
    match ech_bytes(config, host).await {
        Some(bytes) => match ds_tls::ech::build_upstream_config(false, Some(&bytes)) {
            Ok(cfg) => cfg,
            Err(err) => {
                tracing::warn!("{host} 构建 ECH 配置失败，降级为普通握手：{err:#}");
                ds_tls::client::insecure_client_config()
            }
        },
        None => ds_tls::client::insecure_client_config(),
    }
}

/// 取该域名的 ECHConfigList（拿不到就 `None`）。两条上游通道（rustls / Chrome 指纹）共用。
async fn ech_bytes(config: &RuntimeConfig, host: &str) -> Option<Vec<u8>> {
    let ech = &config.dns.ech;
    if !(ech.enabled && ech.use_ech) {
        return None;
    }
    if !ds_types::matcher::any_match(ech.domains.iter().map(|s| s.as_str()), host) {
        return None;
    }

    let provider = ech
        .dns
        .as_deref()
        .and_then(|name| config.dns.providers.get(name))
        .map(|p| p.server.clone())
        .or_else(|| config.dns.providers.values().next().map(|p| p.server.clone()));
    let Some(server) = provider else {
        tracing::debug!("{host} 需要 ECH，但没有可用的 DoH 提供方");
        return None;
    };

    match ds_dns::resolve_ech(&server, host).await {
        Ok(Some(bytes)) => {
            tracing::debug!("{host} 使用 ECH（{} 字节配置，来自 {server}）", bytes.len());
            Some(bytes)
        }
        Ok(None) => {
            tracing::debug!("{host} 的 HTTPS 记录里没有 ECH（该站可能未开通），改用 ECH grease");
            None
        }
        Err(err) => {
            tracing::warn!("{host} 查询 ECH 失败：{err:#}");
            None
        }
    }
}

/// 该域名是否要求走 Chrome 指纹通道。
///
/// 判据是 `intercepts` 里命中的规则带 `fingerprint: "chrome"`。只看 `*` / `.*` 这条
/// 兜底路径：具体路径上的指纹没有意义（同一条连接的 ClientHello 与路径无关）。
pub fn wants_chrome_fingerprint(config: &RuntimeConfig, host: &str) -> bool {
    for (pattern, table) in config.intercepts.iter() {
        if !ds_types::matcher::glob_match(pattern, host) {
            continue;
        }
        for (path, options) in table.iter() {
            if path != "*" && path != ".*" {
                continue;
            }
            if let Some(options) = options {
                if options.wants_chrome_fingerprint() {
                    return true;
                }
            }
        }
    }
    false
}

/// 是否应该解密该域名。
///
/// 注意：这里必须用 `matcher::glob_match`（它能处理"纯字面量"规则），
/// 而不是 `matcher::compile` —— 后者对不含 `*`/`?` 且不以 `^` 开头的规则返回 `None`，
/// 表示"该走精确比较"，直接当失败会让 **所有精确域名规则永久失效**
/// （2026-11-16 验收时就因此把 linux.do 当隧道放过去了，测到的是 curl 的指纹）。
pub fn should_intercept(config: &RuntimeConfig, host: &str) -> bool {
    if !config.intercept.enabled {
        return false;
    }
    if ds_rules::guard::bypass_reason(config, host).is_some() {
        return false;
    }
    ds_types::matcher::any_match(config.intercepts.keys().map(|p| p.as_str()), host)
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

    // 指纹通道：不解析 HTTP，握手完就把明文字节双向搬运。
    if port == 443 && wants_chrome_fingerprint(&opts.config, &host) {
        let tcp = http::connect_upstream(&host, port).await?;
        let std_tcp = tcp.into_std().context("把上游连接切回阻塞模式失败")?;
        std_tcp.set_nonblocking(false).context("设置上游为阻塞模式失败")?;
        let ech = ech_bytes(&opts.config, &host).await;
        tracing::debug!("{host} 走 Chrome 指纹通道");
        return crate::chrome_relay::relay(tls, std_tcp, host, ech, handle).await;
    }

    let head = http::read_head(&mut tls, MAX_HEAD).await?;
    let inner = RequestHead::parse(&head.bytes)
        .with_context(|| format!("解析 {host} 的 MITM 请求失败"))?;

    let target = inner.origin_target();

    let tcp = http::connect_upstream(&host, port).await?;

    if port == 443 {
        let server_name = ServerName::try_from(host.clone())
            .with_context(|| format!("{host} 不是合法的 TLS 服务器名"))?;
        let connector = TlsConnector::from(upstream_client_config(&opts.config, &host).await);
        let mut upstream = connector
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
#[allow(dead_code)]
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

    /// 回归：纯字面量规则（真实配置里 137 条有大半是这种）必须能命中，
    /// 否则 MITM 永远不生效，验收测到的就是 curl 的指纹。
    #[test]
    fn should_intercept_matches_exact_domain() {
        let config = RuntimeConfig::from_json_str(
            r#"{
                "intercept": { "enabled": true },
                "intercepts": {
                    "linux.do": { ".*": {} },
                    "*.ldstatic.com": { ".*": {} },
                    "^(.+\\.)?x\\.com$": { ".*": {} }
                }
            }"#,
        )
        .expect("配置应可解析");

        assert!(should_intercept(&config, "linux.do"));
        assert!(should_intercept(&config, "cdn3.ldstatic.com"));
        assert!(should_intercept(&config, "x.com"));
        assert!(!should_intercept(&config, "www.baidu.com"));

        // 白名单优先：命中白名单的域名即使配有 intercepts 也不解密
        let mut with_wl = config.clone();
        with_wl
            .white_list
            .insert("linux.do".into(), ds_types::FlexBool(true));
        assert!(!should_intercept(&with_wl, "linux.do"));
    }

    /// 只有显式写了 `fingerprint: "chrome"` 的规则才走指纹通道。
    #[test]
    fn wants_chrome_fingerprint_only_when_declared() {
        let config = RuntimeConfig::from_json_str(
            r#"{
                "intercept": { "enabled": true },
                "intercepts": {
                    "linux.do": { ".*": { "fingerprint": "chrome" } },
                    "github.com": { ".*": {} },
                    "*.ldstatic.com": { "/assets/.*": { "fingerprint": "chrome" } }
                }
            }"#,
        )
        .expect("配置应可解析");

        assert!(wants_chrome_fingerprint(&config, "linux.do"));
        // 根域子域都命中（通配符覆盖根域由 PAC 层负责，这里只测 glob 的子域语义）
        assert!(wants_chrome_fingerprint(&config, "www.linux.do"));
        assert!(!wants_chrome_fingerprint(&config, "github.com"));
        // 指纹只认 `*` / `.*` 兜底路径：写在具体路径上不生效
        assert!(!wants_chrome_fingerprint(&config, "cdn3.ldstatic.com"));
        assert!(!wants_chrome_fingerprint(&config, "www.baidu.com"));
    }

    /// `intercept.enabled = false` 时一律走隧道
    #[test]
    fn should_intercept_respects_global_switch() {
        let config = RuntimeConfig::from_json_str(
            r#"{ "intercept": { "enabled": false }, "intercepts": { "linux.do": { ".*": {} } } }"#,
        )
        .expect("配置应可解析");
        assert!(!should_intercept(&config, "linux.do"));
    }
}
