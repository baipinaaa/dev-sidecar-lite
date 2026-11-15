//! 失败分类学（借鉴 hakimi `strategy.rs`）。
//!
//! 关键洞察：**"TCP 起来"是地址的属性，而"TLS 成功"是 (地址, SNI, ECH) 三元组的属性**。
//! 把 TLS 失败记到地址头上会让运维查错方向，所以只有
//! `TcpTimeout | TcpUnreachable | TcpRefused` 才算"这个 IP 不行"。

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureReason {
    DohFailed,
    DohFallback,
    RemoteCatalogUnavailable,
    RemoteIpRejected,
    TcpTimeout,
    TlsTimeout,
    TcpUnreachable,
    TlsReset,
    CertInvalid,
    EchUnavailable,
    SniRejected,
    CaUntrusted,
    ProxyNotApplied,
    QuicBypass,
    UnsupportedWithoutProxy,
    NoCandidate,
    TcpRefused,
    TlsError,
    ConfigInvalid,
    Internal,
}

impl FailureReason {
    /// 只有这三类能归因到 IP 本身。
    pub fn is_ip_level(self) -> bool {
        matches!(
            self,
            FailureReason::TcpTimeout | FailureReason::TcpUnreachable | FailureReason::TcpRefused
        )
    }

    /// 是否该记到"名字"维度（同一 CDN 边缘的每个 IP 都会发同一张外国证书）。
    pub fn is_name_level(self) -> bool {
        matches!(
            self,
            FailureReason::SniRejected | FailureReason::CertInvalid | FailureReason::CaUntrusted
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            FailureReason::DohFailed => "doh_failed",
            FailureReason::DohFallback => "doh_fallback",
            FailureReason::RemoteCatalogUnavailable => "remote_catalog_unavailable",
            FailureReason::RemoteIpRejected => "remote_ip_rejected",
            FailureReason::TcpTimeout => "tcp_timeout",
            FailureReason::TlsTimeout => "tls_timeout",
            FailureReason::TcpUnreachable => "tcp_unreachable",
            FailureReason::TlsReset => "tls_reset",
            FailureReason::CertInvalid => "cert_invalid",
            FailureReason::EchUnavailable => "ech_unavailable",
            FailureReason::SniRejected => "sni_rejected",
            FailureReason::CaUntrusted => "ca_untrusted",
            FailureReason::ProxyNotApplied => "proxy_not_applied",
            FailureReason::QuicBypass => "quic_bypass",
            FailureReason::UnsupportedWithoutProxy => "unsupported_without_proxy",
            FailureReason::NoCandidate => "no_candidate",
            FailureReason::TcpRefused => "tcp_refused",
            FailureReason::TlsError => "tls_error",
            FailureReason::ConfigInvalid => "config_invalid",
            FailureReason::Internal => "internal",
        }
    }

    /// 给用户看的建议（日志与 UI 共用）。
    pub fn suggestion(self) -> &'static str {
        match self {
            FailureReason::DohFailed => "换成可达的 DoH（aliyun / 1.12.12.12），或检查该端点是否被墙",
            FailureReason::DohFallback => "首选 DoH 超时，已自动回退；建议把首选改成国内可达的端点",
            FailureReason::RemoteCatalogUnavailable => "远程规则目录拉取失败，稍后重试或改用本地规则",
            FailureReason::RemoteIpRejected => "远程下发的 IP 被上游拒绝，已忽略该条",
            FailureReason::TcpTimeout => "该 IP 的 443 不通（可能被墙或已下线），换用其它地址",
            FailureReason::TlsTimeout => "TCP 已连通但 TLS 无响应，多为中间设备干扰",
            FailureReason::TcpUnreachable => "网络不可达，检查路由或代理链",
            FailureReason::TlsReset => "TLS 握手被重置，通常是 SNI 被识别；改用 ECH 或伪装 SNI",
            FailureReason::CertInvalid => "上游证书与域名不匹配，检查 sni / verifyHost 配置",
            FailureReason::EchUnavailable => "该域名没有可用的 ECHConfig，已降级为普通握手",
            FailureReason::SniRejected => "该 CDN 拒绝此 SNI，同一 IP 重试无意义",
            FailureReason::CaUntrusted => "本地 CA 未被信任，请安装 dev-sidecar 根证书",
            FailureReason::ProxyNotApplied => "程序未使用系统代理（PAC/环境变量未生效）",
            FailureReason::QuicBypass => "站点通过 Alt-Svc 升级到 h3 绕过了代理，需移除 alt-svc",
            FailureReason::UnsupportedWithoutProxy => "该程序不支持代理，只能走直连或系统层拦截",
            FailureReason::NoCandidate => "没有任何候选地址，检查 DNS 与预置 IP",
            FailureReason::TcpRefused => "端口被显式拒绝，该 IP 不提供此服务",
            FailureReason::TlsError => "TLS 层其它错误，见详细日志",
            FailureReason::ConfigInvalid => "配置不合法，请检查 running.json",
            FailureReason::Internal => "内部错误，请附日志反馈",
        }
    }
}

