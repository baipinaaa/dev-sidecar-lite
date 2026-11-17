'use strict'

/**
 * 本地接口层：把原 Electron 版 `packages/gui/src/bridge/api/backend.js` 里的 `localApi`
 * 搬到 core-host 里，去掉 electron 依赖，行为保持一致。
 *
 * 为什么要它：core 的 `api` 只提供原子能力（start/stop/config…），而原版界面调用的是
 * 「localApi 优先、core.api 兜底」的复合接口（getApiList、info.get、setting.load、
 * serviceGroups.*、startup…）。少这一层，移植过来的视图会有一半按钮点不动。
 *
 * 依赖从 core-bundle 的 node_modules 里解析（`@blue-frontier/dev-sidecar` 等）。
 */

const fs = require('node:fs')
const http = require('node:http')
const path = require('node:path')
const nodeModule = require('node:module')
const { spawn } = require('node:child_process')

/*
 * 注意：这里**不能**直接用 `require(name)`。
 *
 * SEA 里 esbuild 保留下来的 `require(...)` 走的是 node 的 embedderRequire，
 * 只认内置模块，加载磁盘上的包会抛
 * `ERR_UNKNOWN_BUILTIN_MODULE: No such built-in module: @blue-frontier/mitmproxy/src/json.js`。
 * 所有第三方/工作区包都必须经 `packageRequire(dir)`（= createRequire）加载。
 */

/**
 * 建一个以 `dir` 为基址的 require。
 *
 * 为什么需要：`lodash` 这类裸包名在 SEA 里是按 `createRequire(process.execPath)` 解析的，
 * 基址是安装根目录；而实际依赖树在 `core-bundle/node_modules`，于是
 * `Cannot find module 'lodash'`。以 core 包自己的 package.json 为基址就落在
 * `core-bundle/node_modules` 里，源码布局（pnpm 装好的 dev-sidecar）也一样成立。
 */
function packageRequire (dir) {
  if (dir) {
    try {
      const base = fs.existsSync(path.join(dir, 'package.json'))
        ? path.join(dir, 'package.json')
        : path.join(dir, 'index.js')
      return nodeModule.createRequire(base)
    } catch (e) {
      // 落到下面用默认解析
    }
  }
  return (name) => require(name)
}

/** 发一次 HEAD /pac.js，返回 http 状态码；连不上/超时返回 0 */
function headPac (port, timeoutMs = 2000) {
  return new Promise((resolve) => {
    let settled = false
    const done = (code) => {
      if (settled) {
        return
      }
      settled = true
      resolve(code)
    }
    const req = http.request(
      { host: '127.0.0.1', port, path: '/pac.js', method: 'HEAD', timeout: timeoutMs, agent: false },
      (res) => {
        res.resume()
        done(res.statusCode || 0)
      },
    )
    req.on('timeout', () => {
      req.destroy()
      done(0)
    })
    req.on('error', () => done(0))
    req.end()
  })
}

/**
 * 等数据面真正可用：轮询 `HEAD /pac.js` 直到返回 200。
 *
 * 不用「端口能连上」判断：端口可能被别的程序占着（那时 core 自己的子进程会 EADDRINUSE 死掉），
 * 端口能连上 ≠ 我们的代理在跑。mitmproxy 起进程 + 建 TLS 上下文需要几秒，所以这里给 15s。
 */
async function waitDataPlane (port, timeoutMs = 15000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if ((await headPac(port)) === 200) {
      return true
    }
    await new Promise((resolve) => setTimeout(resolve, 300))
  }
  return false
}

/**
 * 启动数据面，并确认端口真的在听。
 *
 * 为什么要复核：core 的 `server.start` 在 `fork` 之后就把句柄存进模块变量，**不检查子进程是否
 * 真的监听成功**（`modules/server/index.js:176-180`）。端口被占时子进程会 EADDRINUSE 退出，
 * 但 core 依旧认为「server 正在跑」，后续 `start` 直接 `skip start` 并返回那个假端口。
 * 表现就是「启动按钮没反应，日志里也没有错误」。这里等 `/pac.js` 真的可用，失败就把状态清掉再抛错。
 */
