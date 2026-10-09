//! Çalan şarkının widget'ı: kapak, şarkı, sanatçı, önceki / çal / sonraki ve ilerleme. İki yerde
//! durabilir:
//!
//! - Dock'un yanında (varsayılan): dock'un solundaki ya da sağındaki boşlukta, dock'la aynı
//!   boyda ince bir şerit. Dock ekranın altında yer ayırdığı için hiçbir pencerenin altında
//!   kalmaz; tam ekran oyun ve videoda gizlenir. Dock kapalıysa masaüstüne geçer.
//! - Masaüstünde: büyük bir kart, pencerelerin arkasında; "Masaüstünü göster" (Win+D) ile öne
//!   çıkar, sürükleyerek taşınır.
//!
//! Arka planı kapağın bulanık hali, kapağın rengi ya da koyu (dock'un yanında dock'un kendi
//! rengi); saydamlığı ayarlanır.
//!
//! Katmanlı pencere: Direct2D ile bellekteki bitmap'e çizilir, UpdateLayeredWindow ile tek
//! seferde verilir. Yalnızca bir şey değişince çizilir: şarkı, fare, çalarken saniyede bir.

use std::cell::RefCell;
use std::time::Instant;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};
use windows_numerics::{Matrix3x2, Vector2};

use super::look;
use super::media::{Cmd, Now, Remote};
use crate::hatter::Geometry;

const CLASS: PCWSTR = w!("hive-music");
const TIMER_TICK: usize = 1;
/// Dock'un yanındayken dock'un yeri arada bir okunur (simge eklenince hap genişler).
const TIMER_DOCK: usize = 2;
const WM_MOUSELEAVE: u32 = 0x02A3;

// Masaüstü kartının ölçüleri (DIP, orta boyda). Çevresinde gölge payı; gölge aşağı düşer.
const CARD_W: f32 = 344.0;
const CARD_H: f32 = 128.0;
const RADIUS: f32 = 22.0;
const PAD_X: f32 = 24.0;
const PAD_TOP: f32 = 16.0;
const PAD_BOTTOM: f32 = 36.0;
const ART: f32 = 96.0;
// Dock'un yanındaki şerit: en geniş hali ve sığmazsa gizlendiği en dar hali, gölge payı.
const STRIP_W: f32 = 340.0;
const STRIP_MIN: f32 = 220.0;
const STRIP_PAD: f32 = 8.0;

const ICON_PREV: &str = "\u{E892}";
const ICON_NEXT: &str = "\u{E893}";
const ICON_PLAY: &str = "\u{E768}";
const ICON_PAUSE: &str = "\u{E769}";
const ICON_MUSIC: &str = "\u{EC4F}";

/// Kartın arka planı.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    /// Kapağın bulanık, koyulaştırılmış hali.
    Cover,
    /// Kapağın baskın renginden geçiş.
    Color,
    /// Düz: masaüstünde koyu, dock'un yanında dock'un rengi.
    Plain,
}

impl Style {
    pub const ALL: [Style; 3] = [Style::Cover, Style::Color, Style::Plain];

    pub fn id(self) -> &'static str {
        match self {
            Style::Cover => "kapak",
            Style::Color => "renk",
            Style::Plain => "duz",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.id() == s)
    }
}

/// Widget'ın yeri.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    DockLeft,
    DockRight,
    Desktop,
}

impl Place {
    pub const ALL: [Place; 3] = [Place::DockLeft, Place::DockRight, Place::Desktop];

    pub fn id(self) -> &'static str {
        match self {
            Place::DockLeft => "dock_sol",
            Place::DockRight => "dock_sag",
            Place::Desktop => "masaustu",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.id() == s)
    }
}

/// Widget'ın ayarları (hive'ın sayfasından).
#[derive(Clone)]
pub struct Options {
    pub place: Place,
    pub hide_idle: bool,
    /// Son görülen Spotify'ın uygulama kimliği: hiçbir şey açık değilken tıklayınca o açılır.
    pub app: String,
    /// Masaüstü kartının sol üst köşesi (piksel).
    pub pos: Option<(i32, i32)>,
    pub on_moved: fn(i32, i32),
    /// Masaüstü kartının boy çarpanı (orta 1.0).
    pub scale: f32,
    pub style: Style,
    /// Arka planın opaklığı (0.3..1); yazı ve düğmeler hep tam.
    pub opacity: f32,
    pub progress: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Card,
    Prev,
    Play,
    Next,
    Bar,
}

/// Yerleşim (DIP, pencerenin sol üstünden).
struct Layout {
    /// Dock'un yanındaki şerit mi.
    strip: bool,
    w: f32,
    h: f32,
    card: D2D_RECT_F,
    radius: f32,
    art: D2D_RECT_F,
    art_radius: f32,
    title: D2D_RECT_F,
    artist: D2D_RECT_F,
    /// Düğmeler: (ne, merkez x, merkez y, yarıçap).
    buttons: [(Hit, f32, f32, f32); 3],
    bar: Option<D2D_RECT_F>,
    /// Geçen / kalan süre yazıları (yalnızca kartta).
    times: bool,
}

struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *const u8,
    w: i32,
    h: i32,
}

