'use strict'

/**
 * 控制面宿主：把 core 的 `api` 包成「一个 handleInvoke + 一条事件流」，
 * 不关心传输层（stdio JSON-RPC 见 `./rpc.js`，HTTP 调试用见 `./http.js`）。
 *
 * 原来的 `host.js` 把 HTTP/SSE 写死在里面，导致 stdio 模式要复制一遍逻辑；
 * 传输层拆出去之后，两种模式共用同一份「接口解析 + 事件回流 + 引擎信息」。
 */

const path = require('node:path')

const { ApiError, describe, invoke } = require('./dispatch')

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

/**
 * 创建宿主（不含传输层）。
 *
 * @param {object} options
 * @param {object} options.core        core 模块（含 api）
 * @param {object} [options.localApi]  localApi（优先于 core.api 查找）
 * @param {string} [options.dataDir]   数据目录
 * @param {object} [options.engineControl] 引擎信息（v0.3.0 恒为 node）
 * @param {() => any} [options.onShutdown] `shutdown` 接口的收尾
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
  const listeners = new Set()

  const broadcast = (channel, payload) => {
    if (listeners.size === 0) {
      return
    }
    const safe = payload === undefined ? null : payload
    for (const fn of listeners) {
      try {
        fn(channel, safe)
      } catch (e) {
        // 单个订阅者出错不能影响其它人
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
    return { engine: 'node', running: api.status.get().server.enabled === true }
  }

  const handleInvoke = async (apiName, param) => {
    // 本地增强接口：core 里没有，由 core-host 提供
    if (apiName === 'engine.status') {
      return engineStatus()
    }
    if (apiName === 'engine.set') {
      // 只有一种引擎，保留接口形状但不接受切换（旧前端 / 旧配置仍能调通）
      if (options.engineControl) {
        await options.engineControl.set(param && param.engine)
        return engineStatus()
      }
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

  return {
    dataDir,
    handleInvoke,
    describe: () => describe(api, localApi),
    /**
     * 订阅事件流。返回取消订阅函数。
     */
    subscribe (fn) {
      listeners.add(fn)
      return () => listeners.delete(fn)
    },
    broadcast,
    close () {
      listeners.clear()
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
  resolveDataDir,
}
