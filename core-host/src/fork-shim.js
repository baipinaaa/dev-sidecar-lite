'use strict'

/**
 * SEA 下的 `child_process.fork()` 补丁。
 *
 * 问题：core 的 `modules/server/index.js:173` 用 `fork(mitmproxyPath, [runningConfigPath])`
 * 启动数据面，而 `fork` 默认 `execPath = process.execPath`。安装包里 `process.execPath`
 * 是单文件可执行 `ds-core-host.exe`（Node SEA）——SEA 只会运行内嵌入口，
 * **完全忽略 `fork` 传进去的脚本路径**，于是子进程又跑了一遍 ds-core-host，
 * 把 running.json 当成未知参数打印 usage 后 exit(2)：数据面永远起不来，
 * 上层只看到一句 `Proxy port not listening`。
 *
 * 办法：把 `fork(script, args)` 改写成
 *   `spawn(process.execPath, ['--mitmproxy-bridge', script, ...args], { stdio: [...,'ipc'] })`
 * 子进程仍是 SEA，但会走 `main.js` 里的 `--mitmproxy-bridge` 分支去加载真正的脚本；
 * `stdio` 保留 `ipc`，所以 core 的 `serverProcess.send({type:'action'})` 与
 * mitmproxy 的 `process.send({type:'status'})` 照常工作。
 */

const cp = require('node:child_process')

const BRIDGE_FLAG = '--mitmproxy-bridge'

let installed = false

function isSea () {
  try {
    const sea = require('node:sea')
    return typeof sea.isSea === 'function' && sea.isSea()
  } catch (e) {
    return false
  }
}

function installForkShim (options = {}) {
  if (installed) {
    return false
  }
  if (!isSea()) {
    // 源码/普通 node 运行时不需要这套：`node script.js` 本来就是对的
    return false
  }
  const originalFork = cp.fork
  cp.fork = function forkPatched (modulePath, args, forkOptions) {
    const opts = forkOptions || {}
    const execPath = opts.execPath || process.execPath
    if (execPath !== process.execPath) {
      // 调用方显式指定了别的可执行（例如真 node），照旧
      return originalFork.call(this, modulePath, args, forkOptions)
    }
    const spawnOptions = Object.assign({}, opts, {
      env: opts.env || process.env,
      stdio: opts.stdio || ['ignore', 'inherit', 'inherit', 'ipc'],
    })
    delete spawnOptions.execPath
    if (options.verbose) {
      console.log(`[core-host] fork 改写：${modulePath}`)
    }
    return cp.spawn(process.execPath, [BRIDGE_FLAG, modulePath].concat(args || []), spawnOptions)
  }
  installed = true
  return true
}

module.exports = { installForkShim, BRIDGE_FLAG }
