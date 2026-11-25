#!/usr/bin/env node
/**
 * 用 esbuild 把数据面（`@blue-frontier/mitmproxy`）打平成单个 cjs。
 *
 * 为什么：安装后 `core-bundle/node_modules` 有三千多个小文件，用户装完一片
 * 「碎文件」。打平之后数据面只剩 `bundle/mitmproxy.cjs` + `bundle/res/**`（资源镜像）。
 *
 * 两个关键点：
 *
 * 1. `__dirname` 语义要保住。上游有 35 处 `readFileSync(path.join(__dirname, ...))`，
 *    打平后 `__dirname` 会变成 bundle 所在目录，路径全错。所以这里用 onLoad 插件把
 *    每个源文件里的 `__dirname` 换成 `path.join(__dirname, 'res/packages/mitmproxy/<该文件相对包根的目录>')`，
 *    并把包内所有非 js 文件按同样的相对路径镜像到 `res/` 下 —— 相对关系完全不变。
 *
 * 2. native / 可选依赖不能打进来：`koffi`、`@starknt/sysproxy`、`wreq-js`、`keytar`
 *    都是 .node 或平台相关的包，全部 external（运行时它们是 try/catch 的可选项）。
 *
 * 用法：
 *   node scripts/build-bundles.mjs --workspace .build/core-bundle-ws-xxx \
 *        --esbuild .build/esbuild --out app/src-tauri/core-bundle
 */

import fs from 'node:fs'
import path from 'node:path'
import { createRequire } from 'node:module'

const EXTERNALS = [
  'koffi',
  '@starknt/sysproxy',
  'wreq-js',
  'keytar',
]

function parseArgs (argv) {
  const out = { targets: ['mitmproxy'], keepRes: false, userDir: '.dev-sidecar-lite' }
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]
    const next = () => argv[++i]
    if (arg === '--workspace') out.workspace = next()
    else if (arg === '--esbuild') out.esbuild = next()
    else if (arg === '--out') out.out = next()
    else if (arg === '--targets') out.targets = next().split(',')
    else if (arg === '--core-entry') out.coreEntry = next()
    else if (arg === '--bridge-entry') out.bridgeEntry = next()
    else if (arg === '--user-dir') out.userDir = next()
    else if (arg === '--keep-res') out.keepRes = true
    else if (arg === '--help' || arg === '-h') out.help = true
    else throw new Error(`未知参数：${arg}`)
  }
  if (!out.help) {
    for (const key of ['workspace', 'esbuild', 'out']) {
      if (!out[key]) throw new Error(`缺少参数 --${key}`)
    }
  }
  return out
}

function countFiles (dir) {
  let count = 0
  const walk = (current) => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      if (entry.isDirectory()) walk(path.join(current, entry.name))
      else if (entry.isFile()) count++
    }
  }
  if (fs.existsSync(dir)) walk(dir)
  return count
}

function dirSize (dir) {
  let total = 0
  const walk = (current) => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name)
      if (entry.isDirectory()) walk(full)
      else if (entry.isFile()) total += fs.statSync(full).size
    }
  }
  if (fs.existsSync(dir)) walk(dir)
  return total
}

/** 把某个包目录里的非 js 文件按相对路径镜像到 res/ 下（js 已经进 bundle 了） */
function mirrorResources (pkgDir, pkgName, resRoot) {
  let copied = 0
  const walk = (current) => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name)
      if (entry.isDirectory()) {
        if (entry.name === 'node_modules' || entry.name === '.git') continue
        walk(full)
      } else if (entry.isFile()) {
        if (entry.name.endsWith('.js') || entry.name.endsWith('.mjs') || entry.name.endsWith('.cjs')) continue
        if (entry.name.endsWith('.md') || entry.name.endsWith('.map')) continue
        const rel = path.relative(pkgDir, full)
        const target = path.join(resRoot, 'packages', pkgName, rel)
        fs.mkdirSync(path.dirname(target), { recursive: true })
        fs.copyFileSync(full, target)
        copied++
      }
    }
  }
  walk(pkgDir)
  return copied
}

/**
 * `__dirname` / `__filename` 重写插件：只改目标包里的文件，第三方依赖保持原样
 * （它们通常不读自己的包内资源；真踩到会在冒烟里立刻暴露）。
 */
