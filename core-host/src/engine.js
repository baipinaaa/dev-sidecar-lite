'use strict'

/**
 * 数据面引擎。
 *
 * v0.3.0 起只有一个数据面：`@blue-frontier/mitmproxy`（core 用 fork 拉起的 Node 子进程）。
 * 原来的 Rust 数据面（`crates/**` + `shim/rust-engine.js` + `ds-proxy.exe`）已经删除——
 * 它只覆盖 CONNECT 隧道与朴素 MITM，缺 DNS 取址、拦截器链、脚本注入、`/pac.js` 等能力，
 * 留着一个半成品只会让文档、CI、UI 三处都要为它付代价。
 *
 * core 的 `server.start({ mitmproxyPath })` 把「用哪个子进程当数据面」参数化成了
 * `mitmproxyPath`，所以这里只需要把入口脚本交给 core。
 */

/**
 * 解析本次应该用哪个引擎。
 *
 * 函数形状刻意保留（`engine.status` 仍返回 `{engine:'node'}`），避免旧前端拿到 404。
 *
 * @returns {{engine: 'node', reason: string}}
 */
function resolveEngine () {
  return { engine: 'node', reason: '仅支持 Node 数据面' }
}

/**
 * 给 core 的 `mitmproxyPath`。
 *
 * @param {string|null} nodeEntry Node 版 mitmproxy 入口（core-bundle/core-host/mitmproxy-bridge.cjs）
 * @returns {string} 实际传给 core 的入口脚本
 */
function entryFor (nodeEntry) {
  if (!nodeEntry) {
    throw new Error('Node 引擎入口缺失：请设置 DS_MITMPROXY_PATH 或确认 core-bundle 里有 core-host/mitmproxy-bridge.cjs')
  }
  return nodeEntry
}

module.exports = {
  entryFor,
  resolveEngine,
}
