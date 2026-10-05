//! Pencere: solda daraltılabilir kenar çubuğu (kurulu araçlar, Araçlar, Ayarlar), sağda seçili
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
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

use crate::cheshire::{self, Cheshire};
use crate::config::Config;
use crate::gfx::{Color, Gfx, ImageId, Rect};
use crate::log;
use crate::lyrebird::{self, Lyrebird};
use crate::rabbithole::{self, Rabbithole};
use crate::tools::{self, TOOLS};
use crate::tray::Tray;
use crate::ui::*;
use crate::util::{self, Res, wide};

pub const CLASS: PCWSTR = w!("hive");
pub const NAME: &str = "hive";
const VERSION: &str = env!("CARGO_PKG_VERSION");

const WM_TRAY: u32 = WM_APP + 1;
/// Bir aracın kurulumu ya da kaldırılması bitti (sonuçlar `results` kuyruğunda).
const WM_TOOL_SETUP: u32 = WM_APP + 2;
/// Uygulama meşgulken gelen sekme isteği sonradan uygulanır.
const WM_PENDING_TAB: u32 = WM_APP + 3;
/// hive'ın kendini kaldırması bitti (hatalar `self_errors`'ta).
const WM_SELF_DONE: u32 = WM_APP + 4;
/// Yeni sürüm kuruluyor: çalışan kopya tamamen kapansın (WM_CLOSE yalnızca gizler).
pub const WM_EXIT: u32 = WM_APP + 5;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_NCMOUSELEAVE: u32 = 0x02A2;
/// WM_COPYDATA türü: ikinci kopyadan gelen "şu sekmeyi aç" isteği (boşsa yalnızca öne gel).
const COPY_TAB: usize = 0x6d74;

/// Kenar çubuğu: etiketli ve yalnızca ikon genişliği. Pencere AUTO_NARROW'dan darsa hep ikon.
const WIDE: f32 = 208.0;
const NARROW: f32 = 56.0;
const AUTO_NARROW: f32 = 640.0;
const NAV: f32 = 40.0;
/// Araçlar ve Ayarlar sayfalarının kenar boşluğu: araç sayfalarıyla aynı.
const PAGE_PAD: f32 = PAD;
/// Araçlar sayfasındaki kartlar.
const CARD_H: f32 = 108.0;
const PANEL: Color = Color::rgb(0x141416);

/// Pencere düğmeleri (küçült, büyüt, kapat).
const CAP_W: f32 = 46.0;
const CAP_H: f32 = HEADER;
const CAPTIONS: f32 = CAP_W * 3.0;
const CLOSE_RED: Color = Color::rgb(0xc42b1c);

const ICON_APPS: &str = "\u{E71D}";
const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");

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
    Tool(usize),
    Store,
    Settings,
}

impl Page {
    fn id(self) -> &'static str {
        match self {
            Page::Tool(i) => TOOLS[i].id,
            Page::Store => "tools",
            Page::Settings => "settings",
        }
    }

    fn from_id(s: &str) -> Option<Page> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("tools") || s.eq_ignore_ascii_case("araclar") {
            return Some(Page::Store);
        }
        if s.eq_ignore_ascii_case("settings") || s.eq_ignore_ascii_case("ayarlar") {
            return Some(Page::Settings);
        }
        TOOLS.iter().position(|t| t.id.eq_ignore_ascii_case(s)).map(Page::Tool)
    }

    fn label(self) -> &'static str {
        match self {
            Page::Tool(i) => TOOLS[i].name,
            Page::Store => t!("Tools", "Araçlar"),
            Page::Settings => t!("Settings", "Ayarlar"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Toggle,
    Nav(Page),
    /// Aracın kendi sayfası (araç kendisi işler).
    Content,
    Install(usize),
    Remove(usize),
    Open(usize),
    Autostart,
    /// Dil: `true` Türkçe.
    Lang(bool),
    Quit,
    RemoveHive,
}

type SetupResult = (usize, bool, Result<(), String>);

struct App {
    hwnd: HWND,
    gfx: Gfx,
    tray: Tray,
    icons: Vec<ImageId>,
    /// hive'ın kendi ikonu (kenar çubuğunun başında).
    logo: ImageId,
    page: Page,
    narrow_pref: bool,
    autostart: bool,
    lyrebird: Option<Lyrebird>,
    rabbit: Option<Rabbithole>,
    cheshire: Option<Cheshire>,
    /// Süren kurulum (`true`) ya da kaldırma (`false`), araç başına.
    busy: [Option<bool>; 3],
    /// Son kurulum / kaldırma hatası, araç kartında görünür.
    errors: [Option<String>; 3],
    results: Arc<Mutex<Vec<SetupResult>>>,
    /// hive kendini kaldırıyor; bitince süreç kapanır, klasör arkasından silinir.
    removing_self: bool,
    self_errors: Arc<Mutex<Vec<String>>>,
    hover: Hit,
    pressed: Hit,
    dpi: f32,
    taskbar_created: u32,
    /// Tepsi simgesine tıklamak pencereyi arka plana atar; az önce öndeyse tıklama "gizle" demektir.
    deactivated: Option<Instant>,
    active: bool,
    cap_hover: Option<Cap>,
    cap_pressed: Option<Cap>,
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
    static PENDING_TAB: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn autostart() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, w!("hive"), RRF_RT_REG_SZ, None, None, None).is_ok() }
}

