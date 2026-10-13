//! Pencere: solda daraltılabilir kenar çubuğu (açık araçlar, Araçlar, Ayarlar), sağda seçili
//! sayfa. Başlık çubuğu yok: pencere düğmelerini ve sürükleme bandını kendimiz çiziyoruz.
//! Tek iş parçacığı; yalnızca bir şey değişince çizilir. Kapatınca tepside kalır, araçların
//! motorları çalışmaya devam eder.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, InvalidateRect, ScreenToClient, ValidateRect};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

use ortak::gfx::{Color, Gfx, ImageId, Rect};
use ortak::hub::{CLASS, COPY_TAB, WM_EXIT, WM_PENDING_TAB};
use ortak::log;
use ortak::form::{self, Ctl, Form, Item, Row};
use ortak::ui::*;
use ortak::util::{self, Res, wide};

use crate::config::Config;
use crate::tools::{self, TOOLS};
use crate::tray::Tray;

pub const NAME: &str = "winloom";
const VERSION: &str = env!("CARGO_PKG_VERSION");

const WM_TRAY: u32 = WM_APP + 1;
/// Bir aracın kurulumu ya da kaldırılması bitti (sonuçlar `results` kuyruğunda).
const WM_TOOL_SETUP: u32 = WM_APP + 2;
/// winloom'ın kendini kaldırması bitti (hatalar `self_errors`'ta).
const WM_SELF_DONE: u32 = WM_APP + 4;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_NCMOUSELEAVE: u32 = 0x02A2;

/// Kenar çubuğu: etiketli ve yalnızca ikon genişliği. Pencere AUTO_NARROW'dan darsa hep ikon.
const WIDE: f32 = 240.0;
const NARROW: f32 = 56.0;
const AUTO_NARROW: f32 = 640.0;
const NAV: f32 = 40.0;
/// Araçlar ve Ayarlar sayfalarının kenar boşluğu: araç sayfalarıyla aynı.
const PAGE_PAD: f32 = PAD;
/// Araçlar sayfasındaki kartlar.

/// Pencere düğmeleri (küçült, büyüt, kapat).
const CAP_W: f32 = 46.0;
const CAP_H: f32 = HEADER;
const CAPTIONS: f32 = CAP_W * 3.0;
const CLOSE_RED: Color = Color::rgb(0xc42b1c);

const ICON_HOME: &str = "\u{E80F}";
/// Ana sayfadaki kutucuklar.
const TILE_W: f32 = 220.0;
const TILE_H: f32 = 104.0;
const TILE_GAP: f32 = 14.0;
/// Ayarlar sayfasının satırları.
const S_AUTOSTART: u32 = 1;
const S_LANG: u32 = 2;
const S_VERSION: u32 = 3;
const S_AUTO_UPDATE: u32 = 4;
const S_QUIT: u32 = 5;
const S_REMOVE: u32 = 6;
/// Ana sayfa açıkken kutucukları tazeleyen zamanlayıcı (araçların durumu değişir).
const TIMER_HOME: usize = 9;
/// Kenar çubuğunun başındaki arama kutusu ve altındaki boşluk.
const SEARCH_H: f32 = 46.0;
/// Arama sonuçlarının satırı ve en çok kaç tane.
const FOUND_H: f32 = 40.0;
const FOUND_MAX: usize = 8;
/// Kenar çubuğundaki "Araçlar" başlığının yüksekliği.
const SECTION_H: f32 = 30.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Cap {
    Min,
    Max,
    Close,
}

impl Cap {
    const ALL: [Cap; 3] = [Cap::Min, Cap::Max, Cap::Close];

    fn ht(self) -> u32 {
        match self {
            Cap::Min => HTMINBUTTON,
            Cap::Max => HTMAXBUTTON,
            Cap::Close => HTCLOSE,
        }
    }

    fn from_ht(ht: u32) -> Option<Cap> {
        Cap::ALL.into_iter().find(|c| c.ht() == ht)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Page {
    Home,
    Tool(usize),
    Store,
    Settings,
}

impl Page {
    fn id(self) -> &'static str {
        match self {
            Page::Home => "home",
            Page::Tool(i) => TOOLS[i].id,
            Page::Store => "tools",
            Page::Settings => "settings",
        }
    }

    fn from_id(s: &str) -> Option<Page> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("home") || s.eq_ignore_ascii_case("ana") {
            return Some(Page::Home);
        }
        if s.eq_ignore_ascii_case("tools") || s.eq_ignore_ascii_case("araclar") {
            return Some(Page::Store);
        }
        if s.eq_ignore_ascii_case("settings") || s.eq_ignore_ascii_case("ayarlar") {
            return Some(Page::Settings);
        }
        tools::find(s).map(Page::Tool)
    }

    fn label(self) -> &'static str {
        match self {
            Page::Home => t!("Home", "Ana sayfa"),
            Page::Tool(i) => TOOLS[i].name(),
            Page::Store => t!("Add tools", "Araç ekle"),
            Page::Settings => t!("Settings", "Ayarlar"),
        }
    }
}

/// Arama kutusunun bir sonucu.
#[derive(Clone, PartialEq, Debug)]
enum Found {
    /// O sayfaya git.
    Go(Page),
    /// Kapalı araç: Araç ekle'de açılır.
    TurnOn(usize),
    /// Yazılanı araca (engram) soru olarak bırak.
    Ask(usize),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    /// Arama kutusu ve sonuçlarından biri.
    Search,
    Found(usize),
    Toggle,
    Nav(Page),
    /// Aracın kendi sayfası (araç kendisi işler).
    Content,
    /// Ana sayfadaki kutucuk: aracın sayfasına gider.
    Open(usize),
    /// Ana sayfadaki soru kutusu.
    Ask,
    /// Araç ekle ve Ayarlar sayfalarının (sayfa kiti) öğeleri.
    Form(form::Hit),
}

type SetupResult = (usize, bool, Result<(), String>);

struct App {
    hwnd: HWND,
    gfx: Gfx,
    tray: Tray,
    /// winloom'ın kendi ikonu (kenar çubuğunun başında).
    logo: ImageId,
    page: Page,
    narrow_pref: bool,
    /// Araçlar sayfasının kaydırması (kartlar küçük pencereye sığmayınca).
    /// Araç ekle ya da Ana sayfanın kaydırması (pencereye sığmayınca).
    scroll: f32,
    autostart: bool,
    /// Açık araçların sayfaları ve motorları, TOOLS sırasıyla.
    tools: Vec<Option<Box<dyn ToolPage>>>,
    /// Süren kurulum (`true`) ya da kaldırma (`false`), araç başına.
    busy: [Option<bool>; tools::COUNT],
    /// Son kurulum / kaldırma hatası, araç kartında görünür.
    errors: [Option<String>; tools::COUNT],
    results: Arc<Mutex<Vec<SetupResult>>>,
    /// winloom kendini kaldırıyor; bitince süreç kapanır, klasör arkasından silinir.
    removing_self: bool,
    self_errors: Arc<Mutex<Vec<String>>>,
    /// Kenar çubuğunun başındaki arama kutusu ve seçili sonuç.
    search: TextField,
    /// Ana sayfadaki soru kutusu (asistana).
    ask: TextField,
    pick: usize,
    hover: Hit,
    pressed: Hit,
    dpi: f32,
    taskbar_created: u32,
    /// Tepsi simgesine tıklamak pencereyi arka plana atar; az önce öndeyse tıklama "gizle" demektir.
    deactivated: Option<Instant>,
    active: bool,
    cap_hover: Option<Cap>,
    cap_pressed: Option<Cap>,
    auto_update: bool,
    /// Hazır güncelleme tepsiden haber verildi.
    update_told: bool,
}

/// Başlık çubuğunu biz çiziyoruz (pencere kurulduktan sonra açılır).
static CUSTOM_FRAME: AtomicBool = AtomicBool::new(false);

/// WM_NCCALCSIZE: başlık çubuğunu ve üst kenarlığı kaldırır, diğer kenarlıklar yerinde kalır.
/// Uygulamanın durumuna bakmaz: uygulama o an meşgulken (ör. pencere geri açılırken iç içe
/// gelen mesaj) de doğru cevaplanmalı, yoksa Windows standart çerçeveye döner.
fn nc_calc_size(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let p = unsafe { &mut *(lp.0 as *mut NCCALCSIZE_PARAMS) };
    let top = p.rgrc[0].top;
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    p.rgrc[0].top = top;
    if unsafe { IsZoomed(hwnd).as_bool() } {
        // Büyütülmüş pencere ekrandan kenarlık kadar taşar: üstte o kadar içeri al.
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let frame =
            unsafe { GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) };
        p.rgrc[0].top += frame;
    }
    LRESULT(0)
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn autostart() -> bool {
    crate::shell::autostart()
}

fn set_autostart(on: bool) {
    match util::installed_copy() {
        Some(exe) if on => crate::shell::set_autostart(Some(&exe)),
        _ => crate::shell::set_autostart(None),
    }
}

