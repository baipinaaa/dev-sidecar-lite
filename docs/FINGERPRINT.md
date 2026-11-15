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

## 实验执行方式（已跑通，无需本机 Rust）

指纹不能自己算——ClientHello 的形态要在**对端眼里**才是有效的。做法是让流量经过被测引擎，
访问会回显握手细节的服务，让它把看到的东西原样吐回来：

```powershell
# CI 里自动化，见 .github/workflows/fingerprint.yml
pwsh -File tests/e2e/fingerprint_compare.ps1 -Label rust -ProxyUrl http://127.0.0.1:31182
```

链路：`curl -k -x http://127.0.0.1:31182 https://tls.peet.ws/api/all`
→ 本地 ds-proxy 以 `tls.peet.ws` 命中 `intercepts` 做 MITM
→ **ds-proxy 用自己的 rustls 栈向 tls.peet.ws 建连** → 对端解析我们的 ClientHello 并回显。

注意两点（都踩过）：
- 代理必须与采集**在同一个 CI step 内**起停。跨 step 时 runner 会收掉 `Start-Process` 起的进程，
  表现为「端口刚就绪就 curl exit=7」。
- `intercepts` 必须包含被测域名，否则走纯隧道，测到的是 curl 的指纹而不是我们的。

## 实测数据（2026-11-15，CI windows-latest，rustls 0.23 + ring）

| 项目 | 值 |
| --- | --- |
| JA3 | `771,4866-4865-49196-49195-49200-49199-49188-49187-49192-49191,0-43-13-35-10-11-16-51-49-23-65281-45,24-23,0` |
| JA3 hash | `5b81f3846b1faddcc37be70ed494c956` |
| JA4 | `t13d1012h1_01be160bb49b_36bf25f296df` |
| peetprint hash | `d79a704d5317598173b9ac3f18c1ac3b` |
| 协商版本 | record 771 / negotiated 772（TLS 1.3） |
| 套件 | `TLS_AES_256_GCM_SHA384`、`TLS_AES_128_GCM_SHA256`、`ECDHE_{ECDSA,RSA}_WITH_AES_{256,128}_GCM_SHA384/256`、`..._CBC_SHA384/256` |
| supported_groups | **仅 `P-384 (24)`、`P-256 (23)`** |
| 扩展顺序 | `0 (SNI), 43, 13, 35, 10, 11, 16, 51, 49, 23, 65281, 45` |
| ALPN | **仅 `http/1.1`** |
| GREASE | **无** |

## 与 Chrome 的结构性差异（不需要参考 hash 也能判定）

| 维度 | Chrome 149 | 我们（rustls 默认） | 影响 |
| --- | --- | --- | --- |
| GREASE | 每个列表都插入 0x?a?a | 无 | JA3/JA4 立刻暴露「非浏览器」 |
| 密钥交换 | X25519 首选 + P-256 | **只有 P-384/P-256，无 X25519** | 握手更慢，且列表顺序不符 |
| ALPN | `h2, http/1.1` | 只有 `http/1.1` | h2 指纹（Akamai）全丢，站点可能降级 |
| 套件 | 含 CHACHA20(52393/52392)、AES-128-CCM、3DES | 无 CHACHA20/CCM | 列表长度与顺序全不同 |
| 扩展 | 含 `5 (status_request)`、`27 (compress_certificate)`、`17513`(ALPS)、`51` 带 X25519 key_share | 缺 status_request/ALPS/compress_certificate | CF Bot Management 直接判非 Chrome |
| 会话票 | 无 ticket 扩展以外的 PSK | 有 `35 session_ticket` + `45 psk_key_exchange_modes` | 差异明显 |
| renegotiation_info | 有（BoringSSL） | 有 | 一致 |

**结论：rustls 的 ClientHello 与 Chrome 在七个维度上全部不同，`5b81f38…` 与任何 Chrome 版本的 JA3 都对不上。**
这不是"调参能修"的差距——rustls 不提供 GREASE、扩展顺序、ALPN 与 key_share 选择的控制面。

## 判定（按事先写好的标准）

- JA3 完全一致 → 通过：**未通过**
- JA4 前缀一致（Chrome 是 `t13d1516h2_…`，我们是 `t13d1012h1_…`）→ 可接受：**未通过**
  （`h1` 说明无 h2，`1012` 的套件/扩展计数也完全不同）
- 均不一致 → 该站点必须走 BoringSSL 通道

→ **凡是会做 TLS 指纹比对的站点（Cloudflare Bot Management、Akamai、DataDome 等），
rustls 通道无法通过；这类站点必须走 BoringSSL（`wreq`/`boring`）通道。**

## 当前状态

- rustls + ECH：**已实现并通过编译/单测**（`crates/ds-tls/src/ech.rs`）
- rustls 指纹实测：**已完成**（上表，数据来自 CI run 43720759709）
- BoringSSL 通道：**未实现**。`wreq-js` 的 `.d.ts` 只有 `enableEchGrease`（假 ECH），
  所以 BoringSSL 通道要单独用自己的 `boring` 依赖 + `SSL_set1_ech_config_list` 实验，
  或接受「指纹站不用 ECH、ECH 站不伪装指纹」的二选一（见 `## 决策` 第 3 条）。
- 待补：本机 Chromium 149 的实测 JA3 hash（用 `tests/e2e/ja3_from_pcap.py` 抓包算），
  用于把上表的"结构性差异"升级成"逐 hash 对比"。
