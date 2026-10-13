//! Uyandırma: enerji tabanlı konuşma algılayıcı (`Vad`), kullanıcının kendi sesinden öğrenilen
//! uyandırma sözü (`Spotter`: MFCC + DTW şablon eşleme) ve yazıya çevrilmiş metin üzerinde
//! uyandırma sözü / evet-hayır tanıma. Ses her yerde 16 kHz mono i16; çerçeve 320 örnek (20 ms).
#![allow(dead_code)]

use std::collections::VecDeque;
use std::path::Path;
use std::sync::OnceLock;

use crate::util::Res;

const RATE: usize = 16_000;
/// Milisaniye → örnek sayısı.
const fn ms(n: usize) -> usize {
    n * RATE / 1000
}

// ---------------------------------------------------------------------------------------------
// Konuşma algılayıcı
// ---------------------------------------------------------------------------------------------

pub enum VadEvent {
    None,
    Start,
    End(Vec<i16>),
}

/// Konuşma başı: gürültü tabanının bu kadar dB üstü.
const START_DB: f32 = 10.0;
/// Konuşma sürüyor sayılması için (eşik farkı kararsız gidip gelmeyi önler).
const KEEP_DB: f32 = 6.0;
/// Bunun altı (≈ −60 dBFS) hiç konuşma sayılmaz: sessize alınmış mikrofonda taban sıfıra iner.
const ABS_DB: f32 = 30.0;
/// Gürültü tabanı: son 3 s'deki en düşük çerçeve enerjisi (minimum istatistiği). Sabit fan sesi
/// en geç bu süre sonra tabana karışır, konuşma "takılı" kalmaz.
const FLOOR_FRAMES: usize = 150;
/// Başlangıç için art arda bu kadar yüksek çerçeve (≥ 2'si ötümlü) gerekir: tık sesleri elenir.
const START_FRAMES: u32 = 3;
const PRE_ROLL: usize = ms(300);
const TAIL: usize = ms(150);

/// Enerji tabanlı konuşma algılayıcı; gürültü tabanını kendisi uyarlar.
pub struct Vad {
    /// Bu kadar sessizlikten sonra konuşma biter.
    pub silence_ms: u32,
    /// Konuşma en fazla bu kadar sürer (sonra zorla biter).
    pub max_ms: u32,
    /// Son çerçeve enerjileri (dB), taban için.
    hist: VecDeque<f32>,
    floor: f32,
    // Tek kutuplu yüksek geçiren süzgeç durumu (DC kayması enerjiyi ve sıfır geçişini bozmasın).
    hp_x: f32,
    hp_y: f32,
    run: u32,
    voiced: u32,
    pre: VecDeque<i16>,
    seg: Vec<i16>,
    speaking: bool,
    /// `seg` içinde son konuşma çerçevesinin bittiği yer.
    last_speech: usize,
}

impl Default for Vad {
    fn default() -> Self {
        Self::new()
    }
}

impl Vad {
    pub fn new() -> Self {
        Self {
            silence_ms: 700,
            max_ms: 15_000,
            hist: VecDeque::with_capacity(FLOOR_FRAMES + 1),
            floor: 0.0,
            hp_x: 0.0,
            hp_y: 0.0,
            run: 0,
            voiced: 0,
            pre: VecDeque::with_capacity(PRE_ROLL * 2),
            seg: Vec::new(),
            speaking: false,
            last_speech: 0,
        }
    }

    /// Konuşma durumunu sıfırlar; öğrenilmiş gürültü tabanı kalır.
    pub fn reset(&mut self) {
        self.run = 0;
        self.voiced = 0;
        self.pre.clear();
        self.seg.clear();
        self.speaking = false;
        self.last_speech = 0;
    }

    pub fn speaking(&self) -> bool {
        self.speaking
    }

    /// Şu anki gürültü tabanı (dB, 1 LSB = 0 dB); tanılama için.
    pub fn floor_db(&self) -> f32 {
        self.floor
    }

