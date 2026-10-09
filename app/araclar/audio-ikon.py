#!/usr/bin/env python3
"""Ses aygıtları aracının ikonu: koyu yuvarlak kare içinde gök mavisi bir kulaklık: iki kulak
yastığı. Diğer araç ikonlarıyla aynı zemin.
Çalıştır: python3 app/araclar/audio-ikon.py  →  app/assets/araclar/audio-{24..128}.png
"""

import math
import struct
import zlib
from pathlib import Path

ASSETS = Path(__file__).resolve().parent.parent / "assets" / "araclar"
SIZES = [24, 32, 40, 48, 64, 128]
ACCENT = 0x38BDF8


def rrect(px, py, x, y, w, h, r):
    if px < x or py < y or px > x + w or py > y + h:
        return False
    cx = min(max(px, x + r), x + w - r)
    cy = min(max(py, y + r), y + h - r)
    return (px - cx) ** 2 + (py - cy) ** 2 <= r * r


def rrect_stroke(px, py, x, y, w, h, r, s):
    o = s / 2
    return rrect(px, py, x - o, y - o, w + 2 * o, h + 2 * o, r + o) and not rrect(px, py, x + o, y + o, w - 2 * o, h - 2 * o, max(r - o, 0))


def circle(px, py, cx, cy, r):
    return (px - cx) ** 2 + (py - cy) ** 2 <= r * r


# 128 birimlik tuvalde, çizim sırasıyla: (örtme işlevi, renk, saydamlık)
SHAPES = [
    (lambda x, y: rrect(x, y, 4, 4, 120, 120, 28), 0x0F0F10, 1.0),
    (lambda x, y: rrect_stroke(x, y, 4.75, 4.75, 118.5, 118.5, 27.25, 1.5), 0xFFFFFF, 0.12),
    # kafa bandı: üst yarım halka; iki kulaklık yastığı
    (lambda x, y: y <= 72 and circle(x, y, 64, 72, 40) and not circle(x, y, 64, 72, 32), ACCENT, 1.0),
    (lambda x, y: rrect(x, y, 20, 64, 22, 38, 9), ACCENT, 1.0),
    (lambda x, y: rrect(x, y, 86, 64, 22, 38, 9), ACCENT, 1.0),
]


def render(size):
    ss = 4
    scale = 128 / (size * ss)
    rows = []
    for y in range(size):
        row = bytearray([0])
        for x in range(size):
            acc = [0.0, 0.0, 0.0, 0.0]
            for sy in range(ss):
                for sx in range(ss):
                    px = (x * ss + sx + 0.5) * scale
                    py = (y * ss + sy + 0.5) * scale
                    r = g = b = a = 0.0
                    for cover, c, sa in SHAPES:
                        if cover(px, py):
                            cr, cg, cb = (c >> 16) / 255, ((c >> 8) & 255) / 255, (c & 255) / 255
                            r, g, b = cr * sa + r * (1 - sa), cg * sa + g * (1 - sa), cb * sa + b * (1 - sa)
                            a = sa + a * (1 - sa)
                    acc[0] += r
                    acc[1] += g
                    acc[2] += b
                    acc[3] += a
            n = ss * ss
            a = acc[3] / n
            if a > 0:
                row += bytes(round(min(acc[k] / n / a, 1) * 255) for k in range(3)) + bytes([round(a * 255)])
            else:
                row += b"\0\0\0\0"
        rows.append(bytes(row))
    return png(size, b"".join(rows))


def png(size, raw):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def main():
    for size in SIZES:
        (ASSETS / f"audio-{size}.png").write_bytes(render(size))
        print(f"audio-{size}.png")


if __name__ == "__main__":
    main()
