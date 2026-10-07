//! lyrebird sayfası. Motor (çözme, ortak bellek, kulaklık, kısayollar) ../lyrebird'de ve
//! hive sürecinde çalışır; mikrofon efekti (APO) audiodg.exe içinde.
//!
//! Sayfa: üstte mikrofon durumu ve "hepsini sustur", altında iki sekme, en altta iki ses düzeyi.
//! - Seslerim: listedeki sesler. Sağ üstte "Son 10 saniye" (bilgisayarda az önce çalanı listeye
//!   ekler, kısayolu da var) ve "Dosya ekle".
//! - Ses bul: Myinstants araması. Sonuca tıklamak onu kulaklıkta çalar (yalnızca sana, mikrofona
//!   gitmez), "Ekle" listeye indirir.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::Media::Multimedia::mciSendStringW;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::gfx::{Color, Gfx, Rect};
use crate::log;
use crate::ui::*;
use crate::util::{Res, error_box, wide};
use crate::{myinstants, osd};
use lyrebird_motor::bus::{self, Bus};
use lyrebird_motor::clip::{self, Clipper};
use lyrebird_motor::config::{self, Config, Sound};
use lyrebird_motor::decode::EXTENSIONS;
use lyrebird_motor::hotkey::{self, Hotkey};
use lyrebird_motor::install::Mic;
use lyrebird_motor::monitor::Monitor;
use lyrebird_motor::player::{Player, Playing};
pub use lyrebird_motor::{data_dir, install, installed, probe};


pub const WM_PLAYER: u32 = WM_APP + 20;
pub const WM_INSTALLED: u32 = WM_APP + 21;
/// Myinstants araması bitti / bir ses indi.
pub const WM_SEARCHED: u32 = WM_APP + 22;
pub const WM_FETCHED: u32 = WM_APP + 23;
/// MCI önizlemesi bitti (wParam: MCI_NOTIFY_*).
pub const MM_MCINOTIFY: u32 = 0x3B9;
const MCI_NOTIFY_SUCCESSFUL: usize = 1;
pub const TIMER_ANIM: usize = 101;
pub const TIMER_STATUS: usize = 102;
/// Yazmayı bırakınca arama.
pub const TIMER_SEARCH: usize = 103;
const SEARCH_DELAY: u32 = 450;
/// Kısayol kimlikleri: durdur, son 10 saniye ve her ses (1100 + sıra).
pub const HK_STOP: i32 = 1001;
pub const HK_CLIP: i32 = 1002;
pub const HK_SOUND: i32 = 1100;

/// Yükseltilmiş kurulumun komut satırı bayrakları.
pub const ARG_INSTALL: &str = "--lyrebird-kur";
pub const ARG_UNINSTALL: &str = "--lyrebird-kaldir";

const ICON_SEARCH: &str = "\u{E721}";
const ICON_CLIP: &str = "\u{E7C8}";
const ICON_CHECK: &str = "\u{E73E}";
const ICON_STOP: &str = "\u{E71A}";

const BOTTOM: f32 = 60.0;
/// Sekme çubuğu ve altındaki içeriğin başladığı yer.
const TABS_T: f32 = HEADER + 10.0;
const TABS_H: f32 = 34.0;
const BODY_T: f32 = TABS_T + TABS_H + 10.0;
const ROW: f32 = 40.0;
const SEARCH_H: f32 = 40.0;
/// Arama kutusunun en çok karakteri.
const QUERY_MAX: usize = 60;

