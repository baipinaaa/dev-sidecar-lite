import modules from './modules/index.js'
import { apiInit, useApi, bridgeEvents } from './api'
import status from './status'

export default {
  initApi: apiInit,
  async initPre (app, api) {
    app.config.globalProperties.$api = api
    // 先接上后端推送（SSE → ipc.on 分发），再去拉配置，
    // 否则初始化期间到达的 config.changed 会被丢掉。
    await bridgeEvents()
    const setting = await api.setting.load()
    app.config.globalProperties.$global = {
      setting,
      config: await api.config.get(),
    }
    await status.install(app, api)
  },
  initModules (app, router) {
    const api = useApi()
    modules.install(app, api, router)
  },
}
