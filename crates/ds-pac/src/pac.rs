//! PAC 生成：把运行期配置编译成 `FindProxyForURL` 脚本。
//!
//! 与 Node 版 `packages/mitmproxy/src/lib/pac.js` 逐条对齐：
//!
//! | 来源 | 归类 |
//! | --- | --- |
//! | `intercepts` / `preSetIpList` / `dns.mapping` / `dns.ech.domains` / `dns.nat64.domains` | PROXY |
//! | `proxy.excludeIpList`（值为 `true`）/ `whiteList`（值为 `true`）/ 内网地址 | DIRECT |
//!
//! 两条硬约束：
//! 1. **PAC 由 Windows JScript 执行，只能是 ES3**：`var` + `function` + `new RegExp`，
//!    不能用箭头函数 / `let` / `const` / 模板串 / 命名捕获组 `(?<name>` / 后行断言 `(?<=`。
//!    任何一处语法错误都会让整份 PAC 解析失败（WinHTTP 12166），系统代理直接全挂。
//! 2. **`*.example.com` 必须同时覆盖根域**：浏览器打开 `https://example.com/` 时
//!    不命中 `^.*\.example\.com$`，会被判 DIRECT —— 对被墙站点等于打不开。

use ds_types::config::RuntimeConfig;
use sha2::{Digest, Sha256};

/// 永远直连的地址（含 PAC 自身，避免自环）。
pub const ALWAYS_DIRECT: &[&str] = &[
    "localhost",
    "*.localhost",
    "127.*.*.*",
    "10.*.*.*",
    "172.16.*.*",
    "172.17.*.*",
    "172.18.*.*",
    "172.19.*.*",
    "172.20.*.*",
    "172.21.*.*",
    "172.22.*.*",
    "172.23.*.*",
    "172.24.*.*",
    "172.25.*.*",
    "172.26.*.*",
    "172.27.*.*",
    "172.28.*.*",
    "172.29.*.*",
    "172.30.*.*",
    "172.31.*.*",
    "192.168.*.*",
    "[::1]",
    "::1",
];

/// 生成结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacInfo {
    pub content: String,
    /// 内容 sha256 前 8 位，用于拼 `?v=` 绕过 WinHTTP 的 URL 缓存
    pub hash: String,
    /// `host:port`
    pub proxy: String,
    pub proxy_rules: usize,
    pub direct_rules: usize,
}

/// 把配置里的匹配串转成 PAC 可用的正则源码。
pub fn to_pac_regex(pattern: &str) -> Option<String> {
    if pattern.is_empty() {
        return None;
    }

    let mut source = if pattern.starts_with('^') {
        pattern.to_string()
    } else if pattern.contains('*') || pattern.contains('?') {
        let (prefix, body) = match pattern.strip_prefix("*.") {
            Some(rest) => ("(.+\\.)?", rest),
            None => ("", pattern),
        };
        let escaped = body
            .replace('.', "\\.")
            .replace('*', ".*")
            .replace('?', ".");
        format!("^{prefix}{escaped}$")
    } else {
        format!("^{}$", pattern.replace('.', "\\."))
    };

    // JScript 不支持命名捕获组与后行断言：降级为普通分组，先保证能编译。
    source = source
        .replace("(?<=", "(?:")
        .replace("(?<!", "(?:")
        .replace("(?<", "(?:");

    Some(source)
}

fn push_rule(target: &mut Vec<String>, pattern: &str) {
    if let Some(regex) = to_pac_regex(pattern) {
        target.push(regex);
    }
}

/// 收集「应该走代理」的匹配串。
pub fn collect_proxy_rules(config: &RuntimeConfig) -> Vec<String> {
    let mut rules = Vec::new();

    // intercepts：JS 版用 Object.keys，不看值
    for pattern in config.intercepts.keys() {
        push_rule(&mut rules, pattern);
    }
    for pattern in config.pre_set_ip_list.keys() {
        push_rule(&mut rules, pattern);
    }
    // dns.mapping：值为 null 表示显式关闭
    for (pattern, provider) in &config.dns.mapping {
        if provider.is_some() {
            push_rule(&mut rules, pattern);
        }
    }
    for pattern in &config.dns.ech.domains {
        push_rule(&mut rules, pattern);
    }
    for pattern in &config.dns.nat64.domains {
        push_rule(&mut rules, pattern);
    }

    rules
}

/// 收集「必须直连」的匹配串（由调用方保证优先级高于代理规则）。
pub fn collect_direct_rules(config: &RuntimeConfig) -> Vec<String> {
    let mut rules = Vec::new();
    for (pattern, enabled) in &config.proxy.exclude_ip_list {
        if enabled.get() {
            push_rule(&mut rules, pattern);
        }
    }
    for (pattern, enabled) in &config.white_list {
        if enabled.get() {
            push_rule(&mut rules, pattern);
        }
    }
    rules
}