async function startServer (rawStart, rawClose) {
  const result = await rawStart()
  const port = result && result.port
  if (port && !(await waitDataPlane(port))) {
    try {
      await rawClose()
    } catch (e) {
      // 子进程已经死了，收不掉也无所谓
    }
    const error = new Error(`代理端口 ${port} 启动后无法访问（可能被其他程序占用）`)
    error.code = 'port_not_listening'
    throw error
  }
  return result
}

function nowText () {
  const d = new Date()
  const p = (n, w = 2) => String(n).padStart(w, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}

function openExternalPath (target) {
  const resolved = path.resolve(target)
  const [cmd, args] = process.platform === 'win32'
    ? ['rundll32.exe', ['url.dll,FileProtocolHandler', resolved]]
    : process.platform === 'darwin'
      ? ['open', [resolved]]
      : ['xdg-open', [resolved]]
  const child = spawn(cmd, args, { detached: true, stdio: 'ignore' })
  child.unref()
  return ''
}

/**
 * @param {object} options
 * @param {object} options.core             core 模块（`@blue-frontier/dev-sidecar`）
 * @param {string} options.mitmproxyPath    Node 引擎入口（mitmproxy-bridge.cjs）
 * @param {string} [options.bundleRoot]     core-bundle 根目录（读 VERSION）
 * @param {string} [options.coreDir]        core 包所在目录（解析 lodash 等裸包名的基址）
 * @param {string} [options.appVersion]     覆盖上报的版本号
 * @param {(channel: string, payload?: any) => void} [options.emit] 事件出口
 */
function createLocalApi (options) {
  const core = options.core
  const api = core.api
  const mitmproxyPath = options.mitmproxyPath
  const bundleRoot = options.bundleRoot || process.cwd()
  const emit = options.emit || (() => {})

  const pkgReq = packageRequire(options.coreDir)
  const lodash = pkgReq('lodash')
  const jsonApi = pkgReq('@blue-frontier/mitmproxy/src/json.js')
  const configLoader = pkgReq('@blue-frontier/dev-sidecar/src/config/local-config-loader.js')
  const loggerFactory = pkgReq('@blue-frontier/dev-sidecar/src/utils/util.logger.js')
  const serviceGroup = pkgReq('@blue-frontier/dev-sidecar/src/service-group.js')
  const coreConfig = pkgReq('@blue-frontier/dev-sidecar/src/config/index.js')

  const coreDefaultConfig = coreConfig.defaultConfig

  function appVersion () {
    if (options.appVersion) {
      return options.appVersion
    }
    if (process.env.DS_APP_VERSION) {
      return process.env.DS_APP_VERSION
    }
    try {
      return fs.readFileSync(path.join(bundleRoot, 'VERSION'), 'utf-8').trim()
    } catch (e) {
      return '0.0.0'
    }
  }

  function emitConfigChanged () {
    // 走 core 的事件圈：host 已经包了 `event.fire`，会把 config.changed 广播给所有 SSE 订阅者，
    // core 内部（插件、状态机）的订阅者也能收到。直接 emit 会绕过这两者。
    try {
      if (api.event && typeof api.event.fire === 'function') {
        api.event.fire('config.changed')
        return
      }
    } catch (e) {
      api.log.warn('广播 config.changed 失败:', e && e.message ? e.message : e)
    }
    emit('config.changed')
  }

  function getDefaultConfigBasePath () {
    return api.config.get().server.setting.userBasePath
  }

  function getMetaInfo (config, fallbackId) {
    const metaInfo = lodash.get(config, 'app.metaInfo') || lodash.get(config, 'metaInfo') || {}
    return {
      id: metaInfo.id || fallbackId,
      version: metaInfo.version || 0,
      updateLog: metaInfo.updateLog || '',
      showLabel: metaInfo.showLabel !== false && metaInfo.showLabel !== 'false',
    }
  }

  function getSettingsPath () {
    const dir = getDefaultConfigBasePath()
    if (!fs.existsSync(dir)) {
      fs.mkdirSync(dir, { recursive: true })
    } else {
      // 兼容 1.7.3 及以下的 setting.json5
      const newFilePath = path.join(dir, '/setting.json')
      const oldFilePath = path.join(dir, '/setting.json5')
      if (!fs.existsSync(newFilePath) && fs.existsSync(oldFilePath)) {
        return oldFilePath
      }
      return newFilePath
    }
    return path.join(dir, '/setting.json')
  }

  // 直接调 core 的原子接口（不做端口复核），端口复核放在 startServer 里
  const rawStart = () => api.server.start({ mitmproxyPath, setting: localApi.setting.load() })
  const rawClose = () => api.server.close()

  const localApi = {
    /** 原版用自己的 api 树生成可调用接口清单，供前端做「接口存在性」校验 */
    getApiList () {
      const clone = lodash.cloneDeep(api)
      lodash.merge(clone, lodash.cloneDeep(localApi))
      const list = []
      deepFindFunction(list, clone, '')
      return list
    },

    info: {
      get () {
        const runtimeConfig = api.config.get()
        const remoteConfig = lodash.get(runtimeConfig, 'app.remoteConfig') || {}

        const internal = getMetaInfo(coreDefaultConfig, '')
        const user = getMetaInfo(configLoader.getUserConfig(), '')
        const sharedRemote = getMetaInfo(configLoader.getRemoteConfig(), '')
        const personalRemote = getMetaInfo(configLoader.getRemoteConfig('_personal'), '')
        const selection = configLoader.getRemoteSelection()
        const history = configLoader.getRemoteConfigVersions('')
        const personalHistory = configLoader.getRemoteConfigVersions('_personal')
        const normalizeSelected = (list, selected) => {
          return selected && list.some((item) => item.version === selected) ? selected : ''
        }

        return {
          version: appVersion(),
          kernel: {
            sha: process.env.DS_KERNEL_SHA_SHORT || '',
            shaFull: process.env.DS_KERNEL_SHA_FULL || '',
          },
          configProfiles: {
            internal,
            user,
            sharedRemote: {
              ...sharedRemote,
              url: remoteConfig.url || '',
              enabled: remoteConfig.enabled === true && Boolean(remoteConfig.url),
            },
            personalRemote: {
              ...personalRemote,
              url: remoteConfig.personalUrl || '',
              enabled: remoteConfig.enabled === true && Boolean(remoteConfig.personalUrl),
            },
            selectedVersion: normalizeSelected(history, selection.shared),
            personalSelectedVersion: normalizeSelected(personalHistory, selection._personal),
            history,
            personalHistory,
          },
        }
      },
      getConfigDir () {
        return getDefaultConfigBasePath()
      },
      getLogDir () {
        return loggerFactory.getLogDir()
      },
      async securityMigrate () {
        const migrate = pkgReq('@blue-frontier/dev-sidecar/src/utils/util.security-migrate')
        return migrate.runMigration_to3_0_0()
      },
      async redactLogs () {
        const dir = loggerFactory.getLogDir()
        const { redactLogDir } = pkgReq('@blue-frontier/dev-sidecar/src/utils/util.redact-logs')
        return redactLogDir(dir)
      },
      getSystemPlatform (throwIfUnknown = false) {
        return api.shell.getSystemPlatform(throwIfUnknown)
      },
    },

    setting: {
      load () {
        const settingPath = getSettingsPath()
        let setting = {}
        if (fs.existsSync(settingPath)) {
          try {
            setting = jsonApi.parse(fs.readFileSync(settingPath).toString())
          } catch (e) {
            api.log.error('读取 setting.json 失败:', settingPath, ', error:', e)
          }
          if (setting == null) {
            setting = {}
          }
        }
        if (setting.installTime == null) {
          setting.installTime = nowText()
          if (setting.rootCa == null) {
            setting.rootCa = { setuped: false, desc: '根证书未安装' }
          }
          localApi.setting.save(setting)
        }
        return setting
      },
      save (setting = {}) {
        const settingPath = getSettingsPath()
        try {
          fs.writeFileSync(settingPath, jsonApi.stringify(setting))
        } catch (e) {
          api.log.error('保存 setting.json 配置文件失败:', settingPath, ', error:', e)
        }
      },
    },

    config: {
      get () {
        return api.config.get()
      },
      save (newConfig) {
        const result = api.config.save(newConfig)
        emitConfigChanged()
        return result
      },
      reload () {
        const result = api.config.reload()
        emitConfigChanged()
        return result
      },
      async downloadRemoteConfig () {
        try {
          const updated = await api.config.downloadRemoteConfig()
          return { ok: true, updated: updated === true }
        } catch (e) {
          api.log.error('下载远程配置失败（继续使用本地缓存/内置配置）:', e)
          return { ok: false, error: e && e.message ? e.message : String(e) }
        }
      },
      update (partConfig) {
        const result = api.config.update(partConfig)
        emitConfigChanged()
        return result
      },
      resetDefault (key) {
        const result = api.config.resetDefault(key)
        emitConfigChanged()
        return result
      },
      async removeUserConfig () {
        const result = await api.config.removeUserConfig()
        emitConfigChanged()
        return result
      },
    },

    serviceGroups: {
      list () {
        return serviceGroup.listCatalog(api.config.get(), { enhancedEnabled: false })
      },
      setEnabled (options) {
        const { ids, enabled } = options || {}
        const config = lodash.cloneDeep(api.config.get())
        const count = serviceGroup.applyEnabled(config, ids, enabled, { enhancedEnabled: false })
        if (count > 0) {
          api.config.save(config)
          emitConfigChanged()
        }
        return { count, catalog: localApi.serviceGroups.list() }
      },
      setPartValue (options) {
        const { ids, section, text } = options || {}
        let value
        try {
          value = jsonApi.parse(text)
        } catch (e) {
          return { ok: false, error: `配置格式错误：${e.message}` }
        }
        if (value === undefined) {
          return { ok: false, error: '配置值不能为空' }
        }
        const config = lodash.cloneDeep(api.config.get())
        const count = serviceGroup.setPartValue(config, ids, section, value, { enhancedEnabled: false })
        if (count === 0) {
          return { ok: false, error: '没有找到可修改的配置项' }
        }
        api.config.save(config)
        emitConfigChanged()
        return { ok: true, catalog: localApi.serviceGroups.list() }
      },
      resetItems (options) {
        const { ids, sections } = options || {}
        const config = lodash.cloneDeep(api.config.get())
        const count = serviceGroup.resetParts(config, ids, sections, { enhancedEnabled: false })
        if (count > 0) {
          api.config.save(config)
          emitConfigChanged()
        }
        return { count, catalog: localApi.serviceGroups.list() }
      },
      matchHostname (options) {
        const { domain } = options || {}
        const catalog = localApi.serviceGroups.list()
        return {
          domain,
          ids: serviceGroup.matchCatalog(catalog, domain).map((item) => item.id),
        }
      },
    },

    startup () {
      return api.startup({ mitmproxyPath, setting: localApi.setting.load() })
    },

    server: {
      start () {
        return startServer(rawStart, rawClose)
      },
      async restart () {
        return startServer(rawStart, rawClose)
      },
    },

    shell: {
      openPath (filePath) {
        return openExternalPath(filePath)
      },
    },
  }

  return localApi
}

function deepFindFunction (list, parent, parentKey) {
  for (const key in parent) {
    const item = parent[key]
    if (item instanceof Function) {
      list.push(parentKey + key)
    } else if (item instanceof Object) {
      deepFindFunction(list, item, `${parentKey + key}.`)
    }
  }
}

module.exports = {
  createLocalApi,
}
