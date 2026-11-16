'use strict'

/**
 * 把 core-host 打包成单文件可执行（Node SEA）。
 *
 * 前提：core 的依赖（lodash/@blue-frontier/mitmproxy 等）必须已安装，SEA 只打包我们的
 * 入口脚本，运行时仍然从磁盘 `require` 依赖。所以发布目录结构是：
 *
 *   DevSidecar Lite/
 *   ├─ dev-sidecar-lite.exe         # Tauri 壳
 *   ├─ ds-core-host.exe             # 本脚本产物
 *   ├─ ds-proxy.exe                 # Rust 数据面
 *   └─ resources/app/               # node_modules（含 core 与 mitmproxy）
 *
 * 用法：
 *   node scripts/build-sea.js          # 需要 node >= 20 与 postject
 */

const { execFileSync } = require('node:child_process')
const fs = require('node:fs')
const path = require('node:path')

const root = path.join(__dirname, '..')
const distDir = path.join(root, 'dist')
const blobPath = path.join(distDir, 'ds-core-host.blob')
const outPath = path.join(distDir, 'ds-core-host.exe')

function run (command, args, extra = {}) {
  console.log(`$ ${command} ${args.join(' ')}`)
  execFileSync(command, args, { stdio: 'inherit', cwd: root, ...extra })
}

function main () {
  fs.mkdirSync(distDir, { recursive: true })

  // 0) 先打包成单文件。
  //
  // 必须这么做：SEA 入口里的 `require('./host')` 会报
  // `ERR_UNKNOWN_BUILTIN_MODULE: No such built-in module: ./host`
  // —— 内嵌入口的 require 只支持内置模块，不能加载同目录的其它源码文件。
  // esbuild 把 host/dispatch/engine/runtime 全部内联，只把 core 与 mitmproxy
  // 留成运行时 require（它们来自磁盘上的 resources/core-bundle）。
  const npx = process.platform === 'win32' ? 'npx.cmd' : 'npx'
  run(npx, [
    '--yes',
    'esbuild@0.24.0',
    'src/main.js',
    '--bundle',
    '--platform=node',
    '--format=cjs',
    '--target=node20',
    '--keep-names',
    '--outfile=dist/bundle.cjs',
    '--external:@blue-frontier/dev-sidecar',
    '--external:@blue-frontier/mitmproxy',
  ], { shell: process.platform === 'win32' })

  // 1) 生成 SEA blob
  run(process.execPath, ['--experimental-sea-config', path.join(root, 'sea-config.json')])

  // 2) 复制 node 可执行文件作为宿主骨架
  fs.copyFileSync(process.execPath, outPath)
  console.log(`已复制 node 可执行文件 -> ${outPath}`)

  // 3) 注入 blob（postject 需要单独安装：npm i -g postject）
  const postjectArgs = [
    outPath,
    'NODE_SEA_BLOB',
    blobPath,
    '--sentinel-fuse',
    'NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2',
  ]
  // Windows 上 postject 是 .cmd，Node 20.12+ 出于 CVE-2024-27980 不允许无 shell 直接 spawn，
  // 会报 `spawnSync postject.cmd EINVAL`，所以这里显式走 shell。
  execFileSync(
    process.platform === 'win32' ? 'postject.cmd' : 'postject',
    postjectArgs,
    { stdio: 'inherit', cwd: root, shell: process.platform === 'win32' }
  )

  const size = (fs.statSync(outPath).size / 1024 / 1024).toFixed(1)
  console.log(`\n完成：${outPath}（${size} MB）`)
  console.log('下一步：pwsh -File ../scripts/prepare-binaries.ps1')
}

main()
