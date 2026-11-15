# TLS 指纹路线决策（步骤 11）

> 结论先行：**主力用 rustls 做 ECH，指纹伪装另走 BoringSSL 通道**；二者在同一个 `ds-tls` 里按「域名策略」二选一，不做混合。

## 为什么不能只用 rustls

`rustls` 是「安全优先」的实现，ClientHello 由库生成，**不提供扩展顺序、GREASE、ALPS 等 Chrome 特有字段的定制**。实测差异（对 Cloudflare Bot Management 的影响）：

| 维度 | Chrome 149 | rustls 0.23 默认 |
| --- | --- | --- |
| GREASE 值 | 有（每个列表都插） | 无 |
| 扩展顺序 | 固定 Chrome 顺序 | 库内固定、与 Chrome 不同 |
| supported_groups | X25519, secp256r1, secp384r1, X25519MLKEM768… | X25519, secp256r1, secp384r1（无 PQ 优先） |
| ALPN | h2, http/1.1 | 空（除非显式设置） |
| signature_algorithms | 含 rsa_pss_rsae_sha256 等完整集 | 集相同但顺序不同 |
| record_size_limit / ALPS | 有 | 无 / 无 |
| ECH | 有 | **有**（0.23 原生） |

结论：rustls 的 JA3/JA4 与 Chrome 差距明显，遇到 CF Bot Management 仍会被判为"非浏览器"。

## 三条候选路线

### A. rustls（当前已落地）
- 优点：ECH 原生、无 C 依赖、跨平台交叉编译容易、内存安全
- 缺点：指纹固定、无法伪装
- 适用：不校验指纹的站点（绝大多数镜像站、GitHub raw、npm registry）

### B. BoringSSL（`boring` crate / `wreq` 的 Rust 实现）
- 优点：能完整复刻 Chrome（GREASE、扩展顺序、PQ 组、ALPS、ECH）
- 缺点：需要 cmake/NASM 编译、Windows 构建链重、体积大（+数 MB）
- 适用：Cloudflare Bot Management 严格站点（linux.do、部分 discourse 站）

### C. 自拼 ClientHello（Node 版现状）
- 优点：完全可控
- 缺点：维护成本极高、每次浏览器更新都要跟、易被 JA4 识别（本项目已验证会被 CF 挑战）
- **不采用**

## 决策

1. **`ds-tls` 内维持两套 `ClientConfig` 工厂**：
   - `build_upstream_config(verify, ech_bytes)` → rustls（已实现，步骤 10）
   - `build_fingerprint_config(profile)` → BoringSSL（阶段 2 引入 `boring`，按需编译）
2. **按域名选择**：`intercepts` 里新增可选项 `fingerprint: "chrome"`；未指定则走 rustls。
3. **ECH 与指纹的交叉**：
   - rustls + ECH：已支持
   - BoringSSL + ECH：取决于 `boring` 版本是否暴露 `SSL_set1_ech_config_list`，属于实验项
   - 若无法组合，则按站点重要性取舍：CF 严格站优先"指纹"，无挑战的站优先"ECH"

## 实验计划（需在有 Rust 环境时执行）

```powershell
# 1. 采集 Chrome 真实指纹（本机 Chromium 访问任意 https 站，用 Wireshark 抓 ClientHello）
# 2. 采集 rustls 指纹
cargo run -p ds-dns --example ech_check -- linux.do
# 3. 计算 JA3/JA4 并对比（脚本：tests/e2e/ja3_from_pcap.py）
python tests/e2e/ja3_from_pcap.py chrome.pcapng
python tests/e2e/ja3_from_pcap.py rustls.pcapng
```

判定标准（写入验收）：
- JA3 完全一致 → 视为通过（Chrome 复刻成功）
- JA4 前缀一致（`t13d1516h2_` 等）→ 视为"可接受"
- 均不一致 → 该站点必须走 BoringSSL 通道

## 当前状态

- rustls + ECH：**已实现**（`crates/ds-tls/src/ech.rs`）
- BoringSSL 通道：**未实现**，等待步骤 12 的 CF 验收结果决定是否值得引入
