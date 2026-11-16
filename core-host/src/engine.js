'use strict'

/**
 * 数据面引擎选择。
 *
 * **这里刻意不改 Node core 的一行代码**：core 的 `server.start({ mitmproxyPath })` 已经把
 * "用哪个子进程当数据面"参数化成了 `mitmproxyPath`。所以换引擎只需要给它一个**不同的入口**：
 *
 * - `node` → 原来的 `@blue-frontier/mitmproxy`（一个 fork 出来的 Node 进程）
 * - `rust` → `shim/rust-engine.js`（一个极薄的 Node 进程，只做 IPC ↔ stdin/stdout 桥接，
 *            真正的数据面是它 spawn 的 `ds-proxy.exe`）
 *
 * 这样旧仓库保持零改动，回退只需把 `proxy.engine` 改回 `node`。
 */

const fs = require('node:fs')
const path = require('node:path')

const runtime = require('./runtime')

/**
 * shim 必须**留在磁盘上**（core 会 fork 它），不能打进 SEA。
 * 打包后位于 `<安装目录>/resources/core-bundle/core-host/shim/rust-engine.js`。
 */
function resolveShimPath () {
  const candidates = [
    process.env.DS_resolveShimPath(),
    ...runtime.findInResources('core-bundle', 'core-host', 'shim', 'rust-engine.js'),
    path.join(__dirname, '..', 'shim', 'rust-engine.js'),
  ].filter(Boolean)

  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) {
      return candidate
    }
  }
  return path.join(__dirname, '..', 'shim', 'rust-engine.js')
}

/**
 * 找 `ds-proxy.exe`。
 *
 * 顺序：环境变量 → 宿主同目录（安装包/Tauri sidecar 布局）→ 仓库开发产物。
 */
function resolveRustProxyPath () {
  const candidates = [
    process.env.DS_PROXY_EXE,
    path.join(path.dirname(process.execPath), 'ds-proxy.exe'),
    path.join(__dirname, '../../../target/release/ds-proxy.exe'),
    path.join(__dirname, '../../../target/debug/ds-proxy.exe'),
  ]

  for (const candidate of candidates) {
    if (candidate && fs.existsSync(candidate)) {
      return candidate
    }
  }
  return null
}

/**
 * 解析本次应该用哪个引擎。
 *
 * @param {object} options
 * @param {string} [options.configured] `proxy.engine`，取值 auto/node/rust
 * @param {string|null} [options.rustPath] 已解析的 ds-proxy.exe
 * @returns {{engine: 'node'|'rust', rustPath: string|null, reason: string}}
 */
function resolveEngine ({ configured, rustPath }) {
  const wanted = configured || 'auto'
  const exe = rustPath === undefined ? resolveRustProxyPath() : rustPath

  if (wanted === 'node') {
    return { engine: 'node', rustPath: null, reason: '配置指定 node' }
  }
  if (wanted === 'rust') {
    if (exe) {
      return { engine: 'rust', rustPath: exe, reason: '配置指定 rust' }
    }
    return { engine: 'node', rustPath: null, reason: '配置指定 rust，但未找到 ds-proxy.exe，已回退' }
  }
  if (exe) {
    return { engine: 'rust', rustPath: exe, reason: 'auto 命中 ds-proxy.exe' }
  }
  return { engine: 'node', rustPath: null, reason: 'auto 未找到 ds-proxy.exe' }
}

/**
 * 给 core 的 `mitmproxyPath`。
 *
 * @param {'node'|'rust'} engine
 * @param {string|null} nodeEntry  Node 版 mitmproxy 入口
 * @returns {string} 实际传给 core 的入口脚本
 */
function entryFor (engine, nodeEntry) {
  if (engine === 'rust') {
    return resolveShimPath()
  }
  if (!nodeEntry) {
    throw new Error('Node 引擎入口缺失：请设置 DS_MITMPROXY_PATH')
  }
  return nodeEntry
}

module.exports = {
  resolveShimPath,
  entryFor,
  resolveEngine,
  resolveRustProxyPath,
}
