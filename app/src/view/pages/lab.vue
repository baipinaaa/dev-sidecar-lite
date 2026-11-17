<script>
/**
 * 「实验功能」（Lab）。
 *
 * 新增页面的原因：路线 4 把默认数据面换成了 Node（与原版一致），Rust 数据面降级成
 * 实验通道。实验通道必须能被用户显式打开/关闭，且要能看见它到底有没有在跑，
 * 否则出了问题只能靠命令行猜。放在这里而不是设置页，是为了让「这不是正式功能」一目了然。
 */
import { engineStatus, setEngine, coreInfo } from '../../api.js'

const ENGINE_LABEL = {
  node: 'Node（默认，与原版一致）',
  rust: 'Rust（实验通道）',
  stopped: '未启动',
  unknown: '未知',
}

export default {
  name: 'Lab',
  data () {
    return {
      loading: true,
      switching: false,
      engine: 'unknown',
      detail: '',
      core: { ready: false, baseUrl: '', version: null, error: null },
    }
  },
  computed: {
    engineLabel () {
      return ENGINE_LABEL[this.engine] || this.engine
    },
    isRust () {
      return this.engine === 'rust'
    },
  },
  async mounted () {
    await this.refresh()
  },
  methods: {
    async refresh () {
      this.loading = true
      try {
        const [status, core] = await Promise.all([engineStatus(), coreInfo()])
        this.engine = status?.engine || 'unknown'
        this.detail = status?.detail || ''
        this.core = core || this.core
      } catch (e) {
        this.$message.error(`读取引擎状态失败：${e.message || e}`)
      } finally {
        this.loading = false
      }
    },
    async switchTo (engine) {
      if (engine === this.engine) {
        return
      }
      if (engine === 'rust') {
        this.$confirm({
          title: '切换到 Rust 数据面？',
          content: 'Rust 数据面目前是实验通道：只覆盖 CONNECT 隧道与基础 MITM，不含规则改写，'
            + '遇到问题请切回 Node。切换后需要重启代理服务才完全生效。',
          okText: '仍然切换',
          cancelText: '取消',
          onOk: () => this.doSwitch(engine),
        })
        return
      }
      await this.doSwitch(engine)
    },
    async doSwitch (engine) {
      this.switching = true
      try {
        const status = await setEngine(engine)
        this.engine = status?.engine || engine
        this.detail = status?.detail || ''
        this.$message.success(`已切换到 ${ENGINE_LABEL[engine] || engine}，重启代理服务后完全生效`)
      } catch (e) {
        this.$message.error(`切换失败：${e.message || e}`)
      } finally {
        this.switching = false
      }
    },
  },
}
</script>

<template>
  <div class="lab">
    <a-card title="实验功能">
      <a-space direction="vertical" style="width: 100%">
        <a-alert
          type="warning"
          show-icon
          message="这一页的东西默认关闭，出问题时先切回默认值"
        />

        <a-descriptions bordered size="small" :column="1">
          <a-descriptions-item label="当前数据面">
            <a-tag :color="isRust ? 'orange' : 'green'">{{ engineLabel }}</a-tag>
            <span v-if="detail" class="detail">{{ detail }}</span>
          </a-descriptions-item>
          <a-descriptions-item label="控制面">
            <a-tag :color="core.ready ? 'green' : 'red'">{{ core.ready ? '已就绪' : '未就绪' }}</a-tag>
            <span class="detail">{{ core.baseUrl || '（未知地址）' }}</span>
          </a-descriptions-item>
          <a-descriptions-item v-if="core.error" label="错误">
            <span class="detail">{{ core.error }}</span>
          </a-descriptions-item>
        </a-descriptions>

        <a-space>
          <a-button :loading="loading" @click="refresh">
            刷新状态
          </a-button>
          <a-button :disabled="switching || !isRust" @click="switchTo('node')">
            切回 Node 数据面
          </a-button>
          <a-button type="primary" danger :disabled="switching || isRust" @click="switchTo('rust')">
            试用 Rust 数据面
          </a-button>
        </a-space>

        <a-divider>Rust 数据面已知限制</a-divider>
        <ul class="limits">
          <li>不做规则改写（不替换请求/响应体，只做隧道与解密转发）</li>
          <li>不输出速度/流量统计，界面上的速度表会是空的</li>
          <li>上游 HTTP/2 未实现，h2 站点走隧道会更稳</li>
          <li>没有连接数上限与超时保护，长时间大流量不建议用</li>
        </ul>
      </a-space>
    </a-card>
  </div>
</template>

<style scoped>
.detail {
  margin-left: 8px;
  color: var(--text-secondary);
}
.limits {
  margin: 0;
  padding-left: 20px;
  line-height: 1.9;
}
</style>
