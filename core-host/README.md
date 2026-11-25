# core-host（控制面宿主）

DevSidecar Lite 的**控制面宿主**：把 Node core（`@blue-frontier/dev-sidecar`）的 `api`
暴露成一条 stdio JSON-RPC 通道，供 Tauri 壳调用。

它不碰数据面——代理由 core 自己 `fork` 拉起（`@blue-frontier/mitmproxy` 的入口
`core-host/mitmproxy-bridge.cjs`）。

## 为什么需要它

Node core 的入口 `packages/core/src/index.js` 只有：

```js
module.exports = expose
```

没有任何服务端：Electron 版是 `packages/gui` 里通过 `ipcMain.handle('apiInvoke')`
直接 require core 来调用的。换掉 UI 之后必须补一层传输，这就是本目录。

## 为什么是管道而不是 HTTP

控制面是 GUI 的**子进程**，进程边界本身就是天然的隔离：

| 问题 | HTTP + SSE | stdio 管道 |
| --- | --- | --- |
| 端口 | 要挑 31190，可能与别的软件冲突 | 不需要 |
| 鉴权 | token 文件 + 首次启动读写时序（曾导致 SSE 永久 401） | 不需要（管道只属于父子进程） |
| 长连接 | SSE 会被中间件/超时打断，要自己重连 | 管道生命周期 = 进程生命周期 |
| 调试 | `curl` 方便 | 要写脚本（已提供 `scripts/rpc-probe.cjs`） |

所以 GUI 走管道，HTTP 保留成**调试模式**。

## 协议

NDJSON：一行一个 JSON，UTF-8。

```text
GUI → host  {"jsonrpc":"2.0","id":1,"method":"api.invoke","params":{"api":"proxy.start","param":null}}
host → GUI  {"jsonrpc":"2.0","id":1,"result":<任意 JSON>}
host → GUI  {"jsonrpc":"2.0","id":1,"error":{"code":"unknown_api","message":"api 接口不存在: x"}}
host → GUI  {"jsonrpc":"2.0","method":"event","params":{"channel":"status","payload":{...}}}
host → GUI  {"jsonrpc":"2.0","method":"ready","params":{"pid":1234,"version":"0.3.0","engine":"node"}}
```

方法只有三个：`api.invoke`、`core.info`、`shutdown`。

实现：`src/rpc.js`（协议）、`src/host.js`（接口解析 + 事件回流）、`src/local-api.js`
（core 里没有的复合接口）。`src/http.js` 是调试用的 HTTP + SSE 传输。

## 接口

`api` 的路径与 Electron 版 `$api.xxx` 完全相同（`proxy.start`、`config.save`、
`plugin.git.start`…），所以两个前端可以逐页对照。

**额外接口**（core 里没有，由宿主实现）：

- `engine.status` → `{ engine: 'node', running }`（v0.3.0 只有一种数据面，形状保留以免旧前端 404）
- `engine.set` → 同上，忽略参数
- `api.list` → 列出所有可调用接口（调试用）

## 数据目录

`dataDir` 依次取：

1. `DS_DATA_DIR`
2. `%ProgramData%\dev-sidecar-lite`
3. `~/.dev-sidecar-lite`

用户配置目录是 `~/.dev-sidecar-lite`（与原版 `~/.dev-sidecar` 分开，两者可共存），
由 `src/runtime.js` 的 `installUserDir()` 在加载上游源码时替换目录名字面量。

## 运行

```powershell
# 开发期（GUI 用的形态）：stdio RPC
node core-host/src/main.js --stdio -v

# 调试形态：HTTP + SSE（多一个 core.token）
node core-host/src/main.js --port 31190 -v
```

控制面不需要编译：`core-bundle/node/node.exe` 是随包的 Node 运行时，
入口就是 `core-bundle/core-host/src/main.js`（由 `scripts/prepare-core-bundle.mjs` 一并摆放）。

`stdout` 在 `--stdio` 模式下是协议专用通道：`src/rpc.js` 的 `takeOverStdout()`
会把所有非协议写入改道到 `stderr`，所以日志看 `stderr`（安装后落在
`%ProgramData%\dev-sidecar-lite\log\core-host.log`）。

## 自测

```powershell
# 仓库根目录，隔离环境 + 完整断言链（ready → getApiList → 数据面 → /pac.js → close）
pwsh -File scripts/smoke-core-host.ps1

# 只看协议本身（不经 PowerShell，直接 node）
node scripts/rpc-probe.cjs --entry core-host/src/main.js --home .build/probe-home
```

调试模式下想用 `curl`：

```powershell
$token = Get-Content "$env:PROGRAMDATA\dev-sidecar-lite\run\core.token"
curl -s -X POST http://127.0.0.1:31190/invoke -H "Authorization: Bearer $token" -H "Content-Type: application/json" -d '{"api":"status.get","param":null}'
curl -N http://127.0.0.1:31190/events -H "Authorization: Bearer $token"
```
