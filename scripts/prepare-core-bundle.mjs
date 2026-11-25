#!/usr/bin/env node
/**
 * 组装 core-bundle/ —— Tauri 安装包里那个「Node 控制面 + Node 数据面」的运行时目录。
 *
 * 为什么需要它：core 与 mitmproxy 是运行时才 require 的，必须随安装包以真实文件形式存在；
 * 控制面用随包的 `node.exe`（`core-bundle/node/node.exe`）跑磁盘上的 js，不再打 SEA 单文件。
 *
 * 为什么不直接 `pnpm deploy`：core 与 mitmproxy 互相依赖（`@blue-frontier/*`），
 * 单独 deploy 任一个都拿不到另一个；而 `pnpm deploy` 的目标树里还是符号链接，
 * NSIS/Tauri 打包时符号链接会被丢弃。所以这里造一个**合成 workspace**，让 pnpm 自己
 * 解析出正确版本（含版本冲突时自动嵌套），再用 `node-linker=hoisted` 拿到全是真实文件的扁平树。
 *
 * 用法：
 *   node scripts/prepare-core-bundle.mjs [--dev-sidecar <path>] [--out <path>]
 *                                        [--max-mb 140] [--keep]
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
    maxMb: 140,
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
      case '--no-bundle': opts.noBundle = true; break
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

function countFiles (dir) {
  let total = 0
  const stack = [dir]
  while (stack.length) {
    const current = stack.pop()
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name)
      if (entry.isDirectory()) stack.push(full)
      else if (entry.isFile()) total += 1
    }
  }
  return total
}

/// node_modules 里有一半文件运行时根本不会碰：文档、sourcemap、测试用例、示例。
/// 安装后用户看到 4000+ 个小文件，观感极差，也没必要。
/// 只删「确定不会被 require」的东西，宁少不多：
/// - 目录：test/tests/__tests__/example/examples/doc/docs/coverage/.github/benchmark(s)
/// - 文件：*.md（LICENSE 除外）、*.map、CHANGELOG*、.eslintrc*、.eslintignore、*.tsbuildinfo
/// 不删 *.ts / *.yml / *.json：有些包会运行时读它们。
const PRUNE_DIRS = new Set([
  'test', 'tests', '__tests__', 'example', 'examples', 'doc', 'docs',
  'coverage', '.github', 'benchmark', 'benchmarks',
])
const PRUNE_FILE_RE = /(\.md$|\.markdown$|\.map$|\.tsbuildinfo$|^\.eslintrc|^\.eslintignore$|^CHANGELOG)/i

function pruneNodeModules (root) {
  let dirs = 0
  let files = 0
  const stack = [root]
  while (stack.length) {
    const current = stack.pop()
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name)
      if (entry.isDirectory()) {
        if (PRUNE_DIRS.has(entry.name)) {
          files += countFiles(full)
          fs.rmSync(full, { recursive: true, force: true })
          dirs += 1
        } else {
          stack.push(full)
        }
      } else if (entry.isFile() && PRUNE_FILE_RE.test(entry.name) && !/^license/i.test(entry.name)) {
        fs.rmSync(full, { force: true })
        files += 1
      }
    }
  }
  return { dirs, files }
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
  // 版本号只认 app/package.json（与 tauri.conf.json、Cargo.toml 一起升级的那个），
  // 回退到 core-host 的 package.json。core-host/package.json 以前就是这里的唯一来源，
  // 发版时忘了同步 → 界面「关于」一直显示旧版本号。
  const appPkgPath = path.join(liteRoot, 'app', 'package.json')
  const litePkgPath = fs.existsSync(appPkgPath) ? appPkgPath : path.join(liteRoot, 'core-host', 'package.json')
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

    // 2.5) 输出目录骨架（3.x 直接往里拷）
    fs.rmSync(opts.out, { recursive: true, force: true })
    fs.mkdirSync(opts.out, { recursive: true })

    // 2.6) L2 打平：把 core 与 mitmproxy 打成一个 cjs（node_modules 的文件数从 3600+ 降到几十）
    let bundled = false
    if (!opts.noBundle) {
      const esbuildDir = path.join(liteRoot, '.build', 'esbuild')
      if (!fs.existsSync(path.join(esbuildDir, 'node_modules', 'esbuild', 'package.json'))) {
        fs.mkdirSync(esbuildDir, { recursive: true })
        console.log('[core-bundle] 安装 esbuild（首次需要联网）')
        run('npm', ['install', 'esbuild@0.24.0', '--no-save', '--no-audit', '--no-fund', '--loglevel', 'error'], esbuildDir)
      }
      console.log('[core-bundle] 打平 core 与 mitmproxy ...')
      run(process.execPath, [
        path.join(liteRoot, 'scripts', 'build-bundles.mjs'),
        '--workspace', work,
        '--esbuild', esbuildDir,
        '--out', opts.out,
        '--targets', 'mitmproxy,core',
        '--core-entry', path.join(liteRoot, 'core-host', 'core-entry.js'),
        '--bridge-entry', path.join(liteRoot, 'core-host', 'bridge-entry.cjs'),
      ], liteRoot)
      bundled = true
    }

    // 3) 组包
    const depsOut = path.join(opts.out, 'node_modules')

    if (bundled) {
      // 打平后 node_modules 只需要留下走原生/可选二进制的那几个包
      const keepPkgs = ['koffi', '@starknt/sysproxy', 'wreq-js', 'keytar']
      for (const name of keepPkgs) {
        const src = path.join(work, 'node_modules', ...name.split('/'))
        if (!fs.existsSync(src)) {
          continue
        }
        const dst = path.join(depsOut, ...name.split('/'))
        fs.mkdirSync(path.dirname(dst), { recursive: true })
        copyTree(src, dst, { includeNodeModules: true })
        console.log(`[core-bundle] 保留原生依赖 ${name}`)
      }
      removeSymlinks(depsOut)
      console.log(`[core-bundle] 打平后 node_modules: ${countFiles(depsOut)} 文件`)
    } else {
      // 3.1 node_modules：hoisted 树的顶层都是真实文件；.pnpm 是硬链接存储（打包时会翻倍），删掉
      fs.cpSync(path.join(work, 'node_modules'), depsOut, {
        recursive: true,
        dereference: false,
        filter: (src) => !src.startsWith(path.join(work, 'node_modules', '.pnpm')) &&
          !src.startsWith(path.join(work, 'node_modules', '@blue-frontier')),
      })
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

      removeSymlinks(depsOut)

      // 3.4 修剪文档/测试/示例/sourcemap
      const before = countFiles(depsOut)
      const removed = pruneNodeModules(depsOut)
      const after = countFiles(depsOut)
      console.log(
        `[core-bundle] 修剪 node_modules: ${before} -> ${after} 文件` +
        `（删 ${removed.files} 个文件 / ${removed.dirs} 个测试示例目录）`,
      )
    }

    // 4) extra/：sysproxy.exe、EnableLoopback.exe、scripts/、proxy/、pac/
    copyTree(path.join(opts.devSidecar, 'packages', 'gui', 'extra'), path.join(opts.out, 'extra'))

    // 4.1 domestic-domain-allowlist.txt
    //     core 的 set-system-proxy/index.js:311 是
    //     `path.join(__dirname, '../', proxy.domesticDomainAllowListFilePath)`，
    //     即 <core>/src/shell/scripts/extra/proxy/domestic-domain-allowlist.txt。
    //     上游只把它放在 packages/gui/extra/proxy/ 下，core 自己那里没有 →
    //     安装后每次开系统代理都报「读取失败」并去联网下载（离线必失败）。
    {
      const src = path.join(opts.devSidecar, 'packages', 'gui', 'extra', 'proxy', 'domestic-domain-allowlist.txt')
      // 打平布局下 core 的 __dirname 被搬到 bundle/res/packages/core/<相对包根>，
      // 所以白名单文件要放到镜像目录里（build-bundles.mjs 的镜像逻辑不覆盖它，因为上游 core 里没有这个文件）
      const dst = bundled
        ? path.join(opts.out, 'bundle', 'res', 'packages', 'core', 'src', 'shell', 'scripts', 'extra', 'proxy')
        : path.join(depsOut, '@blue-frontier', 'dev-sidecar', 'src', 'shell', 'scripts', 'extra', 'proxy')
      if (fs.existsSync(src)) {
        fs.mkdirSync(dst, { recursive: true })
        fs.copyFileSync(src, path.join(dst, 'domestic-domain-allowlist.txt'))
        console.log('[core-bundle] domestic-domain-allowlist.txt -> core/src/shell/scripts/extra/proxy/')
      } else {
        console.warn(`[core-bundle] 未找到 ${src}，开系统代理时会退回联网下载`)
      }
    }

    // 5) core-host（src，SEA 需要它们在磁盘上；数据面入口是 mitmproxy-bridge.cjs）
    copyTree(path.join(liteRoot, 'core-host', 'src'), path.join(opts.out, 'core-host', 'src'))
    // Node 数据面入口：core 会 `fork(<这个文件>, [running.json])`
    fs.copyFileSync(
      path.join(liteRoot, 'core-host', 'mitmproxy-bridge.cjs'),
      path.join(opts.out, 'core-host', 'mitmproxy-bridge.cjs'),
    )
    fs.copyFileSync(
      path.join(liteRoot, 'core-host', 'package.json'),
      path.join(opts.out, 'core-host', 'package.json'),
    )

    // 6) Node 运行时：控制面用随包的 node.exe 跑（不再 postject 成 SEA 单文件）
    const nodeDir = path.join(opts.out, 'node')
    fs.mkdirSync(nodeDir, { recursive: true })
    const nodeExe = path.join(nodeDir, 'node.exe')
    fs.copyFileSync(process.execPath, nodeExe)
    fs.writeFileSync(path.join(nodeDir, 'VERSION'), `${process.version}\n`)
    const nodeMb = (fs.statSync(nodeExe).size / (1024 * 1024)).toFixed(1)
    console.log(`[core-bundle] node 运行时 -> node/node.exe（${process.version}，${nodeMb} MB）`)

    const version = fs.existsSync(litePkgPath) ? readJson(litePkgPath).version : '0.0.0'
    fs.writeFileSync(path.join(opts.out, 'VERSION'), `${version}\n`)

    // 7) 体积门禁
    const bytes = dirSize(opts.out)
    const mb = bytes / 1048576
    console.log(`[core-bundle] 体积: ${mb.toFixed(1)} MB（门禁 ${opts.maxMb} MB）`)
    if (mb > opts.maxMb) {
      throw new Error(`core-bundle 超过体积门禁：${mb.toFixed(1)} MB > ${opts.maxMb} MB`)
    }

    // 8) 自检：能不能真的 require 起来
    const probe = bundled
      ? [
          `const path = require('node:path')`,
          `const core = require(path.join(process.cwd(), 'bundle', 'core.cjs'))`,
          `if (!core.api) throw new Error('core.api 缺失')`,
          `const intercepts = Object.keys(core.api.config.get().server.intercepts || {}).length`,
          `if (intercepts < 100) throw new Error('intercepts 数量异常: ' + intercepts)`,
          `if (!core.serviceGroup || typeof core.serviceGroup.listCatalog !== 'function') throw new Error('serviceGroup 缺失')`,
          `if (typeof core.lodash.get !== 'function') throw new Error('lodash 缺失')`,
          // 注意：bundle/mitmproxy.cjs 是「可执行入口」（读 argv[2] 启动数据面），
          // require 它会直接 exit(1)，这里只断言文件存在且非空
          `const fs = require('node:fs')`,
          `const bridge = path.join(process.cwd(), 'bundle', 'mitmproxy.cjs')`,
          `if (!fs.existsSync(bridge) || fs.statSync(bridge).size < 64 * 1024) throw new Error('mitmproxy.cjs 缺失或过小: ' + bridge)`,
          `console.log('[core-bundle] 打平自检通过, intercepts=' + intercepts)`,
        ].join(';')
      : [
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
