//! clicker: klasik otomatik tıklayıcı. Tetik tuşuna basılı tutunca (ya da aç/kapa modunda bir
//! kez basınca) imlecin olduğu yere seçilen fare düğmesiyle, seçilen hızda tıklar; istenirse
//! belli sayıda tıklamadan sonra kendiliğinden durur.
//!
//! Tetik tuşu RegisterHotKey ile alınır: klavyedeki her tuş (değiştiricilerle birlikte) olur,
//! fare düğmesi olmaz. Basılı tutma modunda tuşun bırakıldığı kısa aralıklarla GetAsyncKeyState
//! ile anlaşılır. Tıklamalar ayrı bir iş parçacığında SendInput ile gider: zamanlaması arayüzden
//! bağımsız, sürüklenmez (her tıklama başlangıca göre hesaplanır).

#[macro_use]
extern crate ortak;

mod tool;
pub use tool::TOOL;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use ortak::hotkey::{self, Hotkey};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use ortak::gfx::{Gfx, Rect};
use ortak::osd;
use ortak::ui::*;
use ortak::util;

/// Tıklama sayısı doldu (tıklama iş parçacığından).
pub const WM_DONE: u32 = WM_APP + 60;
/// Tetik tuşunun kısayol kimliği (soundboard 1001.., audio 5001..).
pub const HK_TRIGGER: i32 = 6001;
/// Basılı tutma modunda tuş bırakıldı mı diye bakılır.
pub const TIMER_HOLD: usize = 601;
/// Tıklarken sayaç ekranda tazelenir.
pub const TIMER_UI: usize = 602;

const HOLD_MS: u32 = 30;
const UI_MS: u32 = 100;
const ROW: f32 = 60.0;
const ICON_MOUSE: &str = "\u{E962}";
const ICON_STOP: &str = "\u{E71A}";
/// Aracın rengi (ikonuyla aynı), bildirimde.
const COLOR: u32 = 0xA3E635;

/// Hız kaydırıcısı: saniyede tıklama, logaritmik.
const CPS_MIN: f32 = 0.5;
const CPS_MAX: f32 = 100.0;
/// Sayı kaydırıcısı: en solda sınırsız, gerisi 1..LIMIT_MAX logaritmik.
const LIMIT_MAX: f32 = 10000.0;
const LIMIT_OFF: f32 = 0.05;

const DEFAULT_KEY: &str = "F6";

pub fn dir() -> PathBuf {
    util::data_dir().join("clicker")
}

fn ini() -> PathBuf {
    dir().join("ayarlar.ini")
}

pub fn installed() -> bool {
    dir().is_dir()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Tuş basılıyken tıklar.
    Hold,
    /// Bir basış başlatır, bir basış durdurur.
    Toggle,
}

impl Mode {
    const ALL: [Mode; 2] = [Mode::Hold, Mode::Toggle];

    fn id(self) -> &'static str {
        match self {
            Mode::Hold => "basili",
            Mode::Toggle => "ackapa",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Mode::Hold => t!("Hold", "Basılı tut"),
            Mode::Toggle => t!("Toggle", "Aç / kapa"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Button {
    Left,
    Right,
    Middle,
}

impl Button {
    const ALL: [Button; 3] = [Button::Left, Button::Right, Button::Middle];

    fn id(self) -> &'static str {
        match self {
            Button::Left => "sol",
            Button::Right => "sag",
            Button::Middle => "orta",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Button::Left => t!("Left", "Sol"),
            Button::Right => t!("Right", "Sağ"),
            Button::Middle => t!("Middle", "Orta"),
        }
    }

    fn flags(self) -> (MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS) {
        match self {
            Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
            Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
            Button::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        }
    }
}

#[derive(Clone)]
struct Settings {
    /// Kapalıyken tetik tuşu alınmaz; ayarlar durur.
    enabled: bool,
    /// Dosyada yoksa F6; boş bırakılan kapalıdır.
    key: Option<Hotkey>,
    mode: Mode,
    button: Button,
    /// Saniyede tıklama.
    cps: f32,
    /// Bu kadar tıklamadan sonra durur; 0 sınırsız.
    limit: u32,
}

impl Settings {
    fn load() -> Self {
        let mut s = Settings { enabled: true, key: Hotkey::parse(DEFAULT_KEY), mode: Mode::Hold, button: Button::Left, cps: 10.0, limit: 0 };
        let text = std::fs::read_to_string(ini()).unwrap_or_default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "acik" => s.enabled = v != "0",
                "tus" => s.key = Hotkey::parse(v),
                "mod" => s.mode = Mode::ALL.into_iter().find(|m| m.id() == v).unwrap_or(s.mode),
                "dugme" => s.button = Button::ALL.into_iter().find(|b| b.id() == v).unwrap_or(s.button),
                "hiz" => s.cps = v.parse().map_or(s.cps, |c: f32| c.clamp(CPS_MIN, CPS_MAX)),
                "sayi" => s.limit = v.parse().unwrap_or(s.limit),
                _ => {}
            }
        }
        s
    }

    fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(dir())?;
        let text = format!(
            "acik={}\ntus={}\nmod={}\ndugme={}\nhiz={}\nsayi={}\n",
            self.enabled as u8,
            self.key.map(|k| k.to_string()).unwrap_or_default(),
            self.mode.id(),
            self.button.id(),
            self.cps,
            self.limit
        );
        std::fs::write(ini(), text)
    }
}

pub fn install() -> Result<(), String> {
    Settings::load().save().map_err(|e| format!("{}: {e}", ini().display()))
}

pub fn uninstall() -> Result<(), String> {
    match std::fs::remove_dir_all(dir()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{} {}: {e}", t!("could not delete", "silinemedi:"), dir().display()))
        }
        _ => Ok(()),
    }
}

pub fn leftovers() -> Vec<String> {
    let d = dir();
    d.exists().then(|| d.display().to_string()).into_iter().collect()
}

// --- Tıklama ---

/// Süren tıklama: durdurma bayrağı ve sayaç. Düşürülünce iş parçacığı bir sonraki bakışta biter.
struct Run {
    stop: Arc<AtomicBool>,
    count: Arc<AtomicU32>,
}

impl Run {
    fn count(&self) -> u32 {
        self.count.load(Ordering::Relaxed)
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// İmlecin olduğu yere bir basış ve bırakış.
fn click(button: Button) {
    let (down, up) = button.flags();
    let input = |flags| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    };
    unsafe {
        SendInput(&[input(down), input(up)], size_of::<INPUT>() as i32);
    }
}

/// Tıklamayı ayrı iş parçacığında başlatır; sayaç `from`'dan sürer (hız değişince yeniden
/// başlatılır). Sayı dolunca pencereye WM_DONE gelir.
fn start_run(s: &Settings, hwnd: usize, from: u32) -> Run {
    let run = Run { stop: Arc::new(AtomicBool::new(false)), count: Arc::new(AtomicU32::new(from)) };
    let (stop, count) = (run.stop.clone(), run.count.clone());
    let (button, limit) = (s.button, s.limit);
    let period = Duration::from_secs_f32(1.0 / s.cps.clamp(CPS_MIN, CPS_MAX));
    std::thread::spawn(move || {
        // Sleep 15 ms'ye kadar kaba olabilir: bu iş parçacığı sürerken 1 ms'ye iner.
        unsafe {
            let _ = timeBeginPeriod(1);
        }
        let start = Instant::now();
        let mut i = 0u32;
        while !stop.load(Ordering::Relaxed) {
            click(button);
            i += 1;
            let n = from + i;
            count.store(n, Ordering::Relaxed);
            if limit > 0 && n >= limit {
                unsafe {
                    let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_DONE, WPARAM(0), LPARAM(0));
                }
                break;
            }
            // Bir sonraki tıklama başlangıca göre: aralıklar birikmez. Son milisaniye beklenmez.
            let next = start + period * i;
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let now = Instant::now();
                if now >= next {
                    break;
                }
                let left = next - now;
                if left > Duration::from_millis(1) {
                    std::thread::sleep(left - Duration::from_millis(1));
                } else {
                    std::thread::yield_now();
                }
            }
        }
        unsafe {
            let _ = timeEndPeriod(1);
        }
    });
    run
}

// --- Kaydırıcılar ---

