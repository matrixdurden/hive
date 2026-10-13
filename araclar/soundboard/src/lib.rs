//! soundboard sayfası. Motor (çözme, ortak bellek, kulaklık) motor/'da ve
//! winloom sürecinde çalışır; mikrofon efekti (APO) audiodg.exe içinde.
//!
//! Başlıkta mikrofonun durumu, altında üç sekme (ortak kitin sekmeleri):
//! - Seslerim: ses kutularından ızgara; tıklamak çalar / durdurur, kutuda kısayolu, adı düzenle,
//!   kaldır. Üstte "Dosya ekle" ve "Son 10 saniye" (bilgisayarda az önce çalanı listeye ekler).
//! - Ses bul: Myinstants araması. Sonuca tıklamak onu kulaklıkta çalar (yalnızca sana, mikrofona
//!   gitmez), "Ekle" listeye indirir.
//! - Ayarlar (sayfa kiti): mikrofon bağlantısı, iki ses düzeyi, hepsini susturmanın ve son 10
//!   saniyenin kısayolları.

#[macro_use]
extern crate ortak;

mod tool;
pub use tool::TOOL;

mod myinstants;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::Media::Multimedia::mciSendStringW;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use ortak::form::{self, Ctl, Form, Row, TABS_H};
use ortak::gfx::{Color, Gfx, Rect};
use ortak::hotkey::{self, Hotkey};
use ortak::log;
use ortak::osd;
use ortak::ui::*;
use ortak::util::{Res, error_box, wide};

use soundboard_engine::bus::{self, Bus};
use soundboard_engine::clip::{self, Clipper};
use soundboard_engine::config::{self, Config, Sound};
use soundboard_engine::decode::EXTENSIONS;
use soundboard_engine::install::Mic;
use soundboard_engine::monitor::Monitor;
use soundboard_engine::player::{Player, Playing};
pub use soundboard_engine::{data_dir, install, installed, probe};


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
/// Bu aracın kısayol aralığının sonu: Ses aygıtlarının kısayolları buradan başlar.
const HK_END: i32 = 5001;
pub const HK_CLIP: i32 = 1002;
pub const HK_SOUND: i32 = 1100;

/// Yükseltilmiş kurulumun komut satırı bayrakları.
pub const ARG_INSTALL: &str = "--soundboard-kur";
pub const ARG_UNINSTALL: &str = "--soundboard-kaldir";

const ICON_CLIP: &str = "\u{E7C8}";
const ICON_STOP: &str = "\u{E71A}";
const ICON_RENAME: &str = "\u{E70F}";
const ICON_KEYBOARD: &str = "\u{E765}";

/// Sayfa kitindeki satırların kimlikleri; arama sonuçları sıralarıyla (`R_FOUND + n`).
const R_MIC: u32 = 1;
const R_MIC_LEVEL: u32 = 2;
const R_EAR_LEVEL: u32 = 3;
const R_STOP: u32 = 4;
const R_CLIP: u32 = 5;
const R_SEARCH: u32 = 6;
const R_FOUND: u32 = 1_000;
/// İçeriğin sekmelerden uzaklığı; araç çubuğu ve arama kutusunun yüksekliği.
const TOP_GAP: f32 = 16.0;
const TOOLBAR_H: f32 = 34.0;
const SEARCH_H: f32 = 34.0;
/// Ses kutuları: en dar genişlik, yükseklik, aradaki boşluk ve köşe.
const TILE_MIN_W: f32 = 180.0;
const TILE_H: f32 = 96.0;
const TILE_GAP: f32 = 12.0;
const TILE_RADIUS: f32 = 10.0;
/// Tekerin bir adımı.
const WHEEL_STEP: f32 = 60.0;
/// Arama kutusunun ve ses adının en çok karakteri.
const QUERY_MAX: usize = 60;
const NAME_MAX: usize = 80;

/// `--soundboard-kur` / `--soundboard-kaldir`: yükseltilmiş olarak çalışır, çıkış kodu sonucu bildirir.
pub fn setup(install: bool) -> i32 {
    match soundboard_engine::setup(install) {
        Ok(()) => {
            log!("soundboard {} tamam", if install { "kurulumu" } else { "kaldırma" });
            0
        }
        Err(e) => {
            log!("soundboard kurulum hatası: {e}");
            println!("{} {e}", t!("ERROR", "HATA"));
            1
        }
    }
}

/// Satır kimliğinden sonucun sırası.
fn found_index(id: u32) -> Option<usize> {
    (id >= R_FOUND).then(|| (id - R_FOUND) as usize)
}

/// Kaydırıcı satırının düzeyi: 0 mikrofon, 1 kulaklık.
fn level_index(id: u32) -> usize {
    if id == R_MIC_LEVEL { 0 } else { 1 }
}

/// Başlığın altındaki sekmeler, sırayla.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Tab {
    Sounds,
    Find,
    Settings,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Sounds, Tab::Find, Tab::Settings];

    fn label(self) -> &'static str {
        match self {
            Tab::Sounds => t!("My sounds", "Seslerim"),
            Tab::Find => t!("Find sounds", "Ses bul"),
            Tab::Settings => t!("Settings", "Ayarlar"),
        }
    }
}

fn tab_labels() -> [&'static str; 3] {
    Tab::ALL.map(Tab::label)
}

