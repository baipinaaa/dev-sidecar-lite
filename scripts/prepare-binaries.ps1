# 把数据面/控制面二进制放进 Tauri 的 sidecar 目录
#
# Tauri 的 externalBin 要求文件名带目标三元组后缀：
#   binaries/ds-proxy-x86_64-pc-windows-msvc.exe
#   binaries/ds-core-host-x86_64-pc-windows-msvc.exe
#
# 用法：
#   pwsh -File scripts/prepare-binaries.ps1            # 默认 release
#   pwsh -File scripts/prepare-binaries.ps1 -Profile debug

param(
    [ValidateSet('release', 'debug')]
    [string]$Profile = 'release'
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$triple = 'x86_64-pc-windows-msvc'
$destDir = Join-Path $root 'app/src-tauri/binaries'
New-Item -ItemType Directory -Force -Path $destDir | Out-Null

function Copy-Sidecar {
    param([string]$Name, [string]$Source)

    if (-not (Test-Path $Source)) {
        Write-Host "[skip] $Name 未构建：$Source" -ForegroundColor Yellow
        return
    }

    $dest = Join-Path $destDir "$Name-$triple.exe"
    Copy-Item $Source $dest -Force
    $size = [math]::Round((Get-Item $dest).Length / 1KB, 1)
    Write-Host "[ ok ] $Name -> $dest ($size KB)" -ForegroundColor Green
}

$targetDir = if ($Profile -eq 'release') { 'release' } else { 'debug' }
Copy-Sidecar -Name 'ds-proxy' -Source (Join-Path $root "target/$targetDir/ds-proxy.exe")
Copy-Sidecar -Name 'ds-core-host' -Source (Join-Path $root "core-host/dist/ds-core-host.exe")

Write-Host ''
Write-Host '提示：core-host 需要先打包（Node SEA）：' -ForegroundColor Cyan
Write-Host '  cd core-host; npm run build'
