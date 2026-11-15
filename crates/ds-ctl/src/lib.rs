//! ds-ctl —— 控制面协议与活跃连接管理。
//!
//! - `proto`：NDJSON 请求/响应
//! - `registry`：活跃连接注册表（列出 / 关闭 / 统计）
//! - `server`：命令分发与 stdio 循环

#![forbid(unsafe_code)]

pub mod proto;
pub mod registry;
pub mod server;

pub use proto::{Request, Response, COMMANDS};
pub use registry::{ConnHandle, ConnInfo, ConnKind, Registry};
pub use server::{dispatch, serve_stdio, ControlState};
