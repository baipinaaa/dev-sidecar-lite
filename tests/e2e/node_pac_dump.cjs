/**
 * Node 侧 PAC 判定导出（供 tests/e2e/consistency.py 与 Rust 版对比）。
 *
 * 用法：
 *   node tests/e2e/node_pac_dump.cjs <running.json> <host1,host2,...> [pac.js 路径]
 *
 * 输出与 `ds-pac-dump` 同构的 JSON：
 *   { proxy, hash, proxyRules, directRules, alwaysDirect, decisions }
 */

const fs = require('node:fs')
const path = require('node:path')

function fail (message) {
  console.error(`node_pac_dump: ${message}`)
  process.exit(2)
}

const [, , configPath, hostsArg, pacPathArg] = process.argv
if (!configPath) fail('用法：node node_pac_dump.cjs <running.json> <hosts> [pac.js]')

const pacPath = pacPathArg || path.resolve(
  __dirname,
  '../../dev-sidecar/packages/mitmproxy/src/lib/pac.js'
)

let pac
try {
  pac = require(pacPath)
} catch (err) {
  fail(`加载 pac.js 失败：${err.message}`)
}

const ALWAYS_DIRECT = pac.ALWAYS_DIRECT || null

let config
try {
  const raw = fs.readFileSync(configPath, 'utf8')
  config = JSON.parse(raw)
} catch (err) {
  fail(`读取配置失败：${err.message}`)
}

const generated = pac.generatePac(config)
if (!generated) fail('generatePac 返回 null（配置里没有有效 port）')

const proxyRules = pac.collectProxyRules(config).slice()
const directRules = pac.collectDirectRules(config).slice()

// 与 generatePac 一致：ALWAYS_DIRECT 在最前，随后是 directRules 去重
const directAll = []
const always = ALWAYS_DIRECT || [
  'localhost', '*.localhost', '127.*.*.*', '10.*.*.*',
  '172.16.*.*', '172.17.*.*', '172.18.*.*', '172.19.*.*',
  '172.20.*.*', '172.21.*.*', '172.22.*.*', '172.23.*.*',
  '172.24.*.*', '172.25.*.*', '172.26.*.*', '172.27.*.*',
  '172.28.*.*', '172.29.*.*', '172.30.*.*', '172.31.*.*',
  '192.168.*.*', '[::1]', '::1'
]
for (const pattern of always) {
  const regex = pac.toPacRegex(pattern)
  if (regex != null && directAll.indexOf(regex) < 0) directAll.push(regex)
}
for (const regex of directRules) {
  if (directAll.indexOf(regex) < 0) directAll.push(regex)
}

function compile (sources) {
  const out = []
  for (const src of sources) {
    try { out.push(new RegExp(src, 'i')) } catch (e) {}
  }
  return out
}

const directRes = compile(directAll)
const proxyRes = compile(proxyRules)

function decide (host) {
  if (host == null || host.length === 0) return 'DIRECT'
  if (directRes.some(re => re.test(host))) return 'DIRECT'
  if (proxyRes.some(re => re.test(host))) return 'PROXY'
  return 'DIRECT'
}

const hosts = (hostsArg || '').split(',').map(s => s.trim()).filter(Boolean)
const decisions = {}
for (const host of hosts) decisions[host] = decide(host)

process.stdout.write(JSON.stringify({
  proxy: generated.proxy,
  hash: generated.hash,
  proxyRuleCount: proxyRules.length,
  directRuleCount: directAll.length,
  proxyRules,
  directRules,
  alwaysDirect: always.map(p => pac.toPacRegex(p)).filter(x => x != null),
  decisions
}, null, 2))