fn cps_to_v(cps: f32) -> f32 {
    ((cps / CPS_MIN).ln() / (CPS_MAX / CPS_MIN).ln()).clamp(0.0, 1.0)
}

/// Kaydırıcıdan hız: hızlıda tam sayı, ortada yarım, yavaşta onda bir adımlarla.
fn v_to_cps(v: f32) -> f32 {
    quant_cps(CPS_MIN * (CPS_MAX / CPS_MIN).powf(v.clamp(0.0, 1.0)))
}

fn quant_cps(c: f32) -> f32 {
    let q = if c >= 10.0 {
        c.round()
    } else if c >= 1.0 {
        (c * 2.0).round() / 2.0
    } else {
        (c * 10.0).round() / 10.0
    };
    q.clamp(CPS_MIN, CPS_MAX)
}

fn limit_to_v(n: u32) -> f32 {
    if n == 0 { 0.0 } else { LIMIT_OFF + (n as f32).log10() / LIMIT_MAX.log10() * (1.0 - LIMIT_OFF) }
}

fn v_to_limit(v: f32) -> u32 {
    if v < LIMIT_OFF {
        return 0;
    }
    let n = 10f32.powf((v.min(1.0) - LIMIT_OFF) / (1.0 - LIMIT_OFF) * LIMIT_MAX.log10());
    quant_limit(n)
}

/// Yuvarlak sayılar: 20'ye kadar birer, 200'e kadar beşer, 2000'e kadar ellişer, sonra beş yüzer.
fn quant_limit(n: f32) -> u32 {
    let step = if n < 20.0 {
        1.0
    } else if n < 200.0 {
        5.0
    } else if n < 2000.0 {
        50.0
    } else {
        500.0
    };
    ((n / step).round() * step).clamp(1.0, LIMIT_MAX) as u32
}

fn cps_step(c: f32) -> f32 {
    if c >= 10.0 {
        1.0
    } else if c >= 1.0 {
        0.5
    } else {
        0.1
    }
}

fn limit_step(n: u32) -> u32 {
    if n < 20 {
        1
    } else if n < 200 {
        5
    } else if n < 2000 {
        50
    } else {
        500
    }
}

fn fmt_cps(cps: f32) -> String {
    let trim = |v: f32| {
        let s = format!("{v:.1}");
        s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
    };
    if cps < 1.0 {
        t!(format!("every {} s", trim(1.0 / cps)), format!("{} sn'de bir", trim(1.0 / cps)))
    } else {
        t!(format!("{} / s", trim(cps)), format!("{} / sn", trim(cps)))
    }
}

fn fmt_limit(n: u32) -> String {
    if n == 0 { "∞".into() } else { t!(format!("{n} clicks"), format!("{n} tıklama")) }
}

// --- Sayfa ---

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    /// Aracı aç / kapa.
    Enabled,
    Key,
    Mode(Mode),
    Button(Button),
    Speed,
    Limit,
    Stop,
}

/// Sayfanın bölümleri (tetik, tıklama): başlıkların üst kenarı, kutular ve satırlar.
struct Layout {
    titles: [f32; 2],
    panels: [Rect; 2],
    rows: [Vec<Rect>; 2],
    bottom: f32,
}

pub struct Clicker {
    hwnd: HWND,
    settings: Settings,
    run: Option<Run>,
    /// Tetik kayıtlı; tuş varken değilse başka bir uygulama tutuyor.
    registered: bool,
    /// Tetik tuşu atanıyor.
    bind: bool,
    visible: bool,
    /// Sürüklenen kaydırıcı.
    drag: Option<Hit>,
    scroll: f32,
    hover: Hit,
    pressed: Hit,
    w: f32,
    h: f32,
    head_x: f32,
    head_r: f32,
}

