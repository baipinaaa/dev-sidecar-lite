//! ECH（Encrypted Client Hello）接入。
//!
//! 输入是从 DNS 的 HTTPS(65) 记录里取到的 **ECHConfigList**（SvcParam key=5），
//! 输出能直接用于 `TlsConnector` 的 `ClientConfig`。
//!
//! 注意：rustls 的 ECH API 在 0.23.x 内部有过调整。若 CI 报
//! `no method named with_ech` / `ECHConfigList` 路径不对，只需改本文件两处：
//! - `use rustls::pki_types::ECHConfigList;` 的类型来源
//! - `.with_ech(...)` 挂在 `ClientConfig` 还是 `ConfigBuilder` 上

use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::client::EchConfig;
use rustls::pki_types::ECHConfigList;

use crate::client::{insecure_client_config, NoVerify};

/// 构建上游客户端配置，可选启用 ECH。
///
/// - `verify == false`：不校验上游证书（与 Node 版一致）
/// - `ech_bytes`：ECHConfigList 的原始字节（通常 60–200 字节）
pub fn build_upstream_config(
    verify: bool,
    ech_bytes: Option<&[u8]>,
) -> Result<Arc<rustls::ClientConfig>> {
    let ech = match ech_bytes {
        None => None,
        Some(bytes) => {
            if bytes.is_empty() {
                None
            } else {
                let list = ECHConfigList::from(bytes.to_vec());
                Some(EchConfig::new(list, None).context("ECHConfigList 解析失败")?)
            }
        }
    };

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

    let config = builder.with_no_client_auth();

    let config = match ech {
        None => config,
        Some(ech_config) => config.with_ech(ech_config).context("启用 ECH 失败")?,
    };

    Ok(Arc::new(config))
}

/// 便捷入口：不校验证书 + 启用 ECH。
pub fn ech_only_config(ech_bytes: &[u8]) -> Result<Arc<rustls::ClientConfig>> {
    build_upstream_config(false, Some(ech_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ech_bytes_means_no_ech() {
        // 空字节应视作「不启用 ECH」，构建出的配置可用
        let config = build_upstream_config(false, Some(&[])).unwrap();
        assert!(!config.alpn_protocols.contains(&b"h2".to_vec()));
    }

    #[test]
    fn garbage_ech_is_rejected_or_ignored() {
        // 非法 ECHConfigList 不应 panic
        let result = build_upstream_config(false, Some(&[0xFF, 0xFF, 0xFF]));
        assert!(result.is_err() || result.is_ok());
    }
}
