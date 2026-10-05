#!/usr/bin/env python3
"""assets/icon.svg ile aynı şekli bağımlılıksız çizer: assets/icon-*.png ve assets/icon.ico.

hive ikonu: koyu yuvarlak kare içinde dört petek gözü; üçü araçların renginde, biri boş (yeni
gelecek araç). Şekil yuvarlak dikdörtgen ve altıgenlerden oluştuğu için küçük bir örneklemeli
tarayıcı yeter.
Çalıştır: python3 araclar/ikon.py
"""

import struct
import zlib
from pathlib import Path

ASSETS = Path(__file__).resolve().parent.parent / "assets"

import math

R = 19.0  # petek gözlerinin aralığını belirleyen yarıçap; gözler arada boşluk kalsın diye daha küçük


def hexagon(cx, cy, r):
    """Sivri tepeli altıgenin köşeleri."""
    return [(cx + r * math.cos(math.radians(a)), cy + r * math.sin(math.radians(a))) for a in range(-90, 270, 60)]


# ("rrect", x, y, w, h, yarıçap, renk, saydamlık, kontur) ya da ("hex", cx, cy, r, renk, saydamlık, kontur)
SHAPES = [
    ("rrect", 4, 4, 120, 120, 28, 0x0F0F10, 1.0, None),
    ("rrect", 4.75, 4.75, 118.5, 118.5, 27.25, 0xFFFFFF, 0.12, 1.5),
    ("hex", 56.0, 50.0, R - 2.5, 0xFBBF24, 1.0, None),
    ("hex", 89.0, 50.0, R - 2.5, 0xF472B6, 1.0, None),
    ("hex", 72.5, 78.5, R - 2.5, 0xA78BFA, 1.0, None),
    ("hex", 39.5, 78.5, R - 5.0, 0xFFFFFF, 0.28, 4.0),
]

SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256, 512]
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 256]


def inside(px, py, x, y, w, h, r):
    if px < x or py < y or px > x + w or py > y + h:
        return False
    cx = min(max(px, x + r), x + w - r)
    cy = min(max(py, y + r), y + h - r)
    return (px - cx) ** 2 + (py - cy) ** 2 <= r * r


def in_poly(px, py, pts):
    """Dışbükey çokgenin içinde mi (köşeler saat yönünde)."""
    n = len(pts)
    for i in range(n):
        (x1, y1), (x2, y2) = pts[i], pts[(i + 1) % n]
        if (x2 - x1) * (py - y1) - (y2 - y1) * (px - x1) < 0:
            return False
    return True


def covers(shape, px, py):
    if shape[0] == "hex":
        _, cx, cy, r, _, _, stroke = shape
        if stroke is None:
            return in_poly(px, py, hexagon(cx, cy, r))
        o = stroke / 2 / math.cos(math.radians(30))
        return in_poly(px, py, hexagon(cx, cy, r + o)) and not in_poly(px, py, hexagon(cx, cy, r - o))
    _, x, y, w, h, r, _, _, stroke = shape
    if stroke is None:
        return inside(px, py, x, y, w, h, r)
    o, i = stroke / 2, stroke / 2
    return inside(px, py, x - o, y - o, w + 2 * o, h + 2 * o, r + o) and not inside(
        px, py, x + i, y + i, w - 2 * i, h - 2 * i, max(r - i, 0)
    )


def color_of(shape):
    return (shape[4], shape[5]) if shape[0] == "hex" else (shape[6], shape[7])


def render(size):
    ss = 4 if size <= 128 else 2
    scale = 128 / (size * ss)
    rows = []
    for y in range(size):
        row = bytearray([0])
        for x in range(size):
            acc = [0.0, 0.0, 0.0, 0.0]  # önceden çarpılmış r, g, b, a
            for sy in range(ss):
                for sx in range(ss):
                    px = (x * ss + sx + 0.5) * scale
                    py = (y * ss + sy + 0.5) * scale
                    r = g = b = a = 0.0
                    for shape in SHAPES:
                        if covers(shape, px, py):
                            c, sa = color_of(shape)
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


def ico(images):
    head = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries, blobs = b"", b""
    for size, data in images:
        s = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", s, s, 0, 0, 1, 32, len(data), offset)
        blobs += data
        offset += len(data)
    return head + entries + blobs


def main():
    images = {}
    for size in SIZES:
        images[size] = render(size)
        (ASSETS / f"icon-{size}.png").write_bytes(images[size])
        print(f"icon-{size}.png")
    (ASSETS / "icon.ico").write_bytes(ico([(s, images[s]) for s in ICO_SIZES]))
    print("icon.ico")


if __name__ == "__main__":
    main()
