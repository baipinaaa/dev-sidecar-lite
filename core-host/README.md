# ds-core-host

DevSidecar Lite 的**控制面宿主**：把 Node core（`@blue-frontier/dev-sidecar`）的 `api`
暴露成 HTTP + SSE，供 Tauri 壳调用。

它不碰数据面——代理由 core 自己拉起，`server.proxy.engine` 决定用 Node mitmproxy 还是
Rust `ds-proxy.exe`。

## 为什么需要它

Node core 的入口 `packages/core/src/index.js` 只有：

```js
module.exports = expose
```

没有任何 HTTP 服务：Electron 版是 `packages/gui` 里通过 `ipcMain.handle('apiInvoke')`
直接 require core 来调用的。换掉 UI 之后必须补一层传输，这就是本目录。

## 接口

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| POST | `/invoke` | body `{ "api": "proxy.startPac", "param": null }`，需 `Authorization: Bearer <token>` |
| GET | `/events` | SSE，`event: status\|speed\|traffic\|log\|error` + `data: <json>` |
| GET | `/healthz` | 探活，无鉴权 |

`api` 的路径与 Electron 版 `$api.xxx` 完全相同（`proxy.start`、`config.save`、
`plugin.git.start`…），所以两个前端可以逐页对照。

**额外两个接口**（core 里没有，由宿主实现）：

- `engine.status` → `{ engine, configured, running }`
- `engine.set` → `{ engine: "rust" | "node" | "auto" }`，写入 `config.proxy.engine` 并按新引擎重启数据面
- `api.list` → 列出所有可调用接口（调试用）

## 凭据

首次启动生成 `<dataDir>/run/core.token`（24 字节 hex，0600）。`dataDir` 依次取：

1. `DS_DATA_DIR`
2. `%ProgramData%\dev-sidecar-lite`
3. `~/.dev-sidecar-lite`

只监听回环地址：这个进程能改系统代理和装证书。

## 运行

```powershell
# 开发期：直接指向源码里的 core
$env:DS_CORE_PATH = "G:\AIWORK\steam_302\dev-sidecar\packages\core"
node core-host/src/main.js --port 31190 -v
```

```powershell
# 打包成单文件（需要 postject）
cd core-host
npm run build          # 产出 dist/ds-core-host.exe
```

发布目录约定见 `scripts/build-sea.js` 顶部注释：SEA 只打包入口脚本，`node_modules`
仍从磁盘加载，所以安装包里要带 `resources/app/node_modules`。

## 自测

```powershell
# 取 token
$token = Get-Content "$env:PROGRAMDATA\dev-sidecar-lite\run\core.token"

# 调接口
curl -s -X POST http://127.0.0.1:31190/invoke `
  -H "Authorization: Bearer $token" `
  -H "Content-Type: application/json" `
  -d '{"api":"status.get","param":null}'

# 看事件流
curl -N http://127.0.0.1:31190/events -H "Authorization: Bearer $token"
```
