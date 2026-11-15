//! `ds-pac`：PAC 生成与分发。
//!
//! - [`pac`]：把 `running.json` 编译成 ES3 的 `FindProxyForURL` 脚本
//! - [`PacServer`]：把脚本以 `application/x-ns-proxy-autoconfig` 提供出去
//!
//! 设计要点：**先删 alt-svc 这类句柄不放这里**，PAC 只负责"这个域名走不走代理"，
//! 内容改写由 `ds-rules` + `ds-proxy` 负责。

#![forbid(unsafe_code)]

pub mod pac;

pub use pac::{
    build_pac_url, collect_direct_rules, collect_proxy_rules, generate_pac, is_pac_request,
    to_pac_regex, PacInfo, ALWAYS_DIRECT,
};

/// 一次 PAC 响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// PAC 端点：给 `ds-proxy` 在非 CONNECT 请求里判定并回包。
#[derive(Debug, Clone)]
pub struct PacServer {
    info: PacInfo,
}

impl PacServer {
    pub fn new(info: PacInfo) -> Self {
        Self { info }
    }

    pub fn info(&self) -> &PacInfo {
        &self.info
    }

    /// 命中 `/pac.js` 时返回响应，否则 `None`。
    pub fn respond(&self, target: &str) -> Option<PacResponse> {
        if !is_pac_request(target) {
            return None;
        }
        Some(PacResponse {
            status: 200,
            headers: vec![
                (
                    "Content-Type".to_string(),
                    "application/x-ns-proxy-autoconfig".to_string(),
                ),
                (
                    "Cache-Control".to_string(),
                    "no-cache, no-store, must-revalidate".to_string(),
                ),
                (
                    "Content-Length".to_string(),
                    self.info.content.len().to_string(),
                ),
                ("X-Pac-Hash".to_string(), self.info.hash.clone()),
            ],
            body: self.info.content.clone(),
        })
    }
}

/// ES3 兼容性自检（供 CI 与启动时断言用）。
pub fn assert_es3_compatible(content: &str) -> Result<(), String> {
    for forbidden in ["=>", "const ", "let ", "`", "(?<", "class ", "Object.assign"] {
        if content.contains(forbidden) {
            return Err(format!("PAC 含 JScript 不支持的语法：{forbidden}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ds_types::config::RuntimeConfig;

    fn info() -> PacInfo {
        let cfg = RuntimeConfig::from_json_str(
            r#"{ "host": "127.0.0.1", "port": 31181,
                 "intercepts": { "github.com": { ".*": {} } },
                 "whiteList": { "localhost": true } }"#,
        )
        .expect("parse");
        generate_pac(&cfg).expect("pac")
    }

    #[test]
    fn serves_pac_only_on_expected_path() {
        let server = PacServer::new(info());
        assert!(server.respond("/pac.js").is_some());
        assert!(server.respond("http://127.0.0.1:31181/pac.js?v=1").is_some());
        assert!(server.respond("/index.html").is_none());
    }

    #[test]
    fn response_headers_are_correct() {
        let server = PacServer::new(info());
        let resp = server.respond("/pac.js").expect("pac");
        assert_eq!(resp.status, 200);
        let map: std::collections::HashMap<_, _> = resp.headers.iter().cloned().collect();
        assert_eq!(
            map.get("Content-Type").map(String::as_str),
            Some("application/x-ns-proxy-autoconfig")
        );
        assert!(map.contains_key("X-Pac-Hash"));
        assert_eq!(
            resp.body.len().to_string(),
            map.get("Content-Length").cloned().unwrap()
        );
    }

    #[test]
    fn generated_content_passes_es3_check() {
        assert!(assert_es3_compatible(&info().content).is_ok());
        assert!(assert_es3_compatible("var a = () => 1").is_err());
        assert!(assert_es3_compatible("const a = 1").is_err());
    }
}