impl Clicker {
    pub fn start(hwnd: HWND) -> Self {
        let mut t = Self {
            hwnd,
            settings: Settings::load(),
            run: None,
            registered: false,
            bind: false,
            visible: false,
            drag: None,
            scroll: 0.0,
            hover: Hit::None,
            pressed: Hit::None,
            w: 0.0,
            h: 0.0,
            head_x: 0.0,
            head_r: 0.0,
        };
        t.register();
        t
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn running(&self) -> bool {
        self.run.is_some()
    }

    fn key_name(&self) -> String {
        self.settings.key.map(|k| k.to_string()).unwrap_or_default()
    }

    // --- Başlat / durdur ---

    fn begin(&mut self) {
        if self.running() {
            return;
        }
        self.run = Some(start_run(&self.settings, self.hwnd.0 as usize, 0));
        unsafe {
            if self.settings.mode == Mode::Hold {
                SetTimer(Some(self.hwnd), TIMER_HOLD, HOLD_MS, None);
            }
            if self.visible {
                SetTimer(Some(self.hwnd), TIMER_UI, UI_MS, None);
            }
        }
        osd::show(ICON_MOUSE, COLOR, t!("Clicking", "Tıklıyor"));
        self.redraw();
    }

    /// `tell`: ekranın altında kaç tıklandığı görünsün (basılı tutma bırakılınca gerekmez).
    fn end(&mut self, tell: bool) {
        let Some(run) = self.run.take() else { return };
        let n = run.count();
        drop(run);
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HOLD);
            let _ = KillTimer(Some(self.hwnd), TIMER_UI);
        }
        if tell {
            osd::show(ICON_MOUSE, COLOR, &t!(format!("Stopped · {n} clicks"), format!("Durdu · {n} tıklama")));
        }
        self.redraw();
    }

    /// Hız ya da düğme değişti: tıklama sürüyorsa yenisiyle devam eder, sayaç kaldığı yerden.
    fn restart(&mut self) {
        if let Some(run) = self.run.take() {
            let from = run.count();
            drop(run);
            self.run = Some(start_run(&self.settings, self.hwnd.0 as usize, from));
        }
    }

    fn hotkey(&mut self) {
        match self.settings.mode {
            Mode::Hold => self.begin(),
            Mode::Toggle if self.running() => self.end(true),
            Mode::Toggle => self.begin(),
        }
    }

    fn timer(&mut self, id: usize) {
        match id {
            TIMER_HOLD => {
                let down = self.settings.key.is_some_and(|k| unsafe { GetAsyncKeyState(k.vk as i32) } as u16 & 0x8000 != 0);
                if !down {
                    self.end(false);
                }
            }
            TIMER_UI => self.redraw(),
            _ => {}
        }
    }

    // --- Tetik tuşu ---

    fn unregister(&mut self) {
        if self.registered {
            unsafe {
                let _ = UnregisterHotKey(Some(self.hwnd), HK_TRIGGER);
            }
            self.registered = false;
        }
    }

    fn register(&mut self) {
        self.unregister();
        if !self.settings.enabled {
            return;
        }
        self.registered = self.settings.key.is_some_and(|h| {
            unsafe { RegisterHotKey(Some(self.hwnd), HK_TRIGGER, h.mods | MOD_NOREPEAT, h.vk as u32) }.is_ok()
        });
    }

    fn conflict(&self) -> bool {
        self.settings.enabled && self.settings.key.is_some() && !self.registered
    }

    /// Kapatınca süren tıklama durur, tetik tuşu bırakılır; ayarlar olduğu gibi kalır.
    fn set_enabled(&mut self, on: bool) {
        if self.settings.enabled == on {
            return;
        }
        self.end(true);
        self.end_bind(None);
        self.settings.enabled = on;
        self.save();
        self.register();
        self.redraw();
    }

    fn start_bind(&mut self) {
        self.end(true);
        self.unregister();
        self.bind = true;
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
        self.redraw();
    }

    /// `Some(None)`: tuşu sil; `None`: vazgeç.
    fn end_bind(&mut self, key: Option<Option<Hotkey>>) {
        if !self.bind {
            return;
        }
        self.bind = false;
        if let Some(key) = key {
            self.settings.key = key;
            self.save();
        }
        self.register();
        self.redraw();
    }

    pub fn binding(&self) -> bool {
        self.bind
    }

    pub fn kill_focus(&mut self) {
        self.end_bind(None);
    }

    fn save(&self) {
        if let Err(e) = self.settings.save() {
            ortak::log!("clicker ayarları yazılamadı: {e}");
        }
    }

    fn set_mode(&mut self, m: Mode) {
        if self.settings.mode != m {
            self.end(true);
            self.settings.mode = m;
            self.save();
            self.redraw();
        }
    }

    fn set_button(&mut self, b: Button) {
        if self.settings.button != b {
            self.settings.button = b;
            self.restart();
            self.save();
            self.redraw();
        }
    }

    fn set_cps(&mut self, cps: f32) {
        let cps = quant_cps(cps);
        if self.settings.cps != cps {
            self.settings.cps = cps;
            self.restart();
            self.redraw();
        }
    }

    fn set_limit(&mut self, n: u32) {
        let n = n.min(LIMIT_MAX as u32);
        if self.settings.limit != n {
            self.settings.limit = n;
            self.restart();
            self.redraw();
        }
    }

    // --- Yerleşim ---

    fn layout_all(&self) -> Layout {
        let (l, r) = (PAD, self.w - PAD);
        let mut y = HEADER + 16.0 - self.scroll;
        let mut titles = [0.0; 2];
        let mut panels = [Rect::new(0.0, 0.0, 0.0, 0.0); 2];
        let mut rows: [Vec<Rect>; 2] = Default::default();
        for (s, n) in [3usize, 3].into_iter().enumerate() {
            titles[s] = y;
            let top = y + 30.0;
            let row =
                |i: usize| Rect::new(l + 6.0, top + 6.0 + i as f32 * ROW, r - 6.0, top + 6.0 + (i + 1) as f32 * ROW);
            rows[s] = (0..n).map(row).collect();
            panels[s] = Rect::new(l, top, r, top + 12.0 + n as f32 * ROW);
            y = panels[s].b + 24.0;
        }
        Layout { titles, panels, rows, bottom: y + self.scroll }
    }

    fn max_scroll(&self) -> f32 {
        (self.layout_all().bottom - self.h).max(0.0)
    }

    fn chip_text(&self) -> String {
        match (self.bind, self.settings.key) {
            (true, _) => t!("press a key", "tuşa bas").into(),
            (false, Some(k)) => k.to_string(),
            (false, None) => t!("key", "tuş").into(),
        }
    }

    fn key_chip(&self, g: &Gfx, row: Rect) -> Rect {
        chip_rect(g, row.r - 12.0, row.cy(), &self.chip_text())
    }

    /// Sağa yaslı seçenekler, verilen sırayla.
    fn segs(g: &Gfx, row: Rect, labels: &[&str]) -> Vec<Rect> {
        let mut right = row.r - 12.0;
        let mut v: Vec<Rect> = labels
            .iter()
            .rev()
            .map(|l| {
                let w = g.measure(l, &g.f.button) + 26.0;
                let r = Rect::new(right - w, row.cy() - 14.0, right, row.cy() + 14.0);
                right -= w;
                r
            })
            .collect();
        v.reverse();
        v
    }

    fn mode_segs(&self, g: &Gfx, row: Rect) -> Vec<Rect> {
        Self::segs(g, row, &Mode::ALL.map(Mode::label))
    }

    fn button_segs(&self, g: &Gfx, row: Rect) -> Vec<Rect> {
        Self::segs(g, row, &Button::ALL.map(Button::label))
    }

    /// Kaydırıcının izi (a..b) ve tutma alanı.
    fn track(row: Rect) -> (f32, f32, Rect) {
        let (a, b) = (row.l + 150.0, row.r - 100.0);
        (a, b, Rect::new(a - 10.0, row.cy() - 14.0, b + 10.0, row.cy() + 14.0))
    }

    fn stop_rect(&self, g: &Gfx) -> Rect {
        button_rect_right(g, self.head_r, HEAD_CY - 17.0, t!("Stop", "Durdur"), true)
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return if self.running() && self.stop_rect(g).contains(x, y) { Hit::Stop } else { Hit::None };
        }
        let [trigger, clicking] = &self.layout_all().rows;
        if trigger[0].contains(x, y) {
            return Hit::Enabled;
        }
        if self.key_chip(g, trigger[1]).contains(x, y) {
            return Hit::Key;
        }
        if let Some(i) = self.mode_segs(g, trigger[2]).iter().position(|r| r.contains(x, y)) {
            return Hit::Mode(Mode::ALL[i]);
        }
        if let Some(i) = self.button_segs(g, clicking[0]).iter().position(|r| r.contains(x, y)) {
            return Hit::Button(Button::ALL[i]);
        }
        if Self::track(clicking[1]).2.contains(x, y) {
            return Hit::Speed;
        }
        if Self::track(clicking[2]).2.contains(x, y) {
            return Hit::Limit;
        }
        Hit::None
    }

    /// Sürüklenen kaydırıcıya imlecin x'ini uygular.
    fn drag_to(&mut self, x: f32) {
        let Some(which) = self.drag else { return };
        let [_, clicking] = &self.layout_all().rows;
        let row = if which == Hit::Speed { clicking[1] } else { clicking[2] };
        let (a, b, _) = Self::track(row);
        let v = ((x - a) / (b - a)).clamp(0.0, 1.0);
        match which {
            Hit::Speed => self.set_cps(v_to_cps(v)),
            Hit::Limit => self.set_limit(v_to_limit(v)),
            _ => {}
        }
    }

    // --- Çizim ---

    fn paint_segs(g: &Gfx, rects: &[Rect], labels: &[&str], selected: usize, hovered: Option<usize>) {
        let (first, last) = (rects[0], rects[rects.len() - 1]);
        g.fill(Rect::new(first.l - 3.0, first.t - 3.0, last.r + 3.0, last.b + 3.0), 8.0, pal().hover);
        for (i, (r, l)) in rects.iter().zip(labels).enumerate() {
            let on = i == selected;
            let hov = hovered == Some(i);
            if on {
                g.fill(*r, 6.0, pal().sel);
            } else if hov {
                g.fill(*r, 6.0, pal().sel.alpha(0.6));
            }
            g.text(l, &g.f.button, *r, if on || hov { pal().text } else { pal().muted });
        }
    }

    /// Satırın başlığı ve altındaki açıklaması; `right`: kontrolün sol kenarı.
    fn row_text(g: &Gfx, r: Rect, title: &str, sub: &str, right: f32) {
        let x = r.l + 14.0;
        if sub.is_empty() {
            g.text(title, &g.f.strong, Rect::new(x, r.t, right - 10.0, r.b), pal().text);
        } else {
            g.text(title, &g.f.strong, Rect::new(x, r.t + 10.0, right - 10.0, r.cy()), pal().text);
            g.text(sub, &g.f.small, Rect::new(x, r.cy() - 2.0, right - 10.0, r.b - 8.0), pal().muted);
        }
    }

    fn paint_slider(&self, g: &Gfx, row: Rect, v: f32, label: &str, hit: Hit) {
        let (a, b, _) = Self::track(row);
        let active = self.drag == Some(hit) || (self.drag.is_none() && self.hover == hit);
        slider(g, a, b, row.cy(), v, accent(), active);
        g.text(label, &g.f.small_right, Rect::new(b + 12.0, row.t, row.r - 12.0, row.b), pal().muted);
    }

    pub fn paint(&self, g: &Gfx) {
        let w = self.w;
        let key = self.key_name();
        let n = self.run.as_ref().map_or(0, Run::count);
        let (dot, head, color) = match () {
            _ if !self.settings.enabled => (
                pal().faint,
                t!("Off · the key does nothing until you turn it on", "Kapalı · açana kadar tuş bir şey yapmaz").to_string(),
                pal().muted,
            ),
            _ if self.conflict() => (
                pal().red,
                t!(format!("{key} is taken by another app: pick another key"), format!("{key} başka bir uygulamada: başka tuş seç")),
                pal().muted,
            ),
            _ if self.settings.key.is_none() => {
                (pal().faint, t!("Pick a key to start", "Başlamak için bir tuş seç").to_string(), pal().muted)
            }
            _ if self.running() && self.settings.limit > 0 => (
                pal().green,
                t!(format!("Clicking · {n} / {}", self.settings.limit), format!("Tıklıyor · {n} / {}", self.settings.limit)),
                pal().text,
            ),
            _ if self.running() => (pal().green, t!(format!("Clicking · {n}"), format!("Tıklıyor · {n}")), pal().text),
            _ if self.settings.mode == Mode::Hold => {
                (pal().faint, t!(format!("Hold {key} to click"), format!("Tıklamak için {key} tuşunu basılı tut")), pal().muted)
            }
            _ => (
                pal().faint,
                t!(format!("Press {key} to start, again to stop"), format!("{key} başlatır, yine {key} durdurur")),
                pal().muted,
            ),
        };
        let mut right = self.head_r - 8.0;
        if self.running() {
            let r = self.stop_rect(g);
            button(g, r, t!("Stop", "Durdur"), Some(ICON_STOP), Some(accent()), self.hover == Hit::Stop);
            right = r.l - 12.0;
        }
        status(g, self.head_x + 16.0, HEAD_CY, dot, &head, color, right);

        let l = self.layout_all();
        g.clip(Rect::new(0.0, HEADER, w, self.h), || {
            let titles = [t!("Trigger", "Tetik"), t!("Clicking", "Tıklama")];
            for (s, title) in titles.iter().enumerate() {
                let t = l.titles[s];
                g.text(title, &g.f.strong, Rect::new(PAD, t, w - PAD, t + 24.0), pal().text);
                g.fill(l.panels[s], 12.0, pal().panel);
                g.stroke(l.panels[s], 12.0, pal().line, 1.0);
                for r in l.rows[s].iter().skip(1) {
                    g.fill(Rect::new(r.l + 8.0, r.t, r.r - 8.0, r.t + 1.0), 0.0, pal().hover);
                }
            }
            let [trigger, clicking] = &l.rows;

            // Tetik: açık, tuş, mod.
            let r = trigger[0];
            Self::row_text(
                g,
                r,
                t!("On", "Açık"),
                t!("Off, the trigger key is released; your settings stay", "Kapalıyken tetik tuşu bırakılır; ayarların durur"),
                toggle_rect(r.r - 12.0, r.cy()).l,
            );
            toggle(g, toggle_rect(r.r - 12.0, r.cy()), self.settings.enabled, accent(), true);

            let r = trigger[1];
            let chip_r = self.key_chip(g, r);
            Self::row_text(
                g,
                r,
                t!("Key", "Tuş"),
                t!("Any key, with Ctrl / Alt / Shift if you like", "Herhangi bir tuş, istersen Ctrl / Alt / Shift ile"),
                chip_r.l,
            );
            let (c, b) = match () {
                _ if self.bind => (accent(), accent()),
                _ if self.conflict() => (pal().red, pal().line),
                _ if self.settings.key.is_none() => (pal().faint, pal().line),
                _ if self.hover == Hit::Key => (pal().text, pal().faint),
                _ => (pal().muted, pal().line),
            };
            chip(g, chip_r, &self.chip_text(), c, b);

            let r = trigger[2];
            let segs = self.mode_segs(g, r);
            let sub = match self.settings.mode {
                Mode::Hold => t!("Clicks while the key is held down", "Tuş basılı olduğu sürece tıklar"),
                Mode::Toggle => t!("Press once to start, again to stop", "Bir basış başlatır, bir basış durdurur"),
            };
            Self::row_text(g, r, t!("Mode", "Mod"), sub, segs[0].l);
            let sel = Mode::ALL.iter().position(|&m| m == self.settings.mode).unwrap_or(0);
            let hov = Mode::ALL.iter().position(|&m| self.hover == Hit::Mode(m));
            Self::paint_segs(g, &segs, &Mode::ALL.map(Mode::label), sel, hov);

            // Tıklama: düğme, hız, sayı.
            let r = clicking[0];
            let segs = self.button_segs(g, r);
            Self::row_text(g, r, t!("Button", "Düğme"), "", segs[0].l);
            let sel = Button::ALL.iter().position(|&b| b == self.settings.button).unwrap_or(0);
            let hov = Button::ALL.iter().position(|&b| self.hover == Hit::Button(b));
            Self::paint_segs(g, &segs, &Button::ALL.map(Button::label), sel, hov);

            let r = clicking[1];
            Self::row_text(g, r, t!("Speed", "Hız"), "", Self::track(r).0);
            self.paint_slider(g, r, cps_to_v(self.settings.cps), &fmt_cps(self.settings.cps), Hit::Speed);

            let r = clicking[2];
            Self::row_text(g, r, t!("Stop after", "Şu kadar sonra dur"), "", Self::track(r).0);
            self.paint_slider(g, r, limit_to_v(self.settings.limit), &fmt_limit(self.settings.limit), Hit::Limit);
        });
    }
}

