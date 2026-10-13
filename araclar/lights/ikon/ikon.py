#!/usr/bin/env python3
"""Işıklar aracının ikonu: koyu yuvarlak kare içinde sarı bir güneş (ortada daire, çevresinde
sekiz ışın). Diğer araç ikonlarıyla aynı zemin.
Çalıştır: python3 araclar/lights/ikon/ikon.py  →  araclar/lights/ikon/lights-{24..128}.png
"""

import struct
import zlib
from pathlib import Path

ASSETS = Path(__file__).resolve().parent
SIZES = [24, 32, 40, 48, 64, 128]
ACCENT = 0xFACC15


def rrect(px, py, x, y, w, h, r):
    if px < x or py < y or px > x + w or py > y + h:
        return False
    cx = min(max(px, x + r), x + w - r)
    cy = min(max(py, y + r), y + h - r)
    return (px - cx) ** 2 + (py - cy) ** 2 <= r * r


def rrect_stroke(px, py, x, y, w, h, r, s):
    o = s / 2
    return rrect(px, py, x - o, y - o, w + 2 * o, h + 2 * o, r + o) and not rrect(px, py, x + o, y + o, w - 2 * o, h - 2 * o, max(r - o, 0))


def poly(px, py, pts):
    """Çokgenin içinde mi (ışın sayma; içbükey de olur)."""
    inside = False
    n = len(pts)
    for i in range(n):
        (x1, y1), (x2, y2) = pts[i], pts[(i + 1) % n]
        if (y1 > py) != (y2 > py):
            x = x1 + (py - y1) * (x2 - x1) / (y2 - y1)
            if px < x:
                inside = not inside
    return inside


import math


def disc(px, py, cx, cy, r):
    return (px - cx) ** 2 + (py - cy) ** 2 <= r * r


def ray(px, py, k):
    """k. ışın: merkezden dışa uzanan yuvarlak uçlu kalın çizgi."""
    a = k * math.pi / 4
    x1, y1 = 64 + 32 * math.cos(a), 64 + 32 * math.sin(a)
    x2, y2 = 64 + 46 * math.cos(a), 64 + 46 * math.sin(a)
    dx, dy = x2 - x1, y2 - y1
    t = max(0, min(1, ((px - x1) * dx + (py - y1) * dy) / (dx * dx + dy * dy)))
    return (px - x1 - t * dx) ** 2 + (py - y1 - t * dy) ** 2 <= 5.5 ** 2


# 128 birimlik tuvalde, çizim sırasıyla: (örtme işlevi, renk, saydamlık)
SHAPES = [
    (lambda x, y: rrect(x, y, 4, 4, 120, 120, 28), 0x0F0F10, 1.0),
    (lambda x, y: rrect_stroke(x, y, 4.75, 4.75, 118.5, 118.5, 27.25, 1.5), 0xFFFFFF, 0.12),
    (lambda x, y: disc(x, y, 64, 64, 22) or any(ray(x, y, k) for k in range(8)), ACCENT, 1.0),
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
        (ASSETS / f"lights-{size}.png").write_bytes(render(size))
        print(f"lights-{size}.png")


if __name__ == "__main__":
    main()
