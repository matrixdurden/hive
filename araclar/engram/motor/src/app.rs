//! Pencere: üst bantta solda engram, ortada "Ana · Hafıza" geçişi, sağda ayarlar ve pencere
//! düğmeleri; altında seçili sayfa tam genişlikte. Başlık çubuğu yok: pencere düğmelerini ve
//! sürükleme bandını kendimiz çiziyoruz. Tek iş parçacığı; yalnızca bir şey değişince çizilir.
//! Kapatınca tepside kalır. Beynin, sesin ve klavye kancasının olayları da buraya gelir.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
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

use crate::config::Config;
use crate::gfx::{Color, Gfx, ImageId, Rect};
use crate::log;
use crate::tray::Tray;
use crate::ui::*;
use crate::util::{self, Res, wide};

pub const CLASS: PCWSTR = w!("engram");
pub const NAME: &str = "engram";

const WM_TRAY: u32 = WM_APP + 1;
/// Uygulama meşgulken gelen sekme isteği sonradan uygulanır.
const WM_PENDING_TAB: u32 = WM_APP + 3;
/// Tamamen kapan (WM_CLOSE yalnızca gizler).
pub const WM_EXIT: u32 = WM_APP + 5;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_NCMOUSELEAVE: u32 = 0x02A2;
/// WM_COPYDATA türü: ikinci kopyadan gelen "şu sekmeyi aç" isteği (boşsa yalnızca öne gel).
const COPY_TAB: usize = 0x6d74;

/// Ses notu değişmiş mi diye bakan zamanlayıcı.
const NOTE_TIMER: usize = 0x6e74;

/// Pencere düğmeleri (küçült, büyüt, kapat).
const CAP_W: f32 = 46.0;
const CAP_H: f32 = HEADER;
const CAPTIONS: f32 = CAP_W * 3.0;
const CLOSE_RED: Color = Color::rgb(0xc42b1c);

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

