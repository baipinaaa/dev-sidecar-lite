//! MITM 证书颁发：复用 Node 版 CA，按域名动态签发叶子证书。
//!
//! 与 Node 版保持兼容的文件：
//! - `~/.dev-sidecar/dev-sidecar.ca.crt`（CA 证书，PEM）
//! - `~/.dev-sidecar/dev-sidecar.ca.key.pem`（CA 私钥，PEM）
//!
//! 叶子证书在内存中签发并缓存，不落盘。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rcgen::{CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

/// 叶子证书缓存上限。
const LEAF_CACHE_LIMIT: usize = 512;

/// MITM 证书颁发机构。
pub struct CertAuthority {
    ca_key: KeyPair,
    ca_cert: rcgen::Certificate,
    cache: Mutex<HashMap<String, Arc<CertifiedKey>>>,
}

impl CertAuthority {
    /// 从 PEM 文本加载（CA 证书 + CA 私钥）。
    pub fn from_pem(ca_cert_pem: &str, ca_key_pem: &str) -> Result<Self> {
        let ca_key = KeyPair::from_pem(ca_key_pem).context("解析 CA 私钥失败")?;
        let params =
            CertificateParams::from_ca_cert_pem(ca_cert_pem).context("解析 CA 证书失败")?;
        let ca_cert = params.self_signed(&ca_key).context("重建 CA 证书失败")?;

        Ok(Self {
            ca_key,
            ca_cert,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// 从文件加载。
    pub fn from_files(ca_cert_path: &Path, ca_key_path: &Path) -> Result<Self> {
        let cert_pem = std::fs::read_to_string(ca_cert_path)
            .with_context(|| format!("读取 CA 证书失败：{}", ca_cert_path.display()))?;
        let key_pem = std::fs::read_to_string(ca_key_path)
            .with_context(|| format!("读取 CA 私钥失败：{}", ca_key_path.display()))?;
        Self::from_pem(&cert_pem, &key_pem)
    }

    /// 用默认路径加载（`~/.dev-sidecar/`）。
    pub fn from_default_paths() -> Result<Self> {
        let (cert, key) = default_ca_paths()?;
        Self::from_files(&cert, &key)
    }

    /// CA 证书的 PEM（用于提示用户安装）。
    pub fn ca_cert_pem(&self) -> String {
        self.ca_cert.pem()
    }

    /// 取（或签发）某域名的叶子证书。
    pub fn leaf_key(&self, hostname: &str) -> Result<Arc<CertifiedKey>> {
        let hostname = normalize_hostname(hostname)?;

        if let Ok(cache) = self.cache.lock() {
            if let Some(hit) = cache.get(&hostname) {
                return Ok(Arc::clone(hit));
            }
        }

        let issued = Arc::new(self.issue(&hostname)?);

        if let Ok(mut cache) = self.cache.lock() {
            if cache.len() >= LEAF_CACHE_LIMIT {
                cache.clear();
            }
            cache.insert(hostname, Arc::clone(&issued));
        }

        Ok(issued)
    }

    fn issue(&self, hostname: &str) -> Result<CertifiedKey> {
        let leaf_key = KeyPair::generate().context("生成叶子私钥失败")?;

        let mut params = CertificateParams::new(vec![hostname.to_string()])
            .with_context(|| format!("构造 {hostname} 的证书参数失败"))?;

        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, hostname);
        params.distinguished_name = dn;

        params.is_ca = IsCa::NoCa;
        params.use_authority_key_identifier_extension = true;
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];

        let cert = params
            .signed_by(&leaf_key, &self.ca_cert, &self.ca_key)
            .with_context(|| format!("用 CA 签发 {hostname} 失败"))?;

        let chain = vec![cert.der().clone()];
        let key_der = PrivatePkcs8KeyDer::from(leaf_key.serialize_der());
        let signing_key = rustls::crypto::ring::sign::any_supported_type(&PrivateKeyDer::Pkcs8(
            key_der,
        ))
        .context("把叶子私钥转成 rustls 签名器失败")?;

        Ok(CertifiedKey::new(chain, signing_key))
    }
}

/// `ResolvesServerCert` 实现：按 SNI 动态签发。
pub struct DynamicResolver {
    authority: Arc<CertAuthority>,
}

impl DynamicResolver {
    pub fn new(authority: Arc<CertAuthority>) -> Self {
        Self { authority }
    }
}

impl ResolvesServerCert for DynamicResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let Some(name) = client_hello.server_name() else {
            tracing::warn!("MITM 握手缺少 SNI，无法签发证书");
            return None;
        };

