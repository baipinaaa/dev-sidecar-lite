# 架构

## 一、三个进程，各管一件事

```
┌──────────────────────────────────────────────────────────────┐
│ dev-sidecar-lite.exe（Tauri 2 壳，Rust + WebView2）           │
│  · 只做桥接：invoke('api_invoke') → HTTP → core-host          │
│  · 只做表面：首页开关矩阵、九个子页、托盘菜单                  │
└───────────────┬──────────────────────────────┬───────────────┘
                │ POST /invoke                 │ GET /events (SSE)
                │ Bearer token                 │
┌───────────────▼──────────────────────────────▼───────────────┐
│ ds-core-host.exe（Node，SEA 打包）                            │
│  · 加载 core.api（配置、插件、系统代理、PAC 编排）             │
│  · 不碰数据：数据面由 core 自己拉起                            │
└───────────────┬──────────────────────────────────────────────┘
                │ spawn（stdin 控制 / stdout 状态 / stderr 日志）
┌───────────────▼──────────────────────────────────────────────┐
│ ds-proxy.exe（Rust，tokio）                                   │
│  · CONNECT 隧道、MITM 解密、直连转发                           │
│  · DNS 取址（DoH/DoT）、IP 评分、ECH、证书签发                │
│  · /pac.js 由同一进程提供                                      │
└──────────────────────────────────────────────────────────────┘
```

为什么不是两个进程：UI 需要一个**稳定**的控制面，而数据面会崩、会被杀、会重启。把
"配置与状态"留在 Node 里（原样复用），把"字节搬运"放进 Rust，两者通过 `running.json`
（启动参数）与 NDJSON（运行时控制）解耦，任何一边都能单独重启。

### 引擎切换不改 Node 源码

core 的 `server.start({ mitmproxyPath })` 已经把"拿哪个子进程当数据面"参数化。所以换引擎只需要
换入口，而不是改 core：

```
engine=node → mitmproxyPath = @blue-frontier/mitmproxy   （原样）
engine=rust → mitmproxyPath = core-host/shim/rust-engine.js
                                └─ spawn ds-proxy.exe，桥接 IPC ↔ stdin/stdout
```

垫片不碰数据流（字节全在 ds-proxy 里），只转发控制消息与状态；core 那边看到的仍是一个正常的
`fork` 子进程，`close` 依旧是发 IPC 消息。代价是多一个极轻的 Node 进程，好处是
**旧仓库零改动**，回退只需把 `proxy.engine` 改回 `node`。

## 二、一次请求经过什么

### 2.1 CONNECT（HTTPS）

```
浏览器 ──CONNECT steamcommunity.com:443──▶ ds-proxy
                                         │
                     ┌───────────────────┴────────────────────┐
                     │ 1. 解析请求头（http::read_head，保留 leftover）
                     │ 2. guard::bypass_reason：白名单 / excludeIpList / 内网 → 纯隧道
                     │ 3. rules::should_intercept(host)？
                     │      ├─ 否 → tunnel：200 + copy_bidirectional
                     │      └─ 是 → MITM
                     │ 4. dns::select：候选 IP 排序（EWMA × 失败惩罚 − 拒绝记忆）
                     │ 5. 逐个 try：TCP → (ECH? rustls with_ech : 普通 TLS) → HTTP/1.1
                     │ 6. 回 200，把 leftover 回灌给 TLS acceptor（PrefixedStream）
                     └────────────────────────────────────────┘
```

关键点：

- **leftover 必须保留**。浏览器把 CONNECT 与 ClientHello 放在同一个 TCP 段里是常态，丢掉它就会
  出现"200 已回、握手永远等不到"的经典死锁——Electron 版在 `tls-fragment` 重构里踩过一次。
- **MITM 边界在 `should_intercept`**，不在 TLS 层。没命中规则的域名一个字节都不解密。
- **失败要分类**（`ds_dns::failure::FailureReason`）：`TcpTimeout` 记在 IP 头上，
  `SniRejected` 记在域名头上，`Misdirected(421)` 记在"域名+路由"上。混在一起记会让好 IP
  被域名级问题连坐。

### 2.2 明文 HTTP

`http::forward` 把绝对 URI 改成 origin-form 再发上游；不做 MITM（本来就可读），
但仍会经过 `apply_request/apply_response` 做头改写（例如删 `alt-svc` 阻断 QUIC）。

## 三、配置的流向

