//! 选路：把「候选地址 + 评分 + 拒绝记忆」合成一条有序列表。
//!
//! 调用顺序（与浏览器语义一致）：
//! 1. 先剔除仍在拒绝期内的 IP
//! 2. 若整个域名被路由级拒绝，直接返回空（上层据此回 421/直连）
//! 3. 按评分升序排序
//! 4. 去重、保序

use crate::reject::{RejectCause, Rejections, RouteMode};
use crate::score::Scoreboard;
use std::net::IpAddr;

/// 一次选路的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub ordered: Vec<IpAddr>,
    /// 被路由级拒绝时为 `Some`，此时 `ordered` 必为空。
    pub route_blocked: Option<RejectCause>,
    /// 因 IP 级拒绝被剔除的数量。
    pub filtered: usize,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.ordered.is_empty()
    }
}

/// 组合评分与拒绝记忆，产出有序候选。
pub fn select(
    hostname: &str,
    mode: RouteMode,
    candidates: &[IpAddr],
    scoreboard: &Scoreboard,
    rejections: &Rejections,
) -> Selection {
    if let Some(cause) = rejections.route_rejected(hostname, mode) {
        return Selection {
            ordered: Vec::new(),
            route_blocked: Some(cause),
            filtered: candidates.len(),
        };
    }

    let mut seen = std::collections::HashSet::new();
    let mut accepted: Vec<IpAddr> = Vec::with_capacity(candidates.len());
    for ip in candidates {
        if !seen.insert(*ip) {
            continue;
        }
        if rejections.ip_rejected(&ip.to_string()) {
            continue;
        }
        accepted.push(*ip);
    }

    let filtered = candidates.len().saturating_sub(accepted.len());
    let ordered = scoreboard.sort_candidates(&accepted);

    Selection {
        ordered,
        route_blocked: None,
        filtered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(23, 200, 230, last))
    }

    #[test]
    fn orders_by_score_and_dedupes() {
        let mut board = Scoreboard::new();
        board.record_success(ip(2), 80.0);
        board.record_success(ip(1), 20.0);

        let selection = select(
            "steamcommunity.com",
            RouteMode::Default,
            &[ip(2), ip(1), ip(2)],
            &board,
            &Rejections::new(),
        );
        assert_eq!(selection.ordered, vec![ip(1), ip(2)]);
        assert_eq!(selection.filtered, 0);
        assert!(selection.route_blocked.is_none());
    }

    #[test]
    fn drops_rejected_ips() {
        let board = Scoreboard::new();
        let mut rejections = Rejections::new();
        rejections.reject_ip(&ip(1).to_string(), RejectCause::Forbidden);

        let selection = select(
            "steamcommunity.com",
            RouteMode::Default,
            &[ip(1), ip(2)],
            &board,
            &rejections,
        );
        assert_eq!(selection.ordered, vec![ip(2)]);
        assert_eq!(selection.filtered, 1);
    }

    #[test]
    fn blocked_route_returns_empty() {
        let board = Scoreboard::new();
        let mut rejections = Rejections::new();
        rejections.reject_route("github.com", RouteMode::Default, RejectCause::Misdirected);

        let selection = select(
            "github.com",
            RouteMode::Default,
            &[ip(1), ip(2)],
            &board,
            &rejections,
        );
        assert!(selection.is_empty());
        assert_eq!(selection.route_blocked, Some(RejectCause::Misdirected));
    }
}