/// `--lyrebird-kur` / `--lyrebird-kaldir`: yükseltilmiş olarak çalışır, çıkış kodu sonucu bildirir.
pub fn setup(install: bool) -> i32 {
    match lyrebird_motor::setup(install) {
        Ok(()) => {
            log!("lyrebird {} tamam", if install { "kurulumu" } else { "kaldırma" });
            0
        }
        Err(e) => {
            log!("lyrebird kurulum hatası: {e}");
            println!("{} {e}", t!("ERROR", "HATA"));
            1
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tab {
    Mine,
    Find,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Status,
    Stop,
    StopKey,
    Tab(Tab),
    AddFiles,
    Clip,
    ClipKey,
    Row(usize),
    RowKey(usize),
    RowRemove(usize),
    Slider(usize),
    /// Boş listedeki "Ses bul".
    EmptyFind,
    Search,
    SearchClear,
    Found(usize),
    FoundAdd(usize),
}

#[derive(Clone, Copy, PartialEq)]
enum Bind {
    Stop,
    Clip,
    Sound(usize),
}

enum MicState {
    Unknown,
    Missing,
    Detached,
    Attached(String),
    Busy,
}

/// Pencere sahibinin borç dışında çalıştırması gereken kalıcı işler (iç içe mesaj döngüsü).
pub enum Modal {
    AddFiles,
}

struct Item {
    id: u64,
    path: PathBuf,
    name: String,
    hotkey: Option<Hotkey>,
    conflict: bool,
    missing: bool,
}

impl Item {
    fn new(id: u64, s: Sound) -> Self {
        let name =
            s.path.file_stem().map_or_else(|| s.path.display().to_string(), |n| n.to_string_lossy().into_owned());
        Self { id, missing: !s.path.is_file(), path: s.path, name, hotkey: s.hotkey, conflict: false }
    }
}

/// Myinstants sonucu.
struct Found {
    name: String,
    url: String,
    /// Önizleme ya da ekleme için iniyor.
    loading: bool,
    added: bool,
}

enum FindState {
    Idle,
    Loading,
    Done,
    Error(String),
}

/// Arka plandaki bir indirme: hangi ses, listeye mi (yoksa önizleme), sonuç.
struct Fetched {
    url: String,
    add: bool,
    result: Result<PathBuf, String>,
}

fn sounds_dir() -> PathBuf {
    data_dir().join("sesler")
}

fn clips_dir() -> PathBuf {
    data_dir().join("klipler")
}

/// Dinlenen Myinstants sesleri; sekmeden çıkınca silinir.
fn preview_dir() -> PathBuf {
    data_dir().join("onizleme")
}

fn preview_path(url: &str) -> PathBuf {
    preview_dir().join(myinstants::file_name(url.rsplit('/').next().unwrap_or("ses.mp3")))
}

/// Myinstants sesinin listedeki dosyası.
fn target(name: &str) -> PathBuf {
    sounds_dir().join(format!("{}.mp3", myinstants::file_name(name)))
}

fn mci(cmd: &str, notify: Option<HWND>) -> bool {
    let cmd = wide(cmd);
    unsafe { mciSendStringW(PCWSTR(cmd.as_ptr()), None, notify) == 0 }
}

const PREVIEW: &str = "lyrebird_onizleme";

pub struct Lyrebird {
    hwnd: HWND,
    bus: &'static Bus,
    player: Player,
    monitor: Monitor,
    items: Vec<Item>,
    next_id: u64,
    level: [u32; 2],
    stop: Option<Hotkey>,
    stop_conflict: bool,
    playing: Vec<Playing>,
    mic: MicState,
    active: bool,
    hover: Hit,
    pressed: Hit,
    drag: Option<usize>,
    bind: Option<Bind>,
    scroll: f32,
    registered: Vec<i32>,
    modal: Option<Modal>,
    dll_current: bool,
    /// Sayfanın boyutu ve başlık yazısının bittiği x (hive her olaydan önce verir).
    w: f32,
    h: f32,
    head_x: f32,
    head_r: f32,
    /// Sayfa ekranda mı (zamanlayıcılar yalnızca o zaman çalışır).
    visible: bool,
    tab: Tab,
    /// Son 10 saniye: bilgisayarın sesini (yalnızca ses çalarken) izleyen motor.
    clipper: Clipper,
    clip: Option<Hotkey>,
    clip_conflict: bool,
    /// Ses bul.
    query: String,
    /// Ctrl+A: yazılan her şey seçili (yeni yazı yerine geçer).
    selected: bool,
    found: Vec<Found>,
    find_state: FindState,
    find_scroll: f32,
    search_seq: u64,
    searched: Arc<Mutex<Option<(u64, Result<Vec<myinstants::Instant>, String>)>>>,
    fetched: Arc<Mutex<Vec<Fetched>>>,
    /// Kulaklıkta çalan önizleme ve beklenen (inerken).
    preview: Option<String>,
    want_preview: Option<String>,
}

fn is_audio(p: &Path) -> bool {
    p.extension().is_some_and(|e| EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn post(hwnd: usize, msg: u32, wp: usize) {
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(wp), LPARAM(0));
    }
}

/// Ortak bellek açılamazsa (klasör izinleri vb.) süreç içi bellek: kulaklık yine çalışır.
fn open_bus() -> &'static Bus {
    match bus::create() {
        Ok(b) => b,
        Err(e) => {
            log!("lyrebird ortak belleği açılamadı, yalnızca kulaklık çalışacak: {e}");
            let layout = std::alloc::Layout::new::<Bus>();
            unsafe { &*(std::alloc::alloc_zeroed(layout) as *const Bus) }
        }
    }
}

impl Lyrebird {
    pub fn start(hwnd: HWND) -> Self {
        let cfg = Config::load();
        let bus = open_bus();
        bus.reset(config::gain(cfg.mic));
        let monitor = Monitor::start(bus, config::gain(cfg.ear));
        let h = hwnd.0 as usize;
        let player = Player::start(bus, move || post(h, WM_PLAYER, 0), monitor.waker());
        let items: Vec<Item> = cfg.sounds.into_iter().enumerate().map(|(i, s)| Item::new(i as u64 + 1, s)).collect();
        let mut l = Self {
            hwnd,
            bus,
            player,
            monitor,
            next_id: items.len() as u64,
            items,
            level: [cfg.mic, cfg.ear],
            stop: cfg.stop,
            stop_conflict: false,
            playing: Vec::new(),
            mic: MicState::Unknown,
            active: false,
            hover: Hit::None,
            pressed: Hit::None,
            drag: None,
            bind: None,
            scroll: 0.0,
            registered: Vec::new(),
            modal: None,
            dll_current: install::current(),
            w: 0.0,
            h: 0.0,
            head_x: 0.0,
            head_r: 0.0,
            visible: false,
            tab: Tab::Mine,
            clipper: Clipper::start(),
            clip: cfg.clip,
            clip_conflict: false,
            query: String::new(),
            selected: false,
            found: Vec::new(),
            find_state: FindState::Idle,
            find_scroll: 0.0,
            search_seq: 0,
            searched: Arc::default(),
            fetched: Arc::default(),
            preview: None,
            want_preview: None,
        };
        l.register();
        l.refresh_mic();
        log!("lyrebird: {} ses, mikrofon {}, kulaklık {}", l.items.len(), l.level[0], l.level[1]);
        l
    }

    pub fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
        self.scroll = self.scroll.min(self.max_scroll());
        self.find_scroll = self.find_scroll.min(self.find_max_scroll());
    }

    pub fn take_modal(&mut self) -> Option<Modal> {
        self.modal.take()
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn save(&self) {
        Config {
            mic: self.level[0],
            ear: self.level[1],
            stop: self.stop,
            clip: self.clip,
            sounds: self.items.iter().map(|i| Sound { path: i.path.clone(), hotkey: i.hotkey }).collect(),
        }
        .save();
    }

    // --- Kısayollar ---

    fn unregister(&mut self) {
        for id in self.registered.drain(..) {
            unsafe {
                let _ = UnregisterHotKey(Some(self.hwnd), id);
            }
        }
    }

    /// Hepsini baştan kaydeder. Başka bir uygulamanın tuttuğu kısayol kırmızı görünür.
    fn register(&mut self) {
        self.unregister();
        let mut reg = |id: i32, h: Option<Hotkey>| -> bool {
            let Some(h) = h else { return false };
            let ok = unsafe { RegisterHotKey(Some(self.hwnd), id, h.mods | MOD_NOREPEAT, h.vk as u32) }.is_ok();
            if ok {
                self.registered.push(id);
            }
            !ok
        };
        self.stop_conflict = reg(HK_STOP, self.stop);
        self.clip_conflict = reg(HK_CLIP, self.clip);
        let keys: Vec<_> = self.items.iter().map(|i| i.hotkey).collect();
        let conflicts: Vec<bool> = keys.iter().enumerate().map(|(n, &h)| reg(HK_SOUND + n as i32, h)).collect();
        for (item, c) in self.items.iter_mut().zip(conflicts) {
            item.conflict = c;
        }
    }

    fn start_bind(&mut self, b: Bind) {
        self.unregister();
        self.bind = Some(b);
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
        self.redraw();
    }

    fn end_bind(&mut self, key: Option<Option<Hotkey>>) {
        let Some(b) = self.bind.take() else { return };
        if let Some(key) = key {
            // Aynı kısayol başka yerde varsa oradan alınır.
            if key.is_some() {
                if self.stop == key {
                    self.stop = None;
                }
                if self.clip == key {
                    self.clip = None;
                }
                for i in &mut self.items {
                    if i.hotkey == key {
                        i.hotkey = None;
                    }
                }
            }
            match b {
                Bind::Stop => self.stop = key,
                Bind::Clip => self.clip = key,
                Bind::Sound(n) => {
                    if let Some(i) = self.items.get_mut(n) {
                        i.hotkey = key;
                    }
                }
            }
            self.save();
        }
        self.register();
        self.redraw();
    }

    // --- Sesler ---

    pub fn add(&mut self, paths: Vec<PathBuf>) {
        let mut files = Vec::new();
        for p in paths {
            if p.is_dir() {
                let mut inner: Vec<PathBuf> = std::fs::read_dir(&p)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| is_audio(p))
                    .collect();
                inner.sort();
                files.extend(inner);
            } else if is_audio(&p) {
                files.push(p);
            }
        }
        let before = self.items.len();
        for p in files {
            if !self.items.iter().any(|i| i.path == p) {
                self.next_id += 1;
                self.items.push(Item::new(self.next_id, Sound { path: p, hotkey: None }));
            }
        }
        if self.items.len() != before {
            self.save();
            self.scroll = self.max_scroll();
            self.redraw();
        }
    }

    fn remove(&mut self, n: usize) {
        if n >= self.items.len() {
            return;
        }
        let item = self.items.remove(n);
        if self.playing.iter().any(|p| p.id == item.id) {
            self.player.toggle(item.id, item.path.clone());
        }
        if let Some(f) = self.found.iter_mut().find(|f| target(&f.name) == item.path) {
            f.added = false;
        }
        self.register();
        self.save();
        self.hover = Hit::None;
        self.redraw();
    }

    fn toggle(&mut self, n: usize) {
        let Some(item) = self.items.get_mut(n) else {
            return;
        };
        item.missing = !item.path.is_file();
        if item.missing {
            self.redraw();
            return;
        }
        self.player.toggle(item.id, item.path.clone());
    }

    fn set_level(&mut self, k: usize, v: u32) {
        let v = v.min(100);
        if self.level[k] == v {
            return;
        }
        self.level[k] = v;
        match k {
            0 => self.bus.set_gain(config::gain(v)),
            _ => self.monitor.set_gain(config::gain(v)),
        }
        self.redraw();
    }

    /// Bilgisayarda duyulan son 10 saniyeyi listeye ekler; ekranın altında söyler.
    fn save_clip(&mut self) {
        let t = unsafe { GetLocalTime() };
        let name = format!("{} {:02}.{:02}.{:02}", t!("Clip", "Klip"), t.wHour, t.wMinute, t.wSecond);
        let path = clips_dir().join(format!("{name}.wav"));
        match self.clipper.save(&path) {
            Ok(Some(secs)) => {
                self.add(vec![path]);
                let text = format!("{} · {:.0} {}", t!("Clip added", "Klip eklendi"), secs.max(1.0), t!("s", "sn"));
                osd::show(ICON_CLIP, 0xfbbf24, &text);
            }
            Ok(None) => {
                let text = format!(
                    "{} {} {}",
                    t!("Nothing heard in the last", "Son"),
                    clip::SECONDS,
                    t!("seconds", "saniyede ses yok")
                );
                osd::show(ICON_CLIP, 0x8b8b93, &text);
            }
            Err(e) => {
                log!("klip kaydedilemedi: {e}");
                osd::show(ICON_CLIP, osd::RED, t!("Could not save the clip", "Klip kaydedilemedi"));
            }
        }
    }

    // --- Mikrofon durumu ---

    fn refresh_mic(&mut self) {
        if matches!(self.mic, MicState::Busy) {
            return;
        }
        self.mic = match install::default_mic() {
            None => MicState::Missing,
            Some(Mic { guid, name, .. }) if self.dll_current && install::attached(&guid) => MicState::Attached(name),
            Some(_) => MicState::Detached,
        };
        self.active = self.bus.since_beat() < 1.0;
        self.redraw();
    }

    /// Mikrofona bağlar ya da ayırır (yönetici izni ister). Bitince WM_INSTALLED gelir.
    pub fn run_setup(&mut self, install: bool) {
        self.mic = MicState::Busy;
        self.redraw();
        let hwnd = self.hwnd.0 as usize;
        let arg = if install { ARG_INSTALL } else { ARG_UNINSTALL };
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            let code = match install::run_elevated(arg) {
                Ok(true) => 0,
                Ok(false) => 1,
                // Kullanıcı yönetici iznini reddetti.
                Err(_) => 2,
            };
            post(hwnd, WM_INSTALLED, code);
        });
    }

    // --- Sekmeler ---

    fn set_tab(&mut self, tab: Tab) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.hover = Hit::None;
        self.selected = false;
        if tab == Tab::Find {
            if matches!(self.find_state, FindState::Idle) {
                self.search_now();
            }
            unsafe {
                let _ = SetFocus(Some(self.hwnd));
            }
        } else {
            self.stop_preview();
            let _ = std::fs::remove_dir_all(preview_dir());
        }
        self.redraw();
    }

    // --- Yerleşim (sayfanın kendi köşesine göre) ---

    fn stop_rect(&self) -> Rect {
        Rect::new(self.head_r - 36.0, HEAD_CY - 18.0, self.head_r, HEAD_CY + 18.0)
    }

    fn chip_text(&self, key: Option<Hotkey>, binding: bool) -> String {
        match (binding, key) {
            (true, _) => t!("press a key", "tuşa bas").into(),
            (false, Some(k)) => k.to_string(),
            (false, None) => t!("shortcut", "kısayol").into(),
        }
    }

    /// Kısayol kutucuğunun renkleri: (yazı, çerçeve).
    fn chip_colors(&self, binding: bool, conflict: bool, unset: bool, hovered: bool) -> (Color, Color) {
        match () {
            _ if binding => (accent(), accent()),
            _ if conflict => (RED, LINE),
            _ if unset => (FAINT, LINE),
            _ if hovered => (TEXT, FAINT),
            _ => (MUTED, LINE),
        }
    }

    fn stop_key_rect(&self, g: &Gfx) -> Rect {
        let text = self.chip_text(self.stop, self.bind == Some(Bind::Stop));
        chip_rect(g, self.head_r - 42.0, HEAD_CY, &text)
    }

    /// Başlıktaki mikrofon rozeti: bağlıyken "Bağlı" (ad balonda), değilken "Mikrofona bağla"
    /// düğmesi. Tıklanabilir alan yalnızca rozetin kendisi.
    fn pill_label(&self) -> &'static str {
        match &self.mic {
            MicState::Unknown => "",
            MicState::Missing => t!("No microphone", "Mikrofon yok"),
            MicState::Busy => t!("Connecting…", "Bağlanıyor…"),
            MicState::Detached => t!("Connect microphone", "Mikrofona bağla"),
            MicState::Attached(_) => t!("Connected", "Bağlı"),
        }
    }

    fn pill_rect(&self, g: &Gfx) -> Rect {
        let label = self.pill_label();
        let w = if matches!(self.mic, MicState::Detached) {
            g.measure(label, &g.f.button) + 28.0
        } else {
            g.measure(label, &g.f.small) + 38.0
        };
        let l = self.head_x + 14.0;
        Rect::new(l, HEAD_CY - 13.0, l + w, HEAD_CY + 13.0)
    }

    fn tab_label(&self, tab: Tab) -> String {
        match tab {
            Tab::Mine if self.items.is_empty() => t!("My sounds", "Seslerim").into(),
            Tab::Mine => format!("{}  {}", t!("My sounds", "Seslerim"), self.items.len()),
            Tab::Find => t!("Find sounds", "Ses bul").into(),
        }
    }

    fn tab_rects(&self, g: &Gfx) -> [Rect; 2] {
        let mut x = 16.0 + 3.0;
        [Tab::Mine, Tab::Find].map(|t| {
            let w = g.measure(&self.tab_label(t), &g.f.button) + 32.0;
            let r = Rect::new(x, TABS_T + 3.0, x + w, TABS_T + TABS_H - 3.0);
            x += w;
            r
        })
    }

    /// Seslerim sekmesinin sağındaki düğmeler: (dosya ekle, son 10 saniye, kısayolu). Yer
    /// yoksa kısayol kutucuğu gizlenir.
    fn mine_tools(&self, g: &Gfx) -> (Rect, Rect, Option<Rect>) {
        let t = TABS_T;
        let add = button_rect_right(g, self.w - 16.0, t, t!("Add files", "Dosya ekle"), true);
        let clip = button_rect_right(g, add.l - 8.0, t, &self.clip_label(), true);
        let text = self.chip_text(self.clip, self.bind == Some(Bind::Clip));
        let key = chip_rect(g, clip.l - 8.0, clip.cy(), &text);
        let tabs_r = self.tab_rects(g)[1].r + 12.0;
        (add, clip, (key.l > tabs_r).then_some(key))
    }

    fn clip_label(&self) -> String {
        format!("{} {} {}", t!("Last", "Son"), clip::SECONDS, t!("seconds", "saniye"))
    }

    fn body(&self) -> Rect {
        Rect::new(0.0, BODY_T, self.w, self.h - BOTTOM)
    }

    fn max_scroll(&self) -> f32 {
        let body = self.body();
        (self.items.len() as f32 * ROW + 10.0 - (body.b - body.t)).max(0.0)
    }

    /// Boş listede ortadaki düğmeler: (dosya ekle, ses bul).
    fn empty_rects(&self, g: &Gfx) -> (Rect, Rect) {
        let cy = self.body().cy();
        let add = button_rect(g, 0.0, cy + 34.0, t!("Add files", "Dosya ekle"), true);
        let find = button_rect(g, 0.0, cy + 34.0, t!("Find sounds", "Ses bul"), true);
        let l = self.w / 2.0 - (add.w() + 10.0 + find.w()) / 2.0;
        (
            Rect::new(l, add.t, l + add.w(), add.b),
            Rect::new(l + add.w() + 10.0, find.t, l + add.w() + 10.0 + find.w(), find.b),
        )
    }

    fn row_rect(&self, n: usize) -> Rect {
        let t = BODY_T + n as f32 * ROW - self.scroll;
        Rect::new(16.0, t + 1.0, self.w - 16.0, t + ROW - 1.0)
    }

    fn row_key_rect(&self, g: &Gfx, n: usize) -> Rect {
        let item = &self.items[n];
        let text = self.chip_text(item.hotkey, self.bind == Some(Bind::Sound(n)));
        chip_rect(g, self.w - 56.0, self.row_rect(n).cy(), &text)
    }

    fn remove_rect(&self, n: usize) -> Rect {
        let r = self.row_rect(n);
        Rect::new(self.w - 50.0, r.t + 6.0, self.w - 22.0, r.b - 6.0)
    }

    fn search_rect(&self) -> Rect {
        Rect::new(16.0, BODY_T, self.w - 16.0, BODY_T + SEARCH_H)
    }

    fn search_clear_rect(&self) -> Rect {
        let b = self.search_rect();
        Rect::new(b.r - 36.0, b.t + 6.0, b.r - 8.0, b.b - 6.0)
    }

    /// Sonuç listesinin üstü (kaydırılmamış).
    fn found_top(&self) -> f32 {
        BODY_T + SEARCH_H + 10.0
    }

    fn found_rect(&self, n: usize) -> Rect {
        let t = self.found_top() + n as f32 * ROW - self.find_scroll;
        Rect::new(16.0, t + 1.0, self.w - 16.0, t + ROW - 1.0)
    }

    fn found_add_rect(&self, g: &Gfx, n: usize) -> Rect {
        let r = self.found_rect(n);
        let b = button_rect_right(g, r.r - 6.0, 0.0, t!("Add", "Ekle"), true);
        Rect::new(b.l, r.cy() - 14.0, b.r, r.cy() + 14.0)
    }

    fn find_max_scroll(&self) -> f32 {
        (self.found.len() as f32 * ROW + 10.0 - (self.h - BOTTOM - self.found_top())).max(0.0)
    }

    /// (bölge, iz başı, iz sonu)
    fn slider(&self, k: usize) -> (Rect, f32, f32) {
        let (w, h) = (self.w, self.h);
        let (l, r) = if k == 0 { (PAD, w / 2.0 - 10.0) } else { (w / 2.0 + 10.0, w - PAD) };
        (Rect::new(l, h - BOTTOM + 8.0, r, h - 8.0), l + 30.0, r - 34.0)
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return if self.stop_rect().contains(x, y) {
                Hit::Stop
            } else if self.stop_key_rect(g).contains(x, y) {
                Hit::StopKey
            } else if !self.pill_label().is_empty() && self.pill_rect(g).contains(x, y) {
                Hit::Status
            } else {
                Hit::None
            };
        }
        if y >= self.h - BOTTOM {
            return (0..2).find(|&k| self.slider(k).0.contains(x, y)).map_or(Hit::None, Hit::Slider);
        }
        if y < BODY_T {
            let [mine, find] = self.tab_rects(g);
            if mine.contains(x, y) {
                return Hit::Tab(Tab::Mine);
            }
            if find.contains(x, y) {
                return Hit::Tab(Tab::Find);
            }
            if self.tab == Tab::Mine {
                let (add, clip, key) = self.mine_tools(g);
                return match () {
                    _ if add.contains(x, y) => Hit::AddFiles,
                    _ if clip.contains(x, y) => Hit::Clip,
                    _ if key.is_some_and(|k| k.contains(x, y)) => Hit::ClipKey,
                    _ => Hit::None,
                };
            }
            return Hit::None;
        }
        match self.tab {
            Tab::Mine => self.mine_hit(g, x, y),
            Tab::Find => self.find_hit(g, x, y),
        }
    }

    fn mine_hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if self.items.is_empty() {
            let (add, find) = self.empty_rects(g);
            return match () {
                _ if add.contains(x, y) => Hit::AddFiles,
                _ if find.contains(x, y) => Hit::EmptyFind,
                _ => Hit::None,
            };
        }
        let n = ((y - BODY_T + self.scroll) / ROW).floor();
        if n < 0.0 || n as usize >= self.items.len() {
            return Hit::None;
        }
        let n = n as usize;
        if self.remove_rect(n).contains(x, y) {
            Hit::RowRemove(n)
        } else if self.row_key_rect(g, n).contains(x, y) {
            Hit::RowKey(n)
        } else {
            Hit::Row(n)
        }
    }

    fn find_hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        let b = self.search_rect();
        if b.contains(x, y) {
            return if !self.query.is_empty() && self.search_clear_rect().contains(x, y) {
                Hit::SearchClear
            } else {
                Hit::Search
            };
        }
        if y < self.found_top() {
            return Hit::None;
        }
        let n = ((y - self.found_top() + self.find_scroll) / ROW).floor();
        if n < 0.0 || n as usize >= self.found.len() {
            return Hit::None;
        }
        let n = n as usize;
        if self.found_add_rect(g, n).contains(x, y) { Hit::FoundAdd(n) } else { Hit::Found(n) }
    }

    fn slide_to(&mut self, k: usize, x: f32) {
        let (_, a, b) = self.slider(k);
        self.set_level(k, (((x - a) / (b - a)).clamp(0.0, 1.0) * 100.0).round() as u32);
    }

    // --- Çizim ---

    pub fn paint(&self, g: &Gfx) {
        let (w, h) = (self.w, self.h);
        self.paint_header(g);
        self.paint_tabs(g);
        match self.tab {
            Tab::Mine => self.paint_mine(g),
            Tab::Find => self.paint_find(g),
        }

        // Alt çubuk: mikrofon ve kulaklık düzeyi.
        g.fill(Rect::new(0.0, h - BOTTOM, w, h - BOTTOM + 1.0), 0.0, LINE);
        for k in 0..2 {
            let (r, a, b) = self.slider(k);
            let active = self.drag == Some(k) || self.hover == Hit::Slider(k);
            g.text(
                if k == 0 { ICON_MIC } else { ICON_EAR },
                &g.f.icon,
                Rect::new(r.l, r.t, r.l + 20.0, r.b),
                if active { TEXT } else { MUTED },
            );
            slider(g, a, b, r.cy(), self.level[k] as f32 / 100.0, accent(), active);
            g.text(
                &self.level[k].to_string(),
                &g.f.small_right,
                Rect::new(b + 6.0, r.t, r.r, r.b),
                if active { TEXT } else { MUTED },
            );
        }
    }

    /// Üst çubuk: mikrofon rozeti, hepsini sustur ve kısayolu.
    fn paint_header(&self, g: &Gfx) {
        let label = self.pill_label();
        if !label.is_empty() {
            let p = self.pill_rect(g);
            let hovered = self.hover == Hit::Status;
            if matches!(self.mic, MicState::Detached) {
                g.fill(p, 13.0, accent().alpha(if hovered { 0.88 } else { 1.0 }));
                g.text(label, &g.f.button, p, on_accent());
            } else {
                g.fill(p, 13.0, HOVER);
                let dot = match &self.mic {
                    MicState::Attached(_) if self.active => GREEN,
                    MicState::Busy => accent(),
                    _ => FAINT,
                };
                g.circle(p.l + 14.0, p.cy(), 3.5, dot);
                g.text(label, &g.f.small, Rect::new(p.l + 24.0, p.t, p.r, p.b), if hovered { TEXT } else { MUTED });
            }
        }
        let playing_any = !self.playing.is_empty();
        icon_button(g, self.stop_rect(), ICON_MUTE, if playing_any { accent() } else { MUTED }, self.hover == Hit::Stop);
        let binding = self.bind == Some(Bind::Stop);
        if self.stop.is_some() || binding || matches!(self.hover, Hit::Stop | Hit::StopKey) {
            let (c, b) = self.chip_colors(binding, self.stop_conflict, self.stop.is_none(), false);
            chip(g, self.stop_key_rect(g), &self.chip_text(self.stop, binding), c, b);
        }
    }

    /// Sekmeler ve seçili sekmenin düğmeleri.
    fn paint_tabs(&self, g: &Gfx) {
        let [mine, find] = self.tab_rects(g);
        g.fill(Rect::new(mine.l - 3.0, mine.t - 3.0, find.r + 3.0, find.b + 3.0), 8.0, HOVER);
        for (r, t) in [(mine, Tab::Mine), (find, Tab::Find)] {
            let selected = self.tab == t;
            if selected {
                g.fill(r, 6.0, SEL);
            }
            let fg = if selected || self.hover == Hit::Tab(t) { TEXT } else { MUTED };
            g.text(&self.tab_label(t), &g.f.button, r, fg);
            if selected {
                g.fill(Rect::new(r.l + 12.0, r.b - 2.0, r.r - 12.0, r.b), 1.0, accent());
            }
        }
        if self.tab == Tab::Mine {
            let (add, clip, key) = self.mine_tools(g);
            button(g, add, t!("Add files", "Dosya ekle"), Some(ICON_ADD), None, self.hover == Hit::AddFiles);
            button(g, clip, &self.clip_label(), Some(ICON_CLIP), None, self.hover == Hit::Clip);
            if let Some(k) = key {
                let binding = self.bind == Some(Bind::Clip);
                let (c, b) =
                    self.chip_colors(binding, self.clip_conflict, self.clip.is_none(), self.hover == Hit::ClipKey);
                chip(g, k, &self.chip_text(self.clip, binding), c, b);
            }
        }
    }

    fn paint_mine(&self, g: &Gfx) {
        let w = self.w;
        let list = self.body();
        if self.items.is_empty() {
            let cy = list.cy();
            g.text(ICON_AUDIO, &g.f.icon_large, Rect::new(0.0, cy - 62.0, w, cy - 22.0), FAINT);
            let title = t!("No sounds yet", "Henüz ses yok");
            text_center(g, title, &g.f.strong, w / 2.0, cy - 14.0, cy + 8.0, TEXT);
            let kinds = t!(
                "drop sound files here, or find some on Myinstants",
                "ses dosyalarını buraya sürükle ya da Myinstants'ta bul"
            );
            g.text(kinds, &g.f.small_center, Rect::new(0.0, cy + 8.0, w, cy + 26.0), MUTED);
            let (add, find) = self.empty_rects(g);
            button(g, add, t!("Add files", "Dosya ekle"), Some(ICON_ADD), Some(accent()), self.hover == Hit::AddFiles);
            button(g, find, t!("Find sounds", "Ses bul"), Some(ICON_SEARCH), None, self.hover == Hit::EmptyFind);
            return;
        }
        g.clip(list, || {
            for (n, item) in self.items.iter().enumerate() {
                let r = self.row_rect(n);
                if r.b < list.t || r.t > list.b {
                    continue;
                }
                let hovered = matches!(self.hover, Hit::Row(i) | Hit::RowKey(i) | Hit::RowRemove(i) if i == n);
                let playing = self.playing.iter().find(|p| p.id == item.id);
                if playing.is_some() {
                    g.fill(r, 8.0, accent().alpha(if hovered { 0.12 } else { 0.08 }));
                } else if hovered {
                    g.fill(r, 8.0, HOVER);
                }
                let binding = self.bind == Some(Bind::Sound(n));
                let key = self.row_key_rect(g, n);
                let show_chip = item.hotkey.is_some() || binding || hovered;
                let name_right = if show_chip { key.l - 10.0 } else { w - 56.0 };
                let (icon, icon_color) = if playing.is_some() {
                    (ICON_PAUSE, accent())
                } else {
                    (ICON_PLAY, if hovered { MUTED } else { FAINT })
                };
                g.text(icon, &g.f.icon_small, Rect::new(r.l + 8.0, r.t, r.l + 28.0, r.b), icon_color);
                let name_color = match () {
                    _ if item.missing => FAINT,
                    _ if playing.is_some() => accent(),
                    _ => TEXT,
                };
                g.text(&item.name, &g.f.text, Rect::new(r.l + 36.0, r.t, name_right, r.b), name_color);
                if show_chip {
                    let (c, b) =
                        self.chip_colors(binding, item.conflict, item.hotkey.is_none(), self.hover == Hit::RowKey(n));
                    chip(g, key, &self.chip_text(item.hotkey, binding), c, b);
                }
                if hovered {
                    let rm = self.remove_rect(n);
                    let over = self.hover == Hit::RowRemove(n);
                    if over {
                        g.fill(rm, 6.0, LINE);
                    }
                    g.text(ICON_REMOVE, &g.f.icon_small, rm, if over { TEXT } else { FAINT });
                }
                if let Some(p) = playing
                    && p.duration > 0.0
                {
                    let t = (p.started.elapsed().as_secs_f64() / p.duration).clamp(0.0, 1.0) as f32;
                    let (a, b) = (r.l + 10.0, r.r - 10.0);
                    g.fill(Rect::new(a, r.b - 3.0, a + (b - a) * t, r.b - 1.0), 1.0, accent().alpha(0.7));
                }
            }
        });
    }

    fn paint_find(&self, g: &Gfx) {
        let (w, h) = (self.w, self.h);

        // Arama kutusu: yazılan her şey buraya gelir.
        let b = self.search_rect();
        g.fill(b, 8.0, HOVER);
        g.stroke(b, 8.0, accent().alpha(0.6), 1.0);
        g.text(ICON_SEARCH, &g.f.icon_small, Rect::new(b.l + 12.0, b.t, b.l + 32.0, b.b), MUTED);
        let tx = b.l + 40.0;
        let text_r = if self.query.is_empty() { b.r - 12.0 } else { self.search_clear_rect().l - 6.0 };
        let tw = g.measure(&self.query, &g.f.text);
        if self.query.is_empty() {
            g.text(
                t!("Search Myinstants…", "Myinstants'ta ara…"),
                &g.f.text,
                Rect::new(tx + 4.0, b.t, text_r, b.b),
                FAINT,
            );
        } else {
            if self.selected {
                g.fill(
                    Rect::new(tx - 2.0, b.cy() - 11.0, (tx + tw + 2.0).min(text_r), b.cy() + 11.0),
                    3.0,
                    accent().alpha(0.35),
                );
            }
            g.text(&self.query, &g.f.text, Rect::new(tx, b.t, text_r, b.b), TEXT);
            let c = self.search_clear_rect();
            icon_button(g, c, ICON_REMOVE, FAINT, self.hover == Hit::SearchClear);
        }
        if !self.selected {
            let cx = (tx + tw + 1.0).min(text_r);
            g.fill(Rect::new(cx, b.cy() - 9.0, cx + 1.5, b.cy() + 9.0), 0.0, accent());
        }

        // Sonuçlar ya da durum.
        let area = Rect::new(0.0, self.found_top() - 2.0, w, h - BOTTOM);
        let message = match &self.find_state {
            FindState::Error(e) => Some((e.as_str(), RED)),
            FindState::Loading if self.found.is_empty() => Some((t!("Searching…", "Aranıyor…"), MUTED)),
            FindState::Done if self.found.is_empty() => Some((t!("Nothing found", "Bir şey bulunamadı"), MUTED)),
            _ => None,
        };
        if let Some((text, color)) = message {
            g.text(text, &g.f.small_center, Rect::new(16.0, area.t + 20.0, w - 16.0, area.t + 44.0), color);
            if !matches!(self.find_state, FindState::Error(_)) {
                return;
            }
        }
        g.clip(area, || {
            for (n, f) in self.found.iter().enumerate() {
                let r = self.found_rect(n);
                if r.b < area.t || r.t > area.b {
                    continue;
                }
                let hovered = matches!(self.hover, Hit::Found(i) | Hit::FoundAdd(i) if i == n);
                let playing = self.preview.as_deref() == Some(f.url.as_str());
                let waiting = self.want_preview.as_deref() == Some(f.url.as_str());
                if playing {
                    g.fill(r, 8.0, accent().alpha(0.08));
                } else if hovered {
                    g.fill(r, 8.0, HOVER);
                }
                let left = Rect::new(r.l + 8.0, r.t, r.l + 28.0, r.b);
                if waiting {
                    g.text("…", &g.f.small_center, left, MUTED);
                } else if playing {
                    g.text(ICON_STOP, &g.f.icon_small, left, accent());
                } else if hovered {
                    g.text(ICON_PLAY, &g.f.icon_small, left, MUTED);
                }
                let add = self.found_add_rect(g, n);
                let name_r = if f.added || f.loading || hovered { add.l - 10.0 } else { r.r - 10.0 };
                g.text(
                    &f.name,
                    &g.f.text,
                    Rect::new(r.l + 36.0, r.t, name_r, r.b),
                    if playing { accent() } else { TEXT },
                );
                if f.added {
                    g.text(ICON_CHECK, &g.f.icon_small, Rect::new(add.l, add.t, add.l + 20.0, add.b), accent());
                    g.text(t!("Added", "Eklendi"), &g.f.small, Rect::new(add.l + 24.0, add.t, add.r, add.b), accent());
                } else if f.loading && !waiting {
                    g.text(t!("Adding…", "Ekleniyor…"), &g.f.small, add, MUTED);
                } else if hovered {
                    button(g, add, t!("Add", "Ekle"), Some(ICON_ADD), None, self.hover == Hit::FoundAdd(n));
                }
            }
        });
    }

    // --- Görünürlük ve zamanlayıcılar ---

    /// Sayfa ekrana geldi ya da gitti (pencere gizlendi, başka sekmeye geçildi).
    pub fn set_visible(&mut self, visible: bool) {
        if visible && !self.visible {
            for i in &mut self.items {
                i.missing = !i.path.is_file();
            }
            self.refresh_mic();
        }
        if !visible {
            self.end_bind(None);
            self.hover = Hit::None;
            self.stop_preview();
        }
        self.visible = visible;
        self.update_timers();
    }

    fn update_timers(&self) {
        unsafe {
            if self.visible && !self.playing.is_empty() {
                SetTimer(Some(self.hwnd), TIMER_ANIM, 33, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_ANIM);
            }
            if self.visible {
                SetTimer(Some(self.hwnd), TIMER_STATUS, 1500, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_STATUS);
            }
        }
    }

    // --- Olaylar (koordinatlar sayfanın köşesine göre) ---

    pub fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        if let Some(k) = self.drag {
            self.slide_to(k, x);
            return;
        }
        let hit = self.hit(g, x, y);
        if hit != self.hover {
            self.hover = hit;
            self.redraw();
        }
    }

    pub fn mouse_leave(&mut self) {
        if self.hover != Hit::None {
            self.hover = Hit::None;
            self.redraw();
        }
    }

    pub fn mouse_down(&mut self, g: &Gfx, x: f32, y: f32) {
        let hit = self.hit(g, x, y);
        self.pressed = hit;
        if self.bind.is_some() && !matches!(hit, Hit::RowKey(_) | Hit::StopKey | Hit::ClipKey) {
            self.end_bind(None);
        }
        if let Hit::Slider(k) = hit {
            self.drag = Some(k);
            self.slide_to(k, x);
        }
    }

    pub fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        if self.drag.take().is_some() {
            self.save();
            self.redraw();
            return;
        }
        let hit = self.hit(g, x, y);
        if hit != std::mem::replace(&mut self.pressed, Hit::None) {
            return;
        }
        match hit {
            Hit::Stop => self.player.stop_all(),
            Hit::StopKey => self.start_bind(Bind::Stop),
            Hit::Status if matches!(self.mic, MicState::Detached) => self.run_setup(true),
            Hit::Tab(t) => self.set_tab(t),
            Hit::EmptyFind => self.set_tab(Tab::Find),
            Hit::AddFiles => self.modal = Some(Modal::AddFiles),
            Hit::Clip => self.save_clip(),
            Hit::ClipKey => self.start_bind(Bind::Clip),
            Hit::Row(n) => self.toggle(n),
            Hit::RowKey(n) => self.start_bind(Bind::Sound(n)),
            Hit::RowRemove(n) => self.remove(n),
            Hit::Search => {
                self.selected = false;
                self.redraw();
            }
            Hit::SearchClear => self.clear_query(),
            Hit::Found(n) => self.toggle_preview(n),
            Hit::FoundAdd(n) => self.add_found(n),
            _ => {}
        }
    }

    pub fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        match self.hit(g, x, y) {
            Hit::Slider(k) => {
                self.set_level(k, (self.level[k] as f32 + delta * 5.0).round().max(0.0) as u32);
                self.save();
            }
            _ if self.tab == Tab::Find => {
                self.find_scroll = (self.find_scroll - delta * ROW).clamp(0.0, self.find_max_scroll());
                self.hover = self.hit(g, x, y);
                self.redraw();
            }
            _ => {
                self.scroll = (self.scroll - delta * ROW).clamp(0.0, self.max_scroll());
                self.hover = self.hit(g, x, y);
                self.redraw();
            }
        }
    }

    /// Kısayol atanıyorsa tuşu yakalar (`true`). Ctrl+F ses bul'u açar; Esc ses bul'da önce
    /// seçimi, sonra yazıyı siler, sonra Seslerim'e döner; Seslerim'de her şeyi susturur.
    pub fn key(&mut self, vk: u16) -> bool {
        if self.bind.is_some() {
            if !hotkey::is_modifier(vk) {
                let key = match VIRTUAL_KEY(vk) {
                    VK_ESCAPE => None,
                    VK_BACK | VK_DELETE => Some(None),
                    _ => Some(Some(Hotkey::pressed(vk))),
                };
                self.end_bind(key);
            }
            return true;
        }
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        if ctrl && VIRTUAL_KEY(vk) == VK_F {
            self.set_tab(Tab::Find);
            self.selected = !self.query.is_empty();
            self.redraw();
            return true;
        }
        if self.tab == Tab::Find {
            return self.find_key(vk, ctrl);
        }
        if VIRTUAL_KEY(vk) == VK_ESCAPE {
            self.player.stop_all();
            return true;
        }
        false
    }

    /// Kısayol atarken Alt ile gelen menü ve bip sesi olmasın.
    pub fn binding(&self) -> bool {
        self.bind.is_some()
    }

    pub fn kill_focus(&mut self) {
        self.end_bind(None);
    }

    pub fn hotkey(&mut self, id: i32) {
        match id {
            HK_STOP => self.player.stop_all(),
            HK_CLIP => self.save_clip(),
            id if id >= HK_SOUND => self.toggle((id - HK_SOUND) as usize),
            _ => {}
        }
    }

    pub fn timer(&mut self, id: usize) {
        match id {
            TIMER_ANIM => self.redraw(),
            TIMER_STATUS => self.refresh_mic(),
            TIMER_SEARCH => self.search_now(),
            _ => {}
        }
    }

    pub fn player_changed(&mut self) {
        self.playing = self.player.playing();
        self.update_timers();
        self.redraw();
    }

    /// Kurulum bitti: 0 tamam, 1 hata, 2 izin verilmedi.
    pub fn setup_done(&mut self, code: usize) {
        self.dll_current = install::current();
        self.mic = MicState::Unknown;
        self.refresh_mic();
        if code == 1 {
            error_box(t!(
                "Could not connect to the microphone.\n\nDetails: %LOCALAPPDATA%\\Programs\\hive\\hive.log",
                "Mikrofona bağlanamadı.\n\nAyrıntılar: %LOCALAPPDATA%\\Programs\\hive\\hive.log"
            ));
        }
    }
}

