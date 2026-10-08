//! hatter'ın dock'u: ekranın altında ortada yüzen, piksel başına saydam (katmanlı) pencere.
//! Çizim Direct2D ile bellekteki bir bitmap'e yapılır, UpdateLayeredWindow ile tek seferde
//! ekrana verilir; saydam pikseller tıklamayı alttaki pencereye geçirir.
//!
//! Boştayken hiçbir şey çalışmaz: pencere listesi WinEvent kancaları ve kabuk kancasıyla
//! değişince yenilenir, zamanlayıcı yalnızca animasyon sürerken (kayma, büyütme, açılış
//! zıplaması) döner.
//!
//! Gizlenme: öndeki pencere dock'un üstüne gelirse dock aşağı kayar, ekranın en altına
//! dokununca çıkar (2 piksellik neredeyse görünmez şerit fareyi yakalar). Tam ekran bir
//! uygulama (oyun, video) öndeyken pencere tamamen saklanır: oyunun üstünde hiçbir şey kalmaz.
//! Otomatik gizleme kapalıysa dock ekranın altında yer ayırır (appbar), büyütülen pencereler
//! onun üstünde biter.
//!
//! Durum iş parçacığına bağlı (`DOCK`); pencere işlevi onu `try_borrow_mut` ile alır. Başka
//! pencereye dokunan işler (aç, öne getir, menü) ödünç bırakıldıktan sonra yapılır: o sırada
//! gelen iç içe mesajlar durumu bulamaz, yeniden denenir.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use super::apps::{self, IconSource, Pin};
use super::stack::{self, WM_STACK_DONE, WM_STACK_ENTER, WM_STACK_LEAVE};
use super::status::{self, Status, WM_STATUS};
use super::theme::{self, Theme};
use super::{Settings, taskbar};

const CLASS: PCWSTR = w!("hive-hatter");
/// Pencere listesi değişti (kancalardan, birleştirilerek).
const WM_REFRESH: u32 = WM_APP + 1;
/// Öndeki pencere yer değiştirdi: dock'un üstüne geldi mi.
const WM_COVER: u32 = WM_APP + 2;
const WM_APPBAR: u32 = WM_APP + 3;
/// Dock listesi dışarıdan (sağ tık menüsünden) değişti: yeniden oku.
const WM_RELOAD: u32 = WM_APP + 15;
/// Windows'un paneli açıldı (wParam: pencere, lParam: `QUICK` ya da `NOTICES`).
const WM_FLYOUT: u32 = WM_APP + 8;
const QUICK: isize = 1;
const NOTICES: isize = 2;
const WM_MOUSELEAVE: u32 = 0x02A3;
/// Arka planda yüklenen ikon hazır (lParam: Box<IconDone>).
const WM_ICON: u32 = WM_APP + 6;

struct IconDone {
    id: String,
    generation: u32,
    /// (genişlik, yükseklik, önçarpımlı BGRA)
    pixels: Option<(u32, u32, Vec<u8>)>,
}

const TIMER_ANIM: usize = 1;
const TIMER_HIDE: usize = 2;
const TIMER_RETRY: usize = 3;
/// Açılıştan sonra görev çubuğunun gerçekten saklı kaldığına bakılır.
const TIMER_TRAY: usize = 4;
/// Çok pencereli simgenin üstünde durunca yığın açılır; fare dock'tan ve yığından çıkınca kapanır.
const TIMER_STACK: usize = 5;
const TIMER_STACK_CLOSE: usize = 6;
const STACK_DWELL_MS: u32 = 500;
/// Saat dakika başında güncellenir.
const TIMER_CLOCK: usize = 7;
/// Okunamayan ikonlar bir süre sonra yeniden denenir.
const TIMER_ICON_RETRY: usize = 8;
const ICON_TRIES: u8 = 4;

// Ölçüler (DIP). Simge boyu ayardan gelir.
const GAP: f32 = 6.0;
/// Pil simgesinin genişliği (yüzde yazısı yanında).
const BATTERY_GLYPH: f32 = 20.0;
const PAD: f32 = 10.0;
const PAD_T: f32 = 7.0;
const PAD_B: f32 = 11.0;
const MARGIN: f32 = 8.0;
const RADIUS: f32 = 12.0;
const SEP_W: f32 = 17.0;
const LABEL_H: f32 = 26.0;
const LABEL_GAP: f32 = 10.0;
/// En büyük büyütme ve etkisinin uzandığı mesafe (simge genişliği cinsinden, her iki yana).
const MAG: f32 = 1.55;
const REACH: f32 = 2.4;

const LAUNCH_MAX: Duration = Duration::from_secs(8);
const BOUNCE: f32 = 1.65;
/// Dikkat isteyen uygulamanın zıplaması: iki sıçrayış.
const ATTENTION: f32 = 1.1;


thread_local! {
    static DOCK: RefCell<Option<Dock>> = const { RefCell::new(None) };
    /// Listedeki pencereler: kanca hangi yok olan pencerenin bizi ilgilendirdiğini buradan bilir.
    static KNOWN: RefCell<HashSet<isize>> = RefCell::new(HashSet::new());
    static HWND_DOCK: Cell<isize> = const { Cell::new(0) };
    static REFRESH_POSTED: Cell<bool> = const { Cell::new(false) };
    static COVER_POSTED: Cell<bool> = const { Cell::new(false) };
    /// Durum simgesine az önce tıklandı: açılan Windows paneli dock'un üstüne taşınacak.
    static FLYOUT: Cell<Option<(Instant, isize)>> = const { Cell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Dock) -> R) -> Option<R> {
    DOCK.with(|d| d.try_borrow_mut().ok().and_then(|mut d| d.as_mut().map(f)))
}

fn post_once(flag: &'static std::thread::LocalKey<Cell<bool>>, msg: u32) {
    let h = HWND_DOCK.get();
    if h == 0 || flag.get() {
        return;
    }
    flag.set(true);
    unsafe {
        let _ = PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(0), LPARAM(0));
    }
}

unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    if object != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 || hwnd.is_invalid() {
        return;
    }
    let known = || KNOWN.with_borrow(|k| k.contains(&(hwnd.0 as isize)));
    let top = || unsafe { GetAncestor(hwnd, GA_PARENT) == GetDesktopWindow() };
    match event {
        EVENT_OBJECT_LOCATIONCHANGE => {
            if unsafe { GetForegroundWindow() } == hwnd {
                post_once(&COVER_POSTED, WM_COVER);
            }
        }
        EVENT_SYSTEM_FOREGROUND | EVENT_SYSTEM_MINIMIZESTART | EVENT_SYSTEM_MINIMIZEEND => {
            if event == EVENT_SYSTEM_FOREGROUND
                && let Some((t, kind)) = FLYOUT.get()
                && t.elapsed() < Duration::from_secs(2)
            {
                let class = apps::class_name(hwnd);
                let ours = match kind {
                    QUICK => class == "ControlCenterWindow",
                    _ => class == "Windows.UI.Core.CoreWindow",
                };
                if ours {
                    FLYOUT.set(None);
                    let h = HWND_DOCK.get();
                    unsafe {
                        let _ = PostMessageW(Some(HWND(h as *mut _)), WM_FLYOUT, WPARAM(hwnd.0 as usize), LPARAM(kind));
                    }
                }
            }
            post_once(&REFRESH_POSTED, WM_REFRESH)
        }
        EVENT_OBJECT_SHOW => {
            if top() {
                if taskbar::is_tray(hwnd) {
                    taskbar::rehide(hwnd);
                }
                post_once(&REFRESH_POSTED, WM_REFRESH);
            }
        }
        EVENT_OBJECT_UNCLOAKED => {
            if top() {
                post_once(&REFRESH_POSTED, WM_REFRESH)
            }
        }
        EVENT_OBJECT_HIDE | EVENT_OBJECT_DESTROY | EVENT_OBJECT_CLOAKED | EVENT_OBJECT_NAMECHANGE => {
            if known() {
                post_once(&REFRESH_POSTED, WM_REFRESH)
            }
        }
        _ => {}
    }
}

// --- Öğeler ---

struct Item {
    pin: Option<Pin>,
    /// Sabitlenmemişse grubun anahtarı; sabitliyse kısayolun yolu.
    id: String,
    name: String,
    icon: IconSource,
    /// Sabitlenince dock.txt'ye yazılacak satır.
    line: Option<String>,
    windows: Vec<HWND>,
}

impl Item {
    fn folder(&self) -> bool {
        self.pin.as_ref().is_some_and(|p| p.folder)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Cover {
    Clear,
    /// Öndeki pencere dock'un üstünde.
    Window,
    /// Tam ekran uygulama.
    Full,
}

/// Sürükleyerek sıralama: basılan simge, imlecin x'i, hareket eşiği geçildi mi, bırakılırsa
/// gideceği sıra (bütün öğeler içinde; kendi bölümünden çıkmaz).
#[derive(Clone, Copy)]
struct Drag {
    item: usize,
    start_x: f32,
    x: f32,
    active: bool,
    target: usize,
}

/// Ödünç bırakıldıktan sonra yapılacak iş.
enum Action {
    Open(String),
    Activate(HWND),
    Minimize(HWND),
    Menu(MenuSpec),
    /// Windows'un hızlı ayarları (Win+A): Wi-Fi, Bluetooth, ses, parlaklık, medya.
    QuickSettings,
    /// hive'ı hatter sayfasında aç.
    Settings,
    /// Windows bildirimleri ve takvim (Win+N).
    Notices,
    /// Gizli simgeler: alt orta noktası (fiziksel piksel).
    Tray(i32, i32),
}

struct MenuSpec {
    id: String,
    name: String,
    icon: IconSource,
    windows: Vec<(HWND, String)>,
    pinned: bool,
    line: Option<String>,
    x: i32,
    y: i32,
}

/// Sağ uçtaki durum parçaları.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Part {
    /// Windows'un gizli simgeler penceresi.
    Tray,
    Net,
    Volume,
    Battery,
    Clock,
}

impl Part {
    /// Ağ, ses ve pil birlikte hızlı ayarları açar, birlikte vurgulanır.
    fn quick(self) -> bool {
        matches!(self, Part::Net | Part::Volume | Part::Battery)
    }
}

/// Yuvalar: simge, ayraç ya da durum parçası; x'ler pencerenin solundan.
#[derive(Clone, Copy)]
struct Slot {
    item: Option<usize>,
    part: Option<Part>,
    l: f32,
    r: f32,
    size: f32,
}

struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
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
            Some(Self { dc, bmp, old, w, h })
        }
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