impl Surface {
    fn new(w: i32, h: i32) -> Option<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
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
            let Ok(bmp) = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) else {
                let _ = DeleteDC(dc);
                return None;
            };
            let old = SelectObject(dc, bmp.into());
            Some(Self { dc, bmp, old, bits: bits as *const u8, w, h })
        }
    }

    /// (x, y) pikselinin BGRA'sı (önçarpımlı).
    fn pixel(&self, x: i32, y: i32) -> [u8; 4] {
        let i = ((y * self.w + x) * 4) as usize;
        unsafe { std::slice::from_raw_parts(self.bits.add(i), 4).try_into().unwrap() }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

struct Target {
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
}

/// Çözülmüş kapak: 32bppPBGRA pikseller, baskın rengi ve bulanık arka planları (kart ve şerit
/// oranında).
struct Art {
    w: u32,
    h: u32,
    px: Vec<u8>,
    color: u32,
    ambient_card: (usize, usize, Vec<u8>),
    ambient_strip: (usize, usize, Vec<u8>),
}

/// Hedefe bağlı bitmap'ler (hedef yeniden kurulunca düşer).
#[derive(Default)]
struct Bitmaps {
    shadow: Option<ID2D1Bitmap>,
    shadow_key: (i32, i32, bool),
    art: Option<ID2D1Bitmap>,
    ambient: Option<ID2D1Bitmap>,
    ambient_strip: bool,
}

struct Fonts {
    title: IDWriteTextFormat,
    artist: IDWriteTextFormat,
    title_s: IDWriteTextFormat,
    artist_s: IDWriteTextFormat,
    time_l: IDWriteTextFormat,
    time_r: IDWriteTextFormat,
    glyph: IDWriteTextFormat,
    glyph_s: IDWriteTextFormat,
    glyph_big: IDWriteTextFormat,
}

struct Widget {
    hwnd: HWND,
    remote: Remote,
    o: Options,
    d2d: ID2D1Factory,
    wic: IWICImagingFactory,
    f: Fonts,
    target: Option<Target>,
    surface: Option<Surface>,
    bitmaps: Bitmaps,
    /// Pencerenin bulunduğu ekranın dpi'ı (masaüstü kartı için).
    dpi: f32,
    /// Masaüstü kartının sol üst köşesi (piksel).
    pos: POINT,
    /// Dock'un yeri (açıksa); şerit buna göre durur.
    dock: Option<Geometry>,
    now: Now,
    art: Option<Art>,
    art_gen: u32,
    /// Saniyede bir çizen zamanlayıcı açık mı (yalnızca çalarken).
    ticking: bool,
    /// Son çizilen saniye.
    drawn_sec: i64,
    hover: Hit,
    pressed: Hit,
    /// Sürükleme: imlecin ve pencerenin başlangıcı, gerçekten kıpırdadı mı.
    drag: Option<(POINT, POINT, bool)>,
    tracking: bool,
    /// "Masaüstünü göster" açık: masaüstü kartı geçici olarak en üstte.
    peek: bool,
    /// Şerit tam ekran bir uygulama önündeyken gizli.
    ducked: bool,
    hook: HWINEVENTHOOK,
}

thread_local! {
    static W: RefCell<Option<Widget>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Widget) -> R) -> Option<R> {
    W.with(|w| w.try_borrow_mut().ok().and_then(|mut w| w.as_mut().map(f)))
}

fn color(c: u32, a: f32) -> D2D1_COLOR_F {
    let ch = |s: u32| ((c >> s) & 0xFF) as f32 / 255.0;
    D2D1_COLOR_F { r: ch(16), g: ch(8), b: ch(0), a }
}

/// Rengi `f` ile çarpar (koyulaştırır).
fn shade(c: u32, f: f32) -> u32 {
    let ch = |s: u32| ((((c >> s) & 0xFF) as f32 * f).min(255.0) as u32) << s;
    ch(16) | ch(8) | ch(0)
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

fn rounded(r: D2D_RECT_F, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }
}

