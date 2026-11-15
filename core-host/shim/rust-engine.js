'use strict'

/**
 * Rust 数据面的 fork 垫片。
 *
 * core 会用 `fork(mitmproxyPath, [runningConfigPath])` 启动数据面，并通过 IPC 发
 * `{ type: 'action', event: { key: 'close' } }` 来关闭它。这个脚本保持同样的对外契约，
 * 内部换成 spawn `ds-proxy.exe`：
 *
 *   core ──fork+IPC──▶ rust-engine.js ──spawn+stdin/stdout──▶ ds-proxy.exe
 *
 * 之所以要垫一层，是因为 `ds-proxy` 不是 Node 程序，没有 `process.send`。
 * 垫片本身不碰任何字节流（数据面在 ds-proxy 里），只转发控制消息与状态。
 */

const fs = require('node:fs')
const path = require('node:path')
const { spawn } = require('node:child_process')

function resolveProxyExe () {
  const candidates = [
    process.env.DS_PROXY_EXE,
    path.join(path.dirname(process.execPath), 'ds-proxy.exe'),
    path.join(__dirname, '../../target/release/ds-proxy.exe'),
    path.join(__dirname, '../../target/debug/ds-proxy.exe'),
  ]
  for (const candidate of candidates) {
    if (candidate && fs.existsSync(candidate)) {
      return candidate
    }
  }
  return null
}

function send (message) {
  if (process.connected) {
    try {
      process.send(message)
    } catch (e) {
      // core 已断开，无需处理
    }
  }
}

function main () {
  const configPath = process.argv[2]
  if (!configPath) {
    console.error('[rust-engine] 缺少 running.json 参数')
    process.exit(2)
  }

  let config = {}
  try {
    config = JSON.parse(fs.readFileSync(configPath, 'utf-8'))
  } catch (err) {
    console.error(`[rust-engine] 读取 ${configPath} 失败:`, err.message)
    process.exit(2)
  }

  const exe = resolveProxyExe()
  if (!exe) {
    send({ type: 'error', message: '未找到 ds-proxy.exe', event: { code: 'rust_engine_missing' } })
    console.error('[rust-engine] 未找到 ds-proxy.exe，请设置 DS_PROXY_EXE')
    process.exit(3)
  }

  const args = ['--config', configPath]
  if (config.port) {
    args.push('--port', String(config.port))
  }
  if (config.host) {
    args.push('--host', config.host)
  }

  const child = spawn(exe, args, { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true })

  send({ type: 'status', event: true })
  console.error(`[rust-engine] 已启动 ${exe}（pid ${child.pid}）`)

  // core → 垫片：IPC 控制消息转成 NDJSON 写 stdin
  process.on('message', (msg) => {
    if (msg == null || msg.type !== 'action') {
      return
    }
    const key = msg.event && msg.event.key
    if (key === 'close') {
      try {
        child.stdin.write(JSON.stringify({ id: 0, cmd: 'shutdown', args: null }) + '\n')
      } catch (e) {
        // 子进程可能已退出
      }
      setTimeout(() => child.kill(), 1500).unref()
      return
    }
    try {
      child.stdin.write(JSON.stringify({ id: 0, cmd: key, args: msg.event }) + '\n')
    } catch (e) {
      console.error('[rust-engine] 转发控制消息失败:', e.message)
    }
  })

  // ds-proxy → core：stdout 是 NDJSON
  let buffer = ''
  child.stdout.setEncoding('utf-8')
  child.stdout.on('data', (chunk) => {
    buffer += chunk
    let index = buffer.indexOf('\n')
    while (index >= 0) {
      const line = buffer.slice(0, index).trim()
      buffer = buffer.slice(index + 1)
      index = buffer.indexOf('\n')
      if (!line) {
        continue
      }
      let message
      try {
        message = JSON.parse(line)
      } catch (e) {
        console.error('[rust-engine] 非 JSON 输出:', line)
        continue
      }
      if (message.type === 'status') {
        send({ type: 'status', event: message.value !== false })
      } else if (message.type === 'speed') {
        send({ type: 'speed', event: message.value })
      } else if (message.type === 'traffic') {
        send({ type: 'traffic', event: message.value })
      } else if (message.id == null) {
        console.error('[rust-engine] ds-proxy:', line)
      }
    }
  })

  child.stderr.setEncoding('utf-8')
  child.stderr.on('data', (chunk) => {
    const text = String(chunk).trimEnd()
    if (text) {
      console.error(`[ds-proxy] ${text}`)
    }
  })

  child.on('error', (err) => {
    send({ type: 'error', message: err.message, event: { code: 'rust_engine_spawn_failed' } })
    console.error('[rust-engine] spawn 失败:', err.message)
    process.exit(4)
  })

  child.on('exit', (code, signal) => {
    console.error(`[rust-engine] ds-proxy 退出，code=${code} signal=${signal}`)
    send({ type: 'status', event: false })
    process.exit(code == null ? 0 : code)
  })

  const shutdown = (signal) => {
    console.error(`[rust-engine] 收到 ${signal}，转发关闭`)
    try {
      child.stdin.write(JSON.stringify({ id: 0, cmd: 'shutdown', args: null }) + '\n')
    } catch (e) {
      // 忽略
    }
    setTimeout(() => {
      child.kill()
      process.exit(0)
    }, 500).unref()
  }

  process.on('SIGINT', () => shutdown('SIGINT'))
  process.on('SIGTERM', () => shutdown('SIGTERM'))
}

main()