fn color(c: u32, a: f32) -> D2D1_COLOR_F {
    let ch = |s: u32| ((c >> s) & 0xFF) as f32 / 255.0;
    D2D1_COLOR_F { r: ch(16), g: ch(8), b: ch(0), a }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

fn rounded(l: f32, t: f32, r: f32, b: f32, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: rect(l, t, r, b), radiusX: radius, radiusY: radius }
}

fn approach(v: f32, target: f32, dt: f32, tau: f32) -> f32 {
    let n = v + (target - v) * (1.0 - (-dt / tau).exp());
    if (n - target).abs() < 0.003 { target } else { n }
}

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

struct Dock {
    hwnd: HWND,
    /// Gerçek dock (önizleme değil): görev çubuğuna, kancalara, appbar'a dokunur.
    live: bool,
    settings: Settings,
    /// dock.txt satırları (çözülemeyenler de: kısayolu geri gelirse yine görünür).
    lines: Vec<String>,
    pins: Vec<Pin>,
    items: Vec<Item>,
    /// Sabitlenmemiş uygulamaların görünme sırası.
    order: Vec<String>,
    running: HashMap<String, apps::Running>,
    flashing: HashSet<isize>,
    launched: HashMap<String, Instant>,
    /// Dikkat isteyen (yanıp sönen) uygulamalar: zıplamanın başladığı an.
    attention: HashMap<String, Instant>,
    /// Bildirim gönderen, henüz bakılmamış uygulamalar (öğe kimliği): turuncu gösterge.
    notified: HashSet<String>,
    /// Pencerelerin bilinen başlıkları: "yeniden çizildi" bildirimi başlık değişmeden gelirse
    /// pencere dikkat istiyordur.
    titles: HashMap<isize, (String, Instant)>,
    foreground: HWND,

    d2d: ID2D1Factory,
    wic: IWICImagingFactory,
    dw: IDWriteFactory,
    label: IDWriteTextFormat,
    target: Option<Target>,
    surface: Option<Surface>,
    icons: HashMap<String, Option<ID2D1Bitmap>>,
    /// Arka planda yüklenen ikonlar ve yükleme kuşağı (boyut değişince eski sonuçlar atılır).
    loading: HashSet<String>,
    /// Okunamayan ikonlar: kaç kez denendi (açılışta kabuk meşgulken boş dönebiliyor).
    failed: HashMap<String, u8>,
    icon_gen: u32,
    dpi: f32,
    monitor: RECT,
    /// Pencerenin ekrandaki yeri (piksel).
    place: RECT,

    mouse: Option<(f32, f32)>,
    tracking: bool,
    hover: Option<usize>,
    pressed: Option<usize>,
    menu_open: bool,
    /// Fare yığının üstünde.
    in_stack: bool,
    /// Dock'un üstüne hizalanan Windows paneli (açıkken dock saklanmaz).
    flyout: Option<(HWND, isize)>,
    drag: Option<Drag>,
    /// Açık sağ tık menüsünün öğesi (seçim gelince işlenir).
    menu: Option<MenuSpec>,
    status: Status,
    theme: Theme,
    hover_part: Option<Part>,
    glyph: IDWriteTextFormat,
    clock: IDWriteTextFormat,
    clock_w: f32,
    /// Pil yüzdesinin genişliği (simgenin yanında yazılır).
    battery_w: f32,

    shown: f32,
    mag: f32,
    /// Bir sonraki karede çizilecek.
    dirty: bool,
    /// Büyütmenin merkezi: imlecin simgelerin üstündeki son konumu (çıkınca büyütme oradan söner).
    mag_x: Option<f32>,
    /// İmleç uygulama/klasör simgelerinin üstünde.
    over_items: bool,
    tick: Instant,
    animating: bool,
    cover: Cover,
    /// Gizliyken alttan çağrıldı: fare ayrılana kadar görünür.
    revealed: bool,
    hidden_window: bool,
    appbar: bool,

    hooks: Vec<HWINEVENTHOOK>,
    shellhook: u32,
    taskbar_created: u32,
}

impl Dock {
    fn icon_size(&self) -> f32 {
        self.settings.size as f32
    }

    fn max_mag(&self) -> f32 {
        if self.settings.magnify { MAG } else { 1.0 }
    }

    fn pill_h(&self) -> f32 {
        PAD_T + self.icon_size() + PAD_B
    }

    fn k(&self) -> f32 {
        self.dpi / 96.0
    }

    /// Pencere boyu (DIP): en büyük simge ve üstünde adı sığar.
    fn size_dip(&self) -> (f32, f32) {
        let icon = self.icon_size();
        let base = self.base_width();
        let extra = if self.settings.magnify { icon * (MAG - 1.0) * REACH } else { 0.0 };
        let w = base + 2.0 * extra + 24.0;
        let h = MARGIN + PAD_B + icon * self.max_mag() + LABEL_GAP + LABEL_H + 6.0;
        (w.max(160.0), h)
    }

    /// Sağ uçtaki parçalar ve genişlikleri.
    fn parts(&self) -> Vec<(Part, f32)> {
        let mut v = vec![(Part::Tray, 26.0), (Part::Net, 30.0), (Part::Volume, 30.0)];
        if self.status.battery.is_some() {
            v.push((Part::Battery, BATTERY_GLYPH + self.battery_w + 10.0));
        }
        v.push((Part::Clock, self.clock_w + 16.0));
        v
    }

    /// Büyütülmemiş hapın genişliği.
    fn base_width(&self) -> f32 {
        let icon = self.icon_size();
        let n = self.items.len() as f32;
        let parts: f32 = self.parts().iter().map(|p| p.1).sum();
        let sep2 = if self.items.is_empty() { 0.0 } else { SEP_W };
        n * (icon + GAP) + sep2 + parts + 2.0 * PAD
    }

