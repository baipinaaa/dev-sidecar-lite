#!/usr/bin/env pwsh
<#
.SYNOPSIS
    冒烟测试 `ds-core-host`：按「安装目录形态」起 exe，验证控制面 + 数据面都通。

.DESCRIPTION
    为什么需要这个脚本：`ds-core-host` 是 Node SEA，只把入口打进 exe，
    core（`@blue-frontier/dev-sidecar`）与 mitmproxy 都是运行时才 `require` 的。
    如果打包时忘了放 core-bundle，安装包能装上、exe 也能启动，但一进去就 MODULE_NOT_FOUND。

    这里模拟真实安装目录（tauri 的 `bundle.resources` 会原样放到安装根目录）：

        <work>/ds-core-host.exe
        <work>/core-bundle/node_modules/@blue-frontier/dev-sidecar/...
        <work>/core-bundle/node_modules/@blue-frontier/mitmproxy/...
        <work>/core-bundle/extra/sysproxy.exe ...

    不设 `DS_CORE_PATH` / `DS_EXTRA_PATH`，完全靠 exe 自己按安装布局解析。

    断言链（缺一不可）：
      1. `/healthz` 起来
      2. `getApiList` 有内容（前端靠它判断接口是否存在）
      3. `server.start` 返回端口，且 `HEAD /pac.js` 返回 200（数据面真的在跑）
      4. `server.close` 后端口释放

    用隔离的 `USERPROFILE`，绝不碰用户真实的 `~/.dev-sidecar`。

.EXAMPLE
    pwsh -File scripts/smoke-core-host.ps1
    pwsh -File scripts/smoke-core-host.ps1 -Exe core-host/dist/ds-core-host.exe -Port 31199
