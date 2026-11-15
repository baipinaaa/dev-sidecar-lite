# 性能基准：Node 引擎 vs Rust 引擎

> 目的不是"证明 Rust 更快"，而是给出**可复现的判定方法**，以及每个指标该看什么。
> 本文的 Node 数据来自本机实测（Windows 11 / 千兆宽带 / 26.6.0），Rust 数据待 CI 或本地构建后填入。

## 一、怎么跑

两个引擎用**同一份 `running.json`**、同一批目标、同一个客户端脚本，唯一变量是数据面实现。

```powershell
# 1) Node 引擎（Electron 版，或 Lite 里把 engine 设成 node）
#    端口 31181
pwsh -File tests/e2e/bench.ps1 -Label node -ProxyUrl http://127.0.0.1:31181 -OutFile docs/BENCHMARK.md

# 2) Rust 引擎
#    ds-proxy --config running.json --port 31182
pwsh -File tests/e2e/bench.ps1 -Label rust -ProxyUrl http://127.0.0.1:31182 -OutFile docs/BENCHMARK.md
```

前置条件：

- 两次测量之间**不要重启网络**，尽量在同一个 10 分钟窗口内跑完（运营商限速会变）。
- 跑之前清掉 DNS/连接缓存影响：`ipconfig /flushdns`，并让两个引擎都冷启动一次。
- `-ProcessId` 指定被测代理进程；不指定时按 `-ProcessName dev-sidecar` 找第一个同名进程
  （Rust 版请显式传 `-ProcessName ds-proxy`）。

## 二、指标定义与判定标准

| 指标 | 怎么测 | 说明 | 合格线 |
| --- | --- | --- | --- |
| **TTFB** | `curl -w '%{time_starttransfer}'`，同 URL 跑 3 轮取平均 | 反映"取址 + 建连 + TLS + 首字节"，最能代表体感 | 冷缓存下 ≤ 1.5×直连延迟 |
| **总耗时** | `curl -w '%{time_total}'` | 含内容传输 | 同上 |
| **单连接吞吐** | Range 请求固定 8MB | 排除"大文件尺寸不一致"的干扰 | ≥ 上游带宽的 60% |
| **并发吞吐** | 8 个 Range 请求同时跑 | 只有一个核的引擎会在这里垮 | 单连接吞吐的 1.5 倍以上 |
| **进程 CPU** | 测前测后读 `Process.CPU` 差 / 墙钟 | **核心指标**：MITM 每字节都要过用户态 | 并发时 < 100%（即不占满一核） |
| **内存** | 测后 `WorkingSet64` | 缓冲策略是否把整块 body 读进来 | 稳态 < 200MB |

### 为什么 CPU 是核心
Node 引擎的实测数据（见下）显示：**8 并发时 CPU 峰值 228%**，也就是两个核心在跑满做 TLS
解密与字节搬运，而同一时刻上游只给了 6MB/s。这说明瓶颈在数据面而不是网络。

Rust（tokio + rustls）的期望是把这份开销压到一个核的 20% 以内，从而在同带宽下把并发吞吐
拉高——**但如果上游先限速，吞吐不会有肉眼可见提升**，这是第一轮测试就要看清的事实。

## 三、基线数据（Node 引擎，本机实测）

测量条件：Windows 11、千兆宽带、Node v26.6.0、DS 2.4.6/2.4.7、走 `127.0.0.1:31181`。

#### Node（Electron 版）

| 项目 | 数值 |
| --- | --- |
| TTFB https://www.baidu.com/ | ~60 ms（直连约 59 ms） |
| TTFB https://github.com/ | ~980 ms |
| TTFB https://steamcommunity.com/sharedfiles | ~430–870 ms（动态上游生效后） |
| TTFB https://api.github.com/ | ~360 ms |
| 单连接吞吐 | 6.10 MB/s 平均（峰值 25.57 MB/s，谷底 1.95 MB/s） |
| 8 并发 | 第 1 秒 48MB ≈ 48 MB/s 瞬时，随后被上游压到 ~2 MB/s |
| 8 并发 CPU | **峰值 228%**，上游停顿时回落到 2% |
| 内存 | 621–649 MB 稳态 |

关键观察：

1. **CPU 与吞吐同步**：`1s 228% / 48MB` → `2s 50% / 57MB` → `3s 2% / 57MB`，说明一旦上游给量，
   Node 的数据面能吃掉两个核；一旦上游不给量，CPU 立刻归零。**瓶颈在上游，不在 Node**。
2. 单连接曲线呈锯齿（`17.79 → 2.27 → 3.6 → 1.95 → 25.57`），是 TCP 拥塞窗口 + CDN 限速共同作用，
   换引擎也不会消失。
3. 内存 600MB+ 与 Electron 本身有关（Chromium 渲染进程占大头），数据面占比很小。

> 结论：**在这台机器 + 这条线路上，Rust 引擎的可测收益是 CPU 与内存，而不是吞吐**。吞吐瓶颈在
> CDN 侧限速，任何本地引擎都改不了。

## 四、第一轮 Rust 数据（待填）

#### Rust（ds-proxy）

| 项目 | 数值 |
| --- | --- |
| TTFB https://www.baidu.com/ | 待测 |
| TTFB https://github.com/ | 待测 |
| TTFB https://steamcommunity.com/sharedfiles | 待测 |
| TTFB https://api.github.com/ | 待测 |
| 单连接吞吐 | 待测 |
| 单连接 CPU | 待测 |
| 8 并发吞吐 | 待测 |
| 8 并发 CPU | 待测 |
| 内存峰值 | 待测 |

### 判定"值不值得换"的三条硬标准
1. **并发 CPU 是否降到 100% 以下**（即不再吃满一核）——这是这次重构的主要动机。
2. **TTFB 不许变差**（允许 ±10%，超过就说明规则匹配或取址路径比 Node 慢）。
3. **并发吞吐不低于 Node 的 1.2 倍**，如果基本持平，说明瓶颈确实在上游，如实写进报告，不粉饰。

## 五、已知会污染测量的因素

- **运营商 QoS**：同一 URL 连续测两次可能差 3 倍。
- **CDN 边缘**：`release-assets.githubusercontent.com` 落到不同 Fastly 边缘，差异可达 5 倍。
- **DS 自身的缓存**：`cacheDays` 命中的静态资源会把 TTFB 压到个位数，测 TTFB 时请用未缓存的 URL。
- **连接复用**：第二次请求走 keep-alive，TTFB 会比冷启动低一半以上，所以每轮之间 `sleep 1`。
- **schannel 吊销检查**：不传 `--ssl-no-revoke` 会直接失败（DS 的自签 CA 没有 CRL 分发点），
  脚本已统一加上，手工测时别漏。