// --- Ses bul: arama kutusu, Myinstants, önizleme ---

impl Lyrebird {
    /// Ses bul açıkken yazılanlar arama kutusuna gider; her şey seçiliyse yerine geçer.
    pub fn char(&mut self, c: char) -> bool {
        if self.tab != Tab::Find || self.bind.is_some() || c.is_control() {
            return false;
        }
        if std::mem::take(&mut self.selected) {
            self.query.clear();
        }
        if self.query.chars().count() < QUERY_MAX {
            self.query.push(c);
        }
        self.schedule_search();
        true
    }

    fn find_key(&mut self, vk: u16, ctrl: bool) -> bool {
        match VIRTUAL_KEY(vk) {
            VK_ESCAPE => {
                if std::mem::take(&mut self.selected) {
                    self.redraw();
                } else if self.preview.is_some() || self.want_preview.is_some() {
                    self.stop_preview();
                    self.redraw();
                } else if !self.query.is_empty() {
                    self.clear_query();
                } else {
                    self.set_tab(Tab::Mine);
                }
            }
            VK_BACK | VK_DELETE => {
                if std::mem::take(&mut self.selected) || ctrl {
                    self.query.clear();
                } else if VIRTUAL_KEY(vk) == VK_BACK {
                    self.query.pop();
                }
                self.schedule_search();
            }
            VK_RETURN => self.search_now(),
            VK_A if ctrl => {
                self.selected = !self.query.is_empty();
                self.redraw();
            }
            VK_V if ctrl => {
                if let Some(text) = clipboard_text() {
                    if std::mem::take(&mut self.selected) {
                        self.query.clear();
                    }
                    let line = text.lines().next().unwrap_or("").trim().to_string();
                    let room = QUERY_MAX.saturating_sub(self.query.chars().count());
                    self.query.extend(line.chars().take(room));
                    self.schedule_search();
                }
            }
            _ => return false,
        }
        true
    }

