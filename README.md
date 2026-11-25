# DevSidecar Lite

把 [dev-sidecar](https://github.com/docmirror/dev-sidecar) 从 **Electron** 搬到 **Tauri 2** 的版本。
数据面与控制面沿用原版成熟的 Node 实现（`@blue-frontier/mitmproxy` + `@blue-frontier/dev-sidecar`），
本项目只替换外壳与进程模型。

- 安装包 **32 MB**（原版 Electron 版 100 MB 上下），不带 Electron、不带自动更新
- 界面是原版 GUI 的移植（同一套 Vue 页面、同一套配置语义、同一套拦截规则）
- 数据面是 Node，与原版行为一致：DNS 取址、预置 IP、规则改写、脚本注入、PAC 全部照旧

## 当前状态（v0.3.0）

| 部分 | 实现 | 状态 |
| --- | --- | --- |
| 控制面宿主 | `core-host/`（随包 `node.exe` + 磁盘上的 js，stdio JSON-RPC） | ✅ CI 安装冒烟：`ready` 帧、`getApiList` 124 个接口、数据面 `/pac.js` 200 |
| 数据面 | 原版 `@blue-frontier/mitmproxy`，`core-bundle/` 内随包下发 | ✅ 实测 `/pac.js` 200、CONNECT + MITM 解密、按规则直连/走代理 |
| 界面 | 原版 `packages/gui` 的 Vue 页面移植到 Tauri 2 | ✅ `vite build` 通过 |
| 安装包 | NSIS（perMachine）+ 静默安装冒烟 | ✅ 冒烟覆盖「装→布局→healthz→getApiList→起数据面→/pac.js→卸载」 |

`core-bundle/` 是运行时目录：里面是 Node core + mitmproxy + `extra/`（`sysproxy.exe`、
证书脚本、PAC），必须留在磁盘上（数据面是 `fork` 出来的子进程，不是单文件）。

## 安装

从 [Releases](https://github.com/baipinaaa/dev-sidecar-lite/releases) 下载 `*_x64-setup.exe`，
右键以管理员身份安装（默认装到 `C:\Program Files\DevSidecar Lite`）。

- 首次开启「系统代理」或「PAC 分流」时会提示安装根证书，按提示点「是」
- 从 Electron 版迁移见 [`docs/MIGRATE-FROM-ELECTRON.md`](docs/MIGRATE-FROM-ELECTRON.md)：
  两个版本会争抢 `127.0.0.1:31181`，先卸载旧版或在旧版里关掉「系统代理」
- 配置目录是 `C:\Users\<你>\.dev-sidecar-lite`（与原版的 `~/.dev-sidecar` 分开，两者可共存）
- 没有自动更新：换版本手动下载覆盖安装

## 架构

```
dev-sidecar-lite.exe       Tauri 2 外壳（WebView2 + 原版 Vue 页面）
  └─ node.exe              core-bundle/node/node.exe，跑 core-bundle/core-host/main.js
       ├─ core-bundle/node_modules/@blue-frontier/dev-sidecar    控制面（配置/插件/规则/PAC/证书）
       ├─ core-bundle/node_modules/@blue-frontier/mitmproxy      数据面（fork 出子进程）
       ├─ core-bundle/extra/                                      sysproxy.exe、证书脚本、PAC
       └─ core-bundle/core-host/mitmproxy-bridge.cjs              fork 目标（注入 rootDir）
```

界面 → 控制面宿主（`core-host`）→ `core.api` 这条链路和原版一致，所以插件、`config.json` 语义、
`intercepts` / `dns.mapping` / `preSetIpList` 等规则全部照旧生效。

### 与 Electron 版的差异

| | Electron 版 | 本版 |
| --- | --- | --- |
| 外壳 | Electron 42（Chromium + Node 双份运行时） | Tauri 2（系统 WebView2） |
| 控制面 | Electron 主进程内 | 独立进程（随包的 `node.exe` + `core-host/main.js`） |
| 界面通信 | `ipcRenderer.invoke('apiInvoke')` | 进程间 RPC（stdio / HTTP，见 `docs/ARCHITECTURE.md`） |
| 状态推送 | `ipcRenderer.on('status'/'speed'/...)` | 事件流（`core-event` 再按 `channel` 分发） |
| 系统能力 | `electron.shell` / `electron-dialog` | `tauri-plugin-opener` / `tauri-plugin-dialog` |
| 自动更新 | electron-updater | 无（手动下载） |
| 抓包诊断 | 内建 CDP + `<webview>` | 变通方案：用系统浏览器打开 + 复制代理地址 |

## 开发

```powershell
# 控制面（用仓库里的 dev-sidecar 源码，不装包）
$env:DS_CORE_PATH = "..\dev-sidecar\packages\core"
$env:DS_EXTRA_PATH = "..\dev-sidecar\packages\gui\extra"
# 调试模式：HTTP + SSE（GUI 走的是 --stdio 管道）
node core-host/src/main.js --port 31190 -v

# 界面
cd app; npm install; npm run tauri:dev

# 组装 core-bundle（CI 里同一条命令）
node scripts/prepare-core-bundle.mjs --dev-sidecar ..\dev-sidecar --max-mb 70

# 装机形态的冒烟（CI 里同一条命令）
pwsh -File scripts/smoke-core-host.ps1
```

## 已知限制

- 只支持 Windows x64；托盘图标已移植，全局快捷键未移植
- 安装包未签名：SmartScreen 会提示「未知发布者」，点「更多信息 → 仍要运行」
- 网页诊断（CDP 抓包）没移植：Tauri 里打开独立浏览器实例的代价太大，改成「用系统浏览器打开 + 复制代理地址」
- `app/src-tauri/icons/` 必须提交进仓库（tauri-build 在编译期就要读 `icon.ico`）

## 设计边界

- **不做锁区/年龄门绕过**：作者服务器中转那套方案不在本项目范围内
- **不内建远程脚本注入**：所有规则来自本地 `config.json`
- **只监听回环**：控制面与数据面都绑 `127.0.0.1`
- **单一数据面**：只有 Node 数据面；不再保留半成品 Rust 数据面

## 致谢

- [docmirror/dev-sidecar](https://github.com/docmirror/dev-sidecar)：配置语义、插件与规则体系的来源
- [Hana-ame/ech-proxy](https://github.com/Hana-ame/ech-proxy)：PAC 分流的思路
- [NLick47/hakimi](https://github.com/NLick47/hakimi)：IP 评分模型、失败分类、路由级拒绝记忆
- [Tauri](https://github.com/tauri-apps/tauri)

## 许可

MIT。原项目 dev-sidecar 亦为 MIT。