    fn text_w(&self, text: &str) -> f32 {
        let t: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            self.dw.CreateTextLayout(&t, &self.clock, 200.0, 40.0).ok().map_or(40.0, |l| {
                let mut m = DWRITE_TEXT_METRICS::default();
                let _ = l.GetMetrics(&mut m);
                m.width.ceil()
            })
        }
    }

    fn measure_clock(&mut self) {
        self.clock_w = self.text_w(&self.status.time);
        self.battery_w = self.text_w(&self.status.battery_pct());
    }

    fn band_dip(&self) -> f32 {
        MARGIN * 2.0 + self.pill_h()
    }

    // --- Yerleşim ---

    fn slots(&self) -> Vec<Slot> {
        let icon = self.icon_size();
        let (w, _) = self.size_dip();
        let mut base: Vec<(Option<usize>, Option<Part>, f32)> = Vec::new();
        for i in self.display_order() {
            base.push((Some(i), None, icon + GAP));
        }
        if !self.items.is_empty() {
            base.push((None, None, SEP_W));
        }
        base.extend(self.parts().into_iter().map(|(p, w)| (None, Some(p), w)));
        let total: f32 = base.iter().map(|s| s.2).sum();
        let start = (w - total) / 2.0;
        // Taban (büyütülmemiş) konumlar ve büyütülmüş genişlikler.
        let mut bl = Vec::with_capacity(base.len());
        let mut x = start;
        for s in &base {
            bl.push(x);
            x += s.2;
        }
        // Büyütme yalnızca imleç simgelerin üstündeyken: durum alanının (tepsi, saat) üstünde
        // yanındaki uygulama büyümesin.
        let first = base.iter().position(|b| b.0.is_some());
        let last = base.iter().rposition(|b| b.0.is_some());
        let mx = match (self.mag_x, first, last) {
            (Some(x), Some(f), Some(l)) if self.mag > 0.0 => Some(x.clamp(bl[f], bl[l] + base[l].2 - 0.01)),
            _ => None,
        };
        let scale = |i: usize| -> f32 {
            let (Some(mx), true) = (mx, self.mag > 0.0) else { return 1.0 };
            let c = bl[i] + base[i].2 / 2.0;
            let t = (mx - c).abs() / (icon * REACH);
            if t >= 1.0 {
                return 1.0;
            }
            let f = (t * std::f32::consts::FRAC_PI_2).cos().powi(2);
            1.0 + (MAG - 1.0) * self.mag * f
        };
        let widths: Vec<(f32, f32)> = base
            .iter()
            .enumerate()
            .map(|(i, s)| match s.0 {
                Some(_) => {
                    let sc = scale(i);
                    (icon * sc + GAP, icon * sc)
                }
                None => (s.2, 0.0),
            })
            .collect();
        // İmlecin altındaki nokta yerinde kalsın: dock iki yana ona göre açılır.
        let mut ml = Vec::with_capacity(base.len());
        let mut x = 0.0;
        for wd in &widths {
            ml.push(x);
            x += wd.0;
        }
        let shift = match mx.filter(|_| self.mag > 0.0) {
            Some(mx) if !base.is_empty() => {
                let last = base.len() - 1;
                if mx <= bl[0] {
                    bl[0] - ml[0]
                } else if mx >= bl[last] + base[last].2 {
                    bl[last] + base[last].2 - (ml[last] + widths[last].0)
                } else {
                    let j = (0..base.len()).rev().find(|&j| bl[j] <= mx).unwrap_or(0);
                    let frac = (mx - bl[j]) / base[j].2;
                    mx - (ml[j] + frac * widths[j].0)
                }
            }
            _ => start,
        };
        base.iter()
            .zip(&widths)
            .zip(&ml)
            .map(|((b, wd), l)| Slot { item: b.0, part: b.1, l: shift + l, r: shift + l + wd.0, size: wd.1 })
            .collect()
    }

    /// Öğelerin çizim sırası: sürüklenen simge bırakılacağı yerde (orada boşluk kalır).
    fn display_order(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.items.len()).collect();
        if let Some(d) = self.drag.filter(|d| d.active && d.item < v.len()) {
            v.remove(d.item);
            v.insert(d.target.min(v.len()), d.item);
        }
        v
    }

    /// Sürüklenen simgenin bırakılacağı sıra: imlecin büyütülmemiş yerleşimdeki yuvasına göre. Yuvalar sabit genişlikte olduğundan
    /// boşluk kayarken hedef titremez.
    fn drag_target(&self, item: usize, x: f32) -> usize {
        let wslot = self.icon_size() + GAP;
        let (w, _) = self.size_dip();
        let start = (w - (self.base_width() - 2.0 * PAD)) / 2.0;
        let _ = item;
        let n = self.items.len().max(1);
        (((x - start) / wslot).floor().max(0.0) as usize).min(n - 1)
    }

    /// Sürükleme bitti: yeni sıra dock.txt'ye yazılır. Sabitlenmemiş bir uygulama sürüklendiyse
    /// artık dock'ta kalıcıdır (Mac'teki gibi).
    fn commit_drag(&mut self, d: Drag) {
        let order = self.display_order();
        let mut pinned = Vec::new();
        for &i in &order {
            let it = &self.items[i];
            let line = match &it.pin {
                Some(p) => Some(p.target.clone()),
                None if i == d.item => it.line.clone(),
                None => None,
            };
            if let Some(l) = line {
                pinned.push(l);
            }
        }
        // Çözülemeyen satırlar (kısayolu şimdilik yok) sonda kalır, kaybolmaz.
        let known: HashSet<String> = self.pins.iter().map(|p| p.target.to_lowercase()).collect();
        let rest = self.lines.iter().filter(|l| !known.contains(&l.to_lowercase())).cloned();
        let mut seen = HashSet::new();
        self.lines = pinned.into_iter().chain(rest).filter(|l| seen.insert(l.to_lowercase())).collect();
        super::save_pins(&self.lines);
        let old = std::mem::take(&mut self.pins);
        self.pins = self
            .lines
            .iter()
            .filter_map(|l| old.iter().find(|p| p.target.eq_ignore_ascii_case(l)).cloned().or_else(|| Pin::resolve(l)))
            .collect();
        self.refresh();
    }

    /// Hapın alt kenarı (DIP, pencerenin üstünden), kayma dahil.
    fn pill_bottom(&self) -> f32 {
        let (_, h) = self.size_dip();
        h - MARGIN + (1.0 - smooth(self.shown)) * (self.pill_h() + MARGIN + 8.0)
    }

    fn pill_x(slots: &[Slot]) -> (f32, f32) {
        match (slots.first(), slots.last()) {
            (Some(a), Some(b)) => (a.l - (PAD - GAP / 2.0), b.r + (PAD - GAP / 2.0)),
            _ => (0.0, 0.0),
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<usize> {
        let slots = self.slots();
        let bottom = self.pill_bottom();
        let top = bottom - PAD_B - self.icon_size() * self.max_mag() - 4.0;
        if y < top || y > bottom {
            return None;
        }
        slots.iter().find(|s| x >= s.l && x < s.r).and_then(|s| s.item)
    }

    /// Uygulama ve klasör simgelerinin kapladığı aralık (şu anki yerleşimde).
    fn items_span(&self) -> Option<(f32, f32)> {
        let slots = self.slots();
        let f = slots.iter().find(|s| s.item.is_some())?;
        let l = slots.iter().rev().find(|s| s.item.is_some())?;
        Some((f.l, l.r))
    }

    fn hit_part(&self, x: f32, y: f32) -> Option<Part> {
        let bottom = self.pill_bottom();
        if y < bottom - self.pill_h() || y > bottom {
            return None;
        }
        self.slots().iter().find(|s| x >= s.l && x < s.r).and_then(|s| s.part)
    }

    /// Dock'un taban halinin ekrandaki dikdörtgeni (piksel): pencere buna değerse dock saklanır.
    fn screen_rect(&self) -> RECT {
        let k = self.k();
        let (w, _) = self.size_dip();
        let pw = self.base_width();
        let l = self.place.left + ((w - pw) / 2.0 * k) as i32;
        RECT {
            left: l,
            top: self.monitor.bottom - ((MARGIN + self.pill_h()) * k) as i32,
            right: l + (pw * k) as i32,
            bottom: self.monitor.bottom,
        }
    }

    fn relayout(&mut self) {
        unsafe {
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY), &mut mi);
            self.monitor = mi.rcMonitor;
            let dpi = GetDpiForWindow(self.hwnd) as f32;
            if dpi > 0.0 && dpi != self.dpi {
                self.dpi = dpi;
                self.clear_icons();
            }
        }
        let k = self.k();
        let (w, h) = self.size_dip();
        let (wp, hp) = ((w * k).ceil() as i32, (h * k).ceil() as i32);
        let m = self.monitor;
        let x = m.left + (m.right - m.left - wp) / 2;
        self.place = RECT { left: x, top: m.bottom - hp, right: x + wp, bottom: m.bottom };
        if self.appbar {
            self.appbar_pos();
        }
        self.render();
    }

    // --- Pencere listesi ---

    fn refresh(&mut self) {
        if self.live {
            taskbar::ensure_hidden();
        }
        let wins = apps::windows();
        KNOWN.with_borrow_mut(|k| {
            k.clear();
            k.extend(wins.iter().map(|w| w.hwnd.0 as isize));
        });
        self.foreground = unsafe { GetForegroundWindow() };
        let mut pinned: Vec<Item> = self
            .pins
            .iter()
            .map(|p| Item {
                pin: Some(p.clone()),
                id: p.target.to_lowercase(),
                name: p.name.clone(),
                icon: IconSource::Shell(p.icon.clone()),
                line: Some(p.target.clone()),
                windows: Vec::new(),
            })
            .collect();
        let mut extra: Vec<Item> = Vec::new();
        for w in &wins {
            if let Some(it) = pinned.iter_mut().find(|it| it.pin.as_ref().is_some_and(|p| p.owns(w))) {
                it.windows.push(w.hwnd);
                continue;
            }
            let key = w.key();
            if let Some(it) = extra.iter_mut().find(|it| it.id == key) {
                it.windows.push(w.hwnd);
                continue;
            }
            let r = self.running.entry(key.clone()).or_insert_with(|| apps::running(w));
            extra.push(Item {
                pin: None,
                id: key,
                name: r.name.clone(),
                icon: r.icon.clone(),
                line: r.pin.clone(),
                windows: vec![w.hwnd],
            });
        }
        for it in &extra {
            if !self.order.contains(&it.id) {
                self.order.push(it.id.clone());
            }
        }
        self.order.retain(|k| extra.iter().any(|it| &it.id == k));
        self.running.retain(|k, _| !k.starts_with("pencere:") || extra.iter().any(|it| &it.id == k));
        extra.sort_by_key(|it| self.order.iter().position(|k| *k == it.id));
        let old_len = self.items.len();
        self.items = pinned.into_iter().chain(extra).collect();
        // Penceresi açılan uygulama artık zıplamaz; kapanan pencerelerin parlaması unutulur.
        let live: HashSet<isize> = wins.iter().map(|w| w.hwnd.0 as isize).collect();
        self.titles.retain(|h, _| live.contains(h));
        for w in &wins {
            self.titles.entry(w.hwnd.0 as isize).or_insert_with(|| (apps::title(w.hwnd), Instant::now()));
        }
        self.flashing.retain(|h| live.contains(h));
        self.flashing.remove(&(self.foreground.0 as isize));
        let fg = self.foreground;
        let items = &self.items;
        self.notified.retain(|id| items.iter().any(|it| &it.id == id && !it.windows.contains(&fg)));
        let items = &self.items;
        self.launched
            .retain(|id, t| t.elapsed() < LAUNCH_MAX && items.iter().any(|i| &i.id == id && i.windows.is_empty()));
        if let Some(id) = stack::current() {
            match self.items.iter().find(|i| i.id == id).filter(|i| !i.windows.is_empty()) {
                Some(it) => stack::update(&it.windows),
                None => stack::close(),
            }
        }
        let live_icons: HashSet<String> = self.items.iter().map(|i| i.icon.id()).collect();
        self.icons.retain(|k, _| !k.starts_with("pencere:") || live_icons.contains(k));
        if let Some((x, y)) = self.mouse {
            self.hover = self.hit(x, y);
        }
        self.cover = self.compute_cover();
        if self.items.len() != old_len {
            self.relayout();
        } else {
            self.render();
        }
        self.kick();
    }

    fn compute_cover(&self) -> Cover {
        let fg = unsafe { GetForegroundWindow() };
        if fg.is_invalid() || fg == self.hwnd || apps::is_shell_window(fg) || unsafe { IsIconic(fg).as_bool() } {
            return Cover::Clear;
        }
        if self.flyout.is_some_and(|f| f.0 == fg) {
            return Cover::Clear;
        }
        let r = apps::frame(fg);
        let m = self.monitor;
        // Tam ekran: ekranın tamamını kaplayan pencere (oyun, F11, video). Büyütülmüş pencereler
        // de (görev çubuğu otomatik gizlide) ekranı kaplar; çerçevesi (boyutlandırma kenarı ya da
        // başlığı) olanlar sayılmaz: Chromium/Electron'unki başlıksız görünse de çerçevelidir.
        // Kenarsız büyütülmüş pencere (çoğu oyun, WinForms/WPF tam ekranı) tam ekrandır.
        let mut wr = RECT::default();
        unsafe {
            let _ = GetWindowRect(fg, &mut wr);
        }
        let covers = wr.left <= m.left && wr.top <= m.top && wr.right >= m.right && wr.bottom >= m.bottom;
        let framed = unsafe { GetWindowLongW(fg, GWL_STYLE) } as u32 & (WS_CAPTION.0 | WS_THICKFRAME.0) != 0;
        if covers && (!unsafe { IsZoomed(fg).as_bool() } || !framed) {
            return Cover::Full;
        }
        // Windows'un kendi algısı: DirectX'in özel tam ekranı, sunum modu. Explorer'a soruluyor;
        // pencere sürüklenirken her konum değişiminde sorulmasın diye öndeki pencere başına
        // bir saniye saklanır.
        thread_local!(static QUNS: std::cell::Cell<Option<(isize, Instant, bool)>> = const { std::cell::Cell::new(None) });
        let rude = match QUNS.get() {
            Some((h, t, v)) if h == fg.0 as isize && t.elapsed() < Duration::from_secs(1) => v,
            _ => {
                use windows::Win32::UI::Shell::*;
                let v = matches!(unsafe { SHQueryUserNotificationState() }, Ok(QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE));
                QUNS.set(Some((fg.0 as isize, Instant::now(), v)));
                v
            }
        };
        if rude {
            return Cover::Full;
        }
        let d = self.screen_rect();
        let overlaps = r.left < d.right && r.right > d.left && r.top < d.bottom && r.bottom > d.top;
        if overlaps { Cover::Window } else { Cover::Clear }
    }

    fn target_shown(&self) -> f32 {
        let wanted = match self.cover {
            Cover::Full => false,
            Cover::Clear => true,
            Cover::Window => {
                self.appbar || self.revealed || self.mouse.is_some() || self.menu_open || stack::current().is_some()
            }
        };
        if wanted { 1.0 } else { 0.0 }
    }

    // --- Animasyon ---

    fn kick(&mut self) {
        // Tam ekranda pencere tamamen saklı; kapak kalkınca yeniden görünür.
        let full = self.cover == Cover::Full;
        if !full && self.hidden_window {
            self.hidden_window = false;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
                let _ =
                    SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        if !self.animating && self.needs_anim() {
            self.animating = true;
            self.tick = Instant::now();
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_ANIM, 10, None);
            }
        }
    }

    fn launching(&self) -> bool {
        !self.launched.is_empty() || !self.attention.is_empty()
    }

    /// Windows bildirimi geldi: gönderen uygulama dock'taysa ve önde değilse zıplar, ona
    /// geçilene kadar turuncu gösterge yanar.
    fn toast(&mut self, aumid: &str) {
        let fg = unsafe { GetForegroundWindow() };
        let Some(item) = self.items.iter().find(|it| {
            it.id.eq_ignore_ascii_case(aumid) || it.pin.as_ref().and_then(|p| p.aumid.as_deref()).is_some_and(|a| a.eq_ignore_ascii_case(aumid))
        }) else {
            return;
        };
        if item.windows.contains(&fg) {
            return;
        }
        let id = item.id.clone();
        self.notified.insert(id.clone());
        self.attention.insert(id, Instant::now());
        if self.cover == Cover::Window && self.mouse.is_none() {
            self.revealed = true;
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_HIDE, 2600, None);
            }
        }
        self.render();
        self.kick();
    }

    /// Başlık değişmeden gelen "yeniden çizildi" dikkat isteğidir; ama başlık az önce
    /// değiştiyse (şarkı değişti, tarayıcı ardından küçük resmi ve medya düğmelerini yeniledi)
    /// sıradan bir güncellemedir.
    fn redrawn(&mut self, hwnd: isize) {
        let title = apps::title(HWND(hwnd as *mut _));
        match self.titles.get(&hwnd) {
            Some((old, changed)) if *old == title => {
                if changed.elapsed() > Duration::from_secs(3) {
                    self.attention(hwnd);
                }
            }
            _ => {
                self.titles.insert(hwnd, (title, Instant::now()));
            }
        }
    }

    /// Pencere dikkat istiyor (Windows'ta görev çubuğunun yanıp sönmesi): uygulamanın simgesi
    /// iki kez zıplar. Yanıp sönme sürerken tekrar zıplamaz; her yeni bildirimde bir kez. Dock
    /// saklıysa zıplama görünsün diye kısa süre alttan çıkar (tam ekranda değil).
    fn attention(&mut self, hwnd: isize) {
        if HWND(hwnd as *mut _) == self.foreground || !self.flashing.insert(hwnd) {
            return;
        }
        let Some(item) = self.items.iter().find(|it| it.windows.iter().any(|w| w.0 as isize == hwnd)) else { return };
        self.attention.insert(item.id.clone(), Instant::now());
        if self.cover == Cover::Window && self.mouse.is_none() {
            self.revealed = true;
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_HIDE, 2600, None);
            }
        }
        self.render();
        self.kick();
    }

    fn needs_anim(&self) -> bool {
        let mag = if self.settings.magnify && self.over_items { 1.0 } else { 0.0 };
        self.dirty || self.shown != self.target_shown() || self.mag != mag || self.launching()
    }

    fn step(&mut self) {
        let now = Instant::now();
        let dt = (now - self.tick).as_secs_f32().min(0.05);
        self.tick = now;
        let up = self.target_shown() > self.shown;
        self.shown = approach(self.shown, self.target_shown(), dt, if up { 0.055 } else { 0.07 });
        let mag = if self.settings.magnify && self.over_items { 1.0 } else { 0.0 };
        self.mag = approach(self.mag, mag, dt, 0.06);
        let items = &self.items;
        self.launched
            .retain(|id, t| t.elapsed() < LAUNCH_MAX && items.iter().any(|i| &i.id == id && i.windows.is_empty()));
        self.attention.retain(|_, t| t.elapsed().as_secs_f32() < ATTENTION);
        self.dirty = false;
        self.paint_now();
        if !self.needs_anim() {
            self.animating = false;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_ANIM);
            }
            if self.cover == Cover::Full && self.shown == 0.0 && !self.hidden_window {
                self.hidden_window = true;
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
            }
        }
    }

    // --- Çizim ---

    fn ensure_target(&mut self) -> bool {
        if self.target.is_some() {
            return true;
        }
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: if std::env::var_os("HATTER_GPU").is_some() { D2D1_RENDER_TARGET_TYPE_DEFAULT } else { D2D1_RENDER_TARGET_TYPE_SOFTWARE },
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
                self.clear_icons();
                true
            }
            Err(e) => {
                crate::log!("hatter: çizim hedefi kurulamadı: {e}");
                false
            }
        }
    }

    fn clear_icons(&mut self) {
        self.icons.clear();
        self.loading.clear();
        self.failed.clear();
        self.icon_gen = self.icon_gen.wrapping_add(1);
    }

    /// İkonun D2D bitmap'i. Yoksa arka planda yüklenir (kabuktan ikon çıkarmak onlarca
    /// milisaniye sürebilir, arayüz takılmasın); hazır olunca WM_ICON gelir, o zamana kadar
    /// yer tutucu çizilir. Önizlemede hemen yüklenir.
    fn bitmap(&mut self, src: &IconSource) -> Option<ID2D1Bitmap> {
        let id = src.id();
        if let Some(b) = self.icons.get(&id) {
            return b.clone();
        }
        let px = (self.icon_size() * self.max_mag() * self.k()).ceil() as i32;
        if !self.live {
            let rt = &self.target.as_ref()?.rt;
            let b = apps::icon(&self.wic, src, px.max(32)).and_then(|w| unsafe { rt.CreateBitmapFromWicBitmap(&w, None) }.ok());
            self.icons.insert(id, b.clone());
            return b;
        }
        if self.loading.insert(id.clone()) {
            let (hwnd, generation, src) = (self.hwnd.0 as usize, self.icon_gen, src.clone());
            std::thread::spawn(move || {
                use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                }
                let pixels = unsafe { CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                    .ok()
                    .and_then(|wic| apps::icon(&wic, &src, px.max(32)))
                    .and_then(|b| unsafe {
                        let (mut w, mut h) = (0u32, 0u32);
                        b.GetSize(&mut w, &mut h).ok()?;
                        let mut buf = vec![0u8; (w * h * 4) as usize];
                        b.CopyPixels(std::ptr::null(), w * 4, &mut buf).ok()?;
                        Some((w, h, buf))
                    });
                let done = Box::into_raw(Box::new(IconDone { id, generation, pixels }));
                unsafe {
                    if PostMessageW(Some(HWND(hwnd as *mut _)), WM_ICON, WPARAM(0), LPARAM(done as isize)).is_err() {
                        drop(Box::from_raw(done));
                    }
                }
            });
        }
        None
    }

    fn icon_loaded(&mut self, d: IconDone) {
        if d.generation != self.icon_gen {
            return;
        }
        self.loading.remove(&d.id);
        if d.pixels.is_none() {
            let n = self.failed.entry(d.id.clone()).or_insert(0);
            *n += 1;
            if *n < ICON_TRIES {
                crate::log!("hatter: ikon okunamadı ({}), yeniden denenecek", d.id);
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_ICON_RETRY, 1500 * *n as u32, None);
                }
                return;
            }
        }
        let Some(t) = &self.target else { return };
        let b = d.pixels.and_then(|(w, h, buf)| unsafe {
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            t.rt.CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(buf.as_ptr().cast()), w * 4, &props).ok()
        });
        self.icons.insert(d.id, b);
        self.render();
    }

    /// Çizim isteği: hemen değil, bir sonraki karede (en fazla ekran yenilemesi hızında).
    /// Fare her kıpırdadığında çizmek arayüzü meşgul eder.
    fn render(&mut self) {
        self.dirty = true;
        self.kick();
    }

    fn paint_now(&mut self) {
        if !self.ensure_target() {
            return;
        }
        let k = self.k();
        let (wd, hd) = self.size_dip();
        let (wp, hp) = ((wd * k).ceil() as i32, (hd * k).ceil() as i32);
        if self.surface.as_ref().is_none_or(|s| s.w != wp || s.h != hp) {
            self.surface = Surface::new(wp, hp);
        }
        let Some(dc) = self.surface.as_ref().map(|s| s.dc) else { return };
        let slots = self.slots();
        let icons: Vec<Option<ID2D1Bitmap>> = self.items.iter().map(|i| i.icon.clone()).collect::<Vec<_>>().iter().map(|s| self.bitmap(s)).collect();
        let Some(t) = &self.target else { return };
        let (rt, brush) = (&t.rt, &t.brush);
        let fill = |r: D2D1_ROUNDED_RECT, c: D2D1_COLOR_F| unsafe {
            brush.SetColor(&c);
            rt.FillRoundedRectangle(&r, brush);
        };
        let ok = unsafe {
            if rt.BindDC(dc, &RECT { left: 0, top: 0, right: wp, bottom: hp }).is_err() {
                return;
            }
            rt.SetDpi(self.dpi, self.dpi);
            rt.BeginDraw();
            rt.Clear(Some(&color(0, 0.0)));

            let icon = self.icon_size();
            let bottom = self.pill_bottom();
            let top = bottom - self.pill_h();
            let (pl, pr) = Self::pill_x(&slots);
            if self.shown > 0.0 && !slots.is_empty() {
                // Yumuşak gölge: genişleyen, gittikçe silikleşen katmanlar.
                // Windows 11 panelleri gibi: tema rengi zemin, ince kenarlık, hafif gölge.
                let th = &self.theme;
                for i in 1..=3 {
                    let s = i as f32 * 2.0;
                    fill(rounded(pl - s, top - s + 4.0, pr + s, bottom + s + 4.0, RADIUS + s), color(0, 0.06));
                }
                fill(rounded(pl, top, pr, bottom, RADIUS), color(th.base, 0.88));
                brush.SetColor(&color(th.stroke.0, th.stroke.1 + 0.03));
                rt.DrawRoundedRectangle(&rounded(pl + 0.5, top + 0.5, pr - 0.5, bottom - 0.5, RADIUS - 0.5), brush, 1.0, None);

                // Üstüne gelinen durum parçası (ağ/ses/pil birlikte) hafifçe aydınlanır.
                if let Some(hp_) = self.hover_part {
                    let group: Vec<&Slot> =
                        slots.iter().filter(|s| s.part.is_some_and(|p| p == hp_ || (p.quick() && hp_.quick()))).collect();
                    if let (Some(a), Some(b)) = (group.first(), group.last()) {
                        let cy = bottom - PAD_B - icon / 2.0 + 2.0;
                        let th = &self.theme;
                        fill(rounded(a.l + 2.0, cy - 17.0, b.r - 2.0, cy + 17.0, 4.0), color(th.fill_hover.0, th.fill_hover.1));
                    }
                }
                for s in &slots {
                    let cx = (s.l + s.r) / 2.0;
                    if let Some(part) = s.part {
                        let cy = bottom - PAD_B - icon / 2.0 + 2.0;
                        let st = &self.status;
                        let (text, dim) = match part {
                            Part::Tray => ("\u{E70E}", false),
                            Part::Net => (st.net_icon(), st.net == status::Net::None),
                            Part::Volume => (st.volume_icon(), st.volume.is_none()),
                            Part::Battery => (st.battery_icon(), false),
                            Part::Clock => (st.time.as_str(), false),
                        };
                        let th = &self.theme;
                        let c = match part {
                            Part::Battery if st.battery.is_some_and(|b| b.0 <= 15 && !b.1) => color(0xff99a4, 1.0),
                            _ if dim => color(th.text3.0, th.text3.1),
                            _ => color(th.text.0, th.text.1),
                        };
                        let f = if part == Part::Clock { &self.clock } else { &self.glyph };
                        let t: Vec<u16> = text.encode_utf16().collect();
                        brush.SetColor(&c);
                        if part == Part::Battery {
                            // Simge solda, yüzde sağında (macOS menü çubuğu gibi).
                            let gl = s.l + 5.0;
                            rt.DrawText(&t, f, &rect(gl, cy - 14.0, gl + BATTERY_GLYPH, cy + 14.0), brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                            let pct: Vec<u16> = st.battery_pct().encode_utf16().collect();
                            let tl = gl + BATTERY_GLYPH;
                            rt.DrawText(&pct, &self.clock, &rect(tl, cy - 14.0, tl + self.battery_w + 4.0, cy + 14.0), brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                            continue;
                        }
                        rt.DrawText(&t, f, &rect(s.l, cy - 14.0, s.r, cy + 14.0), brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                        continue;
                    }
                    let Some(i) = s.item else {
                        let th = &self.theme;
                        brush.SetColor(&color(th.stroke.0, th.stroke.1 + 0.08));
                        rt.FillRectangle(&rect(cx - 0.5, top + PAD_T + icon * 0.15, cx + 0.5, bottom - PAD_B - icon * 0.1), brush);
                        continue;
                    };
                    if self.drag.is_some_and(|d| d.active && d.item == i) {
                        continue;
                    }
                    let item = &self.items[i];
                    let mut ib = bottom - PAD_B;
                    let launched = self.launched.get(&item.id).map(|t| t.elapsed().as_secs_f32());
                    if let Some(lt) = launched.filter(|&lt| lt < BOUNCE) {
                        ib -= (lt * std::f32::consts::PI / 0.55).sin().abs() * icon * 0.32 * (1.0 - lt / BOUNCE * 0.5);
                    } else if let Some(at) = self.attention.get(&item.id).map(|t| t.elapsed().as_secs_f32()).filter(|&a| a < ATTENTION) {
                        // İki sıçrayış, ikincisi daha alçak.
                        ib -= (at * std::f32::consts::PI / 0.55).sin().abs() * icon * 0.38 * (1.0 - at / ATTENTION * 0.6);
                    }
                    let r = rect(cx - s.size / 2.0, ib - s.size, cx + s.size / 2.0, ib);
                    let dim = if self.pressed == Some(i) && self.hover == Some(i) { 0.6 } else { 1.0 };
                    match &icons[i] {
                        Some(b) => rt.DrawBitmap(b, Some(&r), dim, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None),
                        None => fill(rounded(r.left, r.top, r.right, r.bottom, s.size * 0.22), color(self.theme.fill.0, self.theme.fill.1 * dim)),
                    }
                    // Windows görev çubuğu gibi gösterge: çalışıyorsa kısa gri çizgi, öndeyse uzun
                    // ve vurgu renginde, dikkat istiyorsa turuncu, açılırken nabız.
                    let th = &self.theme;
                    let dy = bottom - PAD_B / 2.0;
                    let flash = self.notified.contains(&item.id) || item.windows.iter().any(|h| self.flashing.contains(&(h.0 as isize)));
                    let front = item.windows.contains(&self.foreground);
                    let bar = |half: f32, c: D2D1_COLOR_F| fill(rounded(cx - half, dy - 1.5, cx + half, dy + 1.5, 1.5), c);
                    if let Some(lt) = launched {
                        bar(3.0, color(th.text3.0, 0.3 + 0.4 * (lt * 6.0).sin().abs()));
                    } else if flash {
                        bar(8.0, color(0xffb900, 1.0));
                    } else if front {
                        bar(8.0, color(th.accent.0, 1.0));
                    } else if !item.windows.is_empty() {
                        bar(3.0, color(th.text3.0, th.text3.1));
                    }
                }

                // Sürüklenen simge imleci izler, biraz kalkık ve büyük.
                if let Some(d) = self.drag.filter(|d| d.active) {
                    if let Some(b) = icons.get(d.item).cloned().flatten() {
                        let sz = icon * 1.12;
                        let ib = bottom - PAD_B - 8.0;
                        rt.DrawBitmap(&b, Some(&rect(d.x - sz / 2.0, ib - sz, d.x + sz / 2.0, ib)), 0.92, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                    }
                }

                // Üstüne gelinen öğenin adı.
                let label = match (self.hover, self.hover_part) {
                    (Some(i), _) => slots.iter().find(|s| s.item == Some(i)).map(|s| (*s, self.items[i].name.clone())),
                    (None, Some(p)) => slots.iter().find(|s| s.part == Some(p)).map(|s| {
                        let st = &self.status;
                        let text = match p {
                            Part::Tray => t!("Hidden icons", "Gizli simgeler").to_string(),
                            Part::Net => st.net_text().to_string(),
                            Part::Volume => st.volume_text(),
                            Part::Battery => st.battery_text(),
                            Part::Clock => st.date.clone(),
                        };
                        (Slot { size: icon, ..*s }, text)
                    }),
                    _ => None,
                };
                if let (Some((s, text)), false, None) = (label, self.menu_open, stack::current()) {
                    {
                        let name: Vec<u16> = text.encode_utf16().collect();
                        if let Ok(layout) = self.dw.CreateTextLayout(&name, &self.label, 600.0, LABEL_H) {
                            let mut m = DWRITE_TEXT_METRICS::default();
                            let _ = layout.GetMetrics(&mut m);
                            let cx = (s.l + s.r) / 2.0;
                            let lw = m.width + 22.0;
                            let lb = bottom - PAD_B - s.size - LABEL_GAP;
                            let l = (cx - lw / 2.0).clamp(2.0, wd - lw - 2.0);
                            // Windows ipucu kutusu gibi.
                            let th = &self.theme;
                            fill(rounded(l, lb - LABEL_H, l + lw, lb, 4.0), color(if th.dark { 0x2c2c2c } else { 0xf9f9f9 }, 0.98));
                            brush.SetColor(&color(th.stroke.0, th.stroke.1 + 0.05));
                            rt.DrawRoundedRectangle(&rounded(l + 0.5, lb - LABEL_H + 0.5, l + lw - 0.5, lb - 0.5, 3.5), brush, 1.0, None);
                            brush.SetColor(&color(th.text.0, th.text.1));
                            rt.DrawText(
                                &name,
                                &self.label,
                                &rect(l, lb - LABEL_H, l + lw, lb),
                                brush,
                                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                                DWRITE_MEASURING_MODE_NATURAL,
                            );
                        }
                    }
                }
            }
            // Fareyi tutan, gözle görülmeyen alanlar: büyüyen simgelerin arası, hapın altındaki
            // boşluk (alttan çağırıp fareyi kenarda tutan kaçırmasın) ve gizliyken alt şerit.
            // Piksele hizalı ve yumuşatmasız: kesirli kenarın alfası sıfıra yuvarlanmasın.
            brush.SetColor(&color(0, 0.012));
            rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            let floor = hp as f32 / k + 1.0;
            if (self.mouse.is_some() || self.revealed) && self.shown > 0.0 {
                let t2 = bottom - PAD_B - icon * self.max_mag() - 4.0;
                rt.FillRectangle(&rect(pl, t2, pr, floor), brush);
            }
            if self.shown < 0.02 && self.cover != Cover::Full {
                let (bl, br) = Self::pill_x(&slots);
                rt.FillRectangle(&rect(bl, (hp as f32 - 3.0) / k, br, floor), brush);
            }
            rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            rt.EndDraw(None, None)
        };
        if let Err(e) = ok {
            if e.code() == D2DERR_RECREATE_TARGET {
                self.target = None;
                self.clear_icons();
            }
            return;
        }
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8, ..Default::default() };
        let pos = POINT { x: self.place.left, y: self.place.top };
        let size = SIZE { cx: wp, cy: hp };
        unsafe {
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&pos),
                Some(&size),
                Some(dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
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
            let mut t = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut t);
            }
            self.tracking = true;
        }
        let (x, y) = self.dip(lp);
        self.mouse = Some((x, y));
        if let Some(mut d) = self.drag {
            d.x = x;
            if !d.active && (x - d.start_x).abs() > 6.0 {
                d.active = true;
                self.pressed = None;
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_STACK);
                }
                stack::close();
            }
            if d.active {
                d.target = self.drag_target(d.item, x);
                self.drag = Some(d);
                self.over_items = false;
                self.hover = None;
                self.hover_part = None;
                self.render();
                self.kick();
                return;
            }
            self.drag = Some(d);
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
        }
        if self.shown < 1.0 && self.cover == Cover::Window {
            self.revealed = true;
        }
        let hover = self.hit(x, y);
        if hover != self.hover && hover.is_some() {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_STACK);
            }
            let id = hover.map(|i| self.items[i].id.clone());
            // Başka simgeye geçildi: o simgenin yığını değilse kapanır.
            if stack::current().is_some() && stack::current() != id {
                stack::close();
            }
            if hover.is_some_and(|i| !self.items[i].windows.is_empty()) {
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_STACK, STACK_DWELL_MS, None);
                }
            }
        }
        self.hover = hover;
        self.hover_part = self.hit_part(x, y);
        self.over_items = self.hover_part.is_none() && self.items_span().is_some_and(|(l, r)| x >= l && x < r);
        if self.over_items {
            self.mag_x = Some(x);
        }
        self.render();
        self.kick();
    }

    /// Önizlemeyi ve onu açacak zamanlayıcıyı kapatır (sağ tık, tıklama, kısayollar).
    fn dismiss_popups(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_STACK);
            let _ = KillTimer(Some(self.hwnd), TIMER_STACK_CLOSE);
        }
        if stack::current().is_some() {
            stack::close();
            self.in_stack = false;
        }
    }

    /// Yığın kapandı: fare dock'ta değilse alttan çağrılmış dock da saklanabilir.
    fn after_stack(&mut self) {
        if self.mouse.is_none() && self.revealed {
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_HIDE, 400, None);
            }
        }
        self.render();
        self.kick();
    }

    /// `i`'nin pencerelerini yığın olarak açar.
    fn open_stack(&mut self, i: usize) {
        let Some(item) = self.items.get(i) else { return };
        let Some(s) = self.slots().into_iter().find(|s| s.item == Some(i)) else { return };
        let k = self.k();
        let cx = (s.l + s.r) / 2.0;
        let top = self.pill_bottom() - PAD_B - self.icon_size() * self.max_mag() - 6.0;
        let anchor = (self.place.left + (cx * k) as i32, self.place.top + (top * k) as i32);
        stack::open(self.hwnd, &item.id, &item.icon, &item.windows, anchor, self.monitor, self.dpi);
        self.render();
    }

    fn mouse_leave(&mut self) {
        self.tracking = false;
        self.mouse = None;
        self.over_items = false;
        self.hover = None;
        self.hover_part = None;
        self.pressed = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_STACK);
            if stack::current().is_some() {
                SetTimer(Some(self.hwnd), TIMER_STACK_CLOSE, 350, None);
            }
            if self.revealed {
                SetTimer(Some(self.hwnd), TIMER_HIDE, 400, None);
            }
        }
        self.render();
        self.kick();
    }

    fn click(&mut self, i: usize) -> Option<Action> {
        let item = self.items.get(i)?;
        if item.windows.is_empty() {
            let line = item.line.clone()?;
            if !item.folder() {
                self.launched.insert(item.id.clone(), Instant::now());
                self.kick();
            }
            return Some(Action::Open(line));
        }
        // Birden çok pencere: yığın açılır (açıksa kapanır), pencere oradan seçilir.
        if item.windows.len() > 1 {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_STACK);
            }
            if stack::current().as_deref() == Some(item.id.as_str()) {
                stack::close();
            } else {
                self.open_stack(i);
            }
            return None;
        }
        let fg = self.foreground;
        if item.windows.contains(&fg) {
            return Some(Action::Minimize(fg));
        }
        Some(Action::Activate(item.windows[0]))
    }

    /// Durum simgeleri Windows'un kendi panellerini açar: ağ, ses, pil hızlı ayarları (dock'un
    /// üstüne hizalanır, `place_flyout`); saat bildirimleri ve takvimi (sağda, yerinde kalır).
    fn click_part(&mut self, p: Part) -> Option<Action> {
        if p == Part::Tray {
            let slots = self.slots();
            let s = slots.iter().find(|s| s.part == Some(Part::Tray))?;
            let k = self.k();
            let (_, h) = self.size_dip();
            let pill_top = h - MARGIN - self.pill_h();
            let x = self.place.left + ((s.l + s.r) / 2.0 * k) as i32;
            return Some(Action::Tray(x, self.place.top + ((pill_top - 8.0) * k) as i32));
        }
        Some(if p == Part::Clock { Action::Notices } else { Action::QuickSettings })
    }

    /// Windows'un paneli (ekran boyunda saydam bir kap, içindeki panel alta yaslı) yerine konur.
    /// Hızlı ayarlar: sağ kenarı hapın sağ ucuna, alt kenarı dock'un hemen üstüne. Bildirimler:
    /// Windows'un kendi yeri, ekranın sağ kenarı. Panel kendini geri taşırsa yine yerine konur.
    fn place_flyout(&mut self, hwnd: HWND, kind: isize) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(hwnd, &mut r);
        }
        let (right, bottom) = if kind == QUICK {
            let k = self.k();
            let slots = self.slots();
            let (_, pr) = Self::pill_x(&slots);
            let (_, h) = self.size_dip();
            let pill_top = h - MARGIN - self.pill_h();
            (self.place.left + (pr * k) as i32 + (12.0 * k) as i32, self.place.top + (pill_top * k) as i32 + (4.0 * k) as i32)
        } else {
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            unsafe {
                let _ = GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY), &mut mi);
            }
            (mi.rcMonitor.right, mi.rcWork.bottom)
        };
        let (x, y) = (right - (r.right - r.left), bottom - (r.bottom - r.top));
        self.flyout = Some((hwnd, kind));
        if (r.left - x).abs() > 2 || (r.top - y).abs() > 2 {
            unsafe {
                let _ = SetWindowPos(hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
    }

    /// Win+rakam: dock'taki sıradaki uygulama. Kapalıysa açar, arkadaysa öne getirir, öndeyse
    /// küçültür; birden çok penceresi varsa aralarında sırayla gezer.
    fn key_launch(&mut self, n: usize) -> Option<Action> {
        let i = *self.display_order().get(n)?;
        let item = self.items.get(i)?;
        if item.windows.is_empty() {
            let line = item.line.clone()?;
            self.launched.insert(item.id.clone(), Instant::now());
            self.kick();
            return Some(Action::Open(line));
        }
        let fg = unsafe { GetForegroundWindow() };
        Some(match item.windows.iter().position(|&w| w == fg) {
            Some(p) if item.windows.len() > 1 => Action::Activate(item.windows[(p + 1) % item.windows.len()]),
            Some(_) => Action::Minimize(fg),
            None => Action::Activate(item.windows[0]),
        })
    }

    fn new_window(&mut self, i: usize) -> Option<Action> {
        let item = self.items.get(i)?;
        let line = item.line.clone()?;
        self.launched.insert(item.id.clone(), Instant::now());
        self.kick();
        Some(Action::Open(line))
    }

    fn menu_spec(&self, i: usize) -> Option<MenuSpec> {
        let item = self.items.get(i)?;
        let slots = self.slots();
        let s = slots.iter().find(|s| s.item == Some(i))?;
        let k = self.k();
        let cx = (s.l + s.r) / 2.0;
        let top = self.pill_bottom() - self.pill_h() - 8.0;
        Some(MenuSpec {
            id: item.id.clone(),
            name: item.name.clone(),
            icon: item.icon.clone(),
            windows: item.windows.iter().map(|&h| (h, apps::title(h))).collect(),
            pinned: item.pin.is_some(),
            line: item.line.clone(),
            x: self.place.left + (cx * k) as i32,
            y: self.place.top + (top * k) as i32,
        })
    }


    /// Sağ tık menüsünden seçilen.
    fn menu_done(&mut self, spec: &MenuSpec, choice: usize) -> Option<Action> {
        self.menu_open = false;
        let r = match choice {
            0 => None,
            c if c >= 1000 => spec.windows.get(c - 1000).map(|w| Action::Activate(w.0)),
            M_PIN => {
                if spec.pinned {
                    self.lines.retain(|l| !l.eq_ignore_ascii_case(&spec.id));
                } else if let Some(line) = &spec.line {
                    self.lines.push(line.clone());
                }
                super::save_pins(&self.lines);
                self.pins = self.lines.iter().filter_map(|l| Pin::resolve(l)).collect();
                self.refresh();
                None
            }
            M_CLOSE => {
                for w in &spec.windows {
                    apps::close(w.0);
                }
                None
            }
            M_SETTINGS => Some(Action::Settings),
            _ => None,
        };
        self.render();
        self.kick();
        r
    }

    // --- Appbar: otomatik gizleme kapalıyken ekranın altında yer ayırır ---

    fn appbar_data(&self) -> APPBARDATA {
        APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            hWnd: self.hwnd,
            uCallbackMessage: WM_APPBAR,
            uEdge: ABE_BOTTOM,
            ..Default::default()
        }
    }

    fn set_appbar(&mut self, on: bool) {
        if on == self.appbar {
            return;
        }
        let mut d = self.appbar_data();
        unsafe {
            SHAppBarMessage(if on { ABM_NEW } else { ABM_REMOVE }, &mut d);
        }
        self.appbar = on;
        if on {
            self.appbar_pos();
        }
    }

    fn appbar_pos(&self) {
        // Ekran henüz ölçülmediyse (açılışın ilk anı) relayout yeniden çağırır.
        if self.monitor.bottom <= self.monitor.top {
            return;
        }
        let mut d = self.appbar_data();
        let band = (self.band_dip() * self.k()).ceil() as i32;
        d.rc = self.monitor;
        d.rc.top = d.rc.bottom - band;
        unsafe {
            SHAppBarMessage(ABM_QUERYPOS, &mut d);
            d.rc.top = d.rc.bottom - band;
            SHAppBarMessage(ABM_SETPOS, &mut d);
        }
    }

    fn apply(&mut self, s: Settings) {
        let old = std::mem::replace(&mut self.settings, s);
        if old.size != s.size || old.magnify != s.magnify {
            self.clear_icons();
        }
        if old.hide_icons != s.hide_icons {
            taskbar::set_icons_hidden(s.hide_icons);
        }
        self.set_appbar(!s.autohide);
        self.cover = self.compute_cover();
        self.relayout();
        self.kick();
    }
}