impl App {
    fn size(&self) -> (f32, f32) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        (rc.right as f32 * 96.0 / self.dpi, rc.bottom as f32 * 96.0 / self.dpi)
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn save(&self) {
        Config {
            narrow: self.narrow_pref,
            tab: self.page.id().into(),
            turkish: ortak::i18n::turkish(),
            auto_update: self.auto_update,
        }
        .save();
    }

    fn visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() && !IsIconic(self.hwnd).as_bool() }
    }

    fn zoomed(&self) -> bool {
        unsafe { IsZoomed(self.hwnd).as_bool() }
    }

    fn installed(&self, i: usize) -> bool {
        self.tools[i].is_some()
    }

    fn page_ref(&self, i: usize) -> Option<&dyn ToolPage> {
        self.tools[i].as_deref()
    }

    fn page_mut(&mut self, i: usize) -> Option<&mut (dyn ToolPage + 'static)> {
        self.tools[i].as_deref_mut()
    }

    /// Kenar çubuğu sırasıyla sayfalar: Ana sayfa, açık araçlar, Araç ekle, Ayarlar.
    fn pages(&self) -> Vec<Page> {
        std::iter::once(Page::Home)
            .chain(self.top_tools().into_iter().map(Page::Tool))
            .chain(self.live_tools().into_iter().map(Page::Tool))
            .chain([Page::Store, Page::Settings])
            .collect()
    }

    /// Açık "üst" araçlar (asistan): Ana sayfanın hemen altında, Araçlar'dan ayrı.
    fn top_tools(&self) -> Vec<usize> {
        (0..TOOLS.len()).filter(|&i| self.installed(i) && TOOLS[i].top).collect()
    }

    /// Soru kutusunun aracı (asistan): açık ve soru alıyorsa.
    fn asker(&self) -> Option<usize> {
        self.top_tools().into_iter().next()
    }

    /// Aracın görünen adı: açıksa sayfasının verdiği (engram: asistanın adı), değilse kendi adı.
    fn tool_name(&self, i: usize) -> String {
        self.page_ref(i).and_then(|p| p.title()).unwrap_or_else(|| TOOLS[i].name().to_string())
    }

    /// Sayfanın kenar çubuğundaki adı.
    fn label(&self, page: Page) -> String {
        match page {
            Page::Tool(i) => self.tool_name(i),
            p => p.label().to_string(),
        }
    }

    /// Açık araçlar ("Araçlar" başlığının altındakiler), kenar çubuğu sırasıyla.
    fn live_tools(&self) -> Vec<usize> {
        (0..TOOLS.len()).filter(|&i| self.installed(i) && !TOOLS[i].top).collect()
    }

    /// Kurulu değilse araç sayfası yerine Araçlar açılır.
    fn valid(&self, page: Page) -> Page {
        match page {
            Page::Tool(i) if !self.installed(i) => Page::Home,
            p => p,
        }
    }

    /// Seçili sayfa bir aracın kendi arayüzü mü.
    fn tool_live(&self) -> Option<usize> {
        match self.page {
            Page::Tool(i) if self.installed(i) => Some(i),
            _ => None,
        }
    }

    /// Sayfaların görünürlüğünü bildirir: zamanlayıcılar yalnızca ekrandaki sayfada çalışır.
    fn sync_visible(&mut self) {
        let on = self.visible();
        let page = self.page;
        unsafe {
            if on && page == Page::Home {
                SetTimer(Some(self.hwnd), TIMER_HOME, 2000, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_HOME);
            }
        }
        for (i, t) in self.tools.iter_mut().enumerate() {
            if let Some(t) = t {
                t.set_visible(on && page == Page::Tool(i));
            }
        }
    }

    fn select(&mut self, page: Page) {
        let page = self.valid(page);
        if self.page != page {
            self.page = page;
            self.scroll = 0.0;
            self.hover = Hit::None;
            self.save();
            self.sync_visible();
            self.redraw();
        }
    }

    // --- Araçlar: kur, kaldır, motorları başlat ---

    /// Motorları açık araçlarla eşler: açık olup çalışmayanı başlatır, kapatılanı düşürür.
    fn sync_tools(&mut self) {
        for i in 0..TOOLS.len() {
            if self.busy[i].is_some() {
                continue;
            }
            let on = tools::installed(i);
            if on == self.installed(i) {
                continue;
            }
            self.tools[i] = on.then(|| tools::start(i, self.hwnd));
        }
        self.page = self.valid(self.page);
        self.sync_visible();
        let installed: Vec<bool> = (0..TOOLS.len()).map(|i| self.installed(i)).collect();
        crate::shell::sync_shortcuts(&installed);
        self.redraw();
    }

    fn confirm_remove(&self, i: usize) -> bool {
        let detail = TOOLS[i].off_note();
        let name = TOOLS[i].name();
        let text = wide(&t!(
            format!("Turn off {name}?\n\n{detail}\n\nNo trace of it is left on this computer; you can turn it on again any time."),
            format!("{name} kapatılsın mı?\n\n{detail}\n\nBilgisayarda hiçbir izi kalmaz; istediğin zaman yeniden açabilirsin.")
        ));
        unsafe { MessageBoxW(Some(self.hwnd), PCWSTR(text.as_ptr()), w!("winloom"), MB_YESNO | MB_ICONQUESTION) == IDYES }
    }

    /// Kurulum ya da kaldırma arka planda; bitince WM_TOOL_SETUP gelir.
    fn start_setup(&mut self, i: usize, install: bool) {
        if self.busy[i].is_some() || (!install && !self.confirm_remove(i)) {
            return;
        }
        if !install {
            // Motor önce durur: duvar kâğıdı motorunun exe'si silinebilsin, soundboard kısayolları bıraksın.
            self.tools[i] = None;
            self.page = self.valid(self.page);
        }
        self.busy[i] = Some(install);
        self.errors[i] = None;
        self.redraw();
        let (results, hwnd) = (self.results.clone(), self.hwnd.0 as usize);
        std::thread::spawn(move || {
            let r = tools::run(i, install);
            results.lock().unwrap().push((i, install, r));
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_TOOL_SETUP, WPARAM(0), LPARAM(0));
            }
        });
    }

    fn setup_done(&mut self) {
        let done = std::mem::take(&mut *self.results.lock().unwrap());
        for (i, install, r) in done {
            self.busy[i] = None;
            match r {
                Ok(()) => log!("{} {}", TOOLS[i].name(), if install { "açıldı" } else { "kapatıldı" }),
                Err(e) => {
                    log!("{} {}: {e}", TOOLS[i].name(), if install { "açılamadı" } else { "kapatılamadı" });
                    self.errors[i] = Some(e);
                }
            }
        }
        self.sync_tools();
    }

    /// Araçların tepsi bildirimleri (pil vitesi değişti, ...).
    fn notices(&mut self) {
        let notes: Vec<_> = self.tools.iter_mut().flatten().filter_map(|t| t.take_notice()).collect();
        for (title, text) in notes {
            self.tray.notify(&title, &text);
        }
    }

    // --- Yerleşim ---

    fn narrow(&self, w: f32) -> bool {
        self.narrow_pref || w < AUTO_NARROW
    }

    fn side_w(&self, w: f32) -> f32 {
        if self.narrow(w) { NARROW } else { WIDE }
    }

    fn toggle_rect() -> Rect {
        Rect::new(10.0, HEAD_CY - 18.0, 46.0, HEAD_CY + 18.0)
    }

    fn nav_rect(&self, page: Page, w: f32, h: f32) -> Rect {
        let sw = self.side_w(w);
        // Dar kenar çubuğunda "Araçlar" başlığı yerine ince bir çizgi.
        let section = if self.narrow(w) { 12.0 } else { SECTION_H };
        let top = HEADER + 4.0 + if self.narrow(w) { 0.0 } else { SEARCH_H };
        // Ana sayfanın altında üst araçlar (asistan), sonra "Araçlar" başlığı ve araçlar. Araçlar
        // sığmazsa satırları sıkışır (en az 30 px), alttaki Araç ekle'nin üstüne binmez.
        let tops = self.top_tools();
        let tools_top = top + NAV + 2.0 + tops.len() as f32 * (NAV + 2.0) + section - 2.0;
        let store_top = h - 10.0 - 2.0 * NAV - 2.0;
        let n = self.live_tools().len().max(1) as f32;
        let row = ((store_top - 10.0 - tools_top) / n).clamp(30.0, NAV + 2.0);
        let (t, height) = match page {
            Page::Home => (top, NAV),
            Page::Tool(i) if TOOLS[i].top => {
                let k = tops.iter().position(|&j| j == i).unwrap_or(0);
                (top + (k + 1) as f32 * (NAV + 2.0), NAV)
            }
            Page::Tool(i) => {
                let k = (0..i).filter(|&j| self.installed(j) && !TOOLS[j].top).count();
                (tools_top + k as f32 * row, row - 2.0)
            }
            Page::Store => (store_top, NAV),
            Page::Settings => (h - 10.0 - NAV, NAV),
        };
        Rect::new(6.0, t, sw - 6.0, t + height)
    }

    /// Canlı araç sayfasının başlığında adın bittiği x (sayfanın kendi köşesine göre).
    fn head_x(&self, i: usize) -> f32 {
        PAD + 44.0 + self.gfx.measure(&self.tool_name(i), &self.gfx.f.heading)
    }

    /// Araç sayfasına boyutunu verir; kenar çubuğunun genişliğini döndürür.
    fn layout_tool(&mut self, w: f32, h: f32) -> f32 {
        let sw = self.side_w(w);
        if let Some(i) = self.tool_live() {
            let hx = self.head_x(i);
            let hr = w - sw - CAPTIONS - 12.0;
            if let Some(p) = self.page_mut(i) {
                p.layout(w - sw, h, hx, hr);
            }
        }
        sw
    }

    fn max_scroll(&self) -> f32 {
        let (w, h) = self.size();
        let bottom = match self.page {
            Page::Home => self.live_tools().len().checked_sub(1).map_or(0.0, |k| self.tile(k, w).b + self.scroll),
            _ => self.page_form(w).map_or(0.0, |f| f.bottom() - 24.0 + self.scroll),
        };
        (bottom + 12.0 - h).max(0.0)
    }

    /// winloom'ı kaldır: önce kurulu bütün araçlar (her biri kendi kalıntı denetimiyle), sonra
    /// winloom'ın kendi kayıtları ve kısayolları; klasörü süreç kapanınca silinir.
    fn start_self_remove(&mut self) {
        if self.removing_self || self.busy.iter().any(Option::is_some) {
            return;
        }
        let tools: Vec<usize> = (0..TOOLS.len()).filter(|&i| self.installed(i)).collect();
        let names: Vec<&str> = tools.iter().map(|&i| TOOLS[i].name()).collect();
        let list = names.join(", ");
        let text = wide(&match (ortak::i18n::turkish(), names.is_empty()) {
            (false, true) => "Remove winloom?\n\nNo trace is left on this computer.".to_string(),
            (false, false) => format!(
                "Remove winloom and the tools that are on ({list})?\n\nTheir settings and data are deleted too; no trace is \
                 left on this computer. The soundboard and the tunnel ask for administrator permission."
            ),
            (true, true) => "winloom kaldırılsın mı?\n\nBilgisayarda hiçbir iz kalmaz.".to_string(),
            (true, false) => format!(
                "winloom ve açık araçlar ({list}) kaldırılsın mı?\n\nAraçların ayarları ve verileri de silinir; \
                 bilgisayarda hiçbir iz kalmaz. Soundboard ve Tünel için yönetici izni istenir."
            ),
        });
        let yes = unsafe {
            MessageBoxW(Some(self.hwnd), PCWSTR(text.as_ptr()), w!("winloom"), MB_YESNO | MB_ICONWARNING) == IDYES
        };
        if !yes {
            return;
        }
        log!("winloom kaldırılıyor: {names:?}");
        self.tools.iter_mut().for_each(|t| *t = None);
        self.page = Page::Settings;
        self.removing_self = true;
        self.redraw();
        let (errors, hwnd) = (self.self_errors.clone(), self.hwnd.0 as usize);
        std::thread::spawn(move || {
            for i in tools {
                if let Err(e) = tools::run(i, false) {
                    errors.lock().unwrap().push(format!("{}: {e}", TOOLS[i].name()));
                }
            }
            if errors.lock().unwrap().is_empty() {
                crate::shell::unregister_app();
            }
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_SELF_DONE, WPARAM(0), LPARAM(0));
            }
        });
    }

    fn self_remove_done(&mut self) {
        let errors = std::mem::take(&mut *self.self_errors.lock().unwrap());
        if errors.is_empty() {
            log!("winloom kaldırıldı");
            crate::shell::remove_self_later();
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
            return;
        }
        self.removing_self = false;
        let head = t!("winloom could not be removed; nothing was left half done:", "winloom kaldırılamadı, hiçbir şey yarım bırakılmadı:");
        let text = wide(&format!("{head}\n\n{}", errors.join("\n")));
        unsafe {
            MessageBoxW(Some(self.hwnd), PCWSTR(text.as_ptr()), w!("winloom"), MB_OK | MB_ICONERROR);
        }
        self.sync_tools();
    }

    /// Sürüm satırının düğmesi: güncelleme hazırsa "Güncelle", değilse "Denetle" (bakılırken
    /// basılmaz).
    fn update_label() -> &'static str {
        match crate::update::status() {
            crate::update::Status::Ready(_) => t!("Update", "Güncelle"),
            crate::update::Status::Checking => t!("Checking…", "Bakılıyor…"),
            _ => t!("Check", "Denetle"),
        }
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        let (w, h) = self.size();
        let sw = self.side_w(w);
        if self.search.focused
            && let Some(k) = (0..self.found().len()).find(|&k| self.found_rect(k, w).contains(x, y))
        {
            return Hit::Found(k);
        }
        if self.search_rect(w).is_some_and(|r| r.contains(x, y)) {
            return Hit::Search;
        }
        if x < sw {
            if Self::toggle_rect().contains(x, y) {
                return Hit::Toggle;
            }
            return self.pages().into_iter().find(|&p| self.nav_rect(p, w, h).contains(x, y)).map_or(Hit::None, Hit::Nav);
        }
        if self.tool_live().is_some() {
            return Hit::Content;
        }
        match self.page {
            Page::Home => {
                // Kaydırılıp başlığın altına giren kutucuklar tıklanmaz.
                if y < HEADER + 70.0 {
                    return Hit::None;
                }
                if self.ask_rect(w).is_some_and(|r| r.contains(x, y)) {
                    return Hit::Ask;
                }
                let tools = self.live_tools();
                (0..tools.len()).find(|&k| self.tile(k, w).contains(x, y)).map_or(Hit::None, |k| Hit::Open(tools[k]))
            }
            Page::Store | Page::Settings => {
                let f = self.page_form(w).map_or(form::Hit::None, |f| f.hit(&self.gfx, x, y));
                if f == form::Hit::None { Hit::None } else { Hit::Form(f) }
            }
            Page::Tool(_) => Hit::None,
        }
    }

    fn click(&mut self, hit: Hit) {
        match hit {
            Hit::Toggle => {
                let (w, _) = self.size();
                if w < AUTO_NARROW {
                    // Pencere dar olduğu için ikonda kalıyordu: açmak isteyen için pencereyi genişlet.
                    self.narrow_pref = false;
                    let mut rc = RECT::default();
                    unsafe {
                        let _ = GetWindowRect(self.hwnd, &mut rc);
                        let extra = ((AUTO_NARROW + 120.0 - w) * self.dpi / 96.0) as i32;
                        let size = (rc.right - rc.left + extra, rc.bottom - rc.top);
                        let _ = SetWindowPos(self.hwnd, None, 0, 0, size.0, size.1, SWP_NOMOVE | SWP_NOZORDER);
                    }
                } else {
                    self.narrow_pref = !self.narrow_pref;
                }
                self.save();
                self.redraw();
            }
            Hit::Nav(p) => self.select(p),
            Hit::Search => {
                self.search.focus(false);
                self.redraw();
            }
            Hit::Found(k) => self.run_found(k),
            Hit::Open(i) => self.select(Page::Tool(i)),
            Hit::Ask => {
                self.ask.focus(false);
                self.redraw();
            }
            Hit::Form(f) => self.form_click(f),
            Hit::None | Hit::Content => {}
        }
    }

    // --- Çizim ---

    fn paint_nav(&self, page: Page, w: f32, h: f32) {
        let g = &self.gfx;
        let r = self.nav_rect(page, w, h);
        let selected = self.page == page;
        let hovered = self.hover == Hit::Nav(page);
        // Windows'un gezinti çubuğu gibi: seçili satır hafif dolu, solunda vurgu renginde hap.
        if selected {
            g.fill(r, 4.0, pal().sel);
            g.fill(Rect::new(r.l, r.cy() - 8.0, r.l + 3.0, r.cy() + 8.0), 1.5, accent());
        } else if hovered {
            g.fill(r, 4.0, pal().hover);
        }
        let ic = Rect::new(r.l + 10.0, r.cy() - 12.0, r.l + 34.0, r.cy() + 12.0);
        let fg = if selected || hovered { pal().text } else { pal().muted };
        let glyph = match page {
            Page::Home => ICON_HOME,
            Page::Tool(i) => TOOLS[i].glyph,
            Page::Store => ICON_ADD,
            Page::Settings => ICON_SETTINGS,
        };
        g.text(glyph, &g.f.icon, ic, fg);
        if !self.narrow(w) {
            g.text(&self.label(page), &g.f.text, Rect::new(r.l + 46.0, r.t, r.r - 10.0, r.b), fg);
        }
    }

    fn paint_sidebar(&self, w: f32, h: f32) {
        let g = &self.gfx;
        let sw = self.side_w(w);
        icon_button(g, Self::toggle_rect(), ICON_MENU, pal().muted, self.hover == Hit::Toggle);
        if !self.narrow(w) {
            g.image(self.logo, Rect::new(56.0, HEAD_CY - 11.0, 78.0, HEAD_CY + 11.0), 1.0);
            g.text(NAME, &g.f.strong, Rect::new(88.0, HEAD_CY - 18.0, sw - 12.0, HEAD_CY + 18.0), pal().text);
        }
        for p in self.pages() {
            self.paint_nav(p, w, h);
        }
        // Ana sayfa ile araçlar arasında "Araçlar" başlığı (dar çubukta ince çizgi).
        // Ana sayfa ve üst araçların altında: "Araçlar" başlığı (dar çubukta ince çizgi).
        let above = self.top_tools().last().map_or(Page::Home, |&i| Page::Tool(i));
        let home = self.nav_rect(above, w, h);
        if self.narrow(w) {
            g.fill(Rect::new(14.0, home.b + 6.0, sw - 14.0, home.b + 7.0), 0.0, pal().line);
        } else {
            let label = Rect::new(home.l + 10.0, home.b + 6.0, sw - 12.0, home.b + SECTION_H);
            g.text(t!("Tools", "Araçlar"), &g.f.small, label, pal().faint);
        }
    }

    /// Canlı araç sayfası: başlıkta ikon ve ad, gerisini araç çizer; üstüne gelinen ikon
    /// düğmesinin açıklaması en üstte.
    fn paint_live(&self, i: usize, sw: f32, w: f32) {
        let g = &self.gfx;
        g.translate(sw, 0.0);
        let sq = Rect::new(PAD, HEAD_CY - 16.0, PAD + 32.0, HEAD_CY + 16.0);
        g.fill(sq, 8.0, pal().panel);
        g.stroke(sq, 8.0, pal().line, 1.0);
        g.text(TOOLS[i].glyph, &g.f.icon, sq, accent());
        let name = Rect::new(PAD + 44.0, HEAD_CY - 18.0, PAD + 260.0, HEAD_CY + 18.0);
        g.text(&self.tool_name(i), &g.f.heading, name, pal().text);
        if let Some(p) = self.page_ref(i) {
            p.paint(g);
            if let Some((r, text)) = p.tip() {
                let tw = g.measure(&text, &g.f.small);
                let l = ((r.l + r.r) / 2.0 - tw / 2.0 - 10.0).clamp(8.0, w - sw - tw - 28.0);
                let tip = Rect::new(l, r.b + 6.0, l + tw + 20.0, r.b + 32.0);
                g.fill(tip, 4.0, pal().tip);
                g.stroke(tip, 4.0, pal().line, 1.0);
                g.text(&text, &g.f.small_center, tip, pal().text);
            }
        }
        g.translate(0.0, 0.0);
    }

    // --- Arama ---

    fn search_rect(&self, w: f32) -> Option<Rect> {
        (!self.narrow(w)).then(|| Rect::new(12.0, HEADER + 2.0, self.side_w(w) - 12.0, HEADER + 38.0))
    }

    /// Yazılana uyan sayfalar ve eylemler: (sonuç, yazı, simge).
    fn found(&self) -> Vec<(Found, String, &'static str)> {
        let q = self.search.text.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let hit = |s: &str| s.to_lowercase().contains(&q);
        let mut out = Vec::new();
        for page in self.pages() {
            let extra = match page {
                Page::Tool(i) => format!("{} {} {}", TOOLS[i].id, TOOLS[i].tagline(), TOOLS[i].names.join(" ")),
                Page::Settings => t!("language theme update start with windows quit remove", "dil tema güncelle windows ile başlat kapat kaldır").into(),
                _ => String::new(),
            };
            if hit(&self.label(page)) || hit(&extra) {
                let glyph = match page {
                    Page::Home => ICON_HOME,
                    Page::Tool(i) => TOOLS[i].glyph,
                    Page::Store => ICON_ADD,
                    Page::Settings => ICON_SETTINGS,
                };
                out.push((Found::Go(page), self.label(page), glyph));
            }
        }
        for (i, t) in TOOLS.iter().enumerate() {
            if !self.installed(i) && (hit(t.name()) || hit(t.tagline()) || hit(t.id)) {
                out.push((Found::TurnOn(i), t!(format!("Turn on: {}", t.name()), format!("Aç: {}", t.name())), t.glyph));
            }
        }
        out.truncate(FOUND_MAX - 1);
        // Hiçbiri değilse yazılanı soru olarak alabilecek araç (engram, asistanın adıyla) en sonda.
        if let Some(i) = (0..TOOLS.len()).find(|&i| self.installed(i) && TOOLS[i].id == "engram") {
            let (text, name) = (self.search.text.trim(), self.tool_name(i));
            let label = t!(format!("Ask {name}: {text}"), format!("{}'{} sor: {text}", name, dative(&name)));
            out.push((Found::Ask(i), label, TOOLS[i].glyph));
        }
        out
    }

    fn found_rect(&self, k: usize, w: f32) -> Rect {
        let s = self.search_rect(w).unwrap_or_default();
        let t = s.b + 6.0 + 4.0 + k as f32 * FOUND_H;
        Rect::new(s.l + 4.0, t, s.l + 4.0 + (s.w() - 8.0).max(340.0), t + FOUND_H)
    }

    fn close_search(&mut self) {
        self.search.blur();
        self.search.set("");
        self.pick = 0;
        self.redraw();
    }

    fn run_found(&mut self, k: usize) {
        let Some((f, _, _)) = self.found().into_iter().nth(k) else { return };
        let text = self.search.text.trim().to_string();
        self.close_search();
        match f {
            Found::Go(p) => self.select(p),
            Found::TurnOn(_) => self.select(Page::Store),
            Found::Ask(i) => {
                self.select(Page::Tool(i));
                if let Some(p) = self.page_mut(i) {
                    p.ask(&text);
                }
            }
        }
    }

    /// Arama kutusu ve sonuçları: kenar çubuğunun başında, sonuçlar içeriğin üstüne açılır.
    fn paint_search(&self, w: f32) {
        let Some(r) = self.search_rect(w) else { return };
        let g = &self.gfx;
        let hint = match self.asker() {
            Some(i) => {
                let name = self.tool_name(i);
                t!(format!("Search or ask {name}"), format!("Ara ya da {}'{} sor", name, dative(&name)))
            }
            None => t!("Search", "Ara").to_string(),
        };
        self.search.paint(g, r, &hint);
        if self.search.text.is_empty() {
            let hint = Rect::new(r.l, r.t, r.r - 12.0, r.b);
            g.text("Ctrl+Space", &g.f.small_right, hint, pal().faint);
        }
        let found = self.found();
        if !self.search.focused || found.is_empty() {
            return;
        }
        let first = self.found_rect(0, w);
        let panel = Rect::new(first.l - 4.0, first.t - 4.0, first.r + 4.0, self.found_rect(found.len() - 1, w).b + 4.0);
        g.fill(panel, 8.0, pal().tip);
        g.stroke(panel, 8.0, pal().line, 1.0);
        for (k, (_, label, glyph)) in found.iter().enumerate() {
            let fr = self.found_rect(k, w);
            if k == self.pick || self.hover == Hit::Found(k) {
                g.fill(fr, 6.0, pal().sel);
            }
            g.text(glyph, &g.f.icon_small, Rect::new(fr.l + 10.0, fr.t, fr.l + 32.0, fr.b), accent());
            g.text(label, &g.f.text, Rect::new(fr.l + 40.0, fr.t, fr.r - 10.0, fr.b), pal().text);
        }
    }

    /// Ana sayfadaki soru kutusu (asistan açıksa): selamın altında, bütün genişlikte.
    fn ask_rect(&self, w: f32) -> Option<Rect> {
        self.asker()?;
        let (l, r) = (self.side_w(w) + PAGE_PAD, w - PAGE_PAD);
        let t = HEADER + 76.0 - self.scroll;
        Some(Rect::new(l, t, r, t + 48.0))
    }

    fn tiles_top(&self) -> f32 {
        HEADER + 76.0 + if self.asker().is_some() { 48.0 + 24.0 } else { 0.0 }
    }

    /// Soru kutusunun yazısı asistana gider, sayfası açılır.
    fn send_ask(&mut self) {
        let text = self.ask.text.trim().to_string();
        let Some(i) = self.asker() else { return };
        if text.is_empty() {
            return;
        }
        self.ask.set("");
        self.ask.blur();
        self.select(Page::Tool(i));
        if let Some(p) = self.page_mut(i) {
            p.ask(&text);
        }
    }

    /// Ana sayfanın `k`'inci kutucuğu: içerik genişliğine sığdığı kadar sütun.
    fn tile(&self, k: usize, w: f32) -> Rect {
        let (l, r) = (self.side_w(w) + PAGE_PAD, w - PAGE_PAD);
        let cols = (((r - l + TILE_GAP) / (TILE_W + TILE_GAP)).floor() as usize).max(1);
        let tw = (r - l - TILE_GAP * (cols - 1) as f32) / cols as f32;
        let (col, row) = (k % cols, k / cols);
        let x = l + col as f32 * (tw + TILE_GAP);
        let y = self.tiles_top() + row as f32 * (TILE_H + TILE_GAP) - self.scroll;
        Rect::new(x, y, x + tw, y + TILE_H)
    }

    /// Ana sayfa: selam, tarih ve açık araçların kutucukları.
    fn paint_home(&self, w: f32, h: f32) {
        let g = &self.gfx;
        let x0 = self.side_w(w) + PAGE_PAD;
        let (hour, date) = today();
        let hello = match hour {
            5..=11 => t!("Good morning", "Günaydın"),
            12..=17 => t!("Good afternoon", "İyi günler"),
            _ => t!("Good evening", "İyi akşamlar"),
        };
        g.text(&date, &g.f.small, Rect::new(x0, HEADER + 10.0, w - PAGE_PAD, HEADER + 30.0), pal().muted);
        g.text(hello, &g.f.h1, Rect::new(x0, HEADER + 28.0, w - PAGE_PAD, HEADER + 64.0), pal().text);
        let tools = self.live_tools();
        if tools.is_empty() {
            let r = Rect::new(x0, HEADER + 80.0, w - PAGE_PAD, HEADER + 104.0);
            let text = t!("No tools on yet: turn some on in Add tools.", "Henüz açık araç yok: Araç ekle'den istediğini aç.");
            g.text(text, &g.f.text, r, pal().muted);
            return;
        }
        g.clip(Rect::new(self.side_w(w), HEADER + 70.0, w, h), || {
            if let (Some(r), Some(i)) = (self.ask_rect(w), self.asker()) {
                let name = self.tool_name(i);
                let hint = t!(format!("Ask {name} or tell it something…"), format!("{}'{} sor ya da bir şey söyle…", name, dative(&name)));
                self.ask.paint(g, r, &hint);
                g.text("Enter", &g.f.small_right, Rect::new(r.l, r.t, r.r - 14.0, r.b), pal().faint);
            }
            for (k, &i) in tools.iter().enumerate() {
                let r = self.tile(k, w);
                let hovered = self.hover == Hit::Open(i);
                g.fill(r, 10.0, if hovered { pal().sel } else { pal().panel });
                g.stroke(r, 10.0, pal().line, 1.0);
                // Üstte simge ve ad, altında aracın şu anki durumu (yoksa açıklaması).
                let ic = Rect::new(r.l + 14.0, r.t + 12.0, r.l + 36.0, r.t + 36.0);
                g.text(TOOLS[i].glyph, &g.f.icon_small, ic, accent());
                g.text(&self.tool_name(i), &g.f.small, Rect::new(r.l + 42.0, r.t + 12.0, r.r - 14.0, r.t + 36.0), pal().muted);
                let (big, sub) = self.page_ref(i).and_then(|p| p.summary()).unwrap_or_else(|| (self.tool_name(i), TOOLS[i].tagline().into()));
                g.text(&big, &g.f.heading, Rect::new(r.l + 14.0, r.t + 42.0, r.r - 14.0, r.t + 72.0), pal().text);
                g.text(&sub, &g.f.small, Rect::new(r.l + 14.0, r.t + 72.0, r.r - 14.0, r.b - 10.0), pal().muted);
            }
        });
    }

    /// Araç ekle ve Ayarlar: sayfa kitiyle.
    fn page_form(&self, w: f32) -> Option<Form> {
        let (l, r, top) = (self.side_w(w) + PAGE_PAD, w - PAGE_PAD, HEADER + 16.0 - self.scroll);
        let items = match self.page {
            Page::Store => self.store_items(),
            Page::Settings => self.settings_items(),
            _ => return None,
        };
        Some(Form::new(items, l, r, top))
    }

    /// Araç ekle: her araç bir satır, sağında aç / kapa.
    fn store_items(&self) -> Vec<Item> {
        let mut items = vec![Item::Note(
            t!(
                "All of them come with winloom: turn on what you want. Turned off, a tool leaves no trace.",
                "Hepsi winloom ile gelir: istediğini aç. Kapattığın araç bilgisayarda iz bırakmaz."
            )
            .into(),
            false,
        )];
        items.push(Item::Section(String::new()));
        for (i, tool) in TOOLS.iter().enumerate() {
            let ctl = match self.busy[i] {
                Some(install) => Ctl::Text(tools::busy_label(i, install).into()),
                None => Ctl::Toggle(self.installed(i)),
            };
            let sub = self.errors[i].clone().unwrap_or_else(|| tool.tagline().into());
            items.push(Item::Row(Row::new(i as u32, tool.name(), sub, ctl).icon(tool.glyph)));
        }
        items
    }

    fn settings_items(&self) -> Vec<Item> {
        use crate::update::Status;
        let dev = util::installed_copy().is_none();
        let status = crate::update::status();
        let autostart_sub = match dev {
            true => t!("Not available in a development copy", "Geliştirme kopyasında kullanılamaz"),
            false => t!(
                "Starts quietly in the tray when you sign in; your wallpaper comes with it",
                "Oturum açınca tepside sessizce başlar; duvar kâğıdın da onunla gelir"
            ),
        };
        let version_sub = match &status {
            _ if dev => t!("A development copy is not updated", "Geliştirme kopyası güncellenmez").to_string(),
            Status::Checking => t!("Looking for a new version…", "Yeni sürüm var mı bakılıyor…").to_string(),
            Status::Current => t!("This is the latest version", "En son sürüm bu").to_string(),
            Status::Ready(v) => t!(
                format!("{v} is ready · installs now or the next time winloom starts"),
                format!("{v} hazır · şimdi ya da winloom bir sonraki açılışında kurulur")
            ),
            Status::Failed(e) => t!(format!("Could not check: {e}"), format!("Bakılamadı: {e}")),
            Status::Idle if self.auto_update => t!("New versions are looked for now and then", "Yeni sürüme arada bir bakılır").to_string(),
            Status::Idle => t!("Automatic updates are off", "Otomatik güncelleme kapalı").to_string(),
        };
        let update = match () {
            _ if dev => Ctl::None,
            _ if matches!(status, Status::Ready(_)) => Ctl::Primary(Self::update_label().into()),
            _ => Ctl::Button(Self::update_label().into()),
        };
        let remove = if self.removing_self {
            Row::new(
                S_REMOVE,
                t!("Remove winloom", "winloom'ı kaldır"),
                t!("Removing… each tool is removed and checked in turn", "Kaldırılıyor… araçlar sırayla kaldırılıp denetleniyor"),
                Ctl::None,
            )
        } else {
            Row::new(
                S_REMOVE,
                t!("Remove winloom", "winloom'ı kaldır"),
                t!("Together with the tools that are on; no trace is left on this computer", "Açık araçlarla birlikte; bilgisayarda hiçbir iz kalmaz"),
                Ctl::Button(t!("Remove", "Kaldır").into()),
            )
        };
        vec![
            Item::Section(t!("General", "Genel").into()),
            Item::Row(Row::new(S_AUTOSTART, t!("Start with Windows", "Windows ile başlat"), autostart_sub, Ctl::Toggle(self.autostart)).dim(dev)),
            Item::Row(Row::new(
                S_LANG,
                t!("Language", "Dil"),
                "",
                Ctl::Seg(vec!["English".into(), "Türkçe".into()], ortak::i18n::turkish() as usize),
            )),
            Item::Section(t!("Updates", "Güncelleme").into()),
            Item::Row(Row::new(S_VERSION, format!("{} {VERSION}", t!("Version", "Sürüm")), version_sub, update)),
            Item::Row(
                Row::new(
                    S_AUTO_UPDATE,
                    t!("Automatic updates", "Otomatik güncelle"),
                    t!("Downloads new versions from GitHub, verifies and installs them", "Yeni sürümleri GitHub'dan indirir, doğrular ve kurar"),
                    Ctl::Toggle(self.auto_update),
                )
                .dim(dev),
            ),
            Item::Section("winloom".into()),
            Item::Row(Row::new(
                S_QUIT,
                t!("Quit", "Kapat"),
                t!("winloom and the tools' engines shut down completely", "winloom ve araçların motorları tamamen kapanır"),
                Ctl::Button(t!("Quit", "Çık").into()),
            )),
            Item::Row(remove),
        ]
    }

    /// Araç ekle ve Ayarlar'da tıklanan.
    fn form_click(&mut self, f: form::Hit) {
        match (self.page, f) {
            (Page::Store, form::Hit::Toggle(i)) if self.busy[i as usize].is_none() => {
                self.start_setup(i as usize, !self.installed(i as usize));
            }
            (Page::Store, form::Hit::Row(i)) if self.installed(i as usize) => self.select(Page::Tool(i as usize)),
            (Page::Settings, form::Hit::Toggle(S_AUTOSTART)) if util::installed_copy().is_some() => {
                set_autostart(!self.autostart);
                self.autostart = autostart();
                self.redraw();
            }
            (Page::Settings, form::Hit::Seg(S_LANG, k)) => {
                ortak::i18n::set_turkish(k == 1);
                self.save();
                // Başlat menüsü açıklamaları da seçili dilde olsun.
                let installed: Vec<bool> = (0..TOOLS.len()).map(|i| self.installed(i)).collect();
                crate::shell::sync_shortcuts(&installed);
                self.redraw();
            }
            (Page::Settings, form::Hit::Button(S_VERSION)) if crate::update::status() != crate::update::Status::Checking => {
                if matches!(crate::update::status(), crate::update::Status::Ready(_)) {
                    crate::update::install(false);
                } else {
                    crate::update::check_now(self.hwnd);
                }
            }
            (Page::Settings, form::Hit::Toggle(S_AUTO_UPDATE)) if util::installed_copy().is_some() => {
                self.auto_update = !self.auto_update;
                crate::update::set_enabled(self.hwnd, self.auto_update);
                self.save();
                self.redraw();
            }
            (Page::Settings, form::Hit::Button(S_QUIT)) => unsafe {
                let _ = DestroyWindow(self.hwnd);
            },
            (Page::Settings, form::Hit::Button(S_REMOVE)) if !self.removing_self => self.start_self_remove(),
            _ => {}
        }
    }

    /// Araç ekle ya da Ayarlar: başlıkta adı, altında sayfa kiti.
    fn paint_form(&self, w: f32, h: f32) {
        let Some(f) = self.page_form(w) else { return };
        let g = &self.gfx;
        let x0 = self.side_w(w) + PAGE_PAD;
        let title = if self.page == Page::Store { t!("Tools", "Araçlar") } else { t!("Settings", "Ayarlar") };
        g.text(title, &g.f.heading, Rect::new(x0, HEAD_CY - 18.0, w - CAPTIONS - 12.0, HEAD_CY + 18.0), pal().text);
        let hover = if let Hit::Form(fh) = self.hover { fh } else { form::Hit::None };
        g.clip(Rect::new(self.side_w(w), HEADER, w, h), || f.paint(g, hover, None));
    }

    fn paint(&mut self) {
        let (w, h) = self.size();
        let sw = self.layout_tool(w, h);
        // Mica varsa zemin Windows'un camı (saydam çizilir); yoksa temanın düz rengi.
        if !self.gfx.begin(self.hwnd, self.dpi, pal().bg) {
            unsafe {
                let _ = ValidateRect(Some(self.hwnd), None);
            }
            return;
        }
        let g = &self.gfx;
        // İçerik alanı zeminin üstünde ayrı bir katman: sol üst köşesi yuvarlak (Windows Ayarlar
        // uygulaması ve Claude gibi).
        let layer = Rect::new(sw, HEADER, w + 12.0, h + 12.0);
        g.fill(layer, 8.0, pal().layer);
        g.stroke(layer, 8.0, pal().layer_line, 1.0);
        self.paint_sidebar(w, h);
        match (self.page, self.tool_live()) {
            (_, Some(i)) => g.clip(Rect::new(sw, 0.0, w, h), || self.paint_live(i, sw, w)),
            (Page::Home, _) => self.paint_home(w, h),
            _ => self.paint_form(w, h),
        }

        // Dar kenar çubuğunda üstüne gelinen öğenin adı.
        if self.narrow(w)
            && let Hit::Nav(p) = self.hover
        {
            let r = self.nav_rect(p, w, h);
            let label = self.label(p);
            let tw = g.measure(&label, &g.f.text);
            let tip = Rect::new(sw + 6.0, r.cy() - 14.0, sw + 6.0 + tw + 20.0, r.cy() + 14.0);
            g.fill(tip, 4.0, pal().tip);
            g.stroke(tip, 4.0, pal().line, 1.0);
            g.text(&label, &g.f.text, Rect::new(tip.l + 10.0, tip.t, tip.r, tip.b), pal().text);
        }
        self.paint_search(w);
        self.paint_captions(w);

        let lost = self.gfx.end();
        unsafe {
            let _ = ValidateRect(Some(self.hwnd), None);
        }
        if lost {
            self.redraw();
        }
    }

    // --- Pencere düğmeleri ---

    fn cap_rect(c: Cap, w: f32) -> Rect {
        let i = Cap::ALL.iter().position(|&x| x == c).unwrap_or(0) as f32;
        let l = w - CAPTIONS + i * CAP_W;
        Rect::new(l, 0.0, l + CAP_W, CAP_H)
    }

    fn cap_at(&self, x: f32, y: f32) -> Option<Cap> {
        let (w, _) = self.size();
        Cap::ALL.into_iter().find(|&c| Self::cap_rect(c, w).contains(x, y))
    }

    fn paint_captions(&self, w: f32) {
        let g = &self.gfx;
        for c in Cap::ALL {
            let r = Self::cap_rect(c, w);
            let hovered = self.cap_hover == Some(c);
            let pressed = hovered && self.cap_pressed == Some(c);
            let fg = match (c, hovered) {
                (Cap::Close, true) => {
                    g.fill(r, 0.0, if pressed { CLOSE_RED.alpha(0.8) } else { CLOSE_RED });
                    Color::rgb(0xffffff)
                }
                (_, true) => {
                    g.fill(r, 0.0, if pressed { pal().sel } else { pal().hover });
                    pal().text
                }
                _ if self.active => pal().muted,
                _ => pal().faint,
            };
            let glyph = match c {
                Cap::Min => "\u{E921}",
                Cap::Max if self.zoomed() => "\u{E923}",
                Cap::Max => "\u{E922}",
                Cap::Close => "\u{E8BB}",
            };
            g.text(glyph, &g.f.icon_caption, r, fg);
        }
    }

    fn cap_click(&mut self, c: Cap) {
        unsafe {
            match c {
                Cap::Min => {
                    let _ = ShowWindow(self.hwnd, SW_MINIMIZE);
                }
                Cap::Max => {
                    let _ = ShowWindow(self.hwnd, if self.zoomed() { SW_RESTORE } else { SW_MAXIMIZE });
                }
                Cap::Close => self.hide(),
            }
        }
    }

    /// Başlık çubuğu yok: pencere düğmeleri, üst kenardan boyutlandırma ve boş yerden sürükleme.
    fn nc_hit(&self, x: f32, y: f32) -> u32 {
        let (w, _) = self.size();
        if !self.zoomed() && y < 5.0 {
            return if x < 12.0 {
                HTTOPLEFT
            } else if x > w - 12.0 {
                HTTOPRIGHT
            } else {
                HTTOP
            };
        }
        if let Some(c) = self.cap_at(x, y) {
            return c.ht();
        }
        if y >= HEADER {
            return HTCLIENT;
        }
        let sw = self.side_w(w);
        if x < sw {
            return if Self::toggle_rect().contains(x, y) { HTCLIENT } else { HTCAPTION };
        }
        let busy = match self.tool_live() {
            Some(i) => self.page_ref(i).is_some_and(|p| p.interactive(&self.gfx, x - sw, y)),
            None => self.hit(x, y) != Hit::None,
        };
        if busy { HTCLIENT } else { HTCAPTION }
    }

    // --- Pencere ---

    fn show(&mut self) {
        self.autostart = autostart();
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            } else {
                let _ = ShowWindow(self.hwnd, SW_SHOW);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
        self.sync_visible();
        self.redraw();
    }

    fn hide(&mut self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.sync_visible();
    }

    fn toggle_window(&mut self) {
        let front = self.visible()
            && (unsafe { GetForegroundWindow() } == self.hwnd
                || self.deactivated.is_some_and(|t| t.elapsed().as_millis() < 400));
        if front { self.hide() } else { self.show() }
    }

    /// İkinci kopyadan ya da komut satırından: sekmeyi aç; "kaldir" ise (Windows'un Uygulamalar
    /// listesinden) winloom'ı kaldırmayı başlat.
    fn open_tab(&mut self, tab: &str) {
        if tab.trim() == "kaldir" {
            self.select(Page::Settings);
            self.show();
            self.start_self_remove();
            return;
        }
        if let Some(p) = Page::from_id(tab) {
            self.select(p);
        }
        self.show();
    }

    // --- Mesajlar ---

    fn on(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        let dip = |v: i32| v as f32 * 96.0 / self.dpi;
        let (mx, my) = (dip((lp.0 & 0xFFFF) as i16 as i32), dip(((lp.0 >> 16) & 0xFFFF) as i16 as i32));
        let live = self.tool_live();
        match msg {
            WM_PAINT => self.paint(),
            WM_NCHITTEST => {
                let def = unsafe { DefWindowProcW(self.hwnd, msg, wp, lp) };
                if def.0 as u32 != HTCLIENT {
                    return Some(def);
                }
                let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                }
                return Some(LRESULT(self.nc_hit(dip(pt.x), dip(pt.y)) as isize));
            }
            WM_NCMOUSEMOVE => {
                let c = Cap::from_ht(wp.0 as u32);
                if c != self.cap_hover || self.hover != Hit::None {
                    self.cap_hover = c;
                    self.hover = Hit::None;
                    self.redraw();
                }
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE | TME_NONCLIENT,
                    hwndTrack: self.hwnd,
                    dwHoverTime: 0,
                };
                unsafe {
                    let _ = TrackMouseEvent(&mut tme);
                }
                return None;
            }
            WM_NCMOUSELEAVE => {
                self.cap_hover = None;
                self.cap_pressed = None;
                self.redraw();
            }
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if Cap::from_ht(wp.0 as u32).is_some() => {
                // Windows'un eski tip düğmeleri çizmesin diye varsayılana bırakılmaz.
                self.cap_pressed = Cap::from_ht(wp.0 as u32);
                self.redraw();
            }
            WM_NCLBUTTONUP if Cap::from_ht(wp.0 as u32).is_some() => {
                let c = Cap::from_ht(wp.0 as u32);
                if let Some(c) = c.filter(|&c| self.cap_pressed.take() == Some(c)) {
                    self.cap_click(c);
                }
                self.redraw();
            }
            WM_SIZE => {
                self.hover = Hit::None;
                self.scroll = self.scroll.min(self.max_scroll());
                self.sync_visible();
                self.redraw();
            }
            WM_DPICHANGED => {
                self.dpi = (wp.0 & 0xFFFF) as f32;
                let r = unsafe { &*(lp.0 as *const RECT) };
                unsafe {
                    let (cx, cy) = (r.right - r.left, r.bottom - r.top);
                    let _ = SetWindowPos(self.hwnd, None, r.left, r.top, cx, cy, SWP_NOZORDER | SWP_NOACTIVATE);
                }
            }
            WM_GETMINMAXINFO => {
                let info = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
                info.ptMinTrackSize =
                    POINT { x: (480.0 * self.dpi / 96.0) as i32, y: (440.0 * self.dpi / 96.0) as i32 };
            }
            WM_MOUSEMOVE => {
                let (w, h) = self.size();
                let sw = self.layout_tool(w, h);
                let hit = self.hit(mx, my);
                if hit != self.hover {
                    if self.hover == Hit::Content
                        && let Some(p) = live.and_then(|i| self.tools[i].as_deref_mut())
                    {
                        p.mouse_leave();
                    }
                    self.hover = hit;
                    self.redraw();
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut tme);
                    }
                }
                // Kaydırıcı sürüklenirken imleç kenar çubuğuna kaysa da sayfa izlemeli.
                if (hit == Hit::Content || self.pressed == Hit::Content)
                    && let Some(p) = live.and_then(|i| self.tools[i].as_deref_mut())
                {
                    p.mouse_move(&self.gfx, mx - sw, my);
                }
            }
            WM_MOUSELEAVE => {
                if let Some(p) = live.and_then(|i| self.tools[i].as_deref_mut()) {
                    p.mouse_leave();
                }
                self.hover = Hit::None;
                self.redraw();
            }
            WM_LBUTTONDOWN => {
                let (w, h) = self.size();
                let sw = self.layout_tool(w, h);
                self.pressed = self.hit(mx, my);
                if self.search.focused && !matches!(self.pressed, Hit::Search | Hit::Found(_)) {
                    self.close_search();
                }
                if self.ask.focused && self.pressed != Hit::Ask {
                    self.ask.blur();
                    self.redraw();
                }
                unsafe { SetCapture(self.hwnd) };
                if self.pressed == Hit::Content
                    && let Some(p) = live.and_then(|i| self.tools[i].as_deref_mut())
                {
                    p.mouse_down(&self.gfx, mx - sw, my);
                }
            }
            WM_LBUTTONUP => {
                unsafe {
                    let _ = ReleaseCapture();
                }
                let (w, h) = self.size();
                let sw = self.layout_tool(w, h);
                let pressed = std::mem::replace(&mut self.pressed, Hit::None);
                if pressed == Hit::Content {
                    if let Some(p) = live.and_then(|i| self.tools[i].as_deref_mut()) {
                        p.mouse_up(&self.gfx, mx - sw, my);
                    }
                } else if self.hit(mx, my) == pressed {
                    self.click(pressed);
                }
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
                let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                }
                let (x, y) = (dip(pt.x), dip(pt.y));
                let (w, h) = self.size();
                let sw = self.layout_tool(w, h);
                if x >= sw
                    && let Some(p) = live.and_then(|i| self.tools[i].as_deref_mut())
                {
                    p.wheel(&self.gfx, x - sw, y, delta);
                } else if x >= sw && live.is_none() && matches!(self.page, Page::Store | Page::Home) {
                    self.scroll = (self.scroll - delta * 60.0).clamp(0.0, self.max_scroll());
                    self.hover = self.hit(x, y);
                    self.redraw();
                }
            }
            WM_CHAR => {
                let c = char::from_u32(wp.0 as u32)?;
                if self.search.focused {
                    // Ctrl+Space'in boşluğu kutuya yazılmaz.
                    let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                    if !ctrl && self.search.char(c) {
                        self.pick = 0;
                        self.redraw();
                    }
                    return Some(LRESULT(0));
                }
                if self.ask.focused {
                    if self.ask.char(c) {
                        self.redraw();
                    }
                    return Some(LRESULT(0));
                }
                let p = live.and_then(|i| self.tools[i].as_deref_mut())?;
                if !p.char(c) {
                    return None;
                }
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let vk = VIRTUAL_KEY(wp.0 as u16);
                let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                // Ctrl+Space: arama kutusu (dar kenar çubuğunda önce genişler).
                if ctrl && vk == VK_SPACE {
                    self.narrow_pref = false;
                    // Pencere çok darsa kenar çubuğu yine dar kalır: kutu yok.
                    if self.search_rect(self.size().0).is_none() {
                        return Some(LRESULT(0));
                    }
                    self.search.focus(true);
                    self.redraw();
                    return Some(LRESULT(0));
                }
                if self.search.focused {
                    let n = self.found().len();
                    match vk {
                        VK_DOWN if n > 0 => self.pick = (self.pick + 1) % n,
                        VK_UP if n > 0 => self.pick = (self.pick + n - 1) % n,
                        _ => match self.search.key(vk.0, ctrl) {
                            FieldKey::Enter => self.run_found(self.pick),
                            FieldKey::Escape => self.close_search(),
                            _ => self.pick = 0,
                        },
                    }
                    self.redraw();
                    return Some(LRESULT(0));
                }
                if self.ask.focused {
                    match self.ask.key(vk.0, ctrl) {
                        FieldKey::Enter => self.send_ask(),
                        FieldKey::Escape => self.ask.blur(),
                        _ => {}
                    }
                    self.redraw();
                    return Some(LRESULT(0));
                }
                if let Some(p) = live.and_then(|i| self.tools[i].as_deref_mut())
                    && p.key(vk.0)
                {
                    return Some(LRESULT(0));
                }
                // Ctrl+Tab ve Ctrl+1..9: kenar çubuğundaki sırayla sayfalar.
                let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                let pages = self.pages();
                let n = pages.len();
                let i = pages.iter().position(|&p| p == self.page).unwrap_or(0);
                match vk {
                    VK_TAB if ctrl => self.select(pages[if shift { (i + n - 1) % n } else { (i + 1) % n }]),
                    _ if ctrl && (0x31..0x31 + n as u16).contains(&vk.0) => self.select(pages[(vk.0 - 0x31) as usize]),
                    _ => return None,
                }
            }
            // Kısayol atarken Alt ile gelen menü ve bip sesi olmasın.
            WM_SYSCHAR | WM_SYSKEYUP if self.tools.iter().flatten().any(|t| t.binding()) => {}
            WM_KILLFOCUS => self.tools.iter_mut().flatten().for_each(|t| t.kill_focus()),
            WM_TOOL_SETUP => self.setup_done(),
            crate::update::WM_UPDATE => {
                // Güncelleme indirildi: bir kez tepsiden haber verilir.
                if let crate::update::Status::Ready(v) = crate::update::status()
                    && !self.update_told
                {
                    self.update_told = true;
                    let text = t!(
                        format!("winloom {v} is ready: it installs the next time winloom starts, or now from Settings."),
                        format!("winloom {v} hazır: bir sonraki açılışta kurulur, istersen Ayarlar'dan hemen."),
                    );
                    self.tray.notify(t!("Update ready", "Güncelleme hazır"), &text);
                }
                self.redraw();
            }
            WM_SELF_DONE => self.self_remove_done(),
            WM_EXIT => unsafe {
                let _ = DestroyWindow(self.hwnd);
            },
            WM_DROPFILES => {
                let drop = HDROP(wp.0 as *mut _);
                let mut paths = Vec::new();
                unsafe {
                    for i in 0..DragQueryFileW(drop, u32::MAX, None) {
                        let mut buf = [0u16; 1024];
                        let n = DragQueryFileW(drop, i, Some(&mut buf));
                        paths.push(PathBuf::from(String::from_utf16_lossy(&buf[..n as usize])));
                    }
                    DragFinish(drop);
                }
                if let Some(p) = live.and_then(|i| self.tools[i].as_deref_mut()) {
                    p.add_files(paths);
                }
            }
            WM_ACTIVATE => {
                self.active = (wp.0 & 0xFFFF) as u32 != WA_INACTIVE;
                if self.active {
                    self.autostart = autostart();
                } else {
                    self.deactivated = Some(Instant::now());
                }
                self.redraw();
                return None;
            }
            WM_COPYDATA => {
                let cds = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
                if cds.dwData != COPY_TAB {
                    return None;
                }
                let bytes = unsafe { std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize) };
                self.open_tab(&String::from_utf8_lossy(bytes));
                return Some(LRESULT(1));
            }
            WM_TIMER if wp.0 == TIMER_HOME => self.redraw(),
            WM_PENDING_TAB => {
                if let Some(tab) = ortak::hub::take_tab() {
                    self.open_tab(&tab);
                }
            }
            WM_TRAY => match (lp.0 as u32) & 0xFFFF {
                WM_LBUTTONUP | WM_RBUTTONUP => self.toggle_window(),
                _ => {}
            },
            WM_CLOSE => self.hide(),
            m if m != 0 && m == self.taskbar_created => self.tray.add(),
            // Araçların kendi mesajları, zamanlayıcıları, kısayolları.
            _ => {
                let gfx = &mut self.gfx;
                let r = self.tools.iter_mut().flatten().find_map(|t| t.message(gfx, msg, wp, lp));
                self.notices();
                return if msg == WM_POWERBROADCAST { Some(LRESULT(1)) } else { r };
            }
        }
        Some(LRESULT(0))
    }
}

