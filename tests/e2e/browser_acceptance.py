#!/usr/bin/env python3
"""真浏览器验收：Chrome headless + CDP，读导航响应的状态码与页面正文。

为什么不用 curl：Cloudflare 的 Bot Management 不只看 TLS 指纹，还看 HTTP/2
SETTINGS/帧顺序（peetprint）、header 顺序、UA。curl 的 h2 指纹与 Chrome 天差地别，
用 curl 测「指纹通道是否骗过 CF」永远得到 403，测不出通道本身好不好。

为什么不用 `--dump-dom`：Chrome 132 起该参数被移除；netlog 里的 `response_info`
在 `--net-log-capture-mode=Default` 下也不落盘（实测 events 3k+ 但零命中）。
所以走 DevTools 协议：导航后读 `performance.getEntriesByType('navigation')[0].responseStatus`
和 `document.body.innerText`，一次拿到「状态码 + 是不是挑战页」。

WS 客户端是手写的（RFC6455 客户端帧，掩码 + 分片重组），不引第三方库。

用法：
    python tests/e2e/browser_acceptance.py --port 31183 \
        --urls https://linux.do/ https://discord.com/ --rounds 2 --out report.md
"""

from __future__ import annotations

import argparse
import base64
import io
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlparse
from urllib.request import Request, urlopen

sys.stdout.reconfigure(encoding="utf-8")

CHROME_UA = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36"
)
CHALLENGE_MARKERS = ("Just a moment", "请稍候", "cf-browser-verification", "challenge-platform", "Attention Required")


