//! 绕行判定：哪些主机**不该**交给代理处理。
//!
//! 与 Node 版一致的三类来源：
//! 1. `whiteList`（白名单，命中即直连）
//! 2. `proxy.excludeIpList`（值必须为 `true` 才生效）
//! 3. 内网/回环地址
//!
//! 三者都会让该主机**不进入 MITM**，从而回到"系统代理之外"的路径。

use ds_types::config::RuntimeConfig;
use ds_types::matcher;

/// 绕行的原因，便于日志与统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BypassReason {
    /// 命中 `whiteList`
    WhiteList,
    /// 命中 `proxy.excludeIpList`
    ExcludeIpList,
    /// 内网 / 回环地址
    Private,
}

impl BypassReason {
    pub fn as_str(self) -> &'static str {
        match self {
            BypassReason::WhiteList => "whiteList",
            BypassReason::ExcludeIpList => "excludeIpList",
            BypassReason::Private => "private",
        }
    }
}

/// 判定是否需要绕行；`None` 表示正常走代理。
pub fn bypass_reason(config: &RuntimeConfig, host: &str) -> Option<BypassReason> {
    if config.is_white_listed(host) {
        return Some(BypassReason::WhiteList);
    }

    if config
        .proxy
        .exclude_ip_list
        .iter()
        .any(|(pattern, enabled)| enabled.get() && matcher::glob_match(pattern, host))
    {
        return Some(BypassReason::ExcludeIpList);
    }

    if is_private_host(host) {
        return Some(BypassReason::Private);
    }

    None
}

/// 是否内网 / 回环（按字面量判断，不做 DNS 解析）。
pub fn is_private_host(host: &str) -> bool {
    let host = host.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();

    if host == "localhost" || host.ends_with(".localhost") || host == "::1" {
        return true;
    }

    if host.starts_with("fe80:") || host.starts_with("fc") || host.starts_with("fd") {
        return true;
    }

    let octets: Vec<u8> = host
        .split('.')
        .map(|part| part.parse::<u8>().unwrap_or(0xFF))
        .collect();

    if octets.len() != 4 || octets.contains(&0xFF) {
        return false;
    }

    let [a, b, ..] = [octets[0], octets[1], octets[2], octets[3]];
    a == 127
        || a == 10
        || (a == 192 && b == 168)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 169 && b == 254)
        || (a == 100 && (64..=127).contains(&b))
        || a == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> RuntimeConfig {
        RuntimeConfig::from_json_str(
            r#"{
                "whiteList": { "*.icloud.com": true, "localhost": true },
                "proxy": { "excludeIpList": { "objects-origin.githubusercontent.com": true } }
            }"#,
        )
        .expect("parse")
    }

    #[test]
    fn white_list_bypasses() {
        assert_eq!(
            bypass_reason(&config(), "p01.icloud.com"),
            Some(BypassReason::WhiteList)
        );
    }

    #[test]
    fn exclude_list_bypasses() {
        assert_eq!(
            bypass_reason(&config(), "objects-origin.githubusercontent.com"),
            Some(BypassReason::ExcludeIpList)
        );
    }

    #[test]
    fn private_hosts_bypass() {
        // 用空配置排除白名单/排除名单的干扰，专测内网判定
        let cfg = RuntimeConfig::from_json_str("{}").expect("parse");
        for host in [
            "localhost",
            "127.0.0.1",
            "10.1.2.3",
            "192.168.3.1",
            "172.16.0.1",
            "172.31.255.255",
            "169.254.1.1",
            "100.64.0.1",
            "[::1]",
        ] {
            assert_eq!(
                bypass_reason(&cfg, host),
                Some(BypassReason::Private),
                "{host} 应判为内网"
            );
        }
    }

    #[test]
    fn white_list_wins_over_private() {
        // 判定顺序：白名单 → 排除名单 → 内网；localhost 同时在白名单里
        assert_eq!(
            bypass_reason(&config(), "localhost"),
            Some(BypassReason::WhiteList)
        );
    }

    #[test]
    fn public_hosts_pass_through() {
        for host in [
            "github.com",
            "www.baidu.com",
            "8.8.8.8",
            "172.32.0.1",
            "172.15.0.1",
        ] {
            assert_eq!(bypass_reason(&config(), host), None, "{host} 不该绕行");
        }
    }
}
