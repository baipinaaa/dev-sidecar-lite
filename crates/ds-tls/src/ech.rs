//! ECH（Encrypted Client Hello）接入。
//!
//! 输入是从 DNS 的 HTTPS(65) 记录里取到的 **ECHConfigList**（SvcParam key=5），
//! 输出能直接用于 `TlsConnector` 的 `ClientConfig`。
//!
//! ## 两个已经踩过的坑（对着 rustls v0.23.46 源码核对过）
//!
//! 1. **`with_ech` 在 builder 上，不在 `ClientConfig` 上**，而且它会强制 TLS 1.3：
//!    ```text
//!    ClientConfig::builder_with_provider(p)   // ConfigBuilder<ClientConfig, WantsVersions>
//!        .with_ech(EchMode::Enable(cfg))?     // ConfigBuilder<ClientConfig, WantsVerifier>
//!        .dangerous().with_custom_certificate_verifier(..) / .with_root_certificates(..)
//!        .with_no_client_auth()
//!    ```
//! 2. **HPKE 只有 aws-lc-rs 提供者实现**（`rustls/src/crypto/ring/` 里没有 `hpke.rs`），
//!    所以即使用 ring 做常规 TLS，ECH 也必须用 aws-lc-rs 建配置，否则
//!    `EchConfig::new` 根本拿不到 `&[&dyn Hpke]` 可用套件。

use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::client::{EchConfig, EchMode};
use rustls::crypto::aws_lc_rs::hpke::ALL_SUPPORTED_SUITES;
use rustls::pki_types::EchConfigListBytes;

use crate::client::{insecure_client_config, NoVerify};

/// 构建启用 ECH 的上游客户端配置（TLS 1.3 only）。
///
/// - `verify == false`：不校验上游证书（与 Node 版一致）
/// - `ech_bytes`：ECHConfigList 的原始字节（通常 60–200 字节）
///
/// ECH 只对 TLS 1.3 有意义，所以这里不提供 TLS 1.2 回退；需要回退时用
/// [`build_plain_upstream_config`]。
pub fn build_ech_upstream_config(
    verify: bool,
    ech_bytes: &[u8],
) -> Result<Arc<rustls::ClientConfig>> {
    if ech_bytes.is_empty() {
        anyhow::bail!("ECHConfigList 为空");
    }

    let list = EchConfigListBytes::from(ech_bytes.to_vec());
    let ech_config = EchConfig::new(list, ALL_SUPPORTED_SUITES)
        .map_err(|err| anyhow::anyhow!("ECHConfigList 解析失败: {err}"))?;
    let mode = EchMode::Enable(ech_config);

    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());

    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_ech(mode)
        .map_err(|err| anyhow::anyhow!("启用 ECH 失败: {err}"))?;

    let builder = if verify {
        let mut roots = rustls::RootCertStore::empty();
        let result = rustls_native_certs::load_native_certs();
        let (added, _ignored) = roots.add_parsable_certificates(result.certs);
        if added == 0 {
            tracing::warn!("没有可用系统根证书，ECH 连接退回不校验模式");
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerify { provider }))
        } else {
            builder.with_root_certificates(roots)
        }
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify { provider }))
    };

    Ok(Arc::new(builder.with_no_client_auth()))
}

/// 不带 ECH 的常规上游配置（可用 TLS 1.2，ring 提供者）。
pub fn build_plain_upstream_config(verify: bool) -> Result<Arc<rustls::ClientConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());

    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .context("构造 TLS 版本配置失败")?;

    let builder = if verify {
        let mut roots = rustls::RootCertStore::empty();
        let result = rustls_native_certs::load_native_certs();
        let (added, _ignored) = roots.add_parsable_certificates(result.certs);
        if added == 0 {
            tracing::warn!("没有可用系统根证书，退回不校验模式");
            return Ok(insecure_client_config());
        }
        builder.with_root_certificates(roots)
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify { provider }))
    };

    Ok(Arc::new(builder.with_no_client_auth()))
}

/// 统一入口：拿到 ECH 字节就用 ECH（TLS 1.3），拿不到就用常规配置。
pub fn build_upstream_config(
    verify: bool,
    ech_bytes: Option<&[u8]>,
) -> Result<Arc<rustls::ClientConfig>> {
    match ech_bytes.filter(|bytes| !bytes.is_empty()) {
        Some(bytes) => build_ech_upstream_config(verify, bytes),
        None => build_plain_upstream_config(verify),
    }
}

/// 便捷入口：不校验证书 + 启用 ECH。
pub fn ech_only_config(ech_bytes: &[u8]) -> Result<Arc<rustls::ClientConfig>> {
    build_ech_upstream_config(false, ech_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ech_bytes_falls_back_to_plain() {
        // 空字节视作「没有 ECH」，应拿到常规配置，且不是 TLS1.3-only
        let config = build_upstream_config(false, Some(&[])).unwrap();
        assert!(!config.alpn_protocols.contains(&b"h2".to_vec()));
    }

    #[test]
    fn garbage_ech_is_rejected() {
        // 非法 ECHConfigList 必须是 Err，不能 panic
        let result = build_upstream_config(false, Some(&[0xFF, 0xFF, 0xFF]));
        assert!(result.is_err());
    }

    #[test]
    fn plain_config_builds() {
        let config = build_plain_upstream_config(false).unwrap();
        assert_eq!(config.alpn_protocols.len(), 0);
    }
}
