//! engram'ın winloom'daki sayfası: başlıkta "Ana · Hafıza" geçişi ve ayarlar düğmesi, altında
//! seçili alt sayfa. Beynin, sesin, klavye kancasının ve hatırlatmaların olayları winloom'ın
//! penceresine gelir, buradan dağıtılır. engram'ın kendi penceresi yok; yalnızca konuşurken
//! ekranda ışık katmanı (overlay) görünür.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Config;
use crate::gfx::{Gfx, Rect};
use crate::log;
use crate::ui::*;

/// Başlığın altındaki sekme şeridi (Sohbet · Hafıza · Ayarlar): ortak kitteki.
const TABS: f32 = ortak::form::TABS_H;
/// Ses notunun değişip değişmediğine bakan zamanlayıcı.
const NOTE_TIMER: usize = 0x6e74;

/// Alt sayfalar, sırayla: başlıktaki geçişte ilk ikisi, ayarlar sağdaki düğmede.
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
            Tab::Home => t!("Chat", "Sohbet"),
            Tab::Memory => t!("Memory", "Hafıza"),
            Tab::Settings => t!("Settings", "Ayarlar"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Nav(Tab),
    /// Alt sayfanın kendi alanı (sayfa kendisi işler).
    Content,
}

/// Alt sayfaların ortak durumu: ayarlar ve not klasörü. Her sayfa aynı `Rc`'yi tutar.
pub type Shared = Rc<RefCell<crate::pages::State>>;

pub struct Engram {
    hwnd: HWND,
    tab: Tab,
    pages: Vec<Box<dyn Page>>,
    voice: crate::voice::Voice,
    /// Ses notunun son değişme zamanı (değişince ses yeniden kurulur).
    note_mtime: u64,
    shared: Shared,
    hover: Hit,
    pressed: Hit,
    w: f32,
    h: f32,
    head_x: f32,
    head_r: f32,
    /// Sayfa ekranda (winloom'da seçili ve pencere açık).
    visible: bool,
    /// Bilgisayar kontrolü için winloom küçültüldü: yazılı tur bitince geri gelsin.
    restore_after: bool,
    /// Tepside gösterilecek bildirim (hatırlatma).
    notice: Option<(String, String)>,
}

impl Engram {
    /// winloom açılırken (araç açıksa) ya da araç açılınca: beyin, ses, klavye kancası ve
    /// hatırlatmalar winloom'ın penceresine bağlanır.
    pub fn start(hwnd: HWND) -> Self {
        let cfg = Config::load();
        let last_tab = cfg.tab.clone();
        let shared: Shared = Rc::new(RefCell::new(crate::pages::State::new(cfg, hwnd)));
        let pages = crate::pages::all(&shared, hwnd);
        let mut e = Engram {
            hwnd,
            tab: Tab::from_id(&last_tab).unwrap_or(Tab::Home),
            pages,
            voice: crate::voice::Voice::new(hwnd),
            note_mtime: 0,
            shared,
            hover: Hit::None,
            pressed: Hit::None,
            w: 0.0,
            h: 0.0,
            head_x: 0.0,
            head_r: 0.0,
            visible: false,
            restore_after: false,
            notice: None,
        };
        e.apply_voice();
        crate::hotkey::hook(hwnd, crate::voice::WM_KEY);
        crate::reminders::init(hwnd);
        unsafe {
            SetTimer(Some(hwnd), NOTE_TIMER, 4000, None);
        }
        log!("engram başladı: sekme {}", e.tab.id());
        e
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn save(&self) {
        let mut s = self.shared.borrow_mut();
        s.config.tab = self.tab.id().into();
        s.config.save();
    }

    /// winloom'ın penceresi ekranda mı (küçültülmemiş).
    fn window_shown(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() && !IsIconic(self.hwnd).as_bool() }
    }