/// Adı iki satıra böler: ilk satıra (`w1` genişliğe) sığan sözcükler, kalanı ikinci satır
/// (o da sığmazsa çizerken "…" ile kesilir).
fn two_lines(g: &Gfx, s: &str, w1: f32) -> (String, String) {
    let font = &g.f.strong;
    if g.measure(s, font) <= w1 {
        return (s.into(), String::new());
    }
    let mut first = String::new();
    // Boşluk, tire ve alt çizgiden bölünür (dosya adları "deha-alacarte-sub-alert" gibi).
    for word in s.split_inclusive([' ', '-', '_']) {
        let next = format!("{first}{word}");
        if g.measure(next.trim_end(), font) > w1 {
            break;
        }
        first = next;
    }
    // Tek uzun sözcük: harf harf.
    if first.is_empty() {
        for c in s.chars() {
            let next = format!("{first}{c}");
            if g.measure(&next, font) > w1 {
                break;
            }
            first = next;
        }
    }
    let rest = s[first.len()..].trim_start().to_string();
    (first.trim_end().into(), rest)
}

/// Tıklanan / üstüne gelinen: sekme, kit (Ses bul, Ayarlar) ya da Seslerim'deki bir şey.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Tab(Tab),
    Form(form::Hit),
    /// Seslerim'in "Dosya ekle" ve "Son 10 saniye" düğmeleri.
    Add,
    Clip,
    /// Ses kutusu ve üstündekiler: kısayol, adı düzenle, kaldır.
    Tile(usize),
    Key(usize),
    Rename(usize),
    Remove(usize),
}

impl Hit {
    /// Basılan yerde mi bırakıldı (kaydırıcının değeri dışında).
    fn same(self, other: Hit) -> bool {
        match (self, other) {
            (Hit::Form(a), Hit::Form(b)) => a.same(b),
            _ => self == other,
        }
    }

    /// Kitin gördüğü.
    fn form(self) -> form::Hit {
        match self {
            Hit::Form(h) => h,
            _ => form::Hit::None,
        }
    }

    /// Hangi ses kutusunda (kutunun üstündekiler de kutu sayılır: kutu belirgin kalsın).
    fn tile(self) -> Option<usize> {
        match self {
            Hit::Tile(n) | Hit::Key(n) | Hit::Rename(n) | Hit::Remove(n) => Some(n),
            _ => None,
        }
    }
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
    /// Dosya adı (uzantısız).
    stem: String,
    /// Kullanıcının verdiği ad.
    label: Option<String>,
    hotkey: Option<Hotkey>,
    conflict: bool,
    missing: bool,
}

impl Item {
    fn new(id: u64, s: Sound) -> Self {
        let stem =
            s.path.file_stem().map_or_else(|| s.path.display().to_string(), |n| n.to_string_lossy().into_owned());
        Self { id, missing: !s.path.is_file(), path: s.path, stem, label: s.name, hotkey: s.hotkey, conflict: false }
    }

    fn name(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.stem)
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

/// Dinlenen Myinstants sesleri; sayfa kapanınca silinir.
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

const PREVIEW: &str = "soundboard_onizleme";

pub struct Soundboard {
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
    /// Sürüklenen kaydırıcının satırı.
    drag: Option<u32>,
    bind: Option<Bind>,
    /// Seçili sekme (sayfa yaşadıkça hatırlanır) ve her sekmenin kaydırması.
    tab: Tab,
    scroll: [f32; 3],
    registered: Vec<i32>,
    modal: Option<Modal>,
    dll_current: bool,
    /// Sayfanın boyutu ve başlık yazısının bittiği x (winloom her olaydan önce verir).
    w: f32,
    h: f32,
    head_x: f32,
    head_r: f32,
    /// Sayfa ekranda mı (zamanlayıcılar yalnızca o zaman çalışır).
    visible: bool,
    /// Son 10 saniye: bilgisayarın sesini (yalnızca ses çalarken) izleyen motor.
    clipper: Clipper,
    clip: Option<Hotkey>,
    clip_conflict: bool,
    /// Ses bul: arama kutusu (odaktayken yazılanlar buraya gelir) ve sonuçlar.
    search: TextField,
    found: Vec<Found>,
    find_state: FindState,
    search_seq: u64,
    searched: Arc<Mutex<Option<(u64, Result<Vec<myinstants::Instant>, String>)>>>,
    fetched: Arc<Mutex<Vec<Fetched>>>,
    /// Kulaklıkta çalan önizleme ve beklenen (inerken).
    preview: Option<String>,
    want_preview: Option<String>,
    /// Adı düzenlenen ses ve kutusu.
    rename: Option<(usize, TextField)>,
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
            log!("soundboard ortak belleği açılamadı, yalnızca kulaklık çalışacak: {e}");
            let layout = std::alloc::Layout::new::<Bus>();
            unsafe { &*(std::alloc::alloc_zeroed(layout) as *const Bus) }
        }
    }
}

