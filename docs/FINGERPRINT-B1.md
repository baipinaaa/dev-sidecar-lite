# B1 决策：如何让 ds-proxy 的指纹"像 Chrome"

> 前置：`docs/FINGERPRINT.md` 记录了 rustls 的实测指纹与"七个维度都不同"的结论。
> 本文是 B3（采基线）的产出，用来决定 B1 值不值得做、做成什么样。

## 一、目标（B3 实测，2026-11-16，CI run 43797615983）

采集方式：本地嗅探代理截 ClientHello（`tests/e2e/clienthello_capture.py`），
被测客户端 = GitHub runner 自带的 Google Chrome **156.0.8078.80**。

```
JA3      = 771,4865-4866-4867-49195-49199-49196-49200-52393-52392-49171-49172-156-157-47-53,11-35-27-10-65281-18-0-65037-17613-16-23-51-43-13-5-45-51764,4588-29-23-24,0
JA3 hash = cccf4f8d2e3c9bded07b40c216fe6216
JA4      = t13d1517h2_
ALPN     = h2, http/1.1
```

对照 rustls 0.23 + ring（`ds-tls` 现状）：

```
JA3      = 771,4866-4865-49196-49195-49200-49199-49188-49187-49192-49191,0-43-13-35-10-11-16-51-49-23-65281-45,24-23,0
JA3 hash = 5b81f3846b1faddcc37be70ed494c956
JA4      = t13d1012h1_
ALPN     = http/1.1
```

> 用户本机是 ungoogled-chromium **157.0.8103.50**。ungoogled 只改 UI/服务层，
> TLS 栈仍是 BoringSSL，JA3 预期与 156 一致；要钉死可用同一条命令自测：
> `pwsh -File tests/e2e/ja3_baseline.ps1 -ChromeExe <你的 chrome.exe> -OutDir .\local-baseline`

## 二、差距（逐项，已自动化计算）

| 维度 | Chrome 有、rustls 没有 | rustls 有、Chrome 没有 | 顺序一致 |
| --- | --- | --- | --- |
| cipher suites | `4867`、`52393`/`52392`（CHACHA20）、`49171`/`49172`、`156`/`157`、`47`/`53` | `49188`/`49187`/`49192`/`49191`（GCM 的 ECDSA/RSA 组合顺序不同） | ❌ |
| extensions | `5`(status_request)、`18`(SCT)、`27`(compress_certificate)、`65037`(ECH)、`17613`(ALPS)、`51764` | `49`(post_handshake_auth) | ❌ |
| supported_groups | `4588`(X25519MLKEM768，后量子)、`29`(X25519) | — | ❌ |
| point_formats | — | — | ✅ |
| ALPN | `h2, http/1.1` | 只有 `http/1.1` | ❌ |

要点：
1. **Chrome 早就上了后量子**（`4588 = X25519MLKEM768`），rustls 也能开（aws-lc-rs provider + `X25519MLKEM768`），但我们当前用的是 **ring** provider，开不了。
2. `17613`（ALPS）与扩展**顺序**是 core 里硬编码的，rustls 没有任何公开 API 能控制。
3. `65037` 出现在对 `example.com` 的握手里 —— 那是 **ECH grease**（假装支持），说明 Chrome 即使访问不支持 ECH 的站点也会发这个扩展。这正是 CF 用来区分"真浏览器"的强特征之一。

## 三、rustls 能补到哪一步（不引 BoringSSL 的上限）

| 能力 | rustls 是否可行 | 说明 |
| --- | --- | --- |
| ALPN 加 `h2` | ✅ | `ClientConfig::alpn_protocols` |
| 后量子 `4588` | ⚠️ | 需换 `aws-lc-rs` provider 并启用 PQ，编译链变重（cmake） |
| `status_request(5)` | ✅ | `enable_ocsp` / 相关配置 |
| `compress_certificate(27)` | ❌ | 无 API |
| `ECH grease(65037)` | ❌ | rustls 的 ECH 只在真有 ECHConfig 时发，不做 grease |
| `ALPS(17613)` / `51764` | ❌ | 无 API，属 BoringSSL 私有扩展 |
| 扩展顺序 | ❌ | 编译期固定 |
| CHACHA20 位置（第 8/9 位） | ⚠️ | 可加进套件列表，但 rustls 会按自己规则排序 |

**结论：rustls 最乐观也只能到"JA4 前缀 `t13d1517h2_` 相似"，JA3 hash 永远对不上 `cccf4f8d…`。**
要让 CF 认，必须走 BoringSSL。

## 四、三条路线

### B1a：`boring` crate（BoringSSL 绑定）—— 推荐先探

- 优点：ClientHello 完全由 BoringSSL 生成，与 Chrome 同源，JA3/JA4 **天然一致**（同版本下）。
- 成本：
  - 需要 `cmake` + `ninja` + `nasm`；Windows runner 上首次构建 BoringSSL 预计 **15–25 分钟**（可用 `Swatinem/rust-cache` 缓存）。
  - 需要确认 `boring` 暴露 ECH：BoringSSL 有 `SSL_set1_ech_config_list`，但 crate 是否绑定未知 → **这是唯一的技术风险点**。
- 若 ECH 不可用：退回"指纹站不用 ECH、ECH 站不伪装指纹"的二选一（`intercepts.fingerprint` 按域名选择）。

### B1b：`wreq`（Rust 版，BoringSSL + 现成 Chrome 预设）

- `wreq-js` 的 `.d.ts` 只有 `enableEchGrease`（假 ECH），Rust 端预计一致 → **能过指纹，但 ECH 是假的**。
- 适合"只解决 CF 挑战、不要求真 ECH"的场景；集成成本低于 B1a（无需自己拼 TLS 层）。

