'use strict'

/**
 * `ds-core-host` 入口。
 *
 * 职责：加载 Node core（控制面），起 HTTP/SSE 宿主，然后把「服务/系统代理/PAC/插件」
 * 按用户配置恢复到上次的状态。数据面由 core 自己拉起（Node 或 Rust）。
 *
 * 用法：
 *   node src/main.js [--port 31190] [--host 127.0.0.1] [--data-dir <dir>] [--no-startup] [-v]
 */

const fs = require('node:fs')
const path = require('node:path')

const { createHost, resolveDataDir } = require('./host')
const { createLocalApi } = require('./local-api')
const { entryFor, resolveEngine, resolveRustProxyPath } = require('./engine')
const runtime = require('./runtime')

function parseArgs (argv) {
  const options = {
    host: process.env.DS_CORE_HOST || '127.0.0.1',
    port: process.env.DS_CORE_PORT ? Number(process.env.DS_CORE_PORT) : 31190,
    dataDir: process.env.DS_DATA_DIR || null,
    startup: true,
    verbose: false,
  }

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]
    switch (arg) {
      case '--port':
        options.port = Number(argv[++i])
        break
      case '--host':
        options.host = argv[++i]
        break
      case '--data-dir':
        options.dataDir = argv[++i]
        break
      case '--no-startup':
        options.startup = false
        break
      case '-v':
      case '--verbose':
        options.verbose = true
        break
      case '-h':
      case '--help':
        printUsage()
        process.exit(0)
        break
      default:
        console.error(`未知参数: ${arg}`)
        printUsage()
        process.exit(2)
    }
  }
  return options
}

function printUsage () {
  console.log(`ds-core-host — DevSidecar Lite 控制面宿主

  --port <n>        监听端口（默认 31190，0 表示随机）
  --host <addr>     监听地址（默认 127.0.0.1，只允许回环）
  --data-dir <dir>  数据目录（默认 %ProgramData%\\dev-sidecar-lite）
  --no-startup      只起 HTTP，不自动恢复代理/系统代理/插件
  -v, --verbose     打印接口调用明细

环境变量:
  DS_CORE_BUNDLE      指定 core-bundle 根目录（默认按安装布局解析）
  DS_CORE_PATH        指定 core 包目录（默认自动解析）
  DS_MITMPROXY_PATH   指定 mitmproxy 入口（Node 引擎用）
  DS_PROXY_EXE        指定 ds-proxy.exe（Rust 引擎用）
`)
}

const requireErrors = []

function tryRequire (dir) {
  try {
    // SEA 里普通 require 只认内置模块，必须走 runtime 提供的磁盘 require
    return runtime.requireFrom(dir)
  } catch (e) {
    // 记下真实原因：打包形态下最常见的是「core 在，但它依赖的某个包不在」，
    // 只抛一句"无法加载 core 模块"会让人以为是路径问题，白排查半天。
    const message = String((e && e.message) || e).split('\n')[0]
    requireErrors.push(`${dir} → ${message}`)
    return null
  }
}

/**
 * 解析 core 模块。
 *
 * 四种来源，按优先级：环境变量 → 已安装依赖 → 源码目录 → **安装包的 core-bundle**。
 * 打包形态下 SEA 只打入口脚本，core 是运行时才 require 的，所以必须随安装包下发。
 */
function resolveCore () {
  const packaged = runtime.installRoots().flatMap((root) => [
    // 实测安装布局：core-bundle 与主程序同级（$INSTDIR/core-bundle/…）
    path.join(root, 'core-bundle', 'node_modules', '@blue-frontier', 'dev-sidecar'),
    path.join(root, 'resources', 'core-bundle', 'node_modules', '@blue-frontier', 'dev-sidecar'),
    // 旧布局（core-bundle/packages/*），留着兼容早期包
    path.join(root, 'core-bundle', 'packages', 'core'),
    path.join(root, 'resources', 'core-bundle', 'packages', 'core'),
    path.join(root, 'resources', 'app', 'packages', 'core'),
  ])

  const candidates = [
    process.env.DS_CORE_PATH,
    process.env.DS_CORE_BUNDLE &&
      path.join(process.env.DS_CORE_BUNDLE, 'node_modules', '@blue-frontier', 'dev-sidecar'),
    '@blue-frontier/dev-sidecar',
    path.join(__dirname, '../../dev-sidecar/packages/core'),
    ...packaged,
  ].filter(Boolean)

  const fs = require('node:fs')

  for (const candidate of candidates) {
    // 候选是本地目录时，先把 @blue-frontier/* 映射过去再 require：
    // core 与 mitmproxy 互相引用，安装包里它们是并列目录而非 node_modules 布局。
    if (path.isAbsolute(candidate)) {
      // core-bundle 里两者都在 node_modules/@blue-frontier 下，能直接 require；
      // 源码/自定义目录（DS_CORE_PATH）才需要临时别名。
      const mitmproxyDir = path.join(candidate, '..', 'mitmproxy')
      const hasMitmproxy = fs.existsSync(path.join(mitmproxyDir, 'package.json'))
      if (hasMitmproxy) {
        runtime.installPackageAliases({
          core: candidate,
          mitmproxy: mitmproxyDir,
        })
      }
    }

    const core = tryRequire(candidate)
    if (core && core.api) {
      return { core, from: candidate }
    }
  }

  throw new Error(
    '无法加载 core 模块。请设置 DS_CORE_PATH 指向 packages/core，或先 pnpm install 让 @blue-frontier/dev-sidecar 可用。\n' +
    `已尝试: ${candidates.join(', ')}
` +
    `失败原因:
${requireErrors.map((line) => '  - ' + line).join('\n')}`,
  )
}

