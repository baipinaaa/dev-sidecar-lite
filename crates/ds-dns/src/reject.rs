//! 拒绝记忆：路由级 / IP 级 / 名字级（借鉴 hakimi `decision/engine.rs`）。
//!
//! 三种 TTL 的区别来自实测经验：
//! - **路由级 300s**：421 Misdirected Request 说明"这个域名不该从这个路由走"，重试同域名的其它 IP 也没用
//! - **IP 级 300s**：滥用页 / 封禁页只针对某台边缘，换 IP 还能通
//! - **名字级 600s**：SNI 拒绝时，同一 CDN 的每个 IP 都会发同一张外国证书，重拨只是白烧握手

use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const ROUTE_REJECTION_TTL: Duration = Duration::from_secs(300);
pub const IP_REJECTION_TTL: Duration = Duration::from_secs(300);
pub const SNI_REJECTION_TTL: Duration = Duration::from_secs(600);

/// 路由模式：决定 403/451 是否算"整条路由不可用"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteMode {
    /// 默认：只有 421 才拒绝整条路由
    Default,
    /// 严格：403/451 也拒绝（用于已知会封禁的站点，需显式 opt-in）
    BlockOn403,
}

impl RouteMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RouteMode::Default => "default",
            RouteMode::BlockOn403 => "block_on_403",
        }
    }
}

/// 某条路由被拒的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectCause {
    /// 421 Misdirected Request
    Misdirected,
    /// 403 / 451（仅 opt-in 模式）
    Forbidden,
}

#[derive(Debug, Clone, Copy)]
struct Rejection {
    until: Instant,
    cause: RejectCause,
}

/// 拒绝记忆表（路由 / IP / 名字三张表）。
#[derive(Debug, Default)]
pub struct Rejections {
    routes: HashMap<String, Rejection>,
    ips: HashMap<String, Rejection>,
    names: HashMap<String, Rejection>,
}

impl Rejections {
    pub fn new() -> Self {
        Self::default()
    }

    fn route_key(domain: &str, mode: RouteMode) -> String {
        format!("{}|{}", domain.to_ascii_lowercase(), mode.as_str())
    }

    /// 记录一条路由拒绝。
    pub fn reject_route(&mut self, domain: &str, mode: RouteMode, cause: RejectCause) {
        self.routes.insert(
            Self::route_key(domain, mode),
            Rejection {
                until: Instant::now() + ROUTE_REJECTION_TTL,
                cause,
            },
        );
    }

    /// 该路由是否仍在拒绝期内。
    pub fn route_rejected(&self, domain: &str, mode: RouteMode) -> Option<RejectCause> {
        let key = Self::route_key(domain, mode);
        match self.routes.get(&key) {
            Some(entry) if entry.until > Instant::now() => Some(entry.cause),
            _ => None,
        }
    }

    /// 根据状态码决定是否记录路由拒绝。
    pub fn note_status(&mut self, domain: &str, mode: RouteMode, status: u16) -> Option<RejectCause> {
        let cause = match status {
            421 => Some(RejectCause::Misdirected),
            403 | 451 if mode == RouteMode::BlockOn403 => Some(RejectCause::Forbidden),
            _ => None,
        };
        if let Some(cause) = cause {
            self.reject_route(domain, mode, cause);
        }
        cause
    }

    pub fn reject_ip(&mut self, ip: &str, cause: RejectCause) {
        self.ips.insert(
            ip.to_string(),
            Rejection {
                until: Instant::now() + IP_REJECTION_TTL,
                cause,
            },
        );
    }

    pub fn ip_rejected(&self, ip: &str) -> bool {
        self.ips
            .get(ip)
            .map(|entry| entry.until > Instant::now())
            .unwrap_or(false)
    }

    pub fn reject_name(&mut self, hostname: &str, cause: RejectCause) {
        self.names.insert(
            hostname.to_ascii_lowercase(),
            Rejection {
                until: Instant::now() + SNI_REJECTION_TTL,
                cause,
            },
        );
    }

    pub fn name_rejected(&self, hostname: &str) -> bool {
        self.names
            .get(&hostname.to_ascii_lowercase())
            .map(|entry| entry.until > Instant::now())
            .unwrap_or(false)
    }

    /// 过滤候选地址：去掉仍在 IP 拒绝期内的。
    pub fn filter_ips(&self, candidates: &[String]) -> Vec<String> {
        candidates
            .iter()
            .filter(|ip| !self.ip_rejected(ip))
            .cloned()
            .collect()
    }

    /// 清理过期记录。
    pub fn prune(&mut self) {
        let now = Instant::now();
        self.routes.retain(|_, entry| entry.until > now);
        self.ips.retain(|_, entry| entry.until > now);
        self.names.retain(|_, entry| entry.until > now);
    }

    pub fn len(&self) -> usize {
        self.routes.len() + self.ips.len() + self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_421_rejects_route_by_default() {
        let mut rejections = Rejections::new();
        assert!(rejections
            .note_status("github.com", RouteMode::Default, 403)
            .is_none());
        assert!(rejections.route_rejected("github.com", RouteMode::Default).is_none());

        assert_eq!(
            rejections.note_status("github.com", RouteMode::Default, 421),
            Some(RejectCause::Misdirected)
        );
        assert_eq!(
            rejections.route_rejected("github.com", RouteMode::Default),
            Some(RejectCause::Misdirected)
        );
    }

    #[test]
    fn opt_in_mode_rejects_on_403() {
        let mut rejections = Rejections::new();
        assert_eq!(
            rejections.note_status("linux.do", RouteMode::BlockOn403, 403),
            Some(RejectCause::Forbidden)
        );
        assert!(rejections.route_rejected("linux.do", RouteMode::BlockOn403).is_some());
        // 另一模式不受影响
        assert!(rejections.route_rejected("linux.do", RouteMode::Default).is_none());
    }

    #[test]
    fn ip_and_name_rejections_filter_candidates() {
        let mut rejections = Rejections::new();
        rejections.reject_ip("23.200.230.145", RejectCause::Forbidden);
        rejections.reject_name("steamcommunity.com", RejectCause::Forbidden);

        let candidates = vec![
            "23.200.230.145".to_string(),
            "23.200.230.175".to_string(),
        ];
        assert_eq!(rejections.filter_ips(&candidates), vec!["23.200.230.175".to_string()]);
        assert!(rejections.name_rejected("SteamCommunity.com"));
    }

    #[test]
    fn prune_keeps_table_bounded() {
        let mut rejections = Rejections::new();
        for i in 0..100 {
            rejections.reject_ip(&format!("10.0.0.{i}"), RejectCause::Forbidden);
        }
        assert_eq!(rejections.len(), 100);
        rejections.prune();
        assert_eq!(rejections.len(), 100, "尚未过期不应被清理");
    }
}
