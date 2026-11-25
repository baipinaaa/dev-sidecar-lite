#!/usr/bin/env pwsh
<#
.SYNOPSIS
    冒烟测试控制面：以 stdio JSON-RPC 拉起 `core-host/main.js`，验证控制面 + 数据面都通。

.DESCRIPTION
    为什么需要这个脚本：core（`@blue-frontier/dev-sidecar`）与 mitmproxy 都是运行时才
    `require` 的，如果打包时忘了放 core-bundle，安装包能装上、进程也能启动，
    但一进去就 MODULE_NOT_FOUND。

    v0.3.0 起控制面走 stdio JSON-RPC，所以探针逻辑在 `scripts/rpc-probe.cjs` 里
    （起进程 → 写帧读帧 → 断言），这个脚本只负责准备隔离环境与路径。

    断言链（缺一不可）：
      1. `ready` 帧到达（控制面真的起来了）
      2. `getApiList` 覆盖界面依赖的接口
      3. `config.save` + `server.start` 返回端口，数据面端口真正监听
      4. `HEAD /pac.js` 返回 200（PAC 端点可用）
      5. `server.close` 后端口释放、`shutdown` 让控制面自行退出

    用隔离的 `USERPROFILE` 与 `DS_DATA_DIR`，绝不碰用户真实的 `~/.dev-sidecar-lite`。

.EXAMPLE
    pwsh -File scripts/smoke-core-host.ps1
    pwsh -File scripts/smoke-core-host.ps1 -Port 31199 -ProxyPort 31198
#>
param(
    [string]$NodeEntry = 'core-host/src/main.js',
    [string]$CoreBundle = 'app/src-tauri/core-bundle',
    [int]$Port = 31199,
    [int]$ProxyPort = 31198,
    [int]$TimeoutSeconds = 120
)

$ErrorActionPreference = 'Stop'

# core 用 console 打日志（中文），PowerShell 默认用 OEM 代码页读子进程输出会乱码
try {
    [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
    $OutputEncoding = [System.Text.Encoding]::UTF8
} catch {
    # 某些宿主不允许改，忽略
}

$root = Split-Path -Parent $PSScriptRoot

function Resolve-Path2 {
    param([string]$Path)
    if ([System.IO.Path]::IsPathRooted($Path)) { return $Path }
    return (Join-Path $root $Path)
}

$entry = Resolve-Path2 $NodeEntry
if (-not (Test-Path -LiteralPath $entry)) {
    throw "找不到控制面入口: $entry"
}

$bundle = Resolve-Path2 $CoreBundle
if (Test-Path -LiteralPath $bundle) {
    # 让 main.js 直接从这个 bundle 找 core / extra
    $env:DS_CORE_BUNDLE = $bundle
} else {
    Write-Host "[提示] 没有 $bundle，将按源码布局解析 core（需要先 pnpm install）" -ForegroundColor Yellow
}

# 优先用 bundle 里随包的 node，保证与安装包运行时一致
$nodeExe = $null
$bundledNode = Join-Path $bundle 'node/node.exe'
if (Test-Path -LiteralPath $bundledNode) {
    $nodeExe = $bundledNode
} else {
    $command = Get-Command node -ErrorAction SilentlyContinue
    if ($command) {
        $nodeExe = $command.Source
    }
}
if (-not $nodeExe) {
    throw '找不到 node（既没有 core-bundle/node/node.exe，PATH 里也没有）'
}

$probe = Join-Path $root 'scripts/rpc-probe.cjs'
if (-not (Test-Path -LiteralPath $probe)) {
    throw "找不到探针脚本: $probe"
}

$workDir = Join-Path $root '.build'
New-Item -ItemType Directory -Force -Path $workDir | Out-Null
$homeDir = Join-Path $workDir ("smoke-home-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
$dataDir = Join-Path $workDir ("smoke-data-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force -Path $homeDir | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $homeDir '.dev-sidecar-lite') | Out-Null
New-Item -ItemType Directory -Force -Path $dataDir | Out-Null

# 用配置把代理端口固定到探针端口，避免撞用户正在使用的 31181
$configFile = Join-Path $homeDir '.dev-sidecar-lite/config.json'
@{
    server = @{ port = $ProxyPort; startup = $false }
    proxy  = @{ enabled = $false; pacEnabled = $false; useMapping = $false; enableLoopback = $false }
} | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $configFile -Encoding utf8

Write-Host "[smoke] node   : $nodeExe"
Write-Host "[smoke] entry  : $entry"
Write-Host "[smoke] bundle : $(if (Test-Path -LiteralPath $bundle) { $bundle } else { '(源码布局)' })"
Write-Host "[smoke] home   : $homeDir"
Write-Host "[smoke] 代理端口: $ProxyPort"

$exit = 1
try {
    & $nodeExe $probe --node $nodeExe --entry $entry --data-dir $dataDir --home $homeDir --port $ProxyPort --timeout ($TimeoutSeconds * 1000)
    $exit = $LASTEXITCODE
} finally {
    # 控制面子进程应当已自行退出；万一残留，按端口兜底清理
    try {
        Get-CimInstance Win32_Process -Filter "Name='node.exe'" -ErrorAction SilentlyContinue |
            Where-Object { $_.CommandLine -and $_.CommandLine -like "*$entry*" } |
            ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    } catch {
        # 忽略
    }
    Remove-Item -LiteralPath $homeDir, $dataDir -Recurse -Force -ErrorAction SilentlyContinue
}

if ($exit -ne 0) {
    throw "控制面冒烟失败（退出码 $exit）"
}

Write-Host '[smoke] 控制面与数据面冒烟全部通过' -ForegroundColor Green
