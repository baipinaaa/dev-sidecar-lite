'use strict'

/**
 * 控制面宿主：把 core 的 `api` 暴露成 HTTP。
 *
 * 协议（与 `app/src-tauri/src/lib.rs` 的 CoreClient 对齐）：
 * - `POST /invoke`  body `{ "api": "proxy.startPac", "param": null }` → 返回接口的 JSON 结果
 * - `GET  /events`  SSE，`event: status|speed|traffic|log|error` + `data: <json>`
 * - `GET  /healthz` 探活，无鉴权
 *
 * 鉴权：`Authorization: Bearer <token>`，token 落在 `<dataDir>/run/core.token`（首次自动生成）。
 * 只监听回环地址，避免把「能改系统代理」的接口暴露到局域网。
 */

const http = require('node:http')
const fs = require('node:fs')
const path = require('node:path')
const crypto = require('node:crypto')

const { ApiError, describe, invoke } = require('./dispatch')

const MAX_BODY = 4 * 1024 * 1024
const HEARTBEAT_MS = 15000
const FORWARDED_CHANNELS = new Set(['status', 'speed', 'traffic', 'error', 'log', 'config.changed'])

function resolveDataDir () {
  const fromEnv = process.env.DS_DATA_DIR
  if (fromEnv) {
    return fromEnv
  }
  if (process.env.PROGRAMDATA) {
    return path.join(process.env.PROGRAMDATA, 'dev-sidecar-lite')
  }
  const home = process.env.USERPROFILE || process.env.HOME || '.'
  return path.join(home, '.dev-sidecar-lite')
}

function loadOrCreateToken (dataDir) {
  const dir = path.join(dataDir, 'run')
  const file = path.join(dir, 'core.token')
  try {
    const existing = fs.readFileSync(file, 'utf-8').trim()
    if (existing.length >= 16) {
      return existing
    }
  } catch (e) {
    // 文件不存在或读不了，继续生成
  }

  fs.mkdirSync(dir, { recursive: true })
  const token = crypto.randomBytes(24).toString('hex')
  fs.writeFileSync(file, token, { mode: 0o600 })
  return token
}

function readJsonBody (req) {
  return new Promise((resolve, reject) => {
    const chunks = []
    let size = 0
    req.on('data', (chunk) => {
      size += chunk.length
      if (size > MAX_BODY) {
        reject(new ApiError('请求体过大', 'body_too_large'))
        req.destroy()
        return
      }
      chunks.push(chunk)
    })
    req.on('end', () => {
      const text = Buffer.concat(chunks).toString('utf-8').trim()
      if (!text) {
        resolve(null)
        return
      }
      try {
        resolve(JSON.parse(text))
      } catch (e) {
        reject(new ApiError('请求体不是合法 JSON', 'invalid_json'))
      }
    })
    req.on('error', reject)
  })
}

function writeJson (res, status, payload) {
  const body = Buffer.from(JSON.stringify(payload), 'utf-8')
  res.writeHead(status, {
    'Content-Type': 'application/json; charset=utf-8',
    'Content-Length': body.length,
    'Cache-Control': 'no-store',
  })
  res.end(body)
}

/**
 * 创建宿主。
 *
 * @param {object} options
 * @param {object} options.core        core 模块（含 api）
 * @param {object} [options.localApi]  localApi（优先于 core.api 查找）
 * @param {string} [options.dataDir]   数据目录
 * @param {string} [options.host]      监听地址，默认 127.0.0.1
 * @param {number} [options.port]      监听端口，0 表示随机（实际端口见返回值）
 * @param {string} [options.mitmproxyPath] Node 引擎入口
 * @param {object} [options.setting]   server.setting 覆盖项
 */