/// Sayfalar, sırayla: ortadaki geçişte ilk ikisi, ayarlar sağdaki düğmede.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Home,
    Memory,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Home, Tab::Memory, Tab::Settings];

    pub fn id(self) -> &'static str {
        match self {
            Tab::Home => "home",
            Tab::Memory => "memory",
            Tab::Settings => "settings",
        }
    }

    fn from_id(s: &str) -> Option<Tab> {
        match s.trim().to_ascii_lowercase().as_str() {
            "home" | "ana" | "chat" | "sohbet" => Some(Tab::Home),
            "memory" | "hafiza" | "hafıza" | "notes" | "notlar" => Some(Tab::Memory),
            "settings" | "ayarlar" => Some(Tab::Settings),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tab::Home => t!("Home", "Ana"),
            Tab::Memory => t!("Memory", "Hafıza"),
            Tab::Settings => t!("Settings", "Ayarlar"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Nav(Tab),
    /// Sayfanın kendi alanı (sayfa kendisi işler).
    Content,
}

/// Sayfaların ortak durumu: ayarlar ve not klasörü. Her sayfa aynı `Rc`'yi tutar.
pub type Shared = Rc<RefCell<crate::pages::State>>;

struct App {
    hwnd: HWND,
    gfx: Gfx,
    tray: Tray,
    logo: ImageId,
    tab: Tab,
    pages: Vec<Box<dyn Page>>,
    voice: crate::voice::Voice,
    /// Ses notunun son değişme zamanı (değişince ses yeniden kurulur).
    note_mtime: u64,
    shared: Shared,
    hover: Hit,
    pressed: Hit,
    dpi: f32,
    taskbar_created: u32,
    /// Tepsi simgesine tıklamak pencereyi arka plana atar; az önce öndeyse tıklama "gizle" demektir.
    deactivated: Option<Instant>,
    active: bool,
    cap_hover: Option<Cap>,
    cap_pressed: Option<Cap>,
    mica: bool,
    /// Bilgisayar kontrolü için küçültüldü: yazılı tur bitince geri gelsin.
    restore_after: bool,
    /// WM_CHAR'da bekleyen vekil çiftin ilk yarısı.
    high_surrogate: Option<u16>,
}

/// Başlık çubuğunu biz çiziyoruz (pencere kurulduktan sonra açılır).
static CUSTOM_FRAME: AtomicBool = AtomicBool::new(false);
/// Mica: pencere düğmelerini Windows çizer ve işler.
static MICA: AtomicBool = AtomicBool::new(false);

/// WM_NCCALCSIZE: başlık çubuğunu ve üst kenarlığı kaldırır, diğer kenarlıklar yerinde kalır.
fn nc_calc_size(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let p = unsafe { &mut *(lp.0 as *mut NCCALCSIZE_PARAMS) };
    let top = p.rgrc[0].top;
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    p.rgrc[0].top = top;
    if unsafe { IsZoomed(hwnd).as_bool() } {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let frame = unsafe { GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) };
        p.rgrc[0].top += frame;
    }
    LRESULT(0)
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static PENDING_TAB: RefCell<Option<String>> = const { RefCell::new(None) };
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
        let mut s = self.shared.borrow_mut();
        s.config.tab = self.tab.id().into();
        s.config.turkish = crate::i18n::turkish();
        s.config.theme = theme();
        s.config.save();
    }

    fn visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() && !IsIconic(self.hwnd).as_bool() }
    }

    fn zoomed(&self) -> bool {
        unsafe { IsZoomed(self.hwnd).as_bool() }
    }

    fn page(&mut self) -> &mut (dyn Page + 'static) {
        let i = Tab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0);
        self.pages[i].as_mut()
    }

    fn page_ref(&self) -> &dyn Page {
        let i = Tab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0);
        self.pages[i].as_ref()
    }

    /// Sayfaların görünürlüğünü bildirir: zamanlayıcılar yalnızca ekrandaki sayfada çalışır.
    fn sync_visible(&mut self) {
        let on = self.visible();
        let tab = self.tab;
        for (i, p) in self.pages.iter_mut().enumerate() {
            p.set_visible(on && Tab::ALL[i] == tab);
        }
    }

    fn select(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.hover = Hit::None;
            self.save();
            self.sync_visible();
            self.redraw();
        }
    }

    // --- Yerleşim ---

    /// Ortadaki geçiş: (sayfa, düğme).
    fn nav_rects(&self, w: f32) -> Vec<(Tab, Rect)> {
        let g = &self.gfx;
        let tabs = [Tab::Home, Tab::Memory];
        let widths: Vec<f32> = tabs.iter().map(|t| g.measure(t.label(), &g.f.button) + 36.0).collect();
        let total: f32 = widths.iter().sum();
        let mut x = (w / 2.0 - total / 2.0).max(150.0);
        tabs.iter()
            .zip(widths)
            .map(|(&t, wi)| {
                let r = Rect::new(x, HEAD_CY - 15.0, x + wi, HEAD_CY + 15.0);
                x += wi;
                (t, r)
            })
            .collect()
    }

    fn gear_rect(w: f32) -> Rect {
        let r = w - CAPTIONS - 8.0;
        Rect::new(r - 36.0, HEAD_CY - 16.0, r, HEAD_CY + 16.0)
    }

    /// Sayfaya boyutunu verir: başlık bandında sayfa düğmelerinin yeri logodan ayarlar
    /// düğmesine kadar.
    fn layout_page(&mut self, w: f32, h: f32) {
        let hr = Self::gear_rect(w).l - 8.0;
        let hx = self.nav_rects(w).last().map_or(150.0, |n| n.1.r + 12.0);
        self.page().layout(w, h, hx, hr);
    }

    fn set_theme(&mut self, t: Theme) {
        if t == theme() {
            return;
        }
        set_theme(t);
        let mica = pal().mica;
        apply_backdrop(self.hwnd, mica);
        self.gfx.set_transparent(mica);
        self.mica = mica;
        self.cap_hover = None;
        self.save();
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER);
        }
        self.redraw();
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        let (w, _) = self.size();
        if y < HEADER {
            if Self::gear_rect(w).contains(x, y) {
                return Hit::Nav(Tab::Settings);
            }
            if let Some((t, _)) = self.nav_rects(w).into_iter().find(|(_, r)| r.contains(x, y)) {
                return Hit::Nav(t);
            }
        }
        Hit::Content
    }

    fn click(&mut self, hit: Hit) {
        if let Hit::Nav(t) = hit {
            // Ayarlar düğmesi açıkken tekrar basılınca geri döner.
            let t = if t == Tab::Settings && self.tab == Tab::Settings { Tab::Home } else { t };
            self.select(t);
        }
    }

    // --- Çizim ---

    fn paint_header(&self, w: f32) {
        let g = &self.gfx;
        g.image(self.logo, Rect::new(16.0, HEAD_CY - 11.0, 38.0, HEAD_CY + 11.0), 1.0);
        g.text(NAME, &g.f.strong, Rect::new(46.0, HEAD_CY - 18.0, 140.0, HEAD_CY + 18.0), pal().text);
        let navs = self.nav_rects(w);
        if let (Some(first), Some(last)) = (navs.first(), navs.last()) {
            let track = Rect::new(first.1.l - 3.0, first.1.t - 3.0, last.1.r + 3.0, last.1.b + 3.0);
            g.fill(track, track.h() / 2.0, pal().panel);
        }
        for (t, r) in navs {
            let selected = self.tab == t;
            let hovered = self.hover == Hit::Nav(t);
            if selected {
                g.fill(r, r.h() / 2.0, pal().sel.alpha(1.6));
            } else if hovered {
                g.fill(r, r.h() / 2.0, pal().hover);
            }
            g.text(t.label(), &g.f.button, r, if selected || hovered { pal().text } else { pal().muted });
        }
        let gear = Self::gear_rect(w);
        let on = self.tab == Tab::Settings;
        if on {
            g.fill(gear, 6.0, pal().sel);
        }
        icon_button(g, gear, ICON_SETTINGS, if on { pal().text } else { pal().muted }, self.hover == Hit::Nav(Tab::Settings));
    }

    /// Sayfa ve üstüne gelinen düğmenin açıklaması.
    fn paint_page(&self, w: f32) {
        let g = &self.gfx;
        let p = self.page_ref();
        p.paint(g);
        if let Some((r, text)) = p.tip() {
            let tw = g.measure(&text, &g.f.small);
            let l = ((r.l + r.r) / 2.0 - tw / 2.0 - 10.0).clamp(8.0, w - tw - 28.0);
            let tip = Rect::new(l, r.b + 6.0, l + tw + 20.0, r.b + 32.0);
            g.fill(tip, 4.0, pal().tip);
            g.stroke(tip, 4.0, pal().line, 1.0);
            g.text(&text, &g.f.small_center, tip, pal().text);
        }
    }

    fn paint(&mut self) {
        let (w, h) = self.size();
        self.layout_page(w, h);
        if !self.gfx.begin(self.hwnd, self.dpi, pal().bg) {
            unsafe {
                let _ = ValidateRect(Some(self.hwnd), None);
            }
            return;
        }
        self.gfx.translate(0.0, 0.0);
        self.paint_page(w);
        self.paint_header(w);
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
        if self.mica {
            return;
        }
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
        if y >= HEADER || self.hit(x, y) != Hit::Content {
            return HTCLIENT;
        }
        if self.page_ref().interactive(&self.gfx, x, y) { HTCLIENT } else { HTCAPTION }
    }

    // --- Pencere ---

    fn show(&mut self) {
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

    /// İkinci kopyadan ya da komut satırından: sekmeyi aç; "kaldir" ise engram'ı kaldırmayı başlat.
    fn open_tab(&mut self, tab: &str) {
        if tab.trim() == "kaldir" && !util::hub() {
            self.select(Tab::Settings);
            self.show();
            self.shared.borrow_mut().remove_requested = true;
            self.redraw();
            return;
        }
        // "hafiza:projeler/engram.md" gibi: sekme ve açılacak not.
        let (tab, note) = tab.split_once(':').map_or((tab, None), |(t, n)| (t, Some(n.to_string())));
        if let Some(n) = note {
            self.shared.borrow_mut().open_note = Some(n);
        }
        if let Some(t) = Tab::from_id(tab) {
            self.select(t);
        }
        self.show();
        self.redraw();
    }

    /// Sayfaların arayüze bıraktığı istekler: sekme değiştir, temayı değiştir, çık, kaldır.
    fn requests(&mut self) {
        let (tab, theme_req, quit, remove, redraw, voice) = {
            let mut s = self.shared.borrow_mut();
            (
                s.open_tab.take(),
                s.set_theme.take(),
                std::mem::take(&mut s.quit),
                std::mem::take(&mut s.remove_requested),
                std::mem::take(&mut s.redraw),
                s.voice_req.take(),
            )
        };
        if let Some(r) = voice {
            use crate::pages::VoiceReq;
            if r == VoiceReq::Apply {
                self.apply_voice();
            } else {
                let shared = self.shared.clone();
                let mut s = shared.borrow_mut();
                match r {
                    VoiceReq::Listen => self.voice.listen(&mut s.brain),
                    VoiceReq::Stop => self.voice.stop(&mut s.brain),
                    VoiceReq::Train => self.voice.train(&mut s.brain),
                    VoiceReq::Apply => {}
                }
                drop(s);
                self.sync_voice();
            }
            self.redraw();
        }
        if let Some(t) = tab {
            self.select(t);
        }
        if let Some(t) = theme_req {
            self.set_theme(t);
        }
        if redraw {
            self.redraw();
        }
        if remove {
            self.remove_self();
        }
        if quit {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }

    // --- Ses ---

    /// Ses notunu okur (yoksa oluşturur) ve sesi ayarlara göre kurar.
    fn apply_voice(&mut self) {
        let (cfg, note) = {
            let mut s = self.shared.borrow_mut();
            let note = s.vault.system_note(crate::voice::NOTE, crate::voice::DEFAULT_NOTE);
            (s.config.clone(), note)
        };
        self.note_mtime = self.note_file_mtime();
        self.voice.apply(&cfg, &note);
        crate::hotkey::set(crate::hotkey::Hotkey::parse(&cfg.hotkey));
        self.sync_voice();
    }

    fn note_file_mtime(&self) -> u64 {
        let path = self.shared.borrow().vault.full(&format!("{}/{}.md", crate::notes::SYSTEM_DIR, crate::voice::NOTE));
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs())
    }

    /// Ses notu dışarıda (Hafıza'da ya da Obsidian'da) değiştiyse sesi yeniden kurar.
    fn check_note(&mut self) {
        if self.note_file_mtime() != self.note_mtime {
            self.shared.borrow_mut().vault.refresh();
            self.apply_voice();
        }
    }

    /// Sesin durumunu sayfaların göreceği yere yazar.
    fn sync_voice(&mut self) {
        let mut s = self.shared.borrow_mut();
        s.voice_state = self.voice.state;
        s.wake_phrase = self.voice.wake_phrase();
        s.trained = self.voice.trained;
        s.train_msg = self.voice.train_msg.clone();
    }

    /// Beynin olayları: döküm tazelenir, ses ve ekran ışığı haberdar olur. Bilgisayar kontrolü
    /// başlayınca pencere önden çekilir (yazılı turda iş bitince geri gelir).
    fn on_agent(&mut self) {
        let outs = self.shared.borrow_mut().brain.on_events();
        if outs.iter().any(|o| matches!(o, crate::agent::Out::Changed(_))) {
            self.shared.borrow_mut().vault.refresh();
            self.check_note();
        }
        if outs.iter().any(|o| matches!(o, crate::agent::Out::Computer)) && self.visible() {
            self.restore_after = self.shared.borrow().brain.mode == crate::agent::Mode::Text;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_MINIMIZE);
            }
        }
        if outs.iter().any(|o| matches!(o, crate::agent::Out::Done)) && std::mem::take(&mut self.restore_after) {
            self.show();
        }
        {
            let s = self.shared.borrow();
            self.voice.on_agent(&outs, &s.brain);
        }
        self.sync_voice();
        self.redraw();
    }

    /// engram'ı kaldır: kayıtlar ve kısayol silinir, klasör süreç kapanınca; notlara dokunulmaz.
    fn remove_self(&mut self) {
        let text = wide(t!(
            "Remove engram?\n\nYour notes stay where they are. Settings, keys and the program folder are deleted.",
            "engram kaldırılsın mı?\n\nNotların yerinde kalır. Ayarlar, anahtarlar ve program klasörü silinir."
        ));
        let yes = unsafe { MessageBoxW(Some(self.hwnd), PCWSTR(text.as_ptr()), w!("engram"), MB_YESNO | MB_ICONWARNING) == IDYES };
        if !yes {
            return;
        }
        log!("engram kaldırılıyor");
        crate::shell::unregister_app();
        crate::shell::remove_self_later();
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    // --- Mesajlar ---

    fn on(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        let dpi = self.dpi;
        let dip = move |v: i32| v as f32 * 96.0 / dpi;
        let (mx, my) = (dip((lp.0 & 0xFFFF) as i16 as i32), dip(((lp.0 >> 16) & 0xFFFF) as i16 as i32));
        let r = match msg {
            WM_PAINT => {
                self.paint();
                Some(LRESULT(0))
            }
            WM_NCHITTEST => {
                let def = unsafe { DefWindowProcW(self.hwnd, msg, wp, lp) };
                if def.0 as u32 != HTCLIENT {
                    return Some(def);
                }
                let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                }
                let (w, h) = self.size();
                self.layout_page(w, h);
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
                Some(LRESULT(0))
            }
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if Cap::from_ht(wp.0 as u32).is_some() => {
                self.cap_pressed = Cap::from_ht(wp.0 as u32);
                self.redraw();
                Some(LRESULT(0))
            }
            WM_NCLBUTTONUP if Cap::from_ht(wp.0 as u32).is_some() => {
                let c = Cap::from_ht(wp.0 as u32);
                if let Some(c) = c.filter(|&c| self.cap_pressed.take() == Some(c)) {
                    self.cap_click(c);
                }
                self.redraw();
                Some(LRESULT(0))
            }
            WM_SIZE => {
                self.hover = Hit::None;
                self.sync_visible();
                self.redraw();
                Some(LRESULT(0))
            }
            WM_SETTINGCHANGE => {
                reset_accent();
                self.redraw();
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                self.dpi = (wp.0 & 0xFFFF) as f32;
                let r = unsafe { &*(lp.0 as *const RECT) };
                unsafe {
                    let (cx, cy) = (r.right - r.left, r.bottom - r.top);
                    let _ = SetWindowPos(self.hwnd, None, r.left, r.top, cx, cy, SWP_NOZORDER | SWP_NOACTIVATE);
                }
                Some(LRESULT(0))
            }
            WM_GETMINMAXINFO => {
                let info = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
                info.ptMinTrackSize = POINT { x: (560.0 * self.dpi / 96.0) as i32, y: (440.0 * self.dpi / 96.0) as i32 };
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                let (w, h) = self.size();
                self.layout_page(w, h);
                let hit = self.hit(mx, my);
                if hit != self.hover {
                    if self.hover == Hit::Content {
                        self.page().mouse_leave();
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
                if hit == Hit::Content || self.pressed == Hit::Content {
                    let g = &self.gfx as *const Gfx;
                    // Gfx sayfa çizimi dışında yalnızca ölçüm için kullanılır; ödünç çakışması yok.
                    self.page().mouse_move(unsafe { &*g }, mx, my);
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.page().mouse_leave();
                self.hover = Hit::None;
                self.redraw();
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                let (w, h) = self.size();
                self.layout_page(w, h);
                self.pressed = self.hit(mx, my);
                unsafe { SetCapture(self.hwnd) };
                if self.pressed == Hit::Content {
                    let g = &self.gfx as *const Gfx;
                    self.page().mouse_down(unsafe { &*g }, mx, my);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                unsafe {
                    let _ = ReleaseCapture();
                }
                let (w, h) = self.size();
                self.layout_page(w, h);
                let pressed = std::mem::replace(&mut self.pressed, Hit::None);
                if pressed == Hit::Content {
                    let g = &self.gfx as *const Gfx;
                    self.page().mouse_up(unsafe { &*g }, mx, my);
                } else if self.hit(mx, my) == pressed {
                    self.click(pressed);
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
                let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                }
                let (x, y) = (dip(pt.x), dip(pt.y));
                let (w, h) = self.size();
                self.layout_page(w, h);
                let g = &self.gfx as *const Gfx;
                self.page().wheel(unsafe { &*g }, x, y, delta);
                Some(LRESULT(0))
            }
            WM_CHAR => {
                // Emoji gibi BMP dışı karakterler iki WM_CHAR ile (vekil çift) gelir.
                let u = wp.0 as u32;
                if (0xD800..0xDC00).contains(&u) {
                    self.high_surrogate = Some(u as u16);
                    return Some(LRESULT(0));
                }
                let c = match self.high_surrogate.take() {
                    Some(hi) if (0xDC00..0xE000).contains(&u) => char::decode_utf16([hi, u as u16]).next()?.ok()?,
                    _ => char::from_u32(u)?,
                };
                if !self.page().char(c) {
                    return None;
                }
                Some(LRESULT(0))
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let vk = VIRTUAL_KEY(wp.0 as u16);
                if self.page().key(vk.0) {
                    return Some(LRESULT(0));
                }
                // Ctrl+Tab ve Ctrl+1..3: sayfalar sırayla.
                let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                let n = Tab::ALL.len();
                let i = Tab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0);
                match vk {
                    VK_TAB if ctrl => self.select(Tab::ALL[if shift { (i + n - 1) % n } else { (i + 1) % n }]),
                    _ if ctrl && (0x31..0x31 + n as u16).contains(&vk.0) => self.select(Tab::ALL[(vk.0 - 0x31) as usize]),
                    _ => return None,
                }
                Some(LRESULT(0))
            }
            WM_SYSCHAR | WM_SYSKEYUP if self.pages.iter().any(|p| p.binding()) => Some(LRESULT(0)),
            WM_KILLFOCUS => {
                self.pages.iter_mut().for_each(|p| p.kill_focus());
                Some(LRESULT(0))
            }
            WM_EXIT => unsafe {
                let _ = DestroyWindow(self.hwnd);
                Some(LRESULT(0))
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
                self.page().add_files(paths);
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                self.active = (wp.0 & 0xFFFF) as u32 != WA_INACTIVE;
                if !self.active {
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
                Some(LRESULT(0))
            }
            crate::brain::WM_AGENT => {
                self.on_agent();
                Some(LRESULT(0))
            }
            crate::voice::WM_VOICE => {
                let shared = self.shared.clone();
                self.voice.on_events(&mut shared.borrow_mut().brain);
                self.sync_voice();
                self.redraw();
                Some(LRESULT(0))
            }
            crate::voice::WM_LEVEL => {
                let v = wp.0 as f32 / 1000.0;
                self.voice.level(v);
                self.shared.borrow_mut().level = v;
                Some(LRESULT(0))
            }
            crate::voice::WM_KEY => {
                let shared = self.shared.clone();
                self.voice.key(wp.0, &mut shared.borrow_mut().brain);
                self.sync_voice();
                self.redraw();
                Some(LRESULT(0))
            }
            crate::voice::WM_CONFIRM => {
                let shared = self.shared.clone();
                self.voice.confirm(wp.0 == 1, &mut shared.borrow_mut().brain);
                self.sync_voice();
                self.redraw();
                Some(LRESULT(0))
            }
            WM_TIMER if wp.0 == crate::voice::TIMER => {
                self.voice.tick();
                self.shared.borrow_mut().level = self.voice.out_level();
                self.sync_voice();
                Some(LRESULT(0))
            }
            crate::reminders::WM_REMINDER => {
                for text in crate::reminders::take_fired() {
                    crate::log!("hatırlatma: {text}");
                    self.tray.notify(t!("Reminder", "Hatırlatma"), &text);
                    self.voice.announce(t!("Reminder", "Hatırlatma"), &text);
                }
                self.sync_voice();
                self.redraw();
                Some(LRESULT(0))
            }
            WM_TIMER if wp.0 == NOTE_TIMER => {
                self.check_note();
                Some(LRESULT(0))
            }
            WM_TRAY => {
                if matches!((lp.0 as u32) & 0xFFFF, WM_LBUTTONUP | WM_RBUTTONUP) {
                    self.toggle_window();
                }
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                self.hide();
                Some(LRESULT(0))
            }
            WM_TIMER if wp.0 == crate::tray::TIMER_HIDE => {
                self.tray.hide();
                Some(LRESULT(0))
            }
            m if m != 0 && m == self.taskbar_created => {
                self.tray.add();
                Some(LRESULT(0))
            }
            // Sayfaların kendi mesajları, zamanlayıcıları, kısayolları.
            _ => {
                let gfx = &mut self.gfx as *mut Gfx;
                self.pages.iter_mut().find_map(|p| p.message(unsafe { &mut *gfx }, msg, wp, lp))
            }
        };
        self.requests();
        r
    }
}

fn with_app<T>(f: impl FnOnce(&mut App) -> T) -> Option<T> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

/// Pencerenin zemini: Mica (Windows teması, Windows 11 22H2+) ya da temanın düz rengi.
fn apply_backdrop(hwnd: HWND, mica: bool) {
    use windows::Win32::Graphics::Dwm::{DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMWA_SYSTEMBACKDROP_TYPE};
    MICA.store(mica, Ordering::Relaxed);
    unsafe {
        if mica {
            let margins = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
            let backdrop = DWMSBT_MAINWINDOW;
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &backdrop as *const _ as _, 4);
        } else {
            let backdrop = DWMSBT_NONE;
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &backdrop as *const _ as _, 4);
            let c = pal().bg.0;
            let caption = COLORREF((c & 0xff) << 16 | (c & 0xff00) | (c >> 16) & 0xff);
            let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(35), &caption as *const _ as _, 4);
            let margins = MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 1, cyBottomHeight: 0 };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCALCSIZE && wp.0 != 0 && CUSTOM_FRAME.load(Ordering::Relaxed) {
        return nc_calc_size(hwnd, msg, wp, lp);
    }
    if MICA.load(Ordering::Relaxed) {
        let mut r = LRESULT(0);
        if unsafe { windows::Win32::Graphics::Dwm::DwmDefWindowProc(hwnd, msg, wp, lp, &mut r) }.as_bool() {
            return r;
        }
    }
    if msg == WM_DESTROY {
        APP.with(|a| drop(a.borrow_mut().take()));
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    match with_app(|app| app.on(msg, wp, lp)) {
        Some(result) => result.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wp, lp) }),
        None if msg == WM_COPYDATA => {
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
        let _ = AllowSetForegroundWindow(pid);
        let data = tab.unwrap_or("").as_bytes();
        let cds = COPYDATASTRUCT { dwData: COPY_TAB, cbData: data.len() as u32, lpData: data.as_ptr() as *mut _ };
        SendMessageW(hwnd, WM_COPYDATA, Some(WPARAM(0)), Some(LPARAM(&cds as *const _ as isize)));
    }
}