fn inside(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn fmt_time(s: f64) -> String {
    let s = s.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Kapaktan baskın renk: doygun ve orta parlaklıktaki piksellere ağırlık verilir.
fn dominant(px: &[u8]) -> u32 {
    let (mut r, mut g, mut b, mut n) = (0f32, 0f32, 0f32, 0f32);
    for p in px.chunks_exact(4).step_by(3) {
        let (pb, pg, pr) = (p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0);
        let (mx, mn) = (pr.max(pg).max(pb), pr.min(pg).min(pb));
        let wgt = (mx - mn) * (1.0 - (mx - 0.6).abs()) + 0.02;
        r += pr * wgt;
        g += pg * wgt;
        b += pb * wgt;
        n += wgt;
    }
    if n == 0.0 {
        return 0x808080;
    }
    let (r, g, b) = (r / n, g / n, b / n);
    let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let f = |v: f32| ((l + (v - l) * 1.5) * (0.55 / l.max(0.05)).clamp(0.6, 1.6)).clamp(0.0, 1.0);
    ((f(r) * 255.0) as u32) << 16 | ((f(g) * 255.0) as u32) << 8 | (f(b) * 255.0) as u32
}

/// Ön plandaki pencere bulunduğu ekranı tamamen kaplıyor mu (oyun, video).
fn fullscreen(fg: HWND) -> bool {
    if fg.is_invalid() || is_desktop(fg) {
        return false;
    }
    unsafe {
        let mut r = RECT::default();
        let mon = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        GetWindowRect(fg, &mut r).is_ok()
            && GetMonitorInfoW(mon, &mut mi).as_bool()
            && r.left <= mi.rcMonitor.left
            && r.top <= mi.rcMonitor.top
            && r.right >= mi.rcMonitor.right
            && r.bottom >= mi.rcMonitor.bottom
    }
}

impl Widget {
    /// Dock'un yanındaysa dock'un yeri (dock kapalıysa masaüstüne düşer).
    fn strip_dock(&self) -> Option<&Geometry> {
        (self.o.place != Place::Desktop).then_some(self.dock.as_ref()).flatten()
    }

    /// DIP başına piksel.
    fn k(&self) -> f32 {
        match self.strip_dock() {
            Some(d) => d.dpi / 96.0,
            None => self.dpi / 96.0 * self.o.scale,
        }
    }

    fn layout(&self) -> Layout {
        match self.strip_dock() {
            Some(d) => self.strip_layout(d),
            None => self.card_layout(),
        }
    }

    fn card_layout(&self) -> Layout {
        let card = rect(PAD_X, PAD_TOP, PAD_X + CARD_W, PAD_TOP + CARD_H);
        let art = rect(card.left + 16.0, card.top + 16.0, card.left + 16.0 + ART, card.top + 16.0 + ART);
        let (cl, cr) = (art.right + 18.0, card.right - 18.0);
        let cx = (cl + cr) / 2.0;
        let cy = card.top + if self.o.progress { 78.0 } else { 86.0 };
        let bar = self.o.progress.then(|| rect(cl + 38.0, card.top + 108.0, cr - 38.0, card.top + 112.0));
        Layout {
            strip: false,
            w: CARD_W + 2.0 * PAD_X,
            h: CARD_H + PAD_TOP + PAD_BOTTOM,
            card,
            radius: RADIUS,
            art,
            art_radius: 14.0,
            title: rect(cl, card.top + 16.0, cr, card.top + 40.0),
            artist: rect(cl, card.top + 40.0, cr, card.top + 58.0),
            buttons: [(Hit::Prev, cx - 50.0, cy, 16.0), (Hit::Play, cx, cy, 19.0), (Hit::Next, cx + 50.0, cy, 16.0)],
            bar,
            times: true,
        }
    }

    /// Dock'un yanındaki boşluğun genişliği (DIP).
    fn strip_room(&self, d: &Geometry) -> f32 {
        let k = d.dpi / 96.0;
        let room = match self.o.place {
            Place::DockRight => d.monitor.right - d.right,
            _ => d.left - d.monitor.left,
        };
        room as f32 / k - 2.0 * d.margin
    }

    fn strip_layout(&self, d: &Geometry) -> Layout {
        let h = d.pill_h;
        let w = self.strip_room(d).min(STRIP_W).max(STRIP_MIN);
        let s = STRIP_PAD;
        let card = rect(s, s, s + w, s + h);
        let pad = 6.0;
        let art = rect(card.left + pad, card.top + pad, card.left + h - pad, card.bottom - pad);
        let cy = (card.top + card.bottom) / 2.0;
        let next = card.right - 22.0;
        let buttons = [(Hit::Prev, next - 72.0, cy, 14.0), (Hit::Play, next - 36.0, cy, 17.0), (Hit::Next, next, cy, 14.0)];
        let (cl, cr) = (art.right + 10.0, next - 72.0 - 22.0);
        let bar = self.o.progress.then(|| rect(cl, card.bottom - 7.0, cr, card.bottom - 5.0));
        Layout {
            strip: true,
            w: w + 2.0 * s,
            h: h + 2.0 * s,
            card,
            radius: d.radius,
            art,
            art_radius: 8.0,
            title: rect(cl, cy - 19.0, cr, cy + 1.0),
            artist: rect(cl, cy, cr, cy + 18.0),
            buttons,
            bar,
            times: false,
        }
    }

    fn size_px(&self, l: &Layout) -> (i32, i32) {
        let k = self.k();
        ((l.w * k).ceil() as i32, (l.h * k).ceil() as i32)
    }

    /// Pencerenin ekrandaki sol üst köşesi: şeritte dock'un yanı, kartta kayıtlı yer.
    fn origin(&self, l: &Layout) -> POINT {
        let Some(d) = self.strip_dock() else { return self.pos };
        let k = self.k();
        let (w, _) = self.size_px(l);
        let edge = ((d.margin - STRIP_PAD) * k) as i32;
        let x = match self.o.place {
            Place::DockRight => d.monitor.right - edge - w,
            _ => d.monitor.left + edge,
        };
        let y = d.monitor.bottom - ((d.margin + d.pill_h + STRIP_PAD) * k) as i32;
        POINT { x, y }
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        let l = self.layout();
        if !inside(&l.card, x, y) {
            return Hit::None;
        }
        if self.now.active {
            for (h, cx, cy, r) in l.buttons {
                if (x - cx).powi(2) + (y - cy).powi(2) <= (r + 4.0).powi(2) {
                    return h;
                }
            }
            if let Some(b) = l.bar
                && self.now.can_seek
                && x >= b.left - 4.0
                && x <= b.right + 4.0
                && (y - (b.top + b.bottom) / 2.0).abs() <= if l.strip { 5.0 } else { 9.0 }
            {
                return Hit::Bar;
            }
        }
        Hit::Card
    }

    /// Şerit yerine sığmıyorsa (dock çok geniş) gizlenir.
    fn fits(&self) -> bool {
        self.strip_dock().is_none_or(|d| self.strip_room(d) >= STRIP_MIN)
    }

    // --- Zamanlayıcılar ---

    /// Çalarken saniyede bir çizilir (ilerleme), değilken hiç. Dock'un yanındayken dock'un
    /// yerine iki saniyede bir bakılır.
    fn sync_timer(&mut self) {
        let want = self.now.active && self.now.playing && self.o.progress;
        if want != self.ticking {
            self.ticking = want;
            unsafe {
                if want {
                    SetTimer(Some(self.hwnd), TIMER_TICK, 1000, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
                }
            }
        }
        unsafe {
            if self.o.place == Place::Desktop {
                let _ = KillTimer(Some(self.hwnd), TIMER_DOCK);
            } else {
                SetTimer(Some(self.hwnd), TIMER_DOCK, 2000, None);
            }
        }
    }

    fn tick(&mut self) {
        // Görünmüyorsa (gizli, tam ekran bir uygulama önde) çizmeye gerek yok.
        let hidden = unsafe { !IsWindowVisible(self.hwnd).as_bool() };
        if hidden || unsafe { fullscreen(GetForegroundWindow()) } {
            return;
        }
        if self.now.position_now() as i64 != self.drawn_sec {
            self.paint();
        }
    }

    /// Dock'un yeri değiştiyse (simge eklendi, ekran değişti, dock açıldı / kapandı) yerleşir.
    fn dock_check(&mut self) {
        let d = crate::hatter::geometry();
        let same = match (&d, &self.dock) {
            (Some(a), Some(b)) => {
                a.left == b.left && a.right == b.right && a.monitor == b.monitor && a.dpi == b.dpi && a.pill_h == b.pill_h
                    && a.theme.dark == b.theme.dark
            }
            (None, None) => true,
            _ => false,
        };
        if !same {
            let was = self.strip_dock().is_some();
            self.dock = d;
            if was != self.strip_dock().is_some() {
                self.place_z();
            }
            self.surface = None;
            self.bitmaps.shadow = None;
            self.bitmaps.ambient = None;
            self.sync_shown();
            self.paint();
        }
    }

    // --- Çizim ---

    fn ensure_target(&mut self) -> bool {
        if self.target.is_some() {
            return true;
        }
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let t = unsafe {
            (|| -> windows::core::Result<Target> {
                let rt = self.d2d.CreateDCRenderTarget(&props)?;
                let brush = rt.CreateSolidColorBrush(&color(0xffffff, 1.0), None)?;
                rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                Ok(Target { rt, brush })
            })()
        };
        match t {
            Ok(t) => {
                self.target = Some(t);
                self.bitmaps = Bitmaps::default();
                true
            }
            Err(e) => {
                crate::log!("müzik: çizim hedefi kurulamadı: {e}");
                false
            }
        }
    }

    fn bitmap(rt: &ID2D1DCRenderTarget, w: u32, h: u32, px: &[u8]) -> Option<ID2D1Bitmap> {
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        unsafe { rt.CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(px.as_ptr().cast()), w * 4, &props).ok() }
    }

    /// Bitmap'i `r`'ye gerili dolduran fırça.
    fn brush_for(rt: &ID2D1DCRenderTarget, bmp: &ID2D1Bitmap, r: &D2D_RECT_F, opacity: f32) -> Option<ID2D1BitmapBrush> {
        unsafe {
            let size = bmp.GetSize();
            let props = D2D1_BITMAP_BRUSH_PROPERTIES {
                extendModeX: D2D1_EXTEND_MODE_CLAMP,
                extendModeY: D2D1_EXTEND_MODE_CLAMP,
                interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            };
            let bp = D2D1_BRUSH_PROPERTIES { opacity, transform: Matrix3x2::identity() };
            let b = rt.CreateBitmapBrush(bmp, Some(&props), Some(&bp)).ok()?;
            let m = Matrix3x2 {
                M11: (r.right - r.left) / size.width,
                M12: 0.0,
                M21: 0.0,
                M22: (r.bottom - r.top) / size.height,
                M31: r.left,
                M32: r.top,
            };
            b.SetTransform(&m);
            Some(b)
        }
    }

    fn linear(
        rt: &ID2D1DCRenderTarget,
        from: (f32, f32),
        to: (f32, f32),
        stops: &[(f32, u32, f32)],
    ) -> Option<ID2D1LinearGradientBrush> {
        let s: Vec<D2D1_GRADIENT_STOP> =
            stops.iter().map(|&(p, c, a)| D2D1_GRADIENT_STOP { position: p, color: color(c, a) }).collect();
        unsafe {
            let stops = rt.CreateGradientStopCollection(&s, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).ok()?;
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                startPoint: Vector2 { X: from.0, Y: from.1 },
                endPoint: Vector2 { X: to.0, Y: to.1 },
            };
            rt.CreateLinearGradientBrush(&props, None, &stops).ok()
        }
    }

    fn paint(&mut self) {
        if !self.ensure_target() || !self.fits() {
            return;
        }
        let l = self.layout();
        let (wp, hp) = self.size_px(&l);
        if self.surface.as_ref().is_none_or(|s| s.w != wp || s.h != hp) {
            self.surface = Surface::new(wp, hp);
        }
        let Some(dc) = self.surface.as_ref().map(|s| s.dc) else { return };
        let k = self.k();
        let card = l.card;
        let origin = self.origin(&l);

        // Hedefe bağlı bitmap'ler: gölge (boyut değişince), kapak ve bulanık hali.
        {
            let rt = &self.target.as_ref().unwrap().rt;
            if self.bitmaps.shadow.is_none() || self.bitmaps.shadow_key != (wp, hp, l.strip) {
                let c = (card.left * k, card.top * k, card.right * k, card.bottom * k);
                let (sigma, dy, a) = if l.strip { (4.0, 1.5, 0.28) } else { (14.0, 6.0, 0.32) };
                let px = look::shadow(wp as usize, hp as usize, c, l.radius * k, sigma * k, dy * k, a);
                self.bitmaps.shadow = Self::bitmap(rt, wp as u32, hp as u32, &px);
                self.bitmaps.shadow_key = (wp, hp, l.strip);
            }
            if let Some(a) = &self.art {
                if self.bitmaps.art.is_none() {
                    self.bitmaps.art = Self::bitmap(rt, a.w, a.h, &a.px);
                }
                if self.bitmaps.ambient.is_none() || self.bitmaps.ambient_strip != l.strip {
                    let (aw, ah, px) = if l.strip { &a.ambient_strip } else { &a.ambient_card };
                    self.bitmaps.ambient = Self::bitmap(rt, *aw as u32, *ah as u32, px);
                    self.bitmaps.ambient_strip = l.strip;
                }
            }
        }

        let pos = self.now.position_now();
        self.drawn_sec = pos as i64;
        let active = self.now.active;
        let art_color = self.art.as_ref().filter(|_| active).map(|a| a.color);
        let opacity = self.o.opacity;
        // Dock'un yanında düz arka plan dock'un rengindedir (açık temada açık, yazı koyu).
        let theme = self.strip_dock().map(|d| d.theme);
        let plain = self.o.style == Style::Plain || (self.o.style == Style::Color && art_color.is_none())
            || (self.o.style == Style::Cover && (self.bitmaps.ambient.is_none() || !active));
        let fg = match theme {
            Some(t) if plain && !t.dark => 0x000000,
            _ => 0xffffff,
        };
        let light = fg == 0;

        let t = self.target.as_ref().unwrap();
        let (rt, brush) = (&t.rt, &t.brush);
        let f = &self.f;
        let fill = |r: D2D1_ROUNDED_RECT, c: D2D1_COLOR_F| unsafe {
            brush.SetColor(&c);
            rt.FillRoundedRectangle(&r, brush);
        };
        let text = |s: &str, tf: &IDWriteTextFormat, r: D2D_RECT_F, a: f32| unsafe {
            let s: Vec<u16> = s.encode_utf16().collect();
            if !light {
                // Saydam bir arka planda da okunsun: yazının altında hafif gölge.
                brush.SetColor(&color(0, 0.3 * a));
                let sr = rect(r.left, r.top + 1.0, r.right, r.bottom + 1.0);
                rt.DrawText(&s, tf, &sr, brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
            }
            brush.SetColor(&color(fg, if light { a * 0.9 } else { a }));
            rt.DrawText(&s, tf, &r, brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        };
        let card_r = rounded(card, l.radius);

        let ok = unsafe {
            if rt.BindDC(dc, &RECT { left: 0, top: 0, right: wp, bottom: hp }).is_err() {
                return;
            }
            rt.SetDpi(96.0 * k, 96.0 * k);
            rt.BeginDraw();
            rt.Clear(Some(&color(0, 0.0)));

            if let Some(s) = &self.bitmaps.shadow {
                let r = rect(0.0, 0.0, wp as f32 / k, hp as f32 / k);
                rt.DrawBitmap(s, Some(&r), opacity, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }

            // Arka plan.
            let ambient = self.bitmaps.ambient.as_ref().filter(|_| active && self.o.style == Style::Cover);
            match (ambient.and_then(|a| Self::brush_for(rt, a, &card, opacity)), art_color, theme) {
                (Some(b), _, _) => rt.FillRoundedRectangle(&card_r, &b),
                (None, Some(c), _) if self.o.style == Style::Color => {
                    let stops = [(0.0, shade(c, 0.62), opacity), (1.0, shade(c, 0.26), opacity)];
                    if let Some(g) = Self::linear(rt, (card.left, card.top), (card.right, card.bottom), &stops) {
                        rt.FillRoundedRectangle(&card_r, &g);
                    }
                }
                (None, _, Some(th)) => {
                    // Dock gibi: temanın rengi, ince kenarlık.
                    fill(card_r, color(th.base, 0.88 * opacity));
                }
                (None, _, None) => {
                    let stops = [(0.0, 0x26262a, opacity), (1.0, 0x18181b, opacity)];
                    if let Some(g) = Self::linear(rt, (card.left, card.top), (card.left, card.bottom), &stops) {
                        rt.FillRoundedRectangle(&card_r, &g);
                    }
                }
            }
            if !plain {
                // Yazının altı biraz daha koyu (sağa doğru).
                let stops = [(0.0, 0, 0.0), (0.35, 0, 0.10 * opacity), (1.0, 0, 0.28 * opacity)];
                if let Some(g) = Self::linear(rt, (card.left, card.top), (card.right, card.top), &stops) {
                    rt.FillRoundedRectangle(&card_r, &g);
                }
            }
            // Kenar: dock'un yanında dock'unki gibi düz, kartta üstten ışık alan.
            let edge = rect(card.left + 0.5, card.top + 0.5, card.right - 0.5, card.bottom - 0.5);
            match theme {
                Some(th) => {
                    brush.SetColor(&color(th.stroke.0, th.stroke.1 + 0.03));
                    rt.DrawRoundedRectangle(&rounded(edge, l.radius - 0.5), brush, 1.0, None);
                }
                None => {
                    let stops = [(0.0, 0xffffff, 0.16), (0.5, 0xffffff, 0.05), (1.0, 0xffffff, 0.08)];
                    if let Some(g) = Self::linear(rt, (card.left, card.top), (card.left, card.bottom), &stops) {
                        rt.DrawRoundedRectangle(&rounded(edge, l.radius - 0.5), &g, 1.0, None);
                    }
                }
            }

            // Kapak (yoksa bir nota).
            let ar = l.art;
            match self.bitmaps.art.as_ref().filter(|_| active).and_then(|a| Self::brush_for(rt, a, &ar, 1.0)) {
                Some(b) => {
                    if !l.strip {
                        let sh = rect(ar.left, ar.top + 3.0, ar.right, ar.bottom + 3.0);
                        fill(rounded(sh, l.art_radius), color(0, 0.25));
                    }
                    rt.FillRoundedRectangle(&rounded(ar, l.art_radius), &b);
                    brush.SetColor(&color(0xffffff, 0.10));
                    let r = rect(ar.left + 0.5, ar.top + 0.5, ar.right - 0.5, ar.bottom - 0.5);
                    rt.DrawRoundedRectangle(&rounded(r, l.art_radius - 0.5), brush, 1.0, None);
                }
                None => {
                    fill(rounded(ar, l.art_radius), color(fg, 0.08));
                    brush.SetColor(&color(fg, 0.6));
                    let s: Vec<u16> = ICON_MUSIC.encode_utf16().collect();
                    let gf = if l.strip { &f.glyph } else { &f.glyph_big };
                    rt.DrawText(&s, gf, &ar, brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                }
            }

            let (tf, af) = if l.strip { (&f.title_s, &f.artist_s) } else { (&f.title, &f.artist) };
            if active {
                text(&self.now.title, tf, l.title, 1.0);
                text(&self.now.artist, af, l.artist, 0.7);

                for (h, cx, cy, rad) in l.buttons {
                    let enabled = match h {
                        Hit::Prev => self.now.can_prev,
                        Hit::Next => self.now.can_next,
                        _ => true,
                    };
                    let hovered = self.hover == h && enabled;
                    let pressed = hovered && self.pressed == h;
                    let icon = match h {
                        Hit::Prev => ICON_PREV,
                        Hit::Next => ICON_NEXT,
                        _ if self.now.playing => ICON_PAUSE,
                        _ => ICON_PLAY,
                    };
                    let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: rad, radiusY: rad };
                    // Çal düğmesi dolu daire, ters renk simge; diğerleri üstüne gelince belirir.
                    let back = if light { 0xffffff } else { 0x111111 };
                    let ink = if h == Hit::Play {
                        brush.SetColor(&color(fg, if pressed { 0.75 } else if hovered { 1.0 } else { 0.92 }));
                        rt.FillEllipse(&e, brush);
                        color(back, 1.0)
                    } else {
                        if hovered {
                            brush.SetColor(&color(fg, if pressed { 0.2 } else { 0.12 }));
                            rt.FillEllipse(&e, brush);
                        }
                        color(fg, if enabled { 0.9 } else { 0.35 })
                    };
                    brush.SetColor(&ink);
                    let s: Vec<u16> = icon.encode_utf16().collect();
                    let r = rect(cx - 20.0, cy - 20.0, cx + 20.0, cy + 20.0);
                    let gf = if l.strip { &f.glyph_s } else { &f.glyph };
                    rt.DrawText(&s, gf, &r, brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                }

                if let Some(b) = l.bar
                    && self.now.duration > 0.0
                {
                    let cy = (b.top + b.bottom) / 2.0;
                    let thick = if self.hover == Hit::Bar { 2.5 } else if l.strip { 1.5 } else { 2.0 };
                    fill(rounded(rect(b.left, cy - thick, b.right, cy + thick), thick), color(fg, 0.2));
                    let x = b.left + (b.right - b.left) * (pos / self.now.duration).clamp(0.0, 1.0) as f32;
                    let done = rect(b.left, cy - thick, x.max(b.left + 2.0 * thick), cy + thick);
                    fill(rounded(done, thick), color(fg, 0.9));
                    if self.hover == Hit::Bar {
                        let e = D2D1_ELLIPSE { point: Vector2 { X: x, Y: cy }, radiusX: 5.0, radiusY: 5.0 };
                        brush.SetColor(&color(fg, 1.0));
                        rt.FillEllipse(&e, brush);
                    }
                    if l.times {
                        let cl = l.title.left;
                        text(&fmt_time(pos), &f.time_l, rect(cl, cy - 9.0, b.left - 6.0, cy + 9.0), 0.6);
                        let rest = format!("-{}", fmt_time(self.now.duration - pos));
                        text(&rest, &f.time_r, rect(b.right + 6.0, cy - 9.0, l.title.right, cy + 9.0), 0.6);
                    }
                }
            } else {
                let sub = t!("Not playing · click to open", "Çalmıyor · açmak için tıkla");
                text("Spotify", tf, l.title, 1.0);
                text(sub, af, l.artist, 0.65);
            }

            rt.EndDraw(None, None)
        };
        if let Err(e) = ok {
            if e.code() == D2DERR_RECREATE_TARGET {
                self.target = None;
                self.bitmaps = Bitmaps::default();
            }
            return;
        }
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
            ..Default::default()
        };
        let size = SIZE { cx: wp, cy: hp };
        unsafe {
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&origin),
                Some(&size),
                Some(dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }

    // --- Medya ---

    fn update(&mut self, now: Now) {
        if now.art_gen != self.art_gen {
            self.art_gen = now.art_gen;
            self.art = now.art.as_ref().and_then(|b| {
                let a = self.decode(b);
                if a.is_none() {
                    crate::log!("müzik: kapak çözülemedi ({} bayt)", b.len());
                }
                a
            });
            self.bitmaps.art = None;
            self.bitmaps.ambient = None;
        }
        if now.spotify {
            self.o.app = now.app.clone();
        }
        self.now = now;
        self.sync_shown();
        self.sync_timer();
        self.paint();
    }

    /// Kapak baytlarını 192 piksele küçültüp çözer.
    fn decode(&self, bytes: &[u8]) -> Option<Art> {
        unsafe {
            let stream = self.wic.CreateStream().ok()?;
            stream.InitializeFromMemory(bytes).ok()?;
            let dec = self.wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
            let frame = dec.GetFrame(0).ok()?;
            let (mut w, mut h) = (0, 0);
            frame.GetSize(&mut w, &mut h).ok()?;
            let side = 192u32;
            let (tw, th) =
                if w >= h { (side, (side * h / w.max(1)).max(1)) } else { ((side * w / h.max(1)).max(1), side) };
            let scaler = self.wic.CreateBitmapScaler().ok()?;
            scaler.Initialize(&frame, tw, th, WICBitmapInterpolationModeHighQualityCubic).ok()?;
            let conv = self.wic.CreateFormatConverter().ok()?;
            conv.Initialize(
                &scaler,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeMedianCut,
            )
            .ok()?;
            let mut px = vec![0u8; (tw * th * 4) as usize];
            conv.CopyPixels(std::ptr::null(), tw * 4, &mut px).ok()?;
            let color = dominant(&px);
            let ambient_card = look::ambient(&px, tw as usize, th as usize, CARD_W / CARD_H);
            let ambient_strip = look::ambient(&px, tw as usize, th as usize, STRIP_W / 64.0);
            Some(Art { w: tw, h: th, px, color, ambient_card, ambient_strip })
        }
    }

    /// Görünür mü: hiçbir şey çalmıyorken (istenirse), şerit tam ekranda ya da sığmıyorken gizli.
    fn sync_shown(&self) {
        let show = (self.now.active || !self.o.hide_idle) && !self.ducked && self.fits();
        unsafe {
            if show != IsWindowVisible(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, if show { SW_SHOWNOACTIVATE } else { SW_HIDE });
                if show {
                    self.place_z();
                }
            }
        }
    }

    /// Katmanı: şerit (ve "Masaüstünü göster" açıkken kart) en üstte; kart masaüstünün hemen
    /// üstünde (bütün pencerelerin altında, HWND_BOTTOM olsa altına düşeceği Progman'ın üstünde).
    fn place_z(&self) {
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
        unsafe {
            if self.strip_dock().is_some() || self.peek {
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
                return;
            }
            let _ = SetWindowPos(self.hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags);
            if let Some(after) = insert_after(self.hwnd) {
                let _ = SetWindowPos(self.hwnd, Some(after), 0, 0, 0, 0, flags);
            }
        }
    }

    // --- Fare ---

    fn dip(&self, lp: LPARAM) -> (f32, f32) {
        let x = (lp.0 & 0xffff) as i16 as f32;
        let y = ((lp.0 >> 16) & 0xffff) as i16 as f32;
        (x / self.k(), y / self.k())
    }

    fn mouse_move(&mut self, lp: LPARAM) {
        if !self.tracking {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            self.tracking = true;
        }
        if let Some((start, origin, moved)) = self.drag {
            let mut p = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut p);
            }
            let (dx, dy) = (p.x - start.x, p.y - start.y);
            if moved || dx.abs() > 4 || dy.abs() > 4 {
                self.drag = Some((start, origin, true));
                self.pos = POINT { x: origin.x + dx, y: origin.y + dy };
                let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;
                unsafe {
                    let _ = SetWindowPos(self.hwnd, None, self.pos.x, self.pos.y, 0, 0, flags);
                }
            }
            return;
        }
        let (x, y) = self.dip(lp);
        let h = self.hit(x, y);
        if h != self.hover {
            self.hover = h;
            self.paint();
        }
    }

    fn mouse_down(&mut self, lp: LPARAM) {
        let (x, y) = self.dip(lp);
        let h = self.hit(x, y);
        if h == Hit::None {
            return;
        }
        self.pressed = h;
        unsafe {
            let _ = SetCapture(self.hwnd);
        }
        // Yalnızca masaüstü kartı sürüklenir; şerit dock'un yanında sabit.
        if h == Hit::Card && self.strip_dock().is_none() {
            let mut p = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut p);
            }
            self.drag = Some((p, self.pos, false));
        }
        self.paint();
    }

    fn mouse_up(&mut self, lp: LPARAM) {
        unsafe {
            let _ = ReleaseCapture();
        }
        let pressed = std::mem::replace(&mut self.pressed, Hit::None);
        if let Some((_, _, moved)) = self.drag.take()
            && moved
        {
            (self.o.on_moved)(self.pos.x, self.pos.y);
            self.paint();
            return;
        }
        let (x, y) = self.dip(lp);
        let h = self.hit(x, y);
        if h != pressed {
            self.paint();
            return;
        }
        match h {
            Hit::Prev if self.now.can_prev => self.remote.send(Cmd::Prev),
            Hit::Next if self.now.can_next => self.remote.send(Cmd::Next),
            Hit::Play => {
                // Hemen tepki: oynatıcı bildirene kadar düğme yeni durumu gösterir.
                self.now.position = self.now.position_now();
                self.now.at = Some(Instant::now());
                self.now.playing = !self.now.playing;
                self.remote.send(Cmd::PlayPause);
                self.sync_timer();
            }
            Hit::Bar => {
                if let Some(b) = self.layout().bar {
                    let t = ((x - b.left) / (b.right - b.left)).clamp(0.0, 1.0) as f64 * self.now.duration;
                    self.now.position = t;
                    self.now.at = Some(Instant::now());
                    self.remote.send(Cmd::Seek(t));
                }
            }
            Hit::Card if self.now.active => open_player(&self.now.app),
            Hit::Card => open_player(&self.o.app),
            _ => {}
        }
        self.paint();
    }

    fn mouse_leave(&mut self) {
        self.tracking = false;
        if self.hover != Hit::None {
            self.hover = Hit::None;
            self.paint();
        }
    }

    // --- Ön plan değişti ---

    fn foreground(&mut self, fg: HWND) {
        if self.strip_dock().is_some() {
            // Şerit tam ekran oyunun, videonun üstünde durmaz (dock da saklanır).
            let duck = fullscreen(fg) && fg != self.hwnd;
            if duck != self.ducked {
                self.ducked = duck;
                self.sync_shown();
            }
            return;
        }
        let peek = is_desktop(fg) && desktop_on_top(fg, self.hwnd);
        if peek != self.peek {
            self.peek = peek;
            self.place_z();
        }
    }

    fn apply(&mut self, o: Options) {
        let app = if o.app.is_empty() { self.o.app.clone() } else { o.app.clone() };
        let moved = o.place != self.o.place || o.scale != self.o.scale;
        self.o = Options { app, ..o };
        if moved {
            self.surface = None;
            self.bitmaps.shadow = None;
            self.bitmaps.ambient = None;
            self.peek = false;
            self.ducked = false;
            self.dock = crate::hatter::geometry();
            self.place_z();
        }
        self.sync_shown();
        self.sync_timer();
        self.paint();
    }
}