    fn clear_query(&mut self) {
        self.query.clear();
        self.selected = false;
        self.search_now();
    }

    /// Yazmayı bırakınca arar.
    fn schedule_search(&mut self) {
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_SEARCH, SEARCH_DELAY, None);
        }
        self.redraw();
    }

    fn search_now(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_SEARCH);
        }
        self.search_seq += 1;
        self.find_state = FindState::Loading;
        let (seq, query, out, hwnd) =
            (self.search_seq, self.query.clone(), self.searched.clone(), self.hwnd.0 as usize);
        std::thread::spawn(move || {
            let r = myinstants::search(&query).map_err(|e| e.to_string());
            *out.lock().unwrap() = Some((seq, r));
            post(hwnd, WM_SEARCHED, 0);
        });
        self.redraw();
    }

    /// WM_SEARCHED: yalnızca son aramanın sonucu alınır.
    pub fn searched(&mut self) {
        let Some((seq, r)) = self.searched.lock().unwrap().take() else { return };
        if seq != self.search_seq {
            return;
        }
        match r {
            Ok(list) => {
                self.found = list
                    .into_iter()
                    .map(|i| {
                        let added = self.items.iter().any(|it| it.path == target(&i.name));
                        Found { name: i.name, url: i.url, loading: false, added }
                    })
                    .collect();
                self.find_scroll = 0.0;
                self.find_state = FindState::Done;
            }
            Err(e) => self.find_state = FindState::Error(e),
        }
        self.redraw();
    }

    /// İndirir (önizleme ya da listeye); bitince WM_FETCHED.
    fn fetch(&mut self, n: usize, dest: Option<PathBuf>) {
        let Some(f) = self.found.get_mut(n) else { return };
        f.loading = true;
        let (url, out, hwnd) = (f.url.clone(), self.fetched.clone(), self.hwnd.0 as usize);
        std::thread::spawn(move || {
            let cache = preview_path(&url);
            let add = dest.is_some();
            let result = (|| -> Result<PathBuf, String> {
                let dest = dest.unwrap_or_else(|| cache.clone());
                if dest == cache && cache.is_file() {
                    return Ok(cache);
                }
                if let Some(dir) = dest.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                if cache.is_file() {
                    std::fs::copy(&cache, &dest).map_err(|e| e.to_string())?;
                } else {
                    let data = myinstants::download(&url).map_err(|e| e.to_string())?;
                    // mp3: ID3 etiketi ya da çerçeve başı. Değilse doğrulama sayfası gelmiştir.
                    if !(data.starts_with(b"ID3") || data.first() == Some(&0xFF) || data.starts_with(b"RIFF")) {
                        return Err(t!(
                            "Myinstants sent something that is not a sound",
                            "Myinstants ses yerine başka bir şey gönderdi"
                        )
                        .into());
                    }
                    std::fs::write(&dest, data).map_err(|e| e.to_string())?;
                }
                Ok(dest)
            })();
            out.lock().unwrap().push(Fetched { url, add, result });
            post(hwnd, WM_FETCHED, 0);
        });
        self.redraw();
    }

    /// WM_FETCHED: listeye ekle ya da kulaklıkta çal.
    pub fn fetched(&mut self) {
        let done = std::mem::take(&mut *self.fetched.lock().unwrap());
        for f in done {
            let n = self.found.iter().position(|x| x.url == f.url);
            if let Some(x) = n.and_then(|n| self.found.get_mut(n)) {
                x.loading = false;
            }
            match f.result {
                Ok(path) if f.add => {
                    self.add(vec![path]);
                    if let Some(x) = n.and_then(|n| self.found.get_mut(n)) {
                        x.added = true;
                    }
                }
                Ok(path) => {
                    if self.want_preview.as_deref() == Some(f.url.as_str()) {
                        self.start_preview(f.url, &path);
                    }
                }
                Err(e) => {
                    log!("myinstants: {e}");
                    if self.want_preview.as_deref() == Some(f.url.as_str()) {
                        self.want_preview = None;
                    }
                    self.find_state = FindState::Error(e);
                }
            }
        }
        self.redraw();
    }

    fn add_found(&mut self, n: usize) {
        let Some(f) = self.found.get(n) else { return };
        if f.added || f.loading {
            return;
        }
        let path = target(&f.name);
        if path.is_file() {
            // Daha önce indirilmiş (listeden çıkarılmış olabilir): yeniden indirmeye gerek yok.
            self.add(vec![path]);
            self.found[n].added = true;
            self.redraw();
        } else {
            self.fetch(n, Some(path));
        }
    }

    // --- Önizleme: yalnızca kulaklıkta, Windows'un MCI oynatıcısıyla ---

    fn toggle_preview(&mut self, n: usize) {
        let Some(f) = self.found.get(n) else { return };
        let url = f.url.clone();
        let same = self.preview.as_deref() == Some(url.as_str()) || self.want_preview.as_deref() == Some(url.as_str());
        self.stop_preview();
        if !same {
            self.want_preview = Some(url.clone());
            let cache = preview_path(&url);
            if cache.is_file() {
                self.start_preview(url, &cache);
            } else {
                self.fetch(n, None);
            }
        }
        self.redraw();
    }

    fn start_preview(&mut self, url: String, path: &Path) {
        mci(&format!("close {PREVIEW}"), None);
        let ok = mci(&format!("open \"{}\" type mpegvideo alias {PREVIEW}", path.display()), None)
            && mci(&format!("play {PREVIEW} notify"), Some(self.hwnd));
        if !ok {
            log!("önizleme çalınamadı: {}", path.display());
        }
        self.preview = ok.then_some(url);
        self.want_preview = None;
        self.redraw();
    }

    fn stop_preview(&mut self) {
        if self.preview.take().is_some() {
            mci(&format!("close {PREVIEW}"), None);
        }
        self.want_preview = None;
    }

    /// MM_MCINOTIFY: önizleme sona geldi (durdurulanlar da bildirilir; onlar yok sayılır).
    pub fn preview_done(&mut self, code: usize) {
        if code == MCI_NOTIFY_SUCCESSFUL {
            self.stop_preview();
            self.redraw();
        }
    }
}

