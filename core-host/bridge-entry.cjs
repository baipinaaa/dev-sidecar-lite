'use strict'

/**
 * esbuild 打平入口（S3/L2）：把 mitmproxy 与 bridge 逻辑打进一个 cjs。
 *
 * 产物 `core-bundle/bundle/mitmproxy.cjs` 就是 core `fork()` 的对象，
 * 行为与开发布局的 `core-host/mitmproxy-bridge.cjs` 完全一致（共用 `src/bridge-runner.js`）。
 *
 * 注意顺序：`installUserDir()` 必须早于 `require('@blue-frontier/mitmproxy')`，
 * 因为 mitmproxy 在模块初始化时就算出了 CA 的默认路径（`~/.dev-sidecar`）。
 */

require('./src/runtime').installUserDir()

const runner = require('./src/bridge-runner')
const mitmproxy = require('@blue-frontier/mitmproxy')

runner.run(mitmproxy, process.argv[2])
