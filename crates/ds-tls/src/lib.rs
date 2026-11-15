//! ds-tls —— MITM CA 与上游 TLS（ECH / SNI 策略）。
//!
//! 阶段 1：证书颁发（`ca` 模块）。
//! 阶段 2：上游 TLS（rustls ECH、SNI 策略）。指纹（JA3/JA4 对齐 Chrome）见 docs/FINGERPRINT.md。

#![forbid(unsafe_code)]

pub mod ca;
pub mod client;
pub mod ech;

pub use ca::{CertAuthority, DynamicResolver};
pub use client::{insecure_client_config, native_roots_client_config};
pub use ech::{build_ech_upstream_config, build_plain_upstream_config, build_upstream_config};