impl Drop for Dock {
    fn drop(&mut self) {
        stack::close();
        super::menu::close();
        if self.live {
            super::keys::unregister();
            super::toasts::stop();
        }
        self.set_appbar(false);
        unsafe {
            for h in self.hooks.drain(..) {
                let _ = UnhookWinEvent(h);
            }
            let _ = DeregisterShellHookWindow(self.hwnd);
            let _ = DestroyWindow(self.hwnd);
        }
        if self.live {
            HWND_DOCK.set(0);
        }
    }
}

const M_PIN: usize = 2;
const M_CLOSE: usize = 3;
const M_SETTINGS: usize = 4;


/// Sağ tık menüsünün satırları.
fn menu_entries(spec: &MenuSpec) -> Vec<super::menu::Entry> {
    use super::menu::{Entry, Glyph};
    let mut v = Vec::new();
    if spec.windows.len() > 1 {
        for (i, (_, title)) in spec.windows.iter().enumerate().take(12) {
            let t: String = if title.chars().count() > 48 { title.chars().take(47).collect::<String>() + "…" } else { title.clone() };
            v.push(Entry { id: 1000 + i, text: t, glyph: Some(Glyph::App) });
        }
        v.push(Entry::sep());
    }
    if spec.pinned {
        v.push(Entry { id: M_PIN, text: t!("Remove from dock", "Dock'tan kaldır").into(), glyph: Some(Glyph::Fluent("\u{E77A}")) });
    } else if spec.line.is_some() {
        v.push(Entry { id: M_PIN, text: t!("Keep in dock", "Dock'ta tut").into(), glyph: Some(Glyph::Fluent("\u{E718}")) });
    }
    match spec.windows.len() {
        0 => {}
        1 => v.push(Entry { id: M_CLOSE, text: t!("Close window", "Pencereyi kapat").into(), glyph: Some(Glyph::Fluent("\u{E8BB}")) }),
        n => v.push(Entry {
            id: M_CLOSE,
            text: t!(format!("Close all windows ({n})"), format!("Bütün pencereleri kapat ({n})")),
            glyph: Some(Glyph::Fluent("\u{E8BB}")),
        }),
    }
    v.push(Entry::sep());
    v.push(Entry { id: M_SETTINGS, text: t!("Dock settings", "Dock ayarları").into(), glyph: Some(Glyph::Fluent("\u{E713}")) });
    v
}