fn set_autostart(on: bool) {
    unsafe {
        match util::installed_copy() {
            Some(exe) if on => {
                let data = wide(&format!("\"{exe}\" --hidden"));
                let _ = RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    RUN_KEY,
                    w!("hive"),
                    REG_SZ.0,
                    Some(data.as_ptr().cast()),
                    (data.len() * 2) as u32,
                );
            }
            _ => {
                let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, w!("hive"));
            }
        }
    }
}

/// Canlı araç sayfası (alanlar ayrı ödünç alınsın diye serbest işlev).
fn page_mut<'a>(
    lyrebird: &'a mut Option<Lyrebird>,
    rabbit: &'a mut Option<Rabbithole>,
    cheshire: &'a mut Option<Cheshire>,
    i: usize,
) -> Option<&'a mut dyn ToolPage> {
    match i {
        tools::LYREBIRD => lyrebird.as_mut().map(|l| l as &mut dyn ToolPage),
        tools::CHESHIRE => cheshire.as_mut().map(|c| c as &mut dyn ToolPage),
        tools::RABBITHOLE => rabbit.as_mut().map(|r| r as &mut dyn ToolPage),
        _ => None,
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
        Config { narrow: self.narrow_pref, tab: self.page.id().into(), turkish: crate::i18n::turkish() }.save();
    }

    fn visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() && !IsIconic(self.hwnd).as_bool() }
    }

    fn zoomed(&self) -> bool {
        unsafe { IsZoomed(self.hwnd).as_bool() }
    }

    fn installed(&self, i: usize) -> bool {
        match i {
            tools::LYREBIRD => self.lyrebird.is_some(),
            tools::CHESHIRE => self.cheshire.is_some(),
            _ => self.rabbit.is_some(),
        }
    }

    fn page_ref(&self, i: usize) -> Option<&dyn ToolPage> {
        match i {
            tools::LYREBIRD => self.lyrebird.as_ref().map(|l| l as &dyn ToolPage),
            tools::CHESHIRE => self.cheshire.as_ref().map(|c| c as &dyn ToolPage),
            tools::RABBITHOLE => self.rabbit.as_ref().map(|r| r as &dyn ToolPage),
            _ => None,
        }
    }

    /// Kenar çubuğu sırasıyla sayfalar: kurulu araçlar, Araçlar, Ayarlar.
    fn pages(&self) -> Vec<Page> {
        (0..TOOLS.len())
            .filter(|&i| self.installed(i))
            .map(Page::Tool)
            .chain([Page::Store, Page::Settings])
            .collect()
    }

    /// Kurulu değilse araç sayfası yerine Araçlar açılır.
    fn valid(&self, page: Page) -> Page {
        match page {
            Page::Tool(i) if !self.installed(i) => Page::Store,
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
        if let Some(l) = self.lyrebird.as_mut() {
            l.set_visible(on && page == Page::Tool(tools::LYREBIRD));
        }
        if let Some(c) = self.cheshire.as_mut() {
            c.set_visible(on && page == Page::Tool(tools::CHESHIRE));
        }
        if let Some(r) = self.rabbit.as_mut() {
            r.set_visible(on && page == Page::Tool(tools::RABBITHOLE));
        }
    }

    fn select(&mut self, page: Page) {
        let page = self.valid(page);
        if self.page != page {
            self.page = page;
            self.hover = Hit::None;
            self.save();
            self.sync_visible();
            self.redraw();
        }
    }

    // --- Araçlar: kur, kaldır, motorları başlat ---

    /// Motorları kurulu araçlarla eşler: kurulu olup çalışmayanı başlatır, kaldırılanı düşürür.
    fn sync_tools(&mut self) {
        for i in 0..TOOLS.len() {
            if self.busy[i].is_some() {
                continue;
            }
            let on = tools::installed(i);
            if on == self.installed(i) {
                continue;
            }
            let hwnd = self.hwnd;
            match (i, on) {
                (tools::LYREBIRD, true) => {
                    tools::retire_standalone_lyrebird();
                    self.lyrebird = Some(Lyrebird::start(hwnd));
                }
                (tools::LYREBIRD, false) => self.lyrebird = None,
                (tools::CHESHIRE, true) => self.cheshire = Some(Cheshire::start(hwnd)),
                (tools::CHESHIRE, false) => self.cheshire = None,
                (_, true) => self.rabbit = Some(Rabbithole::start(hwnd)),
                (_, false) => self.rabbit = None,
            }
        }
        self.page = self.valid(self.page);
        self.sync_visible();
        let installed: Vec<bool> = (0..TOOLS.len()).map(|i| self.installed(i)).collect();
        crate::shell::sync_shortcuts(&installed);
        self.redraw();
    }

    fn confirm_remove(&self, i: usize) -> bool {
        let detail = match i {
            tools::LYREBIRD => t!(
                "Your microphones go back to their previous settings and the audio service restarts for a second. Your sound list is deleted too.",
                "Mikrofonların eski ayarlarına döner, ses hizmeti bir saniyeliğine yeniden başlar. Ses listen de silinir."
            ),
            tools::CHESHIRE => t!(
                "Your wallpaper goes back to the Windows one. Wallpapers you added and their settings are deleted too.",
                "Duvar kâğıdı Windows'unkine döner. Eklediğin duvar kâğıtları ve ayarları da silinir."
            ),
            _ => t!(
                "The tunnel closes; the service, network adapter and your server link are deleted.",
                "Tünel kapanır; hizmet, ağ bağdaştırıcısı ve sunucu bağlantın silinir."
            ),
        };
        let name = TOOLS[i].name;
        let text = wide(&t!(
            format!("Remove {name}?\n\n{detail}\n\nNo trace of it is left on this computer."),
            format!("{name} kaldırılsın mı?\n\n{detail}\n\nBilgisayarda hiçbir izi kalmaz.")
        ));
        unsafe { MessageBoxW(Some(self.hwnd), PCWSTR(text.as_ptr()), w!("hive"), MB_YESNO | MB_ICONQUESTION) == IDYES }
    }

    /// Kurulum ya da kaldırma arka planda; bitince WM_TOOL_SETUP gelir.
    fn start_setup(&mut self, i: usize, install: bool) {
        if self.busy[i].is_some() || (!install && !self.confirm_remove(i)) {
            return;
        }
        if !install {
            // Motor önce durur: cheshire'ın exe'si silinebilsin, lyrebird kısayolları bıraksın.
            match i {
                tools::LYREBIRD => self.lyrebird = None,
                tools::CHESHIRE => self.cheshire = None,
                _ => self.rabbit = None,
            }
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
        let mut opened = None;
        for (i, install, r) in done {
            self.busy[i] = None;
            match r {
                Ok(()) => {
                    log!("{} {}", TOOLS[i].name, if install { "kuruldu" } else { "kaldırıldı" });
                    if install {
                        opened = Some(i);
                    }
                }
                Err(e) => {
                    log!("{} {}: {e}", TOOLS[i].name, if install { "kurulamadı" } else { "kaldırılamadı" });
                    self.errors[i] = Some(e);
                }
            }
        }
        self.sync_tools();
        if let Some(i) = opened.filter(|&i| self.installed(i)) {
            self.select(Page::Tool(i));
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
        let t = match page {
            Page::Tool(i) => {
                let n = (0..i).filter(|&j| self.installed(j)).count();
                HEADER + 8.0 + n as f32 * (NAV + 2.0)
            }
            Page::Store => h - 10.0 - 2.0 * NAV - 2.0,
            Page::Settings => h - 10.0 - NAV,
        };
        Rect::new(6.0, t, sw - 6.0, t + NAV)
    }

    /// Canlı araç sayfasının başlığında adın bittiği x (sayfanın kendi köşesine göre).
    fn head_x(&self, i: usize) -> f32 {
        PAD + 40.0 + self.gfx.measure(TOOLS[i].name, &self.gfx.f.heading)
    }

    /// Araç sayfasına boyutunu verir; kenar çubuğunun genişliğini döndürür.
    fn layout_tool(&mut self, w: f32, h: f32) -> f32 {
        let sw = self.side_w(w);
        if let Some(i) = self.tool_live() {
            let hx = self.head_x(i);
            let hr = w - sw - CAPTIONS - 12.0;
            if let Some(p) = page_mut(&mut self.lyrebird, &mut self.rabbit, &mut self.cheshire, i) {
                p.layout(w - sw, h, hx, hr);
            }
        }
        sw
    }

    fn card(&self, i: usize, w: f32) -> Rect {
        let (l, r) = (self.side_w(w) + PAGE_PAD, w - PAGE_PAD);
        let t = HEADER + 52.0 + i as f32 * (CARD_H + 12.0);
        Rect::new(l, t, r, t + CARD_H)
    }

    /// Kartın düğmeleri: (ana, ikinci). Kurulu: Aç + Kaldır, değil: Kur, sürüyor: durum yazısı.
    fn card_buttons(&self, i: usize, w: f32) -> (Rect, Option<Rect>) {
        let c = self.card(i, w);
        let t = c.cy() - 17.0;
        let g = &self.gfx;
        if let Some(install) = self.busy[i] {
            return (button_rect_right(g, c.r - 20.0, t, tools::busy_label(i, install), false), None);
        }
        if self.installed(i) {
            let remove = button_rect_right(g, c.r - 20.0, t, t!("Remove", "Kaldır"), false);
            (button_rect_right(g, remove.l - 8.0, t, t!("Open", "Aç"), false), Some(remove))
        } else {
            (button_rect_right(g, c.r - 20.0, t, t!("Install", "Kur"), true), None)
        }
    }

    /// hive'ı kaldır: önce kurulu bütün araçlar (her biri kendi kalıntı denetimiyle), sonra
    /// hive'ın kendi kayıtları ve kısayolları; klasörü süreç kapanınca silinir.
    fn start_self_remove(&mut self) {
        if self.removing_self || self.busy.iter().any(Option::is_some) {
            return;
        }
        let tools: Vec<usize> = (0..TOOLS.len()).filter(|&i| self.installed(i)).collect();
        let names: Vec<&str> = tools.iter().map(|&i| TOOLS[i].name).collect();
        let list = names.join(", ");
        let text = wide(&match (crate::i18n::turkish(), names.is_empty()) {
            (false, true) => "Remove hive?\n\nNo trace is left on this computer.".to_string(),
            (false, false) => format!(
                "Remove hive and the installed tools ({list})?\n\nTheir settings and data are deleted too; no trace is \
                 left on this computer. lyrebird and rabbithole ask for administrator permission."
            ),
            (true, true) => "hive kaldırılsın mı?\n\nBilgisayarda hiçbir iz kalmaz.".to_string(),
            (true, false) => format!(
                "hive ve kurulu araçlar ({list}) kaldırılsın mı?\n\nAraçların ayarları ve verileri de silinir; \
                 bilgisayarda hiçbir iz kalmaz. lyrebird ve rabbithole için yönetici izni istenir."
            ),
        });
        let yes = unsafe {
            MessageBoxW(Some(self.hwnd), PCWSTR(text.as_ptr()), w!("hive"), MB_YESNO | MB_ICONWARNING) == IDYES
        };
        if !yes {
            return;
        }
        log!("hive kaldırılıyor: {names:?}");
        self.lyrebird = None;
        self.cheshire = None;
        self.rabbit = None;
        self.page = Page::Settings;
        self.removing_self = true;
        self.redraw();
        let (errors, hwnd) = (self.self_errors.clone(), self.hwnd.0 as usize);
        std::thread::spawn(move || {
            for i in tools {
                if let Err(e) = tools::run(i, false) {
                    errors.lock().unwrap().push(format!("{}: {e}", TOOLS[i].name));
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
            log!("hive kaldırıldı");
            crate::shell::remove_self_later();
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
            return;
        }
        self.removing_self = false;
        let head = t!("hive could not be removed; nothing was left half done:", "hive kaldırılamadı, hiçbir şey yarım bırakılmadı:");
        let text = wide(&format!("{head}\n\n{}", errors.join("\n")));
        unsafe {
            MessageBoxW(Some(self.hwnd), PCWSTR(text.as_ptr()), w!("hive"), MB_OK | MB_ICONERROR);
        }
        self.sync_tools();
    }

    fn settings_row(&self, n: usize, w: f32) -> Rect {
        let t = HEADER + 12.0 + n as f32 * 64.0;
        Rect::new(self.side_w(w) + PAGE_PAD, t, w - PAGE_PAD, t + 64.0)
    }

    fn quit_rect(&self, w: f32) -> Rect {
        let r = self.settings_row(3, w);
        button_rect_right(&self.gfx, r.r, r.cy() - 17.0, t!("Quit", "Çık"), false)
    }

    fn remove_hive_rect(&self, w: f32) -> Rect {
        let r = self.settings_row(4, w);
        button_rect_right(&self.gfx, r.r, r.cy() - 17.0, t!("Remove", "Kaldır"), false)
    }

    /// Dil seçici: [English, Türkçe], satırın sağına yaslı.
    fn lang_rects(&self, w: f32) -> [Rect; 2] {
        let r = self.settings_row(1, w);
        let g = &self.gfx;
        let tr_w = g.measure("Türkçe", &g.f.button) + 26.0;
        let en_w = g.measure("English", &g.f.button) + 26.0;
        let tr = Rect::new(r.r - 3.0 - tr_w, r.cy() - 14.0, r.r - 3.0, r.cy() + 14.0);
        [Rect::new(tr.l - en_w, tr.t, tr.l, tr.b), tr]
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        let (w, h) = self.size();
        let sw = self.side_w(w);
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
            Page::Store => {
                for i in 0..TOOLS.len() {
                    if self.busy[i].is_some() {
                        continue;
                    }
                    let (primary, second) = self.card_buttons(i, w);
                    if primary.contains(x, y) {
                        return if self.installed(i) { Hit::Open(i) } else { Hit::Install(i) };
                    }
                    if second.is_some_and(|r| r.contains(x, y)) {
                        return Hit::Remove(i);
                    }
                }
                Hit::None
            }
            Page::Settings => {
                if self.settings_row(0, w).contains(x, y) {
                    Hit::Autostart
                } else if let Some(k) = self.lang_rects(w).iter().position(|r| r.contains(x, y)) {
                    Hit::Lang(k == 1)
                } else if self.quit_rect(w).contains(x, y) {
                    Hit::Quit
                } else if !self.removing_self && self.remove_hive_rect(w).contains(x, y) {
                    Hit::RemoveHive
                } else {
                    Hit::None
                }
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
            Hit::Install(i) => self.start_setup(i, true),
            Hit::Remove(i) => self.start_setup(i, false),
            Hit::Open(i) => self.select(Page::Tool(i)),
            Hit::Autostart => {
                if util::installed_copy().is_some() {
                    set_autostart(!self.autostart);
                    self.autostart = autostart();
                    self.redraw();
                }
            }
            Hit::Lang(tr) => {
                crate::i18n::set_turkish(tr);
                self.save();
                // Başlat menüsü açıklamaları da seçili dilde olsun.
                let installed: Vec<bool> = (0..TOOLS.len()).map(|i| self.installed(i)).collect();
                crate::shell::sync_shortcuts(&installed);
                self.redraw();
            }
            Hit::Quit => unsafe {
                let _ = DestroyWindow(self.hwnd);
            },
            Hit::RemoveHive => self.start_self_remove(),
            Hit::None | Hit::Content => {}
        }
    }

    // --- Çizim ---

    fn paint_nav(&self, page: Page, w: f32, h: f32) {
        let g = &self.gfx;
        let r = self.nav_rect(page, w, h);
        let selected = self.page == page;
        let hovered = self.hover == Hit::Nav(page);
        let accent = match page {
            Page::Tool(i) => Color::rgb(TOOLS[i].accent),
            _ => TEXT,
        };
        if selected {
            g.fill(r, 6.0, SEL);
            g.fill(Rect::new(r.l, r.t + 11.0, r.l + 3.0, r.b - 11.0), 1.5, accent);
        } else if hovered {
            g.fill(r, 6.0, HOVER);
        }
        let ic = Rect::new(r.l + 10.0, r.cy() - 12.0, r.l + 34.0, r.cy() + 12.0);
        let fg = if selected || hovered { TEXT } else { MUTED };
        match page {
            Page::Tool(i) => g.image(self.icons[i], ic, 1.0),
            Page::Store => g.text(ICON_APPS, &g.f.icon, ic, fg),
            Page::Settings => g.text(ICON_SETTINGS, &g.f.icon, ic, fg),
        }
        if !self.narrow(w) {
            g.text(page.label(), &g.f.text, Rect::new(r.l + 46.0, r.t, r.r - 10.0, r.b), fg);
        }
    }

    fn paint_sidebar(&self, w: f32, h: f32) {
        let g = &self.gfx;
        let sw = self.side_w(w);
        g.fill(Rect::new(sw - 1.0, 0.0, sw, h), 0.0, LINE);
        icon_button(g, Self::toggle_rect(), ICON_MENU, MUTED, self.hover == Hit::Toggle);
        if !self.narrow(w) {
            g.image(self.logo, Rect::new(56.0, HEAD_CY - 11.0, 78.0, HEAD_CY + 11.0), 1.0);
            g.text(NAME, &g.f.strong, Rect::new(88.0, HEAD_CY - 18.0, sw - 12.0, HEAD_CY + 18.0), TEXT);
        }
        for p in self.pages() {
            self.paint_nav(p, w, h);
        }
        // Araçlar ile alt bölüm arasında ince çizgi.
        let store = self.nav_rect(Page::Store, w, h);
        g.fill(Rect::new(14.0, store.t - 7.0, sw - 14.0, store.t - 6.0), 0.0, HOVER);
    }

    /// Canlı araç sayfası: başlıkta ikon ve ad, gerisini araç çizer; üstüne gelinen ikon
    /// düğmesinin açıklaması en üstte.
    fn paint_live(&self, i: usize, sw: f32, w: f32) {
        let g = &self.gfx;
        g.translate(sw, 0.0);
        g.image(self.icons[i], Rect::new(PAD, HEAD_CY - 14.0, PAD + 28.0, HEAD_CY + 14.0), 1.0);
        let name = Rect::new(PAD + 40.0, HEAD_CY - 18.0, PAD + 240.0, HEAD_CY + 18.0);
        g.text(TOOLS[i].name, &g.f.heading, name, TEXT);
        if let Some(p) = self.page_ref(i) {
            p.paint(g);
            if let Some((r, text)) = p.tip() {
                let tw = g.measure(&text, &g.f.small);
                let l = ((r.l + r.r) / 2.0 - tw / 2.0 - 10.0).clamp(8.0, w - sw - tw - 28.0);
                let tip = Rect::new(l, r.b + 6.0, l + tw + 20.0, r.b + 32.0);
                g.fill(tip, 6.0, SEL);
                g.stroke(tip, 6.0, LINE, 1.0);
                g.text(&text, &g.f.small_center, tip, TEXT);
            }
        }
        g.translate(0.0, 0.0);
    }

    fn page_title(&self, w: f32, title: &str, sub: &str) {
        let g = &self.gfx;
        let x0 = self.side_w(w) + PAGE_PAD;
        g.text(title, &g.f.heading, Rect::new(x0, HEAD_CY - 18.0, w - CAPTIONS - 12.0, HEAD_CY + 18.0), TEXT);
        g.text(sub, &g.f.small, Rect::new(x0, HEADER + 14.0, w - PAGE_PAD, HEADER + 36.0), MUTED);
    }

    fn paint_store(&self, w: f32) {
        let g = &self.gfx;
        self.page_title(
            w,
            t!("Tools", "Araçlar"),
            t!(
                "Tools you install show up in the sidebar; remove them any time.",
                "Kurduğun araçlar kenar çubuğunda görünür; istediğin zaman kaldırabilirsin."
            ),
        );
        for (i, tool) in TOOLS.iter().enumerate() {
            let c = self.card(i, w);
            let installed = self.installed(i);
            g.fill(c, 12.0, PANEL);
            g.stroke(c, 12.0, LINE, 1.0);
            g.image(self.icons[i], Rect::new(c.l + 20.0, c.cy() - 28.0, c.l + 76.0, c.cy() + 28.0), 1.0);

            let (primary, second) = self.card_buttons(i, w);
            let text_r = primary.l - 16.0;
            let x = c.l + 96.0;
            let name_w = g.measure(tool.name, &g.f.heading);
            g.text(tool.name, &g.f.heading, Rect::new(x, c.t + 16.0, text_r, c.t + 44.0), TEXT);
            if installed {
                status(g, x + name_w + 14.0, c.t + 31.0, GREEN, t!("Installed", "Kurulu"), MUTED, text_r);
            }
            g.text(tool.tagline(), &g.f.text, Rect::new(x, c.t + 46.0, text_r, c.t + 66.0), MUTED);
            match &self.errors[i] {
                Some(e) => g.text(e, &g.f.small, Rect::new(x, c.t + 68.0, text_r, c.t + 88.0), RED),
                None => g.text(tool.note(), &g.f.small, Rect::new(x, c.t + 68.0, text_r, c.t + 88.0), FAINT),
            }

            let accent = Color::rgb(tool.accent);
            match (self.busy[i], installed) {
                (Some(install), _) => button(g, primary, tools::busy_label(i, install), None, None, false),
                (None, true) => button(g, primary, t!("Open", "Aç"), None, Some(accent), self.hover == Hit::Open(i)),
                (None, false) => {
                    let label = t!("Install", "Kur");
                    button(g, primary, label, Some(ICON_DOWNLOAD), Some(accent), self.hover == Hit::Install(i))
                }
            }
            if let Some(r) = second {
                button(g, r, t!("Remove", "Kaldır"), None, None, self.hover == Hit::Remove(i));
            }
        }
    }

    fn paint_settings(&self, w: f32) {
        let g = &self.gfx;
        self.page_title(w, t!("Settings", "Ayarlar"), "");
        let dev = util::installed_copy().is_none();
        let r = self.settings_row(0, w);
        if self.hover == Hit::Autostart && !dev {
            g.fill(Rect::new(r.l - 12.0, r.t + 4.0, r.r + 12.0, r.b - 4.0), 8.0, HOVER.alpha(0.6));
        }
        let sub = match dev {
            true => t!("Not available in a development copy", "Geliştirme kopyasında kullanılamaz"),
            false => t!(
                "Starts quietly in the tray when you sign in; cheshire's wallpaper comes with it",
                "Oturum açınca tepside sessizce başlar; cheshire'ın duvar kâğıdı da onunla gelir"
            ),
        };
        setting_row(g, r, t!("Start with Windows", "Windows ile başlat"), sub, 120.0);
        toggle(g, toggle_rect(r.r, r.cy()), self.autostart, TEXT, !dev);

        let r = self.settings_row(1, w);
        setting_row(g, r, t!("Language", "Dil"), "", 200.0);
        let [en, tr] = self.lang_rects(w);
        g.fill(Rect::new(en.l - 3.0, en.t - 3.0, tr.r + 3.0, tr.b + 3.0), 8.0, HOVER);
        for (rect, label, on) in [(en, "English", !crate::i18n::turkish()), (tr, "Türkçe", crate::i18n::turkish())] {
            let hovered = self.hover == Hit::Lang(label == "Türkçe");
            if on {
                g.fill(rect, 6.0, SEL);
            } else if hovered {
                g.fill(rect, 6.0, SEL.alpha(0.6));
            }
            g.text(label, &g.f.button, rect, if on || hovered { TEXT } else { MUTED });
        }

        let r = self.settings_row(2, w);
        let sub = t!("hive and the lyrebird and cheshire engines inside it", "hive ve içindeki lyrebird ile cheshire motorları");
        setting_row(g, r, t!("Version", "Sürüm"), sub, 120.0);
        g.text(VERSION, &g.f.small_right, Rect::new(r.r - 120.0, r.t, r.r, r.b), MUTED);

        let r = self.settings_row(3, w);
        let sub = t!("hive and the tools' engines shut down completely", "hive ve araçların motorları tamamen kapanır");
        setting_row(g, r, t!("Quit", "Kapat"), sub, 120.0);
        button(g, self.quit_rect(w), t!("Quit", "Çık"), None, None, self.hover == Hit::Quit);

        let r = self.settings_row(4, w);
        let title = t!("Remove hive", "hive'ı kaldır");
        if self.removing_self {
            let sub = t!("Removing… each tool is removed and checked in turn", "Kaldırılıyor… araçlar sırayla kaldırılıp denetleniyor");
            setting_row(g, r, title, sub, 120.0);
        } else {
            let sub = t!(
                "Together with the installed tools; no trace is left on this computer",
                "Kurulu araçlarla birlikte; bilgisayarda hiçbir iz kalmaz"
            );
            setting_row(g, r, title, sub, 120.0);
            let label = t!("Remove", "Kaldır");
            button(g, self.remove_hive_rect(w), label, None, None, self.hover == Hit::RemoveHive);
        }
    }

    fn paint(&mut self) {
        let (w, h) = self.size();
        let sw = self.layout_tool(w, h);
        if !self.gfx.begin(self.hwnd, self.dpi, BG) {
            unsafe {
                let _ = ValidateRect(Some(self.hwnd), None);
            }
            return;
        }
        let g = &self.gfx;
        self.paint_sidebar(w, h);
        match (self.page, self.tool_live()) {
            (_, Some(i)) => g.clip(Rect::new(sw, 0.0, w, h), || self.paint_live(i, sw, w)),
            (Page::Settings, _) => self.paint_settings(w),
            _ => self.paint_store(w),
        }

        // Dar kenar çubuğunda üstüne gelinen öğenin adı.
        if self.narrow(w)
            && let Hit::Nav(p) = self.hover
        {
            let r = self.nav_rect(p, w, h);
            let tw = g.measure(p.label(), &g.f.text);
            let tip = Rect::new(sw + 6.0, r.cy() - 14.0, sw + 6.0 + tw + 20.0, r.cy() + 14.0);
            g.fill(tip, 6.0, SEL);
            g.stroke(tip, 6.0, LINE, 1.0);
            g.text(p.label(), &g.f.text, Rect::new(tip.l + 10.0, tip.t, tip.r, tip.b), TEXT);
        }
        // Başlık bandının alt çizgisi: bütün sayfalarda aynı yerde.
        g.fill(Rect::new(sw, HEADER - 1.0, w, HEADER), 0.0, LINE);
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
                    g.fill(r, 0.0, if pressed { SEL } else { HOVER });
                    TEXT
                }
                _ if self.active => MUTED,
                _ => FAINT,
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
    /// listesinden) hive'ı kaldırmayı başlat.
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
                        && let Some(p) = live.and_then(|i| page_mut(&mut self.lyrebird, &mut self.rabbit, &mut self.cheshire, i))
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
                    && let Some(p) = live.and_then(|i| page_mut(&mut self.lyrebird, &mut self.rabbit, &mut self.cheshire, i))
                {
                    p.mouse_move(&self.gfx, mx - sw, my);
                }
            }
            WM_MOUSELEAVE => {
                if let Some(p) = live.and_then(|i| page_mut(&mut self.lyrebird, &mut self.rabbit, &mut self.cheshire, i)) {
                    p.mouse_leave();
                }
                self.hover = Hit::None;
                self.redraw();
            }
            WM_LBUTTONDOWN => {
                let (w, h) = self.size();
                let sw = self.layout_tool(w, h);
                self.pressed = self.hit(mx, my);
                unsafe { SetCapture(self.hwnd) };
                if self.pressed == Hit::Content
                    && let Some(p) = live.and_then(|i| page_mut(&mut self.lyrebird, &mut self.rabbit, &mut self.cheshire, i))
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
                    if let Some(p) = live.and_then(|i| page_mut(&mut self.lyrebird, &mut self.rabbit, &mut self.cheshire, i)) {
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
                    && let Some(p) = live.and_then(|i| page_mut(&mut self.lyrebird, &mut self.rabbit, &mut self.cheshire, i))
                {
                    p.wheel(&self.gfx, x - sw, y, delta);
                }
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let vk = VIRTUAL_KEY(wp.0 as u16);
                if let Some(p) = live.and_then(|i| page_mut(&mut self.lyrebird, &mut self.rabbit, &mut self.cheshire, i))
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
            WM_SYSCHAR | WM_SYSKEYUP if self.lyrebird.as_ref().is_some_and(|l| l.binding()) => {}
            WM_KILLFOCUS => {
                if let Some(l) = self.lyrebird.as_mut() {
                    l.kill_focus();
                }
            }
            WM_HOTKEY => {
                if let Some(l) = self.lyrebird.as_mut() {
                    l.hotkey(wp.0 as i32);
                }
            }
            WM_TIMER => match wp.0 {
                lyrebird::TIMER_ANIM | lyrebird::TIMER_STATUS => {
                    if let Some(l) = self.lyrebird.as_mut() {
                        l.timer(wp.0);
                    }
                }
                rabbithole::TIMER_POLL | rabbithole::TIMER_PING => {
                    if let Some(r) = self.rabbit.as_mut() {
                        r.timer(wp.0);
                    }
                }
                _ => return None,
            },
            lyrebird::WM_PLAYER => {
                if let Some(l) = self.lyrebird.as_mut() {
                    l.player_changed();
                }
            }
            lyrebird::WM_INSTALLED => {
                if let Some(l) = self.lyrebird.as_mut() {
                    l.setup_done(wp.0);
                }
            }
            cheshire::WM_STATE => {
                if let Some(c) = self.cheshire.as_mut() {
                    c.state_changed();
                }
            }
            cheshire::WM_THUMB => {
                if let Some(c) = self.cheshire.as_mut() {
                    c.load_thumbs(&mut self.gfx);
                }
            }
            cheshire::WM_RELAUNCH => {
                if let Some(c) = self.cheshire.as_ref() {
                    c.launch();
                }
            }
            rabbithole::WM_DONE => {
                if let Some(r) = self.rabbit.as_mut() {
                    r.done();
                }
            }
            rabbithole::WM_LINE => {
                if let Some(r) = self.rabbit.as_mut() {
                    r.doctor_line();
                }
            }
            WM_TOOL_SETUP => self.setup_done(),
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
                match live {
                    Some(tools::LYREBIRD) => self.lyrebird.as_mut().map(|l| l.add(paths)),
                    Some(tools::CHESHIRE) => self.cheshire.as_mut().map(|c| c.add(paths)),
                    _ => None,
                };
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
            WM_PENDING_TAB => {
                if let Some(tab) = PENDING_TAB.with(|p| p.borrow_mut().take()) {
                    self.open_tab(&tab);
                }
            }
            WM_TRAY => match (lp.0 as u32) & 0xFFFF {
                WM_LBUTTONUP | WM_RBUTTONUP => self.toggle_window(),
                _ => {}
            },
            WM_CLOSE => self.hide(),
            m if m != 0 && m == self.taskbar_created => self.tray.add(),
            _ => return None,
        }
        Some(LRESULT(0))
    }
}

fn with_app<T>(f: impl FnOnce(&mut App) -> T) -> Option<T> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
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
        (r, app.lyrebird.as_mut().and_then(|l| l.take_modal()), app.cheshire.as_mut().and_then(|c| c.take_modal()))
    });
    match handled {
        Some((result, lmodal, cmodal)) => {
            // Dosya pencereleri kendi mesaj döngüsünü açar: uygulama borcu dışında çalışmalı.
            if let Some(m) = lmodal {
                lyrebird::run_modal(hwnd, m, |apply| {
                    with_app(|app| app.lyrebird.as_mut().map(apply));
                });
            }
            if let Some(m) = cmodal {
                cheshire::run_modal(hwnd, m, |apply| {
                    with_app(|app| app.cheshire.as_mut().map(apply));
                });
            }
            result.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
        }
        None if msg == WM_COPYDATA => {
            // Uygulama o an meşgul (örneğin cheshire'a komut gönderirken gelen istek): veriyi
            // sakla, kendi kuyruğumuzdan sonra uygula.
            let cds = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
            if cds.dwData != COPY_TAB {
                return LRESULT(0);
            }
            let bytes = unsafe { std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize) };
            PENDING_TAB.with(|p| *p.borrow_mut() = Some(String::from_utf8_lossy(bytes).into_owned()));
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_PENDING_TAB, WPARAM(0), LPARAM(0));
            }
            LRESULT(1)
        }
        None => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Zaten çalışan kopyaya sekmeyi iletir ve onu öne getirir.
