#!/usr/bin/env pwsh
# ECH 验收脚本（步骤 12）：验证 linux.do 不再落在 Cloudflare 挑战页。
#
# 背景：Node 版因为用自拼 ClientHello（X25519 only、无 GREASE、ALPN 空），
# Cloudflare Bot Management 会判定"非浏览器"，linux.do 反复进 challenge 循环，
# 前端等一个被挡的 XHR → 卡在加载动画。Rust 版用 rustls 原生 ECH + 真实握手，
# 目标是不再出现 `Just a moment...` / `请稍候…`。
#
# 用法：
#   pwsh -File tests/e2e/cf_acceptance.ps1 -Exe .\target\release\ds-proxy.exe -Config ..\running.json
#   pwsh -File tests/e2e/cf_acceptance.ps1 -ProxyUrl http://127.0.0.1:31181   # 直接测已运行实例

[CmdletBinding()]
param(
    [string]$Exe = ".\target\release\ds-proxy.exe",
    [string]$Config = "",
    [int]$Port = 31183,
    [string[]]$Targets = @(
        "https://linux.do/",
        "https://linux.do/latest.json",
        "https://discord.com/",
        "https://cdn.jsdelivr.net/npm/vue@3/dist/vue.global.js",
        "https://example.com/"
    ),
    [string]$ProxyUrl = "",
    [int]$Rounds = 3,
    [string]$OutFile = "",
    [switch]$KeepRunning
)

$ErrorActionPreference = "Stop"
$UserAgent = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36"
$ChallengeMarkers = @("Just a moment", "请稍候", "cf-browser-verification", "challenge-platform")
$script:proc = $null
$script:failures = 0

function Start-Proxy {
    if ($ProxyUrl) { return }
    if (-not (Test-Path $Exe)) { throw "找不到 $Exe（需要先 cargo build --release）" }

    $args = @("--port", $Port)
    if ($Config) { $args += @("--config", $Config) }

    Write-Host "启动：$Exe $($args -join ' ')" -ForegroundColor Cyan
    $script:proc = Start-Process -FilePath $Exe -ArgumentList $args -PassThru -NoNewWindow

    for ($i = 0; $i -lt 50; $i++) {
        Start-Sleep -Milliseconds 200
        if (Test-NetConnection -ComputerName 127.0.0.1 -Port $Port -InformationLevel Quiet -WarningAction SilentlyContinue) {
            Write-Host "端口 $Port 已就绪" -ForegroundColor Green
            return
        }
        if ($script:proc.HasExited) { throw "ds-proxy 启动后立刻退出，退出码 $($script:proc.ExitCode)" }
    }
    throw "等待端口 $Port 超时"
}

function Stop-Proxy {
    if ($script:proc -and -not $script:proc.HasExited) {
        if ($KeepRunning) { Write-Host "保留进程 $($script:proc.Id)" -ForegroundColor Yellow; return }
        Stop-Process -Id $script:proc.Id -Force -ErrorAction SilentlyContinue
    }
}