/// Masaüstü pencerelerinin (Progman, WorkerW) en üsttekinin hemen üstündeki pencere: kart
/// bunun altına girer. Kart zaten oradaysa `None`.
fn insert_after(ours: HWND) -> Option<HWND> {
    unsafe {
        // En alttaki üst düzey pencereden yukarı: masaüstü pencereleri (ve kart) bitene kadar.
        let first = GetWindow(GetDesktopWindow(), GW_CHILD).ok()?;
        let mut h = GetWindow(first, GW_HWNDLAST).ok()?;
        let mut desk = None;
        while !h.is_invalid() && (h == ours || is_desktop(h)) {
            if h != ours {
                desk = Some(h);
            }
            h = GetWindow(h, GW_HWNDPREV).unwrap_or_default();
        }
        let above = GetWindow(desk?, GW_HWNDPREV).ok()?;
        (above != ours && !above.is_invalid()).then_some(above)
    }
}

fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn is_desktop(h: HWND) -> bool {
    matches!(class_name(h).as_str(), "WorkerW" | "Progman")
}

/// Masaüstü penceresinin üstünde görünen sıradan bir pencere kalmamış mı ("Masaüstünü göster").
fn desktop_on_top(desktop: HWND, ours: HWND) -> bool {
    unsafe {
        let mut h = GetWindow(desktop, GW_HWNDPREV).unwrap_or_default();
        while !h.is_invalid() {
            let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as _, 4);
            let mut r = RECT::default();
            let _ = GetWindowRect(h, &mut r);
            if h != ours
                && IsWindowVisible(h).as_bool()
                && !IsIconic(h).as_bool()
                && ex & (WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) == 0
                && cloaked == 0
                && r.right > r.left
                && r.bottom > r.top
            {
                return false;
            }
            h = GetWindow(h, GW_HWNDPREV).unwrap_or_default();
        }
        true
    }
}

