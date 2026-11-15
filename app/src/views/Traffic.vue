<script setup>
import { onMounted, onUnmounted, ref } from 'vue'
import { invoke } from '../api'
import { useAppStore } from '../store'

const store = useAppStore()
const connections = ref([])
const timer = ref(null)
const autoRefresh = ref(true)

function formatBytes (value) {
  const n = Number(value) || 0
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  return `${(n / 1024 / 1024).toFixed(2)} MB`
}

async function reload () {
  try {
    const list = await invoke('control.getList')
    connections.value = Array.isArray(list) ? list : (list?.list || [])
  } catch (err) {
    store.pushLog(`读取连接失败：${err.message}`)
  }
}

async function close (id) {
  await invoke('control.close', String(id)).catch(err => store.pushLog(err.message))
  await reload()
}

async function closeAll () {
  await invoke('control.closeAll').catch(err => store.pushLog(err.message))
  await reload()
}

onMounted(async () => {
  await reload()
  timer.value = setInterval(() => { if (autoRefresh.value) reload() }, 2000)
})

onUnmounted(() => clearInterval(timer.value))
</script>

<template>
  <h2 class="page-title">流量</h2>
  <p class="page-desc">当前活跃连接（来自 <span class="mono">control.getList</span>）。</p>

  <div class="card" style="margin-bottom:12px">
    <div class="row">
      <button @click="reload">刷新</button>
      <button @click="closeAll">全部断开</button>
      <label style="flex:0 0 auto; display:flex; gap:6px; align-items:center">
        <input v-model="autoRefresh" type="checkbox" style="width:auto" /> 自动刷新
      </label>
      <span class="dim">{{ connections.length }} 条</span>
    </div>
  </div>

  <div class="card" style="padding:0; overflow:hidden">
    <table>
      <thead>
        <tr><th>#</th><th>主机</th><th>端口</th><th>类型</th><th>上行</th><th>下行</th><th></th></tr>
      </thead>
      <tbody>
        <tr v-for="conn in connections" :key="conn.id">
          <td class="mono">{{ conn.id }}</td>
          <td class="mono">{{ conn.host }}</td>
          <td class="mono">{{ conn.port }}</td>
          <td><span class="badge">{{ conn.kind }}</span></td>
          <td class="mono">{{ formatBytes(conn.up ?? conn.upBytes) }}</td>
          <td class="mono">{{ formatBytes(conn.down ?? conn.downBytes) }}</td>
          <td style="text-align:right"><button @click="close(conn.id)">断开</button></td>
        </tr>
        <tr v-if="connections.length === 0">
          <td colspan="7" class="dim">暂无连接</td>
        </tr>
      </tbody>
    </table>
  </div>
</template>
