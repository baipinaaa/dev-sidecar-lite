//! 真实配置驱动测试。
//!
//! `tests/fixtures/running.json` 是用户本机的 `~/.dev-sidecar/running.json` 副本，
//! 已被 `.gitignore` 忽略（含私有域名），因此：
//! - 本地有文件 → 跑完整数据驱动断言
//! - CI 无文件 → 测试直接跳过（打印提示）

use std::path::PathBuf;

use ds_types::config::RuntimeConfig;
use ds_types::matcher;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/running.json")
}

fn load_real() -> Option<RuntimeConfig> {
    let path = fixture_path();
    if !path.exists() {
        eprintln!("跳过：{} 不存在（CI 环境属正常）", path.display());
        return None;
    }
    let text = std::fs::read_to_string(&path).expect("读取 running.json");
    Some(RuntimeConfig::from_json_str(&text).expect("解析 running.json"))
}

#[test]
fn parses_real_running_json() {
    let Some(cfg) = load_real() else {
        return;
    };

    assert_eq!(cfg.port, 31181, "端口应与 Node 版一致");
    assert_eq!(cfg.listen_addr(), "127.0.0.1:31181");

    // 规则规模：Node 版实测 137 个 intercepts 域名
    assert!(
        cfg.intercepts.len() > 100,
        "intercepts 域名数异常：{}",
        cfg.intercepts.len()
    );
    assert!(
        cfg.dns.mapping.len() > 50,
        "dns.mapping 条目数异常：{}",
        cfg.dns.mapping.len()
    );
    assert!(
        cfg.pre_set_ip_list.len() > 50,
        "preSetIpList 条目数异常：{}",
        cfg.pre_set_ip_list.len()
    );
    assert!(
        cfg.dns.providers.len() >= 8,
        "dns.providers 数量异常：{}",
        cfg.dns.providers.len()
    );

    // ECH 配置
    assert!(cfg.dns.ech.is_active(), "ECH 应为启用状态");
    assert!(cfg.dns.ech.use_ech);
    assert!(!cfg.dns.ech.try_all_providers, "默认应关闭跨 provider 竞速");
    assert!(
        cfg.ech_domains().len() > 20,
        "ech.domains 数量异常：{}",
        cfg.ech_domains().len()
    );

    // PAC 开关（本定制版新增字段）
    assert!(cfg.proxy.pac_enabled, "PAC 应为开启状态");
    assert!(!cfg.proxy.enabled, "固定代理应为关闭状态（PAC 互斥）");
    assert!(
        !cfg.proxy.active_exclude_domains().is_empty(),
        "excludeIpList 应至少有一条 true"
    );

    // 动态上游（本定制版新增）
    assert!(
        !cfg.dynamic_upstreams.is_empty(),
        "dynamicUpstreams 应存在"
    );
}

#[test]
fn matches_known_domains_from_real_config() {
    let Some(cfg) = load_real() else {
        return;
    };

    // ECH 域名：`*linux.do` 应覆盖根域
    assert!(
        matcher::any_match(cfg.ech_domains(), "linux.do"),
        "*linux.do 应匹配 linux.do"
    );
    assert!(
        matcher::any_match(cfg.ech_domains(), "cdn.linux.do"),
        "*linux.do 应匹配子域"
    );

    // intercepts 键能命中已知站点
    let patterns: Vec<&str> = cfg.intercepts.keys().map(|s| s.as_str()).collect();
    assert!(matcher::any_match(patterns.iter().copied(), "github.com"));
    assert!(matcher::any_match(
        patterns.iter().copied(),
        "api.github.com"
    ));
    assert!(matcher::any_match(
        patterns.iter().copied(),
        "raw.githubusercontent.com"
    ));

    // 该站点不应被任何 intercepts 规则命中（典型直连站点）
    assert!(!matcher::any_match(
        patterns.iter().copied(),
        "www.baidu.com"
    ));
}

#[test]
fn alt_svc_removal_rules_are_visible() {
    let Some(cfg) = load_real() else {
        return;
    };

    let mut total = 0usize;
    let mut alt_svc = 0usize;
    let mut cache_days = 0usize;
    let mut sni = 0usize;

    for table in cfg.intercepts.values() {
        for opts in table.values().flatten() {
            total += 1;
            if opts.removes_alt_svc() {
                alt_svc += 1;
            }
            if opts.cache_days.is_some() {
                cache_days += 1;
            }
            if opts.sni.is_some() {
                sni += 1;
            }
        }
    }

    eprintln!(
        "intercept 选项统计：total={total} sni={sni} altSvc={alt_svc} cacheDays={cache_days}"
    );
    assert!(total > 100, "intercept 选项总数异常：{total}");
}

#[test]
fn preset_ips_are_filtered_by_enabled_flag() {
    let Some(cfg) = load_real() else {
        return;
    };

    // 找一个确实配置了多个 IP 的域名
    let candidate = cfg
        .pre_set_ip_list
        .iter()
        .find(|(_, ips)| ips.len() > 1)
        .map(|(k, _)| k.clone());

    let Some(pattern) = candidate else {
        return;
    };

    // 把 pattern 里的通配符换成具体域名再查询
    let hostname = pattern
        .trim_start_matches("^*")
        .trim_start_matches('*')
        .trim_start_matches('.')
        .to_string();
    if hostname.is_empty() || hostname.contains('^') || hostname.contains('*') {
        return;
    }

    let enabled = cfg.active_preset_ips(&hostname);
    // 过滤后数量不应超过原始条目数
    let all = &cfg.pre_set_ip_list[&pattern];
    assert!(enabled.len() <= all.len());
    eprintln!("{hostname}: 启用 {} / 共 {}", enabled.len(), all.len());
}
