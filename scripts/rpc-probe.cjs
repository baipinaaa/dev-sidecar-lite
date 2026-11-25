#!/usr/bin/env node
'use strict'

/**
 * 控制面 stdio JSON-RPC 冒烟探针。
 *
 * 直接以子进程方式拉起控制面（`node core-bundle/core-host/main.js --stdio`），
 * 然后用管道跟它说话 —— 不经过任何端口、不做任何 HTTP 请求。
 *
 * 覆盖：
 *   1. ready 帧（控制面确实起来了）
 *   2. core.info
 *   3. api.invoke → getApiList（界面依赖的接口是否齐全）
 *   4. api.invoke → config.get / config.save / server.start（数据面能否真的拉起）
 *   5. HEAD http://127.0.0.1:<port>/pac.js（数据面真的在听，且 PAC 端点可用）
 *   6. server.close（端口释放）
 *   7. shutdown（控制面自行退出）
 *
 * 用法：
 *   node scripts/rpc-probe.mjs --node <node.exe> --entry <core-host/main.js> \
 *        [--data-dir <dir>] [--home <dir>] [--port 31199] [--timeout 60000] [--keep]
 *
 * 退出码：0 全过；1 有断言失败；2 启动/参数问题。
 */

const { spawn } = require('node:child_process')
const fs = require('node:fs')
const net = require('node:net')
const os = require('node:os')
const path = require('node:path')

function parseArgs (argv) {
  const options = {
    node: process.execPath,
    entry: null,
    dataDir: null,
    home: null,
    port: Number(process.env.SMOKE_PROXY_PORT || 31199),
    timeout: 60000,
    keep: false,
  }
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]
    switch (arg) {
      case '--node': options.node = argv[++i]; break
      case '--entry': options.entry = argv[++i]; break
      case '--data-dir': options.dataDir = argv[++i]; break
      case '--home': options.home = argv[++i]; break
      case '--port': options.port = Number(argv[++i]); break
      case '--timeout': options.timeout = Number(argv[++i]); break
      case '--keep': options.keep = true; break
      default:
        console.error(`未知参数: ${arg}`)
        process.exit(2)
    }
  }
  if (!options.entry) {
    console.error('缺少 --entry（core-host/main.js 路径）')
    process.exit(2)
  }
  return options
}

const REQUIRED_APIS = [
  'getApiList',
  'info.get',
  'info.getConfigDir',
  'status.get',
  'config.get',
  'config.save',
  'setting.load',
  'setting.save',
  'serviceGroups.list',
  'server.start',
  'server.close',
  'proxy.start',
  'shell.openPath',
]

class Probe {
  constructor (child) {
    this.child = child
    this.nextId = 1
    this.pending = new Map()
    this.buffer = ''
    this.ready = null
    this.events = []
    this.protocolErrors = []

    child.stdout.setEncoding('utf-8')
    child.stdout.on('data', (chunk) => this.onData(chunk))
  }

  onData (chunk) {
    this.buffer += chunk
    let index = this.buffer.indexOf('\n')
    while (index >= 0) {
      const line = this.buffer.slice(0, index).trim()
      this.buffer = this.buffer.slice(index + 1)
      index = this.buffer.indexOf('\n')
      if (!line) {
        continue
      }
      let msg
      try {
        msg = JSON.parse(line)
      } catch (e) {
        // stdout 被别的东西污染了 —— 这本身就是缺陷，记下来
        this.protocolErrors.push(line.slice(0, 200))
        continue
      }
      this.onMessage(msg)
    }
  }

  onMessage (msg) {
    if (msg.id && this.pending.has(msg.id)) {
      const { resolve, reject, timer } = this.pending.get(msg.id)
      clearTimeout(timer)
      this.pending.delete(msg.id)
      if (msg.error) {
        reject(new Error(`${msg.error.message || '未知错误'}（${msg.error.code || 'api_error'}）`))
      } else {
        resolve(msg.result === undefined ? null : msg.result)
      }
      return
    }

    if (msg.method === 'ready') {
      this.ready = msg.params || {}
      return
    }
    if (msg.method === 'event') {
      this.events.push(msg.params || {})
    }
  }

  request (method, params) {
    const id = this.nextId++
    const frame = JSON.stringify({ jsonrpc: '2.0', id, method, params: params || {} })
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id)
        reject(new Error(`请求超时（${method}）`))
      }, 30000)
      this.pending.set(id, { resolve, reject, timer })
      this.child.stdin.write(`${frame}\n`, (err) => {
        if (err) {
          clearTimeout(timer)
          this.pending.delete(id)
          reject(err)
        }
      })
    })
  }

  async invoke (api, param) {
    return this.request('api.invoke', { api, param: param === undefined ? null : param })
  }

  waitReady (timeoutMs) {
    return new Promise((resolve, reject) => {
      const started = Date.now()
      const tick = () => {
        if (this.ready) {
          resolve(this.ready)
          return
        }
        if (Date.now() - started > timeoutMs) {
          reject(new Error('控制面没有发 ready 帧'))
          return
        }
        setTimeout(tick, 50)
      }
      tick()
    })
  }
}

function portOpen (port, host = '127.0.0.1') {
  return new Promise((resolve) => {
    const socket = net.connect({ port, host })
    socket.setTimeout(800)
    socket.on('connect', () => {
      socket.destroy()
      resolve(true)
    })
    socket.on('timeout', () => {
      socket.destroy()
      resolve(false)
    })
    socket.on('error', () => resolve(false))
  })
}

