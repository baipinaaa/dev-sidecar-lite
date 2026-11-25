'use strict'

/**
 * 运行时适配层。
 *
 * 用途只有两个：
 *   1. 「安装目录 → 资源目录」的解析（三个地方要用同一套规则：node 运行时、core、mitmproxy）；
 *   2. 把 `@blue-frontier/*` 解析到磁盘上的真实目录（安装包里它们是并列目录，不走 npm 布局）。
 *
 * 历史：v0.2.x 控制面是 Node SEA 单文件，那时还需要 `isSea()` / `createRequire(execPath)`
 * 之类的适配；v0.3.0 起控制面用普通 `node.exe` 跑磁盘上的源码，这些分支都已删除。
 */

const fs = require('node:fs')
const path = require('node:path')

/** 加载一个模块：`target` 可以是包名、绝对路径、或含 package.json 的目录。 */
function requireFrom (target) {
  return require(target)
}

/**
 * 安装目录候选（Tauri 的 `bundle.resources` 会把 core-bundle 放到安装根目录）：
 *
 *     $INSTDIR/dev-sidecar-lite.exe
 *     $INSTDIR/core-bundle/node/node.exe
 *     $INSTDIR/core-bundle/node_modules/@blue-frontier/...
 *     $INSTDIR/resources/core-bundle/...（部分 Tauri 版本会放到 resources 下）
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
  if (typeof __dirname === 'string' && __dirname) {
    add(__dirname)
    add(path.resolve(__dirname, '..'))
    add(path.resolve(__dirname, '../..'))
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
 * 没有走 npm 的 node_modules 布局，所以在模块解析层做映射。
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
let userDirHits = 0

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
  // 两种布局都要盖到：磁盘依赖树（`@blue-frontier/...`）与打平产物（`bundle/core.cjs`）。
  // 打平后 core 的全部源码在同一个文件里，一次替换就覆盖所有 `~/.dev-sidecar` 字面量。
  const inBundle = /@blue-frontier[\/](?:dev-sidecar|mitmproxy)[\/]|[\/]bundle[\/](?:core|mitmproxy)\.cjs$/i
  const jsExt = Module._extensions['.js']

  Module.prototype._compile = function (content, filename, ...rest) {
    if (typeof content === 'string' && filename && inBundle.test(filename)) {
      let hits = 0
      content = content.replace(pattern, () => {
        hits++
        return dirName
      })
      if (hits > 0) {
        userDirHits += hits
      }
      if (hits === 0 && process.env.DS_CORE_DEBUG) {
        console.log(`[core-host] 用户目录未替换（无字面量）: ${filename}`)
      }
    }
    return originalCompile.call(this, content, filename, ...rest)
  }

  // 双保险：Node 内部 CJS loader 走 `Module._extensions['.js']`，
  // 直接包装它比只 patch `_compile` 更可靠（后者在部分版本上观察不到调用）。
  Module._extensions['.js'] = function (module, filename) {
    if (filename && inBundle.test(filename)) {
      let hits = 0
      const src = require('node:fs').readFileSync(filename, 'utf8').replace(pattern, () => {
        hits++
        return dirName
      })
      userDirHits += hits
      if (process.env.DS_CORE_DEBUG) {
        console.log(`[core-host] _extensions 命中 ${hits} 处: ${filename}`)
      }
      return originalCompile.call(module, src, filename)
    }
    return jsExt.call(this, module, filename)
  }

  userDirInstalled = true
  return dirName
}

module.exports = {
  installPackageAliases,
  installUserDir,
  userDirStats: () => ({ hits: userDirHits, installed: userDirInstalled }),
  requireFrom,
  installRoots,
  findInResources,
}