fn with_app<T>(f: impl FnOnce(&mut App) -> T) -> Option<T> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

/// Pencerenin zemini: temanın düz rengi; gölge ve köşeler DWM'de kalır (başlık çubuğu
/// WM_NCCALCSIZE'da kaldırılıyor).
fn apply_backdrop(hwnd: HWND) {
    unsafe {
        // COLORREF 0x00BBGGRR.
        let c = pal().bg.0;
        let caption = COLORREF((c & 0xff) << 16 | (c & 0xff00) | (c >> 16) & 0xff);
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(35), &caption as *const _ as _, 4);
        let margins = MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 1, cyBottomHeight: 0 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCALCSIZE && wp.0 != 0 && CUSTOM_FRAME.load(Ordering::Relaxed) {
        return nc_calc_size(hwnd, msg, wp, lp);
    }
    if msg == WM_DESTROY {
        APP.with(|a| drop(a.borrow_mut().take()));
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    let handled = with_app(|app| {
        let r = app.on(msg, wp, lp);
        let dialog = app.tools.iter_mut().enumerate().find_map(|(i, t)| Some((i, t.as_mut()?.take_file_dialog()?)));
        (r, dialog)
    });
    match handled {
        Some((result, dialog)) => {
            // Dosya pencereleri kendi mesaj döngüsünü açar: uygulama borcu dışında çalışmalı.
            if let Some((i, open)) = dialog {
                match open(hwnd) {
                    Ok(paths) => {
                        with_app(|app| app.page_mut(i).map(|p| p.add_files(paths)));
                    }
                    Err(e) => log!("dosya penceresi: {e}"),
                }
            }
            result.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
        }
        None if msg == WM_COPYDATA => {
            // Uygulama o an meşgul (örneğin duvar kâğıdı motoruna komut gönderirken gelen istek): veriyi
            // sakla, kendi kuyruğumuzdan sonra uygula.
            let cds = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
            if cds.dwData != COPY_TAB {
                return LRESULT(0);
            }
            let bytes = unsafe { std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize) };
            ortak::hub::set_tab(String::from_utf8_lossy(bytes).into_owned());
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_PENDING_TAB, WPARAM(0), LPARAM(0));
            }
            LRESULT(1)
        }
        None => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

