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

use ortak::form::{Ctl, Form, Hit, Item, Row};
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
const ICON_MOUSE: &str = "\u{E962}";
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

/// Sayfa kitindeki satırların kimlikleri.
const R_ON: u32 = 0;
const R_KEY: u32 = 1;
const R_MODE: u32 = 2;
const R_BUTTON: u32 = 3;
const R_SPEED: u32 = 4;
const R_LIMIT: u32 = 5;
/// Tıklarken Tetik'in başındaki Durdur satırı.
const R_STOP: u32 = 6;

pub struct Clicker {
    hwnd: HWND,
    settings: Settings,
    run: Option<Run>,
    /// Tetik kayıtlı; tuş varken değilse başka bir uygulama tutuyor.
    registered: bool,
    /// Tetik tuşu atanıyor.
    bind: bool,
    visible: bool,
    /// Sürüklenen kaydırıcının satırı.
    drag: Option<u32>,
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

    fn chip_text(&self) -> String {
        match (self.bind, self.settings.key) {
            (true, _) => t!("press a key", "tuşa bas").into(),
            (false, Some(k)) => k.to_string(),
            (false, None) => t!("key", "tuş").into(),
        }
    }

    /// Tıklarken durum: kaç tıklama (sınır varsa ondan kaçı).
    fn run_text(&self) -> String {
        let n = self.run.as_ref().map_or(0, Run::count);
        match self.settings.limit {
            0 => t!(format!("Clicking · {n}"), format!("Tıklıyor · {n}")),
            l => t!(format!("Clicking · {n} / {l}"), format!("Tıklıyor · {n} / {l}")),
        }
    }

    /// Sayfanın içeriği: Tetik (tıklarken başında Durdur) ve Tıklama bölümleri.
    fn items(&self) -> Vec<Item> {
        let s = &self.settings;
        let key_sub = if self.conflict() {
            t!("Taken by another app: pick another key", "Başka bir uygulamada: başka tuş seç")
        } else {
            t!("Any key, with Ctrl / Alt / Shift if you like", "Herhangi bir tuş, istersen Ctrl / Alt / Shift ile")
        };
        let mode_sub = match s.mode {
            Mode::Hold => t!("Clicks while the key is held down", "Tuş basılı olduğu sürece tıklar"),
            Mode::Toggle => t!("Press once to start, again to stop", "Bir basış başlatır, bir basış durdurur"),
        };
        let seg = |labels: &[&str], sel: Option<usize>| Ctl::Seg(labels.iter().map(|l| l.to_string()).collect(), sel.unwrap_or(0));
        let mut items = vec![Item::Section(t!("Trigger", "Tetik").into())];
        if self.running() {
            items.push(Item::Row(Row::new(R_STOP, self.run_text(), "", Ctl::Primary(t!("Stop", "Durdur").into()))));
        }
        items.extend([
            Item::Row(Row::new(
                R_ON,
                t!("On", "Açık"),
                t!("Off, the trigger key is released; your settings stay", "Kapalıyken tetik tuşu bırakılır; ayarların durur"),
                Ctl::Toggle(s.enabled),
            )),
            Item::Row(Row::new(R_KEY, t!("Key", "Tuş"), key_sub, Ctl::Chip(self.chip_text(), self.bind))),
            Item::Row(Row::new(
                R_MODE,
                t!("Mode", "Mod"),
                mode_sub,
                seg(&Mode::ALL.map(Mode::label), Mode::ALL.iter().position(|&m| m == s.mode)),
            )),
            Item::Section(t!("Clicking", "Tıklama").into()),
            Item::Row(Row::new(
                R_BUTTON,
                t!("Button", "Düğme"),
                "",
                seg(&Button::ALL.map(Button::label), Button::ALL.iter().position(|&b| b == s.button)),
            )),
            Item::Row(Row::new(R_SPEED, t!("Speed", "Hız"), "", Ctl::Slider(cps_to_v(s.cps), fmt_cps(s.cps)))),
            Item::Row(Row::new(
                R_LIMIT,
                t!("Stop after", "Şu kadar sonra dur"),
                "",
                Ctl::Slider(limit_to_v(s.limit), fmt_limit(s.limit)),
            )),
        ]);
        items
    }

    fn form(&self) -> Form {
        Form::new(self.items(), PAD, self.w - PAD, HEADER + 16.0 - self.scroll)
    }

    fn max_scroll(&self) -> f32 {
        (self.form().bottom() + self.scroll - self.h).max(0.0)
    }

    /// Başlıkta düğme yok; altı sayfa kitinin.
    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return Hit::None;
        }
        self.form().hit(g, x, y)
    }

    /// Sürüklenen kaydırıcıya imlecin x'ini uygular.
    fn drag_to(&mut self, x: f32) {
        let Some(id) = self.drag else { return };
        let Some(v) = self.form().slider_value(id, x) else { return };
        self.slide(id, v);
    }

    fn slide(&mut self, id: u32, v: f32) {
        match id {
            R_SPEED => self.set_cps(v_to_cps(v)),
            R_LIMIT => self.set_limit(v_to_limit(v)),
            _ => {}
        }
    }

    /// Tıklanan (fare bırakılınca).
    fn act(&mut self, hit: Hit) {
        match hit {
            Hit::Toggle(R_ON) | Hit::Row(R_ON) => self.set_enabled(!self.settings.enabled),
            Hit::Chip(R_KEY) => self.start_bind(),
            Hit::Seg(R_MODE, k) => self.set_mode(Mode::ALL[k]),
            Hit::Seg(R_BUTTON, k) => self.set_button(Button::ALL[k]),
            Hit::Button(R_STOP) => self.end(true),
            _ => {}
        }
    }

    // --- Çizim ---

    pub fn paint(&self, g: &Gfx) {
        let key = self.key_name();
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
            _ if self.running() => (pal().green, self.run_text(), pal().text),
            _ if self.settings.mode == Mode::Hold => {
                (pal().faint, t!(format!("Hold {key} to click"), format!("Tıklamak için {key} tuşunu basılı tut")), pal().muted)
            }
            _ => (
                pal().faint,
                t!(format!("Press {key} to start, again to stop"), format!("{key} başlatır, yine {key} durdurur")),
                pal().muted,
            ),
        };
        status(g, self.head_x + 16.0, HEAD_CY, dot, &head, color, self.head_r - 8.0);

        let form = self.form();
        g.clip(Rect::new(0.0, HEADER, self.w, self.h), || form.paint(g, self.hover, self.drag));
    }
}

impl ToolPage for Clicker {
    fn summary(&self) -> Option<(String, String)> {
        let s = &self.settings;
        if !s.enabled {
            return Some((t!("Off", "Kapalı").into(), t!("The key does nothing", "Tuş bir şey yapmaz").into()));
        }
        let key = if s.key.is_some() { self.key_name() } else { t!("No key", "Tuş yok").into() };
        Some((key, format!("{} · {}", s.mode.label(), fmt_cps(s.cps))))
    }

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
        if let Hit::Slider(id, v) = self.pressed {
            self.end_bind(None);
            self.drag = Some(id);
            self.slide(id, v);
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
        if !hit.same(std::mem::replace(&mut self.pressed, Hit::None)) {
            return;
        }
        self.end_bind(None);
        self.act(hit);
    }

    fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        match self.hit(g, x, y) {
            Hit::Slider(R_SPEED, _) => {
                let c = self.settings.cps;
                self.set_cps(c + delta.signum() * cps_step(if delta > 0.0 { c } else { c - 0.01 }));
                self.save();
            }
            Hit::Slider(R_LIMIT, _) => {
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
