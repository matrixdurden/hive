#!/usr/bin/env python3
"""engram ikonu: koyu yuvarlak kare içinde bir anı izi. Üç düğüm birbirine bağlı, en parlağı
yeni oluşan anı: bir nöron ağının küçük bir parçası. Bağımlılıksız, örneklemeli tarayıcı.
Çalıştır: python3 araclar/engram/motor/araclar/ikon.py  →  araclar/engram/motor/assets/icon-*.png ve icon.ico
"""

import struct
import zlib
from pathlib import Path

ASSETS = Path(__file__).resolve().parent.parent / "assets"
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256, 512]
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 256]

BG = 0x0F0F10
LINE = 0x7C6CF0  # bağlantılar: mor-mavi
NODE = 0xA78BFA  # düğümler
SPARK = 0xF5D06F  # yeni anı: sıcak sarı
GLOW = 0xF5D06F


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


def segment(px, py, x1, y1, x2, y2, w):
    """Kalınlığı w olan, uçları yuvarlak çizgi."""
    dx, dy = x2 - x1, y2 - y1
    l2 = dx * dx + dy * dy
    t = 0 if l2 == 0 else max(0, min(1, ((px - x1) * dx + (py - y1) * dy) / l2))
    qx, qy = x1 + t * dx, y1 + t * dy
    return (px - qx) ** 2 + (py - qy) ** 2 <= (w / 2) ** 2


# Düğümler (128 birimlik tuvalde).
A = (38, 88)   # eski anı
B = (66, 44)   # ara düğüm
C = (94, 78)   # yeni anı (parlak)
D = (82, 108)  # küçük uç

# Çizim sırasıyla: (örtme işlevi, renk, saydamlık)
SHAPES = [
    (lambda x, y: rrect(x, y, 4, 4, 120, 120, 28), BG, 1.0),
    (lambda x, y: rrect_stroke(x, y, 4.75, 4.75, 118.5, 118.5, 27.25, 1.5), 0xFFFFFF, 0.12),
    # yeni anının ışıltısı
    (lambda x, y: circle(x, y, *C, 26), GLOW, 0.10),
    (lambda x, y: circle(x, y, *C, 18), GLOW, 0.14),
    # bağlantılar
    (lambda x, y: segment(x, y, *A, *B, 6), LINE, 1.0),
    (lambda x, y: segment(x, y, *B, *C, 6), LINE, 1.0),
    (lambda x, y: segment(x, y, *C, *D, 5), LINE, 1.0),
    (lambda x, y: segment(x, y, *A, *D, 4), LINE, 0.55),
    # düğümler
    (lambda x, y: circle(x, y, *A, 11), NODE, 1.0),
    (lambda x, y: circle(x, y, *B, 9), NODE, 1.0),
    (lambda x, y: circle(x, y, *D, 7), NODE, 1.0),
    (lambda x, y: circle(x, y, *C, 14), SPARK, 1.0),
    (lambda x, y: circle(x, y, C[0] - 4, C[1] - 4, 4.5), 0xFFFFFF, 0.55),
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


def ico(entries):
    """PNG gömülü ICO (Vista ve sonrası): [(boy, png bayt)]."""
    head = struct.pack("<HHH", 0, 1, len(entries))
    offset = 6 + 16 * len(entries)
    dirs, blobs = b"", b""
    for size, data in entries:
        s = 0 if size >= 256 else size
        dirs += struct.pack("<BBBBHHII", s, s, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
        blobs += data
    return head + dirs + blobs


def main():
    pngs = {}
    for size in SIZES:
        pngs[size] = render(size)
        (ASSETS / f"icon-{size}.png").write_bytes(pngs[size])
        print(f"icon-{size}.png")
    (ASSETS / "icon.ico").write_bytes(ico([(s, pngs[s]) for s in ICO_SIZES]))
    print("icon.ico")


if __name__ == "__main__":
    main()
