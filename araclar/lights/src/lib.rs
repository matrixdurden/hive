//! lights: Bluetooth LED ışıklar. Renk, parlaklık, ışığın kendi efektleri ve müzikle yanma.
//! Desteklenen ürünler `surucu/` altında (şimdilik STARLIGHT); sayfa ve müzik modeli bilmez,
//! `surucu::Cmd` gönderir.
//!
//! Üç sekme: Işık (cihaz, aç/kapa, parlaklık, renk), Efektler (arama, hız, yön, liste), Müzik
//! (vuruşta renk değiştir ya da parla; canlı ölçer). Bluetooth işi `link`'in iş parçacığında,
//! ses yakalama `music`'inkinde; ikisi de arayüzü bekletmez.

#[macro_use]
extern crate ortak;

mod link;
mod music;
pub mod surucu;
mod tool;
pub use tool::TOOL;

use std::path::PathBuf;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use ortak::form::{self, Ctl, Form, Item, Row, TABS_H};
use ortak::gfx::{Color, Gfx, Rect};
use ortak::ui::*;
use ortak::util;

use link::{Link, State, WM_LINK};
use music::{Music, React, Source};
use surucu::{Cmd, Model};

/// Müzik sekmesindeki ölçer tazelenir.
pub const TIMER_METER: usize = 901;
const METER_MS: u32 = 33;

/// Hazır renkler: (renk, İngilizce, Türkçe).
const PRESETS: [(u32, &str, &str); 10] = [
    (0xFF2020, "Red", "Kırmızı"),
    (0xFF7A00, "Orange", "Turuncu"),
    (0xFFD000, "Yellow", "Sarı"),
    (0x20E040, "Green", "Yeşil"),
    (0x00D8E0, "Cyan", "Turkuaz"),
    (0x1060FF, "Blue", "Mavi"),
    (0x8A2BFF, "Purple", "Mor"),
    (0xFF1A8C, "Pink", "Pembe"),
    (0xFFB070, "Warm white", "Sıcak beyaz"),
    (0xFFFFFF, "White", "Beyaz"),
];

pub fn dir() -> PathBuf {
    util::data_dir().join("lights")
}

fn ini() -> PathBuf {
    dir().join("ayarlar.ini")
}

pub fn installed() -> bool {
    dir().is_dir()
}

#[derive(Clone)]
struct Settings {
    /// Son bağlanılan ışığın Bluetooth adresi (0: yok) ve modeli.
    address: u64,
    name: String,
    model: String,
    /// winloom açılınca son ışığa bağlan.
    auto: bool,
    power: bool,
    color: [u8; 3],
    /// 0..1
    brightness: f32,
    /// Çalan efekt; 0: düz renk.
    effect: u16,
    speed: f32,
    reverse: bool,
    music: bool,
    react: React,
    source: Source,
    sensitivity: f32,
}

impl Settings {
    fn load() -> Self {
        let mut s = Settings {
            address: 0,
            name: String::new(),
            model: String::new(),
            auto: true,
            power: true,
            color: [255, 255, 255],
            brightness: 1.0,
            effect: 0,
            speed: 0.5,
            reverse: false,
            music: false,
            react: React::Colors,
            source: Source::Bass,
            sensitivity: 1.5,
        };
        let text = std::fs::read_to_string(ini()).unwrap_or_default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "adres" => s.address = u64::from_str_radix(v, 16).unwrap_or(0),
                "ad" => s.name = v.to_string(),
                "model" => s.model = v.to_string(),
                "oto" => s.auto = v != "0",
                "acik" => s.power = v != "0",
                "renk" => s.color = u32::from_str_radix(v, 16).map_or(s.color, rgb),
                "parlaklik" => s.brightness = v.parse().map_or(s.brightness, |b: f32| b.clamp(0.0, 1.0)),
                "efekt" => s.effect = v.parse().unwrap_or(0),
                "hiz" => s.speed = v.parse().map_or(s.speed, |b: f32| b.clamp(0.0, 1.0)),
                "ters" => s.reverse = v == "1",
                "muzik" => s.music = v == "1",
                "tepki" => s.react = if v == "flas" { React::Flash } else { React::Colors },
                "kaynak" => s.source = if v == "hepsi" { Source::All } else { Source::Bass },
                "hassasiyet" => s.sensitivity = v.parse().map_or(s.sensitivity, |b: f32| b.clamp(SENS_MIN, SENS_MAX)),
                _ => {}
            }
        }
        s
    }

    fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(dir())?;
        let [r, g, b] = self.color;
        let text = format!(
            "adres={:X}\nad={}\nmodel={}\noto={}\nacik={}\nrenk={r:02X}{g:02X}{b:02X}\nparlaklik={}\nefekt={}\nhiz={}\nters={}\nmuzik={}\ntepki={}\nkaynak={}\nhassasiyet={}\n",
            self.address,
            self.name,
            self.model,
            self.auto as u8,
            self.power as u8,
            self.brightness,
            self.effect,
            self.speed,
            self.reverse as u8,
            self.music as u8,
            if self.react == React::Flash { "flas" } else { "renk" },
            if self.source == Source::All { "hepsi" } else { "bas" },
            self.sensitivity,
        );
        std::fs::write(ini(), text)
    }

    fn music_config(&self) -> music::Config {
        music::Config { react: self.react, source: self.source, sensitivity: self.sensitivity, brightness: self.brightness.max(0.05) }
    }
}

