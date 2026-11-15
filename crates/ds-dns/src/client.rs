//! DNS 上游传输：DoH（HTTP/1.1 POST）、DoT、UDP。
//!
//! 对齐 Node 版行为：
//! - DoH 用 `application/dns-message` 裸报文（不是 JSON 接口）
//! - 上游证书不校验（中间设备常替换证书），签名仍校验
//! - `h3://` 暂降级为 HTTPS（QUIC 在阶段 2 接入）

use std::time::Duration;

use anyhow::{bail, Context, Result};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::timeout;
use tokio_rustls::TlsConnector;

use crate::wire::{self, Answer};

/// 单次查询的总预算。
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// 上游类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Https,
    Tls,
    Udp,
}

/// 解析后的上游目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub scheme: Scheme,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Target {
    fn authority(&self) -> String {
        // 443/853/53 这类默认端口不写进 Host 头，与浏览器一致
        match self.scheme {
            Scheme::Https if self.port == 443 => self.host.clone(),
            Scheme::Https => format!("{}:{}", self.host, self.port),
            _ => self.host.clone(),
        }
    }
}

/// 解析 provider 的 `server` 字段。
pub fn parse_server(server: &str) -> Result<Target> {
    let server = server.trim();
    if server.is_empty() {
        bail!("DNS 上游为空");
    }

    let (scheme, rest) = if let Some(rest) = server.strip_prefix("https://") {
        (Scheme::Https, rest)
    } else if let Some(rest) = server.strip_prefix("h3://") {
        tracing::warn!("h3:// 上游暂按 HTTPS 处理：{server}");
        (Scheme::Https, rest)
    } else if let Some(rest) = server.strip_prefix("tls://") {
        (Scheme::Tls, rest)
    } else if let Some(rest) = server.strip_prefix("udp://") {
        (Scheme::Udp, rest)
    } else if let Some(rest) = server.strip_prefix("tcp://") {
        (Scheme::Udp, rest)
    } else {
        (Scheme::Udp, server)
    };

    let (authority, path) = match rest.find('/') {
        Some(pos) => (&rest[..pos], rest[pos..].to_string()),
        None => (rest, String::new()),
    };

    let default_port = match scheme {
        Scheme::Https => 443,
        Scheme::Tls => 853,
        Scheme::Udp => 53,
    };

    let (host, port) = split_host_port(authority, default_port)?;
    let path = if path.is_empty() {
        match scheme {
            Scheme::Https => "/dns-query".to_string(),
            _ => String::new(),
        }
    } else {
        path
    };

    Ok(Target {
        scheme,
        host,
        port,
        path,
    })
}

fn split_host_port(authority: &str, default_port: u16) -> Result<(String, u16)> {
    if authority.is_empty() {
        bail!("DNS 上游缺少主机名");
    }

    if let Some(rest) = authority.strip_prefix('[') {
        let Some((host, tail)) = rest.split_once(']') else {
            bail!("IPv6 地址缺少闭合的 ]");
        };
        let port = match tail.strip_prefix(':') {
            Some(value) => value.parse::<u16>().context("IPv6 上游端口非法")?,
            None => default_port,
        };
        return Ok((host.to_string(), port));
    }

    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => Ok((
            host.to_string(),
            port.parse::<u16>().context("上游端口非法")?,
        )),
        _ => Ok((authority.to_string(), default_port)),
    }
}

/// 向上游发一次查询，返回全部记录。
pub async fn query(server: &str, name: &str, qtype: u16) -> Result<Vec<Answer>> {
    let target = parse_server(server)?;
    query_target(&target, name, qtype).await
}

/// 用已解析的目标发查询。
pub async fn query_target(target: &Target, name: &str, qtype: u16) -> Result<Vec<Answer>> {
    let wire = wire::build_query(wire::next_query_id(), name, qtype)?;

    let body = timeout(QUERY_TIMEOUT, exchange(target, &wire))
        .await
        .with_context(|| format!("查询 {name} 超时（{target:?}）"))??;

    wire::parse_response(&body).with_context(|| format!("解析 {name} 的响应失败（{target:?}）"))
}

async fn exchange(target: &Target, wire: &[u8]) -> Result<Vec<u8>> {
    match target.scheme {
        Scheme::Https => doh_exchange(target, wire).await,
        Scheme::Tls => dot_exchange(target, wire).await,
        Scheme::Udp => udp_exchange(target, wire).await,
    }
}

async fn doh_exchange(target: &Target, wire: &[u8]) -> Result<Vec<u8>> {
    let tcp = TcpStream::connect((target.host.as_str(), target.port))
        .await
        .with_context(|| format!("连接 DoH 上游 {}:{} 失败", target.host, target.port))?;

    let server_name = ServerName::try_from(target.host.clone())
        .map_err(|_| anyhow::anyhow!("DoH 主机名非法：{}", target.host))?;

    let connector = TlsConnector::from(ds_tls::client::insecure_client_config());
    let mut tls = connector
        .connect(server_name, tcp)
        .await
        .with_context(|| format!("与 DoH 上游 {} TLS 握手失败", target.host))?;

    let head = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/dns-message\r\nAccept: application/dns-message\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        target.path,
        target.authority(),
        wire.len()
    );

    tls.write_all(head.as_bytes()).await.context("写 DoH 请求头失败")?;
    tls.write_all(wire).await.context("写 DoH 请求体失败")?;
    tls.flush().await.ok();

    let mut resp = Vec::new();
    tls.read_to_end(&mut resp).await.context("读 DoH 响应失败")?;

    split_http_body(&resp)
}

