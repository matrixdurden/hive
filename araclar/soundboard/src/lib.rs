//! soundboard sayfası. Motor (çözme, ortak bellek, kulaklık) motor/'da ve
//! winloom sürecinde çalışır; mikrofon efekti (APO) audiodg.exe içinde.
//!
//! Sayfa ortak sayfa kitiyle (`ortak::form`): başlıkta mikrofonun durumu,
//! altında tek sütunda bölümler.
//! - Mikrofon ve kulaklık: bağlantı, iki ses düzeyi, hepsini susturmanın kısayolu.
//! - Seslerim: her ses bir satır; tıklamak çalar / durdurur, sağda kısayolu, adı düzenle, kaldır.
//! - Ekle: dosyadan ve "Son 10 saniye" (bilgisayarda az önce çalanı listeye ekler, kısayolu da var).
//! - Ses bul: Myinstants araması. Sonuca tıklamak onu kulaklıkta çalar (yalnızca sana, mikrofona
//!   gitmez), "Ekle" listeye indirir.

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

use ortak::form::{self, Ctl, Form, Row};
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

/// Sayfa kitindeki satırların kimlikleri; sesler ve arama sonuçları sıralarıyla (`R_SOUND + n`).
const R_MIC: u32 = 1;
const R_MIC_LEVEL: u32 = 2;
const R_EAR_LEVEL: u32 = 3;
const R_STOP: u32 = 4;
const R_ADD: u32 = 5;
const R_CLIP: u32 = 6;
const R_SEARCH: u32 = 7;
const R_SOUND: u32 = 1_000;
const R_FOUND: u32 = 1_000_000;
/// Ses satırının sağındaki alan: kısayol kutucuğu, adı düzenle ve kaldır.
const SOUND_CTL: f32 = 200.0;
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

/// Satır kimliğinden sesin / sonucun sırası.
fn sound_index(id: u32) -> Option<usize> {
    (R_SOUND..R_FOUND).contains(&id).then(|| (id - R_SOUND) as usize)
}

fn found_index(id: u32) -> Option<usize> {
    (id >= R_FOUND).then(|| (id - R_FOUND) as usize)
}

/// Kaydırıcı satırının düzeyi: 0 mikrofon, 1 kulaklık.
fn level_index(id: u32) -> usize {
    if id == R_MIC_LEVEL { 0 } else { 1 }
}

