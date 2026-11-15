//! ECH 端到端验证：解析 ECHConfig → 用 ECH 握手 → 打印是否生效。
//!
//! 用法：
//!   cargo run -p ds-dns --example ech_check -- linux.do
//!   cargo run -p ds-dns --example ech_check -- discord.com https://cloudflare-dns.com/dns-query
//!
//! 验收标准（步骤 10）：
//! - `linux.do` / `discord.com` 能取到 ECHConfigList（数十上百字节）
//! - 带 ECH 的握手成功，且不出现 SNI 明文阻断

use std::net::SocketAddr;

use anyhow::{Context, Result};
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber_init();

    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "linux.do".to_string());
    let server = args
        .next()
        .unwrap_or_else(|| "https://cloudflare-dns.com/dns-query".to_string());

    println!("域名：{host}");
    println!("DoH ：{server}");

    let (ips, ech) = ds_dns::resolve_a_and_ech(&server, &host).await?;
    println!("A 记录：{ips:?}");

    let Some(ech_bytes) = ech else {
        println!("结果：没有 ECHConfig（该域名可能不支持 ECH）");
        return Ok(());
    };
    println!("ECHConfigList：{} 字节", ech_bytes.len());

    let addr = match ips.first() {
        Some(ip) => {
            let ip: std::net::IpAddr = ip.parse().context("A 记录不是合法 IP")?;
            SocketAddr::new(ip, 443)
        }
        None => {
            println!("结果：没有 A 记录，跳过握手");
            return Ok(());
        }
    };

    let config = ds_tls::build_upstream_config(false, Some(&ech_bytes))?;
    let connector = TlsConnector::from(config);
    let server_name = ServerName::try_from(host.clone()).context("主机名无法用于 TLS")?;

    println!("连接：{addr}");
    let tcp = tokio::time::timeout(std::time::Duration::from_secs(10), TcpStream::connect(addr))
        .await
        .context("TCP 连接超时")?
        .context("TCP 连接失败")?;

    let mut tls = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        connector.connect(server_name, tcp),
    )
    .await
    .context("TLS 握手超时")?
    .context("TLS 握手失败")?;

    let (_, conn) = tls.get_ref();
    println!("握手成功：协议 {:?}", conn.protocol_version());
    println!("套件 {:?}", conn.negotiated_cipher_suite());
    println!("结果：在 ECH 配置下完成握手（若前面没有 RST，说明链路可用）");

    use tokio::io::AsyncWriteExt;
    tls.write_all(b"HEAD / HTTP/1.1\r\nHost: \r\nConnection: close\r\n\r\n")
        .await
        .ok();
    tls.shutdown().await.ok();

    Ok(())
}

fn tracing_subscriber_init() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
}
