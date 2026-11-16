#!/usr/bin/env pwsh
# B3：采集真 Chrome 的 TLS 指纹基线（不抓包，靠回显服务）。
#
# 原理：让 Chrome 访问 tls.peet.ws / tls.browserleaks.com，这两个服务会把
# 它们在对端看到的 ClientHello 解析结果原样返回，于是不需要在 CI 里装 npcap。
#
# 输出：$OutDir/baseline.json（机器可读）+ $OutDir/report.md（人读，含与 rustls 的差距清单）

[CmdletBinding()]
param(
    [string]$OutDir = "./baseline",
    [string]$ChromeExe = "",
    [string[]]$Endpoints = @(
        "https://tls.peet.ws/api/all",
        "https://tls.browserleaks.com/json"
    )
)

$ErrorActionPreference = "Continue"

$RustlsJa3 = "771,4866-4865-49196-49195-49200-49199-49188-49187-49192-49191,0-43-13-35-10-11-16-51-49-23-65281-45,24-23,0"
$RustlsJa3Hash = "5b81f3846b1faddcc37be70ed494c956"
$RustlsJa4 = "t13d1012h1_01be160bb49b_36bf25f296df"

if (-not (Test-Path $OutDir)) { New-Item -ItemType Directory -Path $OutDir | Out-Null }

function Resolve-Chrome {
    if ($ChromeExe) { return $ChromeExe }
    if ($env:CHROME_EXE -and (Test-Path $env:CHROME_EXE)) { return $env:CHROME_EXE }
    $paths = @(
        "C:\Program Files\Google\Chrome\Application\chrome.exe",
        "C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe"
    )
    foreach ($p in $paths) { if (Test-Path $p) { return $p } }
    throw "找不到 chrome.exe"
}

# 用 headless 抓页面；--dump-dom 会把渲染后的 DOM 打到 stdout
function Get-Dom([string]$exe, [string]$url, [string]$profile) {
    # UA 必须伪装成非 headless：headless UA 会被 CF 直接挑战，
    # 而 UA 只影响 HTTP 层，测的仍是我们关心的 TLS 层指纹。
    $ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36"
    $args = @(
        "--headless=new",
        "--disable-gpu",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-extensions",
        "--disable-blink-features=AutomationControlled",
        "--user-agent=$ua",
        "--user-data-dir=$profile",
        "--virtual-time-budget=15000",
        "--dump-dom",
        $url
    )
    $raw = & $exe @args 2>$null
    return ($raw -join "`n")
}

function Extract-Json([string]$dom) {
    # --dump-dom 给的是 HTML，JSON 在 <pre> 里；从第一个 { 到最后一个 } 截取
    $start = $dom.IndexOf("{")
    $end = $dom.LastIndexOf("}")
    if ($start -lt 0 -or $end -le $start) { return $null }
    $text = $dom.Substring($start, $end - $start + 1)
    $text = $text -replace "&quot;", '"' -replace "&amp;", "&" -replace "&lt;", "<" -replace "&gt;", ">"
    try { return ($text | ConvertFrom-Json) } catch { return $null }
}

function Split-Ja3([string]$ja3) {
    if (-not $ja3) { return $null }
    $parts = $ja3.Split(",")
    if ($parts.Count -lt 5) { return $null }
    return [pscustomobject]@{
        Version       = $parts[0]
        Ciphers       = @($parts[1].Split("-") | Where-Object { $_ -ne "" })
        Extensions    = @($parts[2].Split("-") | Where-Object { $_ -ne "" })
        Groups        = @($parts[3].Split("-") | Where-Object { $_ -ne "" })
        PointFormats  = @($parts[4].Split("-") | Where-Object { $_ -ne "" })
    }
}

function Diff-List([string]$name, [string[]]$mine, [string[]]$theirs) {
    $missing = @($theirs | Where-Object { $mine -notcontains $_ })
    $extra = @($mine | Where-Object { $theirs -notcontains $_ })
    return [pscustomobject]@{ Name = $name; Missing = $missing; Extra = $extra }
}

$exe = Resolve-Chrome
$chromeVersion = (Get-Item $exe).VersionInfo.ProductVersion
$profile = Join-Path $env:TEMP ("ja3-profile-" + [guid]::NewGuid().ToString("N").Substring(0, 8))

Write-Host "Chrome：$exe（$chromeVersion）" -ForegroundColor Cyan

$sample = $null
$usedEndpoint = ""
$rawDump = ""

