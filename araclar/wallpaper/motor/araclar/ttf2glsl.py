#!/usr/bin/env python3
"""TrueType glyph outlines → GLSL quadratic Bézier arrays (pure Python, no deps).

Usage: ttf2glsl.py OUT.glsl FONT.ttf "chars" [FONT2.ttf "chars2" ...]
Every glyph is emitted as (start, count, advance) with coordinates normalised so the
cap height (yMax of 'H') is 1.0. Lines are stored as quadratics whose control point
is the midpoint.
"""
import struct
import sys


def u16(b, o): return struct.unpack_from(">H", b, o)[0]
def i16(b, o): return struct.unpack_from(">h", b, o)[0]
def u32(b, o): return struct.unpack_from(">I", b, o)[0]


class TTF:
    def __init__(self, path):
        self.b = open(path, "rb").read()
        b = self.b
        n = u16(b, 4)
        self.tables = {}
        for i in range(n):
            tag = b[12 + i * 16:16 + i * 16].decode("latin1")
            off = u32(b, 12 + i * 16 + 8)
            ln = u32(b, 12 + i * 16 + 12)
            self.tables[tag] = (off, ln)
        head = self.tables["head"][0]
        self.upm = u16(b, head + 18)
        self.long_loca = i16(b, head + 50) == 1
        self.num_glyphs = u16(b, self.tables["maxp"][0] + 4)
        loca = self.tables["loca"][0]
        if self.long_loca:
            self.loca = [u32(b, loca + 4 * i) for i in range(self.num_glyphs + 1)]
        else:
            self.loca = [2 * u16(b, loca + 2 * i) for i in range(self.num_glyphs + 1)]
        hhea = self.tables["hhea"][0]
        nhm = u16(b, hhea + 34)
        hmtx = self.tables["hmtx"][0]
        self.adv = [u16(b, hmtx + 4 * min(i, nhm - 1)) for i in range(self.num_glyphs)]
        self.cmap = self._cmap()

    def _cmap(self):
        b = self.b
        base = self.tables["cmap"][0]
        n = u16(b, base + 2)
        best = None
        for i in range(n):
            pid, eid, off = u16(b, base + 4 + 8 * i), u16(b, base + 6 + 8 * i), u32(b, base + 8 + 8 * i)
            fmt = u16(b, base + off)
            if fmt == 4 and (pid, eid) in ((3, 1), (0, 3), (0, 4), (0, 6)):
                best = base + off
        assert best, "format 4 cmap bulunamadı"
        m = {}
        seg2 = u16(b, best + 6)
        seg = seg2 // 2
        ends = best + 14
        starts = ends + seg2 + 2
        deltas = starts + seg2
        ranges = deltas + seg2
        for s in range(seg):
            end, start = u16(b, ends + 2 * s), u16(b, starts + 2 * s)
            delta, ro = i16(b, deltas + 2 * s), u16(b, ranges + 2 * s)
            for c in range(start, min(end, 0xFFFE) + 1):
                if ro == 0:
                    g = (c + delta) & 0xFFFF
                else:
                    addr = ranges + 2 * s + ro + 2 * (c - start)
                    g = u16(b, addr)
                    if g:
                        g = (g + delta) & 0xFFFF
                if g:
                    m[c] = g
        return m

    def contours(self, gid, dx=0, dy=0):
        """Liste[Liste[(x, y, on_curve)]]"""
        b = self.b
        off, end = self.loca[gid], self.loca[gid + 1]
        if end <= off:
            return []
        g = self.tables["glyf"][0] + off
        nc = i16(b, g)
        if nc < 0:
            return self._composite(g, dx, dy)
        p = g + 10
        ends = [u16(b, p + 2 * i) for i in range(nc)]
        p += 2 * nc
        npts = ends[-1] + 1 if nc else 0
        il = u16(b, p)
        p += 2 + il
        flags = []
        while len(flags) < npts:
            f = b[p]; p += 1
            flags.append(f)
            if f & 8:
                r = b[p]; p += 1
                flags += [f] * r
        xs, v = [], 0
        for f in flags:
            if f & 2:
                d = b[p]; p += 1
                v += d if f & 16 else -d
            elif not f & 16:
                v += i16(b, p); p += 2
            xs.append(v)
        ys, v = [], 0
        for f in flags:
            if f & 4:
                d = b[p]; p += 1
                v += d if f & 32 else -d
            elif not f & 32:
                v += i16(b, p); p += 2
            ys.append(v)
        out, s = [], 0
        for e in ends:
            out.append([(xs[i] + dx, ys[i] + dy, bool(flags[i] & 1)) for i in range(s, e + 1)])
            s = e + 1
        return out

    def _composite(self, g, dx, dy):
        b = self.b
        p = g + 10
        out = []
        while True:
            flags, gi = u16(b, p), u16(b, p + 2)
            p += 4
            if flags & 1:
                a1, a2 = i16(b, p), i16(b, p + 2); p += 4
            else:
                a1, a2 = struct.unpack_from(">bb", b, p); p += 2
            if flags & 8: p += 2
            elif flags & 0x40: p += 4
            elif flags & 0x80: p += 8
            assert flags & 2, "yalnızca xy ofsetli bileşenler"
            out += self.contours(gi, dx + a1, dy + a2)
            if not flags & 0x20:
                break
        return out


