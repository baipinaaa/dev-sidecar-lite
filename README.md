# DevSidecar Lite

把 [dev-sidecar](https://github.com/docmirror/dev-sidecar) 的**数据面**从 Node 换成 Rust、
**展示面**从 Electron 换成 Tauri 2 的重构版。控制面（配置、插件、规则语义）继续沿用原来的
Node 实现，不重写。

## 当前状态

> **骨架完成，未经过编译与端到端验证。**
>
> 开发机上没有 Rust 工具链，所有 `.rs` 文件只做了括号配平与人工复核；JS 侧（`core-host`）
> 有可运行的冒烟测试。首次 `cargo check` 预计需要修一批 API 签名（rcgen / rustls 的版本细节，
> 相关位置在代码注释里已标出）。

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| 0 | workspace、7 个 crate 骨架、配置映射、CI | ✅ |
| 1 | 纯隧道代理 + MITM 证书 + 控制口 IPC | ✅ 待编译 |
| 2 | DoH/DoT + SVCB/ECH + 指纹路线决策 | ✅ 已编译并实测 |
| 2b | Chrome 指纹通道（BoringSSL，JA4 `t13d1517h2_`） | ✅ 已实现，待 CI 自检确认 |
| 3 | 规则引擎 + IP 评分 + PAC | ✅ 待编译 |
| 4 | Tauri UI 九页面 + 托盘 + sidecar | ✅ 前端完成 |
| 5 | 引擎切换、旧版迁移、基准、发版 | ✅ 待实测 |

## 为什么值得这么做

Electron + Node 版能跑，但有三个结构性问题：

1. **每字节都要过一次用户态 JS**：8 并发实测 CPU 峰值 **228%**（两个核满），而上游只给 6MB/s。
2. **ECH 只能靠自写的 TLS 1.3 栈**（2288 行 JS）：没有 HTTP/2、没有 GREASE、ALPN 为空，
   Cloudflare 一眼看出"声称 Chrome 149 但握手不是 Chrome"，于是反复触发人机验证。
3. **UI 与数据面耦合在同一个 Electron 进程树里**：改一个开关要重启整个渲染进程。

Rust 数据面 + Tauri 壳分别解决这三点：rustls 原生 ECH/H2、tokio 多线程字节搬运、UI 只做桥接。

## 目录

```
dev-sidecar-lite/
├─ crates/
│  ├─ ds-types/    # running.json 的完整映射 + 通配符/正则匹配
│  ├─ ds-rules/    # intercepts 语义、白名单/排除、头改写
│  ├─ ds-dns/      # DoH/DoT/UDP + SVCB(ECH) + IP 评分 + 失败分类 + 拒绝记忆
│  ├─ ds-tls/      # rcgen 动态签证书、rustls 客户端、ECH 配置
│  ├─ ds-proxy/    # 代理主进程：CONNECT / MITM / 直连转发 / 控制口
│  ├─ ds-pac/      # PAC 生成（与 Node 版逐条对齐）
│  └─ ds-ctl/      # NDJSON 控制协议 + 连接注册表
├─ core-host/      # Node 控制面宿主：core.api → HTTP + SSE
├─ app/            # Tauri 2 + Vue 3 界面
├─ docs/           # 架构、迁移、指纹、基准
├─ tests/e2e/      # 冒烟、一致性、CF 验收、基准、JA3 解析
└─ .github/workflows/{ci,release}.yml
```

## 快速开始

```powershell
# 1) 只跑数据面（需要先有一份 running.json，Electron 版跑过一次就有）
cargo run --release -p ds-proxy -- --config "$env:USERPROFILE\.dev-sidecar\running.json" --port 31182
curl.exe -x http://127.0.0.1:31182 --ssl-no-revoke https://www.baidu.com/ -o NUL -w "%{http_code} %{time_total}`n"

# 2) 控制面
$env:DS_CORE_PATH = "..\dev-sidecar\packages\core"
node core-host/src/main.js --port 31190 -v

# 3) 界面（浏览器模式，走 mock）
cd app; npm install; npm run dev

# 4) 完整桌面版
cd app; npm run tauri:dev
```

详细步骤见 [`app/README.md`](app/README.md)，从旧版迁移见
[`docs/MIGRATE-FROM-ELECTRON.md`](docs/MIGRATE-FROM-ELECTRON.md)。

## 设计边界（重要）

- **不做锁区/年龄门绕过**：S302 那套"作者服务器中转"的方案不在本项目范围内。
- **不内建任何远程配置/脚本注入**：所有规则来自本地 `running.json`。
- **默认只监听回环**：`ds-proxy` 与 `core-host` 都绑 `127.0.0.1`；`core-host` 改系统代理，绝不暴露到局域网。
- **引擎可回退**：`config.proxy.engine = "node"` 随时回到原实现，不需要改安装包。
- **没有自动更新**：安装包不带 updater（无签名通道、无静默升级），换版本只能手动下载覆盖安装。

## 已知风险

- 未编译验证（见上文状态表）。
- `hudsucker` 未采用，MITM 是手写的（`crates/ds-proxy/src/mitm.rs`），覆盖面比成熟库窄：
  目前只处理 HTTP/1.1，HTTP/2 上游会退化为直连隧道。
- 指纹伪装（JA3/JA4 与 Chrome 一致）**已实现但需显式开启**：`intercepts` 里写
  `"fingerprint": "chrome"` 的域名走 BoringSSL 通道（`crates/ds-tls/src/chrome.rs`），
  其余域名仍走 rustls。rustls 通道的 JA3 已在 CI 实测为
  `5b81f3846b1faddcc37be70ed494c956`，与 Chrome 在 GREASE / X25519 / ALPN h2 / 套件集等
  七个维度全不同，所以**会做 TLS 指纹比对的站点必须走 BoringSSL 通道**。详见 [`docs/FINGERPRINT.md`](docs/FINGERPRINT.md)。
- 性能数据只测了 Node 侧基线，Rust 侧待填，见 [`docs/BENCHMARK.md`](docs/BENCHMARK.md)。

## 致谢

- [docmirror/dev-sidecar](https://github.com/docmirror/dev-sidecar)：配置语义、插件与规则体系的来源
- [Hana-ame/ech-proxy](https://github.com/Hana-ame/ech-proxy)：PAC 分流的思路
- [NLick47/hakimi](https://github.com/NLick47/hakimi)：IP 评分模型、失败分类、路由级拒绝记忆
- [rustls](https://github.com/rustls/rustls)：客户端 ECH 实现
- [rcgen](https://github.com/rustls/rcgen)、[tokio](https://github.com/tokio-rs/tokio)、[Tauri](https://github.com/tauri-apps/tauri)

## 许可

MIT。原项目 dev-sidecar 亦为 MIT。
