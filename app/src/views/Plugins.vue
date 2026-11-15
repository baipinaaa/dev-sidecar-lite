<script setup>
import { computed } from 'vue'
import { useAppStore } from '../store'

const store = useAppStore()

const plugins = computed(() => {
  const config = store.config?.plugin || {}
  return Object.entries(config).map(([name, value]) => ({
    name,
    label: value?.label || name,
    desc: value?.desc || `plugin.${name}`,
    enabled: store.status?.plugin?.[name]?.enabled === true ?? value?.enabled === true
  }))
})
</script>

<template>
  <h2 class="page-title">插件</h2>
  <p class="page-desc">插件状态由 core 推送（<span class="mono">status.plugin.*</span>）。</p>

  <div class="card" style="padding:0; overflow:hidden">
    <table>
      <thead>
        <tr><th>插件</th><th>说明</th><th>状态</th></tr>
      </thead>
      <tbody>
        <tr v-for="plugin in plugins" :key="plugin.name">
          <td class="mono">{{ plugin.label }}</td>
          <td class="dim">{{ plugin.desc }}</td>
          <td>
            <span class="badge" :class="plugin.enabled ? 'ok' : 'warn'">
              {{ plugin.enabled ? '运行中' : '未启动' }}
            </span>
          </td>
        </tr>
        <tr v-if="plugins.length === 0">
          <td colspan="3" class="dim">没有插件</td>
        </tr>
      </tbody>
    </table>
  </div>
</template>