fn split_http_body(resp: &[u8]) -> Result<Vec<u8>> {
    let Some(pos) = find_subslice(resp, b"\r\n\r\n") else {
        bail!("DoH 响应缺少完整的 HTTP 头");
    };

    let head = String::from_utf8_lossy(&resp[..pos]);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);

    if status != 200 {
        bail!("DoH 上游返回 HTTP {status}");
    }

    Ok(resp[pos + 4..].to_vec())
}

async fn dot_exchange(target: &Target, wire: &[u8]) -> Result<Vec<u8>> {
    let tcp = TcpStream::connect((target.host.as_str(), target.port))
        .await
        .with_context(|| format!("连接 DoT 上游 {}:{} 失败", target.host, target.port))?;

    let server_name = ServerName::try_from(target.host.clone())
        .map_err(|_| anyhow::anyhow!("DoT 主机名非法：{}", target.host))?;

    let connector = TlsConnector::from(ds_tls::client::insecure_client_config());
    let mut tls = connector
        .connect(server_name, tcp)
        .await
        .with_context(|| format!("与 DoT 上游 {} TLS 握手失败", target.host))?;

    let mut framed = Vec::with_capacity(wire.len() + 2);
    framed.extend_from_slice(&(wire.len() as u16).to_be_bytes());
    framed.extend_from_slice(wire);

    tls.write_all(&framed).await.context("写 DoT 查询失败")?;
    tls.flush().await.ok();

    let mut len = [0u8; 2];
    tls.read_exact(&mut len).await.context("读 DoT 长度失败")?;
    let size = u16::from_be_bytes(len) as usize;

    let mut body = vec![0u8; size];
    tls.read_exact(&mut body).await.context("读 DoT 响应失败")?;

    Ok(body)
}

async fn udp_exchange(target: &Target, wire: &[u8]) -> Result<Vec<u8>> {
    let socket = UdpSocket::bind("0.0.0.0:0").await.context("创建 UDP 套接字失败")?;
    socket
        .connect((target.host.as_str(), target.port))
        .await
        .with_context(|| format!("UDP 连接 {}:{} 失败", target.host, target.port))?;

    socket.send(wire).await.context("发送 UDP 查询失败")?;

    let mut buf = vec![0u8; 4096];
    let n = socket.recv(&mut buf).await.context("接收 UDP 响应失败")?;
    buf.truncate(n);

    Ok(buf)
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
    fn parses_doh_urls() {
        let t = parse_server("https://cloudflare-dns.com/dns-query").unwrap();
        assert_eq!(t.scheme, Scheme::Https);
        assert_eq!(t.host, "cloudflare-dns.com");
        assert_eq!(t.port, 443);
        assert_eq!(t.path, "/dns-query");

        let t = parse_server("https://1.1.1.1/dns-query").unwrap();
        assert_eq!(t.host, "1.1.1.1");

        let t = parse_server("https://dns.example.org:8443/q").unwrap();
        assert_eq!(t.port, 8443);
        assert_eq!(t.path, "/q");
    }

    #[test]
    fn parses_dot_and_bare_ip() {
        let t = parse_server("tls://1.12.12.12").unwrap();
        assert_eq!(t.scheme, Scheme::Tls);
        assert_eq!(t.port, 853);

        let t = parse_server("223.5.5.5").unwrap();
        assert_eq!(t.scheme, Scheme::Udp);
        assert_eq!(t.port, 53);

        let t = parse_server("udp://127.0.0.1:5353").unwrap();
        assert_eq!(t.port, 5353);
    }

    #[test]
    fn parses_ipv6_authority() {
        let t = parse_server("tls://[2606:4700::1111]:853").unwrap();
        assert_eq!(t.host, "2606:4700::1111");
        assert_eq!(t.port, 853);
    }

    #[test]
    fn h3_falls_back_to_https() {
        let t = parse_server("h3://223.5.5.5/dns-query").unwrap();
        assert_eq!(t.scheme, Scheme::Https);
        assert_eq!(t.path, "/dns-query");
    }

    #[test]
    fn rejects_empty() {
        assert!(parse_server("   ").is_err());
    }

    #[test]
    fn splits_http_body() {
        let mut resp = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n".to_vec();
        resp.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(split_http_body(&resp).unwrap(), vec![1, 2, 3, 4]);

        assert!(split_http_body(b"HTTP/1.1 500 Oops\r\n\r\n").is_err());
        assert!(split_http_body(b"garbage").is_err());
    }

    #[test]
    fn finds_needle() {
        assert_eq!(find_subslice(b"abcdef", b"cd"), Some(2));
        assert_eq!(find_subslice(b"ab", b"zzz"), None);
    }
}
