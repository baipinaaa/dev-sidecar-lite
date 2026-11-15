#!/usr/bin/env python
"""Rust 与 Node 的 PAC 判定一致性验收（步骤 16）。

同一份 `running.json` 下，两个实现必须对同一批域名给出相同结论：
  - 代理规则集合一致
  - 直连规则集合一致
  - 20 个样本域名的 PROXY/DIRECT 判定逐条一致

用法：
  python tests/e2e/consistency.py --config C:/Users/me/.dev-sidecar/running.json
  python tests/e2e/consistency.py --config running.json --rust target/release/ds-pac-dump.exe

缺任何一侧（没编译 Rust / 没有 Node）都会打印"跳过"并返回 0，
这样它既能进 CI（两侧都齐时真跑），也不会卡住只装了 Node 的开发机。
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import subprocess
import sys

SAMPLE_HOSTS = [
    "steamcommunity.com",
    "www.steamcommunity.com",
    "store.steampowered.com",
    "api.steampowered.com",
    "github.com",
    "api.github.com",
    "raw.githubusercontent.com",
    "objects-origin.githubusercontent.com",
    "codeload.github.com",
    "linux.do",
    "cdn3.ldstatic.com",
    "discord.com",
    "huggingface.co",
    "cdn-lfs.hf.co",
    "www.baidu.com",
    "www.qq.com",
    "rj.lovestu.com",
    "localhost",
    "192.168.3.1",
    "p01.icloud.com",
]


def rust_binary(explicit: str | None) -> str | None:
    if explicit:
        return explicit if os.path.exists(explicit) else None
    name = "ds-pac-dump.exe" if platform.system() == "Windows" else "ds-pac-dump"
    for candidate in (
        os.path.join("target", "release", name),
        os.path.join("target", "debug", name),
    ):
        if os.path.exists(candidate):
            return candidate
    return None


def run_json(cmd: list[str]) -> dict:
    result = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8")
    if result.returncode != 0:
        raise RuntimeError(
            f"命令失败（{result.returncode}）：{' '.join(cmd)}\n{result.stderr.strip()}"
        )
    return json.loads(result.stdout)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--config", required=True, help="running.json 路径")
    parser.add_argument("--rust", default=None, help="ds-pac-dump 可执行文件")
    parser.add_argument("--node", default="node", help="node 可执行文件")
    parser.add_argument(
        "--pac-js",
        default=None,
        help="Node 版 pac.js 路径（默认 ../dev-sidecar/packages/mitmproxy/src/lib/pac.js）",
    )
    args = parser.parse_args()

    node = shutil.which(args.node)
    rust = rust_binary(args.rust)
    if node is None:
        print("跳过：找不到 node")
        return 0
    if rust is None:
        print("跳过：找不到 ds-pac-dump（先 cargo build --release -p ds-pac）")
        return 0

    here = os.path.dirname(os.path.abspath(__file__))
    node_script = os.path.join(here, "node_pac_dump.cjs")
    hosts = ",".join(SAMPLE_HOSTS)

    node_cmd = [node, node_script, args.config, hosts]
    if args.pac_js:
        node_cmd.append(args.pac_js)

    print(f"Node：{' '.join(node_cmd)}")
    node_out = run_json(node_cmd)

    rust_cmd = [rust, "--config", args.config, "--hosts", hosts]
    print(f"Rust：{' '.join(rust_cmd)}")
    rust_out = run_json(rust_cmd)

    failures: list[str] = []

    def diff_set(label: str, a: list[str], b: list[str]) -> None:
        sa, sb = set(a), set(b)
        only_node = sorted(sa - sb)
        only_rust = sorted(sb - sa)
        if only_node or only_rust:
            failures.append(
                f"{label} 不一致：仅 Node {len(only_node)} 条 / 仅 Rust {len(only_rust)} 条"
            )
            for item in only_node[:5]:
                failures.append(f"    仅 Node: {item}")
            for item in only_rust[:5]:
                failures.append(f"    仅 Rust: {item}")
        else:
            print(f"{label} 一致（{len(sa)} 条）")

    diff_set("代理规则", node_out["proxyRules"], rust_out["proxyRules"])
    diff_set("直连规则", node_out["directRules"], rust_out["directRules"])

    print("\n域名判定：")
    for host in SAMPLE_HOSTS:
        n = node_out["decisions"].get(host)
        r = rust_out["decisions"].get(host)
        mark = "OK " if n == r else "DIFF"
        print(f"  [{mark}] {host:<45} node={n:<6} rust={r}")
        if n != r:
            failures.append(f"判定不一致：{host} node={n} rust={r}")

    if node_out.get("proxy") != rust_out.get("proxy"):
        failures.append(f"代理地址不一致：node={node_out.get('proxy')} rust={rust_out.get('proxy')}")
    if node_out.get("hash") != rust_out.get("hash"):
        # 生成文本有细微差异时 hash 会不同，只作提示
        print(
            f"\n提示：PAC hash 不同（node={node_out.get('hash')} rust={rust_out.get('hash')}），"
            "说明脚本文本有差异（规则集合仍可一致）"
        )

    print()
    if failures:
        print(f"一致性验收失败（{len(failures)} 项）：")
        for item in failures:
            print(f"  - {item}")
        return 1

    print("一致性验收通过：两侧规则与判定完全一致")
    return 0


if __name__ == "__main__":
    sys.exit(main())
