//! 极简 HTTP/1.1 头解析与明文转发。
//!
//! 只做代理必需的部分：读头、解析请求行、重组 origin-form 请求。
//! 请求体与响应交给 `copy_bidirectional` 全量透传，因此天然支持
//! `Content-Length` / `chunked` / `Connection: upgrade`。

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};

use crate::ProxyOptions;

/// 单个请求头上限，防止恶意长头耗尽内存。
pub const MAX_HEAD: usize = 64 * 1024;

/// 连接上游的超时。
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

/// 读到的原始头 + 头之后多读进来的字节。
pub struct RawHead {
    /// 不含 `\r\n\r\n` 之后的任何字节
    pub bytes: Vec<u8>,
    /// 与头同一个 TCP 包到达的后续数据（例如紧跟 CONNECT 的 ClientHello）
    pub leftover: Vec<u8>,
}

/// 从流里读到 `\r\n\r\n` 为止。
pub async fn read_head<S>(stream: &mut S, limit: usize) -> Result<RawHead>
where
    S: AsyncRead + Unpin,
{
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];

    loop {
        let n = stream.read(&mut chunk).await.context("读取请求头失败")?;
        if n == 0 {
            bail!("连接在收到完整请求头前关闭");
        }
        buf.extend_from_slice(&chunk[..n]);

        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            let end = pos + 4;
            let leftover = buf[end..].to_vec();
            buf.truncate(end);
            return Ok(RawHead {
                bytes: buf,
                leftover,
            });
        }

        if buf.len() > limit {
            bail!("请求头超过 {limit} 字节");
        }
    }
}

/// 解析后的请求头。
#[derive(Debug, Clone)]
pub struct RequestHead {
    pub method: String,
    pub target: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
}

impl RequestHead {
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(raw).context("请求头不是合法 UTF-8")?;
        let mut lines = text.split("\r\n");

        let first = lines.next().context("请求头为空")?;
        let mut parts = first.split_whitespace();
        let method = parts.next().context("缺少请求方法")?.to_string();
        let target = parts.next().context("缺少请求目标")?.to_string();
        let version = parts.next().unwrap_or("HTTP/1.1").to_string();

        let mut headers = Vec::new();
        for line in lines {
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_string(), value.trim().to_string()));
            }
        }

        Ok(Self {
            method,
            target,
            version,
            headers,
        })
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// 目标主机与端口。
    pub fn authority(&self, default_port: u16) -> Result<(String, u16)> {
        if let Some(rest) = self.target.split_once("://").map(|(_, r)| r) {
            let authority = rest.split('/').next().unwrap_or(rest);
            if !authority.is_empty() {
                return split_authority(authority, default_port);
            }
        }
        if let Some(host) = self.header("Host") {
            return split_authority(host, default_port);
        }
        // CONNECT 形态：target 就是 authority
        split_authority(&self.target, default_port)
    }

    /// 把绝对 URI 改成 origin-form（`/path?query`）。
    pub fn origin_target(&self) -> String {
        if let Some(rest) = self.target.split_once("://").map(|(_, r)| r) {
            match rest.find('/') {
                Some(idx) => rest[idx..].to_string(),
                None => "/".to_string(),
            }
        } else if self.target.starts_with('/') {
            self.target.clone()
        } else {
            format!("/{}", self.target)
        }
    }

    /// 按 origin-form 重组请求头，并去掉代理专用头。
    pub fn rebuild(&self, target: &str) -> Vec<u8> {
        let mut out = String::with_capacity(512);
        out.push_str(&self.method);
        out.push(' ');
        out.push_str(target);
        out.push(' ');
        out.push_str(&self.version);
        out.push_str("\r\n");

        for (name, value) in &self.headers {
            if name.eq_ignore_ascii_case("Proxy-Connection")
                || name.eq_ignore_ascii_case("Proxy-Authorization")
            {
                continue;
            }
            out.push_str(name);
            out.push_str(": ");
            out.push_str(value);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        out.into_bytes()
    }
}

