# DevSidecar Lite

把 [dev-sidecar](https://github.com/docmirror/dev-sidecar) 从 **Electron** 搬到 **Tauri 2** 的版本。
数据面与控制面沿用原版成熟的 Node 实现，Rust 侧保留一条可切换的实验数据面。

- 安装包 **32 MB**（原版 Electron 版 100 MB 上下），不带 Electron、不带自动更新
- 界面是原版 GUI 的移植（同一套 Vue 页面、同一套配置语义、同一套拦截规则）
- 默认数据面是 **Node**，与原版行为一致；Rust 数据面需在设置里把 `proxy.engine` 改成 `rust`

## 当前状态（v0.2.0）

| 部分 | 实现 | 状态 |
| --- | --- | --- |
| 控制面宿主 | `core-host/`（Node SEA 单文件 exe） | ✅ 安装包实测：`getApiList` 124 个接口、`/healthz`、`/invoke`、SSE 全通 |
| 数据面（默认） | 原版 `@blue-frontier/mitmproxy`，`core-bundle/` 内随包下发 | ✅ 实测 `/pac.js` 200、CONNECT + MITM 解密、按规则直连/走代理 |
| 界面 | 原版 `packages/gui` 的 Vue 页面移植到 Tauri 2 | ✅ `vite build` 通过，3242 modules |
| Rust 数据面（实验） | `crates/ds-proxy` 等 7 个 crate | ⚠️ CI `cargo build/clippy/test` 全绿，**无端到端验证**，见「已知限制」 |
| 安装包 | NSIS（perMachine）+ 静默安装冒烟 | ✅ 32.4 MB，冒烟覆盖「装→布局→healthz→getApiList→起数据面→/pac.js→卸载」 |

`core-bundle/` 是运行时目录，不打进 SEA：里面是 Node core + mitmproxy + `extra/`（`sysproxy.exe`、
证书脚本、PAC），SEA 的 `require()` 只能加载内置模块，所以这些必须留在磁盘上。

## 安装

从 [Releases](https://github.com/baipinaaa/dev-sidecar-lite/releases) 下载 `*_x64-setup.exe`，
右键以管理员身份安装（默认装到 `C:\Program Files\DevSidecar Lite`）。

- 首次开启「系统代理」或「PAC 分流」时会提示安装根证书，按提示点「是」
- 从 Electron 版迁移见 [`docs/MIGRATE-FROM-ELECTRON.md`](docs/MIGRATE-FROM-ELECTRON.md)：
  两个版本会争抢 `127.0.0.1:31181`，先卸载旧版或在旧版里关掉「系统代理」
- 没有自动更新：换版本手动下载覆盖安装

## 架构

```
DevSidecar Lite.exe        Tauri 2 外壳（WebView2 + 原版 Vue 页面）
  └─ ds-core-host.exe      Node SEA：加载 core-bundle，暴露 HTTP /invoke + SSE /events
       ├─ core-bundle/node_modules/@blue-frontier/dev-sidecar    控制面（配置/插件/规则/PAC/证书）
       ├─ core-bundle/node_modules/@blue-frontier/mitmproxy      数据面（fork 出子进程）
       ├─ core-bundle/extra/                                      sysproxy.exe、证书脚本、PAC
       └─ core-bundle/core-host/mitmproxy-bridge.cjs              fork 目标（注入 rootDir）
  └─ (可选) ds-proxy.exe   Rust 实验数据面，engine=rust 时才启动
```

界面 → `ds-core-host` → `core.api` 这条链路和原版一致，所以插件、`config.json` 语义、
`intercepts` / `dns.mapping` / `preSetIpList` 等规则全部照旧生效。

### 与 Electron 版的差异

| | Electron 版 | 本版 |
| --- | --- | --- |
| 外壳 | Electron 42（Chromium + Node 双份运行时） | Tauri 2（系统 WebView2） |
| 控制面 | Electron 主进程内 | `ds-core-host.exe`（Node SEA）独立进程 |
| 界面通信 | `ipcRenderer.invoke('apiInvoke')` | HTTP `POST /invoke` |
| 状态推送 | `ipcRenderer.on('status'/'speed'/...)` | SSE `/events`（`core-event` 再按 `channel` 分发） |
| 系统能力 | `electron.shell` / `electron-dialog` | `tauri-plugin-opener` / `tauri-plugin-dialog` |
| 自动更新 | electron-updater | 无（手动下载） |
| 抓包诊断 | 内建 CDP + `<webview>` | 变通方案：用系统浏览器打开 + 复制代理地址 |

## 开发

```powershell
# 控制面（用仓库里的 dev-sidecar 源码，不装包）
$env:DS_CORE_PATH = "..\dev-sidecar\packages\core"
$env:DS_EXTRA_PATH = "..\dev-sidecar\packages\gui\extra"
node core-host/src/main.js --port 31190 -v

# 界面
cd app; npm install; npm run tauri:dev

# 组装 core-bundle（CI 里同一条命令）
node scripts/prepare-core-bundle.mjs --dev-sidecar ..\dev-sidecar --max-mb 70
```

Rust 侧（需要 MSVC 工具链 + NASM）：

```powershell
cargo build --release --workspace
node core-host/scripts/build-sea.js     # 需要 postject
pwsh -File scripts/prepare-binaries.ps1 -Strict
```

## 已知限制

**Rust 实验数据面**（`proxy.engine = rust`）目前只适合看热闹：

1. MITM 只处理 HTTP/1.1，HTTP/2 上游退化为直连隧道
2. 请求/响应改写规则（`intercepts` 的 `reqReplace` / `resReplace` / `headerReplace`）尚未接入
3. 不输出 `status` / `speed` / `traffic` 事件，界面上的速度与流量是空的
4. 不支持 Chrome 指纹通道（`fingerprint: chrome`），JA3 与 Chrome 不同

要替代原版数据面，还差「拦截器体系 + 规则改写 + 指纹通道」三块，工作量远大于壳的改造，
所以默认不启用。原版 Node 数据面能跑的地域，两者行为一致。

**其它**

- 只支持 Windows x64；托盘图标与全局快捷键未移植
- 安装包未签名：SmartScreen 会提示「未知发布者」，点「更多信息 → 仍要运行」
- `app/src-tauri/icons/` 需要提交进仓库（tauri-build 在编译期就要读 `icon.ico`，
  被 `.gitignore` 忽略过一次，CI 直接红在 Windows Resource 那一步）

## 设计边界

- **不做锁区/年龄门绕过**：作者服务器中转那套方案不在本项目范围内
- **不内建远程脚本注入**：所有规则来自本地 `config.json`
- **只监听回环**：`ds-core-host` 与 `ds-proxy` 都绑 `127.0.0.1`
- **引擎可回退**：`proxy.engine` 随时切回 `node`

## 致谢

- [docmirror/dev-sidecar](https://github.com/docmirror/dev-sidecar)：配置语义、插件与规则体系的来源
- [Hana-ame/ech-proxy](https://github.com/Hana-ame/ech-proxy)：PAC 分流的思路
- [NLick47/hakimi](https://github.com/NLick47/hakimi)：IP 评分模型、失败分类、路由级拒绝记忆
- [rustls](https://github.com/rustls/rustls)、[rcgen](https://github.com/rustls/rcgen)、
  [tokio](https://github.com/tokio-rs/tokio)、[Tauri](https://github.com/tauri-apps/tauri)

## 许可

MIT。原项目 dev-sidecar 亦为 MIT。
