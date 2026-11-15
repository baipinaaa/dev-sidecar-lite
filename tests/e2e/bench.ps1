# 单次基准测试：对指定代理端口跑固定负载，输出一段可直接粘进 docs/BENCHMARK.md 的表格
#
# 用法：
#   pwsh -File tests/e2e/bench.ps1 -Label node -ProxyUrl http://127.0.0.1:31181
#   pwsh -File tests/e2e/bench.ps1 -Label rust -ProxyUrl http://127.0.0.1:31182
#
# 两边的负载完全相同（同一批 URL、同一批字节数、同样的并发度），差别只有数据面引擎。

param(
    [Parameter(Mandatory = $true)][string]$Label,
    [string]$ProxyUrl = 'http://127.0.0.1:31181',
    [string]$ProcessName = 'dev-sidecar',
    [int]$ProcessId = 0,
    [int]$Concurrency = 8,
    [int]$DownloadBytes = 8 * 1024 * 1024,
    [int]$Rounds = 3,
    [string]$OutFile = ''
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$ttfbTargets = @(
    'https://www.baidu.com/',
    'https://github.com/',
    'https://steamcommunity.com/sharedfiles',
    'https://api.github.com/',
    'https://www.bing.com/'
)

$downloadUrl = 'https://github.com/baipinaaa/dev-sidecar-clean/releases/latest/download/latest.yml'

function Get-ProcessCpu {
    param([int]$Id)
    $p = Get-Process -Id $Id -ErrorAction SilentlyContinue
    if ($null -eq $p) { return $null }
    return [pscustomobject]@{ Cpu = $p.CPU; MemoryMB = [math]::Round($p.WorkingSet64 / 1MB, 1) }
}

function Resolve-TargetProcess {
    if ($ProcessId -gt 0) { return $ProcessId }
    $p = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Sort-Object StartTime | Select-Object -First 1
    if ($null -eq $p) { return 0 }
    return $p.Id
}

Write-Host "=== 基准测试：$Label（$ProxyUrl）===" -ForegroundColor Cyan
$pidOfProxy = Resolve-TargetProcess

# ---------- 1) TTFB / 总耗时 ----------
$ttfbRows = foreach ($url in $ttfbTargets) {
    $samples = foreach ($i in 1..$Rounds) {
        $raw = & curl.exe -s -o NUL --proxy $ProxyUrl --ssl-no-revoke `
            -A 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36' `
            -w '%{http_code} %{time_starttransfer} %{time_total}' $url 2>$null
        $parts = "$raw" -split '\s+'
        [pscustomobject]@{
            Code = [int]$parts[0]
            Ttfb = [double]$parts[1]
            Total = [double]$parts[2]
        }
    }
    $ok = @($samples | Where-Object { $_.Code -ge 200 -and $_.Code -lt 400 })
    [pscustomobject]@{
        Url = $url
        Code = ($samples | Select-Object -Last 1).Code
        TtfbMs = if ($ok.Count) { [math]::Round((($ok | Measure-Object Ttfb -Average).Average) * 1000) } else { $null }
        TotalMs = if ($ok.Count) { [math]::Round((($ok | Measure-Object Total -Average).Average) * 1000) } else { $null }
        Pass = $ok.Count
        Rounds = $Rounds
    }
}

# ---------- 2) 单连接吞吐 ----------
$range = "0-$($DownloadBytes - 1)"
$before = if ($pidOfProxy) { Get-ProcessCpu -Id $pidOfProxy } else { $null }
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$bytes = & curl.exe -s -o NUL --proxy $ProxyUrl --ssl-no-revoke -r $range `
    -w '%{size_download} %{time_total}' $downloadUrl 2>$null
$sw.Stop()
$after = if ($pidOfProxy) { Get-ProcessCpu -Id $pidOfProxy } else { $null }

$single = "$bytes" -split '\s+'
$singleBytes = [int64]$single[0]
$singleSec = [double]$single[1]
$singleMbps = if ($singleSec -gt 0) { [math]::Round($singleBytes / 1MB / $singleSec, 2) } else { 0 }
$cpuDelta = if ($before -and $after) { [math]::Round(($after.Cpu - $before.Cpu) / $singleSec * 100, 0) } else { $null }

# ---------- 3) 并发吞吐 ----------
$jobs = foreach ($i in 1..$Concurrency) {
    Start-Job -ScriptBlock {
        param($url, $proxy, $range)
        $out = & curl.exe -s -o NUL --proxy $proxy --ssl-no-revoke -r $range -w '%{size_download} %{time_total}' $url 2>$null
        $out
    } -ArgumentList $downloadUrl, $ProxyUrl, $range
}

$beforeConcurrent = if ($pidOfProxy) { Get-ProcessCpu -Id $pidOfProxy } else { $null }
$sw2 = [System.Diagnostics.Stopwatch]::StartNew()
$results = $jobs | Wait-Job | Receive-Job
$jobs | Remove-Job -Force
$sw2.Stop()
$afterConcurrent = if ($pidOfProxy) { Get-ProcessCpu -Id $pidOfProxy } else { $null }

$totalBytes = 0
foreach ($line in $results) {
    $p = "$line" -split '\s+'
    if ($p.Count -ge 1 -and $p[0]) { $totalBytes += [int64]$p[0] }
}
$concurrentMbps = if ($sw2.Elapsed.TotalSeconds -gt 0) { [math]::Round($totalBytes / 1MB / $sw2.Elapsed.TotalSeconds, 2) } else { 0 }
$concurrentCpu = if ($beforeConcurrent -and $afterConcurrent -and $sw2.Elapsed.TotalSeconds -gt 0) {
    [math]::Round(($afterConcurrent.Cpu - $beforeConcurrent.Cpu) / $sw2.Elapsed.TotalSeconds * 100, 0)
} else { $null }

# ---------- 输出 ----------
Write-Host "`n--- TTFB ---" -ForegroundColor Cyan
$ttfbRows | Format-Table -AutoSize | Out-String | Write-Host

Write-Host "--- 吞吐 ---" -ForegroundColor Cyan
Write-Host ("单连接: {0} MB / {1:N2}s = {2} MB/s，本进程 CPU {3}%" -f `
    [math]::Round($singleBytes / 1MB, 2), $singleSec, $singleMbps, $cpuDelta)
Write-Host ("{0} 并发: {1} MB / {2:N2}s = {3} MB/s，本进程 CPU {4}%" -f `
    $Concurrency, [math]::Round($totalBytes / 1MB, 2), $sw2.Elapsed.TotalSeconds, $concurrentMbps, $concurrentCpu)
if ($after) { Write-Host ("内存: {0} MB" -f $after.MemoryMB) }

$md = @()
$md += "#### $Label"
$md += ''
$md += '| 项目 | 数值 |'
$md += '| --- | --- |'
foreach ($row in $ttfbRows) {
    $md += "| TTFB $($row.Url) | $(if ($null -ne $row.TtfbMs) { "$($row.TtfbMs) ms / 总 $($row.TotalMs) ms（$($row.Pass)/$($row.Rounds)）" } else { "失败（HTTP $($row.Code)）" }) |"
}
$md += "| 单连接吞吐 | $singleMbps MB/s（$([math]::Round($singleBytes / 1MB, 2)) MB / $([math]::Round($singleSec, 2)) s） |"
$md += "| 单连接 CPU | $(if ($null -ne $cpuDelta) { "$cpuDelta%" } else { 'n/a' }) |"
$md += "| $Concurrency 并发吞吐 | $concurrentMbps MB/s（$([math]::Round($totalBytes / 1MB, 2)) MB） |"
$md += "| $Concurrency 并发 CPU | $(if ($null -ne $concurrentCpu) { "$concurrentCpu%" } else { 'n/a' }) |"
$md += "| 内存峰值 | $(if ($after) { "$($after.MemoryMB) MB" } else { 'n/a' }) |"
$md += ''

$mdText = $md -join "`n"
Write-Host "`n--- 可粘贴的 Markdown ---" -ForegroundColor Cyan
Write-Host $mdText

if ($OutFile) {
    Add-Content -Path $OutFile -Value $mdText -Encoding utf8
    Write-Host "`n已追加到 $OutFile" -ForegroundColor Green
}
