//! 规则表：把 `running.json` 的 `intercepts` 编译成可按 host/path 命中的结构。
//!
//! 与 Node 版的语义对齐点：
//! - 域名模式用 `ds_types::matcher`（`^` 开头当正则，含 `*`/`?` 当 glob，否则精确）
//! - 多个域名模式同时命中时**合并**（后命中者覆盖非 `None` 字段），不是"第一个赢"
//! - 路径正则对 URI 的 path 部分匹配
//! - 值形态宽松：同一个路径下允许 `null`

use ds_types::config::{InterceptOptions, ReplaceRules, RuntimeConfig};
use ds_types::matcher;
use indexmap::IndexMap;
use regex::Regex;

/// 编译后的单条路径规则。
#[derive(Debug)]
struct CompiledPath {
    regex: Regex,
    options: InterceptOptions,
}

/// 编译后的单条域名规则。
#[derive(Debug)]
struct CompiledHost {
    pattern: String,
    paths: Vec<CompiledPath>,
}

/// 编译后的规则表。
#[derive(Debug, Default)]
pub struct Rules {
    hosts: Vec<CompiledHost>,
    white_list: Vec<String>,
    exclude: Vec<String>,
}

impl Rules {
    /// 从运行期配置编译。无法编译的正则会被跳过并告警，不会让整个代理失败。
    pub fn from_config(config: &RuntimeConfig) -> Self {
        let mut hosts = Vec::with_capacity(config.intercepts.len());

        for (pattern, table) in &config.intercepts {
            let mut paths = Vec::with_capacity(table.len());
            for (path_pattern, options) in table {
                let Some(options) = options.as_ref() else {
                    continue;
                };
                match Regex::new(path_pattern) {
                    Ok(regex) => paths.push(CompiledPath {
                        regex,
                        options: options.clone(),
                    }),
                    Err(err) => {
                        tracing::warn!(
                            pattern = %pattern,
                            path = %path_pattern,
                            error = %err,
                            "路径正则无法编译，已跳过"
                        );
                    }
                }
            }
            hosts.push(CompiledHost {
                pattern: pattern.clone(),
                paths,
            });
        }

        let white_list = config
            .white_list
            .iter()
            .filter(|(_, enabled)| enabled.get())
            .map(|(pattern, _)| pattern.clone())
            .collect();

        let exclude = config
            .proxy
            .active_exclude_domains()
            .into_iter()
            .map(|s| s.to_string())
            .collect();

        Self {
            hosts,
            white_list,
            exclude,
        }
    }

    /// 域名是否命中任意 intercepts 模式。
    pub fn matches_host(&self, host: &str) -> bool {
        self.hosts
            .iter()
            .any(|entry| matcher::glob_match(&entry.pattern, host))
    }

    /// 取该域名合并后的选项（不含路径条件）；无命中返回 `None`。
    pub fn options_for_host(&self, host: &str) -> Option<InterceptOptions> {
        let mut merged: Option<InterceptOptions> = None;
        for entry in &self.hosts {
            if !matcher::glob_match(&entry.pattern, host) {
                continue;
            }
            for path in &entry.paths {
                merge_into(merged.get_or_insert_with(InterceptOptions::default), &path.options);
            }
        }
        merged
    }

    /// 同时按域名与路径取合并选项。
    pub fn options_for(&self, host: &str, path: &str) -> Option<InterceptOptions> {
        let mut merged: Option<InterceptOptions> = None;
        for entry in &self.hosts {
            if !matcher::glob_match(&entry.pattern, host) {
                continue;
            }
            for rule in &entry.paths {
                if !rule.regex.is_match(path) {
                    continue;
                }
                merge_into(merged.get_or_insert_with(InterceptOptions::default), &rule.options);
            }
        }
        merged
    }

    /// 是否命中白名单（白名单优先于 intercepts）。
    pub fn is_white_listed(&self, host: &str) -> bool {
        self.white_list
            .iter()
            .any(|pattern| matcher::glob_match(pattern, host))
    }

    /// 是否被 `excludeIpList` 显式排除。
    pub fn is_excluded(&self, host: &str) -> bool {
        self.exclude
            .iter()
            .any(|pattern| matcher::glob_match(pattern, host))
    }

    /// 是否需要 MITM。白名单与排除名单都返回 `false`。
    pub fn should_intercept(&self, host: &str) -> bool {
        if self.is_white_listed(host) || self.is_excluded(host) {
            return false;
        }
        self.matches_host(host)
    }

    pub fn host_rule_count(&self) -> usize {
        self.hosts.len()
    }

    pub fn path_rule_count(&self) -> usize {
        self.hosts.iter().map(|h| h.paths.len()).sum()
    }

    pub fn white_list(&self) -> &[String] {
        &self.white_list
    }

    pub fn exclude_list(&self) -> &[String] {
        &self.exclude
    }
}

