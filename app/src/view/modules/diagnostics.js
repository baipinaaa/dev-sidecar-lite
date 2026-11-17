// 原版「网页诊断」靠 Electron 主进程开一个隐藏窗口抓 HAR（`bridge/diagnostics/backend.js`），
// Tauri 版没有等价能力（也拿不到 webview 的请求体），所以这里的 API 保留形状、
// 但明确告诉调用方「本版不支持」，界面据 capabilities.supported 显示提示，
// 而不是像原版那样静默失败。

const unsupported = () => Promise.reject(new Error('本版不支持内置网页诊断，请用浏览器开发者工具或抓包工具'))

function install (app, api) {
  api.diagnostics = {
    capabilities: { supported: false },
    start: unsupported,
    stop: () => Promise.resolve(),
    clear: () => Promise.resolve(),
    getLogs: () => Promise.resolve([]),
    export: unsupported,
    trace: () => Promise.resolve(),
  }
}

export default { install }
