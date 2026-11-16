#!/usr/bin/env python3
"""生成占位应用图标（PNG + ICO），不需要任何第三方库。

仓库里不放二进制图标是常见做法，但 `tauri build` 必须要 `icons/icon.ico` 存在，
CI 又不可能临时去跑 `npx tauri icon`（那需要一张 1024x1024 的源图）。
所以这里用代码画一个占位图：深色圆角底 + 蓝色圆环 + 白色斜杠，风格与 UI 的深色主题一致。

用法：
    python scripts/gen-icons.py                 # 写 app/src-tauri/icons/{icon.png,icon.ico,32x32.png,128x128.png,...}
    python scripts/gen-icons.py --out <dir>

换成正式图标时，把 1024x1024 的 png 放进来跑 `npx tauri icon` 覆盖即可。
"""

from __future__ import annotations

import argparse
import io
import os
import struct
import sys
import zlib

sys.stdout.reconfigure(encoding="utf-8")

BG = (15, 23, 42, 255)        # #0f172a
RING = (59, 130, 246, 255)    # #3b82f6
ACCENT = (226, 232, 240, 255)  # #e2e8f0


def rounded_alpha(x: float, y: float, size: int, radius: float) -> float:
    """圆角矩形覆盖率（0..1），边缘做 1px 抗锯齿。"""
    edge = 1.0
    cx = min(max(x, radius), size - radius)
    cy = min(max(y, radius), size - radius)
    dist = ((x - cx) ** 2 + (y - cy) ** 2) ** 0.5
    inside = dist <= radius
    if not inside:
        return 0.0
    if dist > radius - edge:
        return max(0.0, (radius - dist) / edge)
    return 1.0


def render(size: int) -> bytes:
    """返回 RGBA 像素行序列。"""
    radius = size * 0.22
    center = size / 2.0
    ring_outer = size * 0.34
    ring_inner = size * 0.24
    rows = []
    for y in range(size):
        row = bytearray()
        for x in range(size):
            px, py = x + 0.5, y + 0.5
            alpha = rounded_alpha(px, py, size, radius)
            if alpha <= 0.0:
                row += bytes((0, 0, 0, 0))
                continue
            dist = ((px - center) ** 2 + (py - center) ** 2) ** 0.5
            if ring_inner <= dist <= ring_outer:
                color = RING
            elif dist < ring_inner * 0.55:
                color = ACCENT
            else:
                color = BG
            # 斜杠：距对角线很近的像素涂成强调色
            if abs((px - py)) < size * 0.055 and ring_inner * 0.5 < dist < ring_outer * 1.05:
                color = ACCENT
            row += bytes((color[0], color[1], color[2], int(color[3] * alpha)))
        rows.append(bytes(row))
    return b"".join(rows)


def png(size: int) -> bytes:
    raw = render(size)
    stride = size * 4
    # 每行前加 filter type 0
    filtered = b"".join(b"\x00" + raw[y * stride : (y + 1) * stride] for y in range(size))

    def chunk(tag: bytes, data: bytes) -> bytes:
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(filtered, 9))
        + chunk(b"IEND", b"")
    )


def ico(png_bytes: bytes, size: int) -> bytes:
    """单张 PNG 压缩的 ICO（Vista+ 支持，Tauri 的 ico 解析器也支持）。"""
    header = struct.pack("<HHH", 0, 1, 1)
    entry = struct.pack(
        "<BBBBHHII",
        0 if size >= 256 else size,
        0 if size >= 256 else size,
        0,
        0,
        1,
        32,
        len(png_bytes),
        6 + 16,
    )
    return header + entry + png_bytes


TARGETS = [
    ("icon.png", 512),
    ("128x128.png", 128),
    ("128x128@2x.png", 256),
    ("32x32.png", 32),
]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    out_dir = args.out or os.path.join(root, "app", "src-tauri", "icons")
    os.makedirs(out_dir, exist_ok=True)

    for name, size in TARGETS:
        data = png(size)
        io.open(os.path.join(out_dir, name), "wb").write(data)
        print(f"写入 {name} ({size}x{size}, {len(data)} 字节)")

    ico_bytes = ico(png(256), 256)
    io.open(os.path.join(out_dir, "icon.ico"), "wb").write(ico_bytes)
    print(f"写入 icon.ico (256x256 PNG 压缩, {len(ico_bytes)} 字节)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