fn rgb(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

fn hex(c: [u8; 3]) -> u32 {
    (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32
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

fn cli(args: &[String]) -> Option<i32> {
    match args.first()?.as_str() {
        "--lights-test" | "--isiklar-dene" => {
            ortak::tool::console(false);
            Some(link::cli_test())
        }
        _ => None,
    }
}

// --- Kaydırıcılar ---

/// Hassasiyet: eşik çarpanı. Kaydırıcının sağı daha hassas (küçük çarpan).
const SENS_MIN: f32 = 0.5;
const SENS_MAX: f32 = 3.0;

fn sens_to_v(s: f32) -> f32 {
    ((SENS_MAX - s) / (SENS_MAX - SENS_MIN)).clamp(0.0, 1.0)
}

fn v_to_sens(v: f32) -> f32 {
    ((SENS_MAX - v.clamp(0.0, 1.0) * (SENS_MAX - SENS_MIN)) * 10.0).round() / 10.0
}

fn pct(v: f32) -> String {
    format!("%{}", (v * 100.0).round() as u32)
}

// --- Sayfa ---

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Light,
    Effects,
    Music,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Light, Tab::Effects, Tab::Music];
}

fn tab_labels() -> [&'static str; 3] {
    [t!("Light", "Işık"), t!("Effects", "Efektler"), t!("Music", "Müzik")]
}

/// Satır ve alan kimlikleri.
const R_DEVICE: u32 = 1;
const R_AUTO: u32 = 2;
const R_POWER: u32 = 3;
const R_BRIGHT: u32 = 4;
const C_COLORS: u32 = 5;
const C_SEARCH: u32 = 6;
const R_SPEED: u32 = 7;
const R_DIR: u32 = 8;
const R_MUSIC: u32 = 9;
const R_REACT: u32 = 10;
const R_SOURCE: u32 = 11;
const R_SENS: u32 = 12;
const C_METER: u32 = 13;
/// Efekt satırları: bu + efektin kimliği.
const R_EFFECT: u32 = 1000;

/// Renk alanı: hazır renkler ve altında ton şeridi.
const COLORS_H: f32 = 112.0;
const SWATCH: f32 = 30.0;
const SEARCH_H: f32 = 36.0;
const METER_H: f32 = 108.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Tab(Tab),
    Form(form::Hit),
    Swatch(usize),
    /// Ton şeridi: 0..1.
    Hue(f32),
    Search,
}

impl Hit {
    fn form(self) -> form::Hit {
        if let Hit::Form(h) = self { h } else { form::Hit::None }
    }

    fn same(self, other: Hit) -> bool {
        match (self, other) {
            (Hit::Form(a), Hit::Form(b)) => a.same(b),
            (Hit::Hue(_), Hit::Hue(_)) => true,
            (a, b) => a == b,
        }
    }
}

pub struct Lights {
    hwnd: HWND,
    settings: Settings,
    link: Link,
    music: Option<Music>,
    tab: Tab,
    search: TextField,
    scroll: [f32; 3],
    hover: Hit,
    pressed: Hit,
    /// Sürüklenen kaydırıcının satırı (ton şeridi: `C_COLORS`).
    drag: Option<u32>,
    visible: bool,
    w: f32,
    h: f32,
}

