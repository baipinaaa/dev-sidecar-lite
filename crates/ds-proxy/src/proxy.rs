//! 监听端口并按请求类型分发。

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use ds_ctl::ConnKind;
use tokio::net::{TcpListener, TcpStream};

use crate::http::{self, MAX_HEAD};
use crate::tunnel;
use crate::ProxyOptions;

pub async fn run(opts: ProxyOptions) -> Result<()> {
    let addr = format!("{}:{}", opts.host, opts.port);
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("监听 {addr} 失败"))?;

    tracing::info!(
        "ds-proxy 已启动，监听 {addr}（intercepts {} 个域名，MITM {}，verbose={}）",
        opts.config.intercepts.len(),
        if opts.mitm.is_some() { "启用" } else { "关闭" },
        opts.verbose
    );

    // 控制口：stdin/stdout 上的 NDJSON；宿主进程（ds-core-host）通过管道驱动
    if std::env::var_os("DS_NO_IPC").is_none() {
        let control = Arc::clone(&opts.control);
        std::thread::Builder::new()
            .name("ds-ctl".into())
            .spawn(move || {
                if let Err(err) = ds_ctl::serve_stdio(control) {
                    tracing::warn!("控制口结束：{err}");
                }
            })
            .context("启动控制口线程失败")?;
    }

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!("accept 失败：{err}");
                continue;
            }
        };

        let opts = opts.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_conn(stream, peer, &opts).await {
                tracing::debug!("{peer} 处理结束：{err:#}");
            }
        });
    }
}

async fn handle_conn(mut stream: TcpStream, peer: SocketAddr, opts: &ProxyOptions) -> Result<()> {
    let _ = stream.set_nodelay(true);

    let head = http::read_head(&mut stream, MAX_HEAD).await?;
    let req = http::RequestHead::parse(&head.bytes)
        .with_context(|| format!("解析 {peer} 的请求头失败"))?;

    if req.method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = req.authority(443)?;

        let mitm = opts
            .mitm
            .as_ref()
            .filter(|_| crate::mitm::should_intercept(&opts.config, &host));

        let kind = if mitm.is_some() {
            ConnKind::Mitm
        } else {
            ConnKind::Tunnel
        };

        let mut handle = opts.control.registry.register(host.clone(), port, kind);

        match mitm {
            Some(mitm) => {
                crate::mitm::handle(stream, req, head.leftover, mitm, opts, &mut handle).await
            }
            None => tunnel::handle(stream, req, head.leftover, opts, &mut handle).await,
        }
    } else {
        let (host, port) = req.authority(80).unwrap_or_else(|_| ("unknown".to_string(), 80));
        let mut handle = opts.control.registry.register(host, port, ConnKind::Http);
        let result = http::forward(stream, req, head.leftover, opts).await;
        handle.finish(0, 0);
        result
    }
}
