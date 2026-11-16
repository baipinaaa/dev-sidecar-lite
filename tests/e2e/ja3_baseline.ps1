#!/usr/bin/env pwsh
# B3：采集真 Chrome 的 TLS 指纹基线。
#
# 为什么不走回显服务（tls.peet.ws / browserleaks）：
#   - Chrome 132+ 的新 headless 已移除 `--dump-dom`，拿不到页面正文；
#   - 就算拿到了，headless UA 也会被 CF 挑战，返回的是挑战页而不是 JSON。
# 所以改成最确定的办法：自己当一次 HTTP 代理，只截 CONNECT 之后的第一段 TLS 字节，
# 再用 tests/e2e/ja3_from_pcap.py 的解析器算 JA3/JA4（见 clienthello_capture.py）。
#
# 输出：$OutDir/baseline.json（机器可读）+ $OutDir/report.md（人读，含与 rustls 的差距清单）

[CmdletBinding()]
param(
    [string]$OutDir = "./baseline",
    [string]$ChromeExe = "",
    [string]$TargetUrl = "https://example.com/",
    [int]$TimeoutSeconds = 30
)

$ErrorActionPreference = "Continue"
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path

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

function Resolve-Python {
    foreach ($c in @("python", "python3", "py")) {
        $cmd = Get-Command $c -ErrorAction SilentlyContinue
        if ($cmd) { return $cmd.Source }
    }
    throw "找不到 python"
}

function Split-Ja3([string]$ja3) {
    if (-not $ja3) { return $null }
    $parts = $ja3.Split(",")
    if ($parts.Count -lt 5) { return $null }
    return [pscustomobject]@{
        Version      = $parts[0]
        Ciphers      = @($parts[1].Split("-") | Where-Object { $_ -ne "" })
        Extensions   = @($parts[2].Split("-") | Where-Object { $_ -ne "" })
        Groups       = @($parts[3].Split("-") | Where-Object { $_ -ne "" })
        PointFormats = @($parts[4].Split("-") | Where-Object { $_ -ne "" })
    }
}

function Diff-List([string]$name, [string[]]$mine, [string[]]$theirs) {
    $missing = @($theirs | Where-Object { $mine -notcontains $_ })
    $extra = @($mine | Where-Object { $theirs -notcontains $_ })
    return [pscustomobject]@{ Name = $name; Missing = $missing; Extra = $extra }
}