impl Lights {
    pub fn start(hwnd: HWND) -> Self {
        let settings = Settings::load();
        let link = Link::start(hwnd);
        let mut t = Self {
            hwnd,
            settings,
            link,
            music: None,
            tab: Tab::Light,
            search: TextField::new(40),
            scroll: [0.0; 3],
            hover: Hit::None,
            pressed: Hit::None,
            drag: None,
            visible: false,
            w: 0.0,
            h: 0.0,
        };
        if t.settings.auto && t.settings.address != 0 {
            t.connect();
        }
        if t.settings.music {
            t.music = Some(Music::start(t.link.tx(), t.settings.music_config()));
        }
        t
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn save(&self) {
        if let Err(e) = self.settings.save() {
            ortak::log!("lights ayarları yazılamadı: {e}");
        }
    }

    /// Bağlı ya da son bağlanılan ışığın modeli; hiç yoksa ilk desteklenen.
    fn model(&self) -> &'static Model {
        self.link.status().model.or_else(|| surucu::by_id(&self.settings.model)).unwrap_or(surucu::MODELS[0])
    }

    fn connect(&self) {
        self.link.connect((self.settings.address != 0).then_some(self.settings.address));
    }

    /// Bağlantı durumu değişti: yeni ışığa bağlandıysa hatırlanır.
    fn on_link(&mut self) {
        let s = self.link.status();
        if s.state == State::Connected && (s.address != self.settings.address || s.name != self.settings.name) {
            self.settings.address = s.address;
            self.settings.name = s.name.clone();
            self.settings.model = s.model.map(|m| m.id.to_string()).unwrap_or_default();
            self.save();
        }
        self.redraw();
    }

    // --- Işığa gidenler ---

    fn set_power(&mut self, on: bool) {
        self.settings.power = on;
        self.link.send(Cmd::Power(on));
        self.save();
        self.redraw();
    }

    fn set_color(&mut self, c: [u8; 3]) {
        self.settings.color = c;
        self.settings.effect = 0;
        if !self.settings.power {
            self.settings.power = true;
            self.link.send(Cmd::Power(true));
        }
        self.link.send(Cmd::Color(c));
        self.redraw();
    }

    fn set_brightness(&mut self, v: f32) {
        let v = (v * 100.0).round() / 100.0;
        if self.settings.brightness != v {
            self.settings.brightness = v;
            self.link.send(Cmd::Brightness(v.max(0.01)));
            self.update_music();
            self.redraw();
        }
    }

    fn set_effect(&mut self, id: u16) {
        self.settings.effect = id;
        if !self.settings.power {
            self.settings.power = true;
            self.link.send(Cmd::Power(true));
        }
        self.link.send(Cmd::Effect(id));
        self.save();
        self.redraw();
    }

    fn set_speed(&mut self, v: f32) {
        let v = (v * 100.0).round() / 100.0;
        if self.settings.speed != v {
            self.settings.speed = v;
            self.link.send(Cmd::Speed(v));
            self.redraw();
        }
    }

    fn set_reverse(&mut self, r: bool) {
        if self.settings.reverse != r {
            self.settings.reverse = r;
            self.link.send(Cmd::Reverse(r));
            self.save();
            self.redraw();
        }
    }

    // --- Müzik ---

    fn set_music(&mut self, on: bool) {
        if self.settings.music == on {
            return;
        }
        self.settings.music = on;
        if on {
            self.music = Some(Music::start(self.link.tx(), self.settings.music_config()));
        } else {
            self.music = None;
            // Müzikten önceki hali geri gelir.
            self.link.send(Cmd::Brightness(self.settings.brightness.max(0.01)));
            if self.settings.effect > 0 {
                self.link.send(Cmd::Effect(self.settings.effect));
            } else {
                self.link.send(Cmd::Color(self.settings.color));
            }
        }
        self.save();
        self.update_timer();
        self.redraw();
    }

    fn update_music(&self) {
        if let Some(m) = &self.music {
            m.set(self.settings.music_config());
        }
    }

    fn update_timer(&self) {
        unsafe {
            if self.visible && self.tab == Tab::Music && self.music.is_some() {
                SetTimer(Some(self.hwnd), TIMER_METER, METER_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_METER);
            }
        }
    }

