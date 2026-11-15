<script setup>
import { computed, ref } from 'vue'
import { invoke, openUrl } from '../api'
import { useAppStore } from '../store'

const store = useAppStore()
const probing = ref(false)
const results = ref([])

const gitPlugin = computed(() => store.config?.plugin?.git || {})

// GitHub 加速相关域名（与 Node 版 intercepts 对齐）
const targets = [
  'https://github.com/',
  'https://api.github.com/',
  'https://raw.githubusercontent.com/',
  'https://codeload.github.com/'
]

async function probe () {
  probing.value = true
  results.value = []
  try {
    const response = await invoke('speed.testGithub', { urls: targets })
    results.value = Array.isArray(response) ? response : (response?.results || [])
  } catch (err) {
    results.value = targets.map(url => ({ url, error: err.message }))
  } finally {
    probing.value = false
  }
}
</script>

<template>
  <h2 class="page-title">GitHub</h2>
  <p class="page-desc">GitHub 加速相关状态。DS 全部直连真实 IP，不经过任何第三方中转。</p>

  <div class="card">
    <div class="row">
      <label>Git.exe 代理</label>
      <span class="badge" :class="gitPlugin.enabled ? 'ok' : 'warn'">{{ gitPlugin.enabled ? '已启用' : '未启用' }}</span>
    </div>
    <div class="row">
      <label>代理地址</label>
      <span class="mono dim">http://127.0.0.1:{{ store.config?.server?.port || 31181 }}</span>
    </div>
    <p class="dim" style="margin:8px 0 0">
      提示：DS 用本地 CA 重签证书，Windows 的 schannel 会因缺少吊销信息报
      <span class="mono">CRYPT_E_NO_REVOCATION_CHECK</span>。给 git 加
      <span class="mono">-c http.schannelCheckRevoke=false</span>，或改用 SSH。
    </p>
  </div>

  <h3 style="margin:22px 0 10px; font-size:14px" class="dim">连通性探测</h3>
  <div class="card">
    <div class="row">
      <button :disabled="probing" @click="probe">{{ probing ? '探测中…' : '开始探测' }}</button>
      <button @click="openUrl('https://github.com/baipinaaa/dev-sidecar-clean/releases')">打开发布页</button>
    </div>
    <table v-if="results.length" style="margin-top:10px">
      <thead><tr><th>地址</th><th>状态</th><th>耗时</th></tr></thead>
      <tbody>
        <tr v-for="item in results" :key="item.url">
          <td class="mono">{{ item.url }}</td>
          <td>
            <span v-if="item.error" class="badge err">{{ item.error }}</span>
            <span v-else class="badge ok">{{ item.status || 200 }}</span>
          </td>
          <td class="mono">{{ item.ms != null ? `${item.ms} ms` : '-' }}</td>
        </tr>
      </tbody>
    </table>
  </div>
</template>
