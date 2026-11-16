//! 抓取 `ds-tls::chrome_connect` 发出的 ClientHello，供 JA4 比对。
//!
//! 用法：
//! ```text
//! cargo run -p ds-tls --bin chrome-probe -- out/chrome-clienthello.bin example.com
//! python tests/e2e/clienthello_capture.py --compute out/chrome-clienthello.bin
//! ```
//!
//! 原理：本地起一个只读的 TCP 监听假装成目标站，客户端用 Chrome 指纹去握手；
//! 监听端把收到的第一个 TLS record 原样落盘（不含 ServerHello，所以握手必然失败，
//! 这对"只看 ClientHello"没有影响）。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

fn main() {
    let out_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "chrome-clienthello.bin".to_string());
    let host = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "example.com".to_string());

    let listener = TcpListener::bind("127.0.0.1:0").expect("无法监听本地端口");
    let addr = listener.local_addr().expect("取本地地址失败");

    let recorder_path = out_path.clone();
    let recorder = std::thread::spawn(move || -> usize {
        let (mut socket, _peer) = listener.accept().expect("accept 失败");
        socket.set_read_timeout(Some(Duration::from_millis(1500))).ok();

        let mut buffer = [0u8; 16 * 1024];
        let mut collected: Vec<u8> = Vec::new();
        loop {
            match socket.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    collected.extend_from_slice(&buffer[..n]);
                    // TLS record 头：type(1) + version(2) + length(2, big-endian)
                    if collected.len() >= 5 {
                        let need = 5 + u16::from_be_bytes([collected[3], collected[4]]) as usize;
                        if collected.len() >= need {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }

        std::fs::write(&recorder_path, &collected).expect("写入 ClientHello 失败");
        // 明确关掉连接，让客户端尽快从握手里返回。
        let _ = socket.write_all(&[]);
        let _ = socket.shutdown(std::net::Shutdown::Both);
        collected.len()
    });

    let tcp = TcpStream::connect(addr).expect("连接本地监听失败");
    // 握手会失败（对端不发 ServerHello），这里只关心"发出去了什么"。
    // 但如果连 ClientHello 都没发出来，失败原因就是唯一线索，必须打出来。
    if let Err(err) = ds_tls::chrome_connect(&host, tcp, None, true) {
        eprintln!("握手未完成（预期，对端不回 ServerHello）：{err:#}");
    }

    let size = recorder.join().expect("采集线程 panic");
    println!("captured {size} bytes -> {out_path}");
    if size == 0 {
        eprintln!("没有抓到任何字节：说明 chrome_connect 在发 ClientHello 之前就失败了，看上面的错误");
        std::process::exit(2);
    }
    println!("next: python tests/e2e/clienthello_capture.py --compute {out_path}");
}
