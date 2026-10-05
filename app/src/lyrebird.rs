//! lyrebird sayfası. Motor (çözme, ortak bellek, kulaklık, kısayollar) ../lyrebird'de ve
//! hive sürecinde çalışır; mikrofon efekti (APO) audiodg.exe içinde.
//!
//! Sayfa: üstte mikrofon durumu ve düğmeler, ortada ses listesi, altta iki ses düzeyi.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use crate::gfx::{Color, Gfx, Rect};
use crate::log;
use crate::ui::*;
use crate::util::{Res, error_box};
use lyrebird_motor::bus::{self, Bus};
use lyrebird_motor::config::{self, Config, Sound};
use lyrebird_motor::decode::EXTENSIONS;
use lyrebird_motor::hotkey::{self, Hotkey};
use lyrebird_motor::install::Mic;
use lyrebird_motor::monitor::Monitor;
use lyrebird_motor::player::{Player, Playing};
pub use lyrebird_motor::{data_dir, install, installed, probe};

const ACCENT: Color = Color::rgb(0xfbbf24);

pub const WM_PLAYER: u32 = WM_APP + 20;
pub const WM_INSTALLED: u32 = WM_APP + 21;
pub const TIMER_ANIM: usize = 101;
pub const TIMER_STATUS: usize = 102;
/// Kısayol kimlikleri: durdur ve her ses (1100 + sıra).
pub const HK_STOP: i32 = 1001;
pub const HK_SOUND: i32 = 1100;

/// Yükseltilmiş kurulumun komut satırı bayrakları.
pub const ARG_INSTALL: &str = "--lyrebird-kur";
pub const ARG_UNINSTALL: &str = "--lyrebird-kaldir";

const BOTTOM: f32 = 60.0;
/// Listenin ilk satırı: başlık çizgisinin biraz altı.
const LIST_T: f32 = HEADER + 6.0;
const ROW: f32 = 40.0;

