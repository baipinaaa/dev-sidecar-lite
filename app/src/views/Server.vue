<script setup>
import { computed } from 'vue'
import { useAppStore } from '../store'

const store = useAppStore()

const dns = computed(() => store.config?.server?.dns || {})
const providers = computed(() => Object.entries(dns.value.providers || {}).map(([name, cfg]) => ({
  name,
  server: cfg?.server || '',
  sni: cfg?.sni || '',
  forSni: Array.isArray(cfg?.forSNI) ? cfg.forSNI.length : 0
})))
const ech = computed(() => dns.value.ech || {})
const fragment = computed(() => store.config?.server?.tlsFragment || {})
const dynamic = computed(() => Object.entries(store.config?.server?.dynamicUpstreams || {}))
</script>

<template>
  <h2 class="page-title">服务</h2>
  <p class="page-desc">DNS / ECH / TLS 分片 / 动态上游（等价于 S302 的 <span class="mono">dynamic a</span>）。</p>

  <h3 style="margin:0 0 10px; font-size:14px" class="dim">DNS 提供方（{{ providers.length }}）</h3>
  <div class="card" style="padding:0; overflow:hidden">
    <table>
      <thead>
        <tr><th>名称</th><th>服务器</th><th>SNI</th><th>用于 SNI 覆写</th></tr>
      </thead>
      <tbody>
        <tr v-for="p in providers" :key="p.name">
          <td class="mono">{{ p.name }}</td>
          <td class="mono">{{ p.server }}</td>
          <td class="mono">{{ p.sni || '-' }}</td>
          <td>{{ p.forSni || '-' }}</td>
        </tr>
      </tbody>
    </table>
  </div>

  <h3 style="margin:22px 0 10px; font-size:14px" class="dim">ECH</h3>
  <div class="card">
    <div class="row"><label>启用</label><span class="badge" :class="ech.enabled ? 'ok' : 'warn'">{{ ech.enabled ? '开启' : '关闭' }}</span></div>
    <div class="row"><label>首选 DoH</label><span class="mono">{{ ech.dns || '-' }}</span></div>
    <div class="row"><label>域名数</label><span class="mono">{{ (ech.domains || []).length }}</span></div>
    <div class="row"><label>跨提供方竞速</label><span class="mono">{{ ech.tryAllProviders ? '开启（较慢）' : '关闭（推荐）' }}</span></div>
    <div class="row"><label>publicName</label><span class="mono">{{ ech.publicName || 'cloudflare-ech.com' }}</span></div>
  </div>

  <h3 style="margin:22px 0 10px; font-size:14px" class="dim">TLS 分片</h3>
  <div class="card">
    <div class="row"><label>启用</label><span class="badge" :class="fragment.enabled ? 'ok' : 'warn'">{{ fragment.enabled ? '开启' : '关闭' }}</span></div>
    <div class="row"><label>分片大小</label><span class="mono">{{ fragment.size }} 字节</span></div>
    <div class="row"><label>域名</label><span class="mono">{{ (fragment.domains || []).join(', ') || '-' }}</span></div>
  </div>

  <h3 style="margin:22px 0 10px; font-size:14px" class="dim">动态上游</h3>
  <div class="card">
    <div v-for="[pattern, ups] in dynamic" :key="pattern" class="row">
      <label class="mono">{{ pattern }}</label>
      <span class="mono">{{ (ups || []).join('  ') }}</span>
    </div>
    <div v-if="dynamic.length === 0" class="dim">没有配置动态上游</div>
  </div>
</template>