function dirnamePlugin ({ pkgDir, pkgName, resRoot, userDir }) {
  return {
    name: 'ds-dirname-mirror',
    setup (build) {
      build.onLoad({ filter: /\.(js|cjs|mjs)$/ }, async (args) => {
        const source = await fs.promises.readFile(args.path, 'utf8')
        if (!args.path.startsWith(pkgDir)) {
          return { contents: source, loader: 'js' }
        }
        const relDir = path.relative(pkgDir, path.dirname(args.path))
        const mirror = path.posix.join('res', 'packages', pkgName, relDir.split(path.sep).join('/'))
        // 用户配置目录：core 里硬编码的 `~/.dev-sidecar` 在这里一次性换成 `.dev-sidecar-lite`；
        // 用 `(?![-.\w])` 保证幂等（已经是 `.dev-sidecar-lite` 的不会再被改）。
        const userDirFixed = source.replace(/\.dev-sidecar(?![-.\w])/g, userDir || '.dev-sidecar-lite')
        const rewritten = userDirFixed
          .replace(/\b__dirname\b/g, `require('node:path').join(__dirname, ${JSON.stringify(mirror)})`)
          .replace(/\b__filename\b/g, `require('node:path').join(__dirname, ${JSON.stringify(path.posix.join(mirror, path.basename(args.path)))})`)
        return { contents: rewritten, loader: 'js' }
      })
    }
  }
}

async function main () {
  const args = parseArgs(process.argv.slice(2))
  if (args.help) {
    console.log('用法：node scripts/build-bundles.mjs --workspace <ws> --esbuild <esbuild 模块目录> --out <core-bundle 目录> [--targets mitmproxy,core]')
    return
  }

  const workspace = path.resolve(args.workspace)
  const outDir = path.resolve(args.out)
  const bundleDir = path.join(outDir, 'bundle')
  const resRoot = path.join(bundleDir, 'res')
  fs.mkdirSync(bundleDir, { recursive: true })

  const esbuildRequire = createRequire(path.join(path.resolve(args.esbuild), 'package.json'))
  const esbuild = esbuildRequire('esbuild')

  const blueFrontier = path.join(workspace, 'node_modules', '@blue-frontier')
  const targets = [
    { name: 'mitmproxy', pkg: path.join(blueFrontier, 'mitmproxy'), entry: 'src/index.js', out: 'mitmproxy.cjs' },
    { name: 'core', pkg: path.join(blueFrontier, 'dev-sidecar'), entry: 'src/index.js', out: 'core.cjs' },
  ].filter((item) => args.targets.includes(item.name))

  for (const target of targets) {
    // core 的入口不是 core 自己的 index.js，而是宿主写的汇总入口（要把几个内部模块一起导出）
    let entryFile = path.join(target.pkg, target.entry)
    if (target.name === 'mitmproxy' && args.bridgeEntry) {
      // 数据面的 fork 目标是「读 argv[2] 再 start」的入口，不是库入口
      if (!fs.existsSync(args.bridgeEntry)) {
        throw new Error(`找不到数据面入口：${args.bridgeEntry}`)
      }
      entryFile = path.join(workspace, 'bridge-entry.cjs')
      fs.copyFileSync(args.bridgeEntry, entryFile)
      // 入口里 `require('./src/...')` 是相对路径，把宿主 src 一起摆过去
      fs.cpSync(path.join(path.dirname(args.bridgeEntry), 'src'), path.join(workspace, 'src'), { recursive: true })
    }
    if (target.name === 'core' && args.coreEntry) {
      if (!fs.existsSync(args.coreEntry)) {
        throw new Error(`找不到 core 汇总入口：${args.coreEntry}`)
      }
      entryFile = path.join(workspace, 'core-entry.js')
      fs.copyFileSync(args.coreEntry, entryFile)
    }
    if (!fs.existsSync(entryFile)) {
      throw new Error(`找不到入口：${entryFile}`)
    }
    const outfile = path.join(bundleDir, target.out)
    const startedAt = Date.now()
    await esbuild.build({
      absWorkingDir: workspace,
      entryPoints: [entryFile],
      outfile,
      bundle: true,
      platform: 'node',
      format: 'cjs',
      target: 'node20',
      keepNames: true,
      sourcemap: false,
      logLevel: 'warning',
      external: EXTERNALS,
      plugins: [dirnamePlugin({ pkgDir: target.pkg, pkgName: target.name === 'core' ? 'dev-sidecar' : target.name, resRoot, userDir: opts.userDir })],
      define: { 'process.env.NODE_ENV': '"production"' },
    })
    const resCount = mirrorResources(target.pkg, target.name === 'core' ? 'dev-sidecar' : target.name, resRoot)
    const size = fs.statSync(outfile).size
    console.log(`[bundle] ${target.out}  ${(size / 1024 / 1024).toFixed(2)} MB  资源镜像 ${resCount} 个  ${Date.now() - startedAt} ms`)
  }

  console.log(`[bundle] bundle/ 共 ${countFiles(bundleDir)} 个文件 ${(dirSize(bundleDir) / 1024 / 1024).toFixed(2)} MB`)
}

main().catch((err) => {
  console.error(`[bundle] 失败：${err && err.message ? err.message : err}`)
  if (err && err.errors) {
    for (const item of err.errors) {
      console.error(`  ${item.text} ${item.location ? `${item.location.file}:${item.location.line}` : ''}`)
    }
  }
  process.exit(1)
})
