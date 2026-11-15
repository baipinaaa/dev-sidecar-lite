//! 把 PAC 判定结果导出成 JSON，供 `tests/e2e/consistency.py` 与 Node 版逐条对比。
//!
//! 用法：
//!   ds-pac-dump --config running.json --hosts github.com,linux.do,www.baidu.com
//!   ds-pac-dump --config running.json --hosts-file hosts.txt
//!
//! 输出：
//! ```json
//! { "proxy": "127.0.0.1:31181", "hash": "...", "proxyRules": [...], "directRules": [...],
//!   "decisions": { "github.com": "PROXY", "www.baidu.com": "DIRECT" } }
//! ```

use std::process::ExitCode;

use ds_pac::{generate_pac, pac::collect_direct_rules, pac::collect_proxy_rules, PacInfo, ALWAYS_DIRECT};
use ds_types::config::RuntimeConfig;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("ds-pac-dump: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut config_path = None;
    let mut hosts: Vec<String> = Vec::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-c" | "--config" => config_path = args.next(),
            "--hosts" => {
                if let Some(list) = args.next() {
                    hosts.extend(list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
                }
            }
            "--hosts-file" => {
                if let Some(path) = args.next() {
                    let text = std::fs::read_to_string(&path)
                        .map_err(|err| format!("读取 {path} 失败：{err}"))?;
                    hosts.extend(
                        text.lines()
                            .map(|line| line.trim().to_string())
                            .filter(|line| !line.is_empty() && !line.starts_with('#')),
                    );
                }
            }
            "-h" | "--help" => {
                println!("{}", env!("CARGO_PKG_DESCRIPTION"));
                return Ok(());
            }
            other => return Err(format!("未知参数：{other}")),
        }
    }

    let path = config_path.ok_or("缺少 --config <running.json>")?;
    let text = std::fs::read_to_string(&path).map_err(|err| format!("读取 {path} 失败：{err}"))?;
    let config = RuntimeConfig::from_json_str(&text).map_err(|err| format!("解析配置失败：{err}"))?;

    let pac = generate_pac(&config).ok_or("配置里没有有效的 host/port，无法生成 PAC")?;

    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"proxy\": {},\n", json_str(&pac.proxy)));
    out.push_str(&format!("  \"hash\": {},\n", json_str(&pac.hash)));
    out.push_str(&format!("  \"proxyRuleCount\": {},\n", pac.proxy_rules));
    out.push_str(&format!("  \"directRuleCount\": {},\n", pac.direct_rules));
    out.push_str("  \"proxyRules\": ");
    out.push_str(&json_array(&collect_proxy_rules(&config)));
    out.push_str(",\n  \"directRules\": ");
    out.push_str(&json_array(&collect_direct_rules(&config)));
    out.push_str(",\n  \"alwaysDirect\": ");
    out.push_str(&json_array(
        &ALWAYS_DIRECT.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    ));
    out.push_str(",\n  \"decisions\": {");

    let mut first = true;
    for host in &hosts {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&format!(
            "\n    {}: {}",
            json_str(host),
            json_str(decide(&pac, host))
        ));
    }
    out.push_str("\n  }\n}\n");

    println!("{out}");
    Ok(())
}

/// 与 PAC 里的 `FindProxyForURL` 语义一致：DIRECT 优先，其次 PROXY，默认 DIRECT。
fn decide(pac: &PacInfo, host: &str) -> &'static str {
    if matches_any(pac, host, true) {
        return "DIRECT";
    }
    if matches_any(pac, host, false) {
        return "PROXY";
    }
    "DIRECT"
}

fn matches_any(pac: &PacInfo, host: &str, direct: bool) -> bool {
    let source = if direct {
        extract_array(&pac.content, "DIRECT_RAW")
    } else {
        extract_array(&pac.content, "PROXY_RAW")
    };
    source.iter().any(|pattern| {
        regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .map(|re| re.is_match(host))
            .unwrap_or(false)
    })
}

/// 从生成的 PAC 文本里抠出 `var XXX_RAW = [...]` 的字符串数组。
fn extract_array(content: &str, name: &str) -> Vec<String> {
    let marker = format!("var {name} = ");
    let Some(start) = content.find(&marker) else {
        return Vec::new();
    };
    let rest = &content[start + marker.len()..];
    let Some(open) = rest.find('[') else {
        return Vec::new();
    };
    let Some(close) = rest[open..].find(']') else {
        return Vec::new();
    };
    let body = &rest[open + 1..open + close];

    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            let mut value = String::new();
            i += 1;
            while i < bytes.len() && bytes[i] != b'\'' {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    value.push(bytes[i + 1] as char);
                    i += 2;
                    continue;
                }
                value.push(bytes[i] as char);
                i += 1;
            }
            out.push(value);
        }
        i += 1;
    }
    out
}

fn json_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_array(items: &[String]) -> String {
    if items.is_empty() {
        return "[]".to_string();
    }
    let body = items
        .iter()
        .map(|item| json_str(item))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{body}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_quoted_array() {
        let content = "var A_RAW = [\n    '^a\\\\.b$',\n    '^c$'\n  ]\n";
        assert_eq!(extract_array(content, "A_RAW"), vec!["^a\\.b$", "^c$"]);
    }

    #[test]
    fn missing_array_is_empty() {
        assert!(extract_array("var X = 1", "A_RAW").is_empty());
    }

    #[test]
    fn json_escapes_quotes() {
        assert_eq!(json_str("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }
}
