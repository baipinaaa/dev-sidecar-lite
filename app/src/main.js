// DevSidecar Lite 前端入口。
//
// 界面整体移植自原版 Electron 版的 `packages/gui/src/view/**`（原样拷到 ./view/），
// 只替换了「跟主进程通信」那一层：electron 的 ipcRenderer/shell 换成走
// ds-core-host 的 HTTP 接口（见 ./view/api.js 与 ./api.js）。
//
// 挂载顺序与原版 `packages/gui/src/main.js` 一致，顺序很重要：
//   initApi（拿到接口列表）→ initPre（setting/config/status）→ initModules（update 等）→ mount

import { createApp } from 'vue'
import { createRouter, createWebHashHistory } from 'vue-router'
import antd from 'ant-design-vue'

import 'ant-design-vue/dist/reset.css'
import './view/style/theme/variables.scss'
import './view/style/index.scss'

import view from './view'
import App from './view/App.vue'
import DsContainer from './view/components/container.vue'
import routes from './view/router/index.js'
import { startStatusStream } from './api.js'

// 原版这几个开关由 Electron preload 通过 `window.config` 注入，
// 用途只是「临时关掉搜索框/输入事件」这类界面微调。
if (typeof window !== 'undefined') {
  window.config = Object.assign({ disableSearchBar: true, disableBeforeInputEvent: false }, window.config)
}

// Tauri 自定义协议下没有服务端路由，必须用 hash 模式
const router = createRouter({
  history: createWebHashHistory(),
  routes,
})

router.afterEach((to) => {
  const title = to.meta?.title || (to.path === '/index' ? '首页' : '')
  document.title = title ? `${title} - DevSidecar Lite` : 'DevSidecar Lite'
})

async function boot () {
  const app = createApp(App)
  app.use(antd)
  app.use(router)
  app.component('DsContainer', DsContainer)

  try {
    const api = await view.initApi(app)
    await view.initPre(app, api)
    view.initModules(app, router)
    app.mount('#app')
    // SSE 必须在界面挂载后拉起来：它负责 status/speed/traffic/config.changed 的推送
    startStatusStream()
  } catch (e) {
    console.error('view 初始化失败：', e)
    renderFatal(e)
  }
}

/** 初始化失败时不要白屏：把错误原文显示出来，否则用户只会看到「点了没反应」 */
function renderFatal (error) {
  const el = document.getElementById('app')
  if (!el) {
    return
  }
  el.innerHTML = ''
  const box = document.createElement('pre')
  box.style.cssText = 'white-space:pre-wrap;padding:24px;font:13px/1.6 Consolas,monospace;color:#c00'
  box.textContent = `前端初始化失败，无法连接本机控制面（ds-core-host）：\n\n${error && error.stack ? error.stack : error}\n\n请把这段内容连同「帮助中心 → 日志目录」下的文件一起反馈。`
  el.appendChild(box)
}

boot()