#>
param(
    [string]$Exe = 'core-host/dist/ds-core-host.exe',
    [string]$NodeEntry = '',
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

# -NodeEntry 用于「没编 SEA 时先验逻辑」：直接 node core-host/src/main.js，断言链完全一样
$bundlePath = Resolve-Path2 $CoreBundle
$useNode = $NodeEntry -ne ''
if ($useNode) {
    $entryPath = Resolve-Path2 $NodeEntry
    if (-not (Test-Path $entryPath)) {
        Write-Host "[FAIL] 找不到 Node 入口：$entryPath" -ForegroundColor Red
        exit 1
    }
} else {
    $exePath = Resolve-Path2 $Exe
    if (-not (Test-Path $exePath)) {
        Write-Host "[FAIL] 找不到 ds-core-host.exe：$exePath（先跑 node core-host/scripts/build-sea.js，或用 -NodeEntry core-host/src/main.js）" -ForegroundColor Red
        exit 1
    }
}

$coreDir = Join-Path $bundlePath 'node_modules/@blue-frontier/dev-sidecar'
if (-not (Test-Path $coreDir)) {
    Write-Host "[FAIL] 找不到 core：$coreDir" -ForegroundColor Red
    Write-Host '       先跑 node scripts/prepare-core-bundle.mjs --dev-sidecar <dev-sidecar 仓库>' -ForegroundColor Yellow
    exit 1
}
$mitmproxyDir = Join-Path $bundlePath 'node_modules/@blue-frontier/mitmproxy'
if (-not (Test-Path $mitmproxyDir)) {
    Write-Host "[FAIL] 找不到 mitmproxy：$mitmproxyDir" -ForegroundColor Red
    exit 1
}
$extraDir = Join-Path $bundlePath 'extra'
if (-not (Test-Path (Join-Path $extraDir 'sysproxy.exe'))) {
    Write-Host "[FAIL] core-bundle/extra 里没有 sysproxy.exe：$extraDir" -ForegroundColor Red
    Write-Host '       没有它，core 的 extra-path 判定失败，系统代理/PAC 全部不可用' -ForegroundColor Yellow
    exit 1
}

# 中间产物一律留在项目内 .build/（不要写 C 盘临时目录）
$buildRoot = Join-Path $root '.build'
New-Item -ItemType Directory -Force -Path $buildRoot | Out-Null
$work = Join-Path $buildRoot ('smoke-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$dataDir = Join-Path $work 'data'
$homeDir = Join-Path $work 'home'
New-Item -ItemType Directory -Force -Path $work, $dataDir, (Join-Path $homeDir '.dev-sidecar') | Out-Null

# 隔离用户配置：core 的 getUserBasePath() 只认 USERPROFILE/HOME
$configFile = Join-Path $homeDir '.dev-sidecar/config.json'
@{
    server            = @{ port = $ProxyPort; enabled = $false; startup = $false }
    proxy             = @{ enabled = $false; pacEnabled = $false }
    app               = @{ autoStart = @{ enabled = $false } }
} | ConvertTo-Json -Depth 6 | Set-Content -Path $configFile -Encoding utf8NoBOM

if ($useNode) {
    # node 模式：入口就在仓库里，直接把 core-bundle 摆到 repo 的 app/src-tauri/core-bundle 位置以外
    # 用 DS_CORE_BUNDLE 让 main.js 认这个临时 bundle（安装形态才靠 exe 相对路径自动发现）
    $env:DS_CORE_BUNDLE = $bundlePath
} else {
    Copy-Item $exePath (Join-Path $work 'ds-core-host.exe') -Force
    $destBundle = Join-Path $work 'core-bundle'
    New-Item -ItemType Directory -Force -Path $destBundle | Out-Null
    Copy-Item (Join-Path $bundlePath '*') $destBundle -Recurse -Force
}

Write-Host "工作目录: $work"

$stdout = Join-Path $work 'stdout.log'
$stderr = Join-Path $work 'stderr.log'

$env:USERPROFILE = $homeDir
$env:HOME = $homeDir
$env:DS_DATA_DIR = $dataDir

if ($useNode) {
    $nodeExe = (Get-Command node -ErrorAction Stop).Source
    $proc = Start-Process -FilePath $nodeExe `
        -ArgumentList $entryPath, '--port', $Port, '--no-startup' `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr `
        -PassThru -WindowStyle Hidden
} else {
    $proc = Start-Process -FilePath (Join-Path $work 'ds-core-host.exe') `
    -ArgumentList '--port', $Port, '--no-startup' `
    -RedirectStandardOutput $stdout -RedirectStandardError $stderr `
    -PassThru -WindowStyle Hidden
}

$ok = $false
$detail = ''
$base = "http://127.0.0.1:$Port"

try {
    $health = $null
    for ($i = 0; $i -lt ($TimeoutSeconds * 2); $i++) {
        if ($proc.HasExited) { break }
        try {
            $health = Invoke-RestMethod -Uri "$base/healthz" -TimeoutSec 2
            if ($health) { break }
        } catch {
            # 还没起来
        }
        Start-Sleep -Milliseconds 500
    }

    if ($proc.HasExited) {
        $detail = "进程已退出（exit=$($proc.ExitCode)）"
    } elseif (-not $health) {
        $detail = "健康检查超时（${TimeoutSeconds}s）"
    } else {
        $tokenFile = Join-Path $dataDir 'run/core.token'
        if (-not (Test-Path $tokenFile)) {
            $detail = "没有生成 token：$tokenFile"
        } else {
            $token = (Get-Content $tokenFile -Raw).Trim()
            $headers = @{ Authorization = "Bearer $token" }
            $invoke = {
                param([string]$api, $param = $null)
                $body = @{ api = $api; param = $param } | ConvertTo-Json -Compress -Depth 12
                Invoke-RestMethod -Uri "$base/invoke" -Method Post -Headers $headers `
                    -ContentType 'application/json' -Body $body -TimeoutSec 60
            }

            $apiList = & $invoke 'getApiList'
            # getApiList 返回扁平接口名数组（"server.start"、"setting.load" …），
            # 前端用它做「接口存在性」校验；缺哪个前缀，对应页面整块功能都会失效。
            $apiNames = @()
            if ($apiList) { $apiNames = @($apiList) }
            $required = @('server.start', 'server.close', 'proxy.start', 'config.get',
                'config.save', 'setting.load', 'setting.save', 'serviceGroups.list',
                'info.get', 'shell.openPath', 'getApiList')
            $missing = @($required | Where-Object { $apiNames -notcontains $_ })

            if ($missing.Count -gt 0) {
                $detail = "getApiList 缺少接口：$($missing -join ', ')（共 $($apiNames.Count) 项）"
            } else {
                $started = & $invoke 'server.start'
                if (-not $started -or -not $started.port) {
                    $detail = "server.start 没有返回端口：$($started | ConvertTo-Json -Compress -Depth 3)"
                } else {
                    # 数据面必须真的应答 /pac.js（core 的 start 不校验子进程是否起来）
                    $pac = $null
                    for ($i = 0; $i -lt 60; $i++) {
                        try {
                            $pac = Invoke-WebRequest -Uri "http://127.0.0.1:$($started.port)/pac.js" `
                                -Method Head -TimeoutSec 3 -NoProxy
                            if ($pac.StatusCode -eq 200) { break }
                        } catch {
                            $pac = $null
                        }
                        Start-Sleep -Milliseconds 500
                    }

                    if (-not $pac -or $pac.StatusCode -ne 200) {
                        $detail = "数据面启动了但 /pac.js 不可用（端口 $($started.port)）"
                    } else {
                        & $invoke 'server.close' | Out-Null
                        Start-Sleep -Seconds 2
                        $listening = Get-NetTCPConnection -LocalPort $started.port -State Listen -ErrorAction SilentlyContinue
                        if ($listening) {
                            $detail = "server.close 之后端口 $($started.port) 仍在监听"
                        } else {
                            $ok = $true
                            $detail = "healthz→getApiList($($apiNames.Count) 项)→server.start→/pac.js 200→server.close，全链路通过"
                        }
                    }
                }
            }
        }
    }
} catch {
    $detail = "请求失败：$($_.Exception.Message)"
} finally {
    if ($proc -and -not $proc.HasExited) {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 300
    }
    # 数据面子进程是 fork 出来的，杀掉父进程不一定会带走它
    Get-CimInstance Win32_Process -Filter "Name='ds-core-host.exe' OR Name='node.exe'" -ErrorAction SilentlyContinue |
        Where-Object { $_.CommandLine -like "*$work*" } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
}

Write-Host ''
Write-Host '--- core-host stdout （末尾 30 行）---'
if (Test-Path $stdout) { Get-Content $stdout -Tail 30 }
Write-Host '--- core-host stderr （末尾 30 行）---'
if (Test-Path $stderr) { Get-Content $stderr -Tail 30 }

if ($ok) {
    Write-Host "[PASS] $detail" -ForegroundColor Green
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
    exit 0
}

Write-Host "[FAIL] $detail" -ForegroundColor Red
Write-Host "工作目录保留供排查：$work" -ForegroundColor Yellow
exit 1
