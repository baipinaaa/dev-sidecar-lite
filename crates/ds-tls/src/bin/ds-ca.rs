//! 生成（或覆盖）本地 MITM 根证书。
//!
//! 用法：
//! ```text
//! cargo run -p ds-tls --bin ds-ca -- [输出目录]
//! ```
//! 默认输出到 `%USERPROFILE%\.dev-sidecar`（与 Node 版同目录同名），
//! 这样 ds-proxy 直接就能加载，不需要额外配置。
//!
//! 这个工具存在的原因：CI 里没有 Node 版 dev-sidecar，无法借它生成 CA，
//! 而 MITM 指纹实验必须有 CA 才能解密上游。

use std::env;
use std::fs;
use std::path::PathBuf;

use ds_tls::ca::generate_self_signed_ca;

const CA_CERT_NAME: &str = "dev-sidecar.ca.crt";
const CA_KEY_NAME: &str = "dev-sidecar.ca.key.pem";

fn main() -> anyhow::Result<()> {
    let dir = resolve_dir()?;
    fs::create_dir_all(&dir)?;

    let (cert_pem, key_pem) = generate_self_signed_ca("DevSidecar Lite Local CA")?;

    let cert_path = dir.join(CA_CERT_NAME);
    let key_path = dir.join(CA_KEY_NAME);
    fs::write(&cert_path, cert_pem.as_bytes())?;
    fs::write(&key_path, key_pem.as_bytes())?;

    println!("CA 证书：{}", cert_path.display());
    println!("CA 私钥：{}", key_path.display());
    println!("提示：把证书导入「受信任的根证书颁发机构」后浏览器才不会告警。");
    Ok(())
}

fn resolve_dir() -> anyhow::Result<PathBuf> {
    if let Some(arg) = env::args().nth(1) {
        return Ok(PathBuf::from(arg));
    }
    for key in ["USERPROFILE", "HOME"] {
        if let Ok(base) = env::var(key) {
            if !base.is_empty() {
                return Ok(PathBuf::from(base).join(".dev-sidecar"));
            }
        }
    }
    anyhow::bail!("找不到 USERPROFILE / HOME，请显式传入输出目录")
}