        match self.authority.leaf_key(name) {
            Ok(key) => Some(key),
            Err(err) => {
                tracing::warn!("为 {name} 签发证书失败：{err:#}");
                None
            }
        }
    }
}

/// 构造 MITM 用的 `ServerConfig`（仅 HTTP/1.1）。
pub fn mitm_server_config(authority: Arc<CertAuthority>) -> Result<Arc<rustls::ServerConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());

    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("构造 TLS 版本配置失败")?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(DynamicResolver::new(authority)));

    config.alpn_protocols = vec![b"http/1.1".to_vec()];

    Ok(Arc::new(config))
}

/// `~/.dev-sidecar/` 下的 CA 证书与私钥路径。
pub fn default_ca_paths() -> Result<(PathBuf, PathBuf)> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .context("无法确定用户目录（USERPROFILE/HOME 均未设置）")?;

    let base = PathBuf::from(home).join(".dev-sidecar");
    Ok((
        base.join("dev-sidecar.ca.crt"),
        base.join("dev-sidecar.ca.key.pem"),
    ))
}

fn normalize_hostname(hostname: &str) -> Result<String> {
    let hostname = hostname.trim().trim_end_matches('.').to_ascii_lowercase();
    if hostname.is_empty() {
        anyhow::bail!("域名为空");
    }
    if hostname.len() > 253 {
        anyhow::bail!("域名过长：{}", hostname.len());
    }
    if hostname.contains('/') || hostname.contains(':') {
        anyhow::bail!("域名含非法字符：{hostname}");
    }
    Ok(hostname)
}

/// 便于外部构造 CA 的测试辅助：生成一个临时 CA 并返回（证书 PEM, 私钥 PEM）。
pub fn generate_self_signed_ca(common_name: &str) -> Result<(String, String)> {
    let key = KeyPair::generate().context("生成 CA 私钥失败")?;

    let mut params = CertificateParams::new(Vec::<String>::new()).context("构造 CA 参数失败")?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, common_name);
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];

    let cert = params.self_signed(&key).context("自签 CA 失败")?;
    Ok((cert.pem(), key.serialize_pem()))
}

/// 叶子证书链的 DER（供测试断言）。
pub fn chain_der(key: &CertifiedKey) -> Vec<CertificateDer<'static>> {
    key.cert.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_leaf_from_generated_ca() {
        let (ca_pem, ca_key_pem) = generate_self_signed_ca("dev-sidecar-lite test CA").unwrap();
        let authority = CertAuthority::from_pem(&ca_pem, &ca_key_pem).unwrap();

        let key = authority.leaf_key("example.com").unwrap();
        assert_eq!(key.cert.len(), 1, "叶子证书链应只有一张证书");
        assert!(!key.cert[0].as_ref().is_empty());

        // 同一域名第二次命中缓存
        let again = authority.leaf_key("example.com").unwrap();
        assert!(Arc::ptr_eq(&key, &again), "同域名应命中缓存");
    }

    #[test]
    fn normalizes_hostname() {
        assert_eq!(normalize_hostname("GitHub.COM.").unwrap(), "github.com");
        assert!(normalize_hostname("  ").is_err());
        assert!(normalize_hostname("a/b").is_err());
        assert!(normalize_hostname("a:443").is_err());
    }

    #[test]
    fn server_config_builds() {
        let (ca_pem, ca_key_pem) = generate_self_signed_ca("test CA").unwrap();
        let authority = Arc::new(CertAuthority::from_pem(&ca_pem, &ca_key_pem).unwrap());
        let config = mitm_server_config(authority).unwrap();
        assert_eq!(config.alpn_protocols, vec![b"http/1.1".to_vec()]);
    }

    #[test]
    fn converts_leaf_private_key() {
        let (ca_pem, ca_key_pem) = generate_self_signed_ca("test CA").unwrap();
        let authority = CertAuthority::from_pem(&ca_pem, &ca_key_pem).unwrap();
        let key = authority.leaf_key("a.example.org").unwrap();
        // CertifiedKey 内的签名器必须可用
        assert!(!key.cert.is_empty());
        assert_eq!(chain_der(&key).len(), 1);
    }
}
