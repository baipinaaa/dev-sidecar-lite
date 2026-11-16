//! B1 探针：用 BoringSSL（`boring` crate）发起一次真正的 TLS 握手，
//! 经本地嗅探代理（tests/e2e/clienthello_capture.py）抓出 ClientHello，
//! 用来回答"BoringSSL 的指纹能不能对上 Chrome"。
//!
//! 用法：fingerprint-probe <嗅探代理地址> <目标域名> [chrome]
//!   例：fingerprint-probe 127.0.0.1:18999 example.com chrome
//!
//! 第三个参数为 `chrome` 时，按 Chrome 156 的实测 JA3 反推配置：
//! 套件顺序、groups 顺序、ALPN=h2、ECH grease。用来验证"配置到位后能否对上"。

/// Chrome 156 的 TLS1.2 套件顺序（不含 ECDHE-*-SHA，即 49161/49162）。
const CHROME_CIPHERS: &str = "ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305:ECDHE-RSA-AES128-SHA:ECDHE-RSA-AES256-SHA:AES128-GCM-SHA256:AES256-GCM-SHA384:AES128-SHA:AES256-SHA";

/// TLS1.3 套件顺序：AES128 → AES256 → CHACHA20（对应 JA3 的 4865-4866-4867）。
const CHROME_CIPHERSUITES: &str = "TLS_AES_128_GCM_SHA256:TLS_AES_256_GCM_SHA384:TLS_CHACHA20_POLY1305_SHA256";

/// Chrome 156 的 groups 顺序：X25519MLKEM768 → X25519 → P-256 → P-384。
const CHROME_GROUPS: &str = "X25519MLKEM768:X25519:P-256:P-384";

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
    let chrome_mode = args.get(3).map(|v| v == "chrome").unwrap_or(false);

    let mut tcp = TcpStream::connect(&endpoint).expect("连不上嗅探代理");
    let connect_req = format!("CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\n\r\n");
    tcp.write_all(connect_req.as_bytes())
        .expect("写 CONNECT 失败");
    read_connect_response(&mut tcp);

    let mut builder =
        boring::ssl::SslConnector::builder(boring::ssl::SslMethod::tls()).expect("SslConnector::builder");
    builder.set_verify_callback(boring::ssl::SslVerifyMode::NONE, |_ok, _ctx| true);

    if chrome_mode {
        // ALPN: h2 在前（Chrome 的顺序），wire 格式是"长度前缀 + 名字"
        builder
            .set_alpn_protos(&[2, b'h', b'2', 8, b'h', b't', b't', b'p', b'/', b'1', b'.', b'1'])
            .expect("set_alpn_protos");
        builder.set_cipher_list(CHROME_CIPHERS).expect("set_cipher_list");
        // boring 5 的命名是 set_curves_list（不是 OpenSSL 3 的 set_groups_list）
        builder.set_curves_list(CHROME_GROUPS).expect("set_curves_list");
        // Chrome 会随机化扩展顺序（SSL_set_permute_extensions），这也是为什么
        // 它的 JA3 每次抓都不一样、而 JA4 稳定（JA4 对顺序不敏感）。
        builder.set_permute_extensions(true);
        // 扩展 5（status_request）与 18（SCT）：Chrome 都会发
        builder.enable_ocsp_stapling();
        builder.enable_signed_cert_timestamps();
        // TLS1.3 套件用默认值（BoringSSL 默认就是 4865-4866-4867，与 Chrome 一致）。
        // 65037（ECH grease）只能在 SSL 对象上开，见下方 connect_chrome()。
        // 17613（ALPS）、27（compress_certificate）、51764：boring 5.5.0 未暴露，暂无解。
        println!("chrome 模式：套件/groups/ALPN/status_request/SCT/permute 已按 Chrome 配置");
    }

    let connector = builder.build();

    if chrome_mode {
        // per-connection 才能开 ECH grease（SSL_set_enable_ech_grease）
        let mut cfg = connector.configure().expect("configure");
        cfg.set_verify_hostname(false);
        let ssl = cfg.into_ssl(&host).expect("into_ssl");
        ssl.set_enable_ech_grease(true);
        let mut stream = boring::ssl::SslStream::new(ssl, tcp).expect("SslStream::new");
        std::pin::Pin::new(&mut stream)
            .connect()
            .expect("TLS 握手失败（chrome 模式）");
        let request = format!("GET / HTTP/1.1
Host: {host}
Connection: close

");
        stream.write_all(request.as_bytes()).expect("写请求失败");
        let mut out: Vec<u8> = Vec::new();
        let _ = stream.read_to_end(&mut out);
        println!("chrome 模式握手完成，收到 {} 字节", out.len());
        return;
    }

    let mut tls = connector.connect(&host, tcp).expect("TLS 握手失败");
    let request = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    tls.write_all(request.as_bytes()).expect("写请求失败");

    let mut out: Vec<u8> = Vec::new();
    let _ = tls.read_to_end(&mut out);
    println!("握手完成，收到 {} 字节", out.len());
}