/// 解析 `host:port` / `host` / `[::1]:port`。
pub fn split_authority(authority: &str, default_port: u16) -> Result<(String, u16)> {
    let authority = authority.trim();
    if authority.is_empty() {
        bail!("目标为主机为空");
    }

    if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .context("IPv6 地址缺少闭合的 ]")?;
        let port = tail
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(default_port);
        return Ok((format!("[{host}]"), port));
    }

    match authority.rsplit_once(':') {
        Some((host, port))
            if !host.is_empty()
                && !port.is_empty()
                && port.chars().all(|c| c.is_ascii_digit()) =>
        {
            Ok((host.to_string(), port.parse::<u16>()?))
        }
        _ => Ok((authority.to_string(), default_port)),
    }
}

/// 连接上游（带超时与可读错误）。
pub async fn connect_upstream(host: &str, port: u16) -> Result<TcpStream> {
    let connect = TcpStream::connect((host, port));
    let stream = timeout(UPSTREAM_CONNECT_TIMEOUT, connect)
        .await
        .map_err(|_| anyhow::anyhow!("连接 {host}:{port} 超时（{}s）", UPSTREAM_CONNECT_TIMEOUT.as_secs()))?
        .with_context(|| format!("连接 {host}:{port} 失败"))?;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

/// 明文 HTTP 转发。
pub async fn forward(
    mut client: TcpStream,
    req: RequestHead,
    leftover: Vec<u8>,
    opts: &ProxyOptions,
) -> Result<()> {
    let (host, port) = req.authority(80)?;

    if opts.verbose {
        tracing::info!("{} {} → {host}:{port}", req.method, req.target);
    }

    let mut upstream = connect_upstream(&host, port).await?;
    let head = req.rebuild(&req.origin_target());

    upstream.write_all(&head).await.context("写上游请求头失败")?;
    if !leftover.is_empty() {
        upstream
            .write_all(&leftover)
            .await
            .context("写上游请求体失败")?;
    }
    upstream.flush().await.context("刷新上游缓冲失败")?;
    client.flush().await.ok();

    tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .context("转发数据失败")?;

    Ok(())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_connect() {
        let raw = b"CONNECT steamcommunity.com:443 HTTP/1.1\r\nHost: steamcommunity.com:443\r\nProxy-Connection: keep-alive\r\n\r\n";
        let req = RequestHead::parse(raw).unwrap();
        assert_eq!(req.method, "CONNECT");
        assert_eq!(req.authority(443).unwrap(), ("steamcommunity.com".to_string(), 443));
    }

    #[test]
    fn parse_absolute_uri() {
        let raw = b"GET http://www.baidu.com/s?wd=1 HTTP/1.1\r\nHost: www.baidu.com\r\n\r\n";
        let req = RequestHead::parse(raw).unwrap();
        assert_eq!(req.authority(80).unwrap(), ("www.baidu.com".to_string(), 80));
        assert_eq!(req.origin_target(), "/s?wd=1");
        let rebuilt = String::from_utf8(req.rebuild("/s?wd=1")).unwrap();
        assert!(rebuilt.starts_with("GET /s?wd=1 HTTP/1.1\r\n"));
    }

    #[test]
    fn split_authority_variants() {
        assert_eq!(split_authority("a.com:8080", 80).unwrap(), ("a.com".to_string(), 8080));
        assert_eq!(split_authority("a.com", 80).unwrap(), ("a.com".to_string(), 80));
        assert_eq!(split_authority("[::1]:443", 80).unwrap(), ("[::1]".to_string(), 443));
        assert_eq!(split_authority("[::1]", 80).unwrap(), ("[::1]".to_string(), 80));
    }

    #[test]
    fn rebuild_drops_proxy_headers() {
        let raw = b"GET / HTTP/1.1\r\nHost: a.com\r\nProxy-Authorization: x\r\nProxy-Connection: keep-alive\r\n\r\n";
        let req = RequestHead::parse(raw).unwrap();
        let rebuilt = String::from_utf8(req.rebuild("/")).unwrap();
        assert!(!rebuilt.contains("Proxy-Authorization"));
        assert!(!rebuilt.contains("Proxy-Connection"));
        assert!(rebuilt.contains("Host: a.com"));
    }

    #[test]
    fn find_subslice_basic() {
        assert_eq!(find_subslice(b"abc\r\n\r\nxyz", b"\r\n\r\n"), Some(3));
        assert_eq!(find_subslice(b"abc", b"\r\n\r\n"), None);
    }
}