/// Tıklanan / üstüne gelinen: sayfa kiti ya da ses satırının sağındakiler.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Form(form::Hit),
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

    /// Kitin gördüğü: ses satırının sağındakiler satırın kendisi sayılır (satır belirgin kalsın).
    fn form(self) -> form::Hit {
        match self {
            Hit::Form(h) => h,
            Hit::Key(n) | Hit::Rename(n) | Hit::Remove(n) => form::Hit::Row(R_SOUND + n as u32),
            Hit::None => form::Hit::None,
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
    scroll: f32,
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
            scroll: 0.0,
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
        self.scroll = self.scroll.min(self.max_scroll());
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

    fn form_items(&self) -> Vec<form::Item> {
        use form::Item::{Note, Section};
        let row = form::Item::Row;
        let mut v = vec![Section(t!("Microphone and headphones", "Mikrofon ve kulaklık").into()), row(self.mic_row())];
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

        let mine = t!("My sounds", "Seslerim");
        v.push(Section(if self.items.is_empty() { mine.into() } else { format!("{mine}  {}", self.items.len()) }));
        if self.items.is_empty() {
            let empty = t!(
                "No sounds yet · drop sound files here, add them below or find some on Myinstants",
                "Henüz ses yok · ses dosyalarını buraya sürükle, aşağıdan ekle ya da Myinstants'ta bul"
            );
            v.push(Note(empty.into(), false));
        }
        for (n, item) in self.items.iter().enumerate() {
            let playing = self.playing.iter().any(|p| p.id == item.id);
            // Ad düzenlenirken yerinde kutu çizilir.
            let title = if self.renaming(n) { "" } else { item.name() };
            let sub = if item.missing { t!("File not found", "Dosya bulunamadı") } else { "" };
            v.push(row(
                Row::new(R_SOUND + n as u32, title, sub, Ctl::Space(SOUND_CTL))
                    .icon(if playing { ICON_PAUSE } else { ICON_PLAY })
                    .clickable()
                    .dim(item.missing),
            ));
        }

        v.push(Section(t!("Add", "Ekle").into()));
        v.push(row(
            Row::new(
                R_ADD,
                t!("From files", "Dosyadan"),
                t!("mp3, wav, m4a, aac, wma or flac · or drop them here", "mp3, wav, m4a, aac, wma ya da flac · ya da buraya sürükle"),
                Ctl::Button(t!("Choose files", "Dosya seç").into()),
            )
            .icon(ICON_FOLDER),
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

        v.push(Section(t!("Find sounds on Myinstants", "Myinstants'ta ses bul").into()));
        // Arama kutusu satırın tamamını kaplar (sayfa çizer).
        v.push(row(Row::new(R_SEARCH, "", "", Ctl::Space((self.w - 2.0 * PAD - 32.0).max(0.0)))));
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
            v.push(row(Row::new(R_FOUND + n as u32, f.name.as_str(), "", ctl).icon(icon).clickable()));
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

    fn form(&self) -> Form {
        Form::new(self.form_items(), PAD, self.w - PAD, HEADER + 16.0 - self.scroll)
    }

    fn max_scroll(&self) -> f32 {
        (self.form().bottom() - 24.0 + self.scroll + 12.0 - self.h).max(0.0)
    }

    /// Ses satırının sağındaki ikonlar: (adı düzenle, kaldır).
    fn sound_icons(&self, f: &Form, n: usize) -> Option<(Rect, Rect)> {
        let c = f.control(R_SOUND + n as u32)?;
        let remove = Rect::new(c.r - 28.0, c.t + 3.0, c.r, c.b - 3.0);
        Some((Rect::new(remove.l - 32.0, remove.t, remove.l - 4.0, remove.b), remove))
    }

    /// Ses satırının kısayol kutucuğu: ikonların solunda.
    fn sound_key(&self, g: &Gfx, f: &Form, n: usize) -> Option<Rect> {
        let (rename, _) = self.sound_icons(f, n)?;
        let item = self.items.get(n)?;
        let text = self.chip_text(item.hotkey, self.bind == Some(Bind::Sound(n)));
        Some(chip_rect(g, rename.l - 8.0, rename.cy(), &text))
    }

    /// Kısayol kutucuğunun renkleri: (yazı, çerçeve).
    fn chip_colors(&self, binding: bool, conflict: bool, unset: bool, hovered: bool) -> (Color, Color) {
        match () {
            _ if binding => (accent(), accent()),
            _ if conflict => (pal().red, pal().line),
            _ if unset => (pal().faint, pal().line),
            _ if hovered => (pal().text, pal().faint),
            _ => (pal().muted, pal().line),
        }
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return Hit::None;
        }
        let f = self.form();
        let h = f.hit(g, x, y);
        if let form::Hit::Row(id) = h
            && let Some(n) = sound_index(id)
            && let (Some((rename, remove)), Some(key)) = (self.sound_icons(&f, n), self.sound_key(g, &f, n))
        {
            return match () {
                _ if remove.contains(x, y) => Hit::Remove(n),
                _ if rename.contains(x, y) => Hit::Rename(n),
                _ if !self.renaming(n) && key.contains(x, y) => Hit::Key(n),
                _ => Hit::Form(h),
            };
        }
        if h == form::Hit::None { Hit::None } else { Hit::Form(h) }
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

        let f = self.form();
        g.clip(Rect::new(0.0, HEADER, self.w, self.h), || {
            f.paint(g, self.hover.form(), self.drag);
            self.paint_sounds(g, &f);
            if let Some(r) = f.control(R_SEARCH) {
                self.search.paint(g, r, t!("Search Myinstants…", "Myinstants'ta ara…"));
            }
        });
    }

    /// Ses satırlarının sayfaca çizilenleri: ad kutusu, kısayol, ikonlar ve çalma çubuğu.
    fn paint_sounds(&self, g: &Gfx, f: &Form) {
        for (n, item) in self.items.iter().enumerate() {
            let id = R_SOUND + n as u32;
            let (Some(r), Some((rename, remove)), Some(key)) = (f.row(id), self.sound_icons(f, n), self.sound_key(g, f, n))
            else {
                continue;
            };
            if r.b < HEADER || r.t > self.h {
                continue;
            }
            let hovered = self.hover.form().id() == Some(id);
            let editing = self.rename.as_ref().filter(|(i, _)| *i == n).map(|(_, f)| f);
            let binding = self.bind == Some(Bind::Sound(n));
            if let Some(field) = editing {
                field.paint(g, Rect::new(r.l + 48.0, r.cy() - 17.0, key.l - 12.0, r.cy() + 17.0), &item.stem);
            } else if item.hotkey.is_some() || binding || hovered {
                let (c, b) = self.chip_colors(binding, item.conflict, item.hotkey.is_none(), self.hover == Hit::Key(n));
                chip(g, key, &self.chip_text(item.hotkey, binding), c, b);
            }
            if hovered || editing.is_some() {
                icon_button(g, rename, ICON_RENAME, if editing.is_some() { pal().text } else { pal().faint }, self.hover == Hit::Rename(n));
                icon_button(g, remove, ICON_REMOVE, pal().faint, self.hover == Hit::Remove(n));
            }
            if let Some(p) = self.playing.iter().find(|p| p.id == item.id)
                && p.duration > 0.0
            {
                let t = (p.started.elapsed().as_secs_f64() / p.duration).clamp(0.0, 1.0) as f32;
                let (a, b) = (r.l + 16.0, r.r - 16.0);
                g.fill(Rect::new(a, r.b - 3.0, a + (b - a) * t, r.b - 1.0), 1.0, accent().alpha(0.7));
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
            // Ses bul ilk açılışta Myinstants'ın öne çıkanlarıyla dolar.
            if matches!(self.find_state, FindState::Idle) {
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
            if let Some(v) = self.form().slider_value(id, x) {
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
        // Düzenlenen satırın dışına tıklamak adı kaydeder.
        if let Some((n, _)) = &self.rename
            && hit.form() != form::Hit::Row(R_SOUND + *n as u32)
        {
            self.end_rename(true);
        }
        // Arama kutusu basınca odak alır; başka yere basınca bırakır.
        if hit == Hit::Form(form::Hit::Row(R_SEARCH)) {
            self.search.focus(false);
            unsafe {
                let _ = SetFocus(Some(self.hwnd));
            }
            self.redraw();
        } else if self.search.focused {
            self.search.blur();
            self.redraw();
        }
        if let Hit::Form(form::Hit::Slider(id, v)) = hit {
            self.drag = Some(id);
            self.set_level(level_index(id), (v * 100.0).round() as u32);
        }
    }

    pub fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        if self.drag.take().is_some() {
            self.save();
            self.redraw();
            return;
        }
        let hit = self.hit(g, x, y);
        if !hit.same(std::mem::replace(&mut self.pressed, Hit::None)) {
            return;
        }
        match hit {
            Hit::Key(n) => self.start_bind(Bind::Sound(n)),
            Hit::Rename(n) if self.renaming(n) => self.end_rename(true),
            Hit::Rename(n) => self.start_rename(n),
            Hit::Remove(n) => self.remove(n),
            Hit::Form(h) => self.form_click(h),
            Hit::None => {}
        }
    }

    fn form_click(&mut self, h: form::Hit) {
        match h {
            form::Hit::Button(R_MIC) if matches!(self.mic, MicState::Detached) => self.run_setup(true),
            form::Hit::Row(R_STOP) => self.player.stop_all(),
            form::Hit::Chip(R_STOP) => self.start_bind(Bind::Stop),
            form::Hit::Button(R_ADD) => self.modal = Some(Modal::AddFiles),
            form::Hit::Row(R_CLIP) => self.save_clip(),
            form::Hit::Chip(R_CLIP) => self.start_bind(Bind::Clip),
            form::Hit::Row(id) => {
                if let Some(n) = sound_index(id) {
                    if !self.renaming(n) {
                        self.toggle(n);
                    }
                } else if let Some(n) = found_index(id) {
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
        self.scroll = (self.scroll - delta * WHEEL_STEP).clamp(0.0, self.max_scroll());
        self.hover = self.hit(g, x, y);
        self.redraw();
    }

    /// Kısayol atanıyorsa tuşu yakalar (`true`). Ctrl+F arama kutusuna gider; kutudayken Esc
    /// önce seçimi, sonra önizlemeyi, sonra yazıyı siler, sonra kutuyu bırakır; dışında Esc
    /// her şeyi susturur.
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

    /// Ctrl+F: arama kutusuna odaklanır (yazılan seçili), görünmüyorsa oraya kaydırır.
    fn focus_search(&mut self) {
        self.end_rename(true);
        self.search.focus(true);
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
        if matches!(self.find_state, FindState::Idle) {
            self.search_now();
        }
        if let Some(r) = self.form().row(R_SEARCH)
            && (r.t < HEADER || r.b > self.h)
        {
            // Bölüm başlığı da görünsün.
            self.scroll = (self.scroll + r.t - HEADER - 52.0).clamp(0.0, self.max_scroll());
        }
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
        let icons = |n| self.sound_icons(&self.form(), n);
        match self.hover {
            Hit::Rename(n) => icons(n).map(|(r, _)| (r, t!("Rename", "Adını değiştir").into())),
            Hit::Remove(n) => icons(n).map(|(_, r)| (r, t!("Remove", "Kaldır").into())),
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