function Test-Target([string]$url, [int]$round) {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $body = ""
    $code = "000"
    try {
                # -k 必须加：命中 intercepts 的域名会被 MITM，叶子证书由本地 CA 现签，
        # curl 不认这个 CA，不加 -k 会在握手阶段被对端（curl）直接断开，
        # 表现成 "tls handshake eof"，看着像我们的 TLS 栈坏了。
        $resp = & curl.exe -sS -k -o - -w "`n%{http_code}" --max-time 30 --ssl-no-revoke `
            -x $proxy -A $UserAgent $url 2>$null
        $text = ($resp -join "`n")
        $idx = $text.LastIndexOf("`n")
        if ($idx -ge 0) {
            $code = $text.Substring($idx + 1).Trim()
            $body = $text.Substring(0, $idx)
        } else {
            $body = $text
        }
    } catch {
        $body = ""
    }
    $sw.Stop()

    $markerHit = $ChallengeMarkers | Where-Object { $body -like "*$_*" } | Select-Object -First 1
    # 只有「非 2xx + 命中标记」才算被挑战：CF 挑战页是 403/503；
    # 正常站点（如 discord 首页）正文里引用 challenge-platform 脚本属于常态。
    $hit = if ($markerHit -and ($code -ne "200" -and $code -ne "204")) { $markerHit } else { $null }
    $ok = ($code -eq "200" -or $code -eq "204") -and -not $hit

    $tag = if ($ok) { "PASS" } else { "FAIL" }
    $color = if ($ok) { "Green" } else { "Red" }
    $extra = if ($hit) { " (命中挑战标记: $hit)" } else { "" }
    Write-Host ("[{0}] 第{1}轮 {2} -> {3} / {4:N0}ms / {5} 字节{6}" -f `
        $tag, $round, $url, $code, $sw.Elapsed.TotalMilliseconds, $body.Length, $extra) -ForegroundColor $color

    if (-not $ok) { $script:failures++ }
    return [pscustomobject]@{ Url = $url; Code = $code; Ms = $sw.Elapsed.TotalMilliseconds; Len = $body.Length; Challenge = [bool]$hit; Ok = $ok }
}

$proxy = if ($ProxyUrl) { $ProxyUrl } else { "http://127.0.0.1:$Port" }

try {
    Start-Proxy
    Write-Host "代理：$proxy" -ForegroundColor Cyan

    $results = @()
    for ($round = 1; $round -le $Rounds; $round++) {
        foreach ($url in $Targets) {
            $results += Test-Target $url $round
            Start-Sleep -Milliseconds 300
        }
    }

    Write-Host ""
    Write-Host "=== 汇总 ===" -ForegroundColor Cyan
    $results | Group-Object Url | ForEach-Object {
        $okCount = ($_.Group | Where-Object Ok).Count
        $avg = ($_.Group | Measure-Object Ms -Average).Average
        $challenges = ($_.Group | Where-Object Challenge).Count
        $color = if ($okCount -eq $_.Count -and $challenges -eq 0) { "Green" } else { "Yellow" }
        Write-Host ("{0,-55} 通过 {1}/{2}  平均 {3:N0}ms  挑战 {4}" -f `
            $_.Name, $okCount, $_.Count, $avg, $challenges) -ForegroundColor $color
    }

    if ($OutFile) {
        $md = @()
        $md += "### CF 验收（linux.do 是否进挑战循环）"
        $md += ''
        $md += "| 目标 | 通过 | 平均耗时 | 命中挑战 | 末次状态码 |"
        $md += "| --- | --- | --- | --- | --- |"
        foreach ($group in ($results | Group-Object Url)) {
            $okCount = ($group.Group | Where-Object Ok).Count
            $avg = ($group.Group | Measure-Object Ms -Average).Average
            $challenges = ($group.Group | Where-Object Challenge).Count
            $last = $group.Group[-1]
            $md += "| $($group.Name) | $okCount/$($group.Count) | $([math]::Round($avg))ms | $challenges | $($last.Code) |"
        }
        $md += ''
        if ($script:failures -eq 0) {
            $md += '**结论：全部通过，未出现 Cloudflare 挑战页。**'
        } else {
            $md += '结论：' + $script:failures + ' 项失败（见上表「命中挑战」列）。'
            $md += ''
            $md += '若只有带挑战标记的站点失败，说明 TLS 指纹（JA3/JA4）被 CF 判定为非浏览器，'
            $md += '与 `docs/FINGERPRINT.md` 的实测结论一致 —— 这类站点必须走 BoringSSL 通道。'
        }
        ($md -join "`n") | Out-File -FilePath $OutFile -Encoding utf8
        Write-Host "报告已写入 $OutFile" -ForegroundColor Cyan
    }

    if ($script:failures -eq 0) {
        Write-Host "`n全部通过：linux.do 未出现 Cloudflare 挑战页" -ForegroundColor Green
        exit 0
    }

    Write-Host "`n失败 $($script:failures) 项（见上方 FAIL 行）" -ForegroundColor Red
    exit 1
} finally {
    Stop-Proxy
}

# acceptance run 1794794999

# run 1794795479

# run 1794795990
