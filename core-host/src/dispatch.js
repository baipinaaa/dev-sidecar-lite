'use strict'

/**
 * 把 core 的 `api` 对象按点号路径解析成可调用函数。
 *
 * 与 Electron 版 `packages/gui/src/bridge/api/backend.js` 的 `invoke` 语义保持一致：
 * - 路径不存在 → 抛 `未知接口`
 * - 目标不是函数 → 抛 `接口不可调用`
 * - 函数抛错 → 原样透出 message
 */

function getByPath (target, dotted) {
  const parts = String(dotted).split('.')
  let current = target
  for (const part of parts) {
    if (current == null) {
      return undefined
    }
    current = current[part]
  }
  return current
}

function describe (api) {
  const result = {}
  const walk = (node, prefix, depth) => {
    if (node == null || depth > 3) {
      return
    }
    for (const key of Object.keys(node)) {
      const value = node[key]
      const path = prefix ? `${prefix}.${key}` : key
      if (typeof value === 'function') {
        result[path] = true
      } else if (value != null && typeof value === 'object' && !Array.isArray(value)) {
        walk(value, path, depth + 1)
      }
    }
  }
  walk(api, '', 0)
  return result
}

class ApiError extends Error {
  constructor (message, code) {
    super(message)
    this.name = 'ApiError'
    this.code = code || 'api_error'
  }
}

/**
 * 调用 core 的接口。
 *
 * @param {object} root  core.api
 * @param {string} api   形如 `proxy.startPac`
 * @param {*} param      原样作为第一个参数传入
 */
async function invoke (root, api, param) {
  if (typeof api !== 'string' || api.trim() === '') {
    throw new ApiError('api 不能为空', 'invalid_api')
  }

  const target = getByPath(root, api)
  if (target === undefined) {
    throw new ApiError(`未知接口: ${api}`, 'unknown_api')
  }
  if (typeof target !== 'function') {
    throw new ApiError(`接口不可调用: ${api}`, 'not_callable')
  }

  try {
    return await target(param)
  } catch (err) {
    const message = err && err.message ? err.message : String(err)
    const wrapped = new ApiError(message, (err && err.code) || 'api_error')
    wrapped.stack = err && err.stack
    throw wrapped
  }
}

module.exports = {
  ApiError,
  describe,
  getByPath,
  invoke,
}
