//! B1 探针：用 BoringSSL（`boring` crate）发起一次真正的 TLS 握手，
//! 经本地嗅探代理（tests/e2e/clienthello_capture.py）抓出 ClientHello，
//! 用来回答"BoringSSL 的指纹能不能对上 Chrome"。
//!
//! 用法：fingerprint-probe <嗅探代理地址> <目标域名>
//!   例：fingerprint-probe 127.0.0.1:18999 example.com

use std::io::{Read, Write};
use std::net::TcpStream;

/// 嗅探代理会先回 `HTTP/1.1 200 Connection Established\r\n\r\n`，
/// 必须逐字节读掉，否则这段 HTTP 文本会被当成 TLS 记录开头。
fn read_connect_response(stream: &mut TcpStream) {
    let mut acc: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    while acc.len() < 8192 {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                acc.push(byte[0]);
                if acc.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "127.0.0.1:18999".to_string());
    let host = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "example.com".to_string());

    let mut tcp = TcpStream::connect(&endpoint).expect("连不上嗅探代理");
    let connect_req = format!("CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\n\r\n");
    tcp.write_all(connect_req.as_bytes())
        .expect("写 CONNECT 失败");
    read_connect_response(&mut tcp);

    let mut builder =
        boring::ssl::SslConnector::builder(boring::ssl::SslMethod::tls()).expect("SslConnector::builder");
    builder.set_verify(boring::ssl::SslVerifyMode::NONE);
    let connector = builder.build();

    let mut tls = connector.connect(&host, tcp).expect("TLS 握手失败");
    let request = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    tls.write_all(request.as_bytes()).expect("写请求失败");

    let mut out: Vec<u8> = Vec::new();
    let _ = tls.read_to_end(&mut out);
    println!("握手完成，收到 {} 字节", out.len());
}