impl Soundboard {
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
            tab: Tab::Sounds,
            scroll: [0.0; 3],
            registered: Vec::new(),
            modal: None,
            dll_current: install::current(),
            w: 0.0,
            h: 0.0,
            head_x: 0.0,
            head_r: 0.0,
            visible: false,
            clipper: Clipper::start(),
            clip: cfg.clip,
            clip_conflict: false,
            search: TextField::new(QUERY_MAX),
            found: Vec::new(),
            find_state: FindState::Idle,
            search_seq: 0,
            searched: Arc::default(),
            fetched: Arc::default(),
            preview: None,
            want_preview: None,
            rename: None,
        };
        l.register();
        l.refresh_mic();
        log!("soundboard: {} ses, mikrofon {}, kulaklık {}", l.items.len(), l.level[0], l.level[1]);
        l
    }

    pub fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
        self.clamp_scroll();
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
            sounds: self.items.iter().map(|i| Sound { path: i.path.clone(), hotkey: i.hotkey, name: i.label.clone() }).collect(),
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
                self.items.push(Item::new(self.next_id, Sound { path: p, hotkey: None, name: None }));
            }
        }
        if self.items.len() != before {
            self.save();
            self.redraw();
        }
    }

    // --- Ad düzenleme ---

    fn start_rename(&mut self, n: usize) {
        let Some(name) = self.items.get(n).map(|i| i.name().to_string()) else { return };
        self.end_bind(None);
        self.end_rename(true);
        self.search.blur();
        let mut f = TextField::new(NAME_MAX);
        f.set(&name);
        f.focus(true);
        self.rename = Some((n, f));
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
        self.redraw();
    }

    /// `commit`: kutudakini ad yap (boşsa ya da dosya adıyla aynıysa verilen ad silinir).
    fn end_rename(&mut self, commit: bool) {
        let Some((n, f)) = self.rename.take() else { return };
        if commit && let Some(item) = self.items.get_mut(n) {
            let name = f.text.trim().to_string();
            let label = (!name.is_empty() && name != item.stem).then_some(name);
            if label != item.label {
                item.label = label;
                self.save();
            }
        }
        self.redraw();
    }

    fn renaming(&self, n: usize) -> bool {
        self.rename.as_ref().is_some_and(|(i, _)| *i == n)
    }

    fn remove(&mut self, n: usize) {
        if n >= self.items.len() {
            return;
        }
        if self.renaming(n) {
            self.rename = None;
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
        self.clamp_scroll();
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

    /// Mikrofonun kısa durumu (başlık ve ana sayfadaki kutucuk).
    fn mic_label(&self) -> &'static str {
        match &self.mic {
            MicState::Unknown => "",
            MicState::Missing => t!("No microphone", "Mikrofon yok"),
            MicState::Busy => t!("Connecting…", "Bağlanıyor…"),
            MicState::Detached => t!("Not connected to the microphone", "Mikrofona bağlı değil"),
            MicState::Attached(_) => t!("Connected to the microphone", "Mikrofona bağlı"),
        }
    }

    // --- Sayfa kiti ---

    fn chip_text(&self, key: Option<Hotkey>, binding: bool) -> String {
        match (binding, key) {
            (true, _) => t!("press a key", "tuşa bas").into(),
            (false, Some(k)) => k.to_string(),
            (false, None) => t!("shortcut", "kısayol").into(),
        }
    }

    /// Kısayol satırının açıklaması: başka bir uygulama tutuyorsa onu söyler.
    fn key_sub(&self, sub: &str, conflict: bool) -> String {
        match conflict {
            true => t!("Another app is using this shortcut", "Bu kısayolu başka bir uygulama kullanıyor").into(),
            false => sub.into(),
        }
    }

    fn clip_label(&self) -> String {
        format!("{} {} {}", t!("Last", "Son"), clip::SECONDS, t!("seconds", "saniye"))
    }

    fn mic_row(&self) -> Row {
        let (title, sub, ctl) = match &self.mic {
            MicState::Attached(name) => (
                name.clone(),
                t!("Connected · your sounds go out through this microphone", "Bağlı · seslerin bu mikrofondan karşı tarafa gider"),
                Ctl::None,
            ),
            MicState::Detached => (
                t!("Microphone", "Mikrofon").into(),
                t!("Not connected · for now only you hear your sounds", "Bağlı değil · seslerini şimdilik yalnızca sen duyarsın"),
                Ctl::Primary(t!("Connect microphone", "Mikrofona bağla").into()),
            ),
            MicState::Missing => (
                t!("No microphone", "Mikrofon yok").into(),
                t!("Windows has no default recording device", "Windows'ta varsayılan bir kayıt aygıtı yok"),
                Ctl::None,
            ),
            MicState::Busy => (
                t!("Microphone", "Mikrofon").into(),
                t!("Connecting… administrator permission is asked", "Bağlanıyor… yönetici izni istenir"),
                Ctl::None,
            ),
            MicState::Unknown => (t!("Microphone", "Mikrofon").into(), "", Ctl::None),
        };
        Row::new(R_MIC, title, sub, ctl).icon(ICON_MIC).dim(matches!(self.mic, MicState::Missing))
    }

    /// Ayarlar: mikrofon ve iki ses düzeyi, kısayollar.
    fn settings_items(&self) -> Vec<form::Item> {
        use form::Item::Section;
        let row = form::Item::Row;
        let mut v = vec![Section(t!("Microphone", "Mikrofon").into()), row(self.mic_row())];
        for (id, icon, title, sub) in [
            (
                R_MIC_LEVEL,
                ICON_MIC,
                t!("Microphone level", "Mikrofon düzeyi"),
                t!("How loud your sounds are for the others", "Seslerin karşı tarafa ne kadar yüksek gideceği"),
            ),
            (
                R_EAR_LEVEL,
                ICON_EAR,
                t!("Headphone level", "Kulaklık düzeyi"),
                t!("How loud you hear them yourself", "Seslerin sana ne kadar yüksek geleceği"),
            ),
        ] {
            let l = self.level[level_index(id)];
            v.push(row(Row::new(id, title, sub, Ctl::Slider(l as f32 / 100.0, l.to_string())).icon(icon)));
        }

        v.push(Section(t!("Shortcuts", "Kısayollar").into()));
        let stop_sub = t!("Click or press the shortcut: everything playing stops", "Tıkla ya da kısayola bas: çalan her şey durur");
        let binding = self.bind == Some(Bind::Stop);
        v.push(row(
            Row::new(
                R_STOP,
                t!("Mute all", "Hepsini sustur"),
                self.key_sub(stop_sub, self.stop_conflict),
                Ctl::Chip(self.chip_text(self.stop, binding), binding),
            )
            .icon(ICON_MUTE)
            .clickable(),
        ));
        let clip_sub = t!(
            "Click or press the shortcut: what just played on this computer is added",
            "Tıkla ya da kısayola bas: bilgisayarda az önce çalan eklenir"
        );
        let binding = self.bind == Some(Bind::Clip);
        v.push(row(
            Row::new(
                R_CLIP,
                self.clip_label(),
                self.key_sub(clip_sub, self.clip_conflict),
                Ctl::Chip(self.chip_text(self.clip, binding), binding),
            )
            .icon(ICON_CLIP)
            .clickable(),
        ));
        v
    }

    /// Ses bul: arama kutusu (sayfa çizer), sonuçlar ve altında durum.
    fn find_items(&self) -> Vec<form::Item> {
        use form::Item::{Custom, Note};
        let mut v = vec![Custom(R_SEARCH, SEARCH_H)];
        for (n, f) in self.found.iter().enumerate() {
            let playing = self.preview.as_deref() == Some(f.url.as_str());
            let waiting = self.want_preview.as_deref() == Some(f.url.as_str());
            let icon = match () {
                _ if waiting => ICON_REFRESH,
                _ if playing => ICON_STOP,
                _ => ICON_PLAY,
            };
            let ctl = match () {
                _ if f.added => Ctl::Text(t!("Added", "Eklendi").into()),
                _ if f.loading && !waiting => Ctl::Text(t!("Adding…", "Ekleniyor…").into()),
                _ => Ctl::Button(t!("Add", "Ekle").into()),
            };
            v.push(form::Item::Row(Row::new(R_FOUND + n as u32, f.name.as_str(), "", ctl).icon(icon).clickable()));
        }
        let note = match &self.find_state {
            FindState::Error(e) => Some((e.clone(), true)),
            FindState::Loading if self.found.is_empty() => Some((t!("Searching…", "Aranıyor…").into(), false)),
            FindState::Done if self.found.is_empty() => Some((t!("Nothing found", "Bir şey bulunamadı").into(), false)),
            _ if !self.found.is_empty() => Some((
                t!(
                    "Click to listen: only you hear it, not the microphone · Add downloads it to your sounds",
                    "Dinlemek için tıkla: yalnızca sen duyarsın, mikrofona gitmez · Ekle seslerine indirir"
                )
                .into(),
                false,
            )),
            _ => None,
        };
        if let Some((text, err)) = note {
            v.push(Note(text, err));
        }
        v
    }

    /// İçeriğin üstü (seçili sekmenin kaydırmasıyla).
    fn content_top(&self) -> f32 {
        HEADER + TABS_H + TOP_GAP - self.scroll[self.tab as usize]
    }

    /// Seçili sekmenin kiti; Seslerim'in kendi ızgarası var (`None`).
    fn form(&self) -> Option<Form> {
        let items = match self.tab {
            Tab::Sounds => return None,
            Tab::Find => self.find_items(),
            Tab::Settings => self.settings_items(),
        };
        Some(Form::new(items, PAD, self.w - PAD, self.content_top()))
    }

    fn max_scroll(&self) -> f32 {
        let bottom = match self.form() {
            // Kitin sonundaki boşluk yerine biraz pay.
            Some(f) => f.bottom() - 24.0 + 12.0,
            None => self.sounds_bottom() + TOP_GAP,
        };
        (bottom + self.scroll[self.tab as usize] - self.h).max(0.0)
    }

    fn clamp_scroll(&mut self) {
        let m = self.max_scroll();
        let s = &mut self.scroll[self.tab as usize];
        *s = s.min(m);
    }

    fn select(&mut self, tab: Tab) {
        if self.tab == tab {
            return;
        }
        self.end_bind(None);
        self.end_rename(true);
        self.search.blur();
        // Önizlemenin durdurulduğu yer Ses bul'da: oradan çıkınca susar.
        self.stop_preview();
        self.tab = tab;
        if !matches!(self.hover, Hit::Tab(_)) {
            self.hover = Hit::None;
        }
        // Ses bul ilk açılışta Myinstants'ın öne çıkanlarıyla dolar.
        if tab == Tab::Find && matches!(self.find_state, FindState::Idle) {
            self.search_now();
        }
        self.redraw();
    }

    // --- Seslerim: araç çubuğu ve ses kutuları ---

    /// "Dosya ekle" ve "Son 10 saniye": liste varsa üstte sağda, boşsa ortada.
    fn add_buttons(&self, g: &Gfx) -> (Rect, Rect) {
        let (add, clip) = (t!("Add files", "Dosya ekle"), self.clip_label());
        if self.items.is_empty() {
            let (a, c) = (button_rect(g, 0.0, 0.0, add, true), button_rect(g, 0.0, 0.0, &clip, true));
            let l = (self.w - a.w() - 8.0 - c.w()) / 2.0;
            let t = self.empty_top() + 36.0;
            (button_rect(g, l, t, add, true), button_rect(g, l + a.w() + 8.0, t, &clip, true))
        } else {
            let t = self.content_top();
            let c = button_rect_right(g, self.w - PAD, t, &clip, true);
            (button_rect_right(g, c.l - 8.0, t, add, true), c)
        }
    }

    /// Boş listede yazının üstü (içerik alanının ortasında).
    fn empty_top(&self) -> f32 {
        (HEADER + TABS_H + self.h) / 2.0 - 36.0
    }

    /// Izgara: sütun sayısı, kutu genişliği ve ilk sıranın üstü (araç çubuğunun altı).
    fn grid(&self) -> (usize, f32, f32) {
        let avail = (self.w - 2.0 * PAD).max(TILE_MIN_W);
        let cols = (((avail + TILE_GAP) / (TILE_MIN_W + TILE_GAP)).floor() as usize).max(1);
        let tw = (avail - TILE_GAP * (cols - 1) as f32) / cols as f32;
        (cols, tw, self.content_top() + TOOLBAR_H + TOP_GAP)
    }

    fn tile_rect(&self, n: usize) -> Rect {
        let (cols, tw, top) = self.grid();
        let (l, t) = (PAD + (n % cols) as f32 * (tw + TILE_GAP), top + (n / cols) as f32 * (TILE_H + TILE_GAP));
        Rect::new(l, t, l + tw, t + TILE_H)
    }

    fn sounds_bottom(&self) -> f32 {
        if self.items.is_empty() {
            return self.content_top();
        }
        let (cols, _, top) = self.grid();
        top + self.items.len().div_ceil(cols) as f32 * (TILE_H + TILE_GAP) - TILE_GAP
    }

    /// Kutunun sağ üstündeki ikonlar: (adı düzenle, kaldır).
    fn tile_icons(p: Rect) -> (Rect, Rect) {
        let remove = Rect::new(p.r - 32.0, p.t + 8.0, p.r - 8.0, p.t + 32.0);
        (Rect::new(remove.l - 28.0, remove.t, remove.l - 4.0, remove.b), remove)
    }

    /// Kutunun altındaki kısayol satırının ortası.
    fn key_cy(p: Rect) -> f32 {
        p.b - 26.0
    }

    /// Kısayolu olmayan kutuda (üstüne gelince) klavye ikonu.
    fn key_glyph(p: Rect) -> Rect {
        let cy = Self::key_cy(p);
        Rect::new(p.l + 8.0, cy - 12.0, p.l + 32.0, cy + 12.0)
    }

    /// Kısayol kutucuğu (kısayol varsa ya da atanıyorsa), yoksa klavye ikonu.
    fn key_rect(&self, g: &Gfx, n: usize, p: Rect) -> Rect {
        let binding = self.bind == Some(Bind::Sound(n));
        match self.items.get(n) {
            Some(i) if i.hotkey.is_some() || binding => {
                let c = chip_rect(g, 0.0, Self::key_cy(p), &self.chip_text(i.hotkey, binding));
                Rect::new(p.l + 14.0, c.t, p.l + 14.0 + c.w(), c.b)
            }
            _ => Self::key_glyph(p),
        }
    }

    /// Kısayol kutucuğunun renkleri: (yazı, çerçeve).
    fn chip_colors(&self, binding: bool, conflict: bool, hovered: bool) -> (Color, Color) {
        match () {
            _ if binding => (accent(), accent()),
            _ if conflict => (pal().red, pal().line),
            _ if hovered => (pal().text, pal().faint),
            _ => (pal().muted, pal().line),
        }
    }

    fn sounds_hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        let (add, clip) = self.add_buttons(g);
        if add.contains(x, y) {
            return Hit::Add;
        }
        if clip.contains(x, y) {
            return Hit::Clip;
        }
        for n in 0..self.items.len() {
            let p = self.tile_rect(n);
            if !p.contains(x, y) {
                continue;
            }
            let (rename, remove) = Self::tile_icons(p);
            return match () {
                _ if remove.contains(x, y) => Hit::Remove(n),
                _ if rename.contains(x, y) => Hit::Rename(n),
                _ if !self.renaming(n) && self.key_rect(g, n, p).contains(x, y) => Hit::Key(n),
                _ => Hit::Tile(n),
            };
        }
        Hit::None
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return Hit::None;
        }
        if y < HEADER + TABS_H {
            return form::tab_at(g, &tab_labels(), x, y).map_or(Hit::None, |k| Hit::Tab(Tab::ALL[k]));
        }
        match self.form() {
            None => self.sounds_hit(g, x, y),
            Some(f) => match f.hit(g, x, y) {
                form::Hit::None => Hit::None,
                h => Hit::Form(h),
            },
        }
    }

    // --- Çizim ---

    pub fn paint(&self, g: &Gfx) {
        // Başlık: mikrofonun durumu.
        let label = self.mic_label();
        if !label.is_empty() {
            let (dot, text) = match &self.mic {
                MicState::Attached(_) if self.active => {
                    (pal().green, format!("{label} · {}", t!("an app is listening", "bir uygulama dinliyor")))
                }
                MicState::Attached(_) => (pal().faint, format!("{label} · {}", t!("nothing is listening", "şu an dinleyen yok"))),
                MicState::Busy => (accent(), label.into()),
                _ => (pal().faint, label.into()),
            };
            status(g, self.head_x + 14.0, HEAD_CY, dot, &text, pal().muted, self.head_r - 8.0);
        }

        g.clip(Rect::new(0.0, HEADER + TABS_H, self.w, self.h), || match self.form() {
            None => self.paint_sounds(g),
            Some(f) => {
                f.paint(g, self.hover.form(), self.drag);
                if let Some(r) = f.custom(R_SEARCH) {
                    self.search.paint(g, r, t!("Search Myinstants…", "Myinstants'ta ara…"));
                }
            }
        });
        let hover = Tab::ALL.iter().position(|&t| self.hover == Hit::Tab(t));
        form::paint_tabs(g, &tab_labels(), self.tab as usize, hover, self.w);
    }

    /// Seslerim: araç çubuğu (ya da boş listede ortada yazı) ve ses kutuları.
    fn paint_sounds(&self, g: &Gfx) {
        let (add, clip) = self.add_buttons(g);
        if self.items.is_empty() {
            let t = self.empty_top();
            let text = t!(
                "No sounds yet · drop sound files here or find some in Find sounds",
                "Henüz ses yok · ses dosyalarını buraya sürükle ya da Ses bul'da ara"
            );
            text_center(g, text, &g.f.text, self.w / 2.0, t, t + 24.0, pal().muted);
        } else {
            let n = self.items.len();
            g.text(&t!(format!("{n} sounds"), format!("{n} ses")), &g.f.text, Rect::new(PAD + 2.0, add.t, add.l - 16.0, add.b), pal().muted);
        }
        button(g, add, t!("Add files", "Dosya ekle"), Some(ICON_ADD), None, self.hover == Hit::Add);
        button(g, clip, &self.clip_label(), Some(ICON_CLIP), None, self.hover == Hit::Clip);
        for n in 0..self.items.len() {
            let p = self.tile_rect(n);
            if p.b >= HEADER + TABS_H && p.t <= self.h {
                self.paint_tile(g, n, p);
            }
        }
    }

    /// Ses kutusu: ad (iki satıra kadar), altında kısayol; çalarken vurgu çerçeve ve ilerleme.
    fn paint_tile(&self, g: &Gfx, n: usize, p: Rect) {
        let item = &self.items[n];
        let hovered = self.hover.tile() == Some(n);
        let pressed = self.pressed == Hit::Tile(n) && self.hover == Hit::Tile(n);
        let editing = self.rename.as_ref().filter(|(i, _)| *i == n).map(|(_, f)| f);
        let binding = self.bind == Some(Bind::Sound(n));
        let playing = self.playing.iter().find(|p| p.id == item.id);

        g.fill(p, TILE_RADIUS, pal().panel);
        if pressed {
            g.fill(p, TILE_RADIUS, pal().sel);
        } else if hovered {
            g.fill(p, TILE_RADIUS, pal().hover);
        }
        match playing {
            Some(_) => g.stroke(p, TILE_RADIUS, accent(), 2.0),
            None => g.stroke(p, TILE_RADIUS, pal().line, 1.0),
        }

        let (rename, remove) = Self::tile_icons(p);
        let l = p.l + 14.0;
        let line1 = Rect::new(l, p.t + 12.0, rename.l - 4.0, p.t + 32.0);
        let line2 = Rect::new(l, p.t + 32.0, p.r - 14.0, p.t + 52.0);
        if let Some(field) = editing {
            field.paint(g, Rect::new(p.l + 8.0, p.t + 8.0, rename.l - 6.0, p.t + 42.0), &item.stem);
        } else if item.missing {
            g.text(item.name(), &g.f.strong, line1, pal().faint);
            g.text(t!("File not found", "Dosya bulunamadı"), &g.f.small, line2, pal().faint);
        } else {
            let (a, b) = two_lines(g, item.name(), line1.w());
            g.text(&a, &g.f.strong, line1, pal().text);
            g.text(&b, &g.f.strong, line2, pal().text);
        }
        if hovered || editing.is_some() {
            icon_button(g, rename, ICON_RENAME, if editing.is_some() { pal().text } else { pal().faint }, self.hover == Hit::Rename(n));
            icon_button(g, remove, ICON_REMOVE, pal().faint, self.hover == Hit::Remove(n));
        }

        if item.hotkey.is_some() || binding {
            let (c, b) = self.chip_colors(binding, item.conflict, self.hover == Hit::Key(n));
            chip(g, self.key_rect(g, n, p), &self.chip_text(item.hotkey, binding), c, b);
        } else if hovered && editing.is_none() {
            icon_button(g, Self::key_glyph(p), ICON_KEYBOARD, pal().faint, self.hover == Hit::Key(n));
        }

        if let Some(pl) = playing {
            let cy = Self::key_cy(p);
            g.text(ICON_STOP, &g.f.icon_small, Rect::new(p.r - 34.0, cy - 11.0, p.r - 12.0, cy + 11.0), accent());
            if pl.duration > 0.0 {
                let t = (pl.started.elapsed().as_secs_f64() / pl.duration).clamp(0.0, 1.0) as f32;
                let bar = Rect::new(p.l + 12.0, p.b - 6.0, p.r - 12.0, p.b - 4.0);
                g.fill(bar, 1.0, pal().line);
                g.fill(Rect::new(bar.l, bar.t, bar.l + bar.w() * t, bar.b), 1.0, accent());
            }
        }
    }

    // --- Görünürlük ve zamanlayıcılar ---

    /// Sayfa ekrana geldi ya da gitti (pencere gizlendi, başka sekmeye geçildi).
    pub fn set_visible(&mut self, visible: bool) {
        if visible && !self.visible {
            for i in &mut self.items {
                i.missing = !i.path.is_file();
            }
            self.refresh_mic();
            if self.tab == Tab::Find && matches!(self.find_state, FindState::Idle) {
                self.search_now();
            }
        }
        if !visible {
            self.end_bind(None);
            self.end_rename(true);
            self.search.blur();
            self.hover = Hit::None;
            self.stop_preview();
            let _ = std::fs::remove_dir_all(preview_dir());
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
        if let Some(id) = self.drag {
            if let Some(v) = self.form().and_then(|f| f.slider_value(id, x)) {
                self.set_level(level_index(id), (v * 100.0).round() as u32);
            }
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
        if self.bind.is_some() && !matches!(hit, Hit::Key(_) | Hit::Form(form::Hit::Chip(_))) {
            self.end_bind(None);
        }
        // Düzenlenen kutunun dışına tıklamak adı kaydeder.
        if let Some((n, _)) = &self.rename
            && hit.tile() != Some(*n)
        {
            self.end_rename(true);
        }
        // Arama kutusu basınca odak alır; başka yere basınca bırakır.
        if hit == Hit::Form(form::Hit::Custom(R_SEARCH)) {
            self.search.focus(false);
            unsafe {
                let _ = SetFocus(Some(self.hwnd));
            }
        } else if self.search.focused {
            self.search.blur();
        }
        if let Hit::Form(form::Hit::Slider(id, v)) = hit {
            self.drag = Some(id);
            self.set_level(level_index(id), (v * 100.0).round() as u32);
        }
        self.redraw();
    }

    pub fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        if self.drag.take().is_some() {
            self.save();
            self.redraw();
            return;
        }
        let hit = self.hit(g, x, y);
        let pressed = std::mem::replace(&mut self.pressed, Hit::None);
        if pressed != Hit::None {
            self.redraw();
        }
        if !hit.same(pressed) {
            return;
        }
        match hit {
            Hit::Tab(t) => self.select(t),
            Hit::Add => self.modal = Some(Modal::AddFiles),
            Hit::Clip => self.save_clip(),
            Hit::Tile(n) if !self.renaming(n) => self.toggle(n),
            Hit::Key(n) => self.start_bind(Bind::Sound(n)),
            Hit::Rename(n) if self.renaming(n) => self.end_rename(true),
            Hit::Rename(n) => self.start_rename(n),
            Hit::Remove(n) => self.remove(n),
            Hit::Form(h) => self.form_click(h),
            Hit::Tile(_) | Hit::None => {}
        }
    }

    fn form_click(&mut self, h: form::Hit) {
        match h {
            form::Hit::Button(R_MIC) if matches!(self.mic, MicState::Detached) => self.run_setup(true),
            form::Hit::Row(R_STOP) => self.player.stop_all(),
            form::Hit::Chip(R_STOP) => self.start_bind(Bind::Stop),
            form::Hit::Row(R_CLIP) => self.save_clip(),
            form::Hit::Chip(R_CLIP) => self.start_bind(Bind::Clip),
            form::Hit::Row(id) => {
                if let Some(n) = found_index(id) {
                    self.toggle_preview(n);
                }
            }
            form::Hit::Button(id) => {
                if let Some(n) = found_index(id) {
                    self.add_found(n);
                }
            }
            _ => {}
        }
    }

    pub fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        let max = self.max_scroll();
        let s = &mut self.scroll[self.tab as usize];
        *s = (*s - delta * WHEEL_STEP).clamp(0.0, max);
        self.hover = self.hit(g, x, y);
        self.redraw();
    }

    /// Kısayol atanıyorsa tuşu yakalar (`true`). Ctrl+F Ses bul'un arama kutusuna gider; Ctrl+Tab
    /// ve Ctrl+1..3 sekmeler arasında geçer. Kutudayken Esc önce seçimi, sonra önizlemeyi, sonra
    /// yazıyı siler, sonra kutuyu bırakır; dışında Esc her şeyi susturur.
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
        if let Some((_, f)) = &mut self.rename {
            match f.key(vk, ctrl) {
                FieldKey::Enter => self.end_rename(true),
                FieldKey::Escape => self.end_rename(false),
                FieldKey::Handled => self.redraw(),
                FieldKey::Ignored => {}
            }
            return true;
        }
        if ctrl && VIRTUAL_KEY(vk) == VK_F {
            self.focus_search();
            return true;
        }
        let n = Tab::ALL.len();
        let i = self.tab as usize;
        if ctrl && VIRTUAL_KEY(vk) == VK_TAB {
            let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            self.select(Tab::ALL[if shift { (i + n - 1) % n } else { (i + 1) % n }]);
            return true;
        }
        if ctrl && (0x31..0x31 + n as u16).contains(&vk) {
            self.select(Tab::ALL[(vk - 0x31) as usize]);
            return true;
        }
        if self.search.focused {
            let before = self.search.text.clone();
            match self.search.key(vk, ctrl) {
                FieldKey::Enter => self.search_now(),
                FieldKey::Escape => {
                    match () {
                        _ if std::mem::take(&mut self.search.selected) => {}
                        _ if self.preview.is_some() || self.want_preview.is_some() => self.stop_preview(),
                        _ if !self.search.text.is_empty() => self.clear_query(),
                        _ => self.search.blur(),
                    }
                    self.redraw();
                }
                FieldKey::Handled if self.search.text != before => self.schedule_search(),
                FieldKey::Handled => self.redraw(),
                FieldKey::Ignored => return false,
            }
            return true;
        }
        if VIRTUAL_KEY(vk) == VK_ESCAPE {
            self.player.stop_all();
            self.stop_preview();
            self.redraw();
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
        self.end_rename(true);
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
                "Could not connect to the microphone.\n\nDetails: %LOCALAPPDATA%\\Programs\\winloom\\winloom.log",
                "Mikrofona bağlanamadı.\n\nAyrıntılar: %LOCALAPPDATA%\\Programs\\winloom\\winloom.log"
            ));
        }
    }
}

