# 指纹对比：把「我们像不像 Chrome」变成一组数字
#
# 原理：不抓包，直接问服务端。tls.peet.ws / tls.browserleaks.com 会把收到的 ClientHello
# 解析成 JA3/JA4 返回，所以只要让流量经过被测引擎，就能读到"对方眼里的我们"。
#
# 用法：
#   pwsh -File tests/e2e/fingerprint_compare.ps1 -Label rust -ProxyUrl http://127.0.0.1:31182
#   pwsh -File tests/e2e/fingerprint_compare.ps1 -Label node -ProxyUrl http://127.0.0.1:31181

param(
    [Parameter(Mandatory = $true)][string]$Label,
    [string]$ProxyUrl = 'http://127.0.0.1:31181',
    [string]$Url = 'https://tls.peet.ws/api/all',
    [string]$OutFile = ''
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$ua = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36'

Write-Host "=== 指纹采集：$Label（$ProxyUrl）===" -ForegroundColor Cyan

$code = 0
$body = ''
$attempts = 3
for ($i = 1; $i -le $attempts; $i++) {
    $raw = & curl.exe -s --proxy $ProxyUrl --ssl-no-revoke --max-time 30 -A $ua -w "`n%{http_code}" $Url 2>&1
    $text = "$raw"
    $parts = $text -split "`n"
    $last = "$($parts[-1])".Trim()
    if ($last -match '^[0-9]{3}$') {
        $code = [int]$last
        $body = ($parts[0..($parts.Length - 2)] -join "`n")
    } else {
        $code = 0
        $body = $text
    }
    if ($code -ge 200 -and $code -lt 300) { break }
    Write-Host "第 $i 次尝试失败：HTTP $code" -ForegroundColor Yellow
    if ($i -lt $attempts) { Start-Sleep -Seconds 5 }
}

if ($code -lt 200 -or $code -ge 300) {
    Write-Host "请求失败：HTTP $code" -ForegroundColor Red
    Write-Host $body
    if ($OutFile) {
        $head = @("# $Label 采集失败", "", "HTTP ``$code``", "", '```', ($body -split "`n" | Select-Object -First 40), '```')
        ($head -join "`n") | Out-File -FilePath $OutFile -Append -Encoding utf8
    }
    exit 2
}

$json = $body | ConvertFrom-Json

$ja3 = $json.tls.ja3_hash
$ja3Text = $json.tls.ja3
$ja4 = $json.tls.ja4
$peetPrint = $json.tls.peetprint_hash
$akamai = $json.http2.akamai_fingerprint_hash
$tlsVersion = $json.tls.tls_version_negotiated
$cipher = $json.tls.cipher_suite

# Chrome 149 的参考值（本机 Chromium 实测，见 docs/FINGERPRINT.md）
$chromeRef = @{
    Ja3        = $env:DS_CHROME_JA3
    Ja4        = $env:DS_CHROME_JA4
    AkamaiHash = $env:DS_CHROME_AKAMAI
}

function Verdict {
    param([string]$Mine, [string]$Ref)
    if ([string]::IsNullOrWhiteSpace($Ref)) { return '未知（未提供参考值）' }
    if ($Mine -eq $Ref) { return '一致' }
    return '不一致'
}

$rows = @(
    [pscustomobject]@{ 项目 = 'JA3'; 实测 = $ja3; Chrome 参考 = $chromeRef.Ja3; 判定 = (Verdict $ja3 $chromeRef.Ja3) }
    [pscustomobject]@{ 项目 = 'JA4'; 实测 = $ja4; Chrome 参考 = $chromeRef.Ja4; 判定 = (Verdict $ja4 $chromeRef.Ja4) }
    [pscustomobject]@{ 项目 = 'Akamai h2'; 实测 = $akamai; Chrome 参考 = $chromeRef.AkamaiHash; 判定 = (Verdict $akamai $chromeRef.AkamaiHash) }
    [pscustomobject]@{ 项目 = 'TLS 版本'; 实测 = $tlsVersion; Chrome 参考 = '772 (1.3)'; 判定 = '-' }
    [pscustomobject]@{ 项目 = '套件'; 实测 = $cipher; Chrome 参考 = '4865 (TLS_AES_128_GCM_SHA256)'; 判定 = '-' }
)

$rows | Format-Table -AutoSize | Out-String | Write-Host

Write-Host '--- JA3 原文（扩展顺序 + 套件 + 曲线） ---'
Write-Host $ja3Text

$md = @()
$md += "#### $Label"
$md += ''
$md += '| 项目 | 实测 | Chrome 参考 | 判定 |'
$md += '| --- | --- | --- | --- |'
foreach ($row in $rows) {
    $md += "| $($row.项目) | ``$($row.实测)`` | ``$($row.'Chrome 参考')`` | $($row.判定) |"
}
$md += ''
$md += "JA3 原文：``$ja3Text``"
$md += ''

$mdText = $md -join "`n"
Write-Host "`n--- 可粘贴的 Markdown ---" -ForegroundColor Cyan
Write-Host $mdText

if ($OutFile) {
    Add-Content -Path $OutFile -Value $mdText -Encoding utf8
    Write-Host "已追加到 $OutFile" -ForegroundColor Green
}
