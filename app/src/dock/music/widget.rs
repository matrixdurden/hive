//! Çalan şarkının widget'ı: dock'un solundaki boşlukta, dock'la aynı boyda bir şerit. Kapak,
//! şarkı, sanatçı, önceki / çal / sonraki ve ince bir ilerleme çizgisi. Dock ekranın altında yer
//! ayırdığı için hiçbir pencerenin altında kalmaz; tam ekran oyun ve videoda gizlenir. Dock
//! kapalıyken görünmez.
//!
//! Arka planı kapağın bulanık hali, kapağın rengi ya da dock'un kendi rengi; saydamlığı
//! ayarlanır. Düğmeler yazı tipinden değil, vektör şekillerden çizilir.
//!
//! Katmanlı pencere: Direct2D ile bellekteki bitmap'e çizilir, UpdateLayeredWindow ile tek
//! seferde verilir. Yalnızca bir şey değişince çizilir: şarkı, fare, çalarken saniyede bir.

use std::cell::RefCell;
use std::time::Instant;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};
use windows_numerics::{Matrix3x2, Vector2};

use super::look;
use super::media::{Cmd, Now, Remote};
use crate::dock::Geometry;

const CLASS: PCWSTR = w!("hive-music");
const TIMER_TICK: usize = 1;
/// Dock'un yeri arada bir okunur (simge eklenince hap genişler, ekran değişir).
const TIMER_DOCK: usize = 2;
/// Kapak geçişi (kare kare) ve yeni şarkının kapağını bekleme süresi.
const TIMER_FADE: usize = 3;
const TIMER_WAIT: usize = 4;
const FADE_MS: f32 = 280.0;
const WAIT_MS: u32 = 3000;
const WM_MOUSELEAVE: u32 = 0x02A3;

/// Şeridin en geniş hali ve sığmazsa gizlendiği en dar hali, çevresindeki gölge payı (DIP).
const STRIP_W: f32 = 340.0;
const STRIP_MIN: f32 = 220.0;
const PAD: f32 = 8.0;
const ART_RADIUS: f32 = 8.0;

/// Kartın arka planı.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    /// Kapağın bulanık, koyulaştırılmış hali.
    Cover,
    /// Kapağın baskın renginden geçiş.
    Color,
    /// Dock'un rengi.
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

/// Widget'ın ayarları (hive'ın sayfasından).
#[derive(Clone)]
pub struct Options {
    pub hide_idle: bool,
    /// Son görülen Spotify'ın uygulama kimliği: hiçbir şey çalmıyorken tıklayınca o açılır.
    pub app: String,
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
    w: f32,
    h: f32,
    card: D2D_RECT_F,
    art: D2D_RECT_F,
    title: D2D_RECT_F,
    artist: D2D_RECT_F,
    /// Düğmeler: (ne, merkez x, merkez y, yarıçap).
    buttons: [(Hit, f32, f32, f32); 3],
    bar: Option<D2D_RECT_F>,
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
    /// Düğme şekillerinin köşelerini yuvarlatan çizgi stili.
    round: ID2D1StrokeStyle,
}

/// Çözülmüş kapak: 32bppPBGRA pikseller, baskın rengi ve şerit oranında bulanık hali.
struct Art {
    w: u32,
    h: u32,
    px: Vec<u8>,
    color: u32,
    ambient: (usize, usize, Vec<u8>),
}

/// Hedefe bağlı bitmap'ler (hedef yeniden kurulunca düşer).
#[derive(Default)]
struct Bitmaps {
    shadow: Option<ID2D1Bitmap>,
    shadow_key: (i32, i32),
    art: Option<ID2D1Bitmap>,
    ambient: Option<ID2D1Bitmap>,
    /// Geçişte solan eski kapak.
    old_art: Option<ID2D1Bitmap>,
    old_ambient: Option<ID2D1Bitmap>,
}

