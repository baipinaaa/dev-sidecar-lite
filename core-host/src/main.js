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
  DS_CORE_PATH        指定 core 包目录（默认自动解析）
  DS_MITMPROXY_PATH   指定 mitmproxy 入口（Node 引擎用）
  DS_PROXY_EXE        指定 ds-proxy.exe（Rust 引擎用）
`)
}

function tryRequire (dir) {
  try {
    // SEA 里普通 require 只认内置模块，必须走 runtime 提供的磁盘 require
    return runtime.requireFrom(dir)
  } catch (e) {
    return null
  }
}

/**
 * 解析 core 模块。
 *
 * 四种来源，按优先级：环境变量 → 已安装依赖 → 源码目录 → **安装包的 resources/**。
 * 最后一个是为了打包形态：SEA 只打入口脚本，core 是运行时才 require 的，
 * 所以必须随安装包一起放到 `resources/app/packages/core`。
 */
function resolveCore () {
  const packaged = runtime.installRoots().flatMap((root) => [
    // 当前布局：`app/src-tauri/core-bundle/**` 被打进 `$INSTDIR/resources/core-bundle/**`
    path.join(root, 'resources', 'core-bundle', 'packages', 'core'),
    // 旧布局（保留兼容）
    path.join(root, 'resources', 'app', 'packages', 'core'),
  ])

  const candidates = [
    process.env.DS_CORE_PATH,
    '@blue-frontier/dev-sidecar',
    path.join(__dirname, '../../dev-sidecar/packages/core'),
    ...packaged,
  ].filter(Boolean)

  for (const candidate of candidates) {
    const core = tryRequire(candidate)
    if (core && core.api) {
      return { core, from: candidate }
    }
  }

  throw new Error(
    '无法加载 core 模块。请设置 DS_CORE_PATH 指向 packages/core，或先 pnpm install 让 @blue-frontier/dev-sidecar 可用。\n' +
    `已尝试: ${candidates.join(', ')}`,
  )
}

function resolveMitmproxyPath () {
  const packaged = runtime.installRoots().map((root) =>
    path.join(root, 'resources', 'core-bundle', 'packages', 'mitmproxy', 'src', 'index.js'),
  )

  const candidates = [
    process.env.DS_MITMPROXY_PATH,
    path.join(__dirname, '../../dev-sidecar/packages/mitmproxy/src/index.js'),
    path.join(__dirname, '../../../dev-sidecar/packages/mitmproxy/src/index.js'),
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

async function main () {
  const options = parseArgs(process.argv.slice(2))
  const dataDir = options.dataDir || resolveDataDir()
  process.env.DS_DATA_DIR = dataDir

  const { core, from } = resolveCore()
  const nodeEntry = resolveMitmproxyPath()

  console.log(`[core-host] core 已加载: ${from}`)
  console.log(`[core-host] 数据目录: ${dataDir}`)
  console.log(`[core-host] Node 引擎入口: ${nodeEntry || '(未找到)'}`)

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
      let configured = 'auto'
      try {
        const conf = core.api.config.get()
        configured = (conf.proxy && conf.proxy.engine) || 'auto'
      } catch (e) {
        // 配置未就绪时按 auto 处理
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
    dataDir,
    mitmproxyPath: engineControl.entry,
    engineControl,
    setting: undefined,
  })

  const address = await host.listen(options.host, options.port)
  console.log(`[core-host] 监听 http://${address.host}:${address.port}`)
  console.log(`[core-host] 凭据: ${path.join(dataDir, 'run', 'core.token')}`)

  if (options.startup) {
    try {
      await core.api.startup({ mitmproxyPath: engineControl.entry, setting: core.api.config.get().server.setting })
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
