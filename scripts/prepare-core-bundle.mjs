#!/usr/bin/env node
/**
 * 组装 core-bundle/ —— Tauri 安装包里那个「Node 控制面 + Node 数据面」的运行时目录。
 *
 * 为什么需要它：ds-core-host（SEA 单文件）只打包了入口脚本，core 与 mitmproxy 是
 * 运行时才 require 的，必须随安装包以真实文件形式存在（SEA 里没有 node_modules）。
 *
 * 为什么不直接 `pnpm deploy`：core 与 mitmproxy 互相依赖（`@blue-frontier/*`），
 * 单独 deploy 任一个都拿不到另一个；而 `pnpm deploy` 的目标树里还是符号链接，
 * NSIS/Tauri 打包时符号链接会被丢弃。所以这里造一个**合成 workspace**，让 pnpm 自己
 * 解析出正确版本（含版本冲突时自动嵌套），再用 `node-linker=hoisted` 拿到全是真实文件的扁平树。
 *
 * 用法：
 *   node scripts/prepare-core-bundle.mjs [--dev-sidecar <path>] [--out <path>]
 *                                        [--max-mb 70] [--keep]
 */

import fs from 'node:fs'
import path from 'node:path'
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const liteRoot = path.resolve(__dirname, '..')

function parseArgs (argv) {
  const opts = {
    devSidecar: path.resolve(liteRoot, '..', 'dev-sidecar'),
    out: path.resolve(liteRoot, 'app', 'src-tauri', 'core-bundle'),
    maxMb: 70,
    keep: false,
    pnpm: process.env.PNPM_BIN || 'pnpm',
  }
  for (let i = 0; i < argv.length; i++) {
    switch (argv[i]) {
      case '--dev-sidecar': opts.devSidecar = path.resolve(argv[++i]); break
      case '--out': opts.out = path.resolve(argv[++i]); break
      case '--max-mb': opts.maxMb = Number(argv[++i]); break
      case '--pnpm': opts.pnpm = argv[++i]; break
      case '--keep': opts.keep = true; break
      case '-h':
      case '--help':
        console.log(fs.readFileSync(fileURLToPath(import.meta.url), 'utf-8').split('*/')[0])
        process.exit(0)
        break
      default:
        throw new Error(`未知参数: ${argv[i]}`)
    }
  }
  return opts
}

const SKIP_DIRS = new Set(['test', 'tests', '.git', 'dist', 'coverage'])
const SKIP_FILES = new Set(['pnpm-lock.yaml', 'package-lock.json'])

function copyTree (from, to, options = {}) {
  const { includeNodeModules = false, skipJunk = true } = options
  fs.mkdirSync(to, { recursive: true })
  for (const entry of fs.readdirSync(from, { withFileTypes: true })) {
    if (entry.isSymbolicLink()) {
      continue
    }
    if (entry.name === '.bin') {
      continue
    }
    if (entry.name === 'node_modules') {
      // 依赖树里不能再跳 dist/test，否则包会缺运行文件
      if (includeNodeModules) {
        copyTree(path.join(from, entry.name), path.join(to, entry.name), { includeNodeModules, skipJunk: false })
      }
      continue
    }
    if (skipJunk && (SKIP_DIRS.has(entry.name) || SKIP_FILES.has(entry.name))) {
      continue
    }
    const src = path.join(from, entry.name)
    const dst = path.join(to, entry.name)
    if (entry.isDirectory()) {
      copyTree(src, dst, options)
    } else if (entry.isFile()) {
      fs.copyFileSync(src, dst)
    }
  }
}

/** 删掉目录树里所有符号链接（Tauri/NSIS 打包时会丢弃链接，留着只会变成断链） */
function removeSymlinks (dir) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name)
    if (entry.isSymbolicLink()) {
      fs.unlinkSync(full)
      console.log(`[core-bundle] 移除符号链接: ${path.relative(dir, full)}`)
    } else if (entry.isDirectory()) {
      removeSymlinks(full)
    }
  }
}

function dirSize (dir) {
  let total = 0
  const stack = [dir]
  while (stack.length) {
    const current = stack.pop()
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name)
      if (entry.isDirectory()) {
        stack.push(full)
      } else if (entry.isFile()) {
        total += fs.statSync(full).size
      }
    }
  }
  return total
}

function readJson (file) {
  return JSON.parse(fs.readFileSync(file, 'utf-8'))
}

