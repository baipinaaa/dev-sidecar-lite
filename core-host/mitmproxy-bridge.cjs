'use strict'

/**
 * Node 数据面入口（路线 4 的默认引擎）。
 *
 * 为什么不用 `@blue-frontier/mitmproxy/src/index.js` 直接当入口：
 * core 的 `server.start({ mitmproxyPath })` 是 `fork(mitmproxyPath, [runningConfigPath])`，
 * 也就是说被 fork 的脚本要自己读 `argv[2]` 的 running.json 再 `start(config)`。
 * 原版这个脚本在 `packages/gui/src/bridge/mitmproxy.js`（属于 GUI 包，lite 里没有），
 * 而且它是 ESM（本仓库没有 `"type": "module"`，fork 时会当成 CJS 解析而报错）。
 * 所以这里用 `.cjs` 复刻同样的五行逻辑，并把 `setting.rootDir` 补上——不补的话
 * `options.js:85` 会拼出 `undefined/scripts`，插件脚本全加载不到。
 */

const fs = require('node:fs')
const path = require('node:path')

const configPath = process.argv[2]

function fatal (message, err) {
  // 这里不能用 mitmproxy 的 logger：它可能正是因为加载失败才走到这
  console.error('[mitmproxy-bridge] ' + message, err && err.stack ? err.stack : (err || ''))
  process.exit(1)
}

if (!configPath) {
  fatal('缺少 running.json 路径参数')
}

let raw
try {
  raw = fs.readFileSync(configPath, 'utf-8')
} catch (e) {
  fatal(`读取 running.json 失败: ${configPath}`, e)
}

let config
try {
  config = JSON.parse(raw)
} catch (e) {
  // 原版用 json5 解析（允许注释）；坏文件不该让数据面直接死掉，按空配置继续
  console.error(`[mitmproxy-bridge] running.json 不是合法 JSON，按空配置继续: ${configPath}`, e.message)
  config = {}
}
config.setting = config.setting || {}
config.plugin = config.plugin || {}
config.dns = config.dns || {}

/**
 * 清掉 intercepts 里的 null 条目。
 *
 * 为什么要动数据：内置配置用 `null` 表示「关掉这条内置规则」（core 的
 * `src/config/index.js:675` 就是这么关 github 的油猴脚本的），但 mitmproxy 的
 * `lib/interceptor/impl/res/script.js:138` 直接读 `pathConfig.script`，遇到 null 就抛
 * `TypeError: Cannot read properties of null`，整个数据面起不来。
 * 原版能绕过是因为联网拉到远程配置把 null 覆盖掉了；离线/首次启动就没这么幸运。
 */
function dropNullIntercepts (intercepts) {
  if (!intercepts || typeof intercepts !== 'object') {
    return null
  }
  let dropped = 0
  for (const hostname of Object.keys(intercepts)) {
    const hostConfig = intercepts[hostname]
    if (hostConfig === null || typeof hostConfig !== 'object') {
      // 整个域名被设为 null：等价于「不拦截这个域名」
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

const droppedIntercepts = dropNullIntercepts(config.intercepts)
if (droppedIntercepts) {
  console.error(`[mitmproxy-bridge] 已忽略 ${droppedIntercepts} 条值为 null 的 intercepts 配置（表示关闭该规则）`)
}

/**
 * extra/ 的定位。
 *
 * `DS_EXTRA_PATH` 由 core-host 在启动 core 之前设好（安装包 = `<安装目录>/core-bundle/extra`）。
 * mitmproxy 要的是 extra 的**父目录**（它自己再拼 `scripts/`、`pac/`）。
 */
function resolveExtraPath () {
  const fromEnv = process.env.DS_EXTRA_PATH
  if (fromEnv) {
    return fromEnv
  }
  // 退路：<bundle>/core-host/mitmproxy-bridge.cjs → <bundle>/extra
  const guess = path.join(__dirname, '..', 'extra')
  if (fs.existsSync(guess)) {
    return guess
  }
  return null
}

const extraPath = resolveExtraPath()
if (extraPath) {
  process.env.DS_EXTRA_PATH = extraPath
  config.setting.rootDir = path.dirname(extraPath)
} else {
  console.error('[mitmproxy-bridge] 未找到 extra/ 目录（DS_EXTRA_PATH 未设置），插件脚本可能加载失败')
}

let mitmproxy
/**
 * 解析 `@blue-frontier/mitmproxy`。
 *
 * 为什么不能直接 `require('@blue-frontier/mitmproxy')`：数据面是 `fork` 出来的独立进程，
 * 它的模块解析基址是本文件所在目录（`core-host/`），那里没有 `node_modules`；
 * 安装包里依赖树在 `core-bundle/node_modules`，源码开发时在 `app/src-tauri/core-bundle`。
 * 所以按 `DS_MITMPROXY_PATH` → `extra/` 的同级 `node_modules` 顺序找。
 */
function loadMitmproxy () {
  const tried = []
  const roots = []
  const add = (dir) => {
    if (dir && !roots.includes(dir)) {
      roots.push(dir)
    }
  }
  add(process.env.DS_MITMPROXY_PATH)
  if (extraPath) {
    // <bundle>/extra → <bundle>/node_modules/@blue-frontier/mitmproxy
    add(path.join(path.dirname(extraPath), 'node_modules', '@blue-frontier', 'mitmproxy'))
    // 旧布局：<bundle>/node_modules/@blue-frontier/mitmproxy 的兄弟目录 core-host
    add(path.join(path.dirname(extraPath), 'node_modules', 'mitmproxy'))
  }
  add(path.join(__dirname, '..', 'node_modules', '@blue-frontier', 'mitmproxy'))

  for (const dir of roots) {
    try {
      return require(dir)
    } catch (e) {
      tried.push(`${dir} → ${e.message}`)
    }
  }
  // 最后再试一次包名（pnpm 装好的开发仓库里能用）
  try {
    return require('@blue-frontier/mitmproxy')
  } catch (e) {
    tried.push(`@blue-frontier/mitmproxy → ${e.message}`)
  }
  fatal('加载 @blue-frontier/mitmproxy 失败（core-bundle/node_modules 是否完整？）', tried.join('\n'))
}

// 用户配置目录与 core 保持一致：mitmproxy 自己的
// `lib/proxy/common/config.js:21` 也硬编码了 `~/.dev-sidecar`（CA 证书默认路径），
// core 虽然会用 setting.userBasePath 覆盖，但赶在加载前统一改掉更保险。
require('./src/runtime').installUserDir()

mitmproxy = loadMitmproxy()

if (typeof mitmproxy.start !== 'function') {
  fatal('@blue-frontier/mitmproxy 没有导出 start()')
}

Promise.resolve()
  .then(() => mitmproxy.start(config))
  .catch((err) => fatal('数据面启动失败', err))
