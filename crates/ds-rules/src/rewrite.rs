//! 内容改写：头替换 / 头删除 / 脚本注入策略。
//!
//! 只做"纯逻辑"部分（不碰网络），便于单测与在 `ds-proxy` 里复用。
//! 值形态约定与 Node 版一致：`"[remove]"` 表示删除该头。

use ds_types::config::{InterceptOptions, ReplaceRules};
use indexmap::IndexMap;

/// 头集合（保持顺序）。
pub type HeaderList = Vec<(String, String)>;

/// 一次改写的结果，便于统计与日志。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RewriteStats {
    pub added: usize,
    pub replaced: usize,
    pub removed: usize,
}

impl RewriteStats {
    pub fn touched(&self) -> bool {
        self.added + self.replaced + self.removed > 0
    }
}

/// 按规则改写头。
pub fn apply_header_rules(rules: &ReplaceRules, headers: &mut HeaderList) -> RewriteStats {
    let mut stats = RewriteStats::default();
    let Some(rules_headers) = rules.headers.as_ref() else {
        return stats;
    };

    for (name, value) in rules_headers {
        let Some(text) = value.as_str() else {
            continue;
        };

        if text.eq_ignore_ascii_case("[remove]") {
            let before = headers.len();
            headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
            stats.removed += before - headers.len();
            continue;
        }

        match headers
            .iter_mut()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
        {
            Some((_, slot)) => {
                *slot = text.to_string();
                stats.replaced += 1;
            }
            None => {
                headers.push((name.clone(), text.to_string()));
                stats.added += 1;
            }
        }
    }

    stats
}

/// 请求侧改写。
pub fn apply_request(options: &InterceptOptions, headers: &mut HeaderList) -> RewriteStats {
    options
        .request_replace
        .as_ref()
        .map(|rules| apply_header_rules(rules, headers))
        .unwrap_or_default()
}

/// 响应侧改写。
pub fn apply_response(options: &InterceptOptions, headers: &mut HeaderList) -> RewriteStats {
    options
        .response_replace
        .as_ref()
        .map(|rules| apply_header_rules(rules, headers))
        .unwrap_or_default()
}

/// 合并后的强制头（`requestReplace.headers`），用于构造上游请求。
pub fn merged_request_headers(options: &InterceptOptions) -> IndexMap<String, String> {
    let mut out = IndexMap::new();
    let Some(rules) = options.request_replace.as_ref() else {
        return out;
    };
    let Some(headers) = rules.headers.as_ref() else {
        return out;
    };
    for (name, value) in headers {
        if let Some(text) = value.as_str() {
            if !text.eq_ignore_ascii_case("[remove]") {
                out.insert(name.clone(), text.to_string());
            }
        }
    }
    out
}

/// 需要删除的请求头。
pub fn removed_request_headers(options: &InterceptOptions) -> Vec<String> {
    options
        .request_replace
        .as_ref()
        .map(|rules| {
            rules
                .removed_headers()
                .into_iter()
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// 是否配置了脚本注入（`script` / `tampermonkeyScript`）。
///
/// 真正的注入在 `ds-proxy` 阶段完成（需要响应体 + CSP nonce），这里只做判定，
/// 便于规则统计与"是否要缓存响应体"的决策。
pub fn wants_script_injection(options: &InterceptOptions) -> bool {
    options.script.is_some() || options.tampermonkey_script.is_some()
}

/// 是否要求改写响应体（`responseReplace.body`）。
pub fn wants_body_rewrite(options: &InterceptOptions) -> bool {
    options
        .response_replace
        .as_ref()
        .and_then(|rules| rules.body.as_ref())
        .is_some()
}

/// 是否需要完整缓存响应体（脚本注入或 body 改写）。
pub fn needs_full_body(options: &InterceptOptions) -> bool {
    wants_script_injection(options) || wants_body_rewrite(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn headers() -> HeaderList {
        vec![
            ("content-type".to_string(), "text/html".to_string()),
            ("alt-svc".to_string(), "h3=\":443\"".to_string()),
            ("server".to_string(), "cloudflare".to_string()),
        ]
    }

    #[test]
    fn removes_header_case_insensitively() {
        let rules = ReplaceRules {
            headers: Some(
                [("Alt-Svc".to_string(), json!("[remove]"))]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        };
        let mut list = headers();
        let stats = apply_header_rules(&rules, &mut list);
        assert_eq!(stats.removed, 1);
        assert!(list.iter().all(|(name, _)| !name.eq_ignore_ascii_case("alt-svc")));
    }

    #[test]
    fn replaces_existing_and_adds_missing() {
        let rules = ReplaceRules {
            headers: Some(
                [
                    ("content-type".to_string(), json!("text/plain; charset=utf-8")),
                    ("x-ds".to_string(), json!("1")),
                ]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        };
        let mut list = headers();
        let stats = apply_header_rules(&rules, &mut list);
        assert_eq!(stats.replaced, 1);
        assert_eq!(stats.added, 1);
        assert!(list
            .iter()
            .any(|(n, v)| n == "content-type" && v == "text/plain; charset=utf-8"));
        assert!(list.iter().any(|(n, v)| n == "x-ds" && v == "1"));
    }

    #[test]
    fn no_rules_means_no_change() {
        let mut list = headers();
        let before = list.clone();
        let stats = apply_header_rules(&ReplaceRules::default(), &mut list);
        assert_eq!(stats, RewriteStats::default());
        assert_eq!(list, before);
    }

    #[test]
    fn request_header_helpers() {
        let options = InterceptOptions {
            request_replace: Some(ReplaceRules {
                headers: Some(
                    [
                        ("accept-language".to_string(), json!("en-US,en;q=0.8")),
                        ("cookie".to_string(), json!("[remove]")),
                    ]
                    .into_iter()
                    .collect(),
                ),
                ..Default::default()
            }),
            ..Default::default()
        };

        let merged = merged_request_headers(&options);
        assert_eq!(merged.get("accept-language").map(String::as_str), Some("en-US,en;q=0.8"));
        assert!(!merged.contains_key("cookie"));
        assert_eq!(removed_request_headers(&options), vec!["cookie".to_string()]);
    }

    #[test]
    fn body_and_script_flags() {
        let scripted = InterceptOptions {
            script: Some(json!("https://example.com/a.js")),
            ..Default::default()
        };
        assert!(wants_script_injection(&scripted));
        assert!(needs_full_body(&scripted));

        let plain = InterceptOptions::default();
        assert!(!wants_script_injection(&plain));
        assert!(!needs_full_body(&plain));
    }
}
