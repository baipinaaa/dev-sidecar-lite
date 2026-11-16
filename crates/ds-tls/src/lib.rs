//! ds-tls —— MITM CA 与上游 TLS（ECH / SNI 策略）。
//!
//! 阶段 1：证书颁发（`ca` 模块）。
//! 阶段 2：上游 TLS（rustls ECH、SNI 策略）。
//! 阶段 2b：`chrome` 模块——用 BoringSSL 复刻 Chrome 的 ClientHello（JA4 对齐），
//! 见 docs/FINGERPRINT.md 与 docs/FINGERPRINT-B1.md。
//!
//! 这里用 `deny` 而不是 `forbid`：ALPS（扩展 17613）在 boring 5.5.0 里没暴露安全 API，
//! 只有一个函数需要 `unsafe` 调 `SSL_add_application_settings`，其余全部安全代码。

#![deny(unsafe_code)]

pub mod ca;
pub mod chrome;
pub mod client;
pub mod ech;

pub use ca::{CertAuthority, DynamicResolver};
pub use chrome::{chrome_connect, CHROME_JA4_EXPECTED};
pub use client::{insecure_client_config, native_roots_client_config};
pub use ech::{build_ech_upstream_config, build_plain_upstream_config, build_upstream_config};
