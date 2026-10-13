//! Işık katmanı: uyandırma sözü duyulunca ekranın dört kenarı akan, çok renkli bir ışıkla yanar
//! (iOS 18 Siri gibi); üstte ortada küçük bir hap ("Dynamic Island") durumu, canlı yazıyı, cevabı
//! ve gerekirse onay düğmelerini gösterir. Hepsi arayüz iş parçacığında yaşar: görünürken ~60 kare/sn
//! zamanlayıcıyla çizilir, gizliyken ne zamanlayıcı ne pencere kalır.
//!
//! Kenar ışığı dört şerit penceredir (üst/alt tam genişlik, sol/sağ aralarında). Pikselleri
//! işlemci hesaplar: kenar boyunca renk × içeri doğru sönüm (ayrışır, SSE ile ucuz); köşelerde iki
//! kenarın ışığı üst üste biner. Hap Direct2D + DirectWrite ile DIB'e çizilir. Hepsi
//! UpdateLayeredWindow ile piksel başına saydam, tıklamayı geçirir (onay dışında), odak almaz.
#![allow(dead_code)]

use std::arch::x86_64::*;
use std::cell::RefCell;
use std::time::Instant;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};
use windows_numerics::{Matrix3x2, Vector2};

use crate::log;
use crate::util::Res;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Hidden,
    Listening,
    Thinking,
    Speaking,
    Working,
    Confirm,
}

/// Işığın renkleri: turuncu, pembe, mor, mavi, camgöbeği; çevre boyunca döner.
const PALETTE: [u32; 5] = [0xFF8A4C, 0xFF4D8D, 0xB061FF, 0x4D8DFF, 0x2EE6D6];
const ACCENT: u32 = 0xd97757;
const TEXT: u32 = 0xfaf9f5;
const CLASS: PCWSTR = w!("engram-isik");
const TIMER: usize = 1;
/// 15 ms: 15.6 ms'lik sistem tıkında her tıkta bir kare (~64/sn), 1 ms'de ~66/sn.
const FRAME_MS: u32 = 15;
/// Işığın içeri en çok uzandığı yer (DIP); şeritler bu kalınlıkta.
const GLOW_MAX: f32 = 136.0;
/// Hap: en geniş, en yüksek (gövde 3 satır + düğmeler), gölge payı, çalışma alanının tepesine uzaklık.
const PILL_MAX_W: f32 = 640.0;
const PILL_MAX_H: f32 = 200.0;
const MARGIN: f32 = 30.0;
const PILL_TOP: f32 = 14.0;
/// Hap içi ölçüler (DIP): dalga çubuklarının solu, yazının solu, sağ boşluk, gövdenin tepesi.
const PAD_L: f32 = 15.0;
const TEXT_X: f32 = 50.0;
const PAD_R: f32 = 20.0;
const BODY_Y: f32 = 35.0;
const BODY_LINES: usize = 3;
const BTN_H: f32 = 30.0;
/// Kenar renklerinin örnek aralığı (piksel); arası doğrusal.
const STEP: f32 = 16.0;
/// Hale alfasının renge oranı (<1: toplamsal ışık gibi).
const LIGHT_ALPHA: f32 = 0.62;

// ---------------------------------------------------------------- dış arayüz

/// Onay düğmelerine basılınca `hwnd`'a `msg` gönderilir: wParam 1 = evet, 0 = hayır.
pub fn init(hwnd: HWND, msg: u32) {
    STATE.with_borrow_mut(|st| {
        if let Some(st) = st {
            (st.notify, st.msg) = (hwnd, msg);
            return;
        }
        match State::new(hwnd, msg) {
            Ok(s) => *st = Some(s),
            Err(e) => log!("ışık katmanı kurulamadı: {e}"),
        }
    });
}

/// Durumu değiştirir. `title`: hapta kısa durum adı; `text`: gövde (canlı yazı, akan cevap ya da
/// onayda komut). Sık çağrılabilir: düzen bir sonraki karede, gerekirse bir kez kurulur.
pub fn set(mode: Mode, title: &str, text: &str) {
    with_state(|st| st.set(mode, title, text));
}

/// Ses seviyesi 0..1 (dinlerken mikrofon, konuşurken çıkış); ~50/sn çağrılır.
pub fn level(v: f32) {
    with_state(|st| st.level_in = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 });
}

pub fn mode() -> Mode {
    STATE.with(|c| c.try_borrow().ok().and_then(|s| s.as_ref().map(|s| s.mode)).unwrap_or(Mode::Hidden))
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Pencere işlevi iç içe çağrılabilir (SetWindowPos, DestroyWindow iletileri): o zaman durum
/// zaten ödünçtedir, ileti varsayılana gider.
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|c| c.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

// ---------------------------------------------------------------- yardımcılar

const fn rgbf(c: u32) -> [f32; 3] {
    [((c >> 16) & 0xff) as f32 / 255.0, ((c >> 8) & 0xff) as f32 / 255.0, (c & 0xff) as f32 / 255.0]
}

const PAL: [[f32; 3]; 5] = [rgbf(PALETTE[0]), rgbf(PALETTE[1]), rgbf(PALETTE[2]), rgbf(PALETTE[3]), rgbf(PALETTE[4])];

/// Döngüsel paletten yumuşak geçişli renk; `u` 1'de başa döner.
fn palette(u: f32) -> [f32; 3] {
    let x = u.rem_euclid(1.0) * 5.0;
    let i = (x as usize).min(4);
    let f = x - i as f32;
    let f = f * f * (3.0 - 2.0 * f);
    let (a, b) = (PAL[i], PAL[(i + 1) % 5]);
    [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f]
}

fn color(c: [f32; 3], a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c[0], g: c[1], b: c[2], a }
}

fn hex(c: u32, a: f32) -> D2D1_COLOR_F {
    color(rgbf(c), a)
}

fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// Hafif taşan yumuşama: hap düşerken yerine "oturur".
fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0) - 1.0;
    let c = 1.4;
    1.0 + (c + 1.0) * t * t * t + c * t * t
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

fn rrect(r: D2D_RECT_F, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }
}

/// Yaylı geçiş: hap boyu hedefe hafif esneyerek varır.
#[derive(Clone, Copy)]
struct Spring {
    x: f32,
    v: f32,
}

impl Spring {
    fn step(&mut self, target: f32, dt: f32) {
        const K: f32 = 340.0;
        let c = 2.0 * 0.8 * K.sqrt();
        let n = (dt / (1.0 / 240.0)).ceil().max(1.0);
        let h = dt / n;
        for _ in 0..n as usize {
            self.v += (K * (target - self.x) - c * self.v) * h;
            self.x += self.v * h;
        }
    }
}

// ---------------------------------------------------------------- katmanlı pencere

/// Piksel başına saydam, odak almayan, en üstteki pencere ve çizildiği 32 bit DIB.
struct Layered {
    hwnd: HWND,
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    w: i32,
    h: i32,
    shown: bool,
}

impl Layered {
    fn new(w: i32, h: i32, capture: bool) -> Res<Self> {
        unsafe {
            let inst = GetModuleHandleW(None)?;
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
                CLASS,
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                w,
                h,
                None,
                None,
                Some(inst.into()),
                None,
            )?;
            // Asistanın kendi ekran görüntülerine girmesin (deneme için açılabilir).
            if !capture {
                let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
            }
            let mut me = Self {
                hwnd,
                dc: CreateCompatibleDC(None),
                bmp: HBITMAP::default(),
                old: HGDIOBJ::default(),
                bits: std::ptr::null_mut(),
                w,
                h,
                shown: false,
            };
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            me.bmp = CreateDIBSection(Some(me.dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            me.bits = bits.cast();
            me.old = SelectObject(me.dc, me.bmp.into());
            me.pixels().fill(0);
            Ok(me)
        }
    }

    fn pixels(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }
    }

