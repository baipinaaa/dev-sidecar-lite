//! `dev-sidecar-lite` 的共享类型层。
//!
//! 目标：把 Node 版 `running.json` 的结构在 Rust 侧完整表达出来，
//! 供 `ds-rules` / `ds-dns` / `ds-pac` / `ds-proxy` 共用。

#![forbid(unsafe_code)]

pub mod config;
pub mod flex;
pub mod matcher;

pub use config::RuntimeConfig;
pub use flex::FlexBool;

#[cfg(test)]
mod smoke {
    use super::*;

    #[test]
    fn empty_json_uses_defaults() {
        let cfg = RuntimeConfig::from_json_str("{}").expect("parse empty");
        assert_eq!(cfg.port, 0);
        assert!(cfg.dns.ech.enabled);
        assert!(!cfg.dns.ech.try_all_providers);
        assert_eq!(cfg.tls_fragment.size, 32);
        assert_eq!(cfg.listen_addr(), "127.0.0.1:0");
    }

    #[test]
    fn tolerant_of_partial_config() {
        let json = r#"{
            "port": 31181,
            "intercepts": { "github.com": { ".*": { "sni": "baidu.com", "cacheDays": 0 } } },
            "dns": { "providers": { "my-doh": { "server": "https://wozhengtmrile.cc.cd/doh-ech-test" } } },
            "proxy": { "pacEnabled": true, "excludeIpList": { "a.com": true, "b.com": null } }
        }"#;
        let cfg = RuntimeConfig::from_json_str(json).expect("parse partial");
        assert_eq!(cfg.port, 31181);
        assert_eq!(cfg.listen_addr(), "127.0.0.1:31181");
        assert!(cfg.proxy.pac_enabled);
        assert_eq!(cfg.proxy.active_exclude_domains(), vec!["a.com"]);
        assert_eq!(
            cfg.intercepts["github.com"][".*"].as_ref().unwrap().sni.as_deref(),
            Some("baidu.com")
        );
    }
}