/// `--lyrebird-kur` / `--lyrebird-kaldir`: yükseltilmiş olarak çalışır, çıkış kodu sonucu bildirir.
pub fn setup(install: bool) -> i32 {
    match lyrebird_motor::setup(install) {
        Ok(()) => {
            log!("lyrebird {} tamam", if install { "kurulumu" } else { "kaldırma" });
            0
        }
        Err(e) => {
            log!("lyrebird kurulum hatası: {e}");
            println!("HATA {e}");
            1
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Status,
    Add,
    Stop,
    StopKey,
    Row(usize),
    RowKey(usize),
    RowRemove(usize),
    Slider(usize),
    Empty,
}

#[derive(Clone, Copy, PartialEq)]
enum Bind {
    Stop,
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
                for i in &mut self.items {
                    if i.hotkey == key {
                        i.hotkey = None;
                    }
                }
            }
            match b {
                Bind::Stop => self.stop = key,
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
            self.player.toggle(item.id, item.path);
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

    // --- Yerleşim (sayfanın kendi köşesine göre) ---

    fn max_scroll(&self) -> f32 {
        (self.items.len() as f32 * ROW + 14.0 - (self.h - HEADER - BOTTOM)).max(0.0)
    }

    fn add_rect(&self) -> Rect {
        Rect::new(self.head_r - 36.0, HEAD_CY - 18.0, self.head_r, HEAD_CY + 18.0)
    }

    fn stop_rect(&self) -> Rect {
        Rect::new(self.head_r - 74.0, HEAD_CY - 18.0, self.head_r - 38.0, HEAD_CY + 18.0)
    }

    fn chip_text(&self, key: Option<Hotkey>, binding: bool) -> String {
        match (binding, key) {
            (true, _) => "tuşa bas".into(),
            (false, Some(k)) => k.to_string(),
            (false, None) => "kısayol".into(),
        }
    }

    fn stop_key_rect(&self, g: &Gfx) -> Rect {
        let text = self.chip_text(self.stop, self.bind == Some(Bind::Stop));
        chip_rect(g, self.head_r - 80.0, HEAD_CY, &text)
    }

    /// Başlıktaki mikrofon rozeti: bağlıyken "Bağlı" (ad balonda), değilken "Mikrofona bağla"
    /// düğmesi. Tıklanabilir alan yalnızca rozetin kendisi.
    fn pill_label(&self) -> &'static str {
        match &self.mic {
            MicState::Unknown => "",
            MicState::Missing => "Mikrofon yok",
            MicState::Busy => "Bağlanıyor…",
            MicState::Detached => "Mikrofona bağla",
            MicState::Attached(_) => "Bağlı",
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

    /// Boş listede ortadaki "Ses ekle" düğmesi.
    fn empty_rect(&self, g: &Gfx) -> Rect {
        let cy = (HEADER + self.h - BOTTOM) / 2.0;
        let b = button_rect(g, 0.0, cy + 34.0, "Ses ekle", true);
        let l = self.w / 2.0 - b.w() / 2.0;
        Rect::new(l, b.t, l + b.w(), b.b)
    }

    fn row_rect(&self, n: usize) -> Rect {
        let t = LIST_T + n as f32 * ROW - self.scroll;
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

    /// (bölge, iz başı, iz sonu)
    fn slider(&self, k: usize) -> (Rect, f32, f32) {
        let (w, h) = (self.w, self.h);
        let (l, r) = if k == 0 { (PAD, w / 2.0 - 10.0) } else { (w / 2.0 + 10.0, w - PAD) };
        (Rect::new(l, h - BOTTOM + 8.0, r, h - 8.0), l + 30.0, r - 34.0)
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return if self.add_rect().contains(x, y) {
                Hit::Add
            } else if self.stop_rect().contains(x, y) {
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
        if self.items.is_empty() {
            return if self.empty_rect(g).contains(x, y) { Hit::Empty } else { Hit::None };
        }
        let n = ((y - LIST_T + self.scroll) / ROW).floor();
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

    fn slide_to(&mut self, k: usize, x: f32) {
        let (_, a, b) = self.slider(k);
        self.set_level(k, (((x - a) / (b - a)).clamp(0.0, 1.0) * 100.0).round() as u32);
    }

    // --- Çizim ---

    pub fn paint(&self, g: &Gfx) {
        let (w, h) = (self.w, self.h);
        let playing_any = !self.playing.is_empty();

        // Üst çubuk: mikrofon rozeti, sustur, ekle.
        let label = self.pill_label();
        if !label.is_empty() {
            let p = self.pill_rect(g);
            let hovered = self.hover == Hit::Status;
            if matches!(self.mic, MicState::Detached) {
                g.fill(p, 13.0, ACCENT.alpha(if hovered { 0.88 } else { 1.0 }));
                g.text(label, &g.f.button, p, ON_ACCENT);
            } else {
                g.fill(p, 13.0, HOVER);
                let dot = match &self.mic {
                    MicState::Attached(_) if self.active => GREEN,
                    MicState::Busy => ACCENT,
                    _ => FAINT,
                };
                g.circle(p.l + 14.0, p.cy(), 3.5, dot);
                g.text(label, &g.f.small, Rect::new(p.l + 24.0, p.t, p.r, p.b), if hovered { TEXT } else { MUTED });
            }
        }

        let stop = self.stop_rect();
        icon_button(g, stop, ICON_MUTE, if playing_any { ACCENT } else { MUTED }, self.hover == Hit::Stop);
        let binding = self.bind == Some(Bind::Stop);
        if self.stop.is_some() || binding || matches!(self.hover, Hit::Stop | Hit::StopKey) {
            let text = self.chip_text(self.stop, binding);
            let (c, b) = match () {
                _ if binding => (ACCENT, ACCENT),
                _ if self.stop_conflict => (RED, LINE),
                _ if self.stop.is_none() => (FAINT, LINE),
                _ => (MUTED, LINE),
            };
            chip(g, self.stop_key_rect(g), &text, c, b);
        }
        icon_button(g, self.add_rect(), ICON_ADD, MUTED, self.hover == Hit::Add);

        // Liste.
        let list = Rect::new(0.0, HEADER, w, h - BOTTOM);
        if self.items.is_empty() {
            let cy = list.cy();
            g.text(ICON_AUDIO, &g.f.icon_large, Rect::new(0.0, cy - 62.0, w, cy - 22.0), FAINT);
            text_center(g, "Ses dosyalarını buraya sürükle", &g.f.strong, w / 2.0, cy - 14.0, cy + 8.0, TEXT);
            g.text("mp3, wav, m4a, aac, wma ya da flac", &g.f.small_center, Rect::new(0.0, cy + 8.0, w, cy + 26.0), MUTED);
            let b = self.empty_rect(g);
            button(g, b, "Ses ekle", Some(ICON_ADD), Some(ACCENT), self.hover == Hit::Empty);
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
                    g.fill(r, 8.0, ACCENT.alpha(if hovered { 0.12 } else { 0.08 }));
                } else if hovered {
                    g.fill(r, 8.0, HOVER);
                }
                let binding = self.bind == Some(Bind::Sound(n));
                let key = self.row_key_rect(g, n);
                let show_chip = item.hotkey.is_some() || binding || hovered;
                let name_right = if show_chip { key.l - 10.0 } else { w - 56.0 };
                let (icon, icon_color) =
                    if playing.is_some() { (ICON_PAUSE, ACCENT) } else { (ICON_PLAY, if hovered { MUTED } else { FAINT }) };
                g.text(icon, &g.f.icon_small, Rect::new(r.l + 8.0, r.t, r.l + 28.0, r.b), icon_color);
                let name_color = match () {
                    _ if item.missing => FAINT,
                    _ if playing.is_some() => ACCENT,
                    _ => TEXT,
                };
                g.text(&item.name, &g.f.text, Rect::new(r.l + 36.0, r.t, name_right, r.b), name_color);
                if show_chip {
                    let text = self.chip_text(item.hotkey, binding);
                    let (c, b) = match () {
                        _ if binding => (ACCENT, ACCENT),
                        _ if item.conflict => (RED, LINE),
                        _ if item.hotkey.is_none() => (FAINT, LINE),
                        _ if self.hover == Hit::RowKey(n) => (TEXT, FAINT),
                        _ => (MUTED, LINE),
                    };
                    chip(g, key, &text, c, b);
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
                    g.fill(Rect::new(a, r.b - 3.0, a + (b - a) * t, r.b - 1.0), 1.0, ACCENT.alpha(0.7));
                }
            }
        });

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
            slider(g, a, b, r.cy(), self.level[k] as f32 / 100.0, ACCENT, active);
            g.text(
                &self.level[k].to_string(),
                &g.f.small_right,
                Rect::new(b + 6.0, r.t, r.r, r.b),
                if active { TEXT } else { MUTED },
            );
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
        }
        if !visible {
            self.end_bind(None);
            self.hover = Hit::None;
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
        if self.bind.is_some() && !matches!(hit, Hit::RowKey(_) | Hit::StopKey) {
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
            Hit::Add | Hit::Empty => self.modal = Some(Modal::AddFiles),
            Hit::Stop => self.player.stop_all(),
            Hit::StopKey => self.start_bind(Bind::Stop),
            Hit::Status if matches!(self.mic, MicState::Detached) => self.run_setup(true),
            Hit::Row(n) => self.toggle(n),
            Hit::RowKey(n) => self.start_bind(Bind::Sound(n)),
            Hit::RowRemove(n) => self.remove(n),
            _ => {}
        }
    }

    pub fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        match self.hit(g, x, y) {
            Hit::Slider(k) => {
                self.set_level(k, (self.level[k] as f32 + delta * 5.0).round().max(0.0) as u32);
                self.save();
            }
            _ => {
                self.scroll = (self.scroll - delta * ROW).clamp(0.0, self.max_scroll());
                self.hover = self.hit(g, x, y);
                self.redraw();
            }
        }
    }

    /// Kısayol atanıyorsa tuşu yakalar (`true`); Esc her şeyi durdurur.
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
            id if id >= HK_SOUND => self.toggle((id - HK_SOUND) as usize),
            _ => {}
        }
    }

    pub fn timer(&mut self, id: usize) {
        match id {
            TIMER_ANIM => self.redraw(),
            TIMER_STATUS => self.refresh_mic(),
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
            error_box("Mikrofona bağlanamadı.\n\nAyrıntılar: %LOCALAPPDATA%\\Programs\\hive\\hive.log");
        }
    }
}

fn add_dialog(hwnd: HWND) -> Res<Vec<PathBuf>> {
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dlg.SetOptions(dlg.GetOptions()? | FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)?;
        dlg.SetTitle(w!("Ses ekle"))?;
        dlg.SetFileTypes(&[COMDLG_FILTERSPEC {
            pszName: w!("Ses dosyaları"),
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
    fn set_visible(&mut self, visible: bool) {
        Lyrebird::set_visible(self, visible)
    }
    fn tip(&self) -> Option<(Rect, String)> {
        match (&self.hover, &self.mic) {
            (Hit::Stop, _) => Some((self.stop_rect(), "Hepsini sustur".into())),
            (Hit::Add, _) => Some((self.add_rect(), "Ses ekle".into())),
            (Hit::Status, MicState::Attached(name)) => {
                let state = if self.active { "bir uygulama dinliyor" } else { "şu an dinleyen yok" };
                Some((Rect::new(self.head_x + 14.0, HEAD_CY - 13.0, self.head_x + 90.0, HEAD_CY + 13.0), format!("{name} · {state}")))
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
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_ANIM);
            let _ = KillTimer(Some(self.hwnd), TIMER_STATUS);
        }
    }
}