    /// Çerçevenin enerjisi (dB) ve ötümlü olup olmadığı (az sıfır geçişi, tık değil).
    fn analyze(&mut self, frame: &[i16]) -> (f32, bool) {
        let (mut sum, mut peak, mut zc) = (0f32, 0f32, 0u32);
        let mut prev = self.hp_y;
        for &s in frame {
            let x = s as f32;
            let y = x - self.hp_x + 0.995 * self.hp_y;
            self.hp_x = x;
            self.hp_y = y;
            sum += y * y;
            peak = peak.max(y.abs());
            if (y >= 0.0) != (prev >= 0.0) {
                zc += 1;
            }
            prev = y;
        }
        let n = frame.len().max(1) as f32;
        let rms = (sum / n).sqrt();
        let db = 10.0 * (sum / n + 1.0).log10();
        let zcr = zc as f32 / n;
        // Konuşmada tepe/RMS ≈ 3–5; tık ve vuruşlarda çok daha yüksek.
        let crest = peak / (rms + 1.0);
        (db, zcr < 0.3 && crest < 8.0)
    }

    /// 20 ms'lik çerçeve. Konuşma başlayınca `Start`; bitince `End(bölüt)`: bölüt ~300 ms öncesini
    /// içerir, sondaki sessizlik ~150 ms'ye kırpılır.
    pub fn push(&mut self, frame: &[i16]) -> VadEvent {
        if frame.is_empty() {
            return VadEvent::None;
        }
        let (db, voiced) = self.analyze(frame);
        if self.hist.len() == FLOOR_FRAMES {
            self.hist.pop_front();
        }
        self.hist.push_back(db);
        self.floor = self.hist.iter().copied().fold(f32::MAX, f32::min);
        // Isınma: taban oturana kadar (200 ms) karar verilmez.
        let warm = self.hist.len() >= 10;

        if !self.speaking {
            self.pre.extend(frame.iter().copied());
            if warm && db > self.floor + START_DB && db > ABS_DB {
                self.run += 1;
                self.voiced += voiced as u32;
            } else {
                self.run = 0;
                self.voiced = 0;
            }
            let keep = PRE_ROLL + self.run as usize * frame.len();
            while self.pre.len() > keep.min(ms(1000)) {
                self.pre.pop_front();
            }
            if self.run >= START_FRAMES && self.voiced >= 2 {
                self.speaking = true;
                self.seg = self.pre.drain(..).collect();
                self.last_speech = self.seg.len();
                self.run = 0;
                self.voiced = 0;
                return VadEvent::Start;
            }
            return VadEvent::None;
        }

        self.seg.extend_from_slice(frame);
        if db > self.floor + KEEP_DB && db > ABS_DB {
            self.last_speech = self.seg.len();
        }
        let quiet = self.seg.len() - self.last_speech;
        let too_long = self.seg.len() >= ms(self.max_ms as usize);
        if quiet >= ms(self.silence_ms as usize) || too_long {
            let mut seg = std::mem::take(&mut self.seg);
            if !too_long {
                seg.truncate(self.last_speech + TAIL);
            }
            self.reset();
            return VadEvent::End(seg);
        }
        VadEvent::None
    }
}

// ---------------------------------------------------------------------------------------------
// MFCC
// ---------------------------------------------------------------------------------------------

const WIN: usize = 400; // 25 ms
const HOP: usize = 160; // 10 ms
const NFFT: usize = 512;
const NMEL: usize = 26;
const NCEP: usize = 13;
/// Mel enerjilerinin dinamik aralığı (dB).
const FLOOR_DB: f32 = 40.0;
/// MFCC + birinci türevler.
const DIM: usize = NCEP * 2;

struct Mfcc {
    window: [f32; WIN],
    cos: [f32; NFFT / 2],
    sin: [f32; NFFT / 2],
    rev: [u16; NFFT],
    /// Mel süzgeçleri: (ilk FFT kutusu, ağırlıklar).
    mel: Vec<(usize, Vec<f32>)>,
    dct: [[f32; NMEL]; NCEP],
}