struct Fonts {
    title: IDWriteTextFormat,
    artist: IDWriteTextFormat,
    glyph: IDWriteTextFormat,
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
    /// Dock'un yeri (açıksa); şerit onun soluna oturur.
    dock: Option<Geometry>,
    now: Now,
    /// Gösterilen kapak; yeni şarkının kapağı gelene kadar öncekininki kalır.
    art: Option<Art>,
    /// Geçişte solan kapak ve geçişin başladığı an.
    old: Option<Art>,
    fade: Option<Instant>,
    art_gen: u32,
    /// Gösterilen şarkı (başlık, sanatçı) ve kapağının gelip gelmediği.
    track: (String, String),
    track_art: bool,
    /// Saniyede bir çizen zamanlayıcı açık mı (yalnızca çalarken).
    ticking: bool,
    /// Son çizilen saniye.
    drawn_sec: i64,
    hover: Hit,
    pressed: Hit,
    tracking: bool,
    /// Tam ekran bir uygulama önde: geçici olarak gizli.
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

fn pt(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
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

fn is_desktop(h: HWND) -> bool {
    let mut buf = [0u16; 16];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    matches!(String::from_utf16_lossy(&buf[..n.max(0) as usize]).as_str(), "WorkerW" | "Progman")
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

/// Oynatıcıyı öne getirir (açık penceresi varsa, öndeyse küçültür); yoksa açar: Başlat
/// menüsündeki uygulamalar (Store'daki Spotify, tarayıcıya kurulan Spotify) kimlikleriyle,
/// masaüstü Spotify'ı spotify: adresiyle.
fn open_player(app: &str) {
    if !app.is_empty() && crate::dock::focus_app(app) {
        return;
    }
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

impl Widget {
    /// DIP başına piksel.
    fn k(&self) -> f32 {
        self.dock.as_ref().map_or(1.0, |d| d.dpi / 96.0)
    }

    /// Dock'un solundaki boşluğun genişliği (DIP).
    fn room(d: &Geometry) -> f32 {
        (d.left - d.monitor.left) as f32 / (d.dpi / 96.0) - 2.0 * d.margin
    }

    /// Dock açık ve şerit solundaki boşluğa sığıyor mu.
    fn fits(&self) -> bool {
        self.dock.as_ref().is_some_and(|d| Self::room(d) >= STRIP_MIN)
    }

    fn layout(&self) -> Layout {
        let (h, room) = self.dock.as_ref().map_or((66.0, STRIP_W), |d| (d.pill_h, Self::room(d)));
        let w = room.clamp(STRIP_MIN, STRIP_W);
        let card = rect(PAD, PAD, PAD + w, PAD + h);
        let inset = 6.0;
        let art = rect(card.left + inset, card.top + inset, card.left + h - inset, card.bottom - inset);
        let cy = (card.top + card.bottom) / 2.0 - if self.o.progress { 1.0 } else { 0.0 };
        let next = card.right - 22.0;
        let play = next - 38.0;
        let prev = play - 38.0;
        let buttons = [(Hit::Prev, prev, cy, 15.0), (Hit::Play, play, cy, 17.0), (Hit::Next, next, cy, 15.0)];
        let (cl, cr) = (art.right + 12.0, prev - 22.0);
        let bar = self.o.progress.then(|| rect(cl, card.bottom - 9.0, cr, card.bottom - 7.0));
        Layout {
            w: w + 2.0 * PAD,
            h: h + 2.0 * PAD,
            card,
            art,
            title: rect(cl, cy - 18.0, cr, cy + 1.0),
            artist: rect(cl, cy + 1.0, cr, cy + 18.0),
            buttons,
            bar,
        }
    }

    fn size_px(&self, l: &Layout) -> (i32, i32) {
        let k = self.k();
        ((l.w * k).ceil() as i32, (l.h * k).ceil() as i32)
    }

    /// Pencerenin ekrandaki sol üst köşesi: ekranın sol altında, dock'la aynı hizada.
    fn origin(&self) -> POINT {
        let Some(d) = &self.dock else { return POINT::default() };
        let k = self.k();
        POINT {
            x: d.monitor.left + ((d.margin - PAD) * k) as i32,
            y: d.monitor.bottom - ((d.margin + d.pill_h + PAD) * k) as i32,
        }
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        let l = self.layout();
        if !inside(&l.card, x, y) {
            return Hit::None;
        }
        if self.now.active {
            for (h, cx, cy, r) in l.buttons {
                if (x - cx).powi(2) + (y - cy).powi(2) <= (r + 3.0).powi(2) {
                    return h;
                }
            }
            if let Some(b) = l.bar
                && self.now.can_seek
                && x >= b.left
                && x <= b.right
                && (y - (b.top + b.bottom) / 2.0).abs() <= 5.0
            {
                return Hit::Bar;
            }
        }
        Hit::Card
    }

    // --- Zamanlayıcılar ---

    /// Çalarken saniyede bir çizilir (ilerleme), değilken hiç.
    fn sync_timer(&mut self) {
        let want = self.now.active && self.now.playing && self.o.progress;
        if want == self.ticking {
            return;
        }
        self.ticking = want;
        unsafe {
            if want {
                SetTimer(Some(self.hwnd), TIMER_TICK, 1000, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
            }
        }
    }

    fn tick(&mut self) {
        let hidden = unsafe { !IsWindowVisible(self.hwnd).as_bool() };
        if !hidden && self.now.position_now() as i64 != self.drawn_sec {
            self.paint();
        }
    }

    /// Dock'un yeri değiştiyse (simge eklendi, ekran değişti, dock açıldı / kapandı) yerleşir.
    fn dock_check(&mut self) {
        let d = crate::dock::geometry();
        let same = match (&d, &self.dock) {
            (Some(a), Some(b)) => {
                a.left == b.left
                    && a.monitor == b.monitor
                    && a.dpi == b.dpi
                    && a.pill_h == b.pill_h
                    && a.theme.dark == b.theme.dark
            }
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.dock = d;
            self.surface = None;
            self.bitmaps.shadow = None;
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
                let sp = D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    ..Default::default()
                };
                let round = self.d2d.CreateStrokeStyle(&sp, None)?;
                Ok(Target { rt, brush, round })
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
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(from.0, from.1), endPoint: pt(to.0, to.1) };
            rt.CreateLinearGradientBrush(&props, None, &stops).ok()
        }
    }

    /// Kapalı çokgenler (her biri bir şekil): düğme simgeleri.
    fn shape(&self, figures: &[&[Vector2]]) -> Option<ID2D1PathGeometry> {
        unsafe {
            let g = self.d2d.CreatePathGeometry().ok()?;
            let sink = g.Open().ok()?;
            for f in figures {
                sink.BeginFigure(f[0], D2D1_FIGURE_BEGIN_FILLED);
                sink.AddLines(&f[1..]);
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            }
            sink.Close().ok()?;
            Some(g)
        }
    }

    /// Düğmenin simgesi: çal üçgeni, duraklat çubukları, sonraki / önceki (üçgen ve çubuk).
    /// Köşeler aynı renkte yuvarlak bir çizgiyle yumuşatılır.
    fn icon(&self, h: Hit, cx: f32, cy: f32) -> Option<ID2D1PathGeometry> {
        let bar = |x0: f32, x1: f32, half: f32| -> [Vector2; 4] {
            [pt(x0, cy - half), pt(x1, cy - half), pt(x1, cy + half), pt(x0, cy + half)]
        };
        match h {
            Hit::Play if self.now.playing => {
                let (a, b) = (bar(cx - 4.6, cx - 1.6, 5.6), bar(cx + 1.6, cx + 4.6, 5.6));
                self.shape(&[&a, &b])
            }
            Hit::Play => self.shape(&[&[pt(cx - 3.2, cy - 6.0), pt(cx + 6.0, cy), pt(cx - 3.2, cy + 6.0)]]),
            Hit::Next => {
                let tri = [pt(cx - 5.5, cy - 5.5), pt(cx + 2.5, cy), pt(cx - 5.5, cy + 5.5)];
                self.shape(&[&tri, &bar(cx + 3.6, cx + 5.6, 5.5)])
            }
            _ => {
                let tri = [pt(cx + 5.5, cy - 5.5), pt(cx - 2.5, cy), pt(cx + 5.5, cy + 5.5)];
                self.shape(&[&tri, &bar(cx - 5.6, cx - 3.6, 5.5)])
            }
        }
    }

    fn paint(&mut self) {
        if !self.fits() || !self.ensure_target() {
            return;
        }
        let l = self.layout();
        let (wp, hp) = self.size_px(&l);
        if self.surface.as_ref().is_none_or(|s| s.w != wp || s.h != hp) {
            self.surface = Surface::new(wp, hp);
        }
        let Some(dc) = self.surface.as_ref().map(|s| s.dc) else { return };
        let Some(dock) = self.dock else { return };
        let k = self.k();
        let card = l.card;

        // Hedefe bağlı bitmap'ler: gölge (boyut değişince), kapak ve bulanık hali.
        {
            let rt = &self.target.as_ref().unwrap().rt;
            if self.bitmaps.shadow.is_none() || self.bitmaps.shadow_key != (wp, hp) {
                let c = (card.left * k, card.top * k, card.right * k, card.bottom * k);
                let px = look::shadow(wp as usize, hp as usize, c, dock.radius * k, 4.0 * k, 1.5 * k, 0.28);
                self.bitmaps.shadow = Self::bitmap(rt, wp as u32, hp as u32, &px);
                self.bitmaps.shadow_key = (wp, hp);
            }
            let b = &mut self.bitmaps;
            for (a, art, ambient) in [(&self.art, &mut b.art, &mut b.ambient), (&self.old, &mut b.old_art, &mut b.old_ambient)] {
                if let Some(a) = a {
                    if art.is_none() {
                        *art = Self::bitmap(rt, a.w, a.h, &a.px);
                    }
                    if ambient.is_none() {
                        let (aw, ah, px) = &a.ambient;
                        *ambient = Self::bitmap(rt, *aw as u32, *ah as u32, px);
                    }
                }
            }
        }

        let pos = self.now.position_now();
        self.drawn_sec = pos as i64;
        let active = self.now.active;
        let opacity = self.o.opacity;
        let th = dock.theme;
        let p = self.fade_t();
        // Arka planı kapaktan mı geliyor (koyu, beyaz yazı) yoksa dock'un renginde mi; geçişte
        // ağır basan katmana göre.
        let style = self.o.style;
        let uses_art = |a: &Option<Art>| active && style != Style::Plain && a.is_some();
        let from_art = if p < 0.5 && self.fade.is_some() { uses_art(&self.old) } else { uses_art(&self.art) };
        // Yazının altını koyulaştıran katmanın gücü (iki katmanın karışımı).
        let shade_k = match self.fade {
            Some(_) => (uses_art(&self.old) as u8 as f32) * (1.0 - p) + (uses_art(&self.art) as u8 as f32) * p,
            None => uses_art(&self.art) as u8 as f32,
        };
        let fg = if from_art || th.dark { 0xffffff } else { 0x000000 };
        let light = fg == 0;

        // Düğme şekilleri (çizimden önce: hedef ödünç alınmadan).
        let icons: Vec<_> = l.buttons.iter().map(|&(h, cx, cy, _)| self.icon(h, cx, cy)).collect();

        let t = self.target.as_ref().unwrap();
        let (rt, brush) = (&t.rt, &t.brush);
        let f = &self.f;
        let fill = |r: D2D1_ROUNDED_RECT, c: D2D1_COLOR_F| unsafe {
            brush.SetColor(&c);
            rt.FillRoundedRectangle(&r, brush);
        };
        let text = |s: &str, tf: &IDWriteTextFormat, r: D2D_RECT_F, a: f32| unsafe {
            let s: Vec<u16> = s.encode_utf16().collect();
            brush.SetColor(&color(fg, if light { a * 0.9 } else { a }));
            rt.DrawText(&s, tf, &r, brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        };
        let card_r = rounded(card, dock.radius);

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

            // Arka plan: kapak (bulanık), kapağın rengi ya da dock gibi. Geçişte eski katmanın
            // üstüne yenisi belirir.
            let background = |art: Option<&Art>, ambient: Option<&ID2D1Bitmap>, alpha: f32| {
                let art = art.filter(|_| active && style != Style::Plain);
                match (style, art, ambient) {
                    (Style::Cover, Some(_), Some(bmp)) => {
                        if let Some(b) = Self::brush_for(rt, bmp, &card, opacity * alpha) {
                            rt.FillRoundedRectangle(&card_r, &b);
                        }
                    }
                    (Style::Color, Some(a), _) => {
                        let stops = [(0.0, shade(a.color, 0.62), opacity * alpha), (1.0, shade(a.color, 0.26), opacity * alpha)];
                        if let Some(g) = Self::linear(rt, (card.left, card.top), (card.right, card.bottom), &stops) {
                            rt.FillRoundedRectangle(&card_r, &g);
                        }
                    }
                    _ => fill(card_r, color(th.base, 0.88 * opacity * alpha)),
                }
            };
            if self.fade.is_some() {
                background(self.old.as_ref(), self.bitmaps.old_ambient.as_ref(), 1.0);
            }
            background(self.art.as_ref(), self.bitmaps.ambient.as_ref(), if self.fade.is_some() { p } else { 1.0 });
            if shade_k > 0.0 {
                // Yazının altı biraz daha koyu (sağa doğru).
                let stops = [(0.0, 0, 0.0), (0.3, 0, 0.10 * opacity * shade_k), (1.0, 0, 0.30 * opacity * shade_k)];
                if let Some(g) = Self::linear(rt, (card.left, card.top), (card.right, card.top), &stops) {
                    rt.FillRoundedRectangle(&card_r, &g);
                }
            }
            // Dock'unki gibi ince kenarlık.
            brush.SetColor(&color(th.stroke.0, th.stroke.1 + 0.03));
            let edge = rect(card.left + 0.5, card.top + 0.5, card.right - 0.5, card.bottom - 0.5);
            rt.DrawRoundedRectangle(&rounded(edge, dock.radius - 0.5), brush, 1.0, None);

            // Kapak (yoksa bir nota); geçişte eskisinin üstüne yenisi belirir.
            let ar = l.art;
            let cover = |bmp: Option<&ID2D1Bitmap>, alpha: f32| match bmp.filter(|_| active).and_then(|b| Self::brush_for(rt, b, &ar, alpha)) {
                Some(b) => {
                    rt.FillRoundedRectangle(&rounded(ar, ART_RADIUS), &b);
                    brush.SetColor(&color(0xffffff, 0.10 * alpha));
                    let r = rect(ar.left + 0.5, ar.top + 0.5, ar.right - 0.5, ar.bottom - 0.5);
                    rt.DrawRoundedRectangle(&rounded(r, ART_RADIUS - 0.5), brush, 1.0, None);
                }
                None => {
                    fill(rounded(ar, ART_RADIUS), color(th.base, alpha));
                    fill(rounded(ar, ART_RADIUS), color(fg, 0.08 * alpha));
                    brush.SetColor(&color(fg, 0.6 * alpha));
                    let s: Vec<u16> = "\u{EC4F}".encode_utf16().collect();
                    rt.DrawText(&s, &f.glyph, &ar, brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                }
            };
            if self.fade.is_some() {
                cover(self.bitmaps.old_art.as_ref(), 1.0);
            }
            cover(self.bitmaps.art.as_ref(), if self.fade.is_some() { p } else { 1.0 });

            if active {
                text(&self.now.title, &f.title, l.title, 1.0);
                text(&self.now.artist, &f.artist, l.artist, 0.65);

                for (n, &(h, cx, cy, rad)) in l.buttons.iter().enumerate() {
                    let enabled = match h {
                        Hit::Prev => self.now.can_prev,
                        Hit::Next => self.now.can_next,
                        _ => true,
                    };
                    let hovered = self.hover == h && enabled;
                    let pressed = hovered && self.pressed == h;
                    // Basılıyken bir tık küçülür.
                    let r = if pressed { rad - 1.0 } else { rad };
                    let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r };
                    let ink = if h == Hit::Play {
                        // Çal: dolu daire, içinde ters renk simge.
                        brush.SetColor(&color(fg, if hovered { 1.0 } else { 0.92 }));
                        rt.FillEllipse(&e, brush);
                        color(if light { 0xffffff } else { 0x111111 }, 1.0)
                    } else {
                        if hovered {
                            brush.SetColor(&color(fg, if pressed { 0.18 } else { 0.10 }));
                            rt.FillEllipse(&e, brush);
                        }
                        color(fg, if !enabled { 0.3 } else if hovered { 1.0 } else { 0.85 })
                    };
                    if let Some(g) = &icons[n] {
                        brush.SetColor(&ink);
                        rt.FillGeometry(g, brush, None);
                        rt.DrawGeometry(g, brush, 1.6, &t.round);
                    }
                }

                if let Some(b) = l.bar
                    && self.now.duration > 0.0
                {
                    let cy = (b.top + b.bottom) / 2.0;
                    let thick = if self.hover == Hit::Bar { 2.0 } else { 1.25 };
                    fill(rounded(rect(b.left, cy - thick, b.right, cy + thick), thick), color(fg, 0.18));
                    let x = b.left + (b.right - b.left) * (pos / self.now.duration).clamp(0.0, 1.0) as f32;
                    let done = rect(b.left, cy - thick, x.max(b.left + 2.0 * thick), cy + thick);
                    fill(rounded(done, thick), color(fg, 0.85));
                    if self.hover == Hit::Bar {
                        let e = D2D1_ELLIPSE { point: pt(x, cy), radiusX: 4.5, radiusY: 4.5 };
                        brush.SetColor(&color(fg, 1.0));
                        rt.FillEllipse(&e, brush);
                    }
                }
            } else {
                let sub = t!("Not playing · click to open", "Çalmıyor · açmak için tıkla");
                text("Spotify", &f.title, l.title, 1.0);
                text(sub, &f.artist, l.artist, 0.6);
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
        let origin = self.origin();
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
        // Şarkı değişti: yeni kapak gelene kadar eskisi kalır; gelmezse bir süre sonra notaya geçilir.
        let track = (now.title.clone(), now.artist.clone());
        if track != self.track {
            self.track = track;
            self.track_art = false;
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_WAIT, WAIT_MS, None);
            }
        }
        if now.art_gen != self.art_gen {
            self.art_gen = now.art_gen;
            // Tarayıcının simgesi ya da çözülemeyen resim kapak sayılmaz: gösterilen kalır.
            if let Some(a) = now.art.as_ref().and_then(|b| self.decode(b)) {
                self.track_art = true;
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_WAIT);
                }
                if self.art.as_ref().is_none_or(|old| old.px != a.px) {
                    self.crossfade(Some(a));
                }
            }
        }
        if now.spotify {
            self.o.app = now.app.clone();
        }
        self.now = now;
        self.sync_shown();
        self.sync_timer();
        self.paint();
    }

    /// Gösterilen kapaktan `new`'e (yoksa notaya) yumuşak geçiş.
    fn crossfade(&mut self, new: Option<Art>) {
        if self.art.is_none() && new.is_none() {
            return;
        }
        self.old = self.art.take();
        self.bitmaps.old_art = self.bitmaps.art.take();
        self.bitmaps.old_ambient = self.bitmaps.ambient.take();
        self.art = new;
        self.fade = Some(Instant::now());
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_FADE, 16, None);
        }
    }

    /// Geçişin ilerleyişi (0..1, yumuşatılmış); geçiş yoksa 1.
    fn fade_t(&self) -> f32 {
        let t = self.fade.map_or(1.0, |f| (f.elapsed().as_secs_f32() * 1000.0 / FADE_MS).min(1.0));
        t * t * (3.0 - 2.0 * t)
    }

    fn fade_tick(&mut self) {
        if self.fade_t() >= 1.0 {
            self.fade = None;
            self.old = None;
            self.bitmaps.old_art = None;
            self.bitmaps.old_ambient = None;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_FADE);
            }
        }
        self.paint();
    }

    /// Yeni şarkının kapağı gelmedi: notaya geçilir.
    fn wait_over(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_WAIT);
        }
        if !self.track_art {
            self.crossfade(None);
        }
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
            // Uygulama simgesi (tarayıcı şarkı değişirken bir an kendi simgesini verir) küçüktür.
            if w < 100 || h < 100 {
                return None;
            }
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
            // ... ya da saydam köşelidir; kapaklar opaktır.
            let clear = px.chunks_exact(4).filter(|p| p[3] < 250).count();
            if clear * 100 > px.len() / 4 {
                return None;
            }
            let color = dominant(&px);
            let ambient = look::ambient(&px, tw as usize, th as usize, STRIP_W / 66.0);
            Some(Art { w: tw, h: th, px, color, ambient })
        }
    }

    /// Görünür mü: dock açık ve yanına sığıyorsa; tam ekranda ve (istenirse) hiçbir şey
    /// çalmıyorken gizli.
    fn sync_shown(&self) {
        let show = (self.now.active || !self.o.hide_idle) && !self.ducked && self.fits();
        unsafe {
            if show != IsWindowVisible(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, if show { SW_SHOWNOACTIVATE } else { SW_HIDE });
                if show {
                    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
                    let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
                }
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
        self.paint();
    }

    fn mouse_up(&mut self, lp: LPARAM) {
        unsafe {
            let _ = ReleaseCapture();
        }
        let pressed = std::mem::replace(&mut self.pressed, Hit::None);
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

    /// Ön plan değişti: tam ekran oyunun, videonun üstünde durmaz (dock da saklanır).
    fn foreground(&mut self, fg: HWND) {
        let duck = fullscreen(fg) && fg != self.hwnd;
        if duck != self.ducked {
            self.ducked = duck;
            self.sync_shown();
            if !duck {
                self.paint();
            }
        }
    }

    fn apply(&mut self, o: Options) {
        let app = if o.app.is_empty() { self.o.app.clone() } else { o.app.clone() };
        self.o = Options { app, ..o };
        self.sync_shown();
        self.sync_timer();
        self.paint();
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
            WM_TIMER if wp.0 == TIMER_FADE => w.fade_tick(),
            WM_TIMER if wp.0 == TIMER_WAIT => w.wait_over(),
            WM_MOUSEMOVE => w.mouse_move(lp),
            WM_LBUTTONDOWN => w.mouse_down(lp),
            WM_LBUTTONUP => w.mouse_up(lp),
            WM_MOUSELEAVE => w.mouse_leave(),
            WM_SETCURSOR => {
                let hand = matches!(w.hover, Hit::Prev | Hit::Play | Hit::Next | Hit::Bar | Hit::Card);
                unsafe {
                    let _ = SetCursor(LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }).ok());
                }
                return Some(LRESULT(1));
            }
            _ => return None,
        }
        Some(LRESULT(0))
    })
    .flatten();
    handled.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
}