$exe = Resolve-Chrome
$py = Resolve-Python
$chromeVersion = (Get-Item $exe).VersionInfo.ProductVersion
$profile = Join-Path $env:TEMP ("ja3-profile-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
$helloFile = Join-Path $OutDir "chrome-clienthello.bin"
$recLog = Join-Path $OutDir "recorder.log"

$port = 18000 + (Get-Random -Maximum 700)

Write-Host "Chrome：$exe（$chromeVersion）" -ForegroundColor Cyan
Write-Host "python：$py" -ForegroundColor Cyan

# 1) 起嗅探代理
$rec = Start-Process -FilePath $py `
    -ArgumentList @("-u", (Join-Path $scriptDir "clienthello_capture.py"), "--listen", $port, "--out", $helloFile, "--timeout", $TimeoutSeconds) `
    -PassThru -NoNewWindow -RedirectStandardOutput $recLog -RedirectStandardError "$recLog.err"
Start-Sleep -Milliseconds 800

# 2) 让 Chrome 通过它访问（页面内容不重要，只要它发 ClientHello）
$chromeArgs = @(
    "--headless=new",
    "--disable-gpu",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-extensions",
    "--proxy-server=http://127.0.0.1:$port",
    "--user-data-dir=$profile",
    "--ignore-certificate-errors",
    $TargetUrl
)
$chrome = Start-Process -FilePath $exe -ArgumentList $chromeArgs -PassThru -NoNewWindow

# 3) 等文件出现
$deadline = (Get-Date).AddSeconds($TimeoutSeconds + 5)
while (-not (Test-Path $helloFile) -and (Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 300
}

if (-not (Test-Path $helloFile)) {
    Write-Host "未捕获到 ClientHello，recorder 日志：" -ForegroundColor Red
    if (Test-Path $recLog) { Get-Content $recLog | ForEach-Object { Write-Host "  $_" } }
    if (Test-Path "$recLog.err") { Get-Content "$recLog.err" | Select-Object -First 20 | ForEach-Object { Write-Host "  $_" } }
}

# 4) 收尾
foreach ($p in @($chrome, $rec)) {
    if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
}
Remove-Item -Recurse -Force $profile -ErrorAction SilentlyContinue

if (-not (Test-Path $helloFile)) {
    Write-Host "采集失败：没有 ClientHello 字节" -ForegroundColor Red
    exit 1
}

# 5) 算指纹
$jsonText = & $py (Join-Path $scriptDir "clienthello_capture.py") --compute $helloFile
if (-not $jsonText) {
    Write-Host "解析失败：compute 没有输出" -ForegroundColor Red
    exit 1
}
$mine = ($jsonText -join "`n") | ConvertFrom-Json
if ($mine.error) {
    Write-Host "解析失败：$($mine.error)" -ForegroundColor Red
    exit 1
}

Write-Host "Chrome JA3：$($mine.ja3)" -ForegroundColor Green
Write-Host "Chrome JA3 hash：$($mine.ja3_hash)" -ForegroundColor Green
Write-Host "Chrome JA4：$($mine.ja4)" -ForegroundColor Green

$chromeP = Split-Ja3 $mine.ja3
$rustlsP = Split-Ja3 $RustlsJa3
$diffs = @(
    Diff-List "cipher suites" $rustlsP.Ciphers $chromeP.Ciphers
    Diff-List "extensions" $rustlsP.Extensions $chromeP.Extensions
    Diff-List "supported_groups" $rustlsP.Groups $chromeP.Groups
    Diff-List "ec_point_formats" $rustlsP.PointFormats $chromeP.PointFormats
)

[pscustomobject]@{
    ChromeExe        = $exe
    ChromeVersion    = $chromeVersion
    TargetUrl        = $TargetUrl
    ChromeJa3        = $mine.ja3
    ChromeJa3Hash    = $mine.ja3_hash
    ChromeJa4        = $mine.ja4
    ChromeAlpn       = $mine.alpn
    ChromeBytes      = $mine.bytes
    RustlsJa3        = $RustlsJa3
    RustlsJa3Hash    = $RustlsJa3Hash
    RustlsJa4        = $RustlsJa4
    Diffs            = $diffs
} | ConvertTo-Json -Depth 6 | Out-File -FilePath (Join-Path $OutDir "baseline.json") -Encoding utf8

$md = @()
$md += "#### Chrome 基线"
$md += ""
$md += "| 项 | 值 |"
$md += "| --- | --- |"
$md += "| Chrome | ``$chromeVersion`` |"
$md += "| 采集方式 | 本地嗅探代理截 ClientHello（无回显服务） |"
$md += "| JA3 | ``$($mine.ja3)`` |"
$md += "| JA3 hash | ``$($mine.ja3_hash)`` |"
$md += "| JA4 | ``$($mine.ja4)`` |"
$md += "| ALPN | ``$($mine.alpn -join ', ')`` |"
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
$md += "顺序对照（Chrome → rustls）："
$md += ""
$md += "- ciphers: ``$($chromeP.Ciphers -join '-')`` → ``$($rustlsP.Ciphers -join '-')``"
$md += "- extensions: ``$($chromeP.Extensions -join '-')`` → ``$($rustlsP.Extensions -join '-')``"
$md += "- groups: ``$($chromeP.Groups -join '-')`` → ``$($rustlsP.Groups -join '-')``"
($md -join "`n") | Out-File -FilePath (Join-Path $OutDir "report.md") -Encoding utf8

Write-Host "报告：$(Join-Path $OutDir 'report.md')" -ForegroundColor Cyan
exit 0