/// Panodaki yazı.
fn clipboard_text() -> Option<String> {
    unsafe {
        OpenClipboard(None).ok()?;
        let text = (|| {
            let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
            let p = GlobalLock(HGLOBAL(h.0)) as *const u16;
            if p.is_null() {
                return None;
            }
            let len = (0..).take_while(|&i| *p.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            let _ = GlobalUnlock(HGLOBAL(h.0));
            Some(s)
        })();
        let _ = CloseClipboard();
        text
    }
}

fn add_dialog(hwnd: HWND) -> Res<Vec<PathBuf>> {
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dlg.SetOptions(dlg.GetOptions()? | FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)?;
        dlg.SetTitle(t!(w!("Add sounds"), w!("Ses ekle")))?;
        dlg.SetFileTypes(&[COMDLG_FILTERSPEC {
            pszName: t!(w!("Sound files"), w!("Ses dosyaları")),
            pszSpec: w!("*.mp3;*.wav;*.m4a;*.aac;*.wma;*.flac"),
        }])?;
        if dlg.Show(Some(hwnd)).is_err() {
            return Ok(Vec::new());
        }
        let items = dlg.GetResults()?;
        let mut out = Vec::new();
        for i in 0..items.GetCount()? {
            let p = items.GetItemAt(i)?.GetDisplayName(SIGDN_FILESYSPATH)?;
            out.push(PathBuf::from(p.to_string()?));
            CoTaskMemFree(Some(p.0 as *const _));
        }
        Ok(out)
    }
}

