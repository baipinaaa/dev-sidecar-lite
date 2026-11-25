// 与 Rust 侧的桥接。
//
// 数据流：
//   前端 → invoke('api_invoke', {api, param}) → Tauri → stdio JSON-RPC → core-host（Node core）→ 真实 API
//   状态 ← listen('status'|'speed'|'traffic') ← Tauri ← core-host stdout 事件 ← ds-core-host
//
// 在纯浏览器里跑（`npm run dev` 不开 Tauri）时自动降级为 mock，方便只调 UI。

const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

let tauriInvoke = null
let tauriListen = null

if (isTauri) {
  const core = await import('@tauri-apps/api/core')
  const event = await import('@tauri-apps/api/event')
  tauriInvoke = core.invoke
  tauriListen = event.listen
}

export class ApiError extends Error {
  constructor (api, message) {
    super(`${api}: ${message}`)
    this.api = api
  }
}

/**
 * 调用 core 的某个 API。
 * @param {string} api 形如 `proxy.start` / `config.get`
 * @param {any} param
 */
export async function invoke (api, param = null) {
  if (!tauriInvoke) {
    return mockInvoke(api, param)
  }
  try {
    return await tauriInvoke('api_invoke', { api, param })
  } catch (err) {
    throw new ApiError(api, typeof err === 'string' ? err : err?.message || String(err))
  }
}

/** 订阅 core 推来的事件；返回取消函数。 */
export async function subscribe (name, handler) {
  if (!tauriListen) {
    return () => {}
  }
  return tauriListen(name, event => handler(event.payload))
}

/**
 * 占位：v0.3.0 起控制面的事件流由 Rust 侧常驻读取（`spawn_reader` 读 stdout），
 * 不再需要在启动时「订阅」一次 SSE。保留调用是为了不改动 `main.js` 的挂载顺序。
 */
export function startStatusStream () {
  if (!tauriInvoke) {
    return Promise.resolve()
  }
  return tauriInvoke('start_status_stream').catch((err) => {
    console.error('启动状态流失败：', err)
  })
}

/** 打开外部链接（避免在 WebView 里直接跳走）。 */
export async function openUrl (url) {
  if (!isTauri) {
    window.open(url, '_blank')
    return
  }
  const { openUrl: open } = await import('@tauri-apps/plugin-opener')
  await open(url)
}

// ---------------------------------------------------------------------------
// 浏览器模式下的假数据
// ---------------------------------------------------------------------------

const mockConfig = {
  server: {
    host: '127.0.0.1',
    port: 31181,
    intercepts: { 'github.com': { '.*': {} }, '*.huggingface.co': { '.*': {} }, '*linux.do': { '.*': {} } },
    whiteList: { 'localhost': true },
    dns: { providers: { 'my-doh': { server: 'https://example.com/dns-query' } } }
  },
  proxy: { enabled: true, pacEnabled: false, excludeIpList: { 'localhost': true } },
  plugin: { git: { enabled: true }, node: { enabled: true }, pip: { enabled: false } }
}

const mockStatus = {
  server: { enabled: true },
  proxy: { enabled: true, pacEnabled: false },
  plugin: { git: { enabled: true }, node: { enabled: true }, pip: { enabled: false } }
}

async function mockInvoke (api, param) {
  await new Promise(resolve => setTimeout(resolve, 80))
  switch (api) {
    case 'config.get':
      return structuredClone(mockConfig)
    case 'config.save':
      return { saved: true }
    case 'status.get':
      return structuredClone(mockStatus)
    case 'proxy.start':
      mockStatus.proxy.enabled = true
      return { ok: true }
    case 'proxy.unset':
    case 'proxy.close':
      mockStatus.proxy.enabled = false
      return { ok: true }
    case 'proxy.startPac':
      mockStatus.proxy.pacEnabled = true
      mockStatus.proxy.enabled = false
      return { ok: true }
    case 'proxy.closePac':
      mockStatus.proxy.pacEnabled = false
      return { ok: true }
    case 'control.getList':
      return [
        { id: 1, host: 'github.com', port: 443, kind: 'mitm', up: 12_345, down: 678_901 },
        { id: 2, host: 'steamcommunity.com', port: 443, kind: 'tunnel', up: 2_048, down: 51_200 }
      ]
    case 'control.close':
      return { closed: param }
    case 'pac.info':
      return { url: 'http://127.0.0.1:31181/pac.js?v=mock0000', proxyRules: 336, directRules: 110 }
    default:
      return { api, param, mock: true }
  }
}