    fn select(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.search.blur();
            self.update_timer();
            self.redraw();
        }
    }

    // --- Yerleşim ---

    fn device_row(&self) -> Row {
        let s = self.link.status();
        let known = if self.settings.name.is_empty() { self.model().name.to_string() } else { self.settings.name.clone() };
        match s.state {
            State::Off => {
                let sub = s.error.clone().unwrap_or_else(|| {
                    t!("Turn the light on and close its phone app, then connect", "Işığı aç, telefon uygulamasını kapat, sonra bağlan").into()
                });
                let title = if self.settings.address != 0 {
                    t!(format!("{known} · not connected"), format!("{known} · bağlı değil"))
                } else {
                    t!("Not connected", "Bağlı değil").into()
                };
                Row::new(R_DEVICE, title, sub, Ctl::Primary(t!("Connect", "Bağlan").into()))
            }
            State::Searching => Row::new(
                R_DEVICE,
                t!("Searching…", "Aranıyor…"),
                t!("Looking for lights nearby (up to 10 s)", "Yakındaki ışıklara bakılıyor (en fazla 10 sn)"),
                Ctl::None,
            ),
            State::Connecting => {
                let name = if s.name.is_empty() { known } else { s.name.clone() };
                Row::new(R_DEVICE, t!(format!("Connecting · {name}"), format!("Bağlanıyor · {name}")), "", Ctl::None)
            }
            State::Connected => {
                let sub = match (&s.error, s.reachable) {
                    (_, false) => t!("Out of reach · Windows keeps trying", "Ulaşılamıyor · Windows yeniden deniyor").to_string(),
                    (Some(e), _) => e.clone(),
                    (None, true) => t!("Connected", "Bağlı").into(),
                };
                Row::new(R_DEVICE, s.name.clone(), sub, Ctl::Button(t!("Disconnect", "Bağlantıyı kes").into()))
            }
        }
    }

    fn light_items(&self) -> Vec<Item> {
        let s = &self.settings;
        let off = !self.link.connected();
        vec![
            Item::Section(t!("Device", "Cihaz").into()),
            Item::Row(self.device_row()),
            Item::Row(Row::new(
                R_AUTO,
                t!("Connect on start", "Açılışta bağlan"),
                t!("Connects to the last light when winloom starts", "winloom açılınca son ışığa kendiliğinden bağlanır"),
                Ctl::Toggle(s.auto),
            )),
            Item::Section(t!("Light", "Işık").into()),
            Item::Row(Row::new(R_POWER, t!("On", "Açık"), "", Ctl::Toggle(s.power)).dim(off)),
            Item::Row(Row::new(R_BRIGHT, t!("Brightness", "Parlaklık"), "", Ctl::Slider(s.brightness, pct(s.brightness))).dim(off)),
            Item::Section(t!("Color", "Renk").into()),
            Item::Custom(C_COLORS, COLORS_H),
        ]
    }

    /// Aramaya uyan efektler.
    fn shown(&self) -> Vec<&'static surucu::Effect> {
        let q = self.search.text.trim().to_lowercase();
        self.model()
            .effects
            .iter()
            .filter(|e| q.is_empty() || surucu::effect_name(e).to_lowercase().contains(&q) || e.name.to_lowercase().contains(&q) || e.id.to_string() == q)
            .collect()
    }

    fn effect_items(&self) -> Vec<Item> {
        let s = &self.settings;
        let off = !self.link.connected();
        let mut v = vec![Item::Custom(C_SEARCH, SEARCH_H), Item::Section(String::new())];
        v.push(Item::Row(
            Row::new(R_SPEED, t!("Speed", "Hız"), "", Ctl::Slider(s.speed, pct(s.speed))).dim(off),
        ));
        if self.model().reverse {
            v.push(Item::Row(
                Row::new(
                    R_DIR,
                    t!("Direction", "Yön"),
                    "",
                    Ctl::Seg(vec![t!("Forward", "İleri").into(), t!("Reverse", "Geri").into()], s.reverse as usize),
                )
                .dim(off),
            ));
        }
        let shown = self.shown();
        v.push(Item::Section(t!(format!("Effects · {}", shown.len()), format!("Efektler · {}", shown.len()))));
        if shown.is_empty() {
            v.push(Item::Note(t!("No effect matches", "Uyan efekt yok").into(), false));
        }
        for e in shown {
            let on = s.effect == e.id && !s.music;
            v.push(Item::Row(Row::new(R_EFFECT + e.id as u32, format!("{:03}  {}", e.id, surucu::effect_name(e)), "", Ctl::Check(on)).clickable()));
        }
        v
    }

    fn music_items(&self) -> Vec<Item> {
        let s = &self.settings;
        let react_sub = match s.react {
            React::Colors => t!("Every beat brings the next color", "Her vuruşta sıradaki renk"),
            React::Flash => t!("Waits dim, flashes on every beat", "Loş bekler, her vuruşta parlar"),
        };
        let source_sub = match s.source {
            Source::Bass => t!("Only the kick drum", "Yalnızca bas davul"),
            Source::All => t!("Kick, snare or hi-hat", "Bas, trampet ya da zil"),
        };
        let mut v = vec![
            Item::Section(t!("Music", "Müzik").into()),
            Item::Row(Row::new(
                R_MUSIC,
                t!("Move with the music", "Müzikle yansın"),
                t!("Listens to what your computer plays, no microphone", "Bilgisayarda çalanı dinler, mikrofon gerekmez"),
                Ctl::Toggle(s.music),
            )),
            Item::Row(Row::new(
                R_REACT,
                t!("On a beat", "Vuruşta"),
                react_sub,
                Ctl::Seg(vec![t!("Color", "Renk").into(), t!("Flash", "Flaş").into()], (s.react == React::Flash) as usize),
            )),
            Item::Row(Row::new(
                R_SOURCE,
                t!("Beats", "Vuruşlar"),
                source_sub,
                Ctl::Seg(vec![t!("Bass", "Bas").into(), t!("All", "Hepsi").into()], (s.source == Source::All) as usize),
            )),
            Item::Row(Row::new(
                R_SENS,
                t!("Sensitivity", "Hassasiyet"),
                t!("Higher catches quieter beats", "Arttıkça daha hafif vuruşlar da yakalanır"),
                Ctl::Slider(sens_to_v(s.sensitivity), pct(sens_to_v(s.sensitivity))),
            )),
        ];
        if s.music {
            if !self.link.connected() {
                v.push(Item::Note(t!("No light connected: the beats only show here", "Işık bağlı değil: vuruşlar yalnızca burada görünür").into(), false));
            }
            v.push(Item::Section(t!("Now", "Şu an").into()));
            v.push(Item::Custom(C_METER, METER_H));
        }
        v
    }

    fn content_top(&self) -> f32 {
        HEADER + TABS_H + 16.0 - self.scroll[self.tab as usize]
    }

    fn form(&self) -> Form {
        let items = match self.tab {
            Tab::Light => self.light_items(),
            Tab::Effects => self.effect_items(),
            Tab::Music => self.music_items(),
        };
        Form::new(items, PAD, self.w - PAD, self.content_top())
    }

    fn max_scroll(&self) -> f32 {
        (self.form().bottom() + self.scroll[self.tab as usize] - self.h).max(0.0)
    }

    /// Renk alanının parçaları: hazır renklerin yerleri ve ton şeridi.
    fn color_rects(r: Rect) -> (Vec<Rect>, Rect) {
        let inner = Rect::new(r.l + 16.0, r.t + 16.0, r.r - 16.0, r.b - 16.0);
        let gap = ((inner.w() - SWATCH * PRESETS.len() as f32) / (PRESETS.len() - 1) as f32).clamp(6.0, 18.0);
        let swatches = (0..PRESETS.len())
            .map(|i| {
                let x = inner.l + i as f32 * (SWATCH + gap);
                Rect::new(x, inner.t, x + SWATCH, inner.t + SWATCH)
            })
            .collect();
        let hue = Rect::new(inner.l, inner.b - 22.0, inner.r, inner.b);
        (swatches, hue)
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return Hit::None;
        }
        if y < HEADER + TABS_H {
            return form::tab_at(g, &tab_labels(), x, y).map_or(Hit::None, |k| Hit::Tab(Tab::ALL[k]));
        }
        let f = self.form();
        match f.hit(g, x, y) {
            form::Hit::Custom(C_COLORS) => {
                let Some(r) = f.custom(C_COLORS) else { return Hit::None };
                let (swatches, hue) = Self::color_rects(r);
                if let Some(i) = swatches.iter().position(|s| s.contains(x, y)) {
                    return Hit::Swatch(i);
                }
                if Rect::new(hue.l - 6.0, hue.t - 6.0, hue.r + 6.0, hue.b + 6.0).contains(x, y) {
                    return Hit::Hue(((x - hue.l) / hue.w()).clamp(0.0, 1.0));
                }
                Hit::None
            }
            form::Hit::Custom(C_SEARCH) => Hit::Search,
            form::Hit::Custom(_) => Hit::None,
            form::Hit::None => Hit::None,
            h => Hit::Form(h),
        }
    }

    fn drag_to(&mut self, x: f32) {
        let Some(id) = self.drag else { return };
        let f = self.form();
        if id == C_COLORS {
            if let Some(r) = f.custom(C_COLORS) {
                let (_, hue) = Self::color_rects(r);
                self.set_color(music::hsv(((x - hue.l) / hue.w()).clamp(0.0, 1.0) * 360.0, 1.0));
            }
            return;
        }
        if let Some(v) = f.slider_value(id, x) {
            self.slide(id, v);
        }
    }

    fn slide(&mut self, id: u32, v: f32) {
        match id {
            R_BRIGHT => self.set_brightness(v),
            R_SPEED => self.set_speed(v),
            R_SENS => {
                self.settings.sensitivity = v_to_sens(v);
                self.update_music();
                self.redraw();
            }
            _ => {}
        }
    }

    fn act(&mut self, hit: Hit) {
        use form::Hit as F;
        if hit != Hit::Search && self.search.focused {
            self.search.blur();
            self.redraw();
        }
        match hit {
            Hit::Tab(t) => self.select(t),
            Hit::Swatch(i) => {
                self.set_color(rgb(PRESETS[i].0));
                self.save();
            }
            Hit::Search => {
                self.search.focus(false);
                unsafe {
                    let _ = SetFocus(Some(self.hwnd));
                }
                self.redraw();
            }
            Hit::Form(F::Button(R_DEVICE)) => {
                if self.link.status().state == State::Connected {
                    self.link.disconnect();
                } else {
                    self.connect();
                }
            }
            Hit::Form(F::Toggle(R_AUTO) | F::Row(R_AUTO)) => {
                self.settings.auto = !self.settings.auto;
                self.save();
                self.redraw();
            }
            Hit::Form(F::Toggle(R_POWER) | F::Row(R_POWER)) => self.set_power(!self.settings.power),
            Hit::Form(F::Seg(R_DIR, k)) => self.set_reverse(k == 1),
            Hit::Form(F::Toggle(R_MUSIC) | F::Row(R_MUSIC)) => self.set_music(!self.settings.music),
            Hit::Form(F::Seg(R_REACT, k)) => {
                self.settings.react = if k == 1 { React::Flash } else { React::Colors };
                self.update_music();
                self.save();
                self.redraw();
            }
            Hit::Form(F::Seg(R_SOURCE, k)) => {
                self.settings.source = if k == 1 { Source::All } else { Source::Bass };
                self.update_music();
                self.save();
                self.redraw();
            }
            Hit::Form(F::Row(id)) if id > R_EFFECT => {
                if self.settings.music {
                    self.set_music(false);
                }
                self.set_effect((id - R_EFFECT) as u16);
            }
            _ => {}
        }
    }

    // --- Çizim ---

    pub fn paint(&self, g: &Gfx) {
        let f = self.form();
        g.clip(Rect::new(0.0, HEADER + TABS_H, self.w, self.h), || {
            f.paint(g, self.hover.form(), self.drag);
            if let Some(r) = f.custom(C_COLORS) {
                self.paint_colors(g, r);
            }
            if let Some(r) = f.custom(C_SEARCH) {
                self.search.paint(g, r, t!("Search effects (name or number)", "Efektlerde ara (ad ya da numara)"));
            }
            if let Some(r) = f.custom(C_METER) {
                self.paint_meter(g, r);
            }
        });
        let hover = Tab::ALL.iter().position(|&t| self.hover == Hit::Tab(t));
        form::paint_tabs(g, &tab_labels(), self.tab as usize, hover, self.w);
    }

    fn card(g: &Gfx, r: Rect) {
        g.fill(r, 8.0, pal().panel);
        g.stroke(r, 8.0, pal().line, 1.0);
    }

    /// Hazır renkler (seçili olanın çevresinde halka) ve ton şeridi (seçili tonda işaret).
    fn paint_colors(&self, g: &Gfx, r: Rect) {
        Self::card(g, r);
        let (swatches, hue) = Self::color_rects(r);
        let current = hex(self.settings.color);
        let plain = self.settings.effect == 0;
        for (i, s) in swatches.iter().enumerate() {
            let (cx, cy) = (s.l + SWATCH / 2.0, s.cy());
            let c = PRESETS[i].0;
            if plain && c == current {
                g.ring(cx, cy, SWATCH / 2.0 + 3.0, pal().text, 2.0);
            } else if self.hover == Hit::Swatch(i) {
                g.ring(cx, cy, SWATCH / 2.0 + 3.0, pal().faint, 1.5);
            }
            g.circle(cx, cy, SWATCH / 2.0 - 1.0, Color::rgb(c));
        }
        let stops: Vec<(f32, Color)> = (0..=6).map(|k| (k as f32 / 6.0, Color::rgb(hex(music::hsv(k as f32 * 60.0, 1.0))))).collect();
        g.linear(hue, hue.h() / 2.0, (hue.l, hue.cy()), (hue.r, hue.cy()), &stops);
        // Seçili renk tam doygun bir tonsa şeritte yeri işaretlenir.
        if plain && let Some(h) = pure_hue(self.settings.color) {
            let x = hue.l + h / 360.0 * hue.w();
            g.circle(x, hue.cy(), hue.h() / 2.0 + 2.0, pal().text);
            g.circle(x, hue.cy(), hue.h() / 2.0 - 2.0, Color::rgb(current));
        }
    }

    /// Canlı ölçer: solda vuruşta yanan nokta, ortada basın eşiğe göre seviyesi, sağda BPM.
    fn paint_meter(&self, g: &Gfx, r: Rect) {
        Self::card(g, r);
        let Some(m) = self.music.as_ref().map(Music::meter) else { return };
        let age = m.last_beat.map(|t| t.elapsed()).unwrap_or(Duration::from_secs(9));
        let glow = (1.0 - age.as_secs_f32() / 0.25).clamp(0.0, 1.0);
        let (cx, cy) = (r.l + 44.0, r.cy());
        g.circle(cx, cy, 22.0, pal().hover);
        if glow > 0.0 {
            let c = match self.settings.react {
                React::Colors => Color::rgb(0xFACC15),
                React::Flash => pal().text,
            };
            g.circle(cx, cy, 10.0 + 12.0 * glow, c.alpha(glow));
        }
        let (l, rr) = (r.l + 88.0, r.r - 120.0);
        let label = if m.sound { t!("Bass", "Bas") } else { t!("No sound", "Ses yok") };
        g.text(label, &g.f.small, Rect::new(l, r.t + 18.0, rr, r.t + 38.0), pal().muted);
        let bar = Rect::new(l, r.cy() - 4.0, rr, r.cy() + 6.0);
        g.fill(bar, 5.0, pal().hover);
        // Eşik çubuğun ortasında: üstüne çıkan her şey vuruş adayı.
        let v = (m.bass / 2.0).clamp(0.0, 1.0);
        if v > 0.0 {
            g.fill(Rect::new(bar.l, bar.t, bar.l + bar.w() * v, bar.b), 5.0, if m.bass >= 1.0 { pal().text } else { pal().faint });
        }
        let mid = bar.l + bar.w() / 2.0;
        g.fill(Rect::new(mid - 1.0, bar.t - 6.0, mid + 1.0, bar.b + 6.0), 0.0, pal().muted);
        g.text(t!("threshold", "eşik"), &g.f.small, Rect::new(mid - 30.0, bar.b + 8.0, mid + 60.0, bar.b + 28.0), pal().faint);
        let bpm = if m.bpm > 0.0 && m.sound { format!("{:.0}", m.bpm) } else { "–".into() };
        g.text(&bpm, &g.f.heading, Rect::new(r.r - 104.0, r.t + 22.0, r.r - 16.0, r.cy() + 8.0), pal().text);
        g.text("BPM", &g.f.small, Rect::new(r.r - 104.0, r.cy() + 8.0, r.r - 16.0, r.b - 18.0), pal().muted);
    }
}