fn resolve_proxy_address(config: &RuntimeConfig) -> Option<String> {
    if config.port == 0 {
        return None;
    }
    let host = if config.host.is_empty() || config.host == "0.0.0.0" {
        "127.0.0.1"
    } else {
        config.host.as_str()
    };
    Some(format!("{host}:{}", config.port))
}

fn js_array(items: &[String]) -> String {
    if items.is_empty() {
        return "[]".to_string();
    }
    let body = items
        .iter()
        .map(|item| format!("'{}'", item.replace('\\', "\\\\").replace('\'', "\\'")))
        .collect::<Vec<_>>()
        .join(",\n    ");
    format!("[\n    {body}\n  ]")
}

/// 生成 PAC 脚本。
pub fn generate_pac(config: &RuntimeConfig) -> Option<PacInfo> {
    let proxy = resolve_proxy_address(config)?;

    let proxy_rules = collect_proxy_rules(config);

    let mut direct_all: Vec<String> = Vec::new();
    for pattern in ALWAYS_DIRECT {
        push_rule(&mut direct_all, pattern);
    }
    for rule in collect_direct_rules(config) {
        if !direct_all.contains(&rule) {
            direct_all.push(rule);
        }
    }

    let content = format!(
        r#"// DevSidecar PAC —— 自动生成，请勿手工修改
// 规则来源：intercepts / preSetIpList / dns.mapping / ech.domains（走代理）
//           excludeIpList / whiteList / 内网地址（直连）
var PROXY = 'PROXY {proxy}; DIRECT'

var DIRECT_RAW = {direct}

var PROXY_RAW = {proxied}

var DIRECT_RULES = []
var PROXY_RULES = []

;(function () {{
  var i
  for (i = 0; i < DIRECT_RAW.length; i++) {{
    try {{ DIRECT_RULES.push(new RegExp(DIRECT_RAW[i], 'i')) }} catch (e) {{}}
  }}
  for (i = 0; i < PROXY_RAW.length; i++) {{
    try {{ PROXY_RULES.push(new RegExp(PROXY_RAW[i], 'i')) }} catch (e) {{}}
  }}
}})()

function matchAny (rules, host) {{
  for (var i = 0; i < rules.length; i++) {{
    if (rules[i].test(host)) {{
      return true
    }}
  }}
  return false
}}

function FindProxyForURL (url, host) {{
  if (host == null || host.length === 0) {{
    return 'DIRECT'
  }}
  if (matchAny(DIRECT_RULES, host)) {{
    return 'DIRECT'
  }}
  if (matchAny(PROXY_RULES, host)) {{
    return PROXY
  }}
  return 'DIRECT'
}}
"#,
        proxy = proxy,
        direct = js_array(&direct_all),
        proxied = js_array(&proxy_rules),
    );

    let hash = hex::encode(Sha256::digest(content.as_bytes()))[..8].to_string();

    Some(PacInfo {
        content,
        hash,
        proxy,
        proxy_rules: proxy_rules.len(),
        direct_rules: direct_all.len(),
    })
}

/// 带内容指纹的 PAC 地址（`?v=<hash>` 用于绕过 WinHTTP 的 URL 级缓存）。
pub fn build_pac_url(config: &RuntimeConfig, pac: &PacInfo) -> Option<String> {
    let proxy = resolve_proxy_address(config)?;
    Some(format!("http://{proxy}/pac.js?v={}", pac.hash))
}