```
~/.dev-sidecar/config.json（用户差异）
        +  remote_config.json5（远程缓存，可选）
        +  内置默认（ds_types 不负责，仍由 Node core 提供）
        ↓  Node core 合并
running.json（扁平结构，一次启动一份）
        ↓  --config
ds-proxy 用 ds_types::RuntimeConfig 反序列化
```

`RuntimeConfig` 的设计约定：

- 每个子结构都 `#[serde(rename_all = "camelCase", default)]`，缺字段一律取默认值——**配置比代码
  老**是常态，不能让一个没见过的键把整个代理打挂。
- 每个子结构都有 `#[serde(flatten)] extra: JsonMap`，未知键原样保留（将来要写回时不丢）。
- 布尔值用 `FlexBool`：Node 版历史上把 `"true"`、`1`、`null` 都当布尔用，直接 `bool` 会解不动。
- 域名通配（`*.x.com`）与正则（`^(.+.)?x.com$`）混合出现在同一个 map 的 key 里，
  `matcher::compile` 负责判别并编译，结果按容量 4096 的 LRU 缓存。

## 四、事件与控制

| 方向 | 通道 | 载荷 |
| --- | --- | --- |
| ds-proxy → core | stdout NDJSON | `{"id":n,"ok":true,"data":…}` 控制响应；`{"type":"status","value":…}` 状态 |
| core → core-host | `event.fire`（被宿主包了一层） | `status` / `speed` / `traffic` / `error` / `log` |
| core-host → UI | SSE `/events` | 同上，事件名即频道名 |
| UI → core-host | `POST /invoke` | `{api, param}`，`api` 与 Electron 版 `$api.xxx` 同名 |
| UI → ds-proxy | 经 `api.server.*` 间接控制 | 关闭走 `{cmd:"shutdown"}` 写 stdin |

为什么要包一层 `event.fire`：core 的 `event` 是个极简发布订阅（`register/fire/unregister`），
没有"旁路观察"能力。宿主启动时替换 `api.event.fire`，原实现照常执行，顺带广播给 SSE 订阅者——
这样不用改 core 的 20 多处 `fire` 调用点。

日志同理：core 的 `log` 是单例，包 `info/warn/error/debug` 四个方法即可。

## 五、几个刻意的取舍

| 决策 | 备选 | 为什么这么选 |
| --- | --- | --- |
| 手写 MITM，不用 hudsucker | hudsucker 0.25 | 依赖的 hyper 版本与我们锁定的不一致；手写 300 行可控，出问题好定位 |
| `ds-types` 全字段 + flatten | 只解需要的字段 | 配置要能原样写回，且新键不能崩 |
| 保留 Node 控制面 | 全部 Rust | 34k 行插件/规则语义重写风险远大于收益 |
| PAC 在 Rust 侧生成 | 继续 Node 生成 | PAC 与规则同源，减少一次跨进程数据搬运；且启动即可用 |
| 引擎默认 `auto` | 直接 `rust` | 找不到 `ds-proxy.exe` 时自动回退，安装包残缺也能启动 |
| 只监听回环 | 监听 0.0.0.0 | 这两个进程能改系统代理、装证书，暴露到局域网等于开后门 |

## 六、与 Node 版的差异（行为上）

| 场景 | Node 版 | Lite |
| --- | --- | --- |
| ECH | 自写 TLS1.3（无 H2、无 GREASE、ALPN 空） | rustls 原生 ECH，ALPN 可协商 h2 |
| 取址 | 预置 IP + 动态上游 + 单值串行切换 | 预置 IP + 动态上游 + EWMA 排序 + 并行拨号 |
| 失败记忆 | 域名级 successRate，30 秒拉黑 | IP 级失败计数 + 域名级 SNI 拒绝 + 路由级 421/403，各自 TTL |
| 并发 | 单进程事件循环 | tokio 多线程，每连接一个任务 |
| 配置热更新 | 重启整个 Electron 界面 | 重启 `ds-proxy` 子进程（UI 与 core 不动） |
| 托盘 | Electron | Tauri，逻辑与首页共用（托盘只发事件） |

## 七、还没做的

1. **指纹伪装**：`docs/FINGERPRINT.md` 的路线 B（BoringSSL / wreq）尚未接入。
2. **HTTP/2 上游**：`mitm.rs` 目前只做 HTTP/1.1，h2 站点会退化为直连隧道。
3. **QUIC 阻断**：只删 `alt-svc` 响应头，没有 UDP 层拦截（S302 用 WFP 驱动做这件事）。
4. **插件**（git/node/pip/xget/free-eye）：仍只在 Node 侧存在，UI 能看到开关，但 Rust 数据面不参与。
5. **编译验证**：见 README 的状态表。
