<script setup>
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { invoke, openUrl } from '../api'
import { useAppStore } from '../store'

const store = useAppStore()
const expanded = ref(false)
const timer = ref(null)

const lines = computed(() => {
  const all = store.logs
  return (expanded.value ? all : all.slice(-200)).map(item => `${item.at}  ${item.line}`).join('\n')
})

async function refreshLogs () {
  try {
    const result = await invoke('log.get')
    if (typeof result === 'string') {
      store.logs = result.split('\n').filter(Boolean).map(line => ({ at: '', line }))
    }
  } catch (err) {
    store.pushLog(`读取日志失败：${err.message}`)
  }
}

function clearLogs () {
  store.logs = []
}

onMounted(() => {
  refreshLogs()
  timer.value = setInterval(refreshLogs, 5000)
})

onUnmounted(() => clearInterval(timer.value))
</script>

<template>
  <h2 class="page-title">帮助</h2>
  <p class="page-desc">日志与常用链接。</p>

  <div class="card" style="margin-bottom:12px">
    <div class="row">
      <button @click="refreshLogs">刷新日志</button>
      <button @click="clearLogs">清空显示</button>
      <button @click="expanded = !expanded">{{ expanded ? '只看最近 200 行' : '显示全部' }}</button>
      <button @click="openUrl('https://github.com/baipinaaa/dev-sidecar-clean')">项目主页</button>
      <span class="dim">{{ store.logs.length }} 行</span>
    </div>
  </div>

  <div class="log">{{ lines || '（暂无日志）' }}</div>
</template>
