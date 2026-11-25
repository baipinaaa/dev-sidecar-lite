#!/usr/bin/env pwsh
<#
.SYNOPSIS
    安装包冒烟测试：装 NSIS 包 → 验证安装布局 → 起数据面 → 卸包。

.DESCRIPTION
    为什么需要它：0.1.0 的安装包「能装上、能双击、点了没反应」，因为打包流程没有把
    core-bundle 放进安装目录，而且 workflow 里 `if-no-files-found: warn` 让空壳包一路绿灯发出去。
    这个脚本只做「装完之后到底能不能跑」这一件事，任何一个环节不通就 exit 1。

    断言链：
      1. 静默安装成功，`$INSTDIR` 里有 node 运行时与 core-bundle
      2. core-bundle 真的在安装目录里（`core-bundle/bundle/core.cjs`，S3 起是打平产物）
      3. `core-bundle/extra/sysproxy.exe` 存在（缺它则系统代理/PAC 全不可用）
      4. 用**安装目录里的** node + `core-bundle/core-host/src/main.js --stdio` 做一次完整 RPC 冒烟：
         `ready` 帧 → `getApiList` → `server.start` → `HEAD /pac.js` 200 → `server.close` → `shutdown`
      5. 静默卸载，安装目录被清掉

    全程用隔离的 USERPROFILE / DS_DATA_DIR，端口用高位随机端口，
    不碰用户真实配置，也不依赖本机是否装了 dev-sidecar。

    ⚠ 本脚本只在 CI（windows-latest，管理员）里跑；**本地不要跑**。

.EXAMPLE
    pwsh -File scripts/smoke-installer.ps1 -Installer app/src-tauri/target/release/bundle/nsis/"DevSidecar Lite_0.2.0_x64-setup.exe"
#>
param(
    [Parameter(Mandatory = $true)][string]$Installer,
    [int]$Port = 31197,
    [int]$ProxyPort = 31196,
    [int]$TimeoutSeconds = 150,
    [switch]$KeepInstalled
)

$ErrorActionPreference = 'Stop'

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

function Wait-Until {
    param(
        [scriptblock]$Condition,
        [int]$TimeoutSeconds = 60,
        [string]$What = '条件'
    )
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        if (& $Condition) { return $true }
        Start-Sleep -Milliseconds 500
    }
    Write-Host "[WARN] 等待超时：$What" -ForegroundColor Yellow
    return $false
}

$installerPath = Resolve-Path2 $Installer
if (-not (Test-Path $installerPath)) {
    Write-Host "[FAIL] 找不到安装包：$installerPath" -ForegroundColor Red
    exit 1
}

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host '[FAIL] perMachine 安装需要管理员权限（CI runner 默认是管理员；本地请勿运行本脚本）' -ForegroundColor Red
    exit 1
}

$installDir = Join-Path $env:ProgramFiles 'DevSidecar Lite'
$appExe = Join-Path $installDir 'DevSidecar Lite.exe'
$bundleDir = Join-Path $installDir 'core-bundle'
# 控制面 = 随包的 node 运行时 + core-bundle 里的 js（v0.3.0 起不再是 SEA 单文件 exe）
$nodeExe = Join-Path $bundleDir 'node/node.exe'
$entryJs = Join-Path $bundleDir 'core-host/src/main.js'
$corePkg = Join-Path $bundleDir 'bundle/core.cjs'
$mitmproxyPkg = Join-Path $bundleDir 'bundle/mitmproxy.cjs'
$extraDir = Join-Path $bundleDir 'extra'