fn mfcc() -> &'static Mfcc {
    static M: OnceLock<Mfcc> = OnceLock::new();
    M.get_or_init(|| {
        use std::f32::consts::PI;
        let mut m = Mfcc {
            window: [0.0; WIN],
            cos: [0.0; NFFT / 2],
            sin: [0.0; NFFT / 2],
            rev: [0; NFFT],
            mel: Vec::with_capacity(NMEL),
            dct: [[0.0; NMEL]; NCEP],
        };
        for (i, w) in m.window.iter_mut().enumerate() {
            *w = 0.54 - 0.46 * (2.0 * PI * i as f32 / (WIN - 1) as f32).cos();
        }
        for i in 0..NFFT / 2 {
            let a = -2.0 * PI * i as f32 / NFFT as f32;
            m.cos[i] = a.cos();
            m.sin[i] = a.sin();
        }
        let bits = NFFT.trailing_zeros();
        for (i, r) in m.rev.iter_mut().enumerate() {
            *r = ((i as u32).reverse_bits() >> (32 - bits)) as u16;
        }
        // 20 Hz – 7600 Hz arası üçgen süzgeçler.
        let hz2mel = |f: f32| 2595.0 * (1.0 + f / 700.0).log10();
        let mel2hz = |m: f32| 700.0 * (10f32.powf(m / 2595.0) - 1.0);
        let (lo, hi) = (hz2mel(20.0), hz2mel(7600.0));
        let edges: Vec<f32> = (0..NMEL + 2)
            .map(|i| mel2hz(lo + (hi - lo) * i as f32 / (NMEL + 1) as f32) * NFFT as f32 / RATE as f32)
            .collect();
        for k in 0..NMEL {
            let (l, c, r) = (edges[k], edges[k + 1], edges[k + 2]);
            let first = l.ceil() as usize;
            let last = (r.floor() as usize).min(NFFT / 2);
            let w: Vec<f32> = (first..=last)
                .map(|b| {
                    let b = b as f32;
                    if b <= c { (b - l) / (c - l) } else { (r - b) / (r - c) }.max(0.0)
                })
                .collect();
            m.mel.push((first, w));
        }
        for (i, row) in m.dct.iter_mut().enumerate() {
            for (j, d) in row.iter_mut().enumerate() {
                *d = (PI * i as f32 * (j as f32 + 0.5) / NMEL as f32).cos() * (2.0 / NMEL as f32).sqrt();
            }
        }
        m
    })
}