/// Oynatıcıyı açar ya da öne getirir: Başlat menüsündeki uygulamalar (Store'daki Spotify,
/// tarayıcıya kurulan Spotify) kimlikleriyle, masaüstü Spotify'ı spotify: adresiyle.
fn open_player(app: &str) {
    let target = if app.is_empty() || app.eq_ignore_ascii_case("Spotify.exe") {
        "spotify:".to_string()
    } else {
        format!(r"shell:AppsFolder\{app}")
    };
    let t = crate::util::wide(&target);
    unsafe {
        let _ = ShellExecuteW(None, w!("open"), PCWSTR(t.as_ptr()), None, None, SW_SHOWNORMAL);
    }
}

unsafe extern "system" fn on_foreground(_: HWINEVENTHOOK, _: u32, hwnd: HWND, _: i32, _: i32, _: u32, _: u32) {
    with(|w| w.foreground(hwnd));
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let handled = with(|w| {
        match msg {
            WM_TIMER if wp.0 == TIMER_TICK => w.tick(),
            WM_TIMER if wp.0 == TIMER_DOCK => w.dock_check(),
            WM_MOUSEMOVE => w.mouse_move(lp),
            WM_LBUTTONDOWN => w.mouse_down(lp),
            WM_LBUTTONUP => w.mouse_up(lp),
            WM_MOUSELEAVE => w.mouse_leave(),
            WM_SETCURSOR => {
                let hand = matches!(w.hover, Hit::Prev | Hit::Play | Hit::Next | Hit::Bar);
                unsafe {
                    let _ = SetCursor(LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }).ok());
                }
                return Some(LRESULT(1));
            }
            WM_WINDOWPOSCHANGING => {
                // Masaüstü kartı pencerelerin altında kalır.
                let p = unsafe { &mut *(lp.0 as *mut WINDOWPOS) };
                if w.strip_dock().is_none() && !w.peek && p.flags & SWP_NOZORDER == SET_WINDOW_POS_FLAGS(0) {
                    match insert_after(w.hwnd) {
                        Some(after) => p.hwndInsertAfter = after,
                        None => p.flags |= SWP_NOZORDER,
                    }
                }
                return None;
            }
            WM_DPICHANGED => {
                w.dpi = (wp.0 & 0xffff) as f32;
                w.surface = None;
                w.bitmaps.shadow = None;
                w.paint();
            }
            _ => return None,
        }
        Some(LRESULT(0))
    })
    .flatten();
    handled.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
}

