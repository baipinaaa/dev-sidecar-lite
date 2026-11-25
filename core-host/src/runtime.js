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
/**
 * 把 `@blue-frontier/<pkg>` 解析到磁盘上的真实目录。
 *
 * 为什么需要：core 与 mitmproxy 互相 require（core 要 `@blue-frontier/mitmproxy/src/json`，
 * mitmproxy 要 `@blue-frontier/dev-sidecar/src/merge`），而安装包里它们是两个并列目录，
 * 没有走 npm 的 node_modules 布局。SEA 不能建符号链接（也没权限），所以在模块解析层做映射。
 *
 * @param {{ mitmproxy?: string, core?: string }} dirs
 */
let aliasesInstalled = false
let aliasDirs = {}

function installPackageAliases (dirs = {}) {
  // 每次都覆盖映射目标：
  // resolveCore() 会按候选顺序逐个试，先试到不存在的目录时如果就此把映射固定下来，
  // 后面真正的 core-bundle 就再也解析不到了（线上踩过：报
  // `Cannot find module '@blue-frontier/mitmproxy/src/json'`）。
  aliasDirs = dirs
  if (aliasesInstalled) {
    return
  }
  const Module = require('node:module')
  const path = require('node:path')
  const originalResolve = Module._resolveFilename

  Module._resolveFilename = function (request, parent, isMain, options) {
    if (typeof request === 'string' && request.startsWith('@blue-frontier/')) {
      const rest = request.slice('@blue-frontier/'.length)
      const slash = rest.indexOf('/')
      const name = slash === -1 ? rest : rest.slice(0, slash)
      const sub = slash === -1 ? '' : rest.slice(slash)
      const dir = name === 'mitmproxy' ? aliasDirs.mitmproxy : (name === 'dev-sidecar' ? aliasDirs.core : null)
      if (dir) {
        return originalResolve.call(this, path.join(dir, sub), parent, isMain, options)
      }
    }
    return originalResolve.call(this, request, parent, isMain, options)
  }

  aliasesInstalled = true
}

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

let userDirInstalled = false

/**
 * 把 core 的用户目录从 `~/.dev-sidecar` 换成 `~/.dev-sidecar-lite`。
 *
 * core 里是硬编码字符串：
 *   `core/src/config/local-config-loader.js:10`  const dir = path.resolve(userHome, './.dev-sidecar')
 *   `core/src/modules/instance/index.js:11`     path.join(USERPROFILE, '.dev-sidecar')
 *   `core/src/shell/scripts/set-system-proxy/index.js:513`  '.dev-sidecar/proxy.env'
 * 而且`getUserBasePath()` 在模块内部被多处**直接**调用，只改 module.exports 上的引用
 * 没用（内部调用的是局部函数），必须赶在模块编译前改掉那个字面量。
 *
 * 不改 dev-sidecar 仓库的任何文件，只在运行时给 `_compile` 加一层源码替换；
 * 只作用于 `@blue-frontier/{dev-sidecar,mitmproxy}` 下的模块，且正则带
 * `(?![\-\w])` 保证幂等（已经是 `.dev-sidecar-lite` 的不会再被改一次）。
 *
 * 可用 `DS_USER_DIR_NAME` 覆盖目录名（便携版/多开测试用）。
 */
function installUserDir (dirName = process.env.DS_USER_DIR_NAME || '.dev-sidecar-lite') {
  if (userDirInstalled) {
    return dirName
  }
  const Module = require('node:module')
  const originalCompile = Module.prototype._compile
  // `path.resolve(userHome, './.dev-sidecar')`、`path.join(x, '.dev-sidecar')`、
  // `'.dev-sidecar/proxy.env'` 三种写法都要盖到，所以直接匹配那一段字面量；
  // `(?![-.\w])` 保证幂等（已经是 `.dev-sidecar-lite` 的不会再被改）。
  const pattern = /\.dev-sidecar(?![-.\w])/g
  const inBundle = /@blue-frontier[\\/](?:dev-sidecar|mitmproxy)[\\/]/

  Module.prototype._compile = function (content, filename, ...rest) {
    if (typeof content === 'string' && filename && inBundle.test(filename)) {
      content = content.replace(pattern, dirName)
    }
    return originalCompile.call(this, content, filename, ...rest)
  }

  userDirInstalled = true
  return dirName
}

module.exports = {
  installPackageAliases,
  installUserDir,
  isSea,
  requireFrom,
  installRoots,
  findInResources,
}