function resolveMitmproxyPath () {
  // 路线 4：默认引擎是 Node，但入口不是 mitmproxy 的 index.js（它是库，不读 argv[2]），
  // 而是本仓库的 bridge（读 running.json → start(config)）。
  const packaged = runtime.findInResources('core-bundle', 'core-host', 'mitmproxy-bridge.cjs')

  const candidates = [
    process.env.DS_MITMPROXY_PATH,
    path.join(__dirname, '..', 'mitmproxy-bridge.cjs'),
    ...packaged,
  ].filter(Boolean)

  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) {
      return candidate
    }
  }

  try {
    return runtime.requireFrom('@blue-frontier/mitmproxy/src/index.js')
  } catch (e) {
    return null
  }
}

/** 找到 core-bundle 根目录（含 extra/ 与 node_modules/ 的那层），用于 extra 与版本号 */
function resolveBundleRoot () {
  // 显式指定（冒烟测试 / 开发时直接指向 app/src-tauri/core-bundle）
  if (process.env.DS_CORE_BUNDLE) {
    return process.env.DS_CORE_BUNDLE
  }
  const candidates = [
    ...runtime.findInResources('core-bundle', 'core-host', 'mitmproxy-bridge.cjs').map((hit) =>
      path.resolve(path.dirname(hit), '..', '..')),
    path.resolve(__dirname, '..', '..'),
  ]
  for (const candidate of candidates) {
    if (fs.existsSync(path.join(candidate, 'node_modules')) || fs.existsSync(path.join(candidate, 'extra'))) {
      return candidate
    }
  }
  return null
}