// --- Dışarıya ---

/// Masaüstü kartının varsayılan yeri: birincil ekranın sağ üstü.
fn default_pos(w: i32, dpi: f32) -> POINT {
    let mut wa = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    let m = (24.0 * dpi / 96.0) as i32;
    POINT { x: wa.right - w - m, y: wa.top + m }
}

fn fonts() -> windows::core::Result<Fonts> {
    unsafe {
        let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let fmt = |family: PCWSTR, size: f32, weight, align| -> windows::core::Result<IDWriteTextFormat> {
            let f = dw.CreateTextFormat(family, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("tr-tr"))?;
            f.SetTextAlignment(align)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let sign = dw.CreateEllipsisTrimmingSign(&f)?;
            let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            f.SetTrimming(&trim, &sign)?;
            Ok(f)
        };
        let (lead, center, right) = (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_TRAILING);
        let (normal, semi) = (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD);
        let (display, ui, small, icons) =
            (w!("Segoe UI Variable Display"), w!("Segoe UI Variable Text"), w!("Segoe UI Variable Small"), w!("Segoe Fluent Icons"));
        Ok(Fonts {
            title: fmt(display, 16.0, semi, lead)?,
            artist: fmt(ui, 13.0, normal, lead)?,
            title_s: fmt(ui, 13.0, semi, lead)?,
            artist_s: fmt(ui, 12.0, normal, lead)?,
            time_l: fmt(small, 11.0, normal, lead)?,
            time_r: fmt(small, 11.0, normal, right)?,
            glyph: fmt(icons, 16.0, normal, center)?,
            glyph_s: fmt(icons, 13.0, normal, center)?,
            glyph_big: fmt(icons, 26.0, normal, center)?,
        })
    }
}

