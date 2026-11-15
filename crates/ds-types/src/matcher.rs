//! 域名规则匹配，语义对齐 JS 侧 `utils/util.match.js`。
//!
//! 规则形式：
//! - `^` 开头：按正则处理（与 JS 一致，不做转义）
//! - 含 `*` / `?`：通配转正则，`*` → `.*`、`?` → `.`
//! - 其它：精确匹配（忽略大小写）
//!
//! 注意 `*.example.com` 展开为 `^.*\.example\.com$`，**不**匹配根域
//! `example.com`（与 JS 的 `domainMapRegexply` 行为一致）。若需要覆盖根域，
//! 由 PAC 生成侧单独处理。

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use regex::Regex;

/// 规则编译缓存的容量上限，超过后整体清空（配置热重载时数量有限）。
const CACHE_LIMIT: usize = 4096;

type Cache = RwLock<HashMap<String, Option<Regex>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 把规则编译成正则；`None` 表示该规则应走精确比较。
pub fn compile(pattern: &str) -> Option<Regex> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return None;
    }

    if let Ok(guard) = cache().read() {
        if let Some(hit) = guard.get(pattern) {
            return hit.clone();
        }
    }

    let compiled = build(pattern);

    if let Ok(mut guard) = cache().write() {
        if guard.len() >= CACHE_LIMIT {
            guard.clear();
        }
        guard.insert(pattern.to_string(), compiled.clone());
    }

    compiled
}

fn build(pattern: &str) -> Option<Regex> {
    if pattern.starts_with('^') {
        return Regex::new(pattern).ok();
    }
    if !pattern.contains('*') && !pattern.contains('?') {
        return None;
    }

    let mut re = String::with_capacity(pattern.len() * 2 + 8);
    re.push_str("(?i)^");
    for ch in pattern.chars() {
        match ch {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            other => re.push_str(&regex::escape(&other.to_string())),
        }
    }
    re.push('$');
    Regex::new(&re).ok()
}

/// 单条规则匹配。
pub fn glob_match(pattern: &str, hostname: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() || hostname.is_empty() {
        return false;
    }
    match compile(pattern) {
        Some(re) => re.is_match(hostname),
        None => pattern.eq_ignore_ascii_case(hostname),
    }
}

/// 任意一条规则命中即为真。
pub fn any_match<'a, I>(patterns: I, hostname: &str) -> bool
where
    I: IntoIterator<Item = &'a str>,
{
    patterns.into_iter().any(|p| glob_match(p, hostname))
}

/// 返回第一条命中的规则。
pub fn first_match<'a, I>(patterns: I, hostname: &str) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    patterns.into_iter().find(|p| glob_match(p, hostname))
}

/// 规则是否为纯正则（供 PAC 生成判断是否需要转义）。
pub fn is_regex_pattern(pattern: &str) -> bool {
    pattern.trim().starts_with('^')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_ignores_case() {
        assert!(glob_match("github.com", "GitHub.com"));
        assert!(!glob_match("github.com", "api.github.com"));
    }

    #[test]
    fn wildcard_does_not_cover_root_domain() {
        // 与 JS 侧 domainMapRegexply 一致：`*.x` 只匹配子域
        assert!(glob_match("*.huggingface.co", "www.huggingface.co"));
        assert!(glob_match("*.huggingface.co", "a.b.huggingface.co"));
        assert!(!glob_match("*.huggingface.co", "huggingface.co"));
    }

    #[test]
    fn prefix_wildcard() {
        assert!(glob_match("stein*", "steingate.com"));
        assert!(!glob_match("stein*", "xsteingate.com"));
    }

    #[test]
    fn inline_wildcard() {
        assert!(glob_match("*.steamstatic.com", "cdn.akamai.steamstatic.com"));
        assert!(glob_match("cdn-*.example.com", "cdn-1.example.com"));
    }

    #[test]
    fn regex_pattern_passes_through() {
        assert!(glob_match("^(.+.)?x.com$", "x.com"));
        assert!(glob_match("^(.+.)?x.com$", "api.x.com"));
        // 未转义的 `.` 在正则里是任意字符，与 JS 行为一致
        assert!(glob_match("^(.+.)?x.com$", "xacom"));
        assert!(!glob_match("^(.+.)?x.com$", "y.com"));
    }

    #[test]
    fn question_mark_matches_single_char() {
        assert!(glob_match("a?.com", "ab.com"));
        assert!(!glob_match("a?.com", "abc.com"));
    }

    #[test]
    fn empty_pattern_never_matches() {
        assert!(!glob_match("", "a.com"));
        assert!(!glob_match("a.com", ""));
    }

    #[test]
    fn any_and_first_match() {
        let list = ["*.githubusercontent.com", "github.com"];
        assert!(any_match(list, "raw.githubusercontent.com"));
        assert_eq!(first_match(list, "github.com"), Some("github.com"));
        assert_eq!(first_match(list, "example.com"), None);
    }

    #[test]
    fn cache_survives_repeated_calls() {
        for _ in 0..5 {
            assert!(glob_match("*.example.com", "a.example.com"));
        }
    }
}