impl Mfcc {
    /// Yerinde, iteratif radix-2 FFT.
    fn fft(&self, re: &mut [f32; NFFT], im: &mut [f32; NFFT]) {
        for i in 0..NFFT {
            let j = self.rev[i] as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= NFFT {
            let half = len / 2;
            let step = NFFT / len;
            for start in (0..NFFT).step_by(len) {
                for k in 0..half {
                    let (c, s) = (self.cos[k * step], self.sin[k * step]);
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * c - im[b] * s;
                    let ti = re[b] * s + im[b] * c;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len *= 2;
        }
    }

    /// Ham MFCC'ler (çerçeve başına 13), normalleştirilmemiş.
    fn ceps(&self, pcm: &[i16]) -> Vec<[f32; NCEP]> {
        if pcm.len() < WIN {
            return Vec::new();
        }
        let frames = 1 + (pcm.len() - WIN) / HOP;
        // Ön vurgu.
        let mut x = Vec::with_capacity(pcm.len());
        let mut prev = pcm[0] as f32;
        for &s in pcm {
            let v = s as f32;
            x.push((v - 0.97 * prev) / 32768.0);
            prev = v;
        }
        let (mut re, mut im) = ([0f32; NFFT], [0f32; NFFT]);
        let mut mel = vec![[0f32; NMEL]; frames];
        let mut top = 0f32;
        for (f, row) in mel.iter_mut().enumerate() {
            let frame = &x[f * HOP..f * HOP + WIN];
            for i in 0..NFFT {
                re[i] = if i < WIN { frame[i] * self.window[i] } else { 0.0 };
                im[i] = 0.0;
            }
            self.fft(&mut re, &mut im);
            for (k, (first, w)) in self.mel.iter().enumerate() {
                let mut e = 0f32;
                for (i, wi) in w.iter().enumerate() {
                    let b = first + i;
                    e += wi * (re[b] * re[b] + im[b] * im[b]);
                }
                row[k] = e;
                top = top.max(e);
            }
        }
        // Dinamik aralık sınırı: söyleyişin en güçlü bandının FLOOR_DB altı kesilir. Sessiz odada
        // alınmış şablonla gürültülü odadaki söyleyiş arasındaki farkı (boş bantlar) azaltır.
        let floor = top * 10f32.powf(-FLOOR_DB / 10.0) + 1e-10;
        mel.iter()
            .map(|row| {
                let mut logmel = [0f32; NMEL];
                for (l, e) in logmel.iter_mut().zip(row) {
                    *l = (e + floor).ln();
                }
                let mut c = [0f32; NCEP];
                for (i, row) in self.dct.iter().enumerate() {
                    c[i] = row.iter().zip(&logmel).map(|(a, b)| a * b).sum();
                }
                c
            })
            .collect()
    }
}

/// MFCC + türevler (çerçeve × DIM, düz dizi).
fn features(pcm: &[i16]) -> Vec<f32> {
    let c = mfcc().ceps(pcm);
    let n = c.len();
    let mut out = vec![0f32; n * DIM];
    for t in 0..n {
        out[t * DIM..t * DIM + NCEP].copy_from_slice(&c[t]);
        // Türev: ±2 çerçeve doğrusal bağlanım.
        for d in 0..NCEP {
            let at = |k: isize| c[(t as isize + k).clamp(0, n as isize - 1) as usize][d];
            out[t * DIM + NCEP + d] = ((at(1) - at(-1)) + 2.0 * (at(2) - at(-2))) / 10.0;
        }
    }
    out
}

/// Söyleyiş başına ortalama ve varyans normalleştirmesi (mikrofon ve ses düzeyi farkı silinir).
fn cmvn(f: &[f32]) -> Vec<f32> {
    let n = f.len() / DIM;
    let mut out = f.to_vec();
    if n < 2 {
        return out;
    }
    for d in 0..DIM {
        let mean = (0..n).map(|t| f[t * DIM + d]).sum::<f32>() / n as f32;
        let var = (0..n).map(|t| (f[t * DIM + d] - mean).powi(2)).sum::<f32>() / n as f32;
        let inv = 1.0 / (var.sqrt() + 1e-3);
        for t in 0..n {
            out[t * DIM + d] = (f[t * DIM + d] - mean) * inv;
        }
    }
    out
}

/// Sakoe-Chiba kuşaklı, simetrik ağırlıklı DTW; yol uzunluğuna bölünmüş ortalama çerçeve uzaklığı.
fn dtw(a: &[f32], b: &[f32]) -> f32 {
    let (n, m) = (a.len() / DIM, b.len() / DIM);
    if n == 0 || m == 0 {
        return f32::MAX;
    }
    let w = (n.max(m) * 15 / 100).max(4);
    let dist = |i: usize, j: usize| -> f32 {
        let (x, y) = (&a[i * DIM..(i + 1) * DIM], &b[j * DIM..(j + 1) * DIM]);
        x.iter().zip(y).map(|(p, q)| (p - q) * (p - q)).sum::<f32>().sqrt()
    };
    let mut prev = vec![f32::MAX; m];
    let mut cur = vec![f32::MAX; m];
    for i in 0..n {
        let c = if n > 1 { i * (m - 1) / (n - 1) } else { 0 };
        let (lo, hi) = (c.saturating_sub(w), (c + w).min(m - 1));
        cur.fill(f32::MAX);
        for j in lo..=hi {
            let d = dist(i, j);
            // Çapraz adım 2·d, yatay/dikey adım d sayılır (simetrik ağırlık); ilk hücre 2·d.
            let best = if i == 0 && j == 0 {
                d
            } else {
                let mut v = f32::MAX;
                if i > 0 && j > 0 && prev[j - 1] < f32::MAX {
                    v = v.min(prev[j - 1] + d);
                }
                if i > 0 && prev[j] < f32::MAX {
                    v = v.min(prev[j]);
                }
                if j > 0 && cur[j - 1] < f32::MAX {
                    v = v.min(cur[j - 1]);
                }
                v
            };
            if best < f32::MAX {
                cur[j] = best + d;
            }
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    if prev[m - 1] == f32::MAX { f32::MAX } else { prev[m - 1] / (n + m) as f32 }
}

/// Ortalama çerçeve uzaklığından 0..1 benzerlik.
fn to_similarity(d: f32) -> f32 {
    if !d.is_finite() || d == f32::MAX {
        return 0.0;
    }
    (-(d / D_SCALE).powi(2)).exp()
}
const D_SCALE: f32 = 4.5;

/// 10 ms bloklarda enerji (dB).
fn block_db(pcm: &[i16]) -> Vec<f32> {
    pcm.chunks(HOP)
        .map(|b| 10.0 * (b.iter().map(|&s| (s as f32).powi(2)).sum::<f32>() / b.len() as f32 + 1.0).log10())
        .collect()
}

/// Bölütün baştaki ve sondaki sessizliğini kırpar (kenarlarda 30–40 ms pay bırakır);
/// (kırpılmış aralık, paysız ses süresi örnek olarak, en yüksek blok dB'si, gürültü dB'si).
fn trim(pcm: &[i16]) -> (&[i16], usize, f32, f32) {
    let db = block_db(pcm);
    if db.is_empty() {
        return (pcm, 0, 0.0, 0.0);
    }
    let mut sorted = db.clone();
    sorted.sort_by(f32::total_cmp);
    let noise = sorted[sorted.len() / 10];
    let peak = sorted[sorted.len() - 1];
    let thr = noise + (0.25 * (peak - noise)).max(6.0);
    let Some(first) = db.iter().position(|&e| e > thr) else { return (&pcm[..0], 0, peak, noise) };
    let last = db.iter().rposition(|&e| e > thr).unwrap_or(first);
    let a = first.saturating_sub(3) * HOP;
    let b = ((last + 4) * HOP).min(pcm.len());
    (&pcm[a..b], (last + 1 - first) * HOP, peak, noise)
}

// ---------------------------------------------------------------------------------------------
// Şablon eşleme
// ---------------------------------------------------------------------------------------------

struct Template {
    /// Kırpılmış ham ses (öznitelikler değişirse yeniden hesaplanabilsin diye saklanır).
    pcm: Vec<i16>,
    feat: Vec<f32>,
}

impl Template {
    fn new(pcm: Vec<i16>) -> Self {
        let feat = cmvn(&features(&pcm));
        Self { pcm, feat }
    }

    fn frames(&self) -> usize {
        self.feat.len() / DIM
    }
}

/// Kullanıcının kendi sesinden öğrenilen uyandırma sözü: MFCC + DTW şablon eşleme.
pub struct Spotter {
    templates: Vec<Template>,
    thr: f32,
}

const MIN_LEN: usize = ms(250);
const MAX_LEN: usize = ms(2500);
/// En yüksek 10 ms bloğun RMS'i bundan düşükse (≈ −47 dBFS) örnek fazla sessiz.
const MIN_PEAK_DB: f32 = 43.5;
/// Tek şablonla eşik; iki ve daha fazlasında şablonların birbirine benzerliğinden türetilir.
const DEFAULT_THR: f32 = 0.6;
const THR_FACTOR: f32 = 0.8;
const FILE_PREFIX: &str = "uyandirma-";

impl Default for Spotter {
    fn default() -> Self {
        Self::new()
    }
}

impl Spotter {
    pub fn new() -> Self {
        Self { templates: Vec::new(), thr: DEFAULT_THR }
    }

    /// `dir` içindeki şablonları (`uyandirma-1.wav` …, 16 kHz mono) yükler. Klasör yoksa şablon yok.
    pub fn load(dir: &Path) -> Self {
        let mut s = Self::new();
        let Ok(rd) = std::fs::read_dir(dir) else { return s };
        let mut files: Vec<(u32, std::path::PathBuf)> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let n = name.strip_prefix(FILE_PREFIX)?.strip_suffix(".wav")?.parse().ok()?;
                Some((n, e.path()))
            })
            .collect();
        files.sort();
        for (_, path) in files {
            match std::fs::read(&path).ok().and_then(|b| read_wav(&b)) {
                Some(pcm) if pcm.len() >= WIN * 2 => s.templates.push(Template::new(pcm)),
                _ => crate::log!("uyandırma şablonu okunamadı: {}", path.display()),
            }
        }
        s.update_threshold();
        s
    }

    /// Şablonları `dir`'e WAV olarak yazar (eskileri silinir).
    pub fn save(&self, dir: &Path) -> Res<()> {
        std::fs::create_dir_all(dir)?;
        for e in std::fs::read_dir(dir)?.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(FILE_PREFIX) && name.ends_with(".wav") {
                std::fs::remove_file(e.path())?;
            }
        }
        for (i, t) in self.templates.iter().enumerate() {
            std::fs::write(dir.join(format!("{FILE_PREFIX}{}.wav", i + 1)), write_wav(&t.pcm, RATE as u32))?;
        }
        Ok(())
    }

    pub fn clear(&mut self) {
        self.templates.clear();
        self.update_threshold();
    }

    /// Bir örnek ekler (VAD'in uyandırma sözü için verdiği bölüt). Örnek kullanışsızsa (kırpıldıktan
    /// sonra 250 ms'den kısa, 2,5 s'den uzun ya da çok sessiz) `false`.
    pub fn add(&mut self, pcm: &[i16]) -> bool {
        let (cut, core, peak, noise) = trim(pcm);
        if core < MIN_LEN || core > MAX_LEN || peak < MIN_PEAK_DB || peak - noise < 8.0 {
            return false;
        }
        self.templates.push(Template::new(cut.to_vec()));
        self.update_threshold();
        true
    }

    pub fn count(&self) -> usize {
        self.templates.len()
    }

    /// Segment şablonlara ne kadar benziyor: 0..1 (1 = aynı). Şablon yoksa `None`.
    pub fn similarity(&self, pcm: &[i16]) -> Option<f32> {
        if self.templates.is_empty() {
            return None;
        }
        let (cut, _, _, _) = trim(pcm);
        let raw = features(cut);
        let whole = cmvn(&raw);
        let sims: Vec<f32> = self.templates.iter().map(|t| match_one(&raw, &whole, t)).collect();
        Some(top_mean(sims))
    }

    /// Eşik: `similarity >= threshold` ise uyandırma sözü sayılır. Şablonların birbirine
    /// benzerliğinden türetilir.
    /// Bulutta doğrulanmış bir uyandırma örneğini ekler; `max`'ı aşarsa en eski örnek çıkar
    /// (kullandıkça kullanıcının gerçek sesine uyum sağlar).
    pub fn adapt(&mut self, pcm: &[i16], max: usize) -> bool {
        if !self.add(pcm) {
            return false;
        }
        while self.templates.len() > max.max(1) {
            self.templates.remove(0);
        }
        self.update_threshold();
        true
    }

    pub fn threshold(&self) -> f32 {
        self.thr
    }

    /// Her şablonun ötekilere benzerliği (birini dışarıda bırakarak); en zayıfının biraz altı eşik.
    fn update_threshold(&mut self) {
        let n = self.templates.len();
        if n < 2 {
            self.thr = DEFAULT_THR;
            return;
        }
        let mut worst = f32::MAX;
        for (i, t) in self.templates.iter().enumerate() {
            let raw = features(&t.pcm);
            let sims: Vec<f32> = self
                .templates
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, o)| match_one(&raw, &t.feat, o))
                .collect();
            worst = worst.min(top_mean(sims));
        }
        self.thr = (worst * THR_FACTOR).clamp(0.4, 0.72);
    }
}