fn act(a: Action) {
    match a {
        Action::Open(t) => apps::open(&t),
        Action::Activate(h) => apps::activate(h),
        Action::Minimize(h) => apps::minimize(h),
        Action::Settings => crate::app::forward(Some("hatter")),
        Action::Notices => {
            use windows::Win32::UI::Input::KeyboardAndMouse::{VK_LWIN, VK_N};
            FLYOUT.set(Some((Instant::now(), NOTICES)));
            super::press(&[VK_LWIN, VK_N]);
        }
        Action::QuickSettings => {
            use windows::Win32::UI::Input::KeyboardAndMouse::{VK_A, VK_LWIN};
            FLYOUT.set(Some((Instant::now(), QUICK)));
            super::press(&[VK_LWIN, VK_A]);
        }
        Action::Tray(x, y) => super::tray::toggle(x, y),
        Action::Menu(spec) => {
            let hwnd = HWND(HWND_DOCK.get() as *mut _);
            let Some((monitor, dpi)) = with(|d| (d.monitor, d.dpi)) else { return };
            let (entries, name, icon, anchor) = (menu_entries(&spec), spec.name.clone(), spec.icon.clone(), (spec.x, spec.y));
            with(|d| d.menu = Some(spec));
            super::menu::open(hwnd, &name, &icon, entries, anchor, monitor, dpi);
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let action = match msg {
        WM_REFRESH => {
            REFRESH_POSTED.set(false);
            if with(|d| d.refresh()).is_none() {
                unsafe { SetTimer(Some(hwnd), TIMER_RETRY, 30, None) };
            }
            return LRESULT(0);
        }
        WM_COVER => {
            COVER_POSTED.set(false);
            with(|d| {
                let fg = unsafe { GetForegroundWindow() };
                match d.flyout {
                    Some((f, kind)) if f == fg => d.place_flyout(f, kind),
                    Some(_) => d.flyout = None,
                    None => {}
                }
                let c = d.compute_cover();
                if c != d.cover {
                    d.cover = c;
                    d.render();
                    d.kick();
                }
            });
            return LRESULT(0);
        }
        WM_TIMER => {
            match wp.0 {
                TIMER_ANIM => {
                    with(|d| d.step());
                }
                TIMER_HIDE => {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_HIDE);
                    }
                    with(|d| {
                        if d.mouse.is_none() && !d.menu_open && stack::current().is_none() {
                            d.revealed = false;
                            d.cover = d.compute_cover();
                            d.kick();
                        }
                    });
                }
                TIMER_ICON_RETRY => {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_ICON_RETRY);
                    }
                    with(|d| d.render());
                }
                TIMER_CLOCK => {
                    with(|d| {
                        let next = d.status.read_time();
                        let old = (d.clock_w, d.battery_w);
                        d.measure_clock();
                        if (d.clock_w - old.0).abs() > 0.5 || (d.battery_w - old.1).abs() > 0.5 {
                            d.relayout();
                        } else {
                            d.render();
                        }
                        unsafe {
                            SetTimer(Some(hwnd), TIMER_CLOCK, next, None);
                        }
                    });
                }
                TIMER_STACK => {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_STACK);
                    }
                    with(|d| {
                        // Menü açıkken ya da sürüklerken önizleme açılmaz.
                        if d.menu_open || d.drag.is_some_and(|dr| dr.active) {
                            return;
                        }
                        if let Some(i) = d.hover.filter(|&i| d.items.get(i).is_some_and(|it| !it.windows.is_empty()))
                            && stack::current().as_deref() != Some(d.items[i].id.as_str())
                        {
                            d.open_stack(i);
                        }
                    });
                }
                TIMER_STACK_CLOSE => {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_STACK_CLOSE);
                    }
                    with(|d| {
                        if !d.in_stack && d.mouse.is_none() {
                            stack::close();
                            d.after_stack();
                        }
                    });
                }
                TIMER_TRAY => {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_TRAY);
                    }
                    taskbar::ensure_hidden();
                }
                TIMER_RETRY => {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_RETRY);
                    }
                    post_once(&REFRESH_POSTED, WM_REFRESH);
                }
                _ => {}
            }
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            with(|d| d.mouse_move(lp));
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with(|d| d.mouse_leave());
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            with(|d| {
                // Tıklamak önizlemeyi kapatır (Windows görev çubuğundaki gibi).
                d.dismiss_popups();
                d.pressed = d.hover;
                if let (Some(i), true, Some((x, _))) = (d.hover, d.settings.reorder, d.mouse) {
                    d.drag = Some(Drag { item: i, start_x: x, x, active: false, target: i });
                    unsafe {
                        SetCapture(hwnd);
                    }
                }
                d.render();
            });
            return LRESULT(0);
        }
        WM_CAPTURECHANGED => {
            // Sürükleme yarıda kaldı (Alt+Tab, başka pencere): iptal.
            with(|d| {
                if d.drag.take().is_some_and(|dr| dr.active) {
                    d.render();
                }
            });
            return LRESULT(0);
        }
        WM_LBUTTONUP => with(|d| {
            if let Some(dr) = d.drag.take() {
                unsafe {
                    let _ = ReleaseCapture();
                }
                if dr.active {
                    d.pressed = None;
                    if dr.target != dr.item {
                        d.drag = Some(dr);
                        d.commit_drag(dr);
                        d.drag = None;
                    }
                    d.render();
                    return None;
                }
            }
            if let Some(part) = d.hover_part {
                d.pressed = None;
                return d.click_part(part);
            }
            let p = d.pressed.take();
            let r = if p.is_some() && p == d.hover { p.and_then(|i| d.click(i)) } else { None };
            d.render();
            r
        })
        .flatten(),
        WM_MBUTTONUP => with(|d| d.hover.and_then(|i| d.new_window(i))).flatten(),
        WM_RBUTTONUP => with(|d| {
            let spec = d.hover.and_then(|i| d.menu_spec(i))?;
            d.dismiss_popups();
            d.menu_open = true;
            d.render();
            Some(Action::Menu(spec))
        })
        .flatten(),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        super::keys::WM_DOCK_KEY => {
            super::menu::close();
            if let Some(Some(a)) = with(|d| {
                d.dismiss_popups();
                d.key_launch(wp.0)
            }) {
                act(a);
            }
            return LRESULT(0);
        }
        super::menu::WM_MENU_DONE => {
            let spec = with(|d| d.menu.take()).flatten();
            match spec {
                Some(spec) => {
                    if let Some(Some(a)) = with(|d| d.menu_done(&spec, wp.0)) {
                        act(a);
                    }
                }
                None => {
                    with(|d| {
                        d.menu_open = false;
                        d.render();
                        d.kick();
                    });
                }
            }
            return LRESULT(0);
        }
        super::toasts::WM_TOAST => {
            let aumid = unsafe { Box::from_raw(lp.0 as *mut String) };
            with(|d| d.toast(&aumid));
            return LRESULT(0);
        }
        WM_RELOAD => {
            with(|d| {
                d.lines = super::load_pins();
                let old = std::mem::take(&mut d.pins);
                d.pins = d
                    .lines
                    .iter()
                    .filter_map(|l| old.iter().find(|p| p.target.eq_ignore_ascii_case(l)).cloned().or_else(|| Pin::resolve(l)))
                    .collect();
                d.refresh();
            });
            return LRESULT(0);
        }
        WM_ICON => {
            // Dock o an meşgulse (iç içe mesaj) sonuç kuyruğa geri konur.
            let busy = DOCK.with(|d| d.try_borrow_mut().is_err());
            if busy {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_ICON, wp, lp);
                }
            } else {
                let d = unsafe { Box::from_raw(lp.0 as *mut IconDone) };
                with(|dock| dock.icon_loaded(*d));
            }
            return LRESULT(0);
        }
        WM_FLYOUT => {
            with(|d| d.place_flyout(HWND(wp.0 as *mut _), lp.0));
            return LRESULT(0);
        }
        WM_STATUS => {
            with(|d| {
                match wp.0 {
                    status::NET => d.status.read_net(),
                    status::VOLUME => d.status.read_volume(),
                    _ => d.status.attach_volume(true),
                }
                d.render();
            });
            return LRESULT(0);
        }
        WM_POWERBROADCAST => {
            with(|d| {
                let had = d.status.battery.is_some();
                let old = d.battery_w;
                d.status.read_battery();
                d.measure_clock();
                if had != d.status.battery.is_some() || (d.battery_w - old).abs() > 0.5 {
                    d.relayout();
                } else {
                    d.render();
                }
            });
            return LRESULT(1);
        }
        WM_STACK_ENTER => {
            with(|d| {
                d.in_stack = true;
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_STACK_CLOSE);
                    let _ = KillTimer(Some(hwnd), TIMER_HIDE);
                }
            });
            return LRESULT(0);
        }
        WM_STACK_LEAVE => {
            with(|d| {
                d.in_stack = false;
                unsafe {
                    SetTimer(Some(hwnd), TIMER_STACK_CLOSE, 350, None);
                }
            });
            return LRESULT(0);
        }
        WM_STACK_DONE => {
            with(|d| {
                d.in_stack = false;
                d.after_stack();
            });
            return LRESULT(0);
        }
        WM_APPBAR => {
            if wp.0 as u32 == ABN_POSCHANGED {
                with(|d| d.appbar_pos());
            }
            post_once(&COVER_POSTED, WM_COVER);
            return LRESULT(0);
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => {
            with(|d| {
                d.theme = theme::current();
                d.relayout();
                d.cover = d.compute_cover();
                d.kick();
            });
            return LRESULT(0);
        }
        _ => {
            let (shellhook, created) = with(|d| (d.shellhook, d.taskbar_created)).unwrap_or((0, 0));
            if msg != 0 && msg == shellhook {
                let target = lp.0;
                match wp.0 as u32 {
                    0x8006 => {
                        // HSHELL_FLASH: pencere dikkat istiyor.
                        with(|d| d.attention(target));
                    }
                    HSHELL_WINDOWACTIVATED | 0x8004 => {
                        with(|d| d.flashing.remove(&target));
                        post_once(&REFRESH_POSTED, WM_REFRESH);
                    }
                    // Explorer kabukken yanıp sönme (HSHELL_FLASH) bize gelmez, "yeniden çizildi" gelir.
                    // Başlık değişmediyse dikkat isteği (yanıp sönme, ikona rozet), değiştiyse
                    // sıradan güncelleme (Spotify'ın şarkı adı gibi).
                    HSHELL_REDRAW => {
                        with(|d| d.redrawn(target));
                        post_once(&REFRESH_POSTED, WM_REFRESH)
                    }
                    HSHELL_WINDOWCREATED | HSHELL_WINDOWDESTROYED => post_once(&REFRESH_POSTED, WM_REFRESH),
                    _ => {}
                }
                return LRESULT(0);
            }
            if msg != 0 && msg == created {
                // Explorer yeniden başladı: görev çubuğu geri geldi, appbar kaydı gitti.
                with(|d| {
                    taskbar::hide(d.settings.hide_icons);
                    unsafe {
                        SetTimer(Some(d.hwnd), TIMER_TRAY, 1500, None);
                    }
                    if d.appbar {
                        d.appbar = false;
                        d.set_appbar(true);
                    }
                });
                post_once(&REFRESH_POSTED, WM_REFRESH);
                return LRESULT(0);
            }
            return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
        }
    };
    if let Some(a) = action {
        act(a);
    }
    LRESULT(0)
}

