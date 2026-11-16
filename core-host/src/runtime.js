'use strict'

/**
 * 运行时适配层：让同一份源码既能用 `node src/main.js` 跑，也能被 esbuild 打包成单文件后
 * 用 Node SEA 跑。
 *
 * 为什么需要它：SEA 里入口脚本是"虚拟"的，`__dirname` / `__filename` 不可靠，而且
 * 入口里**普通的 `require()` 只支持内置模块**（实测报
 * `ERR_UNKNOWN_BUILTIN_MODULE: No such built-in module: ./host`）。
 * 所以：
 *   - 源码之间的静态 require 交给 esbuild 内联（它自己处理）；
 *   - 运行期按路径加载磁盘模块（core）必须走 `createRequire(process.execPath)`。
 *
 * 另一个用途是「安装目录 → 资源目录」的解析，三个地方要用同一套规则
 * （core、shim、ds-proxy），集中在这里避免各写一份又对不上。
 */

const fs = require('node:fs')
const path = require('node:path')
const nodeModule = require('node:module')

/**
 * 是否跑在 SEA 里。
 *
 * `require('node:sea').isSea()` 只有 Node 21+ 才有，这里用更通用的判断：
 * SEA 的入口是内嵌的，`process.execPath` 指向 exe，而 `__filename` 不等于 execPath。
 */
function isSea () {
  try {
    const sea = nodeModule && typeof nodeModule.isSea === 'function' ? nodeModule.isSea() : false
    if (sea) {
      return true
    }
  } catch (e) {
    // 忽略
  }
  // 退路：打包后入口被内联，__filename 会指向 exe 自身或空
  try {
    return !__filename || path.resolve(__filename) === path.resolve(process.execPath)
  } catch (e) {
    return true
  }
}

let cachedRequire = null

/** 解析磁盘模块用的 require（SEA 下必须从 exe 位置创建）。 */
function diskRequire () {
  if (cachedRequire) {
    return cachedRequire
  }
  if (isSea()) {
    cachedRequire = nodeModule.createRequire(process.execPath)
    return cachedRequire
  }
  cachedRequire = require
  return cachedRequire
}

/** 加载一个模块：`target` 可以是包名、绝对路径、或含 package.json 的目录。 */
function requireFrom (target) {
  return diskRequire()(target)
}

/**
 * 安装目录候选（Tauri 打包后 sidecar 与 `resources/` 平级）：
 *
 *     $INSTDIR/dev-sidecar-lite.exe
 *     $INSTDIR/ds-core-host.exe
 *     $INSTDIR/resources/core-bundle/...
 */
function installRoots () {
  const roots = []
  const add = (dir) => {
    if (dir && !roots.includes(dir)) {
      roots.push(dir)
    }
  }
  try {
    const exeDir = path.dirname(process.execPath)
    add(exeDir)
    add(path.dirname(exeDir))
  } catch (e) {
    // 忽略
  }
  try {
    if (typeof __dirname === 'string' && __dirname) {
      add(__dirname)
      add(path.resolve(__dirname, '..'))
      add(path.resolve(__dirname, '../..'))
    }
  } catch (e) {
    // SEA 下取 __dirname 可能抛错
  }

  // 源码开发布局：core-host/src → 仓库根
  const repoRoot = path.resolve(path.dirname(process.execPath), '..')
  add(repoRoot)
  return roots
}

/**
 * 在「安装资源目录」里找一组相对路径。
 *
 * @param {...string} segments 相对 `resources/` 的路径片段
 * @returns {string[]} 存在的绝对路径（按优先级）
 */
function findInResources (...segments) {
  const hits = []
  for (const root of installRoots()) {
    for (const sub of [
      path.join(root, 'resources', ...segments),
      path.join(root, ...segments),
    ]) {
      if (!hits.includes(sub) && fs.existsSync(sub)) {
        hits.push(sub)
      }
    }
  }
  return hits
}

module.exports = {
  isSea,
  requireFrom,
  installRoots,
  findInResources,
}
