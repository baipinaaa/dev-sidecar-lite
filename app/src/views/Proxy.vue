<script setup>
import { computed, ref } from 'vue'
import { useAppStore } from '../store'
import { invoke } from '../api'

const store = useAppStore()
const port = ref(null)
const host = ref(null)
const message = ref('')

const excluded = computed(() => {
  const list = store.config?.proxy?.excludeIpList || {}
  return Object.keys(list).filter(key => list[key] === true)
})

const useValue = computed(() => store.config?.proxy?.use || 'local')

async function save () {
  if (!store.config) return
  store.config.proxy = store.config.proxy || {}
  if (port.value) store.config.proxy.port = Number(port.value)
  if (host.value) store.config.proxy.host = host.value
  await invoke('config.save', store.config)
  message.value = '已保存（需要重载代理生效）'
}

async function reloadProxy () {
  await invoke('server.reload').catch(err => { message.value = err.message })
  message.value = '已请求重载'
}
</script>

<template>
  <h2 class="page-title">代理</h2>
  <p class="page-desc">本地代理端口、监听地址与排除名单。</p>

  <div class="card">
    <div class="row">
      <label>监听地址</label>
      <input v-model="host" :placeholder="store.config?.server?.host || '127.0.0.1'" />
    </div>
    <div class="row">
      <label>监听端口</label>
      <input v-model="port" :placeholder="String(store.config?.server?.port || 31181)" />
    </div>
    <div class="row">
      <label>代理模式</label>
      <input :value="useValue" disabled />
    </div>
    <div class="row">
      <label></label>
      <button @click="save">保存</button>
      <button @click="reloadProxy">重载代理</button>
      <span class="dim">{{ message }}</span>
    </div>
  </div>

  <h3 style="margin:22px 0 10px; font-size:14px" class="dim">排除名单（直连，{{ excluded.length }} 条）</h3>
  <div class="card" style="max-height:340px; overflow:auto">
    <div v-for="item in excluded" :key="item" class="mono" style="padding:2px 0">{{ item }}</div>
    <div v-if="excluded.length === 0" class="dim">没有排除项</div>
  </div>
</template>
