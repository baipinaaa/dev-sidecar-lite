//! `ds-rules`：规则引擎。
//!
//! 组成部分：
//! - [`rules::Rules`]：`intercepts` 编译与命中（域名 + 路径，多规则合并）
//! - [`guard`]：绕行判定（whiteList / excludeIpList / 内网）
//! - [`rewrite`]：请求/响应头改写、脚本注入与 body 改写判定
//!
//! 约定：本 crate 不做任何 I/O，方便单测与被 `ds-proxy` / `ds-pac` 共用。

#![forbid(unsafe_code)]

pub mod guard;
pub mod rewrite;
pub mod rules;

pub use guard::{bypass_reason, is_private_host, BypassReason};
pub use rewrite::{
    apply_header_rules, apply_request, apply_response, merged_request_headers, needs_full_body,
    removed_request_headers, wants_body_rewrite, wants_script_injection, HeaderList, RewriteStats,
};
pub use rules::{merge_replace_rules, Rules};

#[cfg(test)]
mod integration {
    use super::*;

    fn config() -> ds_types::RuntimeConfig {
        let json = r#"{
            "whiteList": { "localhost": true },
            "proxy": { "excludeIpList": { "objects-origin.githubusercontent.com": true } },
            "intercepts": {
                "*.githubusercontent.com": {
                    ".*": {
                        "sni": "baidu.com",
                        "requestReplace": { "headers": { "accept-language": "en-US,en;q=0.8" } }
                    }
                },
                "github.com": { ".*": { "sni": "baidu.com", "cacheDays": 3 } }
            }
        }"#;
        ds_types::RuntimeConfig::from_json_str(json).expect("parse")
    }

    #[test]
    fn end_to_end_decision_for_raw_githubusercontent() {
        let config = config();
        let rules = Rules::from_config(&config);

        // 命中 MITM
        assert!(rules.should_intercept("raw.githubusercontent.com"));
        let options = rules
            .options_for("raw.githubusercontent.com", "/a/b/c.txt")
            .expect("options");

        // SNI 伪装 + 请求头改写
        assert_eq!(options.sni.as_deref(), Some("baidu.com"));
        assert_eq!(
            merged_request_headers(&options)
                .get("accept-language")
                .map(String::as_str),
            Some("en-US,en;q=0.8")
        );

        // 排除名单里的那个子域不被 MITM
        assert!(!rules.should_intercept("objects-origin.githubusercontent.com"));
        assert_eq!(
            bypass_reason(&config, "objects-origin.githubusercontent.com"),
            Some(BypassReason::ExcludeIpList)
        );
    }

    #[test]
    fn response_rewrite_removes_alt_svc_for_ech_domain() {
        let mut options = ds_types::config::InterceptOptions::default();
        options.response_replace = Some(ds_types::config::ReplaceRules {
            headers: Some(
                [("alt-svc".to_string(), serde_json::json!("[remove]"))]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        });

        let mut headers = vec![
            ("alt-svc".to_string(), "h3=\":443\"".to_string()),
            ("content-type".to_string(), "text/html".to_string()),
        ];
        let stats = apply_response(&options, &mut headers);
        assert_eq!(stats.removed, 1);
        assert_eq!(headers.len(), 1);
        assert!(options.removes_alt_svc());
    }
}
