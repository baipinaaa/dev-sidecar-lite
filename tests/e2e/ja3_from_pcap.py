#!/usr/bin/env python
"""从 pcap/pcapng 提取 ClientHello 并计算 JA3 / JA4 前缀（步骤 11 实验用）。

不依赖 scapy：直接解析 pcapng/pcap 的链路层帧，手工走 IPv4/IPv6 + TCP + TLS。

用法:
    python tests/e2e/ja3_from_pcap.py capture.pcapng
    python tests/e2e/ja3_from_pcap.py --selftest
"""

from __future__ import annotations

import hashlib
import struct
import sys

TLS_HANDSHAKE = 22
CLIENT_HELLO = 1

GREASE = {
    0x0A0A, 0x1A1A, 0x2A2A, 0x3A3A, 0x4A4A, 0x5A5A, 0x6A6A, 0x7A7A,
    0x8A8A, 0x9A9A, 0xAAAA, 0xBABA, 0xCACA, 0xDADA, 0xEAEA, 0xFAFA,
}


def read_pcap(path: str) -> list[bytes]:
    """返回所有 TCP 载荷（按出现顺序）。"""
    with open(path, "rb") as fp:
        data = fp.read()

    if data[:4] == b"\x0a\x0d\x0d\x0a":  # pcapng
        return _read_pcapng(data)
    return _read_pcap(data)


def _read_pcap(data: bytes) -> list[bytes]:
    magic = data[:4]
    if magic in (b"\xd4\xc3\xb2\xa1", b"\x4d\x3c\xb2\xa1"):
        endian = "<"
    elif magic in (b"\xa1\xb2\xc3\xd4", b"\xa1\xb2\x3c\x4d"):
        endian = ">"
    else:
        raise SystemExit("不是 pcap 文件")

    out: list[bytes] = []
    pos = 24
    while pos + 16 <= len(data):
        _, _, incl, _ = struct.unpack(endian + "IIII", data[pos:pos + 16])
        pos += 16
        frame = data[pos:pos + incl]
        pos += incl
        payload = _frame_payload(frame, endian)
        if payload:
            out.append(payload)
    return out


def _read_pcapng(data: bytes) -> list[bytes]:
    out: list[bytes] = []
    pos = 0
    endian = "<"
    linktype = 1
    while pos + 12 <= len(data):
        block_type, block_len = struct.unpack(endian + "II", data[pos:pos + 8])
        if block_len < 12 or pos + block_len > len(data):
            break
        body = data[pos + 8:pos + block_len - 4]

        if block_type == 0x0A0D0D0A:  # SHB
            bom = body[:4]
            endian = "<" if bom == b"\x4d\x3c\x2b\x1a" else ">"
        elif block_type == 0x00000001:  # IDB
            linktype = struct.unpack(endian + "H", body[:2])[0]
        elif block_type == 0x00000006:  # EPB
            captured = struct.unpack(endian + "I", body[12:16])[0]
            frame = body[20:20 + captured]
            payload = _frame_payload(frame, endian, linktype)
            if payload:
                out.append(payload)

        pos += block_len
    return out


def _frame_payload(frame: bytes, endian: str, linktype: int = 1) -> bytes | None:
    # 以太网
    if linktype == 1:
        if len(frame) < 14:
            return None
        ethertype = struct.unpack(">H", frame[12:14])[0]
        offset = 14
        while ethertype in (0x8100, 0x88A8) and len(frame) >= offset + 4:
            ethertype = struct.unpack(">H", frame[offset + 2:offset + 4])[0]
            offset += 4
        if ethertype == 0x0800:
            return _ipv4_payload(frame[offset:])
        if ethertype == 0x86DD:
            return _ipv6_payload(frame[offset:])
        return None
    # 原始 IP
    if linktype == 101 or linktype == 12:
        return _ipv4_payload(frame)
    return None


def _ipv4_payload(packet: bytes) -> bytes | None:
    if len(packet) < 20:
        return None
    ihl = (packet[0] & 0x0F) * 4
    if packet[9] != 6:  # TCP
        return None
    return _tcp_payload(packet[ihl:])


def _ipv6_payload(packet: bytes) -> bytes | None:
    if len(packet) < 40:
        return None
    if packet[6] != 6:  # next header = TCP
        return None
    return _tcp_payload(packet[40:])


def _tcp_payload(segment: bytes) -> bytes | None:
    if len(segment) < 20:
        return None
    offset = (segment[12] >> 4) * 4
    if offset < 20 or offset > len(segment):
        return None
    payload = segment[offset:]
    return payload if payload else None


def find_client_hello(payloads: list[bytes]) -> bytes | None:
    """在 TCP 载荷里找到 ClientHello 的 handshake 消息体。"""
    for payload in payloads:
        blob = payload
        if blob[:1] == b"\x16":  # TLS record
            # 可能一个 record 里有多条 handshake，逐个扫
            pos = 0
            while pos + 5 <= len(blob) and blob[pos] == TLS_HANDSHAKE:
                length = struct.unpack(">H", blob[pos + 3:pos + 5])[0]
                body = blob[pos + 5:pos + 5 + length]
                if body[:1] == bytes([CLIENT_HELLO]):
                    return body
                pos += 5 + length
        elif blob[:1] == bytes([CLIENT_HELLO]) and len(blob) > 100:
            return blob
    return None


