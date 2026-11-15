//! ds-dns —— DNS 解析与 ECH 取参。
//!
//! - `wire`：报文构造/解析（SVCB、ECH）
//! - `client`：DoH / DoT / UDP 传输
//!
//! - `score`：IP 评分（EWMA 延迟 × 失败惩罚）
//! - `failure`：失败分类（只有 TCP 级错误才归因到 IP）
//! - `reject`：路由级 / IP 级 / 名字级拒绝记忆
//! - `select`：把候选 + 评分 + 拒绝记忆合成有序列表

#![forbid(unsafe_code)]

pub mod client;
pub mod failure;
pub mod reject;
pub mod score;
pub mod select;
pub mod wire;

pub use client::{parse_server, query, query_target, Scheme, Target, QUERY_TIMEOUT};
pub use wire::{
    build_query, find_ech, parse_response, parse_svcb, Answer, Svcb, TYPE_A, TYPE_AAAA, TYPE_HTTPS,
};
pub use failure::{classify, FailureReason};
pub use reject::{RejectCause, Rejections, RouteMode};
pub use score::{IpScore, Scoreboard, UNSEEN_RANK};
pub use select::{select, Selection};

use anyhow::Result;

/// 解析 A 记录；没有则退化为 AAAA。
pub async fn resolve_ips(server: &str, host: &str) -> Result<Vec<String>> {
    let answers = query(server, host, TYPE_A).await?;
    let mut ips = dedupe(wire::collect_ips(&answers));

    if ips.is_empty() {
        let answers = query(server, host, TYPE_AAAA).await?;
        ips = dedupe(wire::collect_ips(&answers));
    }

    Ok(ips)
}

/// 只解析 IPv4。
pub async fn resolve_ipv4(server: &str, host: &str) -> Result<Vec<String>> {
    let answers = query(server, host, TYPE_A).await?;
    Ok(dedupe(
        answers
            .iter()
            .filter_map(|answer| answer.as_ipv4().map(|ip| ip.to_string()))
            .collect(),
    ))
}

/// 取 ECHConfigList。
pub async fn resolve_ech(server: &str, host: &str) -> Result<Option<Vec<u8>>> {
    let answers = query(server, host, TYPE_HTTPS).await?;
    Ok(wire::find_ech(&answers))
}

/// 一次拿全：A 记录 + ECH（同源，避免跨 provider 拼装）。
pub async fn resolve_a_and_ech(server: &str, host: &str) -> Result<(Vec<String>, Option<Vec<u8>>)> {
    let a = query(server, host, TYPE_A).await;
    let https = query(server, host, TYPE_HTTPS).await;

    let ips = match a {
        Ok(answers) => dedupe(wire::collect_ips(&answers)),
        Err(err) => {
            tracing::debug!("{host} 的 A 记录查询失败：{err:#}");
            Vec::new()
        }
    };

    let ech = match https {
        Ok(answers) => wire::find_ech(&answers),
        Err(err) => {
            tracing::debug!("{host} 的 HTTPS 记录查询失败：{err:#}");
            None
        }
    };

    Ok((ips, ech))
}

fn dedupe(list: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    list.into_iter()
        .filter(|item| seen.insert(item.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_keeps_first_order() {
        let list = vec!["1.1.1.1".to_string(), "2.2.2.2".to_string(), "1.1.1.1".to_string()];
        assert_eq!(dedupe(list), vec!["1.1.1.1".to_string(), "2.2.2.2".to_string()]);
    }

    #[test]
    fn exports_are_consistent() {
        // 保证对外导出的常量与报文模块一致
        assert_eq!(TYPE_HTTPS, 65);
        assert_eq!(TYPE_A, 1);
        assert_eq!(TYPE_AAAA, 28);
    }
}