// --- Dışarıya ---

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
        let ui = w!("Segoe UI Variable Text");
        Ok(Fonts {
            title: fmt(ui, 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING)?,
            artist: fmt(ui, 12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING)?,
            glyph: fmt(w!("Segoe Fluent Icons"), 18.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER)?,
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
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
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
        let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        Ok(Widget {
            hwnd,
            remote,
            o,
            d2d,
            wic,
            f: fonts()?,
            target: None,
            surface: None,
            bitmaps: Bitmaps::default(),
            dock: crate::dock::geometry(),
            now: Now::default(),
            art: None,
            old: None,
            fade: None,
            art_gen: 0,
            track: Default::default(),
            track_art: false,
            ticking: false,
            drawn_sec: -1,
            hover: Hit::None,
            pressed: Hit::None,
            tracking: false,
            ducked: false,
            hook: HWINEVENTHOOK::default(),
        })
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
        SetTimer(Some(w.hwnd), TIMER_DOCK, 2000, None);
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
            let _ = KillTimer(Some(w.hwnd), TIMER_FADE);
            let _ = KillTimer(Some(w.hwnd), TIMER_WAIT);
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

/// Pencere göstermeden widget'ı sahte bir dock'un yanında, örnek bir zeminin üstünde çizip
/// PNG'ye yazar (test komutu).
pub fn preview(path: &std::path::Path, style: Style, hover: bool, real: Option<Now>) -> windows::core::Result<()> {
    let o = Options { hide_idle: false, app: String::new(), style, opacity: 0.92, progress: true };
    let mut w = create(Remote::dummy(), o)?;
    w.dock = Some(Geometry {
        monitor: RECT { left: 0, top: 0, right: 2880, bottom: 1800 },
        dpi: 144.0,
        pill_h: 66.0,
        margin: 8.0,
        radius: 12.0,
        left: 900,
        theme: crate::dock::theme::current(),
    });
    let l = w.layout();
    let (wp, hp) = w.size_px(&l);
    let bg = |x: f32, y: f32| -> [f32; 3] {
        let (u, v) = (x / wp as f32, y / hp as f32);
        [0.25 + 0.35 * u, 0.3 + 0.1 * v, 0.5 + 0.2 * v]
    };
    let sample: &[u8] = include_bytes!("../../../assets/araclar/wallpaper-128.png");
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
    if hover {
        w.hover = Hit::Next;
    }
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