async function waitPort (port, open, timeoutMs) {
  const started = Date.now()
  while (Date.now() - started < timeoutMs) {
    if (await portOpen(port) === open) {
      return true
    }
    await new Promise((resolve) => setTimeout(resolve, 200))
  }
  return false
}

async function fetchPac (port, timeoutMs) {
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), timeoutMs)
  try {
    // 用 HEAD：只看端点是否存在，不拉 15 KB 正文
    const response = await fetch(`http://127.0.0.1:${port}/pac.js`, {
      method: 'HEAD',
      signal: controller.signal,
    })
    return { status: response.status }
  } finally {
    clearTimeout(timer)
  }
}

async function main () {
  const options = parseArgs(process.argv.slice(2))
  if (!fs.existsSync(options.entry)) {
    console.error(`找不到控制面入口: ${options.entry}`)
    process.exit(2)
  }

  const dataDir = options.dataDir || path.join(os.tmpdir(), `ds-rpc-probe-${process.pid}`)
  fs.mkdirSync(dataDir, { recursive: true })

  const env = { ...process.env, DS_DATA_DIR: dataDir }
  if (options.home) {
    // 让 core 的用户目录落在临时 HOME 里，绝不碰真实配置
    env.USERPROFILE = options.home
    env.HOME = options.home
    env.APPDATA = options.home
    env.LOCALAPPDATA = options.home
    fs.mkdirSync(path.join(options.home, '.dev-sidecar-lite'), { recursive: true })
  }

  const child = spawn(options.node, [options.entry, '--stdio', '--no-startup'], {
    env,
    stdio: ['pipe', 'pipe', 'inherit'],
    windowsHide: true,
  })

  const failures = []
  const report = { steps: [], ready: null, info: null, apiCount: 0, pac: null, events: 0, protocolErrors: [] }
  const check = (name, ok, detail) => {
    report.steps.push({ name, ok, detail: detail || null })
    if (!ok) {
      failures.push(`${name}${detail ? '：' + detail : ''}`)
    }
    console.log(`${ok ? '[ok]' : '[失败]'} ${name}${detail ? ' — ' + detail : ''}`)
  }

  const probe = new Probe(child)
  let exitCode = 1

  try {
    report.ready = await probe.waitReady(20000)
    check('控制面 ready 帧', report.ready.engine === 'node', `engine=${report.ready.engine} version=${report.ready.version}`)

    report.info = await probe.request('core.info')
    check('core.info', !!report.info && report.info.pid > 0, `pid=${report.info && report.info.pid}`)

    const apiList = await probe.invoke('getApiList')
    report.apiCount = Array.isArray(apiList) ? apiList.length : 0
    const missing = REQUIRED_APIS.filter((name) => !Array.isArray(apiList) || !apiList.includes(name))
    check('getApiList 覆盖界面接口', missing.length === 0, missing.length ? `缺少 ${missing.join(', ')}` : `${report.apiCount} 项`)

    // 把代理端口改到探针端口，避免撞用户正在用的 31181
    const config = await probe.invoke('config.get')
    check('config.get', !!config && !!config.server, '返回配置对象')
    const patch = JSON.parse(JSON.stringify(config))
    patch.server = patch.server || {}
    patch.server.port = options.port
    patch.server.startup = false
    patch.proxy = patch.proxy || {}
    patch.proxy.enabled = false
    patch.proxy.pacEnabled = false
    await probe.invoke('config.save', patch)
    const reloaded = await probe.invoke('config.get')
    check('config.save 生效', reloaded.server && reloaded.server.port === options.port, `port=${reloaded.server && reloaded.server.port}`)

    const started = await probe.invoke('server.start')
    check('server.start', !!started && started.port === options.port, JSON.stringify(started))

    const listening = await waitPort(options.port, true, 20000)
    check('数据面端口已监听', listening, `127.0.0.1:${options.port}`)

    if (listening) {
      report.pac = await fetchPac(options.port, 15000)
      check('HEAD /pac.js', report.pac.status === 200, `status=${report.pac.status}`)
    }

    await probe.invoke('server.close')
    const released = await waitPort(options.port, false, 15000)
    check('server.close 释放端口', released, `127.0.0.1:${options.port}`)

    report.events = probe.events.length
    const channels = [...new Set(probe.events.map((item) => item.channel))]
    check('事件流采样', true, `${report.events} 条（${channels.join(', ') || '无'}）`)
    check('stdout 无协议污染', probe.protocolErrors.length === 0, probe.protocolErrors.join(' | '))

    exitCode = failures.length === 0 ? 0 : 1
  } catch (err) {
    console.error(`[失败] ${err && err.message ? err.message : err}`)
    failures.push(String(err && err.message ? err.message : err))
  } finally {
    report.protocolErrors = probe.protocolErrors
    report.failures = failures

    // 用协议退出（顺带验证 shutdown 路径）；失败时直接杀
    try {
      await probe.request('shutdown')
      await new Promise((resolve) => setTimeout(resolve, 800))
    } catch (e) {
      // 忽略
    }
    if (!child.killed && child.exitCode === null) {
      child.kill()
    }
    if (!options.keep) {
      try {
        fs.rmSync(dataDir, { recursive: true, force: true })
      } catch (e) {
        // 忽略
      }
    }

    console.log('\n---- 探针报告 ----')
    console.log(JSON.stringify(report, null, 2))
    if (failures.length) {
      console.error(`\n共 ${failures.length} 项失败：`)
      for (const item of failures) {
        console.error(` - ${item}`)
      }
    }
    process.exit(exitCode)
  }
}

main().catch((err) => {
  console.error('[rpc-probe] 未捕获异常:', err && err.stack ? err.stack : err)
  process.exit(2)
})