impl fmt::Display for FailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 把底层错误文本归类成 `FailureReason`。
pub fn classify(message: &str) -> FailureReason {
    let text = message.to_ascii_lowercase();

    let has = |needle: &str| text.contains(needle);

    if has("timed out") || has("timeout") || has("超时") {
        if has("tls") || has("handshake") || has("握手") {
            return FailureReason::TlsTimeout;
        }
        return FailureReason::TcpTimeout;
    }
    if has("connection refused") || has("拒绝") {
        return FailureReason::TcpRefused;
    }
    if has("unreachable") || has("no route") {
        return FailureReason::TcpUnreachable;
    }
    if has("reset by peer") || has("connection reset") || has("被重置") {
        return FailureReason::TlsReset;
    }
    if has("certificate") || has("cert") || has("证书") {
        if has("unknown issuer") || has("untrusted") || has("不受信任") {
            return FailureReason::CaUntrusted;
        }
        return FailureReason::CertInvalid;
    }
    if has("ech") {
        return FailureReason::EchUnavailable;
    }
    if has("dns") || has("doh") {
        return FailureReason::DohFailed;
    }
    if has("alt-svc") || has("quic") || has("h3") {
        return FailureReason::QuicBypass;
    }
    if has("tls") {
        return FailureReason::TlsError;
    }
    if has("config") || has("配置") {
        return FailureReason::ConfigInvalid;
    }
    FailureReason::Internal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tcp_failures_are_ip_level() {
        assert!(FailureReason::TcpTimeout.is_ip_level());
        assert!(FailureReason::TcpRefused.is_ip_level());
        assert!(!FailureReason::TlsTimeout.is_ip_level());
        assert!(!FailureReason::TlsReset.is_ip_level());
        assert!(!FailureReason::CertInvalid.is_ip_level());
    }

    #[test]
    fn sni_and_cert_are_name_level() {
        assert!(FailureReason::SniRejected.is_name_level());
        assert!(FailureReason::CertInvalid.is_name_level());
        assert!(!FailureReason::TcpTimeout.is_name_level());
    }

    #[test]
    fn classifies_common_messages() {
        assert_eq!(
            classify("connect ETIMEDOUT 23.200.230.145:443"),
            FailureReason::TcpTimeout
        );
        assert_eq!(
            classify("TLS handshake timeout"),
            FailureReason::TlsTimeout
        );
        assert_eq!(
            classify("read ECONNRESET"),
            FailureReason::TlsReset
        );
        assert_eq!(
            classify("unable to verify the first certificate"),
            FailureReason::CertInvalid
        );
        assert_eq!(
            classify("connect ECONNREFUSED 127.0.0.1:9"),
            FailureReason::TcpRefused
        );
        assert_eq!(classify("ECH 失败(TLS 握手超时(15000ms))"), FailureReason::TlsTimeout);
    }

    #[test]
    fn every_reason_has_suggestion() {
        for reason in [
            FailureReason::DohFailed,
            FailureReason::TcpTimeout,
            FailureReason::SniRejected,
            FailureReason::Internal,
        ] {
            assert!(!reason.suggestion().is_empty());
            assert!(!reason.as_str().is_empty());
        }
    }
}
