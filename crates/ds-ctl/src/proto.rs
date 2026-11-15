//! 控制协议：一行一个 JSON（NDJSON），双向同构。
//!
//! 请求：`{"id":1,"cmd":"getList","args":{}}`
//! 响应：`{"id":1,"ok":true,"data":{...}}` 或 `{"id":1,"ok":false,"error":"..."}`

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 客户端 → 数据面。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: u64,
    pub cmd: String,
    #[serde(default)]
    pub args: Value,
}

impl Request {
    pub fn new(id: u64, cmd: impl Into<String>) -> Self {
        Self {
            id,
            cmd: cmd.into(),
            args: Value::Null,
        }
    }

    /// 从一行文本解析；空行返回 `Ok(None)`。
    pub fn parse_line(line: &str) -> Result<Option<Self>, String> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(None);
        }
        serde_json::from_str(line)
            .map(Some)
            .map_err(|err| format!("请求不是合法 JSON：{err}"))
    }
}

/// 数据面 → 客户端。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(id: u64, data: Value) -> Self {
        Self {
            id,
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn empty(id: u64) -> Self {
        Self {
            id,
            ok: true,
            data: None,
            error: None,
        }
    }

    pub fn err(id: u64, message: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            data: None,
            error: Some(message.into()),
        }
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|err| {
            format!(r#"{{"id":{},"ok":false,"error":"序列化响应失败：{}"}}"#, self.id, err)
        })
    }
}

/// 支持的命令名（用于错误提示与自检）。
pub const COMMANDS: &[&str] = &["ping", "getList", "close", "closeAll", "reTest", "stats", "shutdown"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_with_and_without_args() {
        let req = Request::parse_line(r#"{"id":7,"cmd":"getList"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(req.id, 7);
        assert_eq!(req.cmd, "getList");
        assert!(req.args.is_null());

        let req = Request::parse_line(r#"{"id":8,"cmd":"close","args":{"id":3}}"#)
            .unwrap()
            .unwrap();
        assert_eq!(req.args["id"], 3);
    }

    #[test]
    fn empty_line_is_ignored() {
        assert!(Request::parse_line("   ").unwrap().is_none());
    }

    #[test]
    fn invalid_json_reports_error() {
        assert!(Request::parse_line("{oops}").is_err());
    }

    #[test]
    fn responses_round_trip() {
        let line = Response::ok(1, serde_json::json!({"count": 2})).to_line();
        let back: Response = serde_json::from_str(&line).unwrap();
        assert!(back.ok);
        assert_eq!(back.data.unwrap()["count"], 2);

        let line = Response::err(2, "boom").to_line();
        let back: Response = serde_json::from_str(&line).unwrap();
        assert!(!back.ok);
        assert_eq!(back.error.as_deref(), Some("boom"));
    }
}