    /// DIB'in sol üst `w×h`'sini pencereye basar; pencere `x, y`'ye taşınır, `w×h` olur. `dirty`
    /// verilirse yalnız o bölge yenilenir.
    fn present(&mut self, x: i32, y: i32, w: i32, h: i32, dirty: Option<RECT>) {
        let pos = POINT { x, y };
        let size = SIZE { cx: w.clamp(1, self.w), cy: h.clamp(1, self.h) };
        let src = POINT::default();
        let blend =
            BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        let info = UPDATELAYEREDWINDOWINFO {
            cbSize: size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
            pptDst: &pos,
            psize: &size,
            hdcSrc: self.dc,
            pptSrc: &src,
            pblend: &blend,
            dwFlags: ULW_ALPHA,
            prcDirty: dirty.as_ref().filter(|_| self.shown).map_or(std::ptr::null(), |r| r as *const RECT),
            ..Default::default()
        };
        unsafe {
            let _ = UpdateLayeredWindowIndirect(self.hwnd, &info);
            if !self.shown {
                self.shown = true;
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }

    /// Tıklamayı geçirsin mi (onay sırasında hap tıklanır).
    fn set_click_through(&self, on: bool) {
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let t = WS_EX_TRANSPARENT.0 as isize;
            let new = if on { ex | t } else { ex & !t };
            if new != ex {
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, new);
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
        }
    }
}

impl Drop for Layered {
    fn drop(&mut self) {
        unsafe {
            if !self.old.is_invalid() {
                SelectObject(self.dc, self.old);
            }
            if !self.bmp.is_invalid() {
                let _ = DeleteObject(self.bmp.into());
            }
            let _ = DeleteDC(self.dc);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

// ---------------------------------------------------------------- kenar ışığı

/// Bir kenar boyunca her pikselin çekirdek (ince beyazımsı çizgi) ve hale rengi; önçarpımlı,
/// 0..255, kanal sırası B G R A (DIB'in bayt sırası).
#[derive(Default)]
struct Edge {
    core: Vec<[f32; 4]>,
    glow: Vec<[f32; 4]>,
    /// Aynısı 8.8 sabit noktada (toplu doldurma için).
    core16: Vec<[u16; 4]>,
    glow16: Vec<[u16; 4]>,
}

/// Bir karenin ışık ayarları.
struct GlowFrame {
    /// İçeri uzanım (DIP), hale ve çekirdek gücü.
    ext: f32,
    amp: f32,
    core: f32,
    /// Renk dönüşü (tur), parlaklık dalgalarının akışı, "çalışıyor" vurgusunun gücü ve konumu.
    phase: f32,
    flow: f32,
    work: f32,
    work_pos: f32,
}

/// 4×4 Bayer eşikleri: geniş, yumuşak geçişlerde 8 bitin basamaklanmasını gizler.
const BAYER: [[f32; 4]; 4] = {
    let m = [[0.0, 8.0, 2.0, 10.0], [12.0, 4.0, 14.0, 6.0], [3.0, 11.0, 1.0, 9.0], [15.0, 7.0, 13.0, 5.0]];
    let mut out = [[0.0f32; 4]; 4];
    let mut i = 0;
    while i < 4 {
        let mut j = 0;
        while j < 4 {
            out[i][j] = (m[i][j] + 0.5) / 16.0;
            j += 1;
        }
        i += 1;
    }
    out
};

struct Glow {
    /// Üst, alt, sol, sağ.
    strips: [Layered; 4],
    /// Ekran (piksel).
    x: i32,
    y: i32,
    w: usize,
    h: usize,
    /// Şerit kalınlığı (piksel).
    t: usize,
    s: f32,
    /// Üst, alt, sol, sağ kenarın renkleri (ekran koordinatıyla dizinli).
    edges: [Edge; 4],
    ring: Vec<[[f32; 4]; 2]>,
    fc: Vec<f32>,
    fg: Vec<f32>,
    /// Sönümün dört kanala yayılmış 0..65535 hali; `_r` sağ şerit için ters sırada.
    fc4: Vec<[u16; 4]>,
    fg4: Vec<[u16; 4]>,
    fc4_r: Vec<[u16; 4]>,
    fg4_r: Vec<[u16; 4]>,
    /// Önceki karede ışığın uzandığı piksel: küçülünce artakalan silinir.
    prev: usize,
}

#[inline(always)]
unsafe fn mad(c: &[f32; 4], fc: __m128, g: &[f32; 4], fg: __m128) -> __m128 {
    unsafe { _mm_add_ps(_mm_mul_ps(_mm_loadu_ps(c.as_ptr()), fc), _mm_mul_ps(_mm_loadu_ps(g.as_ptr()), fg)) }
}

/// Önçarpımlı "üstüne": a + b·(1 − a.alfa).
#[inline(always)]
unsafe fn over(a: __m128, b: __m128) -> __m128 {
    unsafe {
        let aa = _mm_shuffle_ps::<0xFF>(a, a);
        let k = _mm_max_ps(_mm_sub_ps(_mm_set1_ps(1.0), _mm_mul_ps(aa, _mm_set1_ps(1.0 / 255.0))), _mm_setzero_ps());
        _mm_add_ps(a, _mm_mul_ps(b, k))
    }
}

/// Dört kanalı eşikle yuvarlayıp tamsayıya çevirir (0..255).
#[inline(always)]
unsafe fn quant(v: __m128, th: __m128) -> __m128i {
    unsafe { _mm_cvttps_epi32(_mm_min_ps(_mm_add_ps(v, th), _mm_set1_ps(255.0))) }
}

#[inline(always)]
unsafe fn pack1(v: __m128, th: __m128) -> u32 {
    unsafe {
        let i = quant(v, th);
        let p = _mm_packs_epi32(i, i);
        _mm_cvtsi128_si32(_mm_packus_epi16(p, p)) as u32
    }
}

impl Glow {
    fn new(mon: RECT, s: f32, capture: bool) -> Res<Self> {
        let (w, h) = ((mon.right - mon.left) as usize, (mon.bottom - mon.top) as usize);
        let t = ((GLOW_MAX * s).ceil() as usize).min(h / 2).min(w / 2).max(4);
        let (wi, hi, ti) = (w as i32, h as i32, t as i32);
        let strips = [
            Layered::new(wi, ti, capture)?,
            Layered::new(wi, ti, capture)?,
            Layered::new(ti, (hi - 2 * ti).max(1), capture)?,
            Layered::new(ti, (hi - 2 * ti).max(1), capture)?,
        ];
        let mut edges: [Edge; 4] = Default::default();
        for (e, n) in edges.iter_mut().zip([w, w, h, h]) {
            e.core = vec![[0.0; 4]; n];
            e.glow = vec![[0.0; 4]; n];
            e.core16 = vec![[0; 4]; n];
            e.glow16 = vec![[0; 4]; n];
        }
        Ok(Self { strips, x: mon.left, y: mon.top, w, h, t, s, edges, ring: Vec::new(), fc: vec![0.0; t], fg: vec![0.0; t],
            fc4: vec![[0; 4]; t], fg4: vec![[0; 4]; t], fc4_r: vec![[0; 4]; t], fg4_r: vec![[0; 4]; t], prev: 0 })
    }

    fn render(&mut self, f: &GlowFrame) {
        let (w, h, t, s) = (self.w, self.h, self.t, self.s);
        let p = (2 * (w + h)) as f32;

        // Çevre boyunca seyrek örnekler: renk, parlaklık dalgası, "çalışıyor" vurgusu.
        let n = (p / STEP).ceil() as usize + 2;
        self.ring.resize(n, [[0.0; 4]; 2]);
        let tau = std::f32::consts::TAU;
        for (k, out) in self.ring.iter_mut().enumerate() {
            let u = k as f32 * STEP / p;
            let col = palette(u + f.phase);
            let m = 0.70
                + 0.14 * (tau * (2.0 * u + 0.11 * f.flow)).sin()
                + 0.10 * (tau * (3.0 * u - 0.07 * f.flow) + 1.3).sin()
                + 0.06 * (tau * (5.0 * u + 0.05 * f.flow) + 0.4).sin();
            let mut hl = 0.0;
            if f.work > 0.001 {
                for off in [0.0, 0.5] {
                    let d = ((u - f.work_pos - off).rem_euclid(1.0) - 0.5).abs() - 0.5;
                    let d = d * p / (s * 240.0);
                    hl += (-d * d).exp();
                }
            }
            let ga = f.amp * m + f.work * hl * 0.85;
            let ca = f.core * (0.55 + 0.45 * m) + f.work * hl * 0.6;
            let white = mix3(col, [1.0; 3], 0.55);
            // Alfa renkten düşük: ışık koyu zeminde boya gibi değil, ışık gibi (toplamsal) parlar.
            out[0] = [white[2] * ca * 255.0, white[1] * ca * 255.0, white[0] * ca * 255.0, ca * 255.0 * 0.85];
            out[1] = [col[2] * ga * 255.0, col[1] * ga * 255.0, col[0] * ga * 255.0, ga * 255.0 * LIGHT_ALPHA];
        }
        let ring = &self.ring;
        let sample = |e: &mut Edge, s0: f32, dir: f32| {
            let (k0, dk) = (s0 / STEP, dir / STEP);
            for i in 0..e.core.len() {
                let k = (k0 + dk * i as f32).max(0.0);
                let i0 = (k as usize).min(n - 2);
                let fr = k - i0 as f32;
                let (a, b) = (&ring[i0], &ring[i0 + 1]);
                for c in 0..4 {
                    let (cv, gv) = (a[0][c] + (b[0][c] - a[0][c]) * fr, a[1][c] + (b[1][c] - a[1][c]) * fr);
                    e.core[i][c] = cv;
                    e.glow[i][c] = gv;
                    e.core16[i][c] = (cv * 256.0).min(65535.0) as u16;
                    e.glow16[i][c] = (gv * 256.0).min(65535.0) as u16;
                }
            }
        };
        // Saat yönünde yay uzunluğu: üst x, sağ W+y, alt 2W+H−1−x, sol 2W+2H−1−y.
        let (wf, hf) = (w as f32, h as f32);
        sample(&mut self.edges[0], 0.0, 1.0);
        sample(&mut self.edges[1], 2.0 * wf + hf - 1.0, -1.0);
        sample(&mut self.edges[2], 2.0 * wf + 2.0 * hf - 1.0, -1.0);
        sample(&mut self.edges[3], wf, 1.0);

        // İçeri doğru sönüm: ince parlak çekirdek + geniş yumuşak hale (kısa destekli).
        let e = (f.ext * s).max(1.0);
        let core_px = 1.4 * s.sqrt();
        let act = ((e.ceil() as usize) + 1).max((core_px * 3.0) as usize + 2).min(t);
        for d in 0..t {
            if d >= act {
                self.fc[d] = 0.0;
                self.fg[d] = 0.0;
                continue;
            }
            let x = d as f32 + 0.5;
            self.fc[d] = 1.0 - smoothstep(core_px, core_px * 2.6, x);
            let r = x / e;
            let win = if r < 1.0 { (1.0 - r * r) * (1.0 - r * r) } else { 0.0 };
            let g = 0.6 * (-(r / 0.55) * (r / 0.55)).exp() + 0.4 * (-r / 0.18).exp();
            self.fg[d] = win * g;
        }
        for d in 0..t {
            let (c, g) = ((self.fc[d] * 65535.0) as u16, (self.fg[d] * 65535.0) as u16);
            self.fc4[d] = [c; 4];
            self.fg4[d] = [g; 4];
            self.fc4_r[t - 1 - d] = [c; 4];
            self.fg4_r[t - 1 - d] = [g; 4];
        }
        let rows = act.max(self.prev);
        unsafe { self.fill(act, rows) };
        self.prev = act;

        let (x, y, wi, hi, ti, ri) = (self.x, self.y, w as i32, h as i32, t as i32, rows as i32);
        let side = (hi - 2 * ti).max(1);
        let [top, bottom, left, right] = &mut self.strips;
        // Üst/alt şeritte köşeler her satırda değişir: bütünü basılır.
        top.present(x, y, wi, ti, None);
        bottom.present(x, y + hi - ti, wi, ti, None);
        left.present(x, y + ti, ti, side, Some(RECT { left: 0, top: 0, right: ri, bottom: side }));
        right.present(x + wi - ti, y + ti, ti, side, Some(RECT { left: ti - ri, top: 0, right: ti, bottom: side }));
    }

    /// Şeritlerin piksellerini yazar: ışığın uzandığı `act` piksel, ötesinde `rows`'a kadar sıfır.
    unsafe fn fill(&mut self, act: usize, rows: usize) {
        let (w, h, t) = (self.w, self.h, self.t);
        let (fc, fg) = (&self.fc, &self.fg);
        let [et, eb, el, er] = &self.edges;
        let dither: [[__m128; 4]; 4] = unsafe { BAYER.map(|r| r.map(|v| _mm_set1_ps(v))) };
        // Aynı eşikler 8.8'de, piksel çiftleri için: [x&3 = 0,1] ve [2,3].
        let dither16: [[__m128i; 2]; 4] = unsafe {
            BAYER.map(|r| {
                let q = r.map(|v| (v * 256.0) as i16);
                [_mm_set_epi16(q[1], q[1], q[1], q[1], q[0], q[0], q[0], q[0]), _mm_set_epi16(q[3], q[3], q[3], q[3], q[2], q[2], q[2], q[2])]
            })
        };
        let (fc4, fg4, fc4_r, fg4_r) = (&self.fc4, &self.fg4, &self.fc4_r, &self.fg4_r);
        let [top, bottom, left, right] = &mut self.strips;

        // Üst ve alt: satırda sönüm sabit, renk sütunla değişir. Köşe sütunlarında yan kenarın ışığı
        // şeridin bütün satırlarında üste biner (kenarın kendi ışığı bitse de).
        let prev = rows;
        let corner = rows.min(w / 2);
        for (strip, edge, bottom_side) in [(top, et, false), (bottom, eb, true)] {
            let px = strip.pixels();
            for d in 0..t {
                let ly = if bottom_side { t - 1 - d } else { d };
                let row = &mut px[ly * w..(ly + 1) * w];
                let sy = if bottom_side { h - 1 - d } else { d };
                let th = &dither[ly & 3];
                let (c, g) = (&edge.core, &edge.glow);
                unsafe {
                    let (vc, vg) = (_mm_set1_ps(fc[d]), _mm_set1_ps(fg[d]));
                    if d < act {
                        // 8.8 sabit nokta: iki piksel tek komutta (pmulhuw).
                        let (c16, g16) = (&edge.core16, &edge.glow16);
                        let (f1, f2) = (_mm_set1_epi16(fc4[d][0] as i16), _mm_set1_epi16(fg4[d][0] as i16));
                        let (d01, d23) = (dither16[ly & 3][0], dither16[ly & 3][1]);
                        let two = |x: usize, dd: __m128i| {
                            let cv = _mm_loadu_si128(c16.as_ptr().add(x).cast());
                            let gv = _mm_loadu_si128(g16.as_ptr().add(x).cast());
                            let v = _mm_adds_epu16(_mm_adds_epu16(_mm_mulhi_epu16(cv, f1), _mm_mulhi_epu16(gv, f2)), dd);
                            _mm_srli_epi16::<8>(v)
                        };
                        let mut x = 0;
                        while x + 4 <= w {
                            let p = _mm_packus_epi16(two(x, d01), two(x + 2, d23));
                            _mm_storeu_si128(row.as_mut_ptr().add(x).cast(), p);
                            x += 4;
                        }
                        while x < w {
                            row[x] = pack1(mad(&c[x], vc, &g[x], vg), th[x & 3]);
                            x += 1;
                        }
                    } else if d < prev {
                        row.fill(0);
                    }
                    // Köşe: kenarın ışığı a, yanınki b; önçarpımlı a + b·(1 − a.alfa), 8.8'de.
                    let (c16, g16) = (&edge.core16, &edge.glow16);
                    let (f1, f2) = (_mm_set1_epi16(fc4[d][0] as i16), _mm_set1_epi16(fg4[d][0] as i16));
                    let (d01, d23) = (dither16[ly & 3][0], dither16[ly & 3][1]);
                    for (side, flip) in [(el, false), (er, true)] {
                        let (sc, sg) = (&side.core[sy], &side.glow[sy]);
                        let cc = _mm_set1_epi64x(i64::from_le_bytes(std::mem::transmute::<[u16; 4], [u8; 8]>(side.core16[sy])));
                        let gg = _mm_set1_epi64x(i64::from_le_bytes(std::mem::transmute::<[u16; 4], [u8; 8]>(side.glow16[sy])));
                        // Sütunun yan kenara uzaklığına göre sönüm dizisi ve başlangıcı.
                        let (fs1, fs2, base) = if flip { (fc4_r, fg4_r, w - t) } else { (fc4, fg4, 0) };
                        let two = |x: usize, dd: __m128i| {
                            let a = _mm_adds_epu16(
                                _mm_mulhi_epu16(_mm_loadu_si128(c16.as_ptr().add(x).cast()), f1),
                                _mm_mulhi_epu16(_mm_loadu_si128(g16.as_ptr().add(x).cast()), f2),
                            );
                            let b = _mm_adds_epu16(
                                _mm_mulhi_epu16(cc, _mm_loadu_si128(fs1.as_ptr().add(x - base).cast())),
                                _mm_mulhi_epu16(gg, _mm_loadu_si128(fs2.as_ptr().add(x - base).cast())),
                            );
                            let aa = _mm_shufflehi_epi16::<0xFF>(_mm_shufflelo_epi16::<0xFF>(a));
                            let k = _mm_xor_si128(aa, _mm_set1_epi16(-1));
                            _mm_srli_epi16::<8>(_mm_adds_epu16(_mm_adds_epu16(a, _mm_mulhi_epu16(b, k)), dd))
                        };
                        let (xs, xe) = if flip { (w - corner, w) } else { (0, corner) };
                        let mut x = xs;
                        while x < xe {
                            if x & 3 == 0 && x + 4 <= xe {
                                let p = _mm_packus_epi16(two(x, d01), two(x + 2, d23));
                                _mm_storeu_si128(row.as_mut_ptr().add(x).cast(), p);
                                x += 4;
                                continue;
                            }
                            let dx = if flip { w - 1 - x } else { x };
                            let a = mad(&c[x], vc, &g[x], vg);
                            let b = mad(sc, _mm_set1_ps(fc[dx]), sg, _mm_set1_ps(fg[dx]));
                            row[x] = pack1(over(a, b), th[x & 3]);
                            x += 1;
                        }
                    }
                }
            }
        }

        // Sol ve sağ: satırda renk sabit, sönüm sütunla değişir; dörder piksel.
        let side_h = h.saturating_sub(2 * t);
        let (x0, x1) = (0, rows.min(t));
        for (strip, edge, flip) in [(left, el, false), (right, er, true)] {
            let sh = (strip.h as usize).min(side_h);
            let px = strip.pixels();
            // Sağ şeritte kenar sağda: sütun x'in kenara uzaklığı t−1−x.
            let (a, b) = if flip { (t - x1, t - x0) } else { (x0, x1) };
            let dist = |x: usize| if flip { t - 1 - x } else { x };
            for ly in 0..sh {
                let sy = t + ly;
                let row = &mut px[ly * t..(ly + 1) * t];
                let th = &dither[ly & 3];
                let (c, g) = (&edge.core[sy], &edge.glow[sy]);
                unsafe {
                    let (cv, gv) = (_mm_loadu_ps(c.as_ptr()), _mm_loadu_ps(g.as_ptr()));
                    let px1 = |x: usize| {
                        let d = dist(x);
                        quant(_mm_add_ps(_mm_mul_ps(cv, _mm_set1_ps(fc[d])), _mm_mul_ps(gv, _mm_set1_ps(fg[d]))), th[x & 3])
                    };
                    let mut x = a;
                    while x < b && x & 3 != 0 {
                        let i = px1(x);
                        let p = _mm_packs_epi32(i, i);
                        row[x] = _mm_cvtsi128_si32(_mm_packus_epi16(p, p)) as u32;
                        x += 1;
                    }
                    // 8.8 sabit nokta: satırın rengi iki piksele, sönüm sütundan.
                    let c16 = edge.core16[sy];
                    let g16 = edge.glow16[sy];
                    let cc = _mm_set1_epi64x(i64::from_le_bytes(std::mem::transmute::<[u16; 4], [u8; 8]>(c16)));
                    let gg = _mm_set1_epi64x(i64::from_le_bytes(std::mem::transmute::<[u16; 4], [u8; 8]>(g16)));
                    let (f1s, f2s) = if flip { (fc4_r, fg4_r) } else { (fc4, fg4) };
                    let (d01, d23) = (dither16[ly & 3][0], dither16[ly & 3][1]);
                    let two = |x: usize, dd: __m128i| {
                        let f1 = _mm_loadu_si128(f1s.as_ptr().add(x).cast());
                        let f2 = _mm_loadu_si128(f2s.as_ptr().add(x).cast());
                        _mm_srli_epi16::<8>(_mm_adds_epu16(_mm_adds_epu16(_mm_mulhi_epu16(cc, f1), _mm_mulhi_epu16(gg, f2)), dd))
                    };
                    while x + 4 <= b {
                        let p = _mm_packus_epi16(two(x, d01), two(x + 2, d23));
                        _mm_storeu_si128(row.as_mut_ptr().add(x).cast(), p);
                        x += 4;
                    }
                    while x < b {
                        let i = px1(x);
                        let p = _mm_packs_epi32(i, i);
                        row[x] = _mm_cvtsi128_si32(_mm_packus_epi16(p, p)) as u32;
                        x += 1;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------- hap

/// Hapın yazıları: kare başına değil, metin değişince kurulur.
#[derive(Default)]
struct Texts {
    title: Option<(IDWriteTextLayout, f32)>,
    /// Düzen, genişlik, yükseklik.
    body: Option<(IDWriteTextLayout, f32, f32)>,
    mono: bool,
}

struct Pill {
    win: Layered,
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    grad: ID2D1LinearGradientBrush,
    /// Düğmelerin pencere içindeki yeri (piksel): Vazgeç, Çalıştır.
    buttons: [RECT; 2],
    click_through: bool,
    /// Çizim hedefinin bağlı olduğu boy (piksel): yalnız hapın o anki penceresi temizlenip kopyalanır.
    bound: (i32, i32),
}

/// Hapın bir karelik görünümü.
struct PillFrame {
    /// Merkezin ekran x'i ve çalışma alanının tepesi (piksel).
    cx: f32,
    top: f32,
    s: f32,
    w: f32,
    h: f32,
    alpha: f32,
    scale: f32,
    dy: f32,
    phase: f32,
    lvl: f32,
    bars: [f32; 5],
    confirm: bool,
    /// Düğmelerin üstünde durma (0..1): Vazgeç, Çalıştır; cevap verildiyse soluk.
    hover: [f32; 2],
    answered: bool,
}

/// Onay düğmelerinin genişliği (DIP).
fn button_w(l: &(IDWriteTextLayout, f32)) -> f32 {
    (l.1 + 36.0).max(86.0)
}

impl Pill {
    fn new(sh: &Shared, s: f32, capture: bool) -> Res<Self> {
        let (w, h) = (((PILL_MAX_W + 2.0 * MARGIN) * s).ceil() as i32 + 2, ((PILL_MAX_H + 2.0 * MARGIN) * s).ceil() as i32 + 2);
        let win = Layered::new(w, h, capture)?;
        unsafe {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                // Yazılım hedefi: küçük hapta donanım hedefinin geri okumasından ~3 kat ucuz (ölçüldü).
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0 * s,
                dpiY: 96.0 * s,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = sh.d2d.CreateDCRenderTarget(&props)?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = rt.CreateSolidColorBrush(&hex(TEXT, 1.0), None)?;
            let mut stops: Vec<D2D1_GRADIENT_STOP> =
                PALETTE.iter().enumerate().map(|(i, &c)| D2D1_GRADIENT_STOP { position: i as f32 / 5.0, color: hex(c, 1.0) }).collect();
            stops.push(D2D1_GRADIENT_STOP { position: 1.0, color: hex(PALETTE[0], 1.0) });
            let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_WRAP)?;
            let grad = rt.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: 0.0, Y: 0.0 }, endPoint: Vector2 { X: 100.0, Y: 0.0 } },
                None,
                &coll,
            )?;
            Ok(Self { win, rt, brush, grad, buttons: [RECT::default(); 2], click_through: true, bound: (0, 0) })
        }
    }

    fn draw(&mut self, sh: &Shared, tx: &Texts, f: &PillFrame) {
        let s = f.s;
        // Pencerenin sol üstü tam piksele, hap kesirli konumda: büyürken titremez.
        let ox = f.cx - (f.w / 2.0 + MARGIN) * s;
        let oy = f.top + (PILL_TOP - MARGIN + f.dy) * s;
        let (wx, wy) = (ox.floor(), oy.floor());
        let (fx, fy) = ((ox - wx) / s, (oy - wy) / s);
        let (pl, pt) = (MARGIN + fx, MARGIN + fy);
        let (pr, pb) = (pl + f.w, pt + f.h);
        let radius = (f.h / 2.0).min(22.0);
        let a = f.alpha;
        let center = Vector2 { X: (pl + pr) / 2.0, Y: (pt + pb) / 2.0 };
        let xf = Matrix3x2::scale_around(f.scale, f.scale, center);
        let ww = (((f.w + 2.0 * MARGIN) * s).ceil() as i32 + 2).min(self.win.w);
        let wh = (((f.h + 2.0 * MARGIN) * s).ceil() as i32 + 2).min(self.win.h);
        if self.bound != (ww, wh) {
            self.bound = (ww, wh);
            unsafe {
                let _ = self.rt.BindDC(self.win.dc, &RECT { left: 0, top: 0, right: ww, bottom: wh });
            }
        }
        let rt = &self.rt;
        let br = &self.brush;
        unsafe {
            rt.BeginDraw();
            rt.SetTransform(&Matrix3x2::identity());
            rt.Clear(Some(&D2D1_COLOR_F::default()));
            rt.SetTransform(&xf);

            // Renkli hale: paletin döndüğü geçiş, katman katman sönerek (bulanıklık taklidi).
            let period = (f.w * 1.6).max(360.0);
            let x0 = pl - f.phase.rem_euclid(1.0) * period;
            self.grad.SetStartPoint(Vector2 { X: x0, Y: pt });
            self.grad.SetEndPoint(Vector2 { X: x0 + period, Y: pb });
            let glow = (0.75 + 0.6 * f.lvl) * a;
            const LAYERS: usize = 6;
            for i in 0..LAYERS {
                let k = 2.0 + i as f32 * 3.4;
                let op = 0.07 * (1.0 - i as f32 / LAYERS as f32).powf(1.6) * glow;
                self.grad.SetOpacity(op);
                rt.FillRoundedRectangle(&rrect(rect(pl - k, pt - k * 0.8, pr + k, pb + k * 1.1), radius + k), &self.grad);
            }
            // Gövde: neredeyse siyah, %92; ince kenar çizgisi ve üstünde paletten hafif bir ışık.
            br.SetColor(&hex(0x0E0E10, 0.92 * a));
            rt.FillRoundedRectangle(&rrect(rect(pl, pt, pr, pb), radius), br);
            let px1 = 1.0 / s;
            let edge = rrect(rect(pl + px1 / 2.0, pt + px1 / 2.0, pr - px1 / 2.0, pb - px1 / 2.0), radius - px1 / 2.0);
            br.SetColor(&hex(0xffffff, 0.11 * a));
            rt.DrawRoundedRectangle(&edge, br, px1, None);
            self.grad.SetOpacity(0.35 * a);
            rt.DrawRoundedRectangle(&edge, &self.grad, px1, None);

            // Dalga: 5 çubuk, seviyeyle; renkleri paletten.
            let cy = pt + 22.0;
            for (i, v) in f.bars.iter().enumerate() {
                let bh = 4.0 + 15.0 * v.clamp(0.0, 1.0);
                let bx = pl + PAD_L + i as f32 * 5.6;
                let c = mix3(palette(f.phase + i as f32 * 0.09), [1.0; 3], 0.12);
                br.SetColor(&color(c, a));
                rt.FillRoundedRectangle(&rrect(rect(bx, cy - bh / 2.0, bx + 3.0, cy + bh / 2.0), 1.5), br);
            }

            // Yazılar hapın içine kırpılır: büyürken taşmaz.
            rt.PushAxisAlignedClip(&rect(pl + 8.0, pt + 2.0, pr - 12.0, pb - 2.0), D2D1_ANTIALIAS_MODE_ALIASED);
            let has_body = tx.body.is_some();
            if let Some((l, _)) = &tx.title {
                br.SetColor(&hex(TEXT, if has_body { 0.62 } else { 0.96 } * a));
                rt.DrawTextLayout(Vector2 { X: pl + TEXT_X, Y: pt + 11.0 }, l, br, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
            let mut by = pt + BODY_Y;
            if let Some((l, _, bh)) = &tx.body {
                if tx.mono {
                    br.SetColor(&hex(0xffffff, 0.07 * a));
                    rt.FillRoundedRectangle(&rrect(rect(pl + TEXT_X - 9.0, by + 3.0, pr - 14.0, by + 3.0 + bh + 12.0), 8.0), br);
                    by += 9.0;
                }
                br.SetColor(&hex(TEXT, 0.94 * a));
                rt.DrawTextLayout(Vector2 { X: pl + TEXT_X, Y: by }, l, br, D2D1_DRAW_TEXT_OPTIONS_NONE);
                by += bh + if tx.mono { 9.0 } else { 0.0 };
            }
            rt.PopAxisAlignedClip();

            // Onay: sağa yaslı [Vazgeç] [Çalıştır].
            self.buttons = [RECT::default(); 2];
            if f.confirm {
                let y0 = by + 12.0;
                let mut x = pr - 12.0;
                let dim = if f.answered { 0.45 } else { 1.0 };
                for (i, l) in [(1usize, &sh.run), (0usize, &sh.cancel)] {
                    let bw = button_w(l);
                    let r = rect(x - bw, y0, x, y0 + BTN_H);
                    let hv = f.hover[i];
                    if i == 1 {
                        br.SetColor(&color(mix3(rgbf(ACCENT), [1.0; 3], 0.14 * hv), a * dim));
                    } else {
                        br.SetColor(&hex(0xffffff, (0.09 + 0.08 * hv) * a * dim));
                    }
                    rt.FillRoundedRectangle(&rrect(r, BTN_H / 2.0), br);
                    br.SetColor(&hex(0xffffff, if i == 1 { 1.0 } else { 0.9 } * a * dim));
                    rt.DrawTextLayout(Vector2 { X: r.left, Y: r.top }, &l.0, br, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    // Tıklama sınaması için pencere pikseline (ölçek dönüşümüyle).
                    let tp = |x: f32, y: f32| {
                        let (x, y) = (center.X + (x - center.X) * f.scale, center.Y + (y - center.Y) * f.scale);
                        ((x * s).round() as i32, (y * s).round() as i32)
                    };
                    let (l0, t0) = tp(r.left, r.top);
                    let (r0, b0) = tp(r.right, r.bottom);
                    self.buttons[i] = RECT { left: l0, top: t0, right: r0, bottom: b0 };
                    x -= bw + 8.0;
                }
            }
            let _ = rt.EndDraw(None, None);
            let _ = GdiFlush();
        }
        self.win.present(wx as i32, wy as i32, ww, wh, None);
    }

    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        self.buttons.iter().position(|r| r.right > r.left && x >= r.left && x < r.right && y >= r.top && y < r.bottom)
    }
}

// ---------------------------------------------------------------- durum

struct Shared {
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
    title_fmt: IDWriteTextFormat,
    body_fmt: IDWriteTextFormat,
    mono_fmt: IDWriteTextFormat,
    /// Onay düğmelerinin yazıları (genişliğiyle).
    run: (IDWriteTextLayout, f32),
    cancel: (IDWriteTextLayout, f32),
}

/// Kipin görünüşü; kip değişince bunlar yumuşakça yeni değerlerine kayar.
#[derive(Clone, Copy)]
struct Look {
    /// İçeri uzanım (DIP) ve seviyeyle artışı.
    ext: f32,
    ext_lvl: f32,
    amp: f32,
    amp_lvl: f32,
    core: f32,
    /// Renk dönüşü (tur/sn), parlaklık dalgalarının hızı.
    speed: f32,
    flow: f32,
    think: f32,
    work: f32,
}

fn look(m: Mode) -> Look {
    match m {
        Mode::Listening | Mode::Hidden => {
            Look { ext: 96.0, ext_lvl: 36.0, amp: 1.05, amp_lvl: 0.45, core: 0.95, speed: 0.12, flow: 1.0, think: 0.0, work: 0.0 }
        }
        Mode::Speaking => {
            Look { ext: 74.0, ext_lvl: 46.0, amp: 0.85, amp_lvl: 0.5, core: 0.8, speed: 0.13, flow: 1.3, think: 0.0, work: 0.0 }
        }
        Mode::Thinking | Mode::Confirm => {
            Look { ext: 56.0, ext_lvl: 0.0, amp: 0.72, amp_lvl: 0.0, core: 0.65, speed: 0.38, flow: 3.2, think: 1.0, work: 0.0 }
        }
        Mode::Working => {
            Look { ext: 34.0, ext_lvl: 0.0, amp: 0.5, amp_lvl: 0.0, core: 0.9, speed: 0.08, flow: 0.6, think: 0.0, work: 1.0 }
        }
    }
}

impl Look {
    fn approach(&mut self, to: &Look, k: f32) {
        for (a, b) in [
            (&mut self.ext, to.ext),
            (&mut self.ext_lvl, to.ext_lvl),
            (&mut self.amp, to.amp),
            (&mut self.amp_lvl, to.amp_lvl),
            (&mut self.core, to.core),
            (&mut self.speed, to.speed),
            (&mut self.flow, to.flow),
            (&mut self.think, to.think),
            (&mut self.work, to.work),
        ] {
            *a += (b - *a) * k;
        }
    }
}

/// Görünürken var olan her şey: pencereler, zaman, geçişler.
struct Scene {
    glow: Glow,
    pill: Pill,
    s: f32,
    /// Hapın merkezi ve çalışma alanının tepesi (piksel).
    cx: f32,
    top: f32,
    last: Instant,
    time: f32,
    /// Görünürlük 0..1; `rising` açılırken.
    vis: f32,
    rising: bool,
    phase: f32,
    flow: f32,
    work_pos: f32,
    lvl: f32,
    look: Look,
    w: Spring,
    h: Spring,
    bars: [f32; 5],
    hover: [f32; 2],
    hot: Option<usize>,
    pressed: Option<usize>,
    tracking: bool,
    answered: bool,
    /// Ölçüm: kare sayısı, toplam ve en uzun çizim süresi (ms), açılış anı.
    frames: u32,
    cost: f64,
    worst: f64,
    started: Instant,
}

struct State {
    host: HWND,
    notify: HWND,
    msg: u32,
    sh: Shared,
    capture: bool,
    mode: Mode,
    title: String,
    text: String,
    dirty: bool,
    texts: Texts,
    level_in: f32,
    scene: Option<Scene>,
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let xy = || ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32);
        match msg {
            WM_TIMER if wp.0 == TIMER => {
                with_state(|st| st.frame());
                return LRESULT(0);
            }
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_DPICHANGED => return LRESULT(0),
            WM_MOUSEMOVE => {
                with_state(|st| st.mouse(hwnd, Some(xy()), false, false));
                return LRESULT(0);
            }
            WM_MOUSELEAVE => {
                with_state(|st| st.mouse(hwnd, None, false, false));
                return LRESULT(0);
            }
            WM_LBUTTONDOWN => {
                with_state(|st| st.mouse(hwnd, Some(xy()), true, false));
                return LRESULT(0);
            }
            WM_LBUTTONUP => {
                with_state(|st| st.mouse(hwnd, Some(xy()), false, true));
                return LRESULT(0);
            }
            WM_SETCURSOR => {
                let hot = with_state(|st| st.scene.as_ref().is_some_and(|sc| sc.pill.win.hwnd == hwnd && sc.hot.is_some()));
                if hot == Some(true) {
                    if let Ok(c) = LoadCursorW(None, IDC_HAND) {
                        SetCursor(Some(c));
                    }
                    return LRESULT(1);
                }
            }
            _ => {}
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

impl State {
    fn new(notify: HWND, msg: u32) -> Res<Self> {
        unsafe {
            let inst = GetModuleHandleW(None)?;
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: inst.into(),
                lpszClassName: CLASS,
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                ..Default::default()
            };
            RegisterClassW(&wc);
            // Zamanlayıcının sahibi: yalnızca ileti alan, hiç görünmeyen pencere.
            let host = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                PCWSTR::null(),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(inst.into()),
                None,
            )?;
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let format = |family: PCWSTR, size: f32, weight: DWRITE_FONT_WEIGHT, wrap: bool| -> Res<IDWriteTextFormat> {
                let f = dw.CreateTextFormat(family, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("tr-tr"))?;
                f.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP })?;
                f.SetParagraphAlignment(if wrap { DWRITE_PARAGRAPH_ALIGNMENT_NEAR } else { DWRITE_PARAGRAPH_ALIGNMENT_CENTER })?;
                Ok(f)
            };
            let ui = w!("Segoe UI Variable Text");
            let title_fmt = format(ui, 13.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?;
            let body_fmt = format(ui, 14.0, DWRITE_FONT_WEIGHT_NORMAL, true)?;
            let mono_fmt = format(w!("Cascadia Mono"), 13.0, DWRITE_FONT_WEIGHT_NORMAL, true)?;
            let button_fmt = format(ui, 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?;
            button_fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            let button = |s: &str| -> Res<(IDWriteTextLayout, f32)> {
                let w = measure_layout(&dw.CreateTextLayout(&wide(s), &button_fmt, 400.0, BTN_H)?).0;
                let l = dw.CreateTextLayout(&wide(s), &button_fmt, (w + 36.0).max(86.0), BTN_H)?;
                Ok((l, w))
            };
            let sh = Shared { run: button("Çalıştır")?, cancel: button("Vazgeç")?, d2d, dw, title_fmt, body_fmt, mono_fmt };
            Ok(Self {
                host,
                notify,
                msg,
                sh,
                capture: std::env::var_os("ENGRAM_ISIK_YAKALA").is_some(),
                mode: Mode::Hidden,
                title: String::new(),
                text: String::new(),
                dirty: false,
                texts: Texts::default(),
                level_in: 0.0,
                scene: None,
            })
        }
    }

    fn set(&mut self, mode: Mode, title: &str, text: &str) {
        let was = self.mode;
        self.mode = mode;
        if self.title != title || self.text != text || (was == Mode::Confirm) != (mode == Mode::Confirm) {
            self.title = title.to_string();
            self.text = text.to_string();
            self.dirty = true;
        }
        if mode == Mode::Hidden {
            if let Some(sc) = &mut self.scene {
                sc.rising = false;
            }
            return;
        }
        if was != mode
            && let Some(sc) = &mut self.scene
        {
            sc.answered = false;
            sc.pressed = None;
        }
        match &mut self.scene {
            Some(sc) => sc.rising = true,
            None => match self.open() {
                Ok(sc) => {
                    self.scene = Some(sc);
                    unsafe { SetTimer(Some(self.host), TIMER, FRAME_MS, None) };
                }
                Err(e) => log!("ışık katmanı açılamadı: {e}"),
            },
        }
    }

    /// İmlecin olduğu ekranda pencereleri kurar.
    fn open(&mut self) -> Res<Scene> {
        let mut pt = POINT::default();
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let (mut dx, mut dy) = (96u32, 96u32);
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let _ = GetMonitorInfoW(mon, &mut mi);
            let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        }
        let s = dx.max(96) as f32 / 96.0;
        let glow = Glow::new(mi.rcMonitor, s, self.capture)?;
        // Hap şeritlerden sonra kurulur: en üstte o durur.
        let pill = Pill::new(&self.sh, s, self.capture)?;
        let now = Instant::now();
        Ok(Scene {
            glow,
            pill,
            s,
            cx: (mi.rcWork.left + mi.rcWork.right) as f32 / 2.0,
            top: mi.rcWork.top as f32,
            last: now,
            time: 0.0,
            vis: 0.0,
            rising: true,
            phase: 0.0,
            flow: 0.0,
            work_pos: 0.0,
            lvl: 0.0,
            look: look(self.mode),
            w: Spring { x: 110.0, v: 0.0 },
            h: Spring { x: 34.0, v: 0.0 },
            bars: [0.1; 5],
            hover: [0.0; 2],
            hot: None,
            pressed: None,
            tracking: false,
            answered: false,
            frames: 0,
            cost: 0.0,
            worst: 0.0,
            started: now,
        })
    }

    /// Başlık ve gövdenin düzenini kurar. Uzun gövdenin sonu görünür, başına "…" gelir.
    fn layout(&mut self) {
        self.dirty = false;
        let sh = &self.sh;
        let mono = self.mode == Mode::Confirm;
        self.texts.mono = mono;
        let title = self.title.trim();
        self.texts.title = if title.is_empty() {
            None
        } else {
            unsafe { sh.dw.CreateTextLayout(&wide(title), &sh.title_fmt, 560.0, 22.0).ok() }.map(|l| {
                let w = measure_layout(&l).0;
                (l, w)
            })
        };
        let body: String =
            if mono { self.text.trim().to_string() } else { self.text.split_whitespace().collect::<Vec<_>>().join(" ") };
        self.texts.body = None;
        if body.is_empty() {
            return;
        }
        let fmt = if mono { &sh.mono_fmt } else { &sh.body_fmt };
        let max_w = PILL_MAX_W - TEXT_X - PAD_R - if mono { 8.0 } else { 0.0 };
        // Yalnız son ~400 karakter: uzun cevapta her parçada düzen ucuz kalsın.
        let chars: Vec<char> = body.chars().collect();
        let mut text: Vec<u16> = if chars.len() > 400 {
            let tail: String = chars[chars.len() - 400..].iter().collect();
            let tail = tail.split_once(' ').map_or(tail.as_str(), |(_, r)| r).to_string();
            format!("…{tail}").encode_utf16().collect()
        } else {
            body.encode_utf16().collect()
        };
        for _ in 0..4 {
            let Ok(l) = (unsafe { sh.dw.CreateTextLayout(&text, fmt, max_w, 1000.0) }) else { return };
            let mut n = 0u32;
            unsafe {
                let _ = l.GetLineMetrics(None, &mut n);
            }
            if (n as usize) <= BODY_LINES {
                let (w, h) = measure_layout(&l);
                self.texts.body = Some((l, w, h));
                return;
            }
            let mut lines = vec![DWRITE_LINE_METRICS::default(); n as usize];
            unsafe {
                let _ = l.GetLineMetrics(Some(&mut lines), &mut n);
            }
            let cut: u32 = lines[..n as usize - BODY_LINES].iter().map(|m| m.length).sum();
            let rest = &text[(cut as usize).min(text.len())..];
            let rest = rest.iter().copied().skip_while(|&c| c == '…' as u16 || c == ' ' as u16);
            text = "…".encode_utf16().chain(rest).collect();
        }
    }

    /// Hapın hedef boyu (DIP).
    fn pill_target(&self) -> (f32, f32) {
        let tx = &self.texts;
        let mut cw = tx.title.as_ref().map_or(0.0, |t| t.1);
        let mut h = 44.0;
        if let Some((_, bw, bh)) = &tx.body {
            cw = cw.max(bw + if tx.mono { 8.0 } else { 0.0 });
            h = BODY_Y + bh + if tx.mono { 18.0 } else { 0.0 } + 12.0;
        }
        let mut w = TEXT_X + cw + PAD_R;
        if self.mode == Mode::Confirm {
            w = w.max(TEXT_X + button_w(&self.sh.cancel) + 8.0 + button_w(&self.sh.run) + 12.0);
            h += BTN_H + 10.0;
        }
        (w.clamp(132.0, PILL_MAX_W), h.min(PILL_MAX_H))
    }

    fn frame(&mut self) {
        let t0 = Instant::now();
        if self.dirty {
            self.layout();
        }
        let (tw, th) = self.pill_target();
        let mode = self.mode;
        let level_in = self.level_in;
        let Some(sc) = &mut self.scene else {
            unsafe {
                let _ = KillTimer(Some(self.host), TIMER);
            }
            return;
        };
        let dt = t0.duration_since(sc.last).as_secs_f32().clamp(0.0, 0.05);
        sc.last = t0;
        sc.time += dt;

        // Görünürlük: açılış ~350 ms, kapanış ~300 ms.
        if sc.rising {
            sc.vis = (sc.vis + dt / 0.35).min(1.0);
        } else {
            sc.vis = (sc.vis - dt / 0.30).max(0.0);
            if sc.vis <= 0.0 {
                let fps = sc.frames as f64 / sc.started.elapsed().as_secs_f64().max(0.001);
                log!(
                    "ışık: {} kare ({fps:.0}/sn), kare başına ort {:.2} ms, en çok {:.2} ms",
                    sc.frames,
                    sc.cost / sc.frames.max(1) as f64,
                    sc.worst
                );
                self.scene = None;
                unsafe {
                    let _ = KillTimer(Some(self.host), TIMER);
                }
                return;
            }
        }

        // Seviye: hızlı yükselir, yavaş iner.
        let target = if matches!(mode, Mode::Listening | Mode::Speaking) { level_in } else { 0.0 };
        let tau = if target > sc.lvl { 0.035 } else { 0.28 };
        sc.lvl += (target - sc.lvl) * (1.0 - (-dt / tau).exp());
        let lvl = sc.lvl;
        if mode != Mode::Hidden {
            sc.look.approach(&look(mode), 1.0 - (-dt / 0.25).exp());
        }
        let lk = sc.look;
        sc.phase += lk.speed * dt;
        sc.flow += lk.flow * dt;
        sc.work_pos = (sc.work_pos + dt / 4.5).rem_euclid(1.0);

        // Açılışta ışık kenardan içeri büyür; kapanışta söner, biraz da çekilir.
        let (grow, fade) = if sc.rising {
            (ease_out_cubic(sc.vis), (sc.vis * 1.8).min(1.0))
        } else {
            (0.55 + 0.45 * sc.vis, sc.vis * sc.vis * (3.0 - 2.0 * sc.vis))
        };
        let breath = 1.0 + 0.04 * (sc.time * 1.9).sin();
        let pulse = 1.0 + lk.think * 0.14 * (sc.time * 5.0).sin();
        let ext = ((lk.ext + lk.ext_lvl * lvl) * grow * breath).min(GLOW_MAX - 4.0);
        sc.glow.render(&GlowFrame {
            ext,
            amp: (lk.amp + lk.amp_lvl * lvl) * fade * pulse,
            core: lk.core * fade * (1.0 + 0.25 * lvl),
            phase: sc.phase,
            flow: sc.flow,
            work: lk.work * fade,
            work_pos: sc.work_pos,
        });

        // Hap: boyu yayla, çubuklar seviyeyle.
        sc.w.step(tw, dt);
        sc.h.step(th, dt);
        for (i, b) in sc.bars.iter_mut().enumerate() {
            let fi = i as f32;
            let g = [0.55, 0.85, 1.0, 0.8, 0.6][i];
            let talk = (lvl * 1.6 * g * (0.72 + 0.28 * (sc.time * (7.0 + fi * 1.9) + fi * 1.3).sin())).min(1.0);
            let idle = 0.1 + 0.05 * (sc.time * 2.2 + fi * 0.8).sin();
            let think = 0.08 + 0.75 * (0.5 + 0.5 * (sc.time * 7.5 - fi * 0.8).sin()).powi(2);
            let work = 0.12 + 0.22 * (0.5 + 0.5 * (sc.time * 2.6 - fi * 0.7).sin());
            let v = lerp(lerp(idle.max(talk), think, lk.think), work, lk.work);
            *b += (v - *b) * (1.0 - (-dt / 0.06).exp());
        }
        let confirm = mode == Mode::Confirm;
        for i in 0..2 {
            let to = if sc.hot == Some(i) && !sc.answered { 1.0 } else { 0.0 };
            sc.hover[i] += (to - sc.hover[i]) * (1.0 - (-dt / 0.08).exp());
        }
        let clickable = confirm && !sc.answered && sc.vis > 0.5;
        if sc.pill.click_through == clickable {
            sc.pill.click_through = !clickable;
            sc.pill.win.set_click_through(!clickable);
            if !clickable {
                sc.hot = None;
                sc.pressed = None;
            }
        }
        let (alpha, scale, dy) = if sc.rising {
            ((sc.vis * 2.6).min(1.0), 0.84 + 0.16 * ease_out_back(sc.vis), -18.0 * (1.0 - ease_out_cubic(sc.vis)))
        } else {
            (sc.vis * sc.vis, 0.93 + 0.07 * sc.vis, -8.0 * (1.0 - sc.vis))
        };
        let pf = PillFrame {
            cx: sc.cx,
            top: sc.top,
            s: sc.s,
            w: sc.w.x.clamp(40.0, PILL_MAX_W),
            h: sc.h.x.clamp(24.0, PILL_MAX_H),
            alpha,
            scale,
            dy,
            phase: sc.phase,
            lvl,
            bars: sc.bars,
            confirm,
            hover: sc.hover,
            answered: sc.answered,
        };
        sc.pill.draw(&self.sh, &self.texts, &pf);

        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        sc.frames += 1;
        sc.cost += ms;
        sc.worst = sc.worst.max(ms);
    }

    /// Hap üstünde fare: `pos` yoksa ayrıldı; `down`/`up` sol düğme.
    fn mouse(&mut self, hwnd: HWND, pos: Option<(i32, i32)>, down: bool, up: bool) {
        let (notify, msg) = (self.notify, self.msg);
        let Some(sc) = &mut self.scene else { return };
        if sc.pill.win.hwnd != hwnd || sc.pill.click_through {
            return;
        }
        if pos.is_some() && !sc.tracking {
            let mut tme =
                TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            sc.tracking = true;
        }
        if pos.is_none() {
            sc.tracking = false;
        }
        sc.hot = pos.and_then(|(x, y)| sc.pill.hit(x, y));
        if down {
            sc.pressed = sc.hot;
        }
        if up {
            if let Some(i) = sc.hot.filter(|&i| sc.pressed == Some(i) && !sc.answered) {
                sc.answered = true;
                unsafe {
                    let _ = PostMessageW(Some(notify), msg, WPARAM(i), LPARAM(0));
                }
            }
            sc.pressed = None;
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Düzenin genişliği (sondaki boşluk hariç) ve yüksekliği.
fn measure_layout(l: &IDWriteTextLayout) -> (f32, f32) {
    let mut m = DWRITE_TEXT_METRICS::default();
    unsafe {
        let _ = l.GetMetrics(&mut m);
    }
    (m.width, m.height)
}
