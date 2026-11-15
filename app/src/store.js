import { defineStore } from 'pinia'
import { invoke, subscribe, coreInfo, engineStatus } from './api'

export const useAppStore = defineStore('app', {
  state: () => ({
    ready: false,
    core: { ready: false, baseUrl: '(未连接)' },
    engine: { engine: 'unknown' },
    config: null,
    status: {
      server: { enabled: false },
      proxy: { enabled: false, pacEnabled: false },
      plugin: {}
    },
    logs: [],
    unsubscribers: []
  }),

  getters: {
    pacEnabled: state => state.status?.proxy?.pacEnabled === true,
    proxyEnabled: state => state.status?.proxy?.enabled === true,
    serverEnabled: state => state.status?.server?.enabled === true,
    pluginNames: state => Object.keys(state.config?.plugin || {}),
    serviceGroups: state => Object.keys(state.config?.server?.intercepts || {})
  },

  actions: {
    async bootstrap () {
      this.core = await coreInfo()
      this.engine = await engineStatus()
      await this.reload()
      await this.watch()
      this.ready = true
    },

    async reload () {
      try {
        this.config = await invoke('config.get')
        const status = await invoke('status.get')
        if (status) this.status = status
      } catch (err) {
        this.pushLog(`读取配置失败：${err.message}`)
      }
    },

    async watch () {
      this.unsubscribers.push(
        await subscribe('status', payload => {
          if (payload?.key) this.applyStatus(payload.key, payload.value)
          else if (payload && typeof payload === 'object') this.status = { ...this.status, ...payload }
        })
      )
      this.unsubscribers.push(
        await subscribe('log', payload => this.pushLog(payload?.line || String(payload)))
      )
    },

    applyStatus (key, value) {
      const parts = String(key).split('.')
      let cursor = this.status
      for (let i = 0; i < parts.length - 1; i++) {
        const part = parts[i]
        if (typeof cursor[part] !== 'object' || cursor[part] === null) cursor[part] = {}
        cursor = cursor[part]
      }
      cursor[parts[parts.length - 1]] = value
    },

    pushLog (line) {
      this.logs.push({ at: new Date().toISOString(), line })
      if (this.logs.length > 500) this.logs.splice(0, this.logs.length - 500)
    },

    /** 统一开关入口：PAC 与固定代理互斥。 */
    async toggle (key, enabled) {
      const map = {
        server: { on: 'server.start', off: 'server.stop', statusKey: 'server' },
        proxy: { on: 'proxy.start', off: 'proxy.unset', statusKey: 'proxy' },
        pac: { on: 'proxy.startPac', off: 'proxy.closePac', statusKey: 'proxy' }
      }
      const entry = map[key]
      if (!entry) throw new Error(`未知开关：${key}`)

      await invoke(enabled ? entry.on : entry.off)
      if (key === 'pac') {
        this.applyStatus('proxy.pacEnabled', enabled)
        if (enabled) this.applyStatus('proxy.enabled', false)
      } else if (key === 'proxy') {
        this.applyStatus('proxy.enabled', enabled)
        if (enabled) this.applyStatus('proxy.pacEnabled', false)
      } else {
        this.applyStatus(`${entry.statusKey}.enabled`, enabled)
      }

      if (this.config) {
        if (key === 'pac') {
          this.config.proxy.pacEnabled = enabled
          if (enabled) this.config.proxy.enabled = false
        } else if (key === 'proxy') {
          this.config.proxy.enabled = enabled
          if (enabled) this.config.proxy.pacEnabled = false
        }
      }
      await invoke('config.save', this.config).catch(() => {})
    },

    async togglePlugin (name, enabled) {
      await invoke(`plugin.${name}.${enabled ? 'start' : 'stop'}`)
      this.applyStatus(`plugin.${name}.enabled`, enabled)
      if (this.config?.plugin?.[name]) this.config.plugin[name].enabled = enabled
      await invoke('config.save', this.config).catch(() => {})
    },

    async saveConfig () {
      await invoke('config.save', this.config)
    },

    dispose () {
      this.unsubscribers.forEach(fn => fn())
      this.unsubscribers = []
    }
  }
})