function run (cmd, args, cwd) {
  // pnpm 可能是 .mjs（npm 装的 pnpm 包），此时用当前 node 解释执行
  if (/\.(mjs|cjs|js)$/i.test(cmd)) {
    args = [cmd, ...args]
    cmd = process.execPath
  }
  // Windows 上 npm 全局安装出来的是 pnpm.cmd；`shell: false` 时 execFileSync
  // 不会去补 .cmd 后缀，于是报 `spawnSync pnpm ENOENT`（CI runner 上必踩）。
  let shell = false
  if (process.platform === 'win32' && !/\.[a-z0-9]+$/i.test(cmd) && !/[\\/]/.test(cmd)) {
    cmd = `${cmd}.cmd`
    shell = true
  }
  console.log(`[core-bundle] $ ${cmd} ${args.join(' ')}`)
  execFileSync(cmd, args, {
    cwd,
    stdio: 'inherit',
    env: { ...process.env, CI: process.env.CI || '' },
    shell,
  })
}

function main () {
  const opts = parseArgs(process.argv.slice(2))
  const litePkgPath = path.join(liteRoot, 'core-host', 'package.json')
  console.log(`[core-bundle] dev-sidecar: ${opts.devSidecar}`)
  console.log(`[core-bundle] 输出目录: ${opts.out}`)

  for (const required of ['packages/core/package.json', 'packages/mitmproxy/package.json', 'packages/gui/extra']) {
    const full = path.join(opts.devSidecar, required)
    if (!fs.existsSync(full)) {
      throw new Error(`dev-sidecar 缺少 ${required}（路径: ${full}）`)
    }
  }

  fs.mkdirSync(path.join(liteRoot, '.build'), { recursive: true })
  const work = fs.mkdtempSync(path.join(liteRoot, '.build', 'core-bundle-ws-'))
  console.log(`[core-bundle] 临时 workspace: ${work}`)

  try {
    // 1) 合成 workspace：只带两个包 + 一个同时依赖它们的 bundle 包
    copyTree(path.join(opts.devSidecar, 'packages', 'core'), path.join(work, 'packages', 'core'))
    copyTree(path.join(opts.devSidecar, 'packages', 'mitmproxy'), path.join(work, 'packages', 'mitmproxy'))

    const corePkg = readJson(path.join(work, 'packages', 'core', 'package.json'))
    const mitmPkg = readJson(path.join(work, 'packages', 'mitmproxy', 'package.json'))
    fs.mkdirSync(path.join(work, 'packages', 'bundle'), { recursive: true })
    fs.writeFileSync(path.join(work, 'packages', 'bundle', 'package.json'), JSON.stringify({
      name: 'ds-core-bundle-root',
      private: true,
      version: litePkgPath && fs.existsSync(litePkgPath) ? readJson(litePkgPath).version : '0.0.0',
      dependencies: {
        [corePkg.name]: 'workspace:*',
        [mitmPkg.name]: 'workspace:*',
      },
    }, null, 2))

    fs.writeFileSync(path.join(work, 'package.json'), JSON.stringify({
      name: 'ds-core-bundle-workspace',
      private: true,
    }, null, 2))

    fs.writeFileSync(path.join(work, 'pnpm-workspace.yaml'), [
      'packages:',
      '  - "packages/*"',
      'onlyBuiltDependencies: []',
      '',
    ].join('\n'))

    // 继承源仓库 .npmrc 的 registry / 镜像配置（dev-sidecar 用 npmmirror，直连 npmjs 在本机会 500）
    const inherited = []
    const srcNpmrc = path.join(opts.devSidecar, '.npmrc')
    if (fs.existsSync(srcNpmrc)) {
      for (const raw of fs.readFileSync(srcNpmrc, 'utf-8').split(/\r?\n/)) {
        const line = raw.trim()
        if (!line || line.startsWith('#')) {
          continue
        }
        inherited.push(line)
      }
    }
    if (!inherited.some(line => line.startsWith('registry='))) {
      inherited.push('registry=https://registry.npmmirror.com')
    }
    fs.writeFileSync(path.join(work, '.npmrc'), inherited.concat([
      'node-linker=hoisted',
      'ignore-scripts=true',
      'strict-peer-dependencies=false',
      'auto-install-peers=true',
      'strict-dep-builds=false',
      '',
    ]).join('\n'))

    // 2) 装依赖（hoisted = 全是真实文件，无符号链接；版本冲突时 pnpm 自动嵌套）
    run(opts.pnpm, ['install', '--prod', '--no-frozen-lockfile', '--config.strict-dep-builds=false', '--config.node-linker=hoisted'], work)

    // 3) 组包
    fs.rmSync(opts.out, { recursive: true, force: true })
    fs.mkdirSync(opts.out, { recursive: true })

    // 3.1 node_modules：hoisted 树的顶层都是真实文件；.pnpm 是硬链接存储（打包时会翻倍），删掉
    const depsOut = path.join(opts.out, 'node_modules')
    fs.cpSync(path.join(work, 'node_modules'), depsOut, {
      recursive: true,
      dereference: false,
      filter: (src) => !src.startsWith(path.join(work, 'node_modules', '.pnpm')) &&
        !src.startsWith(path.join(work, 'node_modules', '@blue-frontier')),
    })
    // 先把残留符号链接清掉，避免后面的写入穿透链接写回临时目录
    removeSymlinks(depsOut)

    // 3.2 两个包放到 @blue-frontier/ 下（原版就是这么 require 的），
    //     带上它们自己的 node_modules（pnpm 把版本冲突的依赖嵌套在那里）
    for (const dir of ['core', 'mitmproxy']) {
      const pkgFile = path.join(work, 'packages', dir, 'package.json')
      const pkgName = readJson(pkgFile).name
      const [scope, name] = pkgName.startsWith('@') ? pkgName.split('/') : ['', pkgName]
      const target = scope ? path.join(depsOut, scope, name) : path.join(depsOut, name)
      console.log(`[core-bundle] ${dir} -> node_modules/${pkgName}`)
      copyTree(path.join(work, 'packages', dir), target, { includeNodeModules: true })
    }

    // 3.3 清掉残留的符号链接与空的 .bin（Tauri/NSIS 打包时会丢链接）
    removeSymlinks(depsOut)
    // 4) extra/：sysproxy.exe、EnableLoopback.exe、scripts/、proxy/、pac/
    copyTree(path.join(opts.devSidecar, 'packages', 'gui', 'extra'), path.join(opts.out, 'extra'))

    // 5) core-host（src + shim，SEA 需要它们在磁盘上）
    copyTree(path.join(liteRoot, 'core-host', 'src'), path.join(opts.out, 'core-host', 'src'))
    copyTree(path.join(liteRoot, 'core-host', 'shim'), path.join(opts.out, 'core-host', 'shim'))
    // Node 数据面入口：core 会 `fork(<这个文件>, [running.json])`
    fs.copyFileSync(
      path.join(liteRoot, 'core-host', 'mitmproxy-bridge.cjs'),
      path.join(opts.out, 'core-host', 'mitmproxy-bridge.cjs'),
    )
    fs.copyFileSync(
      path.join(liteRoot, 'core-host', 'package.json'),
      path.join(opts.out, 'core-host', 'package.json'),
    )

    const version = fs.existsSync(litePkgPath) ? readJson(litePkgPath).version : '0.0.0'
    fs.writeFileSync(path.join(opts.out, 'VERSION'), `${version}\n`)

    // 6) 体积门禁
    const bytes = dirSize(opts.out)
    const mb = bytes / 1048576
    console.log(`[core-bundle] 体积: ${mb.toFixed(1)} MB（门禁 ${opts.maxMb} MB）`)
    if (mb > opts.maxMb) {
      throw new Error(`core-bundle 超过体积门禁：${mb.toFixed(1)} MB > ${opts.maxMb} MB`)
    }

    // 7) 自检：能不能真的 require 起来（用 @blue-frontier/ 名字，跟运行时一致）
    const probe = [
      `const core = require('@blue-frontier/dev-sidecar')`,
      `if (!core.api) throw new Error('core.api 缺失')`,
      `const conf = core.api.config.get()`,
      `const intercepts = Object.keys(conf.server.intercepts || {}).length`,
      `if (intercepts < 100) throw new Error('intercepts 数量异常: ' + intercepts)`,
      `const mitm = require('@blue-frontier/mitmproxy')`,
      `if (typeof mitm.start !== 'function') throw new Error('mitmproxy.start 缺失')`,
      `const merge = require('@blue-frontier/dev-sidecar/src/merge')`,
      `if (typeof merge !== 'object' && typeof merge !== 'function') throw new Error('core/src/merge 导出异常')`,
      `const match = require('@blue-frontier/mitmproxy/src/utils/util.match')`,
      `if (typeof match.matchHostname !== 'function') throw new Error('util.match 导出异常')`,
      `console.log('[core-bundle] 自检通过, intercepts=' + intercepts)`,
    ].join(';')
    run(process.execPath, ['-e', probe], opts.out)
  } finally {
    if (opts.keep) {
      console.log(`[core-bundle] 保留临时目录: ${work}`)
    } else {
      fs.rmSync(work, { recursive: true, force: true })
    }
  }

  console.log('[core-bundle] 完成')
}

main()