    fn page(&mut self) -> &mut (dyn Page + 'static) {
        let i = Tab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0);
        self.pages[i].as_mut()
    }

    fn page_ref(&self) -> &dyn Page {
        let i = Tab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0);
        self.pages[i].as_ref()
    }

    /// Alt sayfaların görünürlüğünü bildirir: zamanlayıcılar yalnızca ekrandaki sayfada çalışır.
    fn sync_visible(&mut self) {
        let (on, tab) = (self.visible, self.tab);
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
            self.layout_page();
            self.redraw();
        }
    }

    /// winloom'ı engram'ın sayfasında öne getirir.
    fn show(&self) {
        ortak::hub::post_tab("engram");
    }

    // --- Yerleşim ---

    fn tab_labels() -> Vec<&'static str> {
        Tab::ALL.iter().map(|t| t.label()).collect()
    }

    /// Alt sayfaya boyutunu verir: sekme şeridinin altındaki alan (alt sayfa kendi köşesinden
    /// çizer, başlık bandını kullanmaz).
    fn layout_page(&mut self) {
        let (w, h) = (self.w, self.h - TABS);
        self.page().layout(w, h, PAD, w - PAD);
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return Hit::None;
        }
        if y < HEADER + TABS {
            return ortak::form::tab_at(g, &Self::tab_labels(), x, y).map_or(Hit::None, |k| Hit::Nav(Tab::ALL[k]));
        }
        Hit::Content
    }

    fn click(&mut self, hit: Hit) {
        if let Hit::Nav(t) = hit {
            self.select(t);
        }
    }

    /// Başlığın altında sade sekmeler (seçilinin altında çizgi).
    fn paint_nav(&self, g: &Gfx) {
        let selected = Tab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0);
        let hover = Tab::ALL.iter().position(|&t| self.hover == Hit::Nav(t));
        ortak::form::paint_tabs(g, &Self::tab_labels(), selected, hover, self.w);
    }

    // --- İstekler ---

    /// Alt sayfaların bıraktığı istekler: sekme değiştir, dinle, yeniden çiz.
    fn requests(&mut self) {
        let (tab, redraw, voice) = {
            let mut s = self.shared.borrow_mut();
            (s.open_tab.take(), std::mem::take(&mut s.redraw), s.voice_req.take())
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
        if redraw {
            self.redraw();
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

    /// Sesin durumunu alt sayfaların göreceği yere yazar.
    fn sync_voice(&mut self) {
        let mut s = self.shared.borrow_mut();
        s.voice_state = self.voice.state;
        s.wake_phrase = self.voice.wake_phrase();
        s.trained = self.voice.trained;
        s.train_msg = self.voice.train_msg.clone();
    }

    /// Beynin olayları: döküm tazelenir, ses ve ekran ışığı haberdar olur. Bilgisayar kontrolü
    /// başlayınca winloom önden çekilir (yazılı turda iş bitince geri gelir).
    fn on_agent(&mut self) {
        let outs = self.shared.borrow_mut().brain.on_events();
        if outs.iter().any(|o| matches!(o, crate::agent::Out::Changed(_))) {
            self.shared.borrow_mut().vault.refresh();
            self.check_note();
        }
        if outs.iter().any(|o| matches!(o, crate::agent::Out::Computer)) && self.visible && self.window_shown() {
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

    fn on(&mut self, g: &mut Gfx, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            crate::brain::WM_AGENT => self.on_agent(),
            crate::voice::WM_VOICE => {
                let shared = self.shared.clone();
                self.voice.on_events(&mut shared.borrow_mut().brain);
                self.sync_voice();
                self.redraw();
            }
            crate::voice::WM_LEVEL => {
                let v = wp.0 as f32 / 1000.0;
                self.voice.level(v);
                self.shared.borrow_mut().level = v;
            }
            crate::voice::WM_KEY => {
                let shared = self.shared.clone();
                self.voice.key(wp.0, &mut shared.borrow_mut().brain);
                self.sync_voice();
                self.redraw();
            }
            crate::voice::WM_CONFIRM => {
                let shared = self.shared.clone();
                self.voice.confirm(wp.0 == 1, &mut shared.borrow_mut().brain);
                self.sync_voice();
                self.redraw();
            }
            WM_TIMER if wp.0 == crate::voice::TIMER => {
                self.voice.tick();
                self.shared.borrow_mut().level = self.voice.out_level();
                self.sync_voice();
            }
            crate::reminders::WM_REMINDER => {
                for text in crate::reminders::take_fired() {
                    log!("hatırlatma: {text}");
                    self.notice = Some((t!("Reminder", "Hatırlatma").into(), text.clone()));
                    self.voice.announce(t!("Reminder", "Hatırlatma"), &text);
                }
                self.sync_voice();
                self.redraw();
            }
            WM_TIMER if wp.0 == NOTE_TIMER => self.check_note(),
            // Alt sayfaların kendi mesajları ve zamanlayıcıları.
            _ => return self.pages.iter_mut().find_map(|p| p.message(g, msg, wp, lp)),
        }
        Some(LRESULT(0))
    }
}

impl Drop for Engram {
    /// Araç kapatılınca: bas-konuş tuşu bırakılır, kanca ve zamanlayıcı kalkar, yerel ses durur.
    fn drop(&mut self) {
        crate::hotkey::set(None);
        crate::hotkey::unhook();
        unsafe {
            let _ = KillTimer(Some(self.hwnd), NOTE_TIMER);
        }
        crate::xtts::shutdown();
    }
}

impl ToolPage for Engram {
    /// Asistanın adı (Ayarlar'daki "Adı"): winloom'da engram bu adla görünür.
    fn title(&self) -> Option<String> {
        let name = self.shared.borrow().config.name.trim().to_string();
        (!name.is_empty()).then_some(name)
    }

    /// winloom'ın arama kutusundan: Ana alt sayfasında soru olarak gönderilir.
    fn ask(&mut self, text: &str) -> bool {
        self.select(Tab::Home);
        let ok = {
            let mut s = self.shared.borrow_mut();
            let cfg = s.config.clone();
            s.brain.send(text, crate::agent::Mode::Text, &cfg)
        };
        self.redraw();
        ok
    }

    fn summary(&self) -> Option<(String, String)> {
        let s = self.shared.borrow();
        let state = match s.voice_state {
            crate::voice::State::Listening | crate::voice::State::FollowUp => t!("Listening…", "Dinliyor…"),
            crate::voice::State::Speaking => t!("Speaking", "Konuşuyor"),
            _ if s.brain.busy => t!("Thinking…", "Düşünüyor…"),
            _ => t!("Ready", "Hazır"),
        };
        let how = if s.wake_phrase.is_empty() {
            t!(format!("Hold {} to talk", s.config.hotkey), format!("Konuşmak için {} tuşunu basılı tut", s.config.hotkey))
        } else {
            t!(format!("Say \"{}\"", s.wake_phrase), format!("\"{}\" de", s.wake_phrase))
        };
        Some((state.into(), how))
    }

    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
        self.layout_page();
    }

    fn paint(&self, g: &Gfx) {
        let p = self.page_ref();
        // Alt sayfa şeridin altına kayar; yalnızca o alana çizer.
        let (ox, oy) = g.offset();
        g.clip(Rect::new(0.0, HEADER + TABS, self.w, self.h), || {
            g.translate(ox, oy + TABS);
            p.paint(g);
            if let Some((r, text)) = p.tip() {
                let tw = g.measure(&text, &g.f.small);
                let l = ((r.l + r.r) / 2.0 - tw / 2.0 - 10.0).clamp(8.0, self.w - tw - 28.0);
                let tip = Rect::new(l, r.b + 6.0, l + tw + 20.0, r.b + 32.0);
                g.fill(tip, 4.0, pal().tip);
                g.stroke(tip, 4.0, pal().line, 1.0);
                g.text(&text, &g.f.small_center, tip, pal().text);
            }
            g.translate(ox, oy);
        });
        self.paint_nav(g);
    }

    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        let hit = self.hit(g, x, y);
        if hit != self.hover {
            if self.hover == Hit::Content {
                self.page().mouse_leave();
            }
            self.hover = hit;
            self.redraw();
        }
        if hit == Hit::Content || self.pressed == Hit::Content {
            self.page().mouse_move(g, x, y - TABS);
        }
        self.requests();
    }

    fn mouse_leave(&mut self) {
        self.page().mouse_leave();
        self.hover = Hit::None;
        self.redraw();
    }

    fn mouse_down(&mut self, g: &Gfx, x: f32, y: f32) {
        self.pressed = self.hit(g, x, y);
        if self.pressed == Hit::Content {
            self.page().mouse_down(g, x, y - TABS);
        }
        self.requests();
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        let pressed = std::mem::replace(&mut self.pressed, Hit::None);
        if pressed == Hit::Content {
            self.page().mouse_up(g, x, y - TABS);
        } else if self.hit(g, x, y) == pressed {
            self.click(pressed);
        }
        self.requests();
    }

    fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        self.page().wheel(g, x, y - TABS, delta);
        self.requests();
    }

    fn key(&mut self, vk: u16) -> bool {
        if self.page().key(vk) {
            self.requests();
            return true;
        }
        // Ctrl+Tab ve Ctrl+1..3: alt sayfalar sırayla.
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        let n = Tab::ALL.len();
        let i = Tab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0);
        match VIRTUAL_KEY(vk) {
            VK_TAB if ctrl => self.select(Tab::ALL[if shift { (i + n - 1) % n } else { (i + 1) % n }]),
            _ if ctrl && (0x31..0x31 + n as u16).contains(&vk) => self.select(Tab::ALL[(vk - 0x31) as usize]),
            _ => return false,
        }
        true
    }

    fn char(&mut self, c: char) -> bool {
        let used = self.page().char(c);
        self.requests();
        used
    }

    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        match self.hit(g, x, y) {
            Hit::Nav(_) => true,
            Hit::Content => self.page_ref().interactive(g, x, y - TABS),
            Hit::None => false,
        }
    }

    fn tip(&self) -> Option<(Rect, String)> {
        // Alt sayfanın açıklamasını engram kendisi çizer (paint).
        None
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        self.sync_visible();
    }

    fn message(&mut self, g: &mut Gfx, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        let r = self.on(g, msg, wp, lp);
        if r.is_some() {
            self.requests();
        }
        r
    }

    fn binding(&self) -> bool {
        self.pages.iter().any(|p| p.binding())
    }

    fn kill_focus(&mut self) {
        self.pages.iter_mut().for_each(|p| p.kill_focus());
    }

    fn add_files(&mut self, paths: Vec<PathBuf>) {
        self.page().add_files(paths);
        self.requests();
    }

    fn take_notice(&mut self) -> Option<(String, String)> {
        self.notice.take()
    }
}

/// Pencerede olmayan yerden (sayfaların iş parçacıkları) yeniden çizim.
pub fn redraw(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}