foreach ($url in $Endpoints) {
    Write-Host "尝试 $url" -ForegroundColor Cyan
    for ($attempt = 1; $attempt -le 3 -and -not $sample; $attempt++) {
        $dom = Get-Dom $exe $url $profile
        if (-not $dom) { Start-Sleep -Seconds 2; continue }
        $json = Extract-Json $dom
        if (-not $json) {
            $head = $dom.Substring(0, [Math]::Min(400, $dom.Length))
            Write-Host "没解析出 JSON（dump 长度 $($dom.Length)），前 400 字符：" -ForegroundColor Yellow
            Write-Host $head -ForegroundColor DarkGray
            $dom | Out-File -FilePath (Join-Path $OutDir ("dump-" + [guid]::NewGuid().ToString("N").Substring(0, 6) + ".html")) -Encoding utf8
            Start-Sleep -Seconds 2
            continue
        }
        # 两种端点的字段名不一样，统一归一
        $ja3 = $null; $ja3hash = $null; $ja4 = $null; $peet = $null; $ver = $null
        if ($json.tls) {
            $ja3 = $json.tls.ja3
            $ja3hash = $json.tls.ja3_hash
            $ja4 = $json.tls.ja4
            $peet = $json.tls.peetprint_hash
            $ver = $json.tls.tls_version_negotiated
        } elseif ($json.ja3) {
            $ja3 = $json.ja3
            $ja3hash = $json.ja3_hash
            $peet = $json.peetprint_hash
            $ver = $json.tls_version_negotiated
        }
        if (-not $ja3) { Start-Sleep -Seconds 2; continue }
        $sample = [pscustomobject]@{ Ja3 = $ja3; Ja3Hash = $ja3hash; Ja4 = $ja4; Peet = $peet; Version = $ver; Source = $url; Raw = $json }
        $usedEndpoint = $url
        $rawDump = $dom
    }
    if ($sample) { break }
}

if (-not $sample) {
    Write-Host "采集失败：所有端点都没拿到 JA3（可能被 CF 拦截）" -ForegroundColor Red
    exit 1
}

Write-Host "JA3：$($sample.Ja3)" -ForegroundColor Green
Write-Host "JA4：$($sample.Ja4)" -ForegroundColor Green

$chrome = Split-Ja3 $sample.Ja3
$rustls = Split-Ja3 $RustlsJa3
$diffs = @(
    Diff-List "cipher suites" $rustls.Ciphers $chrome.Ciphers
    Diff-List "extensions" $rustls.Extensions $chrome.Extensions
    Diff-List "supported_groups" $rustls.Groups $chrome.Groups
    Diff-List "ec_point_formats" $rustls.PointFormats $chrome.PointFormats
)

$result = [pscustomobject]@{
    ChromeExe         = $exe
    ChromeVersion     = $chromeVersion
    Endpoint          = $usedEndpoint
    Chrome            = $sample.Ja3
    ChromeJa3Hash     = $sample.Ja3Hash
    ChromeJa4         = $sample.Ja4
    ChromePeetprint   = $sample.Peet
    ChromeTlsVersion  = $sample.Version
    Rustls            = $RustlsJa3
    RustlsJa3Hash     = $RustlsJa3Hash
    RustlsJa4         = $RustlsJa4
    Diffs             = $diffs
}
$result | ConvertTo-Json -Depth 6 | Out-File -FilePath (Join-Path $OutDir "baseline.json") -Encoding utf8
$rawDump | Out-File -FilePath (Join-Path $OutDir "chrome-dump.html") -Encoding utf8

$md = @()
$md += "#### Chrome 基线"
$md += ""
$md += "| 项 | 值 |"
$md += "| --- | --- |"
$md += "| Chrome | ``$chromeVersion`` |"
$md += "| 端点 | $usedEndpoint |"
$md += "| JA3 | ``$($sample.Ja3)`` |"
$md += "| JA3 hash | ``$($sample.Ja3Hash)`` |"
$md += "| JA4 | ``$($sample.Ja4)`` |"
$md += "| peetprint | ``$($sample.Peet)`` |"
$md += ""
$md += "#### 与 rustls（当前 ds-tls）的差距"
$md += ""
$md += "| 维度 | rustls 缺的 | rustls 多出来的 |"
$md += "| --- | --- | --- |"
foreach ($d in $diffs) {
    $missing = if ($d.Missing.Count -gt 0) { ($d.Missing -join ", ") } else { "—" }
    $extra = if ($d.Extra.Count -gt 0) { ($d.Extra -join ", ") } else { "—" }
    $md += "| $($d.Name) | $missing | $extra |"
}
$md += ""
$md += "顺序（Chrome → rustls）："
$md += ""
$md += "- ciphers: ``$($chrome.Ciphers -join "-")`` → ``$($rustls.Ciphers -join "-")``"
$md += "- extensions: ``$($chrome.Extensions -join "-")`` → ``$($rustls.Extensions -join "-")``"
$md += "- groups: ``$($chrome.Groups -join "-")`` → ``$($rustls.Groups -join "-")``"
($md -join "`n") | Out-File -FilePath (Join-Path $OutDir "report.md") -Encoding utf8

Write-Host "报告已写入 $(Join-Path $OutDir 'report.md')" -ForegroundColor Cyan
Remove-Item -Recurse -Force $profile -ErrorAction SilentlyContinue
exit 0