impl ToolPage for Clicker {
    fn message(&mut self, _g: &mut Gfx, msg: u32, wp: WPARAM, _lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_TIMER if wp.0 == TIMER_HOLD || wp.0 == TIMER_UI => self.timer(wp.0),
            WM_HOTKEY if wp.0 as i32 == HK_TRIGGER => self.hotkey(),
            WM_DONE => self.end(true),
            _ => return None,
        }
        Some(LRESULT(0))
    }

    fn binding(&self) -> bool {
        Clicker::binding(self)
    }

    fn kill_focus(&mut self) {
        Clicker::kill_focus(self)
    }

    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn paint(&self, g: &Gfx) {
        Clicker::paint(self, g)
    }

    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        if self.drag.is_some() {
            self.drag_to(x);
            return;
        }
        let hit = self.hit(g, x, y);
        if hit != self.hover {
            self.hover = hit;
            self.redraw();
        }
    }

    fn mouse_leave(&mut self) {
        if self.hover != Hit::None {
            self.hover = Hit::None;
            self.redraw();
        }
    }

    fn mouse_down(&mut self, g: &Gfx, x: f32, y: f32) {
        self.pressed = self.hit(g, x, y);
        if matches!(self.pressed, Hit::Speed | Hit::Limit) {
            self.end_bind(None);
            self.drag = Some(self.pressed);
            self.drag_to(x);
        }
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        if self.drag.take().is_some() {
            self.save();
            self.pressed = Hit::None;
            self.hover = self.hit(g, x, y);
            self.redraw();
            return;
        }
        let hit = self.hit(g, x, y);
        if hit != std::mem::replace(&mut self.pressed, Hit::None) {
            return;
        }
        self.end_bind(None);
        match hit {
            Hit::Enabled => self.set_enabled(!self.settings.enabled),
            Hit::Key => self.start_bind(),
            Hit::Mode(m) => self.set_mode(m),
            Hit::Button(b) => self.set_button(b),
            Hit::Stop => self.end(true),
            Hit::Speed | Hit::Limit | Hit::None => {}
        }
    }

    fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        match self.hit(g, x, y) {
            Hit::Speed => {
                let c = self.settings.cps;
                self.set_cps(c + delta.signum() * cps_step(if delta > 0.0 { c } else { c - 0.01 }));
                self.save();
            }
            Hit::Limit => {
                let n = self.settings.limit;
                let step = limit_step(if delta > 0.0 { n } else { n.saturating_sub(1) });
                let next = if delta > 0.0 { n.saturating_add(step) } else { n.saturating_sub(step) };
                self.set_limit(next);
                self.save();
            }
            _ => {
                self.scroll = (self.scroll - delta * 60.0).clamp(0.0, self.max_scroll());
                self.hover = self.hit(g, x, y);
                self.redraw();
            }
        }
    }

    /// Tetik atanıyorsa tuşu yakalar: Esc vazgeçer, Backspace / Delete siler.
    fn key(&mut self, vk: u16) -> bool {
        if !self.bind {
            return false;
        }
        if !hotkey::is_modifier(vk) {
            let key = match VIRTUAL_KEY(vk) {
                VK_ESCAPE => None,
                VK_BACK | VK_DELETE => Some(None),
                _ => Some(Some(Hotkey::pressed(vk))),
            };
            self.end_bind(key);
        }
        true
    }

    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        self.hit(g, x, y) != Hit::None
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        unsafe {
            if visible && self.running() {
                SetTimer(Some(self.hwnd), TIMER_UI, UI_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_UI);
            }
        }
        if !visible {
            self.hover = Hit::None;
            self.drag = None;
            self.end_bind(None);
        }
    }
}

impl Drop for Clicker {
    fn drop(&mut self) {
        self.run = None;
        self.unregister();
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HOLD);
            let _ = KillTimer(Some(self.hwnd), TIMER_UI);
        }
    }
}