/// En iyi iki benzerliğin ortalaması (tek bir şanslı şablon yetmesin).
fn top_mean(mut v: Vec<f32>) -> f32 {
    v.sort_by(|a, b| b.total_cmp(a));
    let k = v.len().min(2);
    if k == 0 { 0.0 } else { v[..k].iter().sum::<f32>() / k as f32 }
}

/// Aday bir şablona ne kadar benziyor. Aday şablondan belirgin uzunsa ("hey matrix saat kaç"
/// tek nefeste) baştaki birkaç uzunlukta parçası da denenir.
fn match_one(raw: &[f32], whole: &[f32], t: &Template) -> f32 {
    let (m, n) = (raw.len() / DIM, t.frames());
    if n == 0 || m * 2 < n {
        return 0.0;
    }
    let mut best = dtw(whole, &t.feat);
    if m * 100 > n * 135 {
        for pct in [80, 95, 110, 125, 140] {
            let len = n * pct / 100;
            if len < m {
                best = best.min(dtw(&cmvn(&raw[..len * DIM]), &t.feat));
            }
        }
    }
    to_similarity(best)
}

// ---------------------------------------------------------------------------------------------
// WAV
// ---------------------------------------------------------------------------------------------

/// 16 bit mono WAV.
pub fn write_wav(pcm: &[i16], rate: u32) -> Vec<u8> {
    let data = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// 16 kHz, 16 bit PCM WAV'ı okur (çok kanallıysa ilk kanal). Başka biçimse `None`.
pub fn read_wav(b: &[u8]) -> Option<Vec<i16>> {
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let (mut pos, mut channels) = (12, 0usize);
    while pos + 8 <= b.len() {
        let (id, size) = (&b[pos..pos + 4], u32_at(pos + 4) as usize);
        let body = pos + 8;
        if id == b"fmt " && body + 16 <= b.len() {
            let (format, ch, rate, bits) = (u16_at(body), u16_at(body + 2), u32_at(body + 4), u16_at(body + 14));
            if !(format == 1 || format == 0xFFFE) || rate != RATE as u32 || bits != 16 || ch == 0 {
                return None;
            }
            channels = ch as usize;
        } else if id == b"data" && channels > 0 {
            let end = body.saturating_add(size).min(b.len());
            return Some(b[body..end].chunks_exact(2 * channels).map(|c| i16::from_le_bytes([c[0], c[1]])).collect());
        }
        pos = body.saturating_add(size).saturating_add(size & 1);
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Metin
// ---------------------------------------------------------------------------------------------

/// Türkçe kurallarla küçük harf (İ→i, I→ı), sonra ASCII iskelet (ı→i, ş→s, ç→c, ğ→g, ö→o, ü→u …).
fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        // İ'nin Unicode küçültmesi "i + birleşik nokta" olur; I→ı da sonunda i'ye katlanır.
        let lower = if matches!(ch, 'İ' | 'I') { 'i'.to_lowercase() } else { ch.to_lowercase() };
        for c in lower {
            out.push(match c {
                '\u{307}' => continue,
                'ı' | 'î' | 'ï' | 'í' | 'ì' => 'i',
                'ş' | 'ș' => 's',
                'ç' => 'c',
                'ğ' => 'g',
                'ö' | 'ó' | 'ò' | 'ô' => 'o',
                'ü' | 'û' | 'ú' | 'ù' => 'u',
                'â' | 'á' | 'à' | 'ä' => 'a',
                'é' | 'è' | 'ê' | 'ë' => 'e',
                _ => c,
            });
        }
    }
    out
}

/// Sözcükler: (başlangıç, bitiş bayt konumu, katlanmış biçim). Kesme işaretinden sonraki ek
/// sözcüğe dahildir ama katlanmış biçimden atılır ("Matrix'e" → "matrix").
fn tokens(s: &str) -> Vec<(usize, usize, String)> {
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let is_apos = |c: char| matches!(c, '\'' | '’' | '‘' | '`');
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].1.is_alphanumeric() {
            i += 1;
            continue;
        }
        let start = chars[i].0;
        let mut cut = None;
        while i < chars.len() {
            let c = chars[i].1;
            if c.is_alphanumeric() {
                i += 1;
            } else if is_apos(c) && i + 1 < chars.len() && chars[i + 1].1.is_alphanumeric() {
                cut.get_or_insert(chars[i].0);
                i += 1;
            } else {
                break;
            }
        }
        let end = chars.get(i).map_or(s.len(), |c| c.0);
        out.push((start, end, fold(&s[start..cut.unwrap_or(end)])));
    }
    out
}

