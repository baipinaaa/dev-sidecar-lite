#!/usr/bin/env pwsh
<#
.SYNOPSIS
    冒烟测试 `ds-core-host`：验证「安装包形态」下能加载 Node core 并应答 API。

.DESCRIPTION
    为什么需要这个脚本：`ds-core-host` 是 Node SEA，只把入口打进 exe，
    core（`@blue-frontier/dev-sidecar`）是运行时才 `require` 的。
    如果打包时忘了把 core 放进 `resources/core-bundle/`，安装包能装上、
    exe 也能启动，但一进去就 MODULE_NOT_FOUND，UI 全部接口失败 ——
    光看「打包成功」的退出码是发现不了的。

    所以这里模拟真实安装目录：

        <work>/ds-core-host.exe
        <work>/resources/core-bundle/packages/core/...

    然后不设 `DS_CORE_PATH`，完全靠 exe 自己按安装布局解析，
    起 HTTP 后调用 `status.get` 断言返回 200。

.EXAMPLE
    pwsh -File scripts/smoke-core-host.ps1
    pwsh -File scripts/smoke-core-host.ps1 -Exe core-host/dist/ds-core-host.exe -Port 31199
#>
param(
    [string]$Exe = 'core-host/dist/ds-core-host.exe',
    [string]$CoreBundle = 'app/src-tauri/core-bundle',
    [int]$Port = 31199,
    [int]$TimeoutSeconds = 90
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot

function Resolve-Path2 {
    param([string]$Path)
    if ([System.IO.Path]::IsPathRooted($Path)) { return $Path }
    return (Join-Path $root $Path)
}

$exePath = Resolve-Path2 $Exe
$bundlePath = Resolve-Path2 $CoreBundle

if (-not (Test-Path $exePath)) {
    Write-Host "[FAIL] 找不到 ds-core-host.exe：$exePath（先跑 core-host/scripts/build-sea.js）" -ForegroundColor Red
    exit 1
}

$coreDir = Join-Path $bundlePath 'packages/core'
if (-not (Test-Path $coreDir)) {
    Write-Host "[FAIL] 找不到 core 目录：$coreDir" -ForegroundColor Red
    Write-Host '       说明打包流程没有把 core 放进 resources/core-bundle/' -ForegroundColor Yellow
    exit 1
}

$work = Join-Path ([System.IO.Path]::GetTempPath()) ("dsl-smoke-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$dataDir = Join-Path $work 'data'
New-Item -ItemType Directory -Force -Path $work, $dataDir | Out-Null

# 模拟安装目录：exe 与 resources/ 平级
Copy-Item $exePath (Join-Path $work 'ds-core-host.exe') -Force
$destBundle = Join-Path $work 'resources/core-bundle'
New-Item -ItemType Directory -Force -Path $destBundle | Out-Null
Copy-Item $bundlePath/* $destBundle -Recurse -Force

Write-Host "工作目录: $work"
Write-Host "core 目录: $coreDir"

$stdout = Join-Path $work 'stdout.log'
$stderr = Join-Path $work 'stderr.log'

$env:DS_DATA_DIR = $dataDir
$proc = Start-Process -FilePath (Join-Path $work 'ds-core-host.exe') `
    -ArgumentList '--port', $Port, '--no-startup' `
    -RedirectStandardOutput $stdout -RedirectStandardError $stderr `
    -PassThru -NoNewWindow:$false

$ok = $false
$detail = ''
try {
    $health = $null
    for ($i = 0; $i -lt ($TimeoutSeconds * 2); $i++) {
        if ($proc.HasExited) { break }
        try {
            $health = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/healthz" -TimeoutSec 2
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
            $body = '{"api":"status.get","param":null}'
            $resp = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/invoke" -Method Post `
                -Headers $headers -ContentType 'application/json' -Body $body -TimeoutSec 30
            # status.get 返回 { server: {...}, proxy: {...}, plugin: {...}, app: {...} }
            if ($resp -and $resp.PSObject.Properties.Name -contains 'server') {
                $ok = $true
                $detail = 'status.get 返回正常'
            } else {
                $detail = "status.get 返回结构异常：$($resp | ConvertTo-Json -Compress -Depth 3)"
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
}

Write-Host ''
Write-Host '--- core-host stdout （末尾 25 行）---'
if (Test-Path $stdout) { Get-Content $stdout -Tail 25 }
Write-Host '--- core-host stderr （末尾 25 行）---'
if (Test-Path $stderr) { Get-Content $stderr -Tail 25 }

if ($ok) {
    Write-Host "[PASS] $detail" -ForegroundColor Green
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
    exit 0
}

Write-Host "[FAIL] $detail" -ForegroundColor Red
Write-Host "工作目录保留供排查：$work" -ForegroundColor Yellow
exit 1
