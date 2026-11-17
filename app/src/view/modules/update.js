// 原版 `bridge/update/front.js` 依赖 electron-updater，且会连自动更新服务器。
// 这个 fork 明确不做自动更新（用户 11-15 决议），所以这里只保留界面依赖的
// `$global.update` 状态对象与 `api.update.checkForUpdate()`，行为改成「提示手动下载」。

const state = {
  fromUser: false,
  autoDownload: false,
  progress: 0,
  checking: false,
  downloading: false,
  newVersion: false,
  isFullUpdate: true,
  disabled: true,
  errorMessage: '',
}

const RELEASE_URL = 'https://github.com/baipinaaa/dev-sidecar-lite/releases'

function install (app, api) {
  const global = app.config.globalProperties.$global || (app.config.globalProperties.$global = {})
  global.update = state

  api.update = {
    async checkForUpdate (fromUser) {
      state.fromUser = !!fromUser
      state.checking = true
      try {
        if (fromUser) {
          app.config.globalProperties.$confirm({
            title: '本版不提供自动更新',
            content: '为避免用户被动升级，DevSidecar Lite 需要手动下载新版本。现在打开下载页吗？',
            okText: '打开下载页',
            cancelText: '以后再说',
            onOk () {
              api.ipc.openExternal(RELEASE_URL)
            },
          })
        }
      } finally {
        state.checking = false
      }
      return { disabled: true, latest: null, url: RELEASE_URL }
    },
  }
}

export default { install }