/// Yazım farklarını siler: x→ks, w→v, q→k, ph→f, ck→k, çift harfler teke.
fn phonetic(t: &str) -> Vec<char> {
    let s = t.replace("ph", "f").replace("ck", "k").replace('x', "ks").replace('w', "v").replace('q', "k");
    let mut out: Vec<char> = Vec::with_capacity(s.len());
    for c in s.chars() {
        if out.last() != Some(&c) {
            out.push(c);
        }
    }
    out
}

fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + (ca != cb) as usize).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Uzunluğa göre izin verilen düzeltme sayısı.
fn tolerance(len: usize) -> usize {
    match len {
        0..=2 => 0,
        3..=5 => 1,
        6..=8 => 2,
        _ => 3,
    }
}

/// Metin uyandırma sözünü içeriyor mu: Türkçe/İngilizce yazım farklarına toleranslı ("Hey Matrix.",
/// "hey matriks", "hay matrix", "Hey, Matrix!", "ey matrix" hepsi "hey matrix" sayılır). Sözün
/// başındaki kısa selamlama ("hey", "ok") atlanabilir; atlanırsa geri kalanı (okunuş düzeltmesinden
/// sonra) birebir eşleşmeli: "Matrix, saat kaç" eşleşir, "matris çarpımı nedir" eşleşmez. Söz metnin
/// başında olmalı (önünde en fazla bir kısa dolgu: "ee", "şey"). Eşleşirse sözden sonra kalan metin
/// (boş olabilir), yoksa `None`.
pub fn match_wake(transcript: &str, wake: &str) -> Option<String> {
    let wake: Vec<Vec<char>> = tokens(wake).iter().map(|t| phonetic(&t.2)).collect();
    if wake.is_empty() {
        return None;
    }
    let toks = tokens(transcript);
    let keys: Vec<Vec<char>> = toks.iter().map(|t| phonetic(&t.2)).collect();
    // Baştaki kısa selamlamalar atlanabilir (ama hepsi değil).
    let optional = wake.iter().take_while(|w| w.len() <= 3).count().min(wake.len() - 1);
    let rest = |last: usize| -> String {
        transcript[toks[last].1..].trim_start_matches(|c: char| !c.is_alphanumeric()).trim_end().to_string()
    };
    let seq = |from: usize, words: &[Vec<char>], strict: bool| -> Option<usize> {
        if from + words.len() > keys.len() {
            return None;
        }
        for (k, w) in words.iter().enumerate() {
            let tol = if strict { tolerance(w.len()).saturating_sub(2) } else { tolerance(w.len()) };
            if edit_distance(&keys[from + k], w) > tol {
                return None;
            }
        }
        Some(from + words.len() - 1)
    };
    let joined: Vec<char> = wake.concat();
    for start in 0..keys.len().min(2) {
        if start == 1 && keys[0].len() > 4 {
            break;
        }
        if let Some(last) = seq(start, &wake, false) {
            return Some(rest(last));
        }
        if optional > 0
            && let Some(last) = seq(start, &wake[optional..], true)
        {
            return Some(rest(last));
        }
        // "heymatrix" tek sözcük olarak yazılmışsa.
        if wake.len() > 1 && edit_distance(&keys[start], &joined) <= tolerance(joined.len()) {
            return Some(rest(start));
        }
    }
    None
}

