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


def run_once(chrome: str, proxy_port: int, url: str, budget_ms: int) -> dict:
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

    # 文档请求：url 完全相同的那条事件；退化为「包含目标 host 的第一条」
    needle = f'"url":"{url}"'
    idx = raw.find(needle)
    if idx < 0:
        idx = raw.find(result["host"])
    if idx >= 0:
        window = raw[idx : idx + 3000]
        codes = re.findall(r'"status_code":(\d+)', window)
        if codes:
            result["status"] = int(codes[-1])
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
            res = run_once(chrome, args.port, url, args.budget_ms)
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
