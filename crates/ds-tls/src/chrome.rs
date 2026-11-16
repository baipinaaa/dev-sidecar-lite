//! Chrome 指纹通道：用 BoringSSL 复刻 Chrome 的 ClientHello。
//!
//! 为什么不用 rustls：Cloudflare 判的是 TLS 指纹（实测见 `docs/FINGERPRINT.md`、
//! `docs/FINGERPRINT-B1.md`）。rustls 的 JA4 是 `t13d1012h1_`，Chrome 156 实测是
//! `t13d1517h2_`，两者在套件集、`supported_groups`、ALPN、扩展集上逐项不同，而
//! rustls 不提供控制扩展顺序 / 加自定义扩展的接口。Chrome 自己用的就是 BoringSSL，
//! 所以这里直接调 BoringSSL —— 实测三轮把差距从 7 个维度收敛到只差 3 个扩展
//! （见 `docs/FINGERPRINT-B1.md` 第六、七节）。
//!
//! 目标不是"JA3 完全相同"：Chrome 开了 `SSL_set_permute_extensions`，**扩展顺序每次
//! 连接都随机**，所以固定顺序的 JA3 hash 不可能相等。正确标尺是 **JA4**（对顺序不敏感），
//! 目标前缀 `t13d1517h2_`。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use boring::ssl::{
    CertificateCompressionAlgorithm, CertificateCompressor, SslConnector, SslMethod, SslStream,
    SslVerifyMode,
};
use foreign_types::ForeignType;
use tracing::debug;

/// Chrome 156（以及 157）的 JA4 前缀：13=TLS1.3、d=域名 SNI、15=15 个套件、17=17 个扩展、
/// h2=ALPN 首选 h2。我们做完 ALPS + 证书压缩后应当也是这个值。
pub const CHROME_JA4_EXPECTED: &str = "t13d1517h2_";

/// TLS 1.2 套件列表（逐项取自实测的 Chrome ClientHello）。
/// TLS 1.3 的三个套件（4865/4866/4867）由 BoringSSL 默认给出，正是 Chrome 的顺序，
/// 而 boring 5.5.1 **没有** `set_ciphersuites`，所以不必也无法单独设置。
const CHROME_CIPHERS: &str =
    "ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305:ECDHE-RSA-AES128-SHA:ECDHE-RSA-AES256-SHA:AES128-GCM-SHA256:AES256-GCM-SHA384:AES128-SHA:AES256-SHA";

/// `supported_groups`：Chrome 先给后量子混合组，再 X25519、P-256、P-384。
const CHROME_GROUPS: &str = "X25519MLKEM768:X25519:P-256:P-384";

/// ALPN：h2 优先，回退 http/1.1。
const CHROME_ALPN: &[u8] = b"\x02h2\x08http/1.1";

/// ALPS 的协议名（扩展 17613 `application_settings`，只对 h2 生效）。
const ALPS_PROTO: &[u8] = b"h2";

/// 证书压缩：只声明"我能解压"，与 Chrome 一致（Chrome 只支持解压 zlib/brotli/zstd）。
struct ZlibDecompressor;

impl CertificateCompressor for ZlibDecompressor {
    const ALGORITHM: CertificateCompressionAlgorithm = CertificateCompressionAlgorithm::ZLIB;
    const CAN_COMPRESS: bool = false;
    const CAN_DECOMPRESS: bool = true;

    fn decompress<W: Write>(&self, input: &[u8], output: &mut W) -> std::io::Result<()> {
        let data = miniz_oxide::inflate::decompress_to_vec_zlib(input)
            .map_err(|e| std::io::Error::other(format!("zlib 解压失败: {e:?}")))?;
        output.write_all(&data)
    }
}

/// 取全局共享的 connector（构建一次，之后每连接 clone 配置）。
fn connector() -> Result<&'static SslConnector> {
    static CONNECTOR: OnceLock<Result<SslConnector, String>> = OnceLock::new();
    let built = CONNECTOR.get_or_init(|| build_connector().map_err(|e| format!("{e:#}")));
    match built {
        Ok(connector) => Ok(connector),
        Err(message) => Err(anyhow!("无法初始化 Chrome 指纹 connector: {message}")),
    }
}

fn build_connector() -> Result<SslConnector> {
    let mut builder = SslConnector::builder(SslMethod::tls())?;

    // 与 Node 版 `unVerifySslAgent` 等价：上游任意证书都接受。
    // 注意只用 `set_verify(NONE)` 不够 —— BoringSSL 仍会回 65（X509_V_ERR_...），
    // 必须给一个恒真的校验回调。
    // 注意：`set_verify_callback` 返回 `()`，不是 `Result`（boring 5.5.0 的签名如此）。
    builder.set_verify_callback(SslVerifyMode::NONE, |_preverify_ok, _ctx| true);

    // 套件表 / 曲线表都做「失败即降级」：BoringSSL 的构建开关会决定某些名字是否被接受
    // （例如未启用 ML-KEM 时 `X25519MLKEM768` 会报错）。降级只影响指纹精度，不该让
    // 整条通道不可用；降级原因打到 stderr，CI 日志里能直接看到。
    if let Err(err) = builder.set_cipher_list(CHROME_CIPHERS) {
        eprintln!("[ds-tls] 警告: Chrome 套件表被拒绝（{err}），改用 BoringSSL 默认套件");
    }
    if let Err(err) = builder.set_curves_list(CHROME_GROUPS) {
        eprintln!("[ds-tls] 警告: `{CHROME_GROUPS}` 被拒绝（{err}），退回 X25519:P-256:P-384");
        builder
            .set_curves_list("X25519:P-256:P-384")
            .map_err(|e| anyhow!("设置 supported_groups 失败: {e}"))?;
    }
    builder.set_alpn_protos(CHROME_ALPN)?;

    // Chrome 的扩展顺序是随机的；不开这个，JA4 一致但 JA3 会固定成一个非 Chrome 的值。
    builder.set_permute_extensions(true);

    // 扩展 5（status_request）与 18（signed_certificate_timestamp）。
    builder.enable_ocsp_stapling();
    builder.enable_signed_cert_timestamps();

    // 扩展 27（compress_certificate）。
    if let Err(err) = builder.add_certificate_compression_algorithm(ZlibDecompressor) {
        eprintln!("[ds-tls] 警告: 注册 zlib 证书压缩失败（{err}），JA4 会少扩展 27");
    }

    Ok(builder.build())
}