/// 合并策略：`src` 的非 `None` 字段覆盖 `dst`。
fn merge_into(dst: &mut InterceptOptions, src: &InterceptOptions) {
    macro_rules! take {
        ($($field:ident),* $(,)?) => {
            $(
                if src.$field.is_some() {
                    dst.$field = src.$field.clone();
                }
            )*
        };
    }

    take!(
        sni,
        verify_host,
        cache_days,
        desc,
        status,
        proxy,
        response_replace,
        request_replace,
        script,
        tampermonkey_script,
        success,
        abort,
        backup,
        redirect,
        options,
    );

    for (key, value) in &src.extra {
        dst.extra.insert(key.clone(), value.clone());
    }
}

/// 合并两组替换规则（`headers` 按名覆盖，其余字段非 `None` 覆盖）。
pub fn merge_replace_rules(dst: &mut ReplaceRules, src: &ReplaceRules) {
    if let Some(headers) = src.headers.as_ref() {
        let target = dst.headers.get_or_insert_with(IndexMap::new);
        for (name, value) in headers {
            target.insert(name.clone(), value.clone());
        }
    }
    if src.body.is_some() {
        dst.body = src.body.clone();
    }
    if src.do_download.is_some() {
        dst.do_download = src.do_download;
    }
    if src.desc.is_some() {
        dst.desc = src.desc.clone();
    }
    for (key, value) in &src.extra {
        dst.extra.insert(key.clone(), value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> RuntimeConfig {
        let json = r#"{
            "whiteList": { "localhost": true, "*.icloud.com": true, "off.example.com": false },
            "proxy": { "excludeIpList": { "objects-origin.githubusercontent.com": true, "nope.com": null } },
            "intercepts": {
                "*.huggingface.co": { ".*": { "sni": "huggingface.cn", "cacheDays": 1 } },
                "*.wnacg.com": { ".*": { "responseReplace": { "headers": { "alt-svc": "[remove]" } } } },
                "github.com": {
                    "^(/[^/]+){2,}/?$": { "sni": "baidu.com" },
                    ".*": { "cacheDays": 7 }
                },
                "broken.example.com": { "([": { "sni": "x" } }
            }
        }"#;
        RuntimeConfig::from_json_str(json).expect("parse")
    }

    #[test]
    fn white_list_wins_over_intercepts() {
        let rules = Rules::from_config(&config());
        assert!(rules.is_white_listed("localhost"));
        assert!(!rules.should_intercept("localhost"));
        assert!(rules.is_white_listed("a.icloud.com"));
        assert!(!rules.is_white_listed("off.example.com"));
    }

    #[test]
    fn excluded_domain_never_intercepts() {
        let rules = Rules::from_config(&config());
        assert!(rules.is_excluded("objects-origin.githubusercontent.com"));
        assert!(!rules.should_intercept("objects-origin.githubusercontent.com"));
        assert!(!rules.is_excluded("nope.com"));
    }

    #[test]
    fn glob_covers_subdomains_only() {
        // 与 Node 版 `matchHostname` 保持一致：`*.x.com` 只匹配子域，根域需要单独写一条 `x.com`。
        // PAC 层另有处理（通配符同时覆盖根域，见 ds-pac 的 to_pac_regex），两处分工不同。
        let rules = Rules::from_config(&config());
        assert!(rules.should_intercept("cdn.huggingface.co"));
        assert!(!rules.should_intercept("huggingface.co"));
    }

    #[test]
    fn merge_is_not_first_match_wins() {
        let rules = Rules::from_config(&config());
        let opts = rules.options_for("github.com", "/a/b/").expect("merged");
        assert_eq!(opts.sni.as_deref(), Some("baidu.com"));
        assert_eq!(opts.cache_days, Some(7));
    }

    #[test]
    fn path_matching_is_applied() {
        let rules = Rules::from_config(&config());
        let deep = rules.options_for("github.com", "/a/b/").expect("deep path");
        assert_eq!(deep.sni.as_deref(), Some("baidu.com"));

        let root = rules.options_for("github.com", "/").expect("root path");
        assert_eq!(root.sni, None);
        assert_eq!(root.cache_days, Some(7));
    }

    #[test]
    fn alt_svc_removal_is_detected() {
        let rules = Rules::from_config(&config());
        let opts = rules.options_for("www.wnacg.com", "/").expect("opts");
        assert!(opts.removes_alt_svc());
        assert_eq!(
            opts.response_replace
                .as_ref()
                .map(|r| r.removed_headers())
                .unwrap_or_default(),
            vec!["alt-svc"]
        );
    }

    #[test]
    fn broken_regex_is_skipped_not_fatal() {
        let rules = Rules::from_config(&config());
        assert!(rules.matches_host("broken.example.com"));
        assert!(rules.options_for("broken.example.com", "/").is_none());
    }

    #[test]
    fn merge_replace_rules_overrides_headers() {
        let mut dst = ReplaceRules::default();
        let mut src = ReplaceRules::default();
        let mut headers = IndexMap::new();
        headers.insert("alt-svc".to_string(), serde_json::json!("[remove]"));
        headers.insert("x-test".to_string(), serde_json::json!("1"));
        src.headers = Some(headers);
        merge_replace_rules(&mut dst, &src);
        let removed = dst.removed_headers();
        assert_eq!(removed, vec!["alt-svc"]);
    }
}