# 中间产物留在项目内 .build/（不要写 C 盘临时目录）
$buildRoot = Join-Path $root '.build'
New-Item -ItemType Directory -Force -Path $buildRoot | Out-Null
$work = Join-Path $buildRoot ('installer-smoke-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$dataDir = Join-Path $work 'data'
$homeDir = Join-Path $work 'home'
New-Item -ItemType Directory -Force -Path $work, $dataDir, (Join-Path $homeDir '.dev-sidecar-lite') | Out-Null

$detail = ''
$ok = $false
$proc = $null
$installed = $false

function Uninstall-App {
    if (-not $installed) { return }
    $uninstallers = @()
    $regRoots = @(
        'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
        'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall',
        'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall'
    )
    foreach ($regRoot in $regRoots) {
        if (-not (Test-Path $regRoot)) { continue }
        Get-ChildItem $regRoot -ErrorAction SilentlyContinue | ForEach-Object {
            $props = Get-ItemProperty $_.PSPath -ErrorAction SilentlyContinue
            if ($props.DisplayName -like 'DevSidecar Lite*' -and $props.UninstallString) {
                $uninstallers += $props.UninstallString
            }
        }
    }
    if (Test-Path (Join-Path $installDir 'uninstall.exe')) {
        $uninstallers += ('"' + (Join-Path $installDir 'uninstall.exe') + '"')
    }
    foreach ($raw in $uninstallers) {
        $exe = $raw.Trim('"')
        if ($exe -match '^(.+?\.exe)"?\s*(.*)$') {
            $exe = $Matches[1]
            $extraArgs = $Matches[2]
        } else {
            $extraArgs = ''
        }
        if (-not (Test-Path $exe)) { continue }
        Write-Host "  卸载：$exe /S $extraArgs"
        $argList = @('/S')
        if ($extraArgs) { $argList += ($extraArgs -split '\s+') }
        Start-Process -FilePath $exe -ArgumentList $argList -Wait -WindowStyle Hidden
        break
    }
    Wait-Until -TimeoutSeconds 60 -What '卸载完成' -Condition {
        -not (Test-Path $entryJs)
    } | Out-Null
}

function Show-Diagnostics {
    # 安装失败时把「到底装到哪去了」打出来，否则 CI 日志只有一句「没找到 exe」
    Write-Host '--- 诊断：候选安装目录 ---'
    $roots = @($env:ProgramFiles, ${env:ProgramFiles(x86)}, (Join-Path $env:LOCALAPPDATA 'Programs'), $env:ProgramData)
    foreach ($r in $roots) {
        if (-not $r -or -not (Test-Path $r)) { continue }
        Get-ChildItem $r -Directory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -like '*DevSidecar*' -or $_.Name -like '*dev-sidecar*' } |
            ForEach-Object { Write-Host "    $($_.FullName)" }
    }
    Write-Host '--- 诊断：卸载注册项 ---'
    @('HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
        'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall',
        'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall') | ForEach-Object {
        if (-not (Test-Path $_)) { return }
        Get-ChildItem $_ -ErrorAction SilentlyContinue | ForEach-Object {
            $p = Get-ItemProperty $_.PSPath -ErrorAction SilentlyContinue
            if ($p.DisplayName -like '*DevSidecar*') {
                Write-Host "    $($p.DisplayName) -> $($p.InstallLocation) [$($p.DisplayVersion)]"
            }
        }
    }
}