/// Tam doygun ve tam parlak bir renkse tonu.
fn pure_hue(c: [u8; 3]) -> Option<f32> {
    let max = *c.iter().max()?;
    let min = *c.iter().min()?;
    if max != 255 || min != 0 {
        return None;
    }
    (0..360).map(|h| h as f32).find(|h| music::hsv(*h, 1.0).iter().zip(c).all(|(a, b)| a.abs_diff(b) <= 3))
}

impl ToolPage for Lights {
    fn summary(&self) -> Option<(String, String)> {
        let s = self.link.status();
        let top = match s.state {
            State::Connected => s.name.clone(),
            State::Searching | State::Connecting => t!("Connecting…", "Bağlanıyor…").into(),
            State::Off => t!("Not connected", "Bağlı değil").into(),
        };
        let st = &self.settings;
        let what = if st.music {
            t!("With the music", "Müzikle").to_string()
        } else if !st.power {
            t!("Off", "Kapalı").into()
        } else if st.effect > 0 {
            self.model().effects.iter().find(|e| e.id == st.effect).map(surucu::effect_name).unwrap_or_default()
        } else {
            PRESETS
                .iter()
                .find(|p| p.0 == hex(st.color))
                .map_or_else(|| format!("#{:06X}", hex(st.color)), |p| t!(p.1, p.2).to_string())
        };
        Some((top, format!("{what} · {}", pct(st.brightness))))
    }