def parse_client_hello(body: bytes) -> dict:
    pos = 4  # type(1) + length(3)
    version = struct.unpack(">H", body[pos:pos + 2])[0]
    pos += 2 + 32  # random

    sid_len = body[pos]
    pos += 1 + sid_len

    cs_len = struct.unpack(">H", body[pos:pos + 2])[0]
    pos += 2
    ciphers = [struct.unpack(">H", body[pos + i:pos + i + 2])[0] for i in range(0, cs_len, 2)]
    pos += cs_len

    comp_len = body[pos]
    pos += 1 + comp_len

    ext_len = struct.unpack(">H", body[pos:pos + 2])[0]
    pos += 2
    end = pos + ext_len

    exts = []
    sni = None
    alpn = []
    groups = []
    point_formats = []

    while pos + 4 <= end:
        ext_type = struct.unpack(">H", body[pos:pos + 2])[0]
        ext_len_ = struct.unpack(">H", body[pos + 2:pos + 4])[0]
        ext_body = body[pos + 4:pos + 4 + ext_len_]
        exts.append(ext_type)
        pos += 4 + ext_len_

        if ext_type == 0 and len(ext_body) > 5:  # server_name
            name_len = struct.unpack(">H", ext_body[3:5])[0]
            sni = ext_body[5:5 + name_len].decode("ascii", "replace")
        elif ext_type == 16 and len(ext_body) > 2:  # ALPN
            inner = ext_body[2:]
            i = 0
            while i < len(inner):
                l = inner[i]
                alpn.append(inner[i + 1:i + 1 + l].decode("ascii", "replace"))
                i += 1 + l
        elif ext_type == 10 and len(ext_body) > 2:  # supported_groups
            glen = struct.unpack(">H", ext_body[:2])[0]
            groups = [
                struct.unpack(">H", ext_body[2 + i:4 + i])[0]
                for i in range(0, glen, 2)
            ]
        elif ext_type == 11 and len(ext_body) > 1:  # ec_point_formats
            point_formats = list(ext_body[1:])

    return {
        "version": version,
        "ciphers": ciphers,
        "extensions": exts,
        "sni": sni,
        "alpn": alpn,
        "groups": groups,
        "point_formats": point_formats,
    }


def drop_grease(values: list[int]) -> list[int]:
    return [v for v in values if v not in GREASE]


def ja3(info: dict) -> tuple[str, str]:
    parts = [
        str(info["version"]),
        "-".join(str(c) for c in drop_grease(info["ciphers"])),
        "-".join(str(e) for e in drop_grease(info["extensions"])),
        "-".join(str(g) for g in drop_grease(info["groups"])),
        "-".join(str(p) for p in info["point_formats"]),
    ]
    raw = ",".join(parts)
    return raw, hashlib.md5(raw.encode()).hexdigest()


def ja4_prefix(info: dict) -> str:
    """近似 JA4 前缀（t13d1516h2_...），用于快速对比。"""
    proto = "t" if info["extensions"] else "t"
    ver = "13" if 43 in info["extensions"] else "12"
    sni_flag = "d" if info["sni"] and not info["sni"].replace(".", "").isdigit() else "i"
    n_ciphers = len(drop_grease(info["ciphers"]))
    n_ext = len(drop_grease(info["extensions"]))
    alpn = info["alpn"][0][:2] if info["alpn"] else "00"
    return f"{proto}{ver}{sni_flag}{min(n_ciphers, 99):02d}{min(n_ext, 99):02d}{alpn}_"


def selftest() -> int:
    """用一条构造的 ClientHello 验证解析链路。"""
    ciphers = [0x1301, 0x1302, 0x0A0A]
    groups = [0x001D, 0x0017, 0x1A1A]
    exts = [(0, b"\x00\x0b\x00\x00\x08example.com"), (10, b"\x00\x06" + b"\x00\x1d\x00\x17"),
            (11, b"\x01\x00"), (16, b"\x00\x05\x02h2"), (43, b"\x03\x04\x03\x03\x03\x04")]

    ext_blob = b""
    for t, body in exts:
        ext_blob += struct.pack(">HH", t, len(body)) + body

    body = (
        b"\x03\x03"
        + b"\x00" * 32
        + b"\x00"  # session id len
        + struct.pack(">H", len(ciphers) * 2)
        + b"".join(struct.pack(">H", c) for c in ciphers)
        + b"\x01\x00"  # compression
        + struct.pack(">H", len(ext_blob))
        + ext_blob
    )
    hello = b"\x01" + struct.pack(">I", len(body))[1:] + body

    info = parse_client_hello(hello)
    raw, digest = ja3(info)
    assert info["sni"] == "example.com", info["sni"]
    assert info["alpn"] == ["h2"], info["alpn"]
    assert 0x0A0A not in [c for c in info["ciphers"] if c in GREASE], "GREASE 未识别"
    assert 0x1A1A not in drop_grease(info["groups"])
    print(f"JA3 raw     : {raw}")
    print(f"JA3 md5     : {digest}")
    print(f"JA4 前缀    : {ja4_prefix(info)}")
    print("selftest OK")
    return 0


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print(__doc__)
        return 2
    if argv[1] == "--selftest":
        return selftest()

    payloads = read_pcap(argv[1])
    hello = find_client_hello(payloads)
    if hello is None:
        print("没有找到 ClientHello")
        return 1

    info = parse_client_hello(hello)
    raw, digest = ja3(info)
    print(f"SNI         : {info['sni']}")
    print(f"ALPN        : {info['alpn']}")
    print(f"扩展数      : {len(info['extensions'])}")
    print(f"JA3 raw     : {raw}")
    print(f"JA3 md5     : {digest}")
    print(f"JA4 前缀    : {ja4_prefix(info)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