pub fn run(hidden: bool, tab: Option<String>) -> Res<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let instance = GetModuleHandleW(None)?;
        let icon = LoadIconW(Some(instance.into()), PCWSTR(1 as *const u16)).unwrap_or_default();
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hIcon: icon,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: CreateSolidBrush(COLORREF(0x141414)),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("winloom"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let dpi = GetDpiForWindow(hwnd).max(96) as f32;
        let size = ((880.0 * dpi / 96.0) as i32, (580.0 * dpi / 96.0) as i32);
        let _ = SetWindowPos(hwnd, None, 0, 0, size.0, size.1, SWP_NOMOVE | SWP_NOZORDER);
        let dark = BOOL(1);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        let cfg = Config::load();
        apply_backdrop(hwnd);
        DragAcceptFiles(hwnd, true);

        let mut gfx = Gfx::new()?;
        let logo = gfx.load_image(&[
            (24, include_bytes!("../ikon/icon-24.png")),
            (32, include_bytes!("../ikon/icon-32.png")),
            (48, include_bytes!("../ikon/icon-48.png")),
            (64, include_bytes!("../ikon/icon-64.png")),
        ])?;
        let mut app = App {
            hwnd,
            gfx,
            tray: Tray::new(hwnd, WM_TRAY),
            logo,
            page: Page::Store,
            narrow_pref: cfg.narrow,
            scroll: 0.0,
            search: TextField::new(80),
            ask: TextField::new(400),
            pick: 0,
            autostart: autostart(),
            tools: (0..tools::COUNT).map(|_| None).collect(),
            busy: [None; tools::COUNT],
            errors: Default::default(),
            results: Arc::default(),
            removing_self: false,
            self_errors: Arc::default(),
            hover: Hit::None,
            pressed: Hit::None,
            dpi,
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            deactivated: None,
            active: true,
            cap_hover: None,
            cap_pressed: None,
            auto_update: cfg.auto_update,
            update_told: false,
        };
        crate::update::start(hwnd, cfg.auto_update);
        app.sync_tools();
        crate::shell::register_app();
        app.autostart = autostart();
        // İstenen ya da son açık sekme; kurulu araç yoksa Araçlar.
        let page = tab.as_deref().and_then(Page::from_id).or_else(|| Page::from_id(&cfg.tab)).unwrap_or(Page::Home);
        app.page = app.valid(page);
        log!("başladı: sekme {}, kurulu {:?}", app.page.id(), (0..tools::COUNT).map(|i| app.installed(i)).collect::<Vec<_>>());
        APP.with(|a| *a.borrow_mut() = Some(app));
        // Çerçeveyi yeniden hesaplat: artık WM_NCCALCSIZE'ı biz karşılıyoruz.
        CUSTOM_FRAME.store(true, Ordering::Relaxed);
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER);
        if !hidden {
            with_app(|app| app.show());
        }
        if tab.as_deref() == Some("kaldir") {
            with_app(|app| app.open_tab("kaldir"));
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

/// Yerel saat (0..23) ve bugünün tarihi: "Pazartesi, 13 Ekim".
fn today() -> (u16, String) {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let (days, months): ([&str; 7], [&str; 12]) = if ortak::i18n::turkish() {
        (
            ["Pazar", "Pazartesi", "Salı", "Çarşamba", "Perşembe", "Cuma", "Cumartesi"],
            ["Ocak", "Şubat", "Mart", "Nisan", "Mayıs", "Haziran", "Temmuz", "Ağustos", "Eylül", "Ekim", "Kasım", "Aralık"],
        )
    } else {
        (
            ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"],
            ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"],
        )
    };
    let day = days[(t.wDayOfWeek as usize).min(6)];
    let month = months[(t.wMonth as usize).clamp(1, 12) - 1];
    (t.wHour, format!("{day}, {} {month}", t.wDay))
}

/// Türkçe yönelme ekinin kesme işaretinden sonrası: "Matrix'e", "engram'a", "Ayşe'ye".
fn dative(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    let last = lower.chars().rev().find(|c| "aeıioöuü".contains(*c));
    let back = matches!(last, Some('a' | 'ı' | 'o' | 'u'));
    let ends_vowel = lower.chars().last().is_some_and(|c| "aeıioöuü".contains(c));
    match (ends_vowel, back) {
        (true, true) => "ya",
        (true, false) => "ye",
        (false, true) => "a",
        (false, false) => "e",
    }
}
