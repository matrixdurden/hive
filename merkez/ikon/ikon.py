#!/usr/bin/env python3
"""ikon/icon.svg ile aynı şekli bağımlılıksız çizer: ikon/icon-*.png ve ikon/icon.ico.

winloom ikonu: koyu yuvarlak kare içinde bir tezgâh: üstteki kirişten sarkan üç dikey iplik ve
aralarından bir altından bir üstünden geçen iki yatay iplik. Tek renk ailesi, tek şekil.
Çalıştır: python3 merkez/ikon/ikon.py
"""

import struct
import zlib
from pathlib import Path

ASSETS = Path(__file__).resolve().parent

# Tek vurgu rengi: arayüzün varsayılan (Claude teması) vurgusu; yatay iplikler biraz açık.
WARP = 0xD97757
WEFT = 0xF0A98A
BG = 0x0F0F10

T = 13.0                 # iplik kalınlığı
BEAM = 30.0              # tezgâhın üst kirişi (y merkezi), kalınlığı T
WARP_X = [40.0, 64.0, 88.0]   # kirişten sarkan dikey iplikler (x merkezleri)
WEFT_Y = [62.0, 86.0]         # aralarından geçen yatay iplikler (y merkezleri)
A, B = 22.0, 106.0       # kirişin uçları
WA, WB = 30.0, 98.0      # yatay ipliklerin uçları
BOTTOM = 104.0           # dikey ipliklerin alt ucu

SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256, 512]
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 256]


def rrect(px, py, x, y, w, h, r):
    if px < x or py < y or px > x + w or py > y + h:
        return False
    cx = min(max(px, x + r), x + w - r)
    cy = min(max(py, y + r), y + h - r)
    return (px - cx) ** 2 + (py - cy) ** 2 <= r * r


def rrect_stroke(px, py, x, y, w, h, r, s):
    o = s / 2
    return rrect(px, py, x - o, y - o, w + 2 * o, h + 2 * o, r + o) and not rrect(
        px, py, x + o, y + o, w - 2 * o, h - 2 * o, max(r - o, 0)
    )


def beam(px, py):
    return rrect(px, py, A, BEAM - T / 2, B - A, T, T / 2)


def capsule_v(px, py, cx):
    """Dikey iplik: kirişten sarkar, alt ucu yuvarlak."""
    return rrect(px, py, cx - T / 2, BEAM, T, BOTTOM - BEAM, T / 2)


def capsule_h(px, py, cy):
    return rrect(px, py, WA, cy - T / 2, WB - WA, T, T / 2)


def crossing(px, py, cx, cy):
    """Kesişme karesi: yatay iplik üstte kalan yerlerde yeniden çizilir."""
    return cx - T / 2 <= px <= cx + T / 2 and cy - T / 2 <= py <= cy + T / 2


# Çizim sırası: zemin, kenarlık, yatay iplikler, dikey iplikler, sonra (i + j) tek olan
# kesişmelerde yatay iplik üste alınır: dama gibi bir altından bir üstünden.
SHAPES = [
    (lambda x, y: rrect(x, y, 4, 4, 120, 120, 28), BG, 1.0),
    (lambda x, y: rrect_stroke(x, y, 4.75, 4.75, 118.5, 118.5, 27.25, 1.5), 0xFFFFFF, 0.12),
]
SHAPES += [(lambda x, y, cy=cy: capsule_h(x, y, cy), WEFT, 1.0) for cy in WEFT_Y]
SHAPES += [(lambda x, y, cx=cx: capsule_v(x, y, cx), WARP, 1.0) for cx in WARP_X]
SHAPES += [(beam, WARP, 1.0)]
SHAPES += [
    (lambda x, y, cx=cx, cy=cy: crossing(x, y, cx, cy), WEFT, 1.0)
    for i, cx in enumerate(WARP_X)
    for j, cy in enumerate(WEFT_Y)
    if (i + j) % 2 == 1
]


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
                    for covers, c, sa in SHAPES:
                        if covers(px, py):
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


def svg():
    def hexc(c):
        return f"#{c:06x}"

    out = ['<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128">']
    out.append(f'<rect x="4" y="4" width="120" height="120" rx="28" fill="{hexc(BG)}"/>')
    out.append('<rect x="4.75" y="4.75" width="118.5" height="118.5" rx="27.25" fill="none" stroke="#fff" stroke-opacity=".12" stroke-width="1.5"/>')
    for cy in WEFT_Y:
        out.append(f'<rect x="{WA}" y="{cy - T / 2}" width="{WB - WA}" height="{T}" rx="{T / 2}" fill="{hexc(WEFT)}"/>')
    for cx in WARP_X:
        out.append(f'<rect x="{cx - T / 2}" y="{BEAM}" width="{T}" height="{BOTTOM - BEAM}" rx="{T / 2}" fill="{hexc(WARP)}"/>')
    out.append(f'<rect x="{A}" y="{BEAM - T / 2}" width="{B - A}" height="{T}" rx="{T / 2}" fill="{hexc(WARP)}"/>')
    for i, cx in enumerate(WARP_X):
        for j, cy in enumerate(WEFT_Y):
            if (i + j) % 2 == 1:
                out.append(f'<rect x="{cx - T / 2}" y="{cy - T / 2}" width="{T}" height="{T}" fill="{hexc(WEFT)}"/>')
    out.append("</svg>")
    return "\n".join(out) + "\n"


def main():
    (ASSETS / "icon.svg").write_text(svg())
    images = {}
    for size in SIZES:
        images[size] = render(size)
        (ASSETS / f"icon-{size}.png").write_bytes(images[size])
        print(f"icon-{size}.png")
    (ASSETS / "icon.ico").write_bytes(ico([(s, images[s]) for s in ICO_SIZES]))
    print("icon.ico")


if __name__ == "__main__":
    main()