pub fn run(cfg: Config, hidden: bool, tab: Option<String>) -> Res<()> {
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
            w!("engram"),
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
        let size = ((1040.0 * dpi / 96.0) as i32, (680.0 * dpi / 96.0) as i32);
        let _ = SetWindowPos(hwnd, None, 0, 0, size.0, size.1, SWP_NOMOVE | SWP_NOZORDER);
        let dark = BOOL(1);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        set_theme(cfg.theme);
        let mica = pal().mica;
        apply_backdrop(hwnd, mica);
        DragAcceptFiles(hwnd, true);

        let mut gfx = Gfx::new()?;
        gfx.set_transparent(mica);
        let logo = gfx.load_image(&[
            (24, include_bytes!("../assets/icon-24.png")),
            (32, include_bytes!("../assets/icon-32.png")),
            (48, include_bytes!("../assets/icon-48.png")),
            (64, include_bytes!("../assets/icon-64.png")),
        ])?;
        let last_tab = cfg.tab.clone();
        let shared: Shared = Rc::new(RefCell::new(crate::pages::State::new(cfg, hwnd)));
        let pages = crate::pages::all(&shared, hwnd);
        let mut app = App {
            hwnd,
            gfx,
            tray: Tray::new(hwnd, WM_TRAY),
            logo,
            tab: Tab::Home,
            pages,
            voice: crate::voice::Voice::new(hwnd),
            note_mtime: 0,
            shared,
            hover: Hit::None,
            pressed: Hit::None,
            dpi,
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            deactivated: None,
            active: true,
            cap_hover: None,
            cap_pressed: None,
            mica,
            restore_after: false,
            high_surrogate: None,
        };
        // winloom'da kısayol ve Uygulamalar kaydı winloom'ın.
        if !util::hub() {
            crate::shell::register_app();
            crate::shell::sync_shortcut();
        }
        let tab_req = tab.as_deref().and_then(Tab::from_id).or_else(|| Tab::from_id(&last_tab));
        app.tab = tab_req.unwrap_or(Tab::Home);
        app.apply_voice();
        crate::hotkey::hook(hwnd, crate::voice::WM_KEY);
        crate::reminders::init(hwnd);
        SetTimer(Some(hwnd), NOTE_TIMER, 4000, None);
        log!("başladı: sekme {}", app.tab.id());
        APP.with(|a| *a.borrow_mut() = Some(app));
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

/// Pencerede olmayan yerden (sayfaların iş parçacıkları) yeniden çizim.
pub fn redraw(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

#[allow(dead_code)]
pub fn data_dir() -> PathBuf {
    util::data_dir()
}
