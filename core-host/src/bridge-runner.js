'use strict'

/**
 * 数据面启动逻辑（供两个入口共用）。
 *
 * - 开发/未打平：`core-host/mitmproxy-bridge.cjs` 加载磁盘上的 `@blue-frontier/mitmproxy` 后调用 `run()`
 * - 打平后（S3/L2）：`core-host/bridge-entry.cjs` 由 esbuild 把 mitmproxy 一起打进去，再调用同一个 `run()`
 *
 * 两个入口的行为必须完全一致，所以逻辑全在这里，入口只负责「怎么拿到 mitmproxy」。
 */

const fs = require('node:fs')
const path = require('node:path')

function fatal (message, err) {
  // 这里不能用 mitmproxy 的 logger：它可能正是因为加载失败才走到这
  console.error('[mitmproxy-bridge] ' + message, err && err.stack ? err.stack : (err || ''))
  process.exit(1)
}

/**
 * 清掉 intercepts 里的 null 条目。
 *
 * 内置配置用 `null` 表示「关掉这条内置规则」（core 的 `src/config/index.js:675` 就是这么关
 * github 的油猴脚本的），但 mitmproxy 的 `lib/interceptor/impl/res/script.js:138` 直接读
 * `pathConfig.script`，遇到 null 就抛 `TypeError: Cannot read properties of null`，整个
 * 数据面起不来。原版能绕过是因为联网拉到远程配置把 null 覆盖掉了。
 */
function dropNullIntercepts (intercepts) {
  if (!intercepts || typeof intercepts !== 'object') {
    return 0
  }
  let dropped = 0
  for (const hostname of Object.keys(intercepts)) {
    const hostConfig = intercepts[hostname]
    if (hostConfig === null || typeof hostConfig !== 'object') {
      delete intercepts[hostname]
      dropped += 1
      continue
    }
    for (const pattern of Object.keys(hostConfig)) {
      if (hostConfig[pattern] === null) {
        delete hostConfig[pattern]
        dropped += 1
      }
    }
    if (Object.keys(hostConfig).length === 0) {
      delete intercepts[hostname]
    }
  }
  return dropped
}

/**
 * extra/ 的定位：`DS_EXTRA_PATH` 由宿主在启动前设好（安装包 = `<bundle>/extra`），
 * mitmproxy 要的是 extra 的**父目录**（它自己再拼 `scripts/`、`pac/`）。
 */
function resolveExtraPath () {
  const fromEnv = process.env.DS_EXTRA_PATH
  if (fromEnv) {
    return fromEnv
  }
  const guess = path.join(__dirname, '..', 'extra')
  return fs.existsSync(guess) ? guess : null
}

/** 读 running.json 并归一化（坏 JSON 按空配置继续，不让数据面直接死掉） */
function loadConfig (configPath) {
  let raw
  try {
    raw = fs.readFileSync(configPath, 'utf-8')
  } catch (e) {
    fatal(`读取 running.json 失败: ${configPath}`, e)
    return {}
  }

  let config
  try {
    config = JSON.parse(raw)
  } catch (e) {
    console.error(`[mitmproxy-bridge] running.json 不是合法 JSON，按空配置继续: ${configPath}`, e.message)
    config = {}
  }
  config.setting = config.setting || {}
  config.plugin = config.plugin || {}
  config.dns = config.dns || {}

  const dropped = dropNullIntercepts(config.intercepts)
  if (dropped) {
    console.error(`[mitmproxy-bridge] 已忽略 ${dropped} 条值为 null 的 intercepts 配置（表示关闭该规则）`)
  }

  const extraPath = resolveExtraPath()
  if (extraPath) {
    process.env.DS_EXTRA_PATH = extraPath
    // 不设 rootDir 的话 options.js:85 会拼出 `undefined/scripts`，插件脚本全加载不到
    config.setting.rootDir = path.dirname(extraPath)
  } else {
    console.error('[mitmproxy-bridge] 未找到 extra/ 目录（DS_EXTRA_PATH 未设置），插件脚本可能加载失败')
  }

  return config
}

/** 统一入口：拿到 mitmproxy 模块后调用 */
function run (mitmproxy, configPath) {
  if (!configPath) {
    fatal('缺少 running.json 路径参数')
    return
  }
  if (typeof mitmproxy.start !== 'function') {
    fatal('@blue-frontier/mitmproxy 没有导出 start()')
    return
  }
  const config = loadConfig(configPath)
  Promise.resolve()
    .then(() => mitmproxy.start(config))
    .catch((err) => fatal('数据面启动失败', err))
}

module.exports = { fatal, dropNullIntercepts, resolveExtraPath, loadConfig, run }
