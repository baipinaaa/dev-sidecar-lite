/**
 * esbuild 打平 core 的汇总入口（S3 / L2 打包）。
 *
 * 为什么需要单独一个入口：控制面宿主除了 `core` 的主 API，还要几个 core 的内部模块
 * （配置加载器、日志工厂、服务分组、脱敏工具、lodash）。打平之后这些模块不再是磁盘上
 * 的文件，只能通过这个入口一起导出。
 *
 * 由 `scripts/build-bundles.mjs` 复制到合成 workspace 根目录后再交给 esbuild，
 * 这样 `@blue-frontier/*` 才能按 workspace 的 node_modules 解析。
 *
 * 产物：`core-bundle/bundle/core.cjs`，导出 `{ api, lodash, json, localConfigLoader,
 *   loggerFactory, serviceGroup, coreConfig, securityMigrate, redactLogs }`。
 */

module.exports = {
  api: require('@blue-frontier/dev-sidecar'),
  lodash: require('lodash'),
  json: require('@blue-frontier/mitmproxy/src/json.js'),
  localConfigLoader: require('@blue-frontier/dev-sidecar/src/config/local-config-loader.js'),
  loggerFactory: require('@blue-frontier/dev-sidecar/src/utils/util.logger.js'),
  serviceGroup: require('@blue-frontier/dev-sidecar/src/service-group.js'),
  coreConfig: require('@blue-frontier/dev-sidecar/src/config/index.js'),
  securityMigrate: require('@blue-frontier/dev-sidecar/src/utils/util.security-migrate'),
  redactLogs: require('@blue-frontier/dev-sidecar/src/utils/util.redact-logs'),
}