/// Evet/hayır cevabı: "evet", "tamam", "olur", "çalıştır" … → `Some(true)`; "hayır", "yok",
/// "vazgeç", "iptal" … → `Some(false)`; ikisi de değilse `None`. Hem olumlu hem olumsuz sözcük
/// varsa olumsuz kazanır (yanlışlıkla onaylamaktansa yeniden sormak iyidir).
pub fn yes_no(transcript: &str) -> Option<bool> {
    const YES: &[&str] = &[
        "evet",
        "tamam",
        "tamamdir",
        "olur",
        "olsun",
        "calistir",
        "yap",
        "aynen",
        "onayliyorum",
        "onayla",
        "onay",
        "tabii",
        "tabi",
        "elbette",
        "kesinlikle",
        "devam",
        "peki",
        "yes",
        "yeah",
        "yep",
        "ok",
        "okay",
        "okey",
        "oke",
        "sure",
    ];
    const NO: &[&str] = &[
        "hayir",
        "yok",
        "vazgec",
        "vazgectim",
        "iptal",
        "dur",
        "durdur",
        "yapma",
        "calistirma",
        "olmaz",
        "istemiyorum",
        "degil",
        "degilim",
        "bosver",
        "no",
        "nope",
        "cancel",
        "stop",
    ];
    let mut yes = false;
    for (_, _, t) in tokens(transcript) {
        if NO.contains(&t.as_str()) {
            return Some(false);
        }
        yes |= YES.contains(&t.as_str());
    }
    yes.then_some(true)
}