// --- Ses bul: arama kutusu, Myinstants, önizleme ---

impl Soundboard {
    /// Arama kutusu odaktayken yazılanlar oraya gider.
    pub fn char(&mut self, c: char) -> bool {
        if let Some((_, f)) = &mut self.rename {
            if f.char(c) {
                self.redraw();
            }
            return true;
        }
        if self.bind.is_some() || !self.search.char(c) {
            return false;
        }
        self.schedule_search();
        true
    }

    /// Ctrl+F: Ses bul'a geçer ve arama kutusuna odaklanır (yazılan seçili); kutu en üstte.
    fn focus_search(&mut self) {
        self.select(Tab::Find);
        self.search.focus(true);
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
        self.scroll[Tab::Find as usize] = 0.0;
        self.redraw();
    }

    fn clear_query(&mut self) {
        self.search.set("");
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
            (self.search_seq, self.search.text.clone(), self.searched.clone(), self.hwnd.0 as usize);
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
                self.find_state = FindState::Done;
            }
            Err(e) => self.find_state = FindState::Error(e),
        }
        self.clamp_scroll();
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
/// "Dosya ekle" penceresi.
fn file_dialog(hwnd: HWND) -> Result<Vec<PathBuf>, String> {
    add_dialog(hwnd).map_err(|e| e.to_string())
}