pub fn forward(tab: Option<&str>) {
    unsafe {
        let Ok(hwnd) = FindWindowW(CLASS, PCWSTR::null()) else { return };
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        // Ön plana geçme hakkı bu süreçte (kullanıcı başlattı); öbürüne devret.
        let _ = AllowSetForegroundWindow(pid);
        let data = tab.unwrap_or("").as_bytes();
        let cds = COPYDATASTRUCT { dwData: COPY_TAB, cbData: data.len() as u32, lpData: data.as_ptr() as *mut _ };
        SendMessageW(hwnd, WM_COPYDATA, Some(WPARAM(0)), Some(LPARAM(&cds as *const _ as isize)));
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
            hbrBackground: CreateSolidBrush(COLORREF(0x100f0f)),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("hive"),
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
        // Windows 11: kenarlık ve gölge pencereyle aynı tonda (DWMWA_CAPTION_COLOR).
        let caption = COLORREF(0x100f0f);
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(35), &caption as *const _ as _, 4);
        // Gölge ve köşeler DWM'de kalsın (başlık çubuğu WM_NCCALCSIZE'da kaldırılıyor).
        let margins = MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 1, cyBottomHeight: 0 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        DragAcceptFiles(hwnd, true);

        let cfg = Config::load();
        let mut gfx = Gfx::new()?;
        let icons = TOOLS.iter().map(|t| gfx.load_image(t.icons)).collect::<Res<Vec<_>>>()?;
        let logo = gfx.load_image(&[
            (24, include_bytes!("../assets/icon-24.png")),
            (32, include_bytes!("../assets/icon-32.png")),
            (48, include_bytes!("../assets/icon-48.png")),
            (64, include_bytes!("../assets/icon-64.png")),
        ])?;
        let mut app = App {
            hwnd,
            gfx,
            tray: Tray::new(hwnd, WM_TRAY),
            icons,
            logo,
            page: Page::Store,
            narrow_pref: cfg.narrow,
            autostart: autostart(),
            lyrebird: None,
            rabbit: None,
            cheshire: None,
            busy: [None; 3],
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
        };
        app.sync_tools();
        crate::shell::register_app();
        // İstenen ya da son açık sekme; kurulu araç yoksa Araçlar.
        let first = (0..TOOLS.len()).find(|&i| app.installed(i)).map_or(Page::Store, Page::Tool);
        let page = tab.as_deref().and_then(Page::from_id).or_else(|| Page::from_id(&cfg.tab)).unwrap_or(first);
        app.page = app.valid(page);
        log!("başladı: sekme {}, kurulu {:?}", app.page.id(), (0..3).map(|i| app.installed(i)).collect::<Vec<_>>());
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
