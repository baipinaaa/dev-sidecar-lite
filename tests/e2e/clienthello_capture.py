#!/usr/bin/env python
"""捕获任意 TLS 客户端的 ClientHello 并计算 JA3 / JA4 —— 不依赖抓包工具。

为什么需要它：`--dump-dom` 在 Chrome 132+ 的新 headless 里已经被移除，
靠 `tls.peet.ws` 这类回显服务又会撞 CF 的机器人挑战（headless UA 必被拦）。
于是换成最确定的做法：自己当一次 HTTP 代理，只做一件事——把 CONNECT 之后
收到的第一段 TLS 字节原样存下来，再用现成的解析器算指纹。

用法:
    # 1) 起一个嗅探代理（阻塞到抓到一条 ClientHello 或超时）
    python tests/e2e/clienthello_capture.py --listen 18999 --out hello.bin

    # 2) 让被测客户端连它
    chrome.exe --headless=new --proxy-server=http://127.0.0.1:18999 https://example.com/
    curl.exe -x http://127.0.0.1:18999 https://example.com/

    # 3) 算指纹
    python tests/e2e/clienthello_capture.py --compute hello.bin
"""

from __future__ import annotations

import argparse
import json
import socket
import sys
import time

# Windows 下被 Start-Process 重定向到文件时，stdout 默认是 cp1252，
# 打印中文会直接 UnicodeEncodeError 崩掉（CI 上就踩了这个）。
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass

sys.path.insert(0, __file__.rsplit("/", 1)[0].rsplit("\\", 1)[0])

from ja3_from_pcap import CLIENT_HELLO, TLS_HANDSHAKE, find_client_hello, ja3, ja4_prefix, parse_client_hello  # noqa: E402

MAX_RECORD = 65536


def read_head(conn: socket.socket, timeout: float) -> tuple[bytes, bytes]:
    """读到 `\\r\\n\\r\\n`，返回 (头部, 余下的字节)。"""
    conn.settimeout(timeout)
    buf = b""
    while b"\r\n\r\n" not in buf and len(buf) < 16384:
        chunk = conn.recv(4096)
        if not chunk:
            break
        buf += chunk
    head, _, rest = buf.partition(b"\r\n\r\n")
    return head, rest


def read_client_hello(conn: socket.socket, initial: bytes, timeout: float) -> bytes:
    """凑够一条完整的 TLS record（0x16 + 版本 + 长度）。"""
    conn.settimeout(timeout)
    buf = initial
    while True:
        if len(buf) >= 5 and buf[0] == TLS_HANDSHAKE:
            record_len = int.from_bytes(buf[3:5], "big")
            if len(buf) >= 5 + record_len:
                return buf[: 5 + record_len]
        if len(buf) > MAX_RECORD:
            return buf
        try:
            chunk = conn.recv(8192)
        except (socket.timeout, TimeoutError):
            return buf
        if not chunk:
            return buf
        buf += chunk


def serve(listen_port: int, out_path: str, timeout: float, respond_200: bool) -> int:
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", listen_port))
    srv.listen(8)
    srv.settimeout(timeout)
    print(f"嗅探代理监听 127.0.0.1:{listen_port}，等待 ClientHello…", flush=True)

    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            conn, addr = srv.accept()
        except (socket.timeout, TimeoutError):
            break
        with conn:
            head, rest = read_head(conn, max(1.0, deadline - time.time()))
            first = head.split(b"\r\n", 1)[0].decode("latin1", "replace") if head else ""
            if not first.upper().startswith("CONNECT") and rest[:1] != b"\x16":
                # 普通 HTTP 请求，不是 CONNECT，跳过继续等
                continue
            if respond_200 and first.upper().startswith("CONNECT"):
                conn.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            blob = read_client_hello(conn, rest, 5.0)
            if blob[:1] != b"\x16":
                continue
            with open(out_path, "wb") as fh:
                fh.write(blob)
            print(f"已捕获 {len(blob)} 字节 ClientHello → {out_path}", flush=True)
            return 0
    print("超时：没有捕获到 ClientHello", flush=True)
    return 1


def compute(path: str) -> int:
    with open(path, "rb") as fh:
        blob = fh.read()
    body = find_client_hello([blob])
    if body is None:
        print(json.dumps({"error": "没有找到 ClientHello"}, ensure_ascii=False))
        return 1
    info = parse_client_hello(body)
    raw, digest = ja3(info)
    result = {
        "ja3": raw,
        "ja3_hash": digest,
        "ja4": ja4_prefix(info),
        "sni": info.get("sni"),
        "alpn": [
            a if isinstance(a, str) else a.decode("latin1", "replace")
            for a in (info.get("alpn") or [])
        ],
        "version": info.get("version"),
        "ciphers": info.get("ciphers"),
        "extensions": info.get("extensions"),
        "groups": info.get("groups"),
        "point_formats": info.get("point_formats"),
        "bytes": len(blob),
    }
    print(json.dumps(result, ensure_ascii=False))
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen", type=int, default=0)
    parser.add_argument("--out", default="clienthello.bin")
    parser.add_argument("--compute", default="")
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument("--no-respond", action="store_true", help="不回 200，直接读")
    args = parser.parse_args(argv)

    if args.compute:
        return compute(args.compute)
    if args.listen:
        return serve(args.listen, args.out, args.timeout, not args.no_respond)
    parser.print_help()
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
