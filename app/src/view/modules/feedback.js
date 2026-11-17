// 原版「反馈」由主进程把日志打包成 zip 再打开 issue 页（`bridge/feedback/backend.js`）。
// Tauri 版没有日志打包通道，改成：拼一段环境信息 → 打开带 body 的 issue 页面，
// 用户自己补日志路径（帮助页里会显示日志目录）。

const ISSUE_URL = 'https://github.com/baipinaaa/dev-sidecar-lite/issues/new'

async function collectEnv (api) {
  const lines = []
  try {
    const info = await api.info.get()
    lines.push(`版本：${info.version || '未知'}`)
    lines.push(`平台：${info.platform || '未知'}`)
  } catch (e) {
    lines.push('版本：获取失败')
  }
  try {
    const config = await api.config.get()
    lines.push(`代理端口：${config.server?.port}`)
    lines.push(`数据面：${config.proxy?.engine || 'node'}`)
  } catch (e) {
    lines.push('配置：获取失败')
  }
  return lines.join('\n')
}

function install (_app, api) {
  api.feedback = {
    async report (options = {}) {
      const env = await collectEnv(api)
      const title = encodeURIComponent(options.title || '[反馈] ')
      const body = encodeURIComponent(`${options.body || ''}\n\n---\n${env}\n（请附上日志目录里的日志文件）`)
      await api.ipc.openExternal(`${ISSUE_URL}?title=${title}&body=${body}`)
      return { opened: true }
    },
  }
}

export default { install }