fn create(remote: Remote, o: Options) -> windows::core::Result<Widget> {
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: inst.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS,
            w!("hive music"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        // Kayıtlı yer hâlâ bir ekrandaysa orası.
        let pos = o
            .pos
            .map(|(x, y)| POINT { x, y })
            .filter(|&p| !MonitorFromPoint(POINT { x: p.x + 40, y: p.y + 40 }, MONITOR_DEFAULTTONULL).is_invalid());
        let probe = pos.unwrap_or_else(|| default_pos(0, 96.0));
        let _ = SetWindowPos(hwnd, None, probe.x, probe.y, 1, 1, SWP_NOZORDER | SWP_NOACTIVATE);
        let dpi = GetDpiForWindow(hwnd).max(96) as f32;

        let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let mut w = Widget {
            hwnd,
            remote,
            o,
            d2d,
            wic,
            f: fonts()?,
            target: None,
            surface: None,
            bitmaps: Bitmaps::default(),
            dpi,
            pos: POINT::default(),
            dock: crate::hatter::geometry(),
            now: Now::default(),
            art: None,
            art_gen: 0,
            ticking: false,
            drawn_sec: -1,
            hover: Hit::None,
            pressed: Hit::None,
            drag: None,
            tracking: false,
            peek: false,
            ducked: false,
            hook: HWINEVENTHOOK::default(),
        };
        let (wp, _) = w.size_px(&w.card_layout());
        w.pos = pos.unwrap_or_else(|| default_pos(wp, dpi));
        Ok(w)
    }
}

