// 原版走 Electron 主进程的 auto-start 模块（内部用 electron 的 app.setLoginItemSettings），
// Tauri 版用 autostart 插件。签名保持 `api.autoStart.enabled(true|false)`。

const autostart = () => import('@tauri-apps/plugin-autostart')

function install (app, api) {
  api.autoStart = {
    async enabled (value) {
      try {
        const mod = await autostart()
        if (value) {
          await mod.enable()
        } else {
          await mod.disable()
        }
        app.config.globalProperties.$message.info(value ? '已添加开机自启' : '已取消开机自启')
        return true
      } catch (e) {
        console.error('设置开机自启失败：', e)
        app.config.globalProperties.$message.error(`设置开机自启失败：${e.message || e}`)
        return false
      }
    },

    /** 当前是否已开机自启（原版没有这个接口，界面初始化时对一下状态用） */
    async isEnabled () {
      try {
        const mod = await autostart()
        return await mod.isEnabled()
      } catch (e) {
        return false
      }
    },
  }
}

export default { install }