try {
    Write-Host "==> 1/8 静默安装：$installerPath"
    # 显式给 /D=：NSIS 的 /D 必须是最后一个参数且不能带引号，所以走 cmd 拼一次命令行；
    # 同时拿退出码——只等目录出现的话，安装器直接失败时日志里什么都看不到。
    $installCmd = '"' + $installerPath + '" /S /D=' + $installDir
    $installProc = Start-Process -FilePath $env:ComSpec -ArgumentList '/c', $installCmd -PassThru -Wait -WindowStyle Hidden
    Write-Host "    安装器退出码：$($installProc.ExitCode)"
    $installed = $true

    # 不要把主程序 exe 名写死：Tauri 用 productName 命名，改过 productName/mainBinaryName
    # 就会出现「装好了但脚本等不到 DevSidecar Lite.exe」这种假失败
    if (-not (Wait-Until -TimeoutSeconds 180 -What '安装目录出现 core-bundle' -Condition {
                (Test-Path $nodeExe) -and (Test-Path (Join-Path $bundleDir 'node_modules'))
            })) {
        Show-Diagnostics
        # 兜底：万一 NSIS 把目录名换了（productName 带后缀等），只要里面有一份带
        # core-bundle/node/node.exe 的目录就认它，继续验证真实布局
        $alt = @($env:ProgramFiles, ${env:ProgramFiles(x86)}, (Join-Path $env:LOCALAPPDATA 'Programs')) |
            Where-Object { $_ -and (Test-Path $_) } |
            ForEach-Object { Get-ChildItem $_ -Directory -ErrorAction SilentlyContinue } |
            Where-Object { Test-Path (Join-Path $_.FullName 'core-bundle/node/node.exe') } |
            Select-Object -First 1
        if ($alt) {
            Write-Host "    [WARN] 安装目录名与预期不同，改用：$($alt.FullName)" -ForegroundColor Yellow
            $installDir = $alt.FullName
            $appExe = Join-Path $installDir 'DevSidecar Lite.exe'
            $bundleDir = Join-Path $installDir 'core-bundle'
            $nodeExe = Join-Path $bundleDir 'node/node.exe'
            $entryJs = Join-Path $bundleDir 'core-host/src/main.js'
            $corePkg = Join-Path $bundleDir 'bundle/core.cjs'
            $mitmproxyPkg = Join-Path $bundleDir 'bundle/mitmproxy.cjs'
            $extraDir = Join-Path $bundleDir 'extra'
        } else {
            $detail = "安装后没有找到 $appExe / $nodeExe（安装器退出码 $($installProc.ExitCode)）"
            throw $detail
        }
    }
    $mainExe = Get-ChildItem $installDir -Filter '*.exe' -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -notin @('node.exe', 'uninstall.exe') } |
        Select-Object -First 1
    if ($mainExe) {
        $appExe = $mainExe.FullName
    } else {
        Write-Host '    [WARN] 安装目录里没有主程序 exe' -ForegroundColor Yellow
        $appExe = $null
    }
    Write-Host "    安装完成（主程序：$(if ($appExe) { $appExe } else { '未找到' })）"

    Write-Host '==> 2/8 校验安装布局'
    if (-not (Test-Path $nodeExe)) {
        $detail = "缺少 node 运行时：$nodeExe（打包步骤没跑 prepare-core-bundle）"
        throw $detail
    }
    if (-not (Test-Path $entryJs)) {
        $detail = "缺少控制面入口：$entryJs"
        throw $detail
    }
    if (-not (Test-Path (Join-Path $corePkg 'package.json'))) {
        $detail = "缺少 core 打平产物：$corePkg（打包步骤没跑 prepare-core-bundle 或打平失败）"
        throw $detail
    }
    if (-not (Test-Path (Join-Path $mitmproxyPkg 'package.json'))) {
        $detail = "缺少 mitmproxy：$mitmproxyPkg"
        throw $detail
    }
    if (-not (Test-Path (Join-Path $extraDir 'sysproxy.exe'))) {
        $detail = "缺少 $extraDir\sysproxy.exe（没有它 core 的系统代理与 PAC 全不可用）"
        throw $detail
    }
    Write-Host '    布局正确'

    Write-Host '==> 3/8 准备隔离配置'
    @{
        server = @{ port = $ProxyPort; enabled = $false; startup = $false }
        proxy  = @{ enabled = $false; pacEnabled = $false }
        app    = @{ autoStart = @{ enabled = $false } }
    } | ConvertTo-Json -Depth 6 | Set-Content -Path (Join-Path $homeDir '.dev-sidecar-lite/config.json') -Encoding utf8NoBOM

    $env:USERPROFILE = $homeDir
    $env:HOME = $homeDir
    $env:DS_DATA_DIR = $dataDir

    Write-Host '==> 4/6 控制面 stdio 冒烟（ready → getApiList → 数据面 → /pac.js → 关闭）'
    $probe = Join-Path $root 'scripts/rpc-probe.cjs'
    if (-not (Test-Path -LiteralPath $probe)) {
        $detail = "找不到探针脚本：$probe"
        throw $detail
    }

    & $nodeExe $probe --node $nodeExe --entry $entryJs `
        --data-dir $dataDir --home $homeDir --port $ProxyPort --timeout ($TimeoutSeconds * 1000)
    if ($LASTEXITCODE -ne 0) {
        $detail = "控制面 stdio 冒烟失败（退出码 $LASTEXITCODE，看上面 [失败] 行）"
        throw $detail
    }
    Write-Host '    控制面与数据面冒烟均通过'

    Write-Host '==> 5/6 卸载'
    if ($proc -and -not $proc.HasExited) {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    }
    if ($KeepInstalled) {
        Write-Host '    -KeepInstalled：跳过卸载'
        $installed = $false
    } else {
        Uninstall-App
        if ((Test-Path $entryJs) -or ($appExe -and (Test-Path $appExe))) {
            $detail = '卸载之后安装目录里仍有主程序'
            throw $detail
        }
        $installed = $false
        Write-Host '    卸载完成'
    }

    $ok = $true
    $detail = "安装→布局→控制面 stdio 冒烟（ready/getApiList/数据面 /pac.js 200/close）→卸载，全链路通过"
} catch {
    $detail = $_.Exception.Message
} finally {
    if ($proc -and -not $proc.HasExited) {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    }
    # 数据面子进程是 fork 出来的，杀父进程不一定带走它
    Get-CimInstance Win32_Process -Filter "Name='node.exe'" -ErrorAction SilentlyContinue |
        Where-Object { $_.CommandLine -like '*core-bundle*' } |
        Where-Object { $_.CommandLine -like "*$installDir*" -or $_.CommandLine -like "*$work*" } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }

    if (-not $ok -and -not $KeepInstalled) {
        Write-Host '--- 卸载（失败清理） ---'
        try { Uninstall-App } catch { Write-Host "  卸载失败：$($_.Exception.Message)" -ForegroundColor Yellow }
    }
}

Write-Host ''
if (Test-Path (Join-Path $work 'core-host.out.log')) {
    Write-Host '--- core-host stdout（末尾 30 行）---'
    Get-Content (Join-Path $work 'core-host.out.log') -Tail 30
}
if (Test-Path (Join-Path $work 'core-host.err.log')) {
    Write-Host '--- core-host stderr（末尾 30 行）---'
    Get-Content (Join-Path $work 'core-host.err.log') -Tail 30
}

if ($ok) {
    Write-Host "[PASS] $detail" -ForegroundColor Green
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
    exit 0
}

Write-Host "[FAIL] $detail" -ForegroundColor Red
Write-Host "工作目录保留供排查：$work" -ForegroundColor Yellow
exit 1