/// Widget'ı açar (açıksa ayarları uygular). Arayüz iş parçacığından.
pub fn start(remote: Remote, o: Options, now: Now) {
    let o2 = o.clone();
    if with(|w| w.apply(o2)).is_some() {
        return;
    }
    let mut w = match create(remote, o) {
        Ok(w) => w,
        Err(e) => {
            crate::log!("müzik: widget açılamadı: {e}");
            return;
        }
    };
    unsafe {
        w.hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
    }
    W.with(|c| *c.borrow_mut() = Some(w));
    update(now);
}

pub fn stop() {
    let w = W.with(|c| c.borrow_mut().take());
    if let Some(w) = w {
        unsafe {
            let _ = KillTimer(Some(w.hwnd), TIMER_TICK);
            let _ = KillTimer(Some(w.hwnd), TIMER_DOCK);
            if !w.hook.is_invalid() {
                let _ = UnhookWinEvent(w.hook);
            }
            let _ = DestroyWindow(w.hwnd);
        }
    }
}

pub fn update(now: Now) {
    with(|w| w.update(now));
}

/// Masaüstü kartını varsayılan yerine (sağ üst) taşır.
pub fn reset_position() {
    with(|w| {
        let (wp, _) = w.size_px(&w.card_layout());
        w.pos = default_pos(wp, w.dpi);
        (w.o.on_moved)(w.pos.x, w.pos.y);
        w.paint();
    });
}

/// Pencere göstermeden widget'ı örnek bir zeminin üstünde çizip PNG'ye yazar (test komutu).
/// `strip`: dock'un yanındaki şerit (sahte bir dock'la), değilse masaüstü kartı.
pub fn preview(path: &std::path::Path, style: Style, strip: bool, real: Option<Now>) -> windows::core::Result<()> {
    let o = Options {
        place: if strip { Place::DockLeft } else { Place::Desktop },
        hide_idle: false,
        app: String::new(),
        pos: Some((0, 0)),
        on_moved: |_, _| {},
        scale: 1.0,
        style,
        opacity: 0.92,
        progress: true,
    };
    let mut w = create(Remote::dummy(), o)?;
    w.dpi = 144.0;
    w.dock = strip.then(|| Geometry {
        monitor: RECT { left: 0, top: 0, right: 2880, bottom: 1800 },
        dpi: 144.0,
        pill_h: 66.0,
        margin: 8.0,
        radius: 12.0,
        left: 900,
        right: 1980,
        theme: crate::hatter::theme::current(),
    });
    let l = w.layout();
    let (wp, hp) = w.size_px(&l);
    let bg = |x: f32, y: f32| -> [f32; 3] {
        let (u, v) = (x / wp as f32, y / hp as f32);
        [0.25 + 0.35 * u, 0.3 + 0.1 * v, 0.5 + 0.2 * v]
    };
    let sample: &[u8] = include_bytes!("../../assets/araclar/cheshire-128.png");
    let art = real.as_ref().and_then(|n| n.art.clone());
    w.art = w.decode(art.as_deref().map_or(sample, |a| a.as_slice()));
    w.now = real.unwrap_or(Now {
        active: true,
        spotify: true,
        app: String::new(),
        title: "Midnight City".into(),
        artist: "M83".into(),
        playing: true,
        can_prev: true,
        can_next: true,
        can_seek: true,
        position: 83.0,
        duration: 243.0,
        at: Some(Instant::now()),
        art: None,
        art_gen: 0,
    });
    w.hover = Hit::Next;
    w.paint();

    let s = w.surface.as_ref().expect("yüzey");
    let mut out = vec![0u8; (wp * hp * 4) as usize];
    for y in 0..hp {
        for x in 0..wp {
            let c = bg(x as f32, y as f32);
            let src = s.pixel(x, y);
            let a = src[3] as f32 / 255.0;
            let i = ((y * wp + x) * 4) as usize;
            for ch in 0..3 {
                out[i + ch] = (src[ch] as f32 + c[2 - ch] * 255.0 * (1.0 - a)).min(255.0) as u8;
            }
            out[i + 3] = 255;
        }
    }
    unsafe {
        let _ = DestroyWindow(w.hwnd);
        let bmp = w.wic.CreateBitmapFromMemory(wp as u32, hp as u32, &GUID_WICPixelFormat32bppBGRA, wp as u32 * 4, &out)?;
        let stream = w.wic.CreateStream()?;
        let wide = crate::util::wide(&path.display().to_string());
        stream.InitializeFromFilename(PCWSTR(wide.as_ptr()), GENERIC_WRITE.0)?;
        let enc = w.wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        enc.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.unwrap();
        frame.Initialize(None)?;
        frame.SetSize(wp as u32, hp as u32)?;
        let mut fmt = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut fmt)?;
        frame.WriteSource(&bmp, std::ptr::null())?;
        frame.Commit()?;
        enc.Commit()?;
    }
    Ok(())
}
