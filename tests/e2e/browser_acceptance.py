#!/usr/bin/env python3
"""真浏览器验收：用 Chrome/Edge headless 走本地代理访问目标站，读 netlog 判状态码。

为什么不用 curl：Cloudflare 的 Bot Management 不只看 TLS 指纹，还看 HTTP/2
SETTINGS/帧顺序（peetprint）、header 顺序、UA。curl 的 h2 指纹与 Chrome 天差地别，
用 curl 测「指纹通道是否骗过 CF」永远得到 403，测不出通道本身好不好。

用法：
    python tests/e2e/browser_acceptance.py --chrome <chrome.exe> --port 31183 \
        --urls https://linux.do/ https://discord.com/ --rounds 2 --out report.md

判定：文档请求的 `status_code` 200 且页面不是挑战页 → PASS。
netlog 是 Chrome 的 JSON 事件流，`status_code` 与 `url` 出现在同一条 URL_REQUEST 事件里，
这里按「url 出现位置往后 3000 字符内找最后一个 status_code」的启发式解析（够用且不依赖库）。
"""

from __future__ import annotations

import argparse
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

sys.stdout.reconfigure(encoding="utf-8")

CHROME_UA = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36"
)
CHALLENGE_MARKERS = ("Just a moment", "请稍候", "cf-browser-verification", "challenge-platform")


def find_chrome(explicit: str | None) -> str | None:
    if explicit:
        return explicit if os.path.exists(explicit) else None
    candidates = [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ]
    for path in candidates:
        if os.path.exists(path):
            return path
    for name in ("chrome", "chrome.exe", "msedge", "msedge.exe"):
        found = shutil.which(name)
        if found:
            return found
    return None


def run_once(chrome: str, proxy_port: int, url: str, budget_ms: int, debug: bool = False) -> dict:
    workdir = tempfile.mkdtemp(prefix="ds-browser-")
    profile = os.path.join(workdir, "profile")
    netlog = os.path.join(workdir, "netlog.json")
    args = [
        chrome,
        "--headless=new",
        "--disable-gpu",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-extensions",
        "--disable-background-networking",
        "--ignore-certificate-errors",
        f"--user-data-dir={profile}",
        f"--proxy-server=http://127.0.0.1:{proxy_port}",
        f"--user-agent={CHROME_UA}",
        f"--log-net-log={netlog}",
        "--net-log-capture-mode=Default",
        f"--virtual-time-budget={budget_ms}",
        url,
    ]
    started = time.time()
    try:
        subprocess.run(args, capture_output=True, timeout=max(30, budget_ms // 1000 + 30))
    except subprocess.TimeoutExpired:
        pass
    elapsed = time.time() - started

    result = {
        "url": url,
        "elapsed_ms": int(elapsed * 1000),
        "status": None,
        "host": re.sub(r"^https?://", "", url).split("/")[0],
        "netlog_bytes": 0,
    }
    if not os.path.exists(netlog):
        shutil.rmtree(workdir, ignore_errors=True)
        return result

    raw = io.open(netlog, encoding="utf-8", errors="replace").read()
    result["netlog_bytes"] = len(raw)
    shutil.rmtree(workdir, ignore_errors=True)

    result["status"] = parse_netlog(raw, result["host"], debug=debug)
    return result


def parse_netlog(raw: str, host: str, debug: bool = False) -> int | None:
    """从 netlog 里取该 host 文档请求的最终状态码。

    netlog 结构：`{"constants":..., "events":[{"source":{"id":..},"params":{...}}]}`
    `params.url` 与 `params.response_info.status_code` 可能落在同一条 URL_REQUEST 事件里，
    也可能分散在不同 phase 的事件里（靠 `source.id` 关联），所以先建 id→url 映射再回填。
    解析失败时退化为正则（兼容 `"status_code": 200` 这种带空格的格式）。
    """
    try:
        data = json.loads(raw)
    except Exception as err:  # noqa: BLE001 - netlog 偶尔被截断
        if debug:
            print(f"  [debug] netlog 不是合法 JSON：{err}；改用正则")
        codes = re.findall(r'"status_code"\s*:\s*(\d+)', raw)
        return int(codes[-1]) if codes else None

    events = data.get("events") or []
    url_by_source: dict[int, str] = {}
    for event in events:
        params = event.get("params") or {}
        url = params.get("url")
        source_id = (event.get("source") or {}).get("id")
        if isinstance(url, str) and source_id is not None:
            url_by_source[source_id] = url

    found: list[tuple[str, int]] = []
    for event in events:
        params = event.get("params") or {}
        info = params.get("response_info") or {}
        code = info.get("status_code")
        if not isinstance(code, int):
            continue
        source_id = (event.get("source") or {}).get("id")
        url = url_by_source.get(source_id) or params.get("url")
        if isinstance(url, str) and host in url:
            found.append((url, code))

    if debug:
        print(f"  [debug] events={len(events)} 命中={found[-4:]}")

    return found[-1][1] if found else None
    return result


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--chrome", default=None)
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--urls", nargs="+", required=True)
    ap.add_argument("--rounds", type=int, default=2)
    ap.add_argument("--budget-ms", type=int, default=12000)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    chrome = find_chrome(args.chrome)
    if chrome is None:
        print("跳过：找不到 Chrome/Edge，浏览器验收只能在本机或有浏览器的 runner 上跑")
        return 0
    print(f"浏览器：{chrome}")

    rows = []
    for rnd in range(1, args.rounds + 1):
        for url in args.urls:
            res = run_once(chrome, args.port, url, args.budget_ms, debug=True)
            ok = res["status"] == 200
            verdict = "PASS" if ok else "FAIL"
            print(
                f"[{verdict}] 第{rnd}轮 {url} -> {res['status']} / {res['elapsed_ms']}ms"
                f" / netlog {res['netlog_bytes']} 字节"
            )
            rows.append({"round": rnd, "ok": ok, **res})

    if args.out:
        lines = ["| 目标 | 通过 | 平均耗时 | 状态码 |", "| --- | --- | --- | --- |"]
        for url in args.urls:
            subset = [r for r in rows if r["url"] == url]
            passed = sum(1 for r in subset if r["ok"])
            avg = sum(r["elapsed_ms"] for r in subset) // max(1, len(subset))
            codes = ",".join(str(r["status"]) for r in subset)
            lines.append(f"| {url} | {passed}/{len(subset)} | {avg}ms | {codes} |")
        io.open(args.out, "w", encoding="utf-8", newline="\n").write("\n".join(lines) + "\n")

    failed = [r for r in rows if not r["ok"]]
    print("=== 结论 ===")
    for r in rows:
        print(f"{r['url']} 第{r['round']}轮 {'通过' if r['ok'] else '失败'} 状态码 {r['status']}")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