// --- Dışarıya ---

/// Dock penceresini ve çizim araçlarını kurar (henüz kanca yok, görünmez).
fn create(settings: Settings, lines: Vec<String>) -> windows::core::Result<Dock> {
    {
        unsafe {
            let inst = GetModuleHandleW(None)?;
            let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst.into(), lpszClassName: CLASS, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                CLASS,
                w!("hatter"),
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
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let label = dw.CreateTextFormat(
                w!("Segoe UI Variable Text"),
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                12.5,
                w!("tr-tr"),
            )?;
            label.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            label.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            label.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let fmt = |family: PCWSTR, size: f32, weight| -> windows::core::Result<IDWriteTextFormat> {
                let f = dw.CreateTextFormat(family, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("tr-tr"))?;
                f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                Ok(f)
            };
            let glyph = fmt(w!("Segoe Fluent Icons"), 16.0, DWRITE_FONT_WEIGHT_NORMAL)?;
            let clock = fmt(w!("Segoe UI Variable Text"), 13.5, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
            Ok(Dock {
                hwnd,
                live: false,
                settings,
                pins: lines.iter().filter_map(|l| Pin::resolve(l)).collect(),
                lines,
                items: Vec::new(),
                order: Vec::new(),
                running: HashMap::new(),
                flashing: HashSet::new(),
                launched: HashMap::new(),
                attention: HashMap::new(),
                titles: HashMap::new(),
                notified: HashSet::new(),
                foreground: HWND::default(),
                d2d,
                wic,
                dw,
                label,
                target: None,
                surface: None,
                icons: HashMap::new(),
                loading: HashSet::new(),
                failed: HashMap::new(),
                icon_gen: 0,
                dpi: 96.0,
                monitor: RECT::default(),
                place: RECT::default(),
                mouse: None,
                tracking: false,
                hover: None,
                pressed: None,
                menu_open: false,
                in_stack: false,
                flyout: None,
                drag: None,
                menu: None,
                status: Status::new(hwnd, false),
                theme: theme::current(),
                hover_part: None,
                glyph,
                clock,
                clock_w: 40.0,
                battery_w: 24.0,
                shown: 0.0,
                mag: 0.0,
                dirty: true,
                mag_x: None,
                over_items: false,
                tick: Instant::now(),
                animating: false,
                cover: Cover::Clear,
                revealed: false,
                hidden_window: true,
                appbar: false,
                hooks: Vec::new(),
                shellhook: RegisterWindowMessageW(w!("SHELLHOOK")),
                taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            })
        }
    }
}

/// Dock'u açar (zaten açıksa ayarları uygular). Arayüz iş parçacığından çağrılır.
pub fn start(settings: Settings, lines: Vec<String>) {
    if HWND_DOCK.get() != 0 {
        apply(settings);
        return;
    }
    let mut dock = match create(settings, lines) {
        Ok(d) => d,
        Err(e) => {
            crate::log!("hatter: dock açılamadı: {e}");
            return;
        }
    };
    dock.live = true;
    dock.status = Status::new(dock.hwnd, true);
    super::keys::register(dock.hwnd);
    super::toasts::start(dock.hwnd);
    HWND_DOCK.set(dock.hwnd.0 as isize);
    unsafe {
        let _ = RegisterShellHookWindow(dock.hwnd);
        for (a, b) in [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_NAMECHANGE),
            (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
        ] {
            let h = SetWinEventHook(a, b, None, Some(on_event), 0, 0, WINEVENT_OUTOFCONTEXT);
            if !h.is_invalid() {
                dock.hooks.push(h);
            }
        }
    }
    dock.set_appbar(!dock.settings.autohide);
    DOCK.with_borrow_mut(|d| *d = Some(dock));
    with(|d| {
        d.measure_clock();
        d.relayout();
        d.refresh();
        d.kick();
        let next = d.status.read_time();
        unsafe {
            SetTimer(Some(d.hwnd), TIMER_TRAY, 1500, None);
            SetTimer(Some(d.hwnd), TIMER_CLOCK, next, None);
        }
    });
}

pub fn stop() {
    let dock = DOCK.with(|d| d.borrow_mut().take());
    drop(dock);
}

pub fn apply(settings: Settings) {
    with(|d| d.apply(settings));
}


/// Başka bir süreçten (sağ tık menüsü): açık dock listeyi yeniden okusun.
pub fn notify_reload() {
    unsafe {
        if let Ok(h) = FindWindowW(CLASS, PCWSTR::null()) {
            let _ = PostMessageW(Some(h), WM_RELOAD, WPARAM(0), LPARAM(0));
        }
    }
}
