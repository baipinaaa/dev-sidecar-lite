<script>
/**
 * 网页诊断（Tauri 版）。
 *
 * 原版这里内嵌一个 Electron `<webview>` 并挂 CDP 抓 HAR。Tauri 的 webview 没有等价的
 * 抓包接口，硬塞 iframe 也拿不到请求头（还会被目标站点的 X-Frame-Options 挡掉），
 * 所以这一页改成「把地址丢给系统浏览器」，并说明用浏览器开发者工具自查。
 *
 * 保留原有路由与菜单入口，免得用户找不到原来的功能。
 */
const DEFAULT_URL = 'https://github.com/'

export default {
  name: 'WebDiagnostics',
  data () {
    return {
      url: DEFAULT_URL,
      history: [],
    }
  },
  methods: {
    normalize (value) {
      let text = String(value ?? this.url ?? '').trim()
      if (!text) {
        return null
      }
      if (!/^https?:\/\//i.test(text)) {
        text = `https://${text}`
      }
      try {
        const parsed = new URL(text)
        if (!['http:', 'https:'].includes(parsed.protocol)) {
          throw new Error('仅支持 HTTP/HTTPS')
        }
        return parsed.href
      } catch (error) {
        this.$message.error(`网址无效：${error.message}`)
        return null
      }
    },
    async open (value) {
      const target = this.normalize(value)
      if (!target) {
        return
      }
      this.url = target
      this.history = [target, ...this.history.filter((item) => item !== target)].slice(0, 10)
      // 走系统浏览器时，浏览器自己的代理设置决定它是否经过 DevSidecar
      await this.$api.ipc.openExternal(target)
      this.$message.success('已在系统浏览器中打开')
    },
    copyProxyHint () {
      const config = this.$config || (this.$global && this.$global.config)
      const port = config?.server?.port
      const text = port ? `http://127.0.0.1:${port}` : 'http://127.0.0.1:<端口>'
      if (navigator.clipboard) {
        navigator.clipboard.writeText(text)
        this.$message.success(`已复制代理地址：${text}`)
      } else {
        this.$message.info(`代理地址：${text}`)
      }
    },
  },
}
</script>

<template>
  <div class="web-diagnostics">
    <a-card title="网页诊断">
      <a-space direction="vertical" style="width: 100%">
        <a-input-search
          :value="url"
          enter-button="用系统浏览器打开"
          placeholder="输入要诊断的网址，例如 https://github.com/"
          @update:value="url = $event"
          @search="open"
        />
        <a-alert
          type="info"
          show-icon
          message="本版不内置抓包"
          description="内置网页诊断依赖 Electron 的内嵌 webview 与 CDP 抓包，Tauri 版没有等价能力。请用系统浏览器的开发者工具（F12 → Network）排查；浏览器只要走 DevSidecar 的代理，就能看到真实链路。"
        />
        <a-space>
          <a-button type="primary" @click="open(url)">
            在系统浏览器打开
          </a-button>
          <a-button @click="copyProxyHint">
            复制代理地址
          </a-button>
        </a-space>
        <a-list v-if="history.length" size="small" header="最近打开">
          <a-list-item v-for="item in history" :key="item">
            <a @click="open(item)">{{ item }}</a>
          </a-list-item>
        </a-list>
      </a-space>
    </a-card>
  </div>
</template>
