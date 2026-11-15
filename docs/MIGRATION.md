# 迁移对照表（JS → Rust）

本文件记录 Node 版 `dev-sidecar/packages/**` 每个关键模块在 `dev-sidecar-lite` 中的落点、
迁移状态与验收标准。**任何新增/删除模块都必须同步更新本表。**

路径基准：
- Node 版：`dev-sidecar/packages/`
- Rust 版：`dev-sidecar-lite/crates/`

## 一、控制面（保留 Node）

| Node 路径 | Rust/新落点 | 说明 |
| --- | --- | --- |
| `packages/core/src/index.js` | `core-host/`（Node SEA） | core 只 `module.exports = expose`，**没有 HTTP 服务**，必须新增 `POST /invoke` + `GET /events`(SSE) |
| `packages/core/src/expose.js` | 不变 | 启动/停止编排，改为通过 `engine` 开关决定委托给谁 |
| `packages/core/src/modules/server/index.js` | `ds-ctl`（进程管理）+ `core-host` | `fork(mitmproxy.js)` → `core-host` 把 `mitmproxyPath` 换成 `shim/rust-engine.js`，由它 spawn `ds-proxy.exe`（**不改 Node core**） |
| `packages/core/src/config/index.js` | `ds-types::config` | 137 个 intercepts 域名、89 条 dns.mapping、77 条 preSetIpList 全部映射 |
| `packages/core/src/config-api.js` | 不变 | 远程配置已改为手动 |
| `packages/core/src/status.js` / `expose.js` | 不变 | 状态事件经 SSE 推给 Tauri |
| `packages/gui/src/**` | `app/`（Tauri 2 + Vue 3） | 9 个页面重写，见 `docs/ARCHITECTURE.md` |

## 二、数据面（Rust 替换）

### 2.1 入口与生命周期

| Node 路径 | Rust 落点 | 状态 | 验收 |
| --- | --- | --- | --- |
| `packages/mitmproxy/src/index.js` | `ds-proxy/src/main.rs` | 步骤 5 | `ds-proxy <running.json>` 能起监听 |
| `packages/mitmproxy/src/options.js` | `ds-rules::options` | 步骤 13 | 拦截判定顺序一致：whiteList → intercepts → echDomain |
| `packages/gui/src/bridge/mitmproxy.js` | 删除 | — | 由 `ds-proxy --config` 取代 |

### 2.2 连接处理

| Node 路径 | Rust 落点 | 状态 | 验收 |
| --- | --- | --- | --- |
| `proxy/mitmproxy/createConnectHandler.js` | `ds-proxy::connect` | 步骤 5 | CONNECT 200 后双向转发不断流（Node 版曾因 `tlsFragment` ReferenceError 全断） |
| `proxy/mitmproxy/createRequestHandler.js` | `ds-proxy::request` | 步骤 5 | 明文 HTTP 200 |
| `proxy/mitmproxy/createUpgradeHandler.js` | `ds-proxy::upgrade` | 步骤 5 | WebSocket 101 |
| `proxy/mitmproxy/dnsLookup.js` | `ds-dns::lookup` | 步骤 9 | `all:true` 返回完整候选数组（Happy Eyeballs 前提） |
| `proxy/common/util.js` | `ds-proxy::util` | 步骤 5 | keep-alive、超时映射 |
| `proxy/common/tls-fragment.js` | `ds-proxy::fragment` | 步骤 13 | head 非空时立即发送（Node 版曾白等 2 秒） |
| `proxy/tls/FakeServersCenter.js` | `ds-proxy::fake` | 步骤 5 | 命中的响应缓存命中率 |

### 2.3 DNS / 取址 / 选路

| Node 路径 | Rust 落点 | 状态 | 验收 |
| --- | --- | --- | --- |
| `lib/dns/base.js` | `ds-dns::base` | 步骤 9 | `_lookupWithPreSetIpList` + `dynamicUpstreams` 合并顺序一致 |
| `lib/dns/index.js` | `ds-dns::registry` | 步骤 9 | `lookupDomainEch` 同源优先 + 5 秒预算 + 冷却 |
| `lib/dns/https.js` | `ds-dns::doh` | 步骤 9 | h2 优先、h1 回退；解析 SVCB 拿 ECHConfig |
| `lib/dns/tls.js` `tcp.js` `udp.js` | `ds-dns::{dot,tcp,udp}` | 步骤 9 | `tls://` / `udp://` 提供方可用 |
| `lib/dns/preset.js` | `ds-dns::preset` | 步骤 9 | 预置未命中返回空，不回落系统 DNS |
| `lib/dns/nat64.js` | `ds-dns::nat64` | 步骤 9 | 前缀合成 |
| `lib/speed/SpeedTester.js` | `ds-dns::speed` | 步骤 14 | TCP 443 计时 + HTTP 状态码可辨（区分 429） |
| `lib/choice/index.js` | `ds-dns::choice` | 步骤 14 | EWMA 延迟 × 失败惩罚排序；30 秒拉黑、翻倍上限 5 分钟 |