    fn message(&mut self, _g: &mut Gfx, msg: u32, wp: WPARAM, _lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_LINK => self.on_link(),
            WM_TIMER if wp.0 == TIMER_METER => self.redraw(),
            _ => return None,
        }
        Some(LRESULT(0))
    }

    fn layout(&mut self, w: f32, h: f32, _head_x: f32, _head_r: f32) {
        self.w = w;
        self.h = h;
        let k = self.tab as usize;
        self.scroll[k] = self.scroll[k].min(self.max_scroll());
    }

    fn paint(&self, g: &Gfx) {
        Lights::paint(self, g)
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
        match self.pressed {
            Hit::Form(form::Hit::Slider(id, v)) => {
                self.drag = Some(id);
                self.slide(id, v);
            }
            Hit::Hue(v) => {
                self.drag = Some(C_COLORS);
                self.set_color(music::hsv(v * 360.0, 1.0));
            }
            _ => {}
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
        self.act(hit);
    }

    fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        let step = delta.signum() * 0.05;
        match self.hit(g, x, y) {
            Hit::Form(form::Hit::Slider(R_BRIGHT, _)) => self.set_brightness((self.settings.brightness + step).clamp(0.0, 1.0)),
            Hit::Form(form::Hit::Slider(R_SPEED, _)) => self.set_speed((self.settings.speed + step).clamp(0.0, 1.0)),
            Hit::Form(form::Hit::Slider(R_SENS, _)) => {
                self.slide(R_SENS, (sens_to_v(self.settings.sensitivity) + step).clamp(0.0, 1.0));
            }
            _ => {
                let k = self.tab as usize;
                self.scroll[k] = (self.scroll[k] - delta * 60.0).clamp(0.0, self.max_scroll());
                self.hover = self.hit(g, x, y);
                self.redraw();
                return;
            }
        }
        self.save();
    }

    /// Arama kutusu odaktayken tuşlar ona; Ctrl+F efekt aramasını açar.
    fn key(&mut self, vk: u16) -> bool {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        if ctrl && VIRTUAL_KEY(vk) == VK_F {
            self.select(Tab::Effects);
            self.search.focus(true);
            self.redraw();
            return true;
        }
        match self.search.key(vk, ctrl) {
            FieldKey::Ignored => false,
            FieldKey::Enter | FieldKey::Escape => {
                if VIRTUAL_KEY(vk) == VK_ESCAPE {
                    self.search.set("");
                }
                self.search.blur();
                self.scroll[Tab::Effects as usize] = 0.0;
                self.redraw();
                true
            }
            FieldKey::Handled => {
                self.scroll[Tab::Effects as usize] = 0.0;
                self.redraw();
                true
            }
        }
    }

    fn char(&mut self, c: char) -> bool {
        if !self.search.char(c) {
            return false;
        }
        self.scroll[Tab::Effects as usize] = 0.0;
        self.redraw();
        true
    }

    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        self.hit(g, x, y) != Hit::None
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        self.update_timer();
        if !visible {
            self.hover = Hit::None;
            self.drag = None;
            self.search.blur();
        }
    }

    fn kill_focus(&mut self) {
        if self.search.focused {
            self.search.blur();
            self.redraw();
        }
    }
}

impl Drop for Lights {
    fn drop(&mut self) {
        // Önce müzik (bağlantıya komut gönderen), sonra bağlantı.
        self.music = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_METER);
        }
    }
}