/// Kalıcı işi uygulama borcu dışında çalıştırır; sonucu `apply` ile sayfaya geri verir.
pub fn run_modal(hwnd: HWND, modal: Modal, apply: impl FnOnce(Box<dyn FnOnce(&mut Lyrebird)>)) {
    match modal {
        Modal::AddFiles => match add_dialog(hwnd) {
            Ok(paths) => apply(Box::new(move |l| l.add(paths))),
            Err(e) => log!("dosya penceresi: {e}"),
        },
    }
}

impl ToolPage for Lyrebird {
    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        Lyrebird::layout(self, w, h, head_x, head_r)
    }
    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        // Mikrofon durumu yalnızca bağlı değilken tıklanır.
        match self.hit(g, x, y) {
            Hit::None => false,
            Hit::Status => matches!(self.mic, MicState::Detached),
            _ => true,
        }
    }
    fn paint(&self, g: &Gfx) {
        Lyrebird::paint(self, g)
    }
    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        Lyrebird::mouse_move(self, g, x, y)
    }
    fn mouse_leave(&mut self) {
        Lyrebird::mouse_leave(self)
    }
    fn mouse_down(&mut self, g: &Gfx, x: f32, y: f32) {
        Lyrebird::mouse_down(self, g, x, y)
    }
    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        Lyrebird::mouse_up(self, g, x, y)
    }
    fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        Lyrebird::wheel(self, g, x, y, delta)
    }
    fn key(&mut self, vk: u16) -> bool {
        Lyrebird::key(self, vk)
    }
    fn char(&mut self, c: char) -> bool {
        Lyrebird::char(self, c)
    }
    fn set_visible(&mut self, visible: bool) {
        Lyrebird::set_visible(self, visible)
    }
    fn tip(&self) -> Option<(Rect, String)> {
        match (&self.hover, &self.mic) {
            (Hit::Stop, _) => Some((self.stop_rect(), t!("Mute all", "Hepsini sustur").into())),
            (Hit::Status, MicState::Attached(name)) => {
                let state = match self.active {
                    true => t!("an app is listening", "bir uygulama dinliyor"),
                    false => t!("nothing is listening", "şu an dinleyen yok"),
                };
                Some((
                    Rect::new(self.head_x + 14.0, HEAD_CY - 13.0, self.head_x + 90.0, HEAD_CY + 13.0),
                    format!("{name} · {state}"),
                ))
            }
            _ => None,
        }
    }
}

impl Drop for Lyrebird {
    /// Kaldırılınca kısayollar ve zamanlayıcılar bırakılır (pencere hive'la yaşamaya devam eder).
    fn drop(&mut self) {
        self.unregister();
        self.player.stop_all();
        self.stop_preview();
        let _ = std::fs::remove_dir_all(preview_dir());
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_ANIM);
            let _ = KillTimer(Some(self.hwnd), TIMER_STATUS);
            let _ = KillTimer(Some(self.hwnd), TIMER_SEARCH);
        }
    }
}
