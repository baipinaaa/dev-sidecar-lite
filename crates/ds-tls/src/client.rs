//! 上游 TLS 客户端配置。
//!
//! dev-sidecar 的语义是「尽量连上」：DNS/加速链路里上游证书常被中间设备替换，
//! Node 版对此统一使用 `rejectUnauthorized: false`。这里保持一致，
//! 但签名仍按标准算法校验（防止握手被随意伪造）。

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

/// 不校验证书链，只校验握手签名。
#[derive(Debug)]
pub struct NoVerify {
    pub(crate) provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// 不校验上游证书的客户端配置。
pub fn insecure_client_config() -> Arc<rustls::ClientConfig> {
    build_config(|provider| Arc::new(NoVerify { provider }))
}

/// 使用系统根证书校验（DoH / 正常 HTTPS）。
pub fn native_roots_client_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    let result = rustls_native_certs::load_native_certs();
    let (added, ignored) = roots.add_parsable_certificates(result.certs);
    if ignored > 0 {
        tracing::debug!("系统根证书有 {ignored} 张无法解析");
    }
    if added == 0 {
        tracing::warn!("未能加载任何系统根证书，回退为不校验");
        return insecure_client_config();
    }

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring 提供方支持所有默认 TLS 版本")
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

fn build_config<F>(make_verifier: F) -> Arc<rustls::ClientConfig>
where
    F: FnOnce(Arc<CryptoProvider>) -> Arc<dyn ServerCertVerifier>,
{
    let provider = Arc::new(rustls::crypto::ring::default_provider());

    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("ring 提供方支持所有默认 TLS 版本")
        .dangerous()
        .with_custom_certificate_verifier(make_verifier(provider))
        .with_no_client_auth();

    Arc::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insecure_config_builds() {
        let config = insecure_client_config();
        assert!(config.alpn_protocols.is_empty());
    }

    #[test]
    fn native_roots_config_builds() {
        // 允许环境里没有根证书（会回退），只要不 panic
        let _ = native_roots_client_config();
    }
}
