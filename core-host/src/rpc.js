'use strict'

/**
 * stdio JSON-RPC 传输层（NDJSON：一行一个 JSON，UTF-8）。
 *
 * 为什么不再用 HTTP + SSE：控制面与 GUI 是同机的父子进程，HTTP 带来的是
 * 端口（31190）、token 文件、SSE 30s 超时、403/401 竞态这一整套问题；
 * 管道天生就是「谁的子进程谁说话」，这些全都不需要。
 *
 * 协议（与 `app/src-tauri/src/lib.rs` 的 CoreRpc 对齐）：
 *
 *   GUI → host  {"jsonrpc":"2.0","id":1,"method":"api.invoke","params":{"api":"server.start","param":null}}
 *   host → GUI  {"jsonrpc":"2.0","id":1,"result":<任意 JSON>}
 *   host → GUI  {"jsonrpc":"2.0","id":1,"error":{"code":"unknown_api","message":"api 接口不存在: x"}}
 *   host → GUI  {"jsonrpc":"2.0","method":"ready","params":{"pid":1,"version":"0.3.0"}}
 *   host → GUI  {"jsonrpc":"2.0","method":"event","params":{"channel":"status","payload":{...}}}
 *
 * stdout 只允许出协议帧：`takeOverStdout()` 之后任何 `process.stdout.write`
 * 都被改道到 stderr，避免第三方库（core 的 log4js）把日志混进协议流。
 */

const readline = require('node:readline')

let rawStdoutWrite = null

/**
 * 独占 stdout：之后所有非协议写入都改道 stderr。
 *
 * 这一步必须早于加载 core —— core 的 log4js 在模块初始化时就会打日志。
 */
function takeOverStdout () {
  if (rawStdoutWrite) {
    return
  }
  rawStdoutWrite = process.stdout.write.bind(process.stdout)
  process.stdout.write = (chunk, encoding, callback) => process.stderr.write(chunk, encoding, callback)
}

/** 写一帧协议数据（只有它允许写真正的 stdout）。 */
/**
 * 让子进程的 stdout/stderr 不再继承本进程的 fd 1。
 *
 * core 的 `modules/server/index.js:173` 直接 `fork(mitmproxyPath, [runningConfigPath])`，
 * Node 的 fork 默认 `silent: false`，于是数据面的日志会顺着继承来的 fd 1 混进协议流
 * （CI 冒烟里就是这条断言挂的：「stdout 无协议污染」）。
 *
 * 这里把 fork 包一层：强制 `silent: true`（子进程的 stdout/stderr 变成管道），
 * 再把两个管道接到本进程的 stderr。必须在加载 core **之前**调用，
 * 因为 core 用 `const fork = require('node:child_process').fork` 在模块顶层就取走了函数。
 */
function isolateChildStdio () {
  const cp = require('node:child_process')
  if (cp.fork.__dsIsolated) {
    return
  }

  const original = cp.fork
  const patched = function fork (modulePath, args, options) {
    const realArgs = Array.isArray(args) ? args : []
    const realOptions = (Array.isArray(args) ? options : args) || {}
    const child = original.call(cp, modulePath, realArgs, { ...realOptions, silent: true })
    if (child.stdout) {
      child.stdout.pipe(process.stderr, { end: false })
    }
    if (child.stderr) {
      child.stderr.pipe(process.stderr, { end: false })
    }
    return child
  }
  patched.__dsIsolated = true
  cp.fork = patched
}

function writeFrame (payload) {
  const write = rawStdoutWrite || process.stdout.write.bind(process.stdout)
  write(`${JSON.stringify(payload)}\n`)
}

function errorPayload (err) {
  if (err && typeof err === 'object') {
    return {
      code: (err.code && String(err.code)) || 'api_error',
      message: err.message ? String(err.message) : String(err),
    }
  }
  return { code: 'api_error', message: String(err) }
}

/**
 * 启动 RPC 服务。
 *
 * @param {object} options
 * @param {(api: string, param: any) => Promise<any>} options.handleInvoke 与 HTTP 的 /invoke 等价
 * @param {() => object} [options.info]      `core.info` 的返回值
 * @param {() => any} [options.onShutdown]   `shutdown` 方法：收尾后进程自行退出
 * @param {NodeJS.ReadableStream} [options.input]  默认 process.stdin
 */
function serve (options) {
  const input = options.input || process.stdin
  const pending = new Set()
  const rl = readline.createInterface({ input, crlfDelay: Infinity })

  const handle = async (line) => {
    const text = line.trim()
    if (!text) {
      return
    }

    let msg
    try {
      msg = JSON.parse(text)
    } catch (e) {
      writeFrame({ jsonrpc: '2.0', id: null, error: { code: 'invalid_json', message: '请求不是合法 JSON' } })
      return
    }

    const id = msg && msg.id !== undefined ? msg.id : null
    const method = msg && msg.method
    const params = (msg && msg.params) || {}

    if (!method) {
      writeFrame({ jsonrpc: '2.0', id, error: { code: 'invalid_request', message: '缺少 method' } })
      return
    }

    try {
      let result
      if (method === 'api.invoke') {
        result = await options.handleInvoke(params.api, params.param === undefined ? null : params.param)
      } else if (method === 'core.info') {
        result = options.info ? options.info() : {}
      } else if (method === 'shutdown') {
        result = { ok: true }
        writeFrame({ jsonrpc: '2.0', id, result })
        pending.add('shutdown')
        if (options.onShutdown) {
          Promise.resolve(options.onShutdown()).catch(() => {}).finally(() => process.exit(0))
        } else {
          process.exit(0)
        }
        return
      } else {
        writeFrame({ jsonrpc: '2.0', id, error: { code: 'unknown_method', message: `未知方法: ${method}` } })
        return
      }
      writeFrame({ jsonrpc: '2.0', id, result: result === undefined ? null : result })
    } catch (err) {
      writeFrame({ jsonrpc: '2.0', id, error: errorPayload(err) })
    }
  }

  let queue = Promise.resolve()
  rl.on('line', (line) => {
    // 串行处理：core 的接口很多是「读配置→改→写配置」，并发会互相踩
    queue = queue.then(() => handle(line)).catch((err) => {
      process.stderr.write(`[core-host] 处理请求失败: ${err && err.stack ? err.stack : err}\n`)
    })
  })

  rl.on('close', () => {
    // GUI 退出了（管道 EOF）：自己也退出，避免变成孤儿进程
    if (pending.has('shutdown')) {
      return
    }
    process.stderr.write('[core-host] stdin 已关闭，退出\n')
    if (options.onEof) {
      Promise.resolve(options.onEof()).catch(() => {}).finally(() => process.exit(0))
      return
    }
    process.exit(0)
  })

  return {
    notify (channel, payload) {
      writeFrame({ jsonrpc: '2.0', method: 'event', params: { channel, payload: payload === undefined ? null : payload } })
    },
    ready (params) {
      writeFrame({ jsonrpc: '2.0', method: 'ready', params })
    },
  }
}

module.exports = { serve, takeOverStdout, isolateChildStdio, writeFrame }
