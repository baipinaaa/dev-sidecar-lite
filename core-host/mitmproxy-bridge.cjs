'use strict'

/**
 * 数据面入口（开发 / 未打平布局）。
 *
 * core 的 `server.start({ mitmproxyPath })` 是 `fork(mitmproxyPath, [runningConfigPath])`，
 * 被 fork 的脚本要自己读 `argv[2]` 的 running.json 再 `start(config)`。原版这个脚本在
 * `packages/gui/src/bridge/mitmproxy.js`（属于 GUI 包，lite 里没有），而且是 ESM
 * （本仓库没有 `"type": "module"`，fork 时会被当 CJS 解析而报错）。
 *
 * 打平布局（S3/L2）用的是 `core-bundle/bundle/mitmproxy.cjs`（entry 见 `bridge-entry.cjs`），
 * 两者共用 `src/bridge-runner.js` 的同一份逻辑。
 */

const path = require('node:path')

const runner = require('./src/bridge-runner')

/**
 * 解析 `@blue-frontier/mitmproxy`。
 *
 * 数据面是 `fork` 出来的独立进程，模块解析基址是本文件所在目录（`core-host/`），
 * 那里没有 `node_modules`；安装包里依赖树在 `core-bundle/node_modules`。
 */
function loadMitmproxy (extraPath) {
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
  runner.fatal('加载 @blue-frontier/mitmproxy 失败（core-bundle/node_modules 是否完整？）', tried.join('\n'))
}

// 用户配置目录与 core 保持一致：mitmproxy 自己的 `lib/proxy/common/config.js:21` 也硬编码了
// `~/.dev-sidecar`（CA 证书默认路径），赶在加载前统一改掉更保险。
require('./src/runtime').installUserDir()

const extraPath = runner.resolveExtraPath()
runner.run(loadMitmproxy(extraPath), process.argv[2])
