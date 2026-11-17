// 原版 `packages/gui/src/bridge/front.js` 的 Tauri 版。
//
// 原版这些「模块」都在往 api 对象上挂方法，或者监听 Electron 主进程的 IPC 通知；
// 依赖 Electron 的部分（文件选择、开机自启、网页诊断、反馈打包、自动更新）在
// Tauri 里没有对应主进程，这里逐个换成前端直接可用的实现或明确的占位。
//
// 保留 install(app, api, router) 的签名，view/index.js 不用改调用方式。

import fileSelector from './file-selector'
import autoStart from './auto-start'
import update from './update'
import diagnostics from './diagnostics'
import feedback from './feedback'
import error from './error'

const modules = {
  error,
  fileSelector,
  autoStart,
  update,
  diagnostics,
  feedback,
}

export default {
  install (app, api, router) {
    for (const name in modules) {
      try {
        modules[name].install(app, api, router)
      } catch (e) {
        // 单个模块装不上不该拖垮整个界面
        console.error(`模块 ${name} 安装失败：`, e)
      }
    }
  },
  ...modules,
}