def to_quads(contour):
    """TrueType konturu → [(p0, c, p2)] kuadratik parçalar (çizgiler orta noktalı)."""
    pts = [(x, y, on) for x, y, on in contour]
    if not any(on for _, _, on in pts):
        # tüm noktalar kontrol: ilk ima edilen orta noktayı başlangıç yap
        x0, y0, _ = pts[0]; x1, y1, _ = pts[1]
        pts.insert(1, ((x0 + x1) / 2, (y0 + y1) / 2, True))
    # on-curve ile başlat
    while not pts[0][2]:
        pts.append(pts.pop(0))
    pts.append(pts[0])
    quads = []
    i = 0
    while i < len(pts) - 1:
        p0 = pts[i][:2]
        nxt = pts[i + 1]
        if nxt[2]:
            quads.append((p0, ((p0[0] + nxt[0]) / 2, (p0[1] + nxt[1]) / 2), nxt[:2]))
            i += 1
        else:
            c = nxt[:2]
            after = pts[i + 2]
            if after[2]:
                quads.append((p0, c, after[:2]))
                i += 2
            else:
                mid = ((c[0] + after[0]) / 2, (c[1] + after[1]) / 2)
                quads.append((p0, c, mid))
                pts.insert(i + 2, (mid[0], mid[1], True))
                i += 2
    return quads


def main():
    out_path = sys.argv[1]
    pairs = list(zip(sys.argv[2::2], sys.argv[3::2]))
    pts, glyphs, names = [], [], []
    for path, chars in pairs:
        f = TTF(path)
        cap = max(y for c in f.contours(f.cmap[ord("H")]) for _, y, _ in c)
        for ch in chars:
            gid = f.cmap[ord(ch)]
            quads = [q for c in f.contours(gid) for q in to_quads(c)]
            start = len(pts) // 3  # parça indeksi
            xs, ys = [], []
            for q in quads:
                for (x, y) in q:
                    pts.append((x / cap, y / cap))
                    xs.append(x / cap); ys.append(y / cap)
            bbox = (min(xs), min(ys), max(xs), max(ys)) if xs else (0, 0, 0, 0)
            glyphs.append((start, len(quads), f.adv[gid] / cap, bbox))
            names.append(ch)
    def tree(items, fmt, ind="    "):
        # İkili arama ağacı: FXC için düz, dizisiz kod.
        if len(items) == 1:
            return f"{ind}return {fmt(items[0][1])};\n"
        mid = len(items) // 2
        return (f"{ind}if (i < {items[mid][0]}) {{\n" + tree(items[:mid], fmt, ind + "    ")
                + f"{ind}}} else {{\n" + tree(items[mid:], fmt, ind + "    ") + f"{ind}}}\n")
    with open(out_path, "w") as o:
        o.write("// Montserrat (SIL OFL 1.1, github.com/JulietaUla/Montserrat) glif konturları; ttf2glsl.py ile üretildi.\n")
        o.write("// Birim: büyük harf yüksekliği 1. Her parça üç nokta: başlangıç, kontrol, bitiş.\n")
        o.write("// Sabit dizi yok: naga diziyi her piksel için yerel belleğe kopyalıyor, FXC de döngüleri açıyor.\n")
        o.write(f"const int GLYPH_N = {len(glyphs)};\nconst int GP_N = {len(pts)};\n")
        o.write("vec2 gp(int i) {\n" + tree(list(enumerate(pts)), lambda p: "vec2(%.4f, %.4f)" % p) + "}\n")
        o.write("// (başlangıç parçası, parça sayısı, ilerleme) — sıra: " + " ".join(names) + "\n")
        o.write("vec3 gm(int i) {\n" + tree(list(enumerate(glyphs)), lambda g: "vec3(%d.0, %d.0, %.4f)" % (g[0], g[1], g[2])) + "}\n")
        o.write("vec4 gb(int i) {\n" + tree(list(enumerate(glyphs)), lambda g: "vec4(%.4f, %.4f, %.4f, %.4f)" % g[3]) + "}\n")
    print(f"{len(glyphs)} glif, {len(pts)//3} parça, {len(pts)} nokta → {out_path}")
    for n, g in zip(names, glyphs):
        print(f"  {n!r}: {g[1]} parça, ilerleme {g[2]:.3f}, bbox {tuple(round(v,3) for v in g[3])}")


if __name__ == "__main__":
    main()
