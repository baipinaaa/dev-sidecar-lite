<script setup>
import { computed, ref } from 'vue'
import { useAppStore } from '../store'
import { invoke } from '../api'

const store = useAppStore()
const keyword = ref('')
const message = ref('')

// 服务分组 = `server.intercepts` 的键；值为 null 表示该分组被关闭
const groups = computed(() => {
  const intercepts = store.config?.server?.intercepts || {}
  return Object.entries(intercepts)
    .map(([pattern, table]) => ({
      pattern,
      enabled: table !== null && table !== undefined,
      paths: table && typeof table === 'object' ? Object.keys(table).length : 0
    }))
    .filter(group => !keyword.value || group.pattern.includes(keyword.value))
    .sort((a, b) => a.pattern.localeCompare(b.pattern))
})

const enabledCount = computed(() => groups.value.filter(g => g.enabled).length)

async function toggleGroup (group) {
  const intercepts = store.config?.server?.intercepts
  if (!intercepts) return
  if (group.enabled) {
    intercepts[group.pattern] = null
  } else {
    // 关闭时丢了原表，这里恢复成一个最宽松的规则，具体规则等下次从远程/默认配置取
    intercepts[group.pattern] = { '.*': {} }
  }
  await invoke('config.save', store.config)
  await invoke('server.reload').catch(() => {})
  message.value = `${group.pattern} 已${group.enabled ? '关闭' : '开启'}`
}
</script>

<template>
  <h2 class="page-title">服务分组</h2>
  <p class="page-desc">
    共 {{ groups.length }} 个分组，启用 {{ enabledCount }} 个。<span class="dim">关闭的分组不会被 MITM，也不会进入 PAC。</span>
  </p>

  <div class="card" style="margin-bottom:12px">
    <div class="row">
      <label>过滤</label>
      <input v-model="keyword" placeholder="输入域名关键字，如 steam / github" />
    </div>
    <div class="row"><label></label><span class="dim">{{ message }}</span></div>
  </div>

  <div class="card" style="padding:0; overflow:hidden">
    <table>
      <thead>
        <tr><th>域名模式</th><th>路径规则</th><th>状态</th><th></th></tr>
      </thead>
      <tbody>
        <tr v-for="group in groups" :key="group.pattern">
          <td class="mono">{{ group.pattern }}</td>
          <td class="mono">{{ group.paths }}</td>
          <td>
            <span class="badge" :class="group.enabled ? 'ok' : 'warn'">{{ group.enabled ? '启用' : '关闭' }}</span>
          </td>
          <td style="text-align:right">
            <button @click="toggleGroup(group)">{{ group.enabled ? '关闭' : '开启' }}</button>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>
