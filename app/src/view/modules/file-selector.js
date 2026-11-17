// 原版走 Electron 主进程的 dialog 模块（`bridge/file-selector/front.js` → ipc 'file-selector'），
// Tauri 版直接用 dialog 插件，签名保持兼容：
//   open()                      → 选文件
//   open(defaultPath)           → 选文件，带默认路径
//   open(defaultPath, 'file')   → 选文件
//   open(defaultPath, 'dir')    → 选目录
//   open({ properties: [...] }) → 等价 Electron 选项
// 取消时返回 ''（原版语义），不是 null。

const fileDialog = () => import('@tauri-apps/plugin-dialog')

function normalize (value, options) {
  if (options == null && value && typeof value !== 'string') {
    options = { ...value }
    value = null
  } else if (typeof options === 'string') {
    options = { properties: [options === 'dir' ? 'openDirectory' : 'openFile'] }
  } else {
    options = options || {}
  }

  if (!options.defaultPath && typeof value === 'string' && value) {
    options.defaultPath = value
  }
  return options
}

function install (app, api) {
  api.fileSelector = {
    async open (value = null, options = null) {
      const opts = normalize(value, options)
      const properties = opts.properties || ['openFile']
      const directory = properties.includes('openDirectory') || properties.includes('openFolder')

      const { open } = await fileDialog()
      const selected = await open({
        directory,
        multiple: Array.isArray(properties) && (properties.includes('multiSelections') || properties.includes('openFileMulti')),
        defaultPath: opts.defaultPath || undefined,
        title: opts.title || undefined,
        filters: Array.isArray(opts.filters)
          ? opts.filters.map((f) => ({ name: f.name, extensions: (f.extensions || []).map((e) => e.replace(/^\./, '')) }))
          : undefined,
      })

      if (selected == null) {
        return ''
      }
      return Array.isArray(selected) ? selected[0] : selected
    },
  }
}

export default { install }