impl ToolPage for Soundboard {
    fn summary(&self) -> Option<(String, String)> {
        let n = self.items.len();
        Some((t!(format!("{n} sounds"), format!("{n} ses")), self.mic_label().to_string()))
    }

    fn message(
        &mut self,
        _g: &mut ortak::gfx::Gfx,
        msg: u32,
        wp: windows::Win32::Foundation::WPARAM,
        _lp: windows::Win32::Foundation::LPARAM,
    ) -> Option<windows::Win32::Foundation::LRESULT> {
        use windows::Win32::UI::WindowsAndMessaging::{WM_HOTKEY, WM_TIMER};
        match msg {
            WM_TIMER if matches!(wp.0, TIMER_ANIM | TIMER_STATUS | TIMER_SEARCH) => self.timer(wp.0),
            WM_HOTKEY if (HK_STOP..HK_END).contains(&(wp.0 as i32)) => self.hotkey(wp.0 as i32),
            WM_PLAYER => self.player_changed(),
            WM_SEARCHED => self.searched(),
            WM_FETCHED => self.fetched(),
            MM_MCINOTIFY => self.preview_done(wp.0),
            WM_INSTALLED => self.setup_done(wp.0),
            _ => return None,
        }
        Some(windows::Win32::Foundation::LRESULT(0))
    }
    fn binding(&self) -> bool {
        Soundboard::binding(self)
    }
    fn kill_focus(&mut self) {
        Soundboard::kill_focus(self)
    }
    fn add_files(&mut self, paths: Vec<PathBuf>) {
        self.add(paths)
    }
    fn take_file_dialog(&mut self) -> Option<ortak::ui::FileDialog> {
        self.modal.take().map(|Modal::AddFiles| file_dialog as ortak::ui::FileDialog)
    }
    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        Soundboard::layout(self, w, h, head_x, head_r)
    }
    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        self.hit(g, x, y) != Hit::None
    }
    fn paint(&self, g: &Gfx) {
        Soundboard::paint(self, g)
    }
    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        Soundboard::mouse_move(self, g, x, y)
    }
    fn mouse_leave(&mut self) {
        Soundboard::mouse_leave(self)
    }
    fn mouse_down(&mut self, g: &Gfx, x: f32, y: f32) {
        Soundboard::mouse_down(self, g, x, y)
    }
    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        Soundboard::mouse_up(self, g, x, y)
    }
    fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        Soundboard::wheel(self, g, x, y, delta)
    }
    fn key(&mut self, vk: u16) -> bool {
        Soundboard::key(self, vk)
    }
    fn char(&mut self, c: char) -> bool {
        Soundboard::char(self, c)
    }
    fn set_visible(&mut self, visible: bool) {
        Soundboard::set_visible(self, visible)
    }
    fn tip(&self) -> Option<(Rect, String)> {
        let tile = |n| self.tile_rect(n);
        match self.hover {
            Hit::Rename(n) => Some((Self::tile_icons(tile(n)).0, t!("Rename", "Adını değiştir").into())),
            Hit::Remove(n) => Some((Self::tile_icons(tile(n)).1, t!("Remove", "Kaldır").into())),
            // Kısayolu yoksa klavye ikonu: ne işe yaradığı söylenir.
            Hit::Key(n) if self.items.get(n).is_some_and(|i| i.hotkey.is_none()) && self.bind != Some(Bind::Sound(n)) => {
                Some((Self::key_glyph(tile(n)), t!("Set a shortcut", "Kısayol ata").into()))
            }
            _ => None,
        }
    }
}

impl Drop for Soundboard {
    /// Kaldırılınca kısayollar ve zamanlayıcılar bırakılır (pencere winloom'la yaşamaya devam eder).
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
