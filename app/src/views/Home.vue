<script setup>
import { computed, ref } from 'vue'
import SwitchCard from '../components/SwitchCard.vue'
import { useAppStore } from '../store'
import { invoke } from '../api'

const store = useAppStore()
const busy = ref({})
const pac = ref(null)
const legacy = ref(null)

const legacyAdvice = computed(() => legacy.value?.advice || [])

async function checkLegacy () {
  try {
    legacy.value = await invoke('legacy_check')
  } catch (err) {
    // 旧版本没有这个命令（比如在浏览器里跑 mock），静默忽略
    legacy.value = null
  }
}

checkLegacy()

const pluginList = computed(() => {
  const plugins = store.config?.plugin || {}
  return Object.keys(plugins).map(name => ({
    name,
    label: plugins[name]?.label || name,
    enabled: store.status?.plugin?.[name]?.enabled === true
  }))
})

async function toggle (key, value) {
  busy.value[key] = true
  try {
    await store.toggle(key, value)
  } catch (err) {
    store.pushLog(`切换 ${key} 失败：${err.message}`)
  } finally {
    busy.value[key] = false
  }
}

async function togglePlugin (name, value) {
  busy.value[name] = true
  try {
    await store.togglePlugin(name, value)
  } catch (err) {
    store.pushLog(`切换插件 ${name} 失败：${err.message}`)
  } finally {
    busy.value[name] = false
  }
}

async function loadPac () {
  try {
    pac.value = await invoke('pac.info')
  } catch (err) {
    store.pushLog(`读取 PAC 信息失败：${err.message}`)
  }
}
</script>

<template>
  <h2 class="page-title">首页</h2>

  <div v-if="legacyAdvice.length > 0" class="card" style="margin-bottom:16px; border-color:#7a5c1e">
    <div class="badge warn">旧版本检测</div>
    <ul style="margin:10px 0 0 18px; line-height:1.7">
      <li v-for="(tip, index) in legacyAdvice" :key="index">{{ tip }}</li>
    </ul>
    <p class="dim" style="margin-top:8px">
      两个版本共用 127.0.0.1:31181 与同一张根证书，同时运行会互相覆盖系统代理。
    </p>
  </div>

  <p class="page-desc">
    PAC 分流会按域名自动决定"走代理还是直连"，与固定系统代理互斥。
  </p>

  <div class="card-grid">
    <SwitchCard
      title="代理服务"
      desc="本地 MITM 端口，被 PAC 或系统代理指向时才生效"
      :enabled="store.serverEnabled"
      :busy="busy.server"
      @toggle="v => toggle('server', v)"
    />
    <SwitchCard
      title="系统代理"
      desc="把系统代理指向本地端口（固定代理，全部流量走 DS）"
      :enabled="store.proxyEnabled"
      :busy="busy.proxy"
      @toggle="v => toggle('proxy', v)"
    />
    <SwitchCard
      title="PAC 分流"
      desc="命中加速规则的站点走代理，其余直连，便于本机防火墙生效"
      :enabled="store.pacEnabled"
      :busy="busy.pac"
      @toggle="v => toggle('pac', v)"
    />
  </div>

  <h3 style="margin:24px 0 10px; font-size:14px" class="dim">插件</h3>
  <div class="card-grid">
    <SwitchCard
      v-for="plugin in pluginList"
      :key="plugin.name"
      :title="plugin.label"
      :enabled="plugin.enabled"
      :busy="busy[plugin.name]"
      :desc="`plugin.${plugin.name}`"
      @toggle="v => togglePlugin(plugin.name, v)"
    />
    <div v-if="pluginList.length === 0" class="card dim">没有可用插件</div>
  </div>

  <h3 style="margin:24px 0 10px; font-size:14px" class="dim">PAC 详情</h3>
  <div class="card">
    <button @click="loadPac">读取 PAC 信息</button>
    <div v-if="pac" style="margin-top:10px" class="mono">
      <div>地址：{{ pac.url }}</div>
      <div>代理规则：{{ pac.proxyRules }} 条 / 直连规则：{{ pac.directRules }} 条</div>
    </div>
    <p v-else class="dim" style="margin-top:8px">点击上面的按钮查询当前 PAC 端点。</p>
  </div>
</template>
