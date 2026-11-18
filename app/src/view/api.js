// 原版 `view/api.js` 的 Tauri 适配层。
//
// 原版依赖 Electron 的三样东西：
//   ipcRenderer.invoke('apiInvoke', [api, args])   —— 调后端 API
//   ipcRenderer.on(channel, (event, message) => …) —— 收后端推送
//   shell.openExternal(href)                        —— 开外链
//
// 在 Tauri 里分别对应：
//   invoke('api_invoke', {api, param}) —— lib.rs 转发到 ds-core-host 的 POST /invoke
//   listen('core-event')               —— lib.rs 把 SSE 的 {channel, payload} 转成 Tauri 事件
//   plugin-opener 的 openUrl
//
// 组件侧的调用方式一行都不用改：本文件导出的 apiObj 与原版形状完全一致。

import lodash from 'lodash'
import { invoke as coreInvoke, subscribe, openUrl } from '../api'

let inited = false
let apiObj = null

/** channel → Set<callback>，模拟 ipcRenderer 的多监听者语义 */
const listeners = new Map()

function addListener (channel, callback) {
  if (!listeners.has(channel)) {
    listeners.set(channel, new Set())
  }
  listeners.get(channel).add(callback)
}

function removeListener (channel, callback) {
  const set = listeners.get(channel)
  if (!set) {
    return
  }
  if (callback) {
    set.delete(callback)
  } else {
    set.clear()
  }
}

/** 把 core-host 的一个 SSE 事件分发给所有监听者（cb 签名与原版一致：(event, message)） */
export function dispatch (channel, payload) {
  const set = listeners.get(channel)
  if (!set || set.size === 0) {
    return
  }
  for (const callback of [...set]) {
    try {
      callback(null, payload)
    } catch (e) {
      console.error(`事件 ${channel} 的监听者抛错：`, e)
    }
  }
}

let bridged = false

/**
 * 把 core-host 的推送接进分发器。只桥一次。
 *
 * 后端把每个 SSE 事件都额外发一条 `core-event`（channel 里带点号的事件名，
 * 例如 config.changed，不满足 Tauri 事件名规则，不能直接当 Tauri 事件用）。
 */
export async function bridgeEvents () {
  if (bridged) {
    return
  }
  bridged = true
  await subscribe('core-event', (payload) => {
    if (payload && typeof payload.channel === 'string') {
      dispatch(payload.channel, payload.payload)
    }
  })
}

export function apiInit (app) {
  const invoke = (api, args) => {
    return coreInvoke(api, args == null ? null : args).catch((e) => {
      const notification = app.config.globalProperties.$notification
      if (notification) {
        notification.error({
          message: 'Api invoke error',
          description: e.message,
        })
      }
      throw e
    })
  }

  // 后端没有「发消息」通道（原版用来 file-selector / auto-start 之类），
  // 这些交互在 Tauri 侧改成了前端直接实现，这里保留签名以免调用处报错。
  const send = (channel, message) => {
    console.warn(`ipc.send(${channel}) 在 Tauri 版里没有对应的后端通道，已忽略`, message)
  }

  apiObj = {
    ipc: {
      on (channel, callback) {
        addListener(channel, callback)
      },
      removeAllListeners (channel) {
        removeListener(channel, null)
      },
      removeListener (channel, callback) {
        removeListener(channel, callback)
      },
      invoke,
      // 原版用来走 Electron 专用通道；Tauri 版收敛成普通 API 调用，
      // 找不到对应实现时返回一个可读的错误而不是崩掉。
      invokeChannel (channel, message) {
        return invoke(channel, message)
      },
      postMessage (channel, ...args) {
        send(channel, args)
      },
      send,
      async openExternal (href) {
        await openUrl(href)
      },
      openPath (file) {
        return invoke('shell.openPath', file)
      },
    },
  }

  const bindApi = (api, param1) => {
    lodash.set(apiObj, api, (param2) => {
      return invoke(api, param2 || param1)
    })
  }

  if (!inited) {
    // 托盘的「切换 xxx」由 Rust 侧 emit 过来，这里转成 window 事件交给首页接管：
    // 开关逻辑（互斥、持久化、重启数据面）都在页面里，托盘不重复实现一套。
    subscribe('tray-toggle', (payload) => {
      window.dispatchEvent(new CustomEvent('dev-sidecar:tray-toggle', { detail: payload }))
    }).catch(() => {})

    return invoke('getApiList').then((list) => {
      inited = true
      for (const item of list) {
        bindApi(item)
      }
      console.log('api inited:', apiObj)
      return apiObj
    })
  }

  return new Promise((resolve) => {
    resolve(apiObj)
  })
}

export function useApi () {
  return apiObj
}
