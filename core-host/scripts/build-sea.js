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

function run (command, args) {
  console.log(`$ ${command} ${args.join(' ')}`)
  execFileSync(command, args, { stdio: 'inherit', cwd: root })
}

function main () {
  fs.mkdirSync(distDir, { recursive: true })

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
  run(process.platform === 'win32' ? 'postject.cmd' : 'postject', postjectArgs)

  const size = (fs.statSync(outPath).size / 1024 / 1024).toFixed(1)
  console.log(`\n完成：${outPath}（${size} MB）`)
  console.log('下一步：pwsh -File ../scripts/prepare-binaries.ps1')
}

main()
