'use strict'

/**
 * HTTP + SSE 传输层（**只用于调试**）。
 *
 * v0.3.0 起 GUI 走 stdio JSON-RPC（`./rpc.js`）；HTTP 保留是因为：
 *   - 手工 `curl` 调试最顺手（`curl -X POST http://127.0.0.1:31190/invoke ...`）
 *   - 老版本冒烟脚本 / 第三方工具仍能用
 *
 * 启动方式：`node core-host/src/main.js --port 31190`（不带 `--stdio`）。
 *
 * 鉴权：`Authorization: Bearer <token>`，token 落在 `<dataDir>/run/core.token`。
 * 只监听回环地址，避免把「能改系统代理」的接口暴露到局域网。
 */

const http = require('node:http')
const fs = require('node:fs')
const path = require('node:path')
const crypto = require('node:crypto')

const { ApiError } = require('./dispatch')

const MAX_BODY = 4 * 1024 * 1024
const HEARTBEAT_MS = 15000

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
 * @param {object} options
 * @param {object} options.host   `createHost()` 的返回值
 * @param {string} [options.dataDir]
 * @param {string} [options.token] 指定 token（默认从 `<dataDir>/run/core.token` 读取或生成）
 */
function createHttpTransport (options) {
  const host = options.host
  const dataDir = options.dataDir || host.dataDir
  const token = options.token || loadOrCreateToken(dataDir)
  const subscribers = new Set()
  const startedAt = Date.now()

  const unsubscribe = host.subscribe((channel, payload) => {
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
  })

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
        .then((body) => host.handleInvoke(body && body.api, body ? body.param : null))
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
    listen (host2, port) {
      return new Promise((resolve, reject) => {
        server.once('error', reject)
        server.listen(port, host2, () => {
          const address = server.address()
          resolve({ host: address.address, port: address.port })
        })
      })
    },
    close () {
      unsubscribe()
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

module.exports = {
  createHttpTransport,
  loadOrCreateToken,
}
