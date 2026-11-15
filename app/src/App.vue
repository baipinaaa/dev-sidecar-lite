<script setup>
import { onMounted, onUnmounted } from 'vue'
import { useRoute } from 'vue-router'
import { useAppStore } from './store'

const store = useAppStore()
const route = useRoute()

const navs = [
  { to: '/', label: '首页' },
  { to: '/proxy', label: '代理' },
  { to: '/server', label: '服务' },
  { to: '/groups', label: '服务分组' },
  { to: '/settings', label: '设置' },
  { to: '/traffic', label: '流量' },
  { to: '/plugins', label: '插件' },
  { to: '/github', label: 'GitHub' },
  { to: '/help', label: '帮助' }
]

onMounted(() => store.bootstrap())
onUnmounted(() => store.dispose())
</script>

<template>
  <div class="layout">
    <aside class="sidebar">
      <div class="brand">
        DevSidecar Lite
        <small>Rust 数据面 + Tauri 壳</small>
      </div>
      <router-link
        v-for="nav in navs"
        :key="nav.to"
        :to="nav.to"
        class="nav-item"
        :class="{ active: route.path === nav.to }"
      >
        {{ nav.label }}
      </router-link>
      <div style="flex:1"></div>
      <div class="dim" style="font-size:12px; padding:6px 10px">
        引擎：<span class="mono">{{ store.engine.engine }}</span><br />
        core：<span class="mono">{{ store.core.ready ? '已连接' : '未连接' }}</span>
      </div>
    </aside>

    <main class="content">
      <router-view />
    </main>
  </div>
</template>