/// 用 Chrome 指纹连到 `host:443`。
///
/// `ech` 是 DoH 查到的 ECHConfigList 原始字节；有就发真 ECH，没有就只发 ECH grease
/// （Chrome 在拿不到配置时也是这么做的）。
pub fn chrome_connect(
    host: &str,
    tcp: TcpStream,
    ech: Option<&[u8]>,
    alpn_h2: bool,
) -> Result<SslStream<TcpStream>> {
    let _ = tcp.set_nodelay(true);

    let connector = connector()?;
    let configuration = connector.configure()?;
    let mut ssl = configuration.into_ssl(host)?;

    // ALPN 必须与客户端实际协商到的协议一致：中继是纯字节搬运，
    // 客户端说 HTTP/1.1 而上游说 h2 会让对端按 h2 帧解析，直接乱码。
    let alpn: &[u8] = if alpn_h2 { b"h2" } else { b"http/1.1" };
    ssl.set_alpn_protos(alpn)?;

    // 扩展 65037（encrypted_client_hello）。grease 与真 ECH 不冲突：
    // ZlibDecompressor 之外，grease 保证"即使没有配置也带这个扩展"。
    ssl.set_enable_ech_grease(true);
    if let Some(bytes) = ech {
        ssl.set_ech_config_list(bytes)?;
        debug!("{host} 上游握手携带 ECH（{} 字节配置）", bytes.len());
    }

    // 扩展 17613（application_settings / ALPS）。boring 5.5.1 没有安全封装，
    // 只有一个函数需要 unsafe —— 见文件末尾的 `add_application_settings`。
    #[allow(unsafe_code)]
    let alps_ok = {
        let raw = ssl.as_ptr() as *mut core::ffi::c_void;
        // SAFETY: `ssl` 是本函数独占的 `Ssl`，指针在本次调用期间有效；
        // 传入的 proto 是静态切片，settings 为空指针 + 长度 0（Chrome 首次连接就是这样）。
        unsafe { add_application_settings(raw) }
    };
    if !alps_ok {
        debug!("{host} 未能添加 ALPS（application_settings），JA4 会少 17613");
    }

    let mut stream = SslStream::new(ssl, tcp)?;
    // 必须用 `Pin::new(..).connect()` 而不是 `do_handshake()`：
    // 只有前者会把 SSL 置成"客户端连接态"，后者直接报 `CONNECTION_TYPE_NOT_SET`。
    std::pin::Pin::new(&mut stream)
        .connect()
        .map_err(|err| anyhow!("与 {host} 的 TLS 握手失败: {err}"))?;
    Ok(stream)
}

/// 读空对端直到 EOF 或出错，返回读到的字节数（诊断用）。
pub fn drain(stream: &mut SslStream<TcpStream>) -> std::io::Result<u64> {
    let mut buffer = [0u8; 8 * 1024];
    let mut total = 0u64;
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(n) => total += n as u64,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// `SSL_add_application_settings` 是 BoringSSL 对 ALPS 的底层入口，boring 5.5.1 未导出。
///
/// 之所以必须调它：ALPS 是 Chrome 必带的扩展（17613），缺了它 JA4 的扩展计数就是 16 而不是 17。
#[allow(unsafe_code)]
unsafe fn add_application_settings(ssl: *mut core::ffi::c_void) -> bool {
    extern "C" {
        fn SSL_add_application_settings(
            ssl: *mut core::ffi::c_void,
            protocol: *const u8,
            protocol_len: usize,
            settings: *const u8,
            settings_len: usize,
        ) -> core::ffi::c_int;
    }
    // SAFETY: 调用方保证 `ssl` 指向有效的 `SSL*`；空 settings 表示"稍后由回调给出"，
    // 与 Chrome 在 ClientHello 里只发空 ALPS 扩展的行为一致。
    unsafe {
        SSL_add_application_settings(
            ssl,
            ALPS_PROTO.as_ptr(),
            ALPS_PROTO.len(),
            std::ptr::null(),
            0,
        ) == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connector_can_be_built() {
        // 只验证参数组合被 BoringSSL 接受（不需要网络）。
        let connector = build_connector().expect("构建 Chrome connector 失败");
        // 拿它配一个 Ssl 出来，验证曲线/套件真正生效（不握手）。
        let ssl = connector.configure().expect("configure 失败");
        drop(ssl);
    }

    #[test]
    fn alps_constant_and_ja4_target() {
        assert_eq!(ALPS_PROTO, b"h2");
        assert!(CHROME_JA4_EXPECTED.starts_with("t13d15"));
    }
}
