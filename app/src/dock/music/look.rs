//! Kartın işlemcide hazırlanan resimleri: altındaki gölge ve kapaktan bulanık arka plan.
//! İkisi de yalnızca boyut ya da şarkı değişince yapılır.

/// Yuvarlak dikdörtgenin işaretli uzaklığı (içeride negatif). `x, y` merkeze göre.
fn sdf(x: f32, y: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let (qx, qy) = (x.abs() - (hw - r), y.abs() - (hh - r));
    let out = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    out + qx.max(qy).min(0.0) - r
}

/// Kartın altındaki yumuşak gölge: pencere boyunda BGRA önçarpımlı siyah. `card` kartın
/// penceredeki yeri, `radius`, `sigma` (yayılma) ve `dy` (aşağı kayma) piksel.
pub fn shadow(w: usize, h: usize, card: (f32, f32, f32, f32), radius: f32, sigma: f32, dy: f32, alpha: f32) -> Vec<u8> {
    let (l, t, r, b) = card;
    let (hw, hh) = ((r - l) / 2.0, (b - t) / 2.0);
    let (cx, cy) = (l + hw, t + hh + dy);
    let mut px = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let d = sdf(x as f32 + 0.5 - cx, y as f32 + 0.5 - cy, hw, hh, radius);
            let a = if d <= 0.0 { alpha } else { alpha * (-(d * d) / (2.0 * sigma * sigma)).exp() };
            px[(y * w + x) * 4 + 3] = (a * 255.0) as u8;
        }
    }
    px
}

/// Kapaktan arka plan: kartın en-boy oranında ortadan bir şerit, küçültülmüş, bulanık,
/// doygunluğu artırılmış ve koyulaştırılmış (üstünde beyaz yazı okunsun). `px` 32bppPBGRA.
/// Dönen: (genişlik, yükseklik, BGRA önçarpımlı opak).
pub fn ambient(px: &[u8], w: usize, h: usize, aspect: f32) -> (usize, usize, Vec<u8>) {
    let (ow, oh) = (72usize, ((72.0 / aspect).round() as usize).max(8));
    // Kaynağın ortasındaki şerit.
    let band = (w as f32 / aspect).min(h as f32);
    let top = (h as f32 - band) / 2.0;
    let mut buf = vec![[0f32; 3]; ow * oh];
    for y in 0..oh {
        for x in 0..ow {
            let (x0, x1) = ((x * w / ow), ((x + 1) * w / ow).max(x * w / ow + 1));
            let sy = top + band * y as f32 / oh as f32;
            let sy1 = top + band * (y + 1) as f32 / oh as f32;
            let (y0, y1) = (sy as usize, (sy1 as usize).max(sy as usize + 1).min(h));
            let mut acc = [0f32; 3];
            let mut n = 0f32;
            for yy in y0..y1 {
                for xx in x0..x1.min(w) {
                    let i = (yy * w + xx) * 4;
                    for c in 0..3 {
                        acc[c] += px[i + 2 - c] as f32 / 255.0;
                    }
                    n += 1.0;
                }
            }
            buf[y * ow + x] = acc.map(|v| v / n.max(1.0));
        }
    }
    let mut tmp = buf.clone();
    for _ in 0..3 {
        box_pass(&buf, &mut tmp, ow, oh, 4, true);
        box_pass(&tmp, &mut buf, ow, oh, 4, false);
    }
    let mut out = vec![0u8; ow * oh * 4];
    for (i, c) in buf.iter().enumerate() {
        let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        // Doygunluk 1.4×, parlaklık en fazla 0.42 (açık kapaklar da koyu bir zemin versin).
        let dim = (0.42 / l.max(0.01)).min(0.75);
        let v = c.map(|v| ((l + (v - l) * 1.4) * dim).clamp(0.0, 1.0));
        out[i * 4] = (v[2] * 255.0) as u8;
        out[i * 4 + 1] = (v[1] * 255.0) as u8;
        out[i * 4 + 2] = (v[0] * 255.0) as u8;
        out[i * 4 + 3] = 255;
    }
    (ow, oh, out)
}

fn box_pass(src: &[[f32; 3]], dst: &mut [[f32; 3]], w: usize, h: usize, r: usize, horizontal: bool) {
    let (n, lines) = if horizontal { (w, h) } else { (h, w) };
    let at = |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
    let k = 1.0 / (2 * r + 1) as f32;
    for line in 0..lines {
        let mut acc = [0f32; 3];
        for j in 0..=2 * r {
            let v = src[at(line, j.saturating_sub(r).min(n - 1))];
            for c in 0..3 {
                acc[c] += v[c];
            }
        }
        for i in 0..n {
            dst[at(line, i)] = acc.map(|v| v * k);
            let add = src[at(line, (i + r + 1).min(n - 1))];
            let sub = src[at(line, i.saturating_sub(r))];
            for c in 0..3 {
                acc[c] += add[c] - sub[c];
            }
        }
    }
}