/// PAC 端点判定：是否请求 `/pac.js`（兼容相对路径与绝对 URI 两种形态）。
pub fn is_pac_request(target: &str) -> bool {
    let without_scheme = match target.find("://") {
        Some(idx) => match target[idx + 3..].find('/') {
            Some(slash) => &target[idx + 3 + slash..],
            None => "/",
        },
        None => target,
    };
    let path_only = without_scheme.split('?').next().unwrap_or("");
    path_only == "/pac.js"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> RuntimeConfig {
        let json = r#"{
            "host": "127.0.0.1",
            "port": 31181,
            "whiteList": { "*.icloud.com": true, "off.example.com": false, "localhost": true },
            "proxy": { "excludeIpList": { "objects-origin.githubusercontent.com": true, "skip.com": null } },
            "preSetIpList": { "*.steamcommunity.com": { "23.200.230.145": true } },
            "intercepts": {
                "*.githubusercontent.com": { ".*": {} },
                "github.com": { ".*": {} },
                "huggingface.co": { ".*": {} }
            },
            "dns": {
                "mapping": { "*linux.do": "my-doh", "off.example": null },
                "ech": { "domains": ["*linux.do", "*discord.com"] },
                "nat64": { "domains": ["*nat64.example"] }
            }
        }"#;
        RuntimeConfig::from_json_str(json).expect("parse")
    }

    #[test]
    fn regex_globs_and_root_domain() {
        assert_eq!(
            to_pac_regex("*.huggingface.co").as_deref(),
            Some("^(.+\\.)?huggingface\\.co$")
        );
        assert_eq!(to_pac_regex("github.com").as_deref(), Some("^github\\.com$"));
        assert_eq!(
            to_pac_regex("^(.+.)?x.com$").as_deref(),
            Some("^(.+.)?x.com$")
        );
        assert_eq!(to_pac_regex("").as_deref(), None);
    }

    #[test]
    fn regex_downgrades_js_incompatible_syntax() {
        assert_eq!(
            to_pac_regex("^(?<name>a)b$").as_deref(),
            Some("^(?:a)b$")
        );
        assert_eq!(to_pac_regex("^a(?<=b)c$").as_deref(), Some("^a(?:b)c$"));
    }

    #[test]
    fn collects_proxy_rules_from_all_sources() {
        let rules = collect_proxy_rules(&config());
        assert!(rules.contains(&"^(.+\\.)?githubusercontent\\.com$".to_string()));
        assert!(rules.contains(&"^(.+\\.)?steamcommunity\\.com$".to_string()));
        assert!(rules.contains(&"^(.+\\.)?linux\\.do$".to_string())); // mapping 与 ech 共用一个正则
        assert!(rules.contains(&"^(.+\\.)?discord\\.com$".to_string()));
        assert!(rules.contains(&"^(.+\\.)?nat64\\.example$".to_string()));
        // 值为 null 的 mapping 不计入
        assert!(!rules.iter().any(|r| r.contains("off\\.example")));
    }

    #[test]
    fn direct_rules_only_include_enabled() {
        let rules = collect_direct_rules(&config());
        assert!(rules.iter().any(|r| r.contains("icloud")));
        assert!(rules.iter().any(|r| r.contains("objects-origin")));
        assert!(!rules.iter().any(|r| r.contains("skip\\.com")));
        assert!(!rules.iter().any(|r| r.contains("off\\.example")));
    }

    #[test]
    fn generated_pac_is_es3_and_has_counts() {
        let cfg = config();
        let pac = generate_pac(&cfg).expect("pac");
        assert_eq!(pac.proxy, "127.0.0.1:31181");
        assert!(pac.proxy_rules >= 6);
        assert!(pac.direct_rules >= ALWAYS_DIRECT.len());
        assert!(pac.content.contains("function FindProxyForURL (url, host)"));
        assert!(pac.content.contains("PROXY 127.0.0.1:31181; DIRECT"));
        assert_eq!(pac.hash.len(), 8);

        // ES3 兼容性
        for forbidden in ["=>", "const ", "let ", "`", "(?<", "class ", "Object.assign"] {
            assert!(
                !pac.content.contains(forbidden),
                "PAC 里不该出现 {forbidden}"
            );
        }
    }

    #[test]
    fn pac_url_carries_hash() {
        let cfg = config();
        let pac = generate_pac(&cfg).expect("pac");
        let url = build_pac_url(&cfg, &pac).expect("url");
        assert_eq!(url, format!("http://127.0.0.1:31181/pac.js?v={}", pac.hash));
    }

    #[test]
    fn detects_pac_requests_in_both_forms() {
        assert!(is_pac_request("/pac.js"));
        assert!(is_pac_request("/pac.js?v=abc"));
        assert!(is_pac_request("http://127.0.0.1:31181/pac.js?v=abc"));
        assert!(!is_pac_request("/api/pac.js"));
        assert!(!is_pac_request("/pac"));
        assert!(!is_pac_request("/"));
    }

    /// 用真实 `running.json`（存在时）对齐 Node 版的统计口径。
    /// 期望值来自 Node 版实测：336 条代理规则 / 110 条直连规则（视配置而定）。
    #[test]
    fn matches_real_config_if_available() {
        let path = std::env::var("DS_RUNNING_JSON").unwrap_or_else(|_| {
            concat!(env!("CARGO_MANIFEST_DIR"), "/../ds-types/tests/fixtures/running.json")
                .to_string()
        });
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!("跳过：没有 {path}");
            return;
        };
        let cfg = RuntimeConfig::from_json_str(&text).expect("parse running.json");
        let pac = generate_pac(&cfg).expect("pac");
        eprintln!(
            "proxy_rules={} direct_rules={} hash={} bytes={}",
            pac.proxy_rules,
            pac.direct_rules,
            pac.hash,
            pac.content.len()
        );
        assert!(pac.proxy_rules > 100, "代理规则太少：{}", pac.proxy_rules);
        assert!(pac.direct_rules >= ALWAYS_DIRECT.len());
        // `linux.do` 这种明确要加速的站点必须在代理规则里
        assert!(pac.content.to_lowercase().contains("linux"));
    }
}
