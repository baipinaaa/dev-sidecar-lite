//! 控制口：把 NDJSON 请求分发到注册表与运行时。

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;

use crate::proto::{Request, Response, COMMANDS};
use crate::registry::Registry;

/// 运行时可共享状态。
#[derive(Debug)]
pub struct ControlState {
    pub registry: Registry,
    pub started: Instant,
    pub shutdown: AtomicBool,
}

impl ControlState {
    pub fn new() -> Self {
        Self {
            registry: Registry::new(),
            started: Instant::now(),
            shutdown: AtomicBool::new(false),
        }
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }
}

impl Default for ControlState {
    fn default() -> Self {
        Self::new()
    }
}

/// 分发一条请求。
pub fn dispatch(state: &ControlState, req: &Request) -> Response {
    match req.cmd.as_str() {
        "ping" => Response::ok(
            req.id,
            json!({
                "pong": true,
                "uptimeMs": state.started.elapsed().as_millis() as u64,
            }),
        ),

        "getList" => {
            let list = state.registry.list();
            Response::ok(req.id, json!({ "count": list.len(), "list": list }))
        }

        "close" => {
            let Some(id) = req.args.get("id").and_then(|value| value.as_u64()) else {
                return Response::err(req.id, "缺少参数 args.id");
            };
            if state.registry.close(id) {
                Response::ok(req.id, json!({ "closed": id }))
            } else {
                Response::err(req.id, format!("连接 {id} 不存在"))
            }
        }

        "closeAll" => {
            let closed = state.registry.close_all();
            Response::ok(req.id, json!({ "closed": closed }))
        }

        "reTest" => {
            let Some(host) = req.args.get("host").and_then(|value| value.as_str()) else {
                return Response::err(req.id, "缺少参数 args.host");
            };
            // 评分与测速在阶段 2 的 ds-dns 接入，这里先回执，避免控制面阻塞
            Response::ok(
                req.id,
                json!({
                    "host": host,
                    "reTested": false,
                    "note": "IP 评分与测速在 ds-dns 阶段接入",
                }),
            )
        }

        "stats" => {
            let (active, up, down) = state.registry.stats();
            Response::ok(
                req.id,
                json!({
                    "active": active,
                    "upBytes": up,
                    "downBytes": down,
                    "uptimeMs": state.started.elapsed().as_millis() as u64,
                }),
            )
        }

        "shutdown" => {
            state.shutdown.store(true, Ordering::Relaxed);
            Response::ok(req.id, json!({ "shuttingDown": true }))
        }

        other => Response::err(
            req.id,
            format!("未知命令 {other}；支持：{}", COMMANDS.join(", ")),
        ),
    }
}

/// 在 stdin/stdout 上跑 NDJSON 循环，直到 EOF 或 `shutdown`。
pub fn serve_stdio(state: Arc<ControlState>) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        if state.is_shutdown() {
            break;
        }

        let line = line?;
        let response = match Request::parse_line(&line) {
            Ok(Some(req)) => dispatch(&state, &req),
            Ok(None) => continue,
            Err(err) => Response::err(0, err),
        };

        writeln!(stdout, "{}", response.to_line())?;
        stdout.flush()?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ConnKind;

    fn state_with_one_conn() -> ControlState {
        let state = ControlState::new();
        let handle = state.registry.register("example.com", 443, ConnKind::Tunnel);
        handle.finish(1, 2);
        state
    }

    #[test]
    fn ping_reports_uptime() {
        let state = ControlState::new();
        let resp = dispatch(&state, &Request::new(1, "ping"));
        assert!(resp.ok);
        assert_eq!(resp.data.unwrap()["pong"], true);
    }

    #[test]
    fn get_list_returns_connections() {
        let state = state_with_one_conn();
        let resp = dispatch(&state, &Request::new(2, "getList"));
        assert!(resp.ok);
        let data = resp.data.unwrap();
        assert_eq!(data["count"], 1);
        assert_eq!(data["list"][0]["host"], "example.com");
    }

    #[test]
    fn close_missing_connection_errors() {
        let state = ControlState::new();
        let mut req = Request::new(3, "close");
        req.args = json!({ "id": 42 });
        let resp = dispatch(&state, &req);
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("不存在"));
    }

    #[test]
    fn close_without_id_errors() {
        let state = ControlState::new();
        let resp = dispatch(&state, &Request::new(4, "close"));
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("args.id"));
    }

    #[test]
    fn retest_requires_host_and_is_acknowledged() {
        let state = ControlState::new();
        let resp = dispatch(&state, &Request::new(5, "reTest"));
        assert!(!resp.ok);

        let mut req = Request::new(6, "reTest");
        req.args = json!({ "host": "github.com" });
        let resp = dispatch(&state, &req);
        assert!(resp.ok);
        assert_eq!(resp.data.unwrap()["host"], "github.com");
    }

    #[test]
    fn shutdown_sets_flag() {
        let state = ControlState::new();
        let resp = dispatch(&state, &Request::new(7, "shutdown"));
        assert!(resp.ok);
        assert!(state.is_shutdown());
    }

    #[test]
    fn unknown_command_lists_supported() {
        let state = ControlState::new();
        let resp = dispatch(&state, &Request::new(8, "nope"));
        assert!(!resp.ok);
        let err = resp.error.unwrap();
        assert!(err.contains("getList"));
        assert!(err.contains("reTest"));
    }
}
