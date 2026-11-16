#!/usr/bin/env python3
"""没有 updater 签名私钥时，把 tauri.conf.json 里的 updater 产物与插件配置去掉。

为什么需要：`bundle.createUpdaterArtifacts: true` 要求存在签名私钥，
否则 `tauri build` 直接失败。CI 里没有配 secrets 时走这里降级，配了就不动。
"""

from __future__ import annotations

import io
import json
import sys

sys.stdout.reconfigure(encoding="utf-8")

PATH = "app/src-tauri/tauri.conf.json"

conf = json.load(io.open(PATH, encoding="utf-8"))
bundle = conf.get("bundle") or {}
bundle.pop("createUpdaterArtifacts", None)
conf["bundle"] = bundle
conf.pop("plugins", None)
io.open(PATH, "w", encoding="utf-8", newline="\n").write(
    json.dumps(conf, indent=2, ensure_ascii=False) + "\n"
)
print(f"已关闭 updater 产物：{PATH}")