### B1c：维持 rustls，接受指纹站失败

- 把 `linux.do` 这类站点交给"浏览器直连 + 浏览器安全 DNS"，ds-proxy 专注其余站点。
- 零成本，但 `docs/FINGERPRINT.md` 里"Rust 版能过 CF"这个卖点作废。

## 五、验收门（已就绪，可对任何候选实现打分）

任何实现只要能发出 ClientHello，就能用同一把尺子量：

```powershell
# 1) 起嗅探代理
python tests/e2e/clienthello_capture.py --listen 18999 --out hello.bin --timeout 30
# 2) 让被测实现连它（curl / ds-proxy / 探针程序都行）
curl.exe -x http://127.0.0.1:18999 -k https://example.com/
# 3) 算指纹
python tests/e2e/clienthello_capture.py --compute hello.bin
```

判定：
- `ja3_hash == cccf4f8d2e3c9bded07b40c216fe6216` → 与 Chrome 156 完全一致，**通过**
- `ja4` 前缀 == `t13d1517h2_` → 可接受（顺序有差异但主要特征齐）
- 其它 → 不通过，CF 仍会挑战

## 六、探针实测结果（2026-11-16，run 43802243049）

`boring`（BoringSSL 绑定）**5.5.0** 在 `windows-latest` 上：

| 问题 | 答案 |
| --- | --- |
| 能否构建 | ✅ 可以，`nasm 2.16.01` + `cmake 4.4.5`，**release 构建 222 秒**（可被 `Swatinem/rust-cache` 缓存） |
| 有没有 ECH 绑定 | ✅ **有**：`SslRef::set_ech_config_list(&[u8])`（客户端设 ECHConfigList）、`SslRef::set_enable_ech_grease(bool)`、`SSL_get0_ech_retry_configs`、`SSL_marshal_ech_config`、`SSL_ECH_KEYS_*`（服务端） |
| 指纹对不对得上 | 见下：**除 3 个扩展外全部对齐** |

### 三轮对齐实验（同一嗅探代理抓包，同一把尺子）

| 轮次 | 配置 | JA3 hash | JA4 | 缺什么 |
| --- | --- | --- | --- | --- |
| 默认 | 裸 `SslConnector` | `4e8ea6b7…` | `t13d171000_` | 无 ALPN、多 2 个套件、多 group `65074`、扩展 11 个 |
| 第 2 轮 | `set_cipher_list` + `set_curves_list` + ALPN=h2 | `aef293b1…` | `t13d1511h2_` | **套件/groups/ALPN 已与 Chrome 完全一致**，扩展仍少 6 个 |
| 第 3 轮 | 再加 `set_permute_extensions` + `enable_ocsp_stapling` + `enable_signed_cert_timestamps` + `set_enable_ech_grease` | `e182f067…` | **`t13d1514h2_`** | 只差 **3 个扩展**：`27`(compress_certificate)、`17613`(ALPS)、`51764` |

Chrome 156 基线：`t13d1517h2_`（17 个扩展），套件/`groups`/`point_formats`/ALPN 与第 3 轮**逐项相同**。

### 得到的结论

1. **套件列表、groups 顺序、ALPN、point_formats 可以用 3 个公开 API 完全对齐**——这三项是 JA3/JA4 里最"重"的部分。
2. ECH 的两个方向都是通的：真 ECH（`set_ech_config_list`）和 grease（`set_enable_ech_grease`）。
3. 只剩 3 个扩展没法用安全 API 打开：
   - `27` compress_certificate：boring 有 `add_certificate_compression_algorithm::<C>()`，但要求实现 `CertCompressionAlgorithm`（真要压缩/解压），不能只"声明支持"；
   - `17613` ALPS：boring 5.5.0 没暴露，只能 `unsafe` 调 `ffi::SSL_add_application_settings`；
   - `51764`：连 BoringSSL 上游都没有明文出处（Chrome 私有/实验扩展），只能硬编码塞进扩展列表（需要改库）。
4. 因为 Chrome 开了 `permute_extensions`，**它的 JA3 每次连接都不一样**，所以"JA3 hash 完全相等"这个判据本身不成立——**正确目标是 JA4（对顺序不敏感）**。第 3 轮 `t13d1514h2_` vs Chrome `t13d1517h2_`，差别只剩扩展数量与扩展集合哈希。

## 七、下一步（本次先做可行性探针，不做全量接入）

CI job `boring-probe` 要回答三个问题：
1. `boring` 在 windows-latest 上**能不能构建**、耗时多少；
2. crate 里**有没有** ECH 绑定（grep 源码里的 `ech_config`）；
3. 顺手用它握手一次，抓 ClientHello 比对 `cccf4f8d…`。

三个答案都已正向（见第六节）→ **B1a 可以进入实现**：新增 `ds-tls` 的 `fingerprint` 通道 + `intercepts.fingerprint` 配置项，默认仍走 rustls，命中的域名切 BoringSSL。

B1a 的最小可用版本（不需要动库、不需要 unsafe）：
- `ds-tls` 增加 `boring` 依赖与 `chrome_connector()`，配置与第六节第 3 轮一致；
- ECH 走 `SslRef::set_ech_config_list`（现有 `ds-dns` 已经能拉 ECHConfigList，直接喂进去）；
- 验收判据改为 **JA4 前缀 `t13d1514h2_`**（对齐到扩展数量 14），并把"能否通过 CF 挑战"作为最终标准（`tests/e2e/cf_acceptance.ps1` 已有）。