function createHost (options) {
  const core = options.core
  if (core == null || core.api == null) {
    throw new ApiError('core 模块无效（缺少 api）', 'invalid_core')
  }

  const api = core.api
  const localApi = options.localApi || null
  const roots = localApi ? [localApi, api] : [api]
  const dataDir = options.dataDir || resolveDataDir()
  const token = options.token || loadOrCreateToken(dataDir)
  const subscribers = new Set()
  const startedAt = Date.now()

  const broadcast = (channel, payload) => {
    if (subscribers.size === 0) {
      return
    }
    const frame = `event: ${channel}\ndata: ${JSON.stringify(payload === undefined ? null : payload)}\n\n`
    for (const res of subscribers) {
      try {
        res.write(frame)
      } catch (e) {
        subscribers.delete(res)
      }
    }
  }

  // 事件回流：包一层 event.fire，保持原有订阅者行为不变
  if (api.event && typeof api.event.fire === 'function') {
    const originalFire = api.event.fire
    api.event.fire = (channel, event) => {
      const result = originalFire.call(api.event, channel, event)
      if (FORWARDED_CHANNELS.has(channel)) {
        broadcast(channel, event)
      }
      return result
    }
  }

  // 日志回流：core 的 log 是单例对象，包一层即可
  if (api.log) {
    for (const level of ['info', 'warn', 'error', 'debug']) {
      const original = api.log[level]
      if (typeof original !== 'function') {
        continue
      }
      api.log[level] = (...args) => {
        const result = original.apply(api.log, args)
        broadcast('log', {
          level,
          time: Date.now(),
          message: args
            .map((item) => (typeof item === 'string' ? item : safeStringify(item)))
            .join(' '),
        })
        return result
      }
    }
  }

  const engineStatus = async () => {
    if (options.engineControl) {
      return options.engineControl.status()
    }
    const engine = typeof api.server.getEngine === 'function' ? api.server.getEngine() : 'node'
    let configured = 'auto'
    try {
      const conf = api.config.get()
      configured = (conf.proxy && conf.proxy.engine) || 'auto'
    } catch (e) {
      // 配置读不到就只报运行时引擎
    }
    return { engine, configured, running: api.status.get().server.enabled === true }
  }

  const handleInvoke = async (body) => {
    const apiName = body && body.api
    const param = body ? body.param : null

    // 三个本地增强接口：Rust 侧需要，Node core 里没有
    if (apiName === 'engine.status') {
      return engineStatus()
    }
    if (apiName === 'engine.set') {
      const engine = param && param.engine
      if (engine !== 'rust' && engine !== 'node' && engine !== 'auto') {
        throw new ApiError(`未知引擎: ${engine}`, 'invalid_engine')
      }
      if (options.engineControl) {
        await options.engineControl.set(engine)
        return engineStatus()
      }
      api.config.update({ proxy: { engine } })
      return engineStatus()
    }
    if (apiName === 'api.list') {
      return describe(api, localApi)
    }
    if (apiName === 'shutdown') {
      // core 的 shutdown 只是置位/关子进程，不会结束本进程（Electron 版的活由 app.quit 干）。
      // 在 core-host 里它就是「关闭本进程」的语义，所以这里接管：先等结果，再走优雅退出。
      const result = await invoke(roots, apiName, param)
      if (options.onShutdown) {
        setTimeout(() => {
          Promise.resolve(options.onShutdown()).finally(() => process.exit(0))
        }, 50)
      }
      return result
    }

    return invoke(roots, apiName, param)
  }

  const server = http.createServer((req, res) => {
    const url = req.url || '/'
    const pathname = url.split('?')[0]

    if (pathname === '/healthz') {
      writeJson(res, 200, { ok: true, uptimeMs: Date.now() - startedAt })
      return
    }

    const auth = req.headers.authorization || ''
    const provided = auth.startsWith('Bearer ') ? auth.slice(7) : ''
    if (provided !== token) {
      writeJson(res, 401, { ok: false, error: '鉴权失败', code: 'authentication_failed' })
      return
    }

    if (pathname === '/events' && req.method === 'GET') {
      res.writeHead(200, {
        'Content-Type': 'text/event-stream; charset=utf-8',
        'Cache-Control': 'no-cache, no-transform',
        Connection: 'keep-alive',
        'X-Accel-Buffering': 'no',
      })
      res.write(': connected\n\n')
      subscribers.add(res)

      const heartbeat = setInterval(() => {
        try {
          res.write(': ping\n\n')
        } catch (e) {
          clearInterval(heartbeat)
          subscribers.delete(res)
        }
      }, HEARTBEAT_MS)

      const cleanup = () => {
        clearInterval(heartbeat)
        subscribers.delete(res)
      }
      req.on('close', cleanup)
      req.on('error', cleanup)
      return
    }

    if (pathname === '/invoke' && req.method === 'POST') {
      readJsonBody(req)
        .then(handleInvoke)
        .then((result) => {
          writeJson(res, 200, result === undefined ? null : result)
        })
        .catch((err) => {
          const status = err instanceof ApiError && err.code === 'unknown_api' ? 404 : 500
          writeJson(res, status, {
            ok: false,
            error: err && err.message ? err.message : String(err),
            code: (err && err.code) || 'api_error',
          })
        })
      return
    }

    writeJson(res, 404, { ok: false, error: `未实现的路径: ${pathname}`, code: 'not_found' })
  })

  return {
    server,
    token,
    dataDir,
    broadcast,
    describe: () => describe(api, localApi),
    listen (host, port) {
      return new Promise((resolve, reject) => {
        server.once('error', reject)
        server.listen(port, host, () => {
          const address = server.address()
          resolve({ host: address.address, port: address.port })
        })
      })
    },
    close () {
      for (const res of subscribers) {
        try {
          res.end()
        } catch (e) {
          // 忽略
        }
      }
      subscribers.clear()
      return new Promise((resolve) => server.close(() => resolve()))
    },
  }
}

function safeStringify (value) {
  try {
    return JSON.stringify(value)
  } catch (e) {
    return String(value)
  }
}

module.exports = {
  createHost,
  loadOrCreateToken,
  resolveDataDir,
}
