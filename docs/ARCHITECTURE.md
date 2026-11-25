# 架构

本文描述 **v0.3.0** 的进程模型与模块边界。

一句话：**外壳换掉（Electron → Tauri 2），控制面与数据面原样沿用 Node 版**。

## 一、进程拓扑

```
┌──────────────────────────────────────────────────────────┐
│ dev-sidecar-lite.exe        Tauri 2 壳（系统 WebView2）    │
│  └─ 原版 Vue 页面（app/src/view/**，从 packages/gui 移植） │
└───────────────────────┬──────────────────────────────────┘
                        │ 进程间 RPC（api 名与 $api.* 一致）
┌───────────────────────▼──────────────────────────────────┐
│ node.exe + bundle     Node 控制面宿主（core-bundle/core-host/main.js）│
│  ├─ core.api         @blue-frontier/dev-sidecar（配置/插件/  │
│  │                    规则/PAC/证书/系统代理）              │
│  └─ localApi         补充 core 没有的 24 个接口             │
└───────────────────────┬──────────────────────────────────┘
                        │ fork(mitmproxyPath, [running.json]) + IPC
┌───────────────────────▼──────────────────────────────────┐
│ Node 数据面           @blue-frontier/mitmproxy             │
│  └─ 代理监听 127.0.0.1:31181（CONNECT / MITM / PAC）        │
└──────────────────────────────────────────────────────────┘
```

三个进程各自的职责：

| 进程 | 生命周期 | 崩溃影响 |
| --- | --- | --- |
| Tauri 壳 | 用户可见 | 界面消失，代理继续跑（数据面归 core-host 管） |
| `node.exe core-host/main.js` | 壳启动时拉起，壳退出时 taskkill /T | 代理停摆，界面报错横幅 |
| 数据面（Node） | `server.start` 时 fork，`server.close` 时 IPC 退出 | 代理停摆，core 的 `status` 变红 |

## 二、为什么控制面要独立成进程

原版把控制面放在 Electron 主进程里。换成 Tauri 后主进程是 Rust，而控制面是 6 万行 Node 代码
（含 135 条拦截规则、插件体系、证书安装、系统代理操作），用 Rust 重写等于重做一个项目。

所以控制面留在 Node，装成一个独立进程，Tauri 只做「窗口 + 托盘 + 转发」。

代价是**必须跨进程**，于是有三个必须守住的约定：

1. **控制面必须在监听前把 token 写到磁盘**，否则壳的第一次调用会 401（0.2.0 的「点按钮没反应」就是这么来的：
   `CoreClient` 在应用启动瞬间读 token，那时 core-host 还没起来）。
2. **`fork` 的脚本入口必须是 Node 能直接执行的 `.cjs`/`.js`**（`core-bundle/core-host/mitmproxy-bridge.cjs`）。
3. **`require('@blue-frontier/*')` 必须能落到磁盘上的包目录**：core-host 用
   `packageRequire()`（`module.createRequire(<core>/package.json)`）解析第三方包，
   `runtime.installPackageAliases()` 负责把 `@blue-frontier/*` 映射到 core-bundle 下的两个并列目录。

第 2、3 条都是 SEA 带来的历史包袱。v0.3.0 起控制面改成「随包的 `node.exe` + 磁盘上的 js」，
`fork()` 与 `require()` 恢复原生语义，`fork-shim.js` 已删除（`packageRequire()` 保留，
因为安装包里 `@blue-frontier/*` 仍是并列目录）。

## 三、控制面协议

v0.3.0 起 GUI 与核心用**管道**说话：`ds-core-host` 的 stdin/stdout 就是 RPC 通道，
一行一个 JSON（NDJSON），不需要端口、不需要凭据，也不存在「谁先起来」的竞态。

| 项目 | 值 |
| --- | --- |
| 传输 | Tauri 壳 `spawn(node.exe, [core-bundle/core-host/main.js, --stdio])`，stdin/stdout 收发 |
| 调用 | `{"jsonrpc":"2.0","id":1,"method":"api.invoke","params":{"api":"proxy.start","param":null}}` |
| 成功 | `{"jsonrpc":"2.0","id":1,"result":<任意 JSON>}`（裸数据，与 Electron 版回调一致） |
| 失败 | `{"jsonrpc":"2.0","id":1,"error":{"code":"unknown_api","message":"api 接口不存在: x"}}` |
| 事件 | `{"jsonrpc":"2.0","method":"event","params":{"channel":"status","payload":{...}}}` |
| 就绪 | `{"jsonrpc":"2.0","method":"ready","params":{"pid":1234,"version":"0.3.0","engine":"node"}}` |
| 其他方法 | `core.info`（版本/数据目录）、`shutdown`（关数据面并退出） |
| 事件通道 | `status` / `speed` / `traffic` / `error` / `log` / `config.changed` |

