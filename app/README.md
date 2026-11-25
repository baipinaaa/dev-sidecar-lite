# DevSidecar Lite · 桌面壳

Tauri 2 + Vue 3 的展示面。**它不做代理**，只负责：

- 把前端调用转发给控制面 `ds-core-host.exe`（Node core）
- 订阅控制面的 SSE，把 `status` / `speed` / `traffic` / `log` 推给界面
- 托盘菜单与首页开关共用一套互斥逻辑（托盘只发事件 `tray-toggle`，由前端处理）

## 目录

```
app/
├─ index.html
├─ package.json
├─ vite.config.js          # 固定 127.0.0.1:5273
├─ src/
│  ├─ api.js               # invoke 适配层（Tauri / 浏览器 mock）+ tauriInvoke
│  ├─ view/                # 原版 electron 版 GUI 的移植层（pages / components / router / modules）
│  └─ main.js              # 挂载顺序：initApi → initPre → initModules → mount → startStatusStream
└─ src-tauri/
   ├─ src/lib.rs           # 命令：api_invoke / core_info / start_status_stream / legacy_check
   ├─ src/main.rs
   ├─ tauri.conf.json      # externalBin: ds-core-host
   └─ capabilities/default.json
```

## 开发

```powershell
cd dev-sidecar-lite/app
npm install
npm run tauri:dev          # 会先起 vite（5273），再起 Tauri
```

只调前端（浏览器里跑，走 mock）：

```powershell
npm run dev                # http://127.0.0.1:5273
```

## 构建

```powershell
# 1) 先构建控制面（数据面是 Node mitmproxy，随 core-bundle 下发，不需要编译）
cd ..
cd core-host; npm ci; npm run build   # 产出 core-host/dist/ds-core-host.exe
cd ..

# 2) 摆好 sidecar（文件名必须带目标三元组后缀）
pwsh -File scripts/prepare-binaries.ps1

# 3) 图标（首次）
cd app; npx tauri icon ..\assets\icon.png

# 4) 打包 NSIS 安装包
npm run tauri:build
```

产物：`app/src-tauri/target/release/bundle/nsis/DevSidecar Lite_<version>_x64-setup.exe`

## 控制面约定

| 项目 | 值 |
| --- | --- |
| 地址 | `DS_CORE_HOST_URL`，默认 `http://127.0.0.1:31190` |
| 凭据 | `DS_CORE_HOST_TOKEN`，或 `%ProgramData%\dev-sidecar-lite\run\core.token` |
| 调用 | `POST /invoke`，body `{ "api": "proxy.start", "param": null }`，`Authorization: Bearer <token>` |
| 事件 | `GET /events`，SSE，`event: status|speed|traffic|log|error|config.changed` + `data: <json>` |

`api` 的名称与 Electron 版的 `$api.xxx` 一致，所以前端代码可以逐页对照迁移。
