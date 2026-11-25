# 从 Electron 版迁移到 Lite

两个版本**不能同时运行**：它们会争抢同一个本地端口（默认 `127.0.0.1:31181`），并且签发同一张根证书
（`dev-sidecar` 同名 CA 会互相覆盖信任，导致旧版签的证书被新版覆盖后全部失效）。

配置目录已经从 0.2.2 起**分开**：Lite 用 `~/.dev-sidecar-lite`，原版仍然是 `~/.dev-sidecar`，
所以两边的配置文件、日志、证书互不干扰，可以放心各留一份。

正确的顺序是：**先迁移配置，再卸旧，最后装新**。

> 迁移配置（可选，想沿用原版规则时做）：把 `C:\Users\<你>\.dev-sidecar\` 下的
> `config.json`（或 `config.json5`）与 `rootCa.crt` / `rootCa.key` 一起复制到
> `C:\Users\<你>\.dev-sidecar-lite\` 即可。不迁移也没关系，Lite 首次启动会生成一份默认配置。

## 一、卸载旧版

1. 打开旧版界面，把「系统代理」「PAC 分流」「代理服务」全部关掉（让注册表里的
   `ProxyEnable`/`AutoConfigURL` 回到干净状态）。
2. 退出旧版（托盘右键 → 退出），确认托盘图标消失。
3. 「设置 → 应用 → 已安装的应用」里卸载 `dev-sidecar`。
4. 卸载完成后**逐项确认**：

```powershell
# 1) 端口是否还给系统了
Get-NetTCPConnection -LocalPort 31181 -ErrorAction SilentlyContinue

# 2) 系统代理是否已关闭
reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings" /v ProxyEnable
reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings" /v AutoConfigURL

# 3) 旧版根证书是否还在（Compatibility 版可能不会自己删）
Get-ChildItem Cert:\CurrentUser\Root, Cert:\LocalMachine\Root |
  Where-Object { $_.Subject -like "*dev-sidecar*" } |
  Select-Object Subject, Thumbprint, PSParentPath

# 4) 是否还有残留进程
Get-Process dev-sidecar -ErrorAction SilentlyContinue
```

发现残留证书时手动删（**只删 `dev-sidecar` 自己签的那张**）：

```powershell
$cert = Get-ChildItem Cert:\CurrentUser\Root | Where-Object { $_.Subject -like "*dev-sidecar*" }
foreach ($c in $cert) { Remove-Item -Path $c.PSPath -Force }
```

## 二、装新版

1. 运行 `DevSidecar Lite_<version>_x64-setup.exe`。
2. 首次启动会在 `<数据目录>/run/` 下生成 `core.token`（**别外传**，它能改系统代理）。
3. 首次开「系统代理」或「PAC 分流」时会提示装根证书 —— 这一步不可跳过，否则 HTTPS 会报
   `NET::ERR_CERT_AUTHORITY_INVALID`。

## 三、配置怎么带过来

旧版的用户配置在 `C:\Users\<你>\.dev-sidecar`，新版用 `C:\Users\<你>\.dev-sidecar-lite`。
**直接把整个目录复制过去最简单**（`robocopy "%USERPROFILE%\.dev-sidecar" "%USERPROFILE%\.dev-sidecar-lite" /E`）：

| 文件 | 说明 |
| --- | --- |
| `config.json` | 用户自定义（与默认值做 diff 后保存） |
| `remote_config.json5` | 远程配置缓存，可整份删掉让新版重新拉 |
| `running.json` | 运行时合并结果，**不要手改**，每次启动重写 |
| `dev-sidecar.ca.crt` / `dev-sidecar.ca.key.pem` | 根证书。**两个文件一起搬**就继续用同一张，系统里已装的证书不用重装；只搬 `config.json` 不搬证书，新版会现场生成一张新的 CA，必须重新安装证书才不报错 |

如果你之前在旧版里手改过 `running.json` 来绕过某个问题，**不要把它带到新版**：新版每次启动
都会从 `config.json` + `remote_config.json5` 重新合并生成。

## 四、常见问题

**Q：装完新版打不开网页，但旧版能用？**
先看系统代理指向谁：

```powershell
reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings" /v ProxyServer
```

如果还是旧版的端口，说明旧版进程没退干净，任务管理器里结束 `dev-sidecar.exe`。

**Q：两个都想留着试？**
改端口再装：新版 `config.json` 里 `server.port` 改成 `31182`，并让两个版本只有一个开系统代理。

**Q：证书重复安装了一堆怎么办？**
`Cert:\CurrentUser\Root` 里出现多个同 Subject 的证书时，删掉除最新一张以外的全部，然后重启新版，
在「代理」页重新导出/安装一次。

**Q：怎么切回 Node 引擎？**
新版默认 `proxy.engine: "auto"`（找得到 `ds-proxy.exe` 就用 Rust）。出问题时在「设置」页把引擎改成
`node`，或在 `config.json` 里写 `"proxy": { "engine": "node" }`，重启代理服务即可。
