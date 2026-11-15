'use strict'

/**
 * core-host 的冒烟测试：不需要 core 的依赖，用一个假 core 验证传输层。
 *
 * 覆盖：鉴权、未知接口、接口抛错、SSE 事件回流、日志回流、engine.status / engine.set。
 *
 * 运行：node tests/e2e/core_host_smoke.js
 */

const assert = require('node:assert')
const http = require('node:http')
const os = require('node:os')
const path = require('node:path')
const fs = require('node:fs')

const { createHost } = require('../../core-host/src/host')

function createFakeCore () {
  const listeners = []
  const state = {
    status: { server: { enabled: false }, proxy: { enabled: false, pacEnabled: false }, plugin: {} },
    config: { proxy: { engine: 'auto' }, server: { enabled: false, setting: {} } },
  }

  const api = {
    status: {
      get: () => state.status,
    },
    config: {
      get: () => state.config,
      update: (part) => {
        state.config = { ...state.config, ...part }
        return state.config
      },
    },
    event: {
      register: (channel, handle) => {
        listeners.push({ channel, handle })
        return listeners.length
      },
      fire: (channel, event) => {
        for (const item of listeners) {
          if (item.channel === channel) {
            item.handle(event)
          }
        }
      },
    },
    log: {
      info: () => {},
      warn: () => {},
      error: () => {},
      debug: () => {},
    },
    server: {
      enabled: false,
      getEngine: () => 'node',
      restart: async () => {
        api.server.enabled = true
      },
    },
    proxy: {
      start: async () => {
        state.status.proxy.enabled = true
        api.event.fire('status', { key: 'proxy.enabled', value: true })
        return { ok: true }
      },
      boom: async () => {
        throw new Error('故意失败')
      },
    },
  }

  return { api, state, listeners }
}

function request (options, body) {
  return new Promise((resolve, reject) => {
    const req = http.request(options, (res) => {
      const chunks = []
      res.on('data', (chunk) => chunks.push(chunk))
      res.on('end', () => {
        resolve({ status: res.statusCode, headers: res.headers, text: Buffer.concat(chunks).toString('utf-8') })
      })
    })
    req.on('error', reject)
    if (body) {
      req.write(JSON.stringify(body))
    }
    req.end()
  })
}

async function main () {
  const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), 'ds-core-host-'))
  const { api } = createFakeCore()
  const host = createHost({ core: { api }, dataDir, port: 0 })
  const address = await host.listen('127.0.0.1', 0)
  const base = { host: '127.0.0.1', port: address.port }
  const auth = { Authorization: `Bearer ${host.token}` }
  const json = { 'Content-Type': 'application/json', ...auth }

  // 1) 无鉴权 → 401
  let res = await request({ ...base, path: '/invoke', method: 'POST', headers: { 'Content-Type': 'application/json' } }, { api: 'status.get' })
  assert.strictEqual(res.status, 401, '无鉴权应 401')

  // 2) 未知接口 → 404
  res = await request({ ...base, path: '/invoke', method: 'POST', headers: json }, { api: 'nope.nope' })
  assert.strictEqual(res.status, 404, '未知接口应 404')
  assert.ok(res.text.includes('未知接口'), res.text)

  // 3) 正常调用
  res = await request({ ...base, path: '/invoke', method: 'POST', headers: json }, { api: 'proxy.start', param: { a: 1 } })
  assert.strictEqual(res.status, 200, res.text)
  assert.deepStrictEqual(JSON.parse(res.text), { ok: true })

  // 4) 接口抛错 → 500 + 原始 message
  res = await request({ ...base, path: '/invoke', method: 'POST', headers: json }, { api: 'proxy.boom' })
  assert.strictEqual(res.status, 500)
  assert.ok(res.text.includes('故意失败'), res.text)

  // 5) engine.status / engine.set
  res = await request({ ...base, path: '/invoke', method: 'POST', headers: json }, { api: 'engine.status' })
  const engineStatus = JSON.parse(res.text)
  assert.strictEqual(engineStatus.engine, 'node')
  assert.strictEqual(engineStatus.configured, 'auto')

  res = await request({ ...base, path: '/invoke', method: 'POST', headers: json }, { api: 'engine.set', param: { engine: 'rust' } })
  assert.strictEqual(res.status, 200, res.text)
  assert.strictEqual(JSON.parse(res.text).configured, 'rust', 'engine.set 应写入配置')

  res = await request({ ...base, path: '/invoke', method: 'POST', headers: json }, { api: 'engine.set', param: { engine: 'wasm' } })
  assert.ok(res.text.includes('未知引擎'), res.text)

  // 6) SSE 事件回流：先连上，再触发 event.fire 与 log
  const sse = await new Promise((resolve, reject) => {
    const req = http.request({ ...base, path: '/events', method: 'GET', headers: auth }, (stream) => resolve(stream))
    req.on('error', reject)
    req.end()
  })

  const frames = []
  sse.setEncoding('utf-8')
  sse.on('data', (chunk) => frames.push(chunk))
  await new Promise((resolve) => setTimeout(resolve, 60))

  api.event.fire('status', { key: 'proxy.enabled', value: true })
  api.log.info('测试日志', { a: 1 })
  await new Promise((resolve) => setTimeout(resolve, 120))

  const joined = frames.join('')
  assert.ok(joined.includes('event: status'), `应收到 status 事件: ${joined}`)
  assert.ok(joined.includes('"key":"proxy.enabled"'), joined)
  assert.ok(joined.includes('event: log'), `应收到 log 事件: ${joined}`)
  assert.ok(joined.includes('测试日志'), joined)

  sse.destroy()

  // 7) token 文件已落盘
  const tokenFile = path.join(dataDir, 'run', 'core.token')
  assert.ok(fs.existsSync(tokenFile), 'token 文件应存在')
  assert.strictEqual(fs.readFileSync(tokenFile, 'utf-8').trim(), host.token)

  await host.close()
  fs.rmSync(dataDir, { recursive: true, force: true })

  console.log('core-host 冒烟测试全部通过 ✅')
}

main().catch((err) => {
  console.error('core-host 冒烟测试失败 ❌')
  console.error(err)
  process.exit(1)
})