`api` 名字与 Electron 版的 `$api.xxx` 完全一致，所以 `packages/gui` 的页面可以逐页对照移植。

Tauri 侧读到事件后统一转成 `core-event`（payload `{channel, payload}`）再交给前端，
原因是 Tauri 的事件名不接受点号，`config.changed` 这类名字会被静默丢弃。
协议实现见 `core-host/src/rpc.js`（核心）与 `app/src-tauri/src/lib.rs` 的 `CoreRpc`（壳）。

**HTTP + SSE 仍然保留，但只用于调试**（`node core-host/src/main.js --port 31190`）：
`POST /invoke`、`GET /events`、`GET /healthz`，凭据在 `<dataDir>/run/core.token`。

## 四、数据面如何被拉起

```
core.api.server.start({ mitmproxyPath, setting })
  → fork(core-bundle/core-host/mitmproxy-bridge.cjs, [<dataDir>/running.json])
      → bridge 读 running.json，注入 config.setting.rootDir = dirname(DS_EXTRA_PATH)
      → require('@blue-frontier/mitmproxy').start(config)
          → 监听 127.0.0.1:<server.port>（默认 31181）
```

三个容易踩的点：

- `running.json` 里值为 `null` 的表项表示**关掉这条内置规则**（core 就是这么关 git 的油猴脚本的），
  而 mitmproxy 的规则构建不判空，必须由 bridge 先剔除（`dropNullIntercepts`）。
- `config.setting.rootDir` 不给的话，mitmproxy 会用 `undefined/scripts` 拼脚本目录。
- 数据面停止只能通过 IPC（`{type:'action',event:{key:'close'}}`），不是杀进程。

## 五、配置与数据目录

| 内容 | 位置 |
| --- | --- |
| 用户配置 | `C:\Users\<你>\.dev-sidecar-lite\config.json`（`config.json5` 兼容） |
| 根证书 | 同目录 `dev-sidecar.ca.crt` / `dev-sidecar.ca.key` |
| 日志 | 同目录 `log\`；控制面自身日志在 `%ProgramData%\dev-sidecar-lite\log\core-host.log` |
| 运行时 | `%ProgramData%\dev-sidecar-liteunning.json`（`run/core.token` 只在 HTTP 调试模式生成） |

core 内部把 `~/.dev-sidecar` 写死在十几个位置，本项目不改上游代码，而是在加载时对
`@blue-frontier/**` 的源码做一次字面量替换（`core-host/src/runtime.js` 的 `installUserDir()`），
把目录名换成 `.dev-sidecar-lite`。这是唯一一处「运行时改上游源码」的地方，改动范围仅限字符串。

## 六、打包分层

| 层 | 内容 | 说明 |
| --- | --- | --- |
| L1 单文件 | 控制面 `bundle.cjs` | esbuild 打成单文件，只有这份是「代码」 |
| L2 打平 | `core-bundle/node_modules` | 用 pnpm `node-linker=hoisted` 产真实文件树（不能用符号链接，NSIS 会丢） |
| L3 保留 | `core-bundle/extra`、`*.node` 原生模块 | 原生模块不能打平，`extra` 里有 exe 与脚本 |

`core-bundle` 体积门禁 70 MB（当前 33 MB），文件数以「尽力而为」为目标（当前 3.6k，
原版 Electron 版靠 asar 压到 133 个文件）。

## 七、已知边界

- **Windows x64 only**：`sysproxy.exe`、`EnableLoopback.exe`、NSIS 都是 Windows 专用
- **不做锁区/年龄门绕过**：那套方案依赖作者服务器中转，不在本项目范围
- **网页诊断不内建 CDP**：Tauri 里起独立浏览器实例代价太大，改成「系统浏览器 + 复制代理地址」
- **没有自动更新**：换版本手动下载覆盖安装
