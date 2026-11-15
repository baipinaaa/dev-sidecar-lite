<#
.SYNOPSIS
    ds-proxy 冒烟与压力对比脚本（步骤 8）。

.DESCRIPTION
    在有 Rust 工具链的环境中先执行 `cargo build --release`，然后用本脚本验证：
      1. HTTPS（CONNECT 隧道 + MITM）能通
      2. 明文 HTTP 能通
      3. 8 并发下载时的 CPU 占用（用于与 Node 版对比）

    本机（G:/AIWORK/steam_302）没有 Rust 工具链，脚本未经本地执行验证。

.EXAMPLE
    pwsh tests/e2e/smoke.ps1 -Config "$HOME/.dev-sidecar/running.json" -Port 31182
#>

[CmdletBinding()]
param(
    [string] $Exe = "target/release/ds-proxy.exe",
    [string] $Config = "$HOME/.dev-sidecar/running.json",
    [int] $Port = 31182,
    [string[]] $HttpUrls = @("http://www.baidu.com/"),
    [string[]] $HttpsUrls = @("https://www.baidu.com/", "https://github.com/", "https://steamcommunity.com/"),
    [int] $Concurrency = 8,
    [switch] $KeepRunning
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Push-Location $repoRoot
$failures = 0

function Write-Section($text) {
    Write-Host ""
    Write-Host "=== $text ===" -ForegroundColor Cyan
}

function Test-Url($proxy, $url) {
    $curlArgs = @("-s", "-o", "NUL", "-w", "%{http_code} %{time_total}", "--proxy", $proxy, "--ssl-no-revoke", "--max-time", "20", $url)
    $out = & curl.exe @curlArgs 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host ("  [FAIL] {0} -> curl exit {1}: {2}" -f $url, $LASTEXITCODE, $out) -ForegroundColor Red
        return $false
    }
    $parts = "$out".Trim() -split "\s+"
    $code = $parts[0]
    $time = $parts[1]
    if ($code -match "^[23]") {
        Write-Host ("  [ OK ] {0} -> {1} in {2}s" -f $url, $code, $time) -ForegroundColor Green
        return $true
    }
    Write-Host ("  [FAIL] {0} -> {1} in {2}s" -f $url, $code, $time) -ForegroundColor Red
    return $false
}

if (-not (Test-Path $Exe)) {
    throw "找不到 $Exe；先执行 cargo build --release"
}
if (-not (Test-Path $Config)) {
    throw "找不到配置文件 $Config；先运行一次 Node 版 dev-sidecar 生成"
}

Write-Section "启动 ds-proxy"
$proc = Start-Process -FilePath $Exe -ArgumentList @($Config, "--port", "$Port", "--verbose") -PassThru -NoNewWindow
$proxy = "http://127.0.0.1:$Port"

$ready = $false
for ($i = 0; $i -lt 50; $i++) {
    Start-Sleep -Milliseconds 200
    if ((Test-NetConnection -ComputerName 127.0.0.1 -Port $Port -InformationLevel Quiet -WarningAction SilentlyContinue)) {
        $ready = $true
        break
    }
}
if (-not $ready) {
    if (-not $proc.HasExited) { $proc.Kill() }
    throw "ds-proxy 未在 10 秒内监听 $Port"
}
Write-Host "  PID $($proc.Id) 已监听 $Port" -ForegroundColor Green

try {
    Write-Section "明文 HTTP"
    foreach ($url in $HttpUrls) {
        if (-not (Test-Url $proxy $url)) { $failures++ }
    }

    Write-Section "HTTPS（CONNECT / MITM）"
    foreach ($url in $HttpsUrls) {
        if (-not (Test-Url $proxy $url)) { $failures++ }
    }

    Write-Section "$Concurrency 并发下载（CPU 对比用）"
    $url = "https://github.com/git-for-windows/git/releases/download/v2.47.0.windows.1/Git-2.47.0-64-bit.exe"
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $jobs = 1..$Concurrency | ForEach-Object {
        Start-Job -ScriptBlock {
            param($p, $u)
            & curl.exe -s -o NUL -w "%{size_download}" --proxy $p --ssl-no-revoke --max-time 25 -r 0-8000000 $u
        } -ArgumentList $proxy, $url
    }
    Wait-Job -Job $jobs -Timeout 40 | Out-Null
    $bytes = ($jobs | Receive-Job | Measure-Object -Sum).Sum
    $sw.Stop()
    $jobs | Remove-Job -Force

    $cpu = (Get-Process -Id $proc.Id).CPU
    $mb = [math]::Round(($bytes / 1MB), 2)
    $secs = [math]::Round($sw.Elapsed.TotalSeconds, 2)
    Write-Host ("  下载 {0} MB / {1}s = {2} MB/s" -f $mb, $secs, [math]::Round($mb / $secs, 2))
    Write-Host ("  ds-proxy CPU 累计 {0}s" -f [math]::Round($cpu, 2))

    Write-Section "控制口（NDJSON over stdin）"
    Write-Host "  提示：ds-proxy 的 stdin 已被占用时需另起实例手工测试" -ForegroundColor DarkGray
    Write-Host '  样例：{"id":1,"cmd":"stats"}' -ForegroundColor DarkGray
}
finally {
    Pop-Location
    if (-not $KeepRunning -and -not $proc.HasExited) {
        Write-Section "停止 ds-proxy"
        $proc.Kill()
    }
}

Write-Section "结果"
if ($failures -eq 0) {
    Write-Host "全部通过" -ForegroundColor Green
    exit 0
}
Write-Host "$failures 项失败" -ForegroundColor Red
exit 1
