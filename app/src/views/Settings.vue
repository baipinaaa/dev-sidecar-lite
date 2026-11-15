<script setup>
import { onMounted, ref } from 'vue'
import { engineStatus, setEngine } from '../api'
import { useAppStore } from '../store'

const store = useAppStore()
const engine = ref('unknown')
const engineList = ref(['rust', 'node'])
const message = ref('')

onMounted(async () => {
  const info = await engineStatus()
  engine.value = info?.engine || 'unknown'
})

async function switchEngine (value) {
  message.value = '切换中…'
  try {
    const result = await setEngine(value)
    engine.value = result?.engine || value
    message.value = `已切换到 ${engine.value}（重启代理生效）`
  } catch (err) {
    message.value = `切换失败：${err.message}`
  }
}

async function save () {
  await store.saveConfig()
  message.value = '已保存'
}
</script>

<template>
  <h2 class="page-title">设置</h2>
  <p class="page-desc">数据面引擎、启动行为与配置保存。</p>

  <div class="card">
    <div class="row">
      <label>数据面引擎</label>
      <select :value="engine" @change="e => switchEngine(e.target.value)">
        <option v-for="item in engineList" :key="item" :value="item">{{ item }}</option>
      </select>
    </div>
    <p class="dim" style="margin:6px 0 0">
      <span class="mono">rust</span> = ds-proxy（性能与 ECH 修复）；<span class="mono">node</span> = 原 mitmproxy（回退用）。
    </p>

    <div class="row" style="margin-top:16px">
      <label>配置文件</label>
      <span class="mono dim">{{ store.config ? '已加载' : '未加载' }}</span>
    </div>
    <div class="row">
      <label>服务分组</label>
      <span class="mono dim">{{ store.serviceGroups.length }} 个</span>
    </div>
    <div class="row">
      <label>core-host</label>
      <span class="mono dim">{{ store.core.baseUrl }}</span>
    </div>
    <div class="row">
      <label></label>
      <button @click="save">保存配置</button>
      <span class="dim">{{ message }}</span>
    </div>
  </div>
</template>