# --------------------------------------------------------------------------- WS
class Ws:
    """最小 WebSocket 客户端：只处理文本帧 + ping/pong + 分片。"""

    def __init__(self, url: str, timeout: float = 15.0):
        parsed = urlparse(url)
        self.sock = socket.create_connection((parsed.hostname, parsed.port), timeout=timeout)
        key = base64.b64encode(os.urandom(16)).decode()
        request = (
            f"GET {parsed.path} HTTP/1.1\r\n"
            f"Host: {parsed.hostname}:{parsed.port}\r\n"
            "Upgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        self.sock.sendall(request.encode())
        buffer = b""
        while b"\r\n\r\n" not in buffer:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise RuntimeError("WebSocket 握手被关闭")
            buffer += chunk
        head, self.pending = buffer.split(b"\r\n\r\n", 1)
        if b"101" not in head.split(b"\r\n")[0]:
            raise RuntimeError(f"WebSocket 握手失败: {head[:120]!r}")

    def _read(self, n: int) -> bytes:
        while len(self.pending) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise RuntimeError("WebSocket 连接被关闭")
            self.pending += chunk
        out, self.pending = self.pending[:n], self.pending[n:]
        return out

    def send(self, text: str) -> None:
        payload = text.encode()
        mask = os.urandom(4)
        header = bytearray([0x81])
        size = len(payload)
        if size < 126:
            header.append(0x80 | size)
        elif size < 65536:
            header.append(0x80 | 126)
            header += struct.pack(">H", size)
        else:
            header.append(0x80 | 127)
            header += struct.pack(">Q", size)
        header += mask
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        self.sock.sendall(bytes(header) + masked)

    def recv(self) -> str:
        fragments: list[bytes] = []
        while True:
            first, second = self._read(2)
            fin = first & 0x80
            opcode = first & 0x0F
            size = second & 0x7F
            if size == 126:
                size = struct.unpack(">H", self._read(2))[0]
            elif size == 127:
                size = struct.unpack(">Q", self._read(8))[0]
            payload = self._read(size) if size else b""
            if opcode == 0x9:  # ping
                self.sock.sendall(b"\x8a\x80" + os.urandom(4))
                continue
            if opcode == 0x8:  # close
                raise RuntimeError("WebSocket 被对端关闭")
            fragments.append(payload)
            if fin:
                return b"".join(fragments).decode("utf-8", errors="replace")

    def call(self, msg_id: int, method: str, params: dict | None = None, timeout: float = 20.0) -> dict:
        self.sock.settimeout(timeout)
        body = {"id": msg_id, "method": method}
        if params:
            body["params"] = params
        self.send(json.dumps(body))
        deadline = time.time() + timeout
        while time.time() < deadline:
            message = json.loads(self.recv())
            if message.get("id") == msg_id:
                return message
        raise TimeoutError(f"CDP {method} 超时")

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass


# ------------------------------------------------------------------------ chrome
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


def launch(chrome: str, proxy_port: int | None) -> tuple[subprocess.Popen, str]:
    workdir = tempfile.mkdtemp(prefix="ds-browser-")
    profile = os.path.join(workdir, "profile")
    os.makedirs(profile, exist_ok=True)
    process = subprocess.Popen(
        [
            chrome,
            "--headless=new",
            "--disable-gpu",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-background-networking",
            "--disable-features=Translate",
            "--ignore-certificate-errors",
            "--remote-debugging-port=0",
            f"--user-data-dir={profile}",
            (f"--proxy-server=http://127.0.0.1:{proxy_port}" if proxy_port else "--no-proxy-server"),
            f"--user-agent={CHROME_UA}",
            "about:blank",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    port_file = os.path.join(profile, "DevToolsActivePort")
    for _ in range(120):
        if os.path.exists(port_file):
            content = io.open(port_file, encoding="utf-8").read().splitlines()
            if content and content[0].strip().isdigit():
                return process, content[0].strip()
        time.sleep(0.25)
    process.kill()
    raise RuntimeError("Chrome 没写出 DevToolsActivePort（远程调试没起来）")


def open_tab(port: str) -> str:
    # `/json/new` 只接受 PUT，用 GET 会 405（ctf 阶段踩过）。
    request = Request(f"http://127.0.0.1:{port}/json/new?about:blank", method="PUT")
    reply = urlopen(request, timeout=10).read()
    return json.loads(reply)["webSocketDebuggerUrl"]


def probe_url(devtools_port: str, url: str, budget_s: float = 20.0) -> dict:
    ws = Ws(open_tab(devtools_port))
    result = {"url": url, "status": None, "title": "", "text": "", "challenge": False}
    try:
        ws.call(1, "Page.enable")
        ws.call(2, "Runtime.enable")
        ws.call(3, "Page.navigate", {"url": url})
        deadline = time.time() + budget_s
        expression = (
            "JSON.stringify({"
            "s:(performance.getEntriesByType('navigation')[0]||{}).responseStatus,"
            "t:document.title,"
            "b:document.body?document.body.innerText.slice(0,300):''})"
        )
        while time.time() < deadline:
            time.sleep(0.6)
            reply = ws.call(10, "Runtime.evaluate", {"expression": expression, "returnByValue": True})
            raw = ((reply.get("result") or {}).get("result") or {}).get("value")
            if not raw:
                continue
            data = json.loads(raw)
            result["status"] = data.get("s")
            result["title"] = (data.get("t") or "").strip()[:80]
            result["text"] = (data.get("b") or "").strip()[:300]
            if result["status"]:
                break
        joined = f"{result['title']} {result['text']}"
        result["challenge"] = any(marker in joined for marker in CHALLENGE_MARKERS)
    finally:
        ws.close()
    return result


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--chrome", default=None)
    ap.add_argument("--port", type=int, required=True, help="ds-proxy 的监听端口")
    ap.add_argument("--urls", nargs="+", required=True)
    ap.add_argument("--rounds", type=int, default=2)
    ap.add_argument("--budget", type=float, default=20.0)
    ap.add_argument(
        "--direct-control",
        action="store_true",
        help="额外来一轮「不挂代理」的对照，用来区分「指纹没骗过 CF」和「线路 IP 被限流」",
    )
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    chrome = find_chrome(args.chrome)
    if chrome is None:
        print("跳过：找不到 Chrome/Edge，浏览器验收只能在有浏览器的机器上跑")
        return 0
    print(f"浏览器：{chrome}")

    rows = []
    for rnd in range(1, args.rounds + 1):
        process = None
        try:
            process, devtools_port = launch(chrome, args.port)
            for url in args.urls:
                res = probe_url(devtools_port, url, args.budget)
                ok = res["status"] == 200 and not res["challenge"]
                extra = "挑战页" if res["challenge"] else "正常"
                print(
                    f"[{'PASS' if ok else 'FAIL'}] 第{rnd}轮 {url} -> "
                    f"{res['status']} / {extra} / 标题 {res['title']!r}"
                )
                rows.append({"round": rnd, "ok": ok, **res})
        finally:
            if process is not None:
                process.kill()

    control_rows: list[dict] = []
    if args.direct_control:
        print("--- 对照：同一浏览器、不挂代理 ---")
        process = None
        try:
            process, devtools_port = launch(chrome, None)
            for url in args.urls:
                res = probe_url(devtools_port, url, args.budget)
                ok = res["status"] == 200 and not res["challenge"]
                print(f"[{'PASS' if ok else 'FAIL'}] 直连对照 {url} -> {res['status']} / 标题 {res['title']!r}")
                control_rows.append({"round": 0, "ok": ok, **res})
        finally:
            if process is not None:
                process.kill()

    if args.out:
        lines = ["| 目标 | 通过 | 状态码 | 挑战页 |", "| --- | --- | --- | --- |"]
        for url in args.urls:
            subset = [r for r in rows if r["url"] == url]
            passed = sum(1 for r in subset if r["ok"])
            codes = ",".join(str(r["status"]) for r in subset)
            challenged = sum(1 for r in subset if r["challenge"])
            lines.append(f"| {url} | {passed}/{len(subset)} | {codes} | {challenged} |")
        if control_rows:
            lines.append("")
            lines.append("对照（同一浏览器、不挂代理）：")
            lines.append("")
            lines.append("| 目标 | 状态码 | 挑战页 |")
            lines.append("| --- | --- | --- |")
            for url in args.urls:
                subset = [r for r in control_rows if r["url"] == url]
                codes = ",".join(str(r["status"]) for r in subset)
                challenged = sum(1 for r in subset if r["challenge"])
                lines.append(f"| {url} | {codes} | {challenged} |")
        io.open(args.out, "w", encoding="utf-8", newline="\n").write("\n".join(lines) + "\n")

    failed = [r for r in rows if not r["ok"]]
    print("=== 结论 ===")
    for row in rows:
        print(f"{row['url']} 第{row['round']}轮 {'通过' if row['ok'] else '失败'} 状态码 {row['status']} 挑战 {row['challenge']}")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
