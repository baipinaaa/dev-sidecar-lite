//! ds-proxy —— dev-sidecar-lite 数据面主程序。
//!
//! 阶段 1 目标：能作为 HTTP 代理工作
//! - `CONNECT host:port` → 建立隧道并双向透传（HTTPS 全通）
//! - 明文 `GET http://host/path` → 重组为 origin-form 后转发
//!
//! MITM（解密并改写）在阶段 1 后半段接入 `ds-tls`。

#![forbid(unsafe_code)]

mod http;
mod mitm;
mod proxy;
mod tunnel;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use ds_types::RuntimeConfig;

/// 运行参数。
#[derive(Debug, Clone)]
pub struct ProxyOptions {
    pub host: String,
    pub port: u16,
    pub config: Arc<RuntimeConfig>,
    /// 是否记录每个连接的明细（`--verbose`）
    pub verbose: bool,
    /// MITM 运行环境；CA 不可用时为 `None`（降级为纯隧道）
    pub mitm: Option<mitm::Mitm>,
    /// 控制口状态（活跃连接表 + 关闭标志）
    pub control: Arc<ds_ctl::ControlState>,
}

const USAGE: &str = "\
ds-proxy —— dev-sidecar-lite 数据面

用法:
    ds-proxy [running.json] [选项]

选项:
    -c, --config <path>   指定 running.json（等价于位置参数）
    -p, --port <port>     覆盖监听端口
        --host <host>     覆盖监听地址（默认 127.0.0.1）
    -v, --verbose         打印每个连接的明细
        --print-summary   解析配置后打印摘要并退出
    -h, --help            显示本帮助
";

fn main() -> Result<()> {
    init_tracing();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }

    let mut config_path: Option<PathBuf> = None;
    let mut port_override: Option<u16> = None;
    let mut host_override: Option<String> = None;
    let mut verbose = false;
    let mut print_summary = false;

    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-c" | "--config" => {
                let value = iter.next().context("--config 缺少路径")?;
                config_path = Some(PathBuf::from(value));
            }
            "-p" | "--port" => {
                let value = iter.next().context("--port 缺少端口号")?;
                port_override = Some(value.parse().context("端口号非数字")?);
            }
            "--host" => {
                let value = iter.next().context("--host 缺少地址")?;
                host_override = Some(value.clone());
            }
            "-v" | "--verbose" => verbose = true,
            "--print-summary" => print_summary = true,
            other if other.starts_with('-') => {
                anyhow::bail!("未知选项：{other}\n\n{USAGE}");
            }
            other => config_path = Some(PathBuf::from(other)),
        }
    }

    let config = load_config(config_path.as_deref())?;

    let host = host_override.unwrap_or_else(|| {
        if config.host.trim().is_empty() {
            "127.0.0.1".to_string()
        } else {
            config.host.clone()
        }
    });
    let port = port_override.unwrap_or(if config.port == 0 {
        31181
    } else {
        config.port
    });

    if print_summary {
        println!("监听: {host}:{port}");
        println!("intercepts: {} 个域名", config.intercepts.len());
        println!("dns.providers: {} 个", config.dns.providers.len());
        println!("dns.mapping: {} 条", config.dns.mapping.len());
        println!("preSetIpList: {} 条", config.pre_set_ip_list.len());
        println!("ech.domains: {} 条", config.ech_domains().len());
        println!("whiteList: {} 条", config.white_list.len());
        println!(
            "dynamicUpstreams: {} 条",
            config.dynamic_upstreams.len()
        );
        println!("pac.enabled: {}", config.proxy.pac_enabled);
        return Ok(());
    }

    let options = ProxyOptions {
        host,
        port,
        config: Arc::new(config),
        verbose,
        mitm: mitm::Mitm::try_new(),
        control: Arc::new(ds_ctl::ControlState::new()),
    };

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("创建 tokio 运行时失败")?
        .block_on(proxy::run(options))
}

fn load_config(path: Option<&std::path::Path>) -> Result<RuntimeConfig> {
    let Some(path) = path else {
        tracing::warn!("未指定 running.json，使用内置默认值");
        return Ok(RuntimeConfig::default());
    };

    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取配置失败：{}", path.display()))?;
    let config = RuntimeConfig::from_json_str(&text)
        .with_context(|| format!("解析配置失败：{}", path.display()))?;
    tracing::info!(
        "已加载配置 {}（intercepts {} 个，dns.mapping {} 条）",
        path.display(),
        config.intercepts.len(),
        config.dns.mapping.len()
    );
    Ok(config)
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("ds_proxy=info,info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
