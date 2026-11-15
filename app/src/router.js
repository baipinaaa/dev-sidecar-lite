import { createRouter, createWebHashHistory } from 'vue-router'

// 与 Electron 版的 9 个页面一一对应；hash 路由避免 Tauri 自定义协议下的路径问题
const routes = [
  { path: '/', name: 'home', component: () => import('./views/Home.vue'), meta: { title: '首页' } },
  { path: '/proxy', name: 'proxy', component: () => import('./views/Proxy.vue'), meta: { title: '代理' } },
  { path: '/server', name: 'server', component: () => import('./views/Server.vue'), meta: { title: '服务' } },
  { path: '/groups', name: 'groups', component: () => import('./views/Groups.vue'), meta: { title: '服务分组' } },
  { path: '/settings', name: 'settings', component: () => import('./views/Settings.vue'), meta: { title: '设置' } },
  { path: '/traffic', name: 'traffic', component: () => import('./views/Traffic.vue'), meta: { title: '流量' } },
  { path: '/plugins', name: 'plugins', component: () => import('./views/Plugins.vue'), meta: { title: '插件' } },
  { path: '/help', name: 'help', component: () => import('./views/Help.vue'), meta: { title: '帮助' } },
  { path: '/github', name: 'github', component: () => import('./views/Github.vue'), meta: { title: 'GitHub' } }
]

const router = createRouter({
  history: createWebHashHistory(),
  routes
})

router.afterEach(to => {
  document.title = to.meta?.title ? `${to.meta.title} - DevSidecar Lite` : 'DevSidecar Lite'
})

export default router