### 2.4 TLS / ECH / 证书

| Node 路径 | Rust 落点 | 状态 | 验收 |
| --- | --- | --- | --- |
| `lib/proxy/common/ech/EchHttpsAgent.js` | `ds-tls::ech` | 步骤 10 | rustls `with_ech` 打通，`DS-ECH: 1` |
| `lib/proxy/common/ech/tls13.js` | 删除 | 步骤 10 | 自写 TLS 栈由 rustls 取代（原实现无 h2/GREASE） |
| `lib/proxy/common/ech/hpke.js` | 删除 | 步骤 10 | rustls 内置 HPKE |
| `lib/proxy/common/ech/ech.js` | `ds-tls::ech_config` | 步骤 10 | ECHConfigList 解析 + `retry_configs` |
| `lib/proxy/common/ech/domain.js` | `ds-types::matcher` | 步骤 2 ✅ | glob/正则语义一致 |
| `packages/core/src/modules/cert/*` | `ds-tls::ca` | 步骤 6 | 复用 `~/.dev-sidecar/dev-sidecar.ca.crt` 与 `.key.pem` |
| `packages/core/.../certstore/*` | `ds-tls::store` | 步骤 6 | 叶子证书动态签发 + 缓存 |

### 2.5 规则与改写

| Node 路径 | Rust 落点 | 状态 | 验收 |
| --- | --- | --- | --- |
| `lib/interceptor/impl/req/sni.js` | `ds-rules::sni` | 步骤 13 | `verifyHost` + 不校验证书 |
| `lib/interceptor/impl/res/responseReplace.js` | `ds-rules::replace` | 步骤 13 | `[remove]` 头删除（alt-svc 24 处） |
| `lib/interceptor/impl/req/requestReplace.js` | `ds-rules::replace` | 步骤 13 | 请求头/体重写 |
| `lib/interceptor/impl/res/script.js` | `ds-rules::script` | 步骤 13 | 本定制版已禁用远程注入，仅保留本地脚本 |
| `lib/interceptor/impl/redirect.js` | `ds-rules::redirect` | 步骤 13 | 301/302 |
| `lib/interceptor/impl/cache.js` | `ds-rules::cache` | 步骤 13 | `cacheDays` 磁盘缓存 |
| `lib/interceptor/impl/*`（其余） | `ds-rules::*` | 步骤 13 | 行为对齐 |

### 2.6 PAC

| Node 路径 | Rust 落点 | 状态 | 验收 |
| --- | --- | --- | --- |
| `lib/pac.js` | `ds-pac` | 步骤 15 | 336 条代理 / 110 条直连逐条一致；`*.x` 覆盖根域 |
| `packages/core/.../set-system-proxy/index.js` | `core-host`（保留） | — | 仍用 `sysproxy.exe pac <url>`（reg add 会被广播冲掉） |

### 2.7 观测

| Node 路径 | Rust 落点 | 状态 | 验收 |
| --- | --- | --- | --- |
| `lib/traffic/TrafficMonitor.js` | `ds-ctl::traffic` | 步骤 7 | 连接列表/流量统计经 NDJSON 上报 |
| `lib/logstream/*` | `ds-ctl::logs` | 步骤 7 | 环形缓冲 + 脱敏正则 |
| `packages/mitmproxy/src/utils/util.redact.js` | `ds-ctl::redact` | 步骤 7 | Bearer/token/cookie 脱敏 |

## 三、命令行/控制口

| Node 路径 | Rust 落点 | 状态 | 验收 |
| --- | --- | --- | --- |
| `packages/mitmproxy/src/lib/...`（控制口） | `ds-ctl` | 步骤 7 | `close` / `getList` / `reTest` 三个命令 |
| — | `ds-ctl --stdio` | 步骤 7 | NDJSON over stdio，供 `core-host` 调用 |

## 四、已知必须修的 Node 版缺陷（迁移时一并修正）

1. `createConnectHandler` 的 `tlsFragment` 作用域 bug（已修，但说明 JS 闭包陷阱）。
2. `tls-fragment` 的 `head` 白等 2 秒（已修）。
3. `respondLookup(all)` 只回单地址（已修）。
4. `setBackupList` 每次重置当前 IP（已修）。
5. ECH 兜底串行 72 秒（已修）。
6. `excludeIpList` 的 `null` 被当作规则（PAC 侧已修）。
7. `reg add` + 广播会把 PAC 配置冲掉（改用 `sysproxy.exe pac`）。

## 五、当前进度

| 阶段 | 步骤 | 状态 |
| --- | --- | --- |
| 0 | 1–4 | 进行中（1 ✅、2 ✅、3 本文件、4 CI） |
| 1 | 5–8 | 未开始 |
| 2 | 9–12 | 未开始 |
| 3 | 13–16 | 未开始 |
| 4 | 17–21 | 未开始 |
| 5 | 22–26 | 未开始 |
