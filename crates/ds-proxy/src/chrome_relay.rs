//! Chrome 指纹通道的透明中继。
//!
//! 走指纹通道的域名（`intercepts` 里 `fingerprint: "chrome"`）目的只有一个：
//! **让 Cloudflare 看到 Chrome 的 ClientHello**。这些域名不需要改内容，所以这里刻意
//! 不解析 HTTP —— 客户端 TLS 一完成握手，就把明文双向搬运。
//!
//! 之所以要这么绕：BoringSSL 是同步阻塞 IO，`SslStream` 既不能跨线程拆成读写两半，
//! 也不能塞进 tokio 的 reactor。所以结构是
//!
//! ```text
//!   客户端  ──tokio──▶  [async 读任务] ──mpsc──▶ ┌阻塞线程┐ ──▶ 上游(BoringSSL)
//!   客户端  ◀─tokio──  [async 写任务] ◀──mpsc──  └────────┘ ◀── 上游
//! ```
//!
//! 阻塞线程用 50ms 读超时在「客户端→上游」与「上游→客户端」之间轮转，
//! 超时是 `WouldBlock`/`TimedOut`（BoringSSL 对可重试 IO 是安全的），不是错误。

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{Context as _, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use ds_ctl::ConnHandle;

/// 单方向缓冲上限（条数），超过就让发送方等待，形成背压。
const CHANNEL_CAPACITY: usize = 32;
/// 单次搬运的块大小。
const CHUNK: usize = 32 * 1024;
/// 阻塞线程轮转间隔。
const POLL: Duration = Duration::from_millis(50);

/// 在「已解密的客户端连接」与「上游 TCP」之间做透明中继，返回（上行字节, 下行字节）。
///
/// `client` 是任意异步读写流（实际是 tokio-rustls 的 `TlsStream`），
/// `origin` 是刚建好的上游裸 TCP，握手在阻塞线程里用 BoringSSL 完成。
pub async fn relay<S>(
    client: S,
    origin: TcpStream,
    host: String,
    ech: Option<Vec<u8>>,
    handle: &mut ConnHandle<'_>,
) -> Result<(u64, u64)>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut client_read, mut client_write) = tokio::io::split(client);

    let (up_tx, up_rx) = mpsc::channel::<Vec<u8>>(CHANNEL_CAPACITY);
    let (down_tx, mut down_rx) = mpsc::channel::<Vec<u8>>(CHANNEL_CAPACITY);

    // 阻塞线程：BoringSSL 握手 + 双向搬运。
    let origin_task = tokio::task::spawn_blocking(move || -> Result<(u64, u64)> {
        let mut origin = ds_tls::chrome_connect(&host, origin, ech.as_deref())
            .with_context(|| format!("与 {host} 的上游 TLS 握手失败（Chrome 指纹通道）"))?;
        origin
            .get_ref()
            .set_read_timeout(Some(POLL))
            .context("设置上游读超时失败")?;

        let mut buffer = vec![0u8; CHUNK];
        let mut up_total = 0u64;
        let mut down_total = 0u64;
        let mut up_closed = false;

        loop {
            // 客户端 → 上游
            if !up_closed {
                match up_rx.try_recv() {
                    Ok(data) => {
                        origin
                            .write_all(&data)
                            .context("写上游（指纹通道）失败")?;
                        origin.flush().ok();
                        up_total += data.len() as u64;
                    }
                    Err(mpsc::error::TryRecvError::Empty) => {}
                    Err(mpsc::error::TryRecvError::Disconnected) => up_closed = true,
                }
            }

            // 上游 → 客户端
            match origin.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    down_total += n as u64;
                    if down_tx.blocking_send(buffer[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(err) => return Err(err).context("读上游（指纹通道）失败"),
            }
        }

        Ok((up_total, down_total))
    });

    let read_task = tokio::spawn(async move {
        let mut buffer = vec![0u8; CHUNK];
        loop {
            match client_read.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => {
                    if up_tx.send(buffer[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        // 丢掉发送端，让阻塞线程知道上行已结束。
        drop(up_tx);
    });

    let write_task = tokio::spawn(async move {
        while let Some(data) = down_rx.recv().await {
            if client_write.write_all(&data).await.is_err() {
                break;
            }
        }
        let _ = client_write.shutdown().await;
    });

    // 控制面取消：阻塞线程还在跑时可以被 `close` 打断。
    // 客户端或上游任意一侧断开时，阻塞线程自己会退出（read 返回 0 或通道断开）。
    let mut cancel = handle.take_cancel();
    let (up, down) = tokio::select! {
        result = origin_task => result.context("指纹中继线程 panic")??,
        _ = async {
            match cancel.as_mut() {
                Some(receiver) => {
                    receiver.await.ok();
                }
                None => std::future::pending::<()>().await,
            }
        } => (0u64, 0u64),
    };

    read_task.abort();
    write_task.abort();
    handle.finish(up, down);
    Ok((up, down))
}