async function main () {
  const options = parseArgs(process.argv.slice(2))
  const dataDir = options.dataDir || resolveDataDir()
  process.env.DS_DATA_DIR = dataDir

  const { core, from } = resolveCore()
  const nodeEntry = resolveMitmproxyPath()
  const bundleRoot = resolveBundleRoot()

  // extra/：sysproxy.exe、EnableLoopback.exe、scripts/、pac/ 都在它下面。
  // core 的 `extra-path/index.js` 用它解析外部脚本目录，mitmproxy 的 bridge 用它算 rootDir。
  if (!process.env.DS_EXTRA_PATH) {
    const candidates = [
      bundleRoot && path.join(bundleRoot, 'extra'),
      path.join(__dirname, '..', '..', 'extra'),
    ].filter(Boolean)
    for (const candidate of candidates) {
      if (fs.existsSync(candidate)) {
        process.env.DS_EXTRA_PATH = candidate
        break
      }
    }
  }

  console.log(`[core-host] core 已加载: ${from}`)
  console.log(`[core-host] 数据目录: ${dataDir}`)
  console.log(`[core-host] bundle 根目录: ${bundleRoot || '(未识别)'}`)
  console.log(`[core-host] extra 目录: ${process.env.DS_EXTRA_PATH || '(未找到)'}`)
  console.log(`[core-host] Node 引擎入口: ${nodeEntry || '(未找到)'}`)

  /**
   * 把用户配置合并进当前配置。
   *
   * 必须做：`config-api.js:43` 的 `configTarget` 初始值只是内置默认配置
   * （`lodash.cloneDeep(defConfig)`），用户 `config.json` 要等显式 `reload()` 才合并进来。
   * 原版在 `packages/gui/src/bridge/api/backend.js:448` 的 `install()` 末尾调了
   * `DevSidecar.api.config.reload()` 再 `doStart()`，我们漏了这一步 —— 症状是
   * 「界面上改端口/关开关都没用，启动永远用默认 31181 端口并直接改系统代理」。
   */
  try {
    core.api.config.reload()
    console.log('[core-host] 用户配置已合并')
  } catch (err) {
    console.error('[core-host] 合并用户配置失败:', err && err.message ? err.message : err)
  }

  /**
   * 本地接口层（原版 `backend.js` 的 localApi）：界面调的是 getApiList / info.get /
   * setting.load / serviceGroups.* 这些复合接口，core.api 里没有，必须补上。
   */
  const hostRef = { value: null }
  const localApi = createLocalApi({
    core,
    coreDir: path.isAbsolute(from) ? from : null,
    mitmproxyPath: nodeEntry,
    bundleRoot: bundleRoot || process.cwd(),
    emit: (channel, payload) => {
      // host 创建后才有广播能力，这里先攒着（config.changed 只在启动后才有人听）
      if (hostRef.value) {
        hostRef.value.broadcast(channel, payload)
      }
    },
  })

  /**
   * 引擎控制。
   *
   * 关键点：**不修改 Node core**。core 的 `server.start({ mitmproxyPath })` 已经把入口参数化，
   * 换引擎只需给它一个不同的入口脚本（rust 模式给 shim）。
   */
  const engineControl = {
    current: { engine: 'node', configured: 'auto', reason: '' },
    entry: null,
    resolve () {
      let configured = 'node'
      try {
        const conf = core.api.config.get()
        configured = (conf.proxy && conf.proxy.engine) || 'node'
      } catch (e) {
        // 配置未就绪时按默认 node 处理
      }
      const resolved = resolveEngine({ configured })
      this.current = {
        engine: resolved.engine,
        configured,
        reason: resolved.reason,
        rustPath: resolved.rustPath,
      }
      this.entry = entryFor(resolved.engine, nodeEntry)
      return this.current
    },
    status () {
      const running = core.api.status.get().server.enabled === true
      return { ...this.current, running, rustPath: this.current.rustPath || resolveRustProxyPath() }
    },
    async set (engine) {
      core.api.config.update({ proxy: { engine } })
      const next = this.resolve()
      console.log(`[core-host] 引擎已切到 ${next.engine}（${next.reason}）`)
      if (core.api.status.get().server.enabled) {
        const conf = core.api.config.get()
        await core.api.server.restart({ mitmproxyPath: this.entry, setting: conf.server.setting })
      }
      return this.status()
    },
  }

  const resolved = engineControl.resolve()
  console.log(`[core-host] 数据面引擎: ${resolved.engine}（${resolved.reason}）`)

  const host = createHost({
    core,
    localApi,
    dataDir,
    mitmproxyPath: engineControl.entry,
    engineControl,
    setting: undefined,
    onShutdown: async () => {
      // 界面点「退出」→ lib.rs 调 shutdown → 这里把数据面子进程也收干净，
      // 否则 ds-proxy / mitmproxy 会变成孤儿进程占着端口。
      try {
        await core.api.server.close()
      } catch (e) {
        console.error('[core-host] 关闭数据面失败:', e && e.message ? e.message : e)
      }
    },
  })
  hostRef.value = host

  const address = await host.listen(options.host, options.port)
  console.log(`[core-host] 监听 http://${address.host}:${address.port}`)
  console.log(`[core-host] 凭据: ${path.join(dataDir, 'run', 'core.token')}`)

  if (options.startup) {
    try {
      // 走 localApi：它会先把 setting.load() 的默认值补齐（installTime / rootCa.setuped），
      // 与 Electron 版的启动路径一致；core.api.startup 直接调会跳过这些默认值。
      await localApi.startup()
      console.log('[core-host] 已按配置恢复启动状态')
    } catch (err) {
      console.error('[core-host] 启动恢复失败:', err && err.message ? err.message : err)
    }
  }

  console.log(`[core-host] 当前数据面: ${engineControl.status().engine}`)

  let closing = false
  const shutdown = async (signal) => {
    if (closing) {
      return
    }
    closing = true
    console.log(`[core-host] 收到 ${signal}，正在退出…`)
    try {
      await core.api.shutdown()
    } catch (err) {
      console.error('[core-host] 关闭时出错:', err && err.message ? err.message : err)
    }
    await host.close()
    process.exit(0)
  }

  process.on('SIGINT', () => shutdown('SIGINT'))
  process.on('SIGTERM', () => shutdown('SIGTERM'))
  process.on('uncaughtException', (err) => {
    console.error('[core-host] 未捕获异常:', err)
  })
  process.on('unhandledRejection', (reason) => {
    console.error('[core-host] 未处理的 Promise 拒绝:', reason)
  })
}

main().catch((err) => {
  console.error('[core-host] 启动失败:', err && err.stack ? err.stack : err)
  process.exit(1)
})
