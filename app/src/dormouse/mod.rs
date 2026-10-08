//! dormouse: dizüstünün üç vitesi. Prizde her şey tam hız; dışarıda rahat kalır (144 Hz, dengeli)
//! ama RTX uykuda; hayatta kalmada 60 Hz, verimlilik, ekran kısık, işlemci sınırlı. Hiçbir
//! uygulamayı kendisi kapatmaz: pile geçince kapatılması iyi olanları söyler. Şarj
//! takılıp çıkınca kendiliğinden vites değiştirir; pil eşiğin altına inince yalnızca uyarır,
//! hayatta kalmaya geçmek kullanıcının elindedir.
//!
//! Her dakika pilin ne kadar boşaldığını ölçer ve her vites için "dolu pil kaç saat gider"
//! hesabını kendi kullanımından kalibre eder. Pildeyken ayrık ekran kartını uyandıran uygulamayı
//! bildirir (nvidia-smi ile değil: o kartı uyandırır; Windows'un performans sayaçlarıyla).
//!
//! Motor hive sürecinin içinde (lyrebird gibi): güç olayları hive'ın penceresine gelir
//! (WM_POWERBROADCAST), ölçüm zamanlayıcısı sayfa görünmese de çalışır. Kurulum mevcut ayarları
//! `ayarlar.ini`'ye yedekler; kaldırma hepsini geri koyar.

mod power;
mod procs;
mod wmi;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::Power::{
    HPOWERNOTIFY, POWERBROADCAST_SETTING, RegisterPowerSettingNotification, UnregisterPowerSettingNotification,
};
use windows::Win32::System::SystemServices::{GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{GUID, PCWSTR, w};

use crate::gfx::{Gfx, Rect};
use crate::log;
use crate::ui::*;
use crate::util::{self, wide};


pub const TIMER_TICK: usize = 401;
pub const TIMER_UI: usize = 402;
const TICK_MS: u32 = 60_000;

pub const PLUGGED: u8 = 0;
pub const OUT: u8 = 1;
pub const SURVIVAL: u8 = 2;
const MODES: [u8; 3] = [PLUGGED, OUT, SURVIVAL];
/// Hayatta kalma eşiği seçenekleri (yüzde; 0 kapalı).
const THRESHOLDS: [u8; 4] = [0, 20, 30, 40];

const SURVIVAL_HZ: u32 = 60;
/// Kalan süre hesabı: son bu kadar dakikanın tüketimi, en az bu kadar örnekle.
const DRAIN_WINDOW: Duration = Duration::from_secs(15 * 60);
const MIN_DRAIN_SAMPLES: usize = 3;
/// Pilin Windows'un kendini kapattığı dilimi: kalan süreye sayılmaz.
const RESERVE: f64 = 0.07;
const SURVIVAL_BRIGHTNESS: u8 = 30;
const SURVIVAL_CPU: u32 = 40;
/// RTX'te bundan az bellek tutan süreç sayılmaz (sürücünün küçük ayırmaları).
const GUARD_MIN_BYTES: i64 = 48 << 20;
const GUARD_IGNORE: [&str; 7] = ["dwm", "csrss", "explorer", "hive", "System", "svchost", "Registry"];

const DEFAULT_CLOSE: &str = "RobloxPlayerBeta;javaw;ElyPrismLauncher;LightStudio-background;LightStudioHelper;OmenCommandCenterBackground;NVIDIA Overlay";
const DEFAULT_IGPU: &str = "brave;Spotify;claude;Discord;WhatsApp;Code;msedgewebview2";

const PANEL_HEAD: f32 = 56.0;
const ROW: f32 = 52.0;
const GAP: f32 = 12.0;
const CARD_H: f32 = 92.0;

pub fn dir() -> PathBuf {
    util::data_dir().join("dormouse")
}

fn ini() -> PathBuf {
    dir().join("ayarlar.ini")
}

pub fn installed() -> bool {
    ini().is_file()
}

// --- Ayarlar ve yedekler ---

#[derive(Clone, Debug)]
pub struct Config {
    /// Son seçilen vites.
    mode: u8,
    /// Şarj çıkınca Dışarıda, takılınca Prizde.
    auto: bool,
    /// Pil bunun altına inince Hayatta kalma (0 kapalı).
    threshold: u8,
    /// Pildeyken RTX'i uyandıranı bildir.
    guard: bool,
    /// Pile geçince kapatılacak süreç adları.
    close: Vec<String>,
    /// Tümleşik ekran kartına zorlanan uygulamalar (süreç adı).
    igpu: Vec<String>,
    // Kurulumdaki ayarlar: kaldırınca ve Prizde vitesinde geri konur.
    hz: u32,
    lid: u32,
    cpu: u32,
    overlay_ac: Option<GUID>,
    overlay_dc: Option<GUID>,
    /// Hayatta kalma ekranı kıstıysa önceki parlaklık.
    brightness_before: Option<u8>,
    /// Yazdığımız GPU tercihleri: (exe yolu, önceki değer).
    gpu_prefs: Vec<(String, Option<String>)>,
    /// Vites başına ölçüm: boşalan mWh, geçen dakika.
    stats: [(f64, f64); 3],
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mode: PLUGGED,
            auto: true,
            threshold: 30,
            guard: true,
            close: split(DEFAULT_CLOSE),
            igpu: split(DEFAULT_IGPU),
            hz: 60,
            lid: 1,
            cpu: 100,
            overlay_ac: None,
            overlay_dc: None,
            brightness_before: None,
            gpu_prefs: Vec::new(),
            stats: [(0.0, 0.0); 3],
        }
    }
}

fn split(s: &str) -> Vec<String> {
    s.split(';').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
}

fn guid_text(g: Option<GUID>) -> String {
    g.map(|g| format!("{:032x}", g.to_u128())).unwrap_or_default()
}

fn guid_parse(s: &str) -> Option<GUID> {
    u128::from_str_radix(s.trim(), 16).ok().map(GUID::from_u128)
}

impl Config {
    fn load() -> Self {
        let mut c = Config::default();
        let text = std::fs::read_to_string(ini()).unwrap_or_default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "vites" => c.mode = v.parse().unwrap_or(0).min(SURVIVAL),
                "otomatik" => c.auto = v == "1",
                "esik" => c.threshold = v.parse().unwrap_or(30),
                "bekci" => c.guard = v == "1",
                "kapat" => c.close = split(v),
                "igpu" => c.igpu = split(v),
                "yedek_hz" => c.hz = v.parse().unwrap_or(60),
                "yedek_kapak" => c.lid = v.parse().unwrap_or(1),
                "yedek_cpu" => c.cpu = v.parse().unwrap_or(100),
                "yedek_katman_ac" => c.overlay_ac = guid_parse(v),
                "yedek_katman_dc" => c.overlay_dc = guid_parse(v),
                "parlaklik_onceki" => c.brightness_before = v.parse().ok(),
                "gpu" => {
                    let (p, old) = v.split_once('|').unwrap_or((v, ""));
                    c.gpu_prefs.push((p.to_string(), (!old.is_empty()).then(|| old.to_string())));
                }
                "olcum1" | "olcum2" => {
                    let i = if k.trim() == "olcum1" { OUT } else { SURVIVAL } as usize;
                    if let Some((a, b)) = v.split_once(',') {
                        c.stats[i] = (a.trim().parse().unwrap_or(0.0), b.trim().parse().unwrap_or(0.0));
                    }
                }
                _ => {}
            }
        }
        c
    }

    fn save(&self) {
        let _ = std::fs::create_dir_all(dir());
        let mut s = format!(
            "vites={}\notomatik={}\nesik={}\nbekci={}\nkapat={}\nigpu={}\nyedek_hz={}\nyedek_kapak={}\nyedek_cpu={}\nyedek_katman_ac={}\nyedek_katman_dc={}\n",
            self.mode,
            self.auto as u8,
            self.threshold,
            self.guard as u8,
            self.close.join(";"),
            self.igpu.join(";"),
            self.hz,
            self.lid,
            self.cpu,
            guid_text(self.overlay_ac),
            guid_text(self.overlay_dc),
        );
        if let Some(b) = self.brightness_before {
            s += &format!("parlaklik_onceki={b}\n");
        }
        for (p, old) in &self.gpu_prefs {
            s += &format!("gpu={p}|{}\n", old.as_deref().unwrap_or(""));
        }
        s += &format!(
            "olcum1={:.1},{:.2}\nolcum2={:.1},{:.2}\n",
            self.stats[1].0, self.stats[1].1, self.stats[2].0, self.stats[2].1
        );
        if let Err(e) = std::fs::write(ini(), s) {
            log!("dormouse: ayarlar yazılamadı: {e}");
        }
    }

    /// Listedeki uygulamaları tümleşik ekran kartına alır; yeni bulunan yolları kaydeder.
    fn write_gpu_prefs(&mut self) {
        for path in procs::igpu_targets(&self.igpu) {
            if self.gpu_prefs.iter().any(|(p, _)| p.eq_ignore_ascii_case(&path)) {
                continue;
            }
            let old = procs::gpu_pref(&path);
            if old.as_deref() == Some(procs::GPU_POWER_SAVING) {
                continue; // kullanıcı zaten ayarlamış: bize ait değil
            }
            if procs::set_gpu_pref(&path, Some(procs::GPU_POWER_SAVING)) {
                self.gpu_prefs.push((path, old));
            }
        }
    }
}

/// Kaldırırken ve hive kapanmadan: her şeyi kurulumdaki haline döndürür.
fn restore(cfg: &mut Config) {
    let st = power::status();
    let overlay = if st.ac { cfg.overlay_ac } else { cfg.overlay_dc };
    power::set_overlay(&overlay.unwrap_or(if st.ac { power::OVERLAY_PERFORMANCE } else { power::OVERLAY_BALANCED }));
    power::set_refresh_rate(cfg.hz);
    power::set_lid_dc(cfg.lid);
    power::set_cpu_max_dc(cfg.cpu);
    if let Some(b) = cfg.brightness_before.take() {
        wmi::set_brightness(b);
    }
}

/// Kur: pil var mı, mevcut ayarları yedekle, uygulamaların GPU tercihini yaz.
pub fn install() -> Result<(), String> {
    let st = power::status();
    if !st.battery {
        return Err(t!("needs a laptop with a battery", "pili olan bir dizüstü ister").into());
    }
    let mut cfg = Config::default();
    cfg.hz = power::refresh_rate().unwrap_or(60);
    cfg.lid = power::lid_dc().unwrap_or(1);
    cfg.cpu = power::cpu_max_dc().unwrap_or(100);
    if st.ac {
        cfg.overlay_ac = power::overlay();
    } else {
        cfg.overlay_dc = power::overlay();
    }
    cfg.write_gpu_prefs();
    cfg.save();
    log!("dormouse kuruldu: {} Hz, kapak {}, cpu %{}, {} GPU tercihi", cfg.hz, cfg.lid, cfg.cpu, cfg.gpu_prefs.len());
    Ok(())
}

/// Kaldır: ayarları geri koy, GPU tercihlerini eski haline getir, klasörü sil.
pub fn uninstall() -> Result<(), String> {
    let mut cfg = Config::load();
    restore(&mut cfg);
    for (path, old) in &cfg.gpu_prefs {
        if procs::gpu_pref(path).as_deref() == Some(procs::GPU_POWER_SAVING) {
            procs::set_gpu_pref(path, old.as_deref());
        }
    }
    std::fs::remove_dir_all(dir())
        .map_err(|e| format!("{} {}: {e}", t!("could not delete", "silinemedi:"), dir().display()))
}

pub fn leftovers() -> Vec<String> {
    let d = dir();
    d.exists().then(|| d.display().to_string()).into_iter().collect()
}

/// `hive --dormouse-test`: motorun okuduğu her şeyi yazdırır, hiçbir şeyi değiştirmez.
pub fn probe() -> String {
    let st = power::status();
    let mut s = format!(
        "güç: {} · %{} · pil {}\n",
        if st.ac { "şarjda" } else { "pilde" },
        st.percent,
        if st.battery { "var" } else { "yok" }
    );
    match wmi::battery() {
        Some(b) => {
            s += &format!(
                "pil (WMI): kalan {} mWh · dolu {} mWh · hız {} mW · prizde {}\n",
                b.remaining_mwh, b.full_mwh, b.rate_mw, b.online
            )
        }
        None => s += "pil (WMI): okunamadı\n",
    }
    s += &format!("parlaklık: {}\n", wmi::brightness().map_or("okunamadı".into(), |b| format!("%{b}")));
    s += &format!(
        "güç katmanı: {}\n",
        power::overlay().map_or("okunamadı".into(), |g| power::overlay_name(&g).to_string())
    );
    s += &format!("ekran: {} Hz\n", power::refresh_rate().map_or("?".into(), |h| h.to_string()));
    s += &format!("pilde kapak: {:?} · pilde cpu üst sınırı: {:?}\n", power::lid_dc(), power::cpu_max_dc());
    match procs::nvidia_luid() {
        Some(luid) => {
            s += &format!("NVIDIA luid: {luid}\n");
            let list = procs::list();
            for (pid, bytes) in procs::gpu_users(&luid) {
                let name = list.iter().find(|p| p.pid == pid).map_or("?", |p| p.name.as_str());
                s += &format!("  RTX'te: {name} (pid {pid}) {} MB\n", bytes >> 20);
            }
        }
        None => s += "NVIDIA bağdaştırıcısı yok\n",
    }
    let cfg = Config::load();
    s += &format!("iGPU hedefleri: {}\n", procs::igpu_targets(&cfg.igpu).join(" · "));
    s += &format!("kurulu: {} · ayarlar: {}\n", installed(), ini().display());
    s
}

// --- Sayfa ve motor ---

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Folder,
    Card(u8),
    Auto,
    Threshold(usize),
    Guard,
}

struct Layout {
    cards: [Rect; 3],
    hint: Option<Rect>,
    panel1: Rect,
    auto_row: Rect,
    thr_row: Rect,
    guard_row: Rect,
    bottom: f32,
}

pub struct Dormouse {
    hwnd: HWND,
    cfg: Config,
    /// Uygulanan vites.
    mode: u8,
    ac: bool,
    percent: u8,
    bat: Option<wmi::Battery>,
    /// Son ölçüm: zaman, kalan mWh, vites (kalibrasyon için).
    last: Option<(Instant, i64, u8)>,
    /// Son dakikaların gerçek tüketimi (zaman, mW): kalan süre bundan hesaplanır.
    drains: VecDeque<(Instant, f64)>,
    /// Kullanıcı elle vites seçti: bir sonraki şarj olayına kadar otomatik geçiş yok.
    manual: bool,
    /// Pil eşiğin altına indi uyarısı verildi (eşiğin üstüne çıkınca ya da şarj takılınca sıfırlanır).
    low_warned: bool,
    luid: Option<String>,
    /// Bu oturumda uyarılan uygulamalar (bir kez).
    warned: Vec<String>,
    /// Pildeyken RTX'i tutan uygulamalar (son ölçüm).
    awake: Vec<String>,
    /// Pildeyken kapatılması iyi olan, şu an çalışan uygulamalar (kapatma listesinden).
    suggest: Vec<String>,
    notice: Option<(String, String)>,
    notifications: Vec<HPOWERNOTIFY>,
    scroll: f32,
    hover: Hit,
    pressed: Hit,
    w: f32,
    h: f32,
    head_x: f32,
    head_r: f32,
    visible: bool,
}

fn mode_name(m: u8) -> &'static str {
    match m {
        PLUGGED => t!("Plugged in", "Prizde"),
        OUT => t!("Out", "Dışarıda"),
        _ => t!("Survival", "Hayatta kalma"),
    }
}

/// Segoe MDL2: şimşek, yarı dolu pil, hilal (uyuyan fare).
fn mode_icon(m: u8) -> &'static str {
    match m {
        PLUGGED => "\u{E945}",
        OUT => "\u{E855}",
        _ => "\u{E708}",
    }
}

/// Saat ve dakika ("2 h 10 min" / "2 sa 10 dk").
fn hm(hours: f64) -> String {
    let m = (hours * 60.0).round().max(0.0) as u64;
    format!("{} {} {} {}", m / 60, t!("h", "sa"), m % 60, t!("min", "dk"))
}

fn watts(mw: i64) -> String {
    let s = format!("{:.1} W", mw as f64 / 1000.0);
    if crate::i18n::turkish() { s.replace('.', ",") } else { s }
}

impl Dormouse {
    pub fn start(hwnd: HWND) -> Self {
        let cfg = Config::load();
        let st = power::status();
        let mut d = Self {
            hwnd,
            mode: u8::MAX,
            ac: st.ac,
            percent: st.percent,
            bat: None,
            last: None,
            drains: VecDeque::new(),
            manual: false,
            low_warned: false,
            luid: procs::nvidia_luid(),
            warned: Vec::new(),
            awake: Vec::new(),
            suggest: Vec::new(),
            notice: None,
            notifications: Vec::new(),
            cfg,
            scroll: 0.0,
            hover: Hit::None,
            pressed: Hit::None,
            w: 0.0,
            h: 0.0,
            head_x: 0.0,
            head_r: 0.0,
            visible: false,
        };
        unsafe {
            for guid in [&GUID_ACDC_POWER_SOURCE, &GUID_BATTERY_PERCENTAGE_REMAINING] {
                if let Ok(h) = RegisterPowerSettingNotification(HANDLE(hwnd.0), guid, DEVICE_NOTIFY_WINDOW_HANDLE) {
                    d.notifications.push(h);
                }
            }
            SetTimer(Some(hwnd), TIMER_TICK, TICK_MS, None);
        }
        let first = if d.cfg.auto { d.auto_mode() } else { d.cfg.mode };
        d.switch(first, false);
        d.tick();
        log!(
            "dormouse: {} vitesinde, {} , RTX {}",
            mode_name(d.mode),
            if d.ac { "şarjda" } else { "pilde" },
            d.luid.as_deref().unwrap_or("yok")
        );
        d
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    pub fn take_notice(&mut self) -> Option<(String, String)> {
        self.notice.take()
    }

    /// Güç kaynağına göre otomatik vites. Hayatta kalma'ya geçiş hep kullanıcının elinde:
    /// eşik yalnızca uyarır.
    fn auto_mode(&self) -> u8 {
        if self.ac { PLUGGED } else { OUT }
    }

    /// Vitesi uygular (aynıysa dokunmaz). `announce`: tepside balon.
    fn switch(&mut self, mode: u8, announce: bool) {
        if mode == self.mode {
            return;
        }
        self.apply(mode);
        self.mode = mode;
        self.cfg.mode = mode;
        self.cfg.save();
        self.last = None;
        self.drains.clear();
        if announce {
            let what = match mode {
                PLUGGED => t!("Full speed again.", "Yine tam hız."),
                OUT => t!("RTX asleep.", "RTX uykuda."),
                _ => t!("60 Hz, dim screen, CPU capped.", "60 Hz, ekran kısık, işlemci sınırlı."),
            };
            let hint = if self.suggest.is_empty() {
                String::new()
            } else {
                format!(" {}: {}.", t!("Worth closing", "Kapatman iyi olur"), self.suggest.join(", "))
            };
            self.notice = Some((format!("{} · {}", t!("Battery", "Pil"), mode_name(mode)), format!("{what}{hint}")));
        }
        self.redraw();
    }

    /// Kapatma listesinden şu an çalışanlar: dormouse kapatmaz, söyler.
    fn running_junk(&self) -> Vec<String> {
        let mut out: Vec<String> = procs::list()
            .into_iter()
            .filter(|p| self.cfg.close.iter().any(|n| n.eq_ignore_ascii_case(&p.name)))
            .map(|p| p.name)
            .collect();
        out.sort_by_key(|s| s.to_lowercase());
        out.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        out
    }

    fn apply(&mut self, mode: u8) {
        self.suggest = if mode == PLUGGED { Vec::new() } else { self.running_junk() };
        let cfg = &mut self.cfg;
        match mode {
            PLUGGED => {
                let overlay = if self.ac { cfg.overlay_ac } else { cfg.overlay_dc };
                power::set_overlay(&overlay.unwrap_or(if self.ac {
                    power::OVERLAY_PERFORMANCE
                } else {
                    power::OVERLAY_BALANCED
                }));
                power::set_refresh_rate(cfg.hz);
                power::set_lid_dc(cfg.lid);
                power::set_cpu_max_dc(cfg.cpu);
            }
            // Dışarıda rahat kalır: 144 Hz ve dengeli güç; yalnızca görünmeyenler değişir.
            OUT => {
                cfg.write_gpu_prefs();
                power::set_overlay(&power::OVERLAY_BALANCED);
                power::set_refresh_rate(cfg.hz);
                power::set_lid_dc(power::LID_HIBERNATE);
                power::set_cpu_max_dc(cfg.cpu);
            }
            _ => {
                cfg.write_gpu_prefs();
                power::set_overlay(&power::OVERLAY_EFFICIENCY);
                power::set_refresh_rate(SURVIVAL_HZ);
                power::set_lid_dc(power::LID_HIBERNATE);
                power::set_cpu_max_dc(SURVIVAL_CPU);
            }
        }
        // Parlaklık yalnızca hayatta kalmada kısılır; çıkınca geri gelir.
        if mode == SURVIVAL {
            if cfg.brightness_before.is_none() {
                cfg.brightness_before = wmi::brightness();
            }
            wmi::set_brightness(SURVIVAL_BRIGHTNESS);
        } else if let Some(b) = cfg.brightness_before.take() {
            wmi::set_brightness(b);
        }
        log!(
            "dormouse: {} uygulandı{}",
            mode_name(mode),
            if self.suggest.is_empty() { String::new() } else { format!(", kapatılması iyi olur: {}", self.suggest.join(", ")) }
        );
    }

    /// WM_POWERBROADCAST / PBT_POWERSETTINGCHANGE.
    pub fn power_broadcast(&mut self, lp: LPARAM) {
        let Some(s) = (unsafe { (lp.0 as *const POWERBROADCAST_SETTING).as_ref() }) else { return };
        if s.DataLength < 4 {
            return;
        }
        let v = unsafe { *(s.Data.as_ptr() as *const u32) };
        if s.PowerSetting == GUID_ACDC_POWER_SOURCE {
            let ac = v == 0;
            if ac != self.ac {
                self.ac = ac;
                self.manual = false;
                self.warned.clear();
                self.low_warned = false;
                self.last = None;
                if self.cfg.auto {
                    self.switch(self.auto_mode(), true);
                }
                self.redraw();
            }
        } else if s.PowerSetting == GUID_BATTERY_PERCENTAGE_REMAINING {
            self.percent = v.min(100) as u8;
            let low = !self.ac && self.cfg.threshold > 0 && self.percent <= self.cfg.threshold;
            if low && !self.low_warned && self.mode != SURVIVAL {
                self.low_warned = true;
                self.notice = Some((
                    format!("{} · %{}", t!("Battery", "Pil"), self.percent),
                    t!(
                        "Battery is getting low. Switch to Survival from the Battery page if you want.",
                        "Pil azalıyor. İstersen Pil sayfasından Hayatta kalma'ya geç."
                    )
                    .into(),
                ));
            }
            if !low {
                self.low_warned = false;
            }
            self.redraw();
        }
    }

    pub fn timer(&mut self, id: usize) {
        match id {
            TIMER_TICK => self.tick(),
            TIMER_UI => {
                self.bat = wmi::battery();
                let st = power::status();
                self.percent = st.percent;
                self.redraw();
            }
            _ => {}
        }
    }

    /// Dakikada bir: pil ölçümü, kalibrasyon, RTX bekçisi.
    fn tick(&mut self) {
        let st = power::status();
        self.ac = st.ac;
        self.percent = st.percent;
        self.bat = wmi::battery();
        let now = Instant::now();
        match self.bat {
            Some(b) if !self.ac && !b.online => {
                if let Some((t0, rem0, m0)) = self.last
                    && m0 == self.mode
                    && m0 != PLUGGED
                {
                    let dt = now.duration_since(t0).as_secs_f64() / 60.0;
                    let drained = (rem0 - b.remaining_mwh) as f64;
                    // Uyku ve kilitli ekran aralıkları ölçüme girmesin: yalnızca ~1 dakikalık adımlar.
                    if (0.75..=2.5).contains(&dt) && drained > 0.0 && drained < 5000.0 {
                        let s = &mut self.cfg.stats[m0 as usize];
                        s.0 += drained;
                        s.1 += dt;
                        self.cfg.save();
                        self.drains.push_back((now, drained / dt * 60.0));
                    }
                }
                self.last = Some((now, b.remaining_mwh, self.mode));
                while self.drains.front().is_some_and(|d| now.duration_since(d.0) > DRAIN_WINDOW) {
                    self.drains.pop_front();
                }
            }
            _ => {
                self.last = None;
                self.drains.clear();
            }
        }
        self.guard();
        if self.mode != PLUGGED {
            self.suggest = self.running_junk();
        }
        if self.visible {
            self.redraw();
        }
    }

    fn guard(&mut self) {
        self.awake.clear();
        if self.ac || !self.cfg.guard {
            return;
        }
        let Some(luid) = self.luid.as_deref() else { return };
        let users = procs::gpu_users(luid);
        if users.is_empty() {
            return;
        }
        let procs = procs::list();
        for (pid, bytes) in users {
            if bytes < GUARD_MIN_BYTES {
                continue;
            }
            let Some(p) = procs.iter().find(|p| p.pid == pid) else { continue };
            if GUARD_IGNORE.iter().any(|i| i.eq_ignore_ascii_case(&p.name))
                || self.awake.iter().any(|a| a.eq_ignore_ascii_case(&p.name))
            {
                continue;
            }
            self.awake.push(p.name.clone());
            if self.warned.iter().any(|w| w.eq_ignore_ascii_case(&p.name)) {
                continue;
            }
            self.warned.push(p.name.clone());
            self.notice = Some((
                t!("The RTX is awake", "RTX uyanık").into(),
                t!(
                    format!(
                        "{} is using the RTX on battery. Close it, or set it to the power-saving GPU in Windows graphics settings.",
                        p.name
                    ),
                    format!(
                        "{} pildeyken RTX'i kullanıyor. Kapat ya da Windows grafik ayarlarından güç tasarrufu GPU'suna al.",
                        p.name
                    )
                ),
            ));
            log!("dormouse: RTX'i uyandıran: {} ({} MB)", p.name, bytes >> 20);
        }
    }

    // --- Tahminler ---

    /// Vitesin kalibre edilmiş tüketimi (mW); 10 dakikadan az ölçümle yok.
    fn rate_for(&self, mode: u8) -> Option<f64> {
        let (mwh, min) = self.cfg.stats[mode as usize];
        (min >= 10.0 && mwh > 0.0).then(|| mwh / min * 60.0)
    }

    fn full_text(&self, mode: u8) -> String {
        if mode == PLUGGED {
            return String::new();
        }
        let full = self.bat.map(|b| b.full_mwh).filter(|&f| f > 0);
        match (self.rate_for(mode), full) {
            (Some(r), Some(f)) => format!("{} {}", t!("full charge ≈", "dolu pil ≈"), hm(f as f64 / r)),
            _ => {
                let min = self.cfg.stats[mode as usize].1;
                if min > 0.0 {
                    t!(format!("measuring… {min:.0} min so far"), format!("ölçülüyor… {min:.0} dk oldu"))
                } else {
                    t!("not measured yet", "henüz ölçülmedi").into()
                }
            }
        }
    }

    /// Kalan süre, "en az" olacak şekilde: (saat, kullanılan tüketim mW, ölçüme dayalı mı).
    /// Tüketim = son DRAIN_WINDOW dakikanın gerçek ortalaması ile vitesin kalibre değerinin
    /// büyüğü, üstüne %10 pay. Pilin Windows'un kapandığı dilimi (%7) hesaba katılmaz. Ölçüm
    /// henüz yoksa (pile yeni geçildi) anlık değer ve %15 pay ile geçici tahmin.
    fn remaining(&self) -> Option<(f64, f64, bool)> {
        let b = self.bat?;
        if self.ac || b.online || b.remaining_mwh <= 0 {
            return None;
        }
        let calib = self.rate_for(self.mode).unwrap_or(0.0);
        let recent = (self.drains.len() >= MIN_DRAIN_SAMPLES)
            .then(|| self.drains.iter().map(|d| d.1).sum::<f64>() / self.drains.len() as f64);
        let (base, measured) = match recent {
            Some(r) => (r.max(calib), true),
            None => ((b.rate_mw as f64).max(calib), false),
        };
        if base <= 0.0 {
            return None;
        }
        let rate = base * if measured { 1.10 } else { 1.15 };
        let usable = (b.remaining_mwh as f64 - b.full_mwh as f64 * RESERVE).max(0.0);
        Some((usable / rate, base, measured))
    }

    fn status_text(&self) -> String {
        if self.ac {
            return format!("{} · %{}", t!("Plugged in", "Şarjda"), self.percent);
        }
        let mut s = match self.remaining() {
            Some((h, rate, true)) => format!(
                "{} {} {} · %{} · {} {}",
                t!("at least", "en az"),
                hm(h),
                t!("left", "kaldı"),
                self.percent,
                t!("avg", "ort."),
                watts(rate as i64)
            ),
            Some((h, _, false)) => {
                format!("~{} {} · %{} · {}", hm(h), t!("left", "kaldı"), self.percent, t!("measuring…", "ölçülüyor…"))
            }
            None => format!("{} · %{}", t!("On battery", "Pilde"), self.percent),
        };
        if !self.awake.is_empty() {
            s += &format!(" · RTX: {}", self.awake.join(", "));
        }
        s
    }

    // --- Yerleşim ---

    fn folder_rect(&self) -> Rect {
        Rect::new(self.head_r - 36.0, HEAD_CY - 18.0, self.head_r, HEAD_CY + 18.0)
    }

    fn layout_all(&self) -> Layout {
        let w = self.w;
        let top = HEADER + 16.0 - self.scroll;
        let cw = (w - 2.0 * PAD - 2.0 * GAP) / 3.0;
        let ct = top + 30.0;
        let cards = [0, 1, 2].map(|i| {
            let l = PAD + i as f32 * (cw + GAP);
            Rect::new(l, ct, l + cw, ct + CARD_H)
        });
        let inner = |t: f32| Rect::new(PAD + 20.0, t, w - PAD - 20.0, t + ROW);
        let mut y = ct + CARD_H + 12.0;
        // Pildeyken kapatılması iyi olanlar (varsa) kartların altında tek satır.
        let hint = (!self.suggest.is_empty()).then(|| Rect::new(PAD, y, w - PAD, y + 22.0));
        if hint.is_some() {
            y += 26.0;
        }
        let p1_t = y + 4.0;
        y = p1_t + PANEL_HEAD - 12.0;
        let auto_row = inner(y);
        let thr_row = inner(y + ROW);
        let guard_row = inner(y + 2.0 * ROW);
        let panel1 = Rect::new(PAD, p1_t, w - PAD, guard_row.b + 8.0);
        Layout { cards, hint, panel1, auto_row, thr_row, guard_row, bottom: panel1.b + 24.0 }
    }

    fn max_scroll(&self) -> f32 {
        let l = self.layout_all();
        (l.bottom + self.scroll - self.h).max(0.0)
    }

    fn segments(g: &Gfx, r: Rect, labels: &[String]) -> Vec<Rect> {
        let widths: Vec<f32> = labels.iter().map(|l| g.measure(l, &g.f.button) + 26.0).collect();
        let total: f32 = widths.iter().sum::<f32>() + 6.0;
        let mut x = r.r - total + 3.0;
        widths
            .iter()
            .map(|&wd| {
                let s = Rect::new(x, r.cy() - 14.0, x + wd, r.cy() + 14.0);
                x += wd;
                s
            })
            .collect()
    }

    fn threshold_labels() -> Vec<String> {
        THRESHOLDS.iter().map(|&t| if t == 0 { t!("Off", "Kapalı").to_string() } else { format!("%{t}") }).collect()
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return if self.folder_rect().contains(x, y) { Hit::Folder } else { Hit::None };
        }
        let l = self.layout_all();
        if let Some(i) = l.cards.iter().position(|c| c.contains(x, y)) {
            return Hit::Card(MODES[i]);
        }
        if l.auto_row.contains(x, y) {
            return Hit::Auto;
        }
        if let Some(k) = Self::segments(g, l.thr_row, &Self::threshold_labels()).iter().position(|s| s.contains(x, y)) {
            return Hit::Threshold(k);
        }
        if l.guard_row.contains(x, y) {
            return Hit::Guard;
        }
        Hit::None
    }

    // --- Çizim ---

    fn panel(g: &Gfx, r: Rect, title: &str, sub: &str) {
        g.fill(r, 12.0, PANEL);
        g.stroke(r, 12.0, LINE, 1.0);
        let t = Rect::new(r.l + 20.0, r.t + 10.0, r.r - 20.0, r.t + 34.0);
        g.text(title, &g.f.strong, t, TEXT);
        g.text(sub, &g.f.small, Rect::new(t.l, t.b - 2.0, t.r, t.b + 16.0), MUTED);
    }

    fn paint_segments(&self, g: &Gfx, r: Rect, labels: &[String], selected: usize, hover: impl Fn(usize) -> bool) {
        let segs = Self::segments(g, r, labels);
        if let (Some(first), Some(last)) = (segs.first(), segs.last()) {
            g.fill(Rect::new(first.l - 3.0, first.t - 3.0, last.r + 3.0, last.b + 3.0), 8.0, HOVER);
        }
        for (j, (s, label)) in segs.iter().zip(labels).enumerate() {
            let c = if j == selected {
                g.fill(*s, 6.0, accent());
                on_accent()
            } else if hover(j) {
                g.fill(*s, 6.0, SEL);
                TEXT
            } else {
                MUTED
            };
            g.text(label, &g.f.button, *s, c);
        }
    }

    pub fn paint(&self, g: &Gfx) {
        let w = self.w;
        let dot = if self.ac {
            GREEN
        } else if self.awake.is_empty() {
            accent()
        } else {
            RED
        };
        status(g, self.head_x + 16.0, HEAD_CY, dot, &self.status_text(), MUTED, self.folder_rect().l - 8.0);
        icon_button(g, self.folder_rect(), ICON_FOLDER, MUTED, self.hover == Hit::Folder);

        let l = self.layout_all();
        g.clip(Rect::new(0.0, HEADER, w, self.h), || {
            let top = HEADER + 16.0 - self.scroll;
            g.text(t!("Gear", "Vites"), &g.f.strong, Rect::new(PAD, top, w / 2.0, top + 24.0), TEXT);
            let how = if self.cfg.auto && !self.manual { t!("automatic", "otomatik") } else { t!("chosen by you", "elle seçildi") };
            g.text(how, &g.f.small_right, Rect::new(w / 2.0, top + 2.0, w - PAD, top + 22.0), FAINT);

            for (i, c) in l.cards.iter().enumerate() {
                let m = MODES[i];
                let selected = self.mode == m;
                let hovered = self.hover == Hit::Card(m);
                g.fill(*c, 12.0, if hovered && !selected { HOVER } else { PANEL });
                if selected {
                    g.stroke(*c, 12.0, accent(), 2.0);
                } else {
                    g.stroke(*c, 12.0, LINE, 1.0);
                }
                let x = c.l + 16.0;
                let fg = if selected { accent() } else { TEXT };
                g.text(mode_icon(m), &g.f.icon, Rect::new(x, c.t + 14.0, x + 26.0, c.t + 40.0), if selected { accent() } else { MUTED });
                g.text(mode_name(m), &g.f.strong, Rect::new(x + 34.0, c.t + 14.0, c.r - 16.0, c.t + 40.0), fg);
                let est = self.full_text(m);
                let ec = if self.rate_for(m).is_some() { TEXT } else { FAINT };
                g.text(&est, &g.f.small, Rect::new(x, c.b - 32.0, c.r - 16.0, c.b - 10.0), ec);
            }

            // İki satırlı ayar satırı: başlık ve altında kısa açıklama; sağda kontrol.
            let setting = |r: Rect, title: &str, sub: &str, right: f32| {
                g.text(title, &g.f.text, Rect::new(r.l, r.t + 4.0, r.r - right, r.cy() + 4.0), TEXT);
                g.text(sub, &g.f.small, Rect::new(r.l, r.cy() - 2.0, r.r - right, r.b), MUTED);
            };
            Self::panel(g, l.panel1, t!("Automatic", "Otomatik"), "");
            let r = l.auto_row;
            setting(r, t!("Follow the charger", "Şarja göre geç"), t!("unplugged → Out, plugged in → Plugged in", "çıkınca Dışarıda, takılınca Prizde"), 60.0);
            toggle(g, toggle_rect(r.r, r.cy()), self.cfg.auto, accent(), true);
            let r = l.thr_row;
            g.fill(Rect::new(r.l, r.t, r.r, r.t + 1.0), 0.0, HOVER);
            setting(r, t!("Low battery alert", "Pil uyarısı"), t!("a notification; switching stays yours", "bildirim gelir, geçiş sende"), 300.0);
            let sel = THRESHOLDS.iter().position(|&t| t == self.cfg.threshold).unwrap_or(0);
            self.paint_segments(g, r, &Self::threshold_labels(), sel, |k| self.hover == Hit::Threshold(k));
            let r = l.guard_row;
            g.fill(Rect::new(r.l, r.t, r.r, r.t + 1.0), 0.0, HOVER);
            setting(r, t!("RTX watch", "RTX bekçisi"), t!("names the app that wakes the discrete GPU on battery", "pildeyken RTX'i uyandıran uygulamayı söyler"), 60.0);
            toggle(g, toggle_rect(r.r, r.cy()), self.cfg.guard, accent(), self.luid.is_some());

            if let Some(r) = l.hint {
                let text = format!("{} · {}", t!("Worth closing", "Kapatman iyi olur"), self.suggest.join(", "));
                g.text(&text, &g.f.small, r, MUTED);
            }
        });
    }

    fn open_folder(&self) {
        let _ = std::fs::create_dir_all(dir());
        let p = wide(&dir().display().to_string());
        unsafe {
            ShellExecuteW(None, w!("open"), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        }
    }
}

impl ToolPage for Dormouse {
    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn paint(&self, g: &Gfx) {
        Dormouse::paint(self, g)
    }

    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
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
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        let hit = self.hit(g, x, y);
        if hit != std::mem::replace(&mut self.pressed, Hit::None) {
            return;
        }
        match hit {
            Hit::Folder => self.open_folder(),
            Hit::Card(m) => {
                self.manual = true;
                self.switch(m, false);
            }
            Hit::Auto => {
                self.cfg.auto = !self.cfg.auto;
                self.manual = false;
                self.cfg.save();
                if self.cfg.auto {
                    self.switch(self.auto_mode(), false);
                }
                self.redraw();
            }
            Hit::Threshold(k) => {
                self.cfg.threshold = THRESHOLDS[k];
                self.cfg.save();
                self.low_warned = false;
                self.redraw();
            }
            Hit::Guard => {
                self.cfg.guard = !self.cfg.guard;
                self.cfg.save();
                self.guard();
                self.redraw();
            }
            Hit::None => {}
        }
    }

    fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        self.scroll = (self.scroll - delta * 60.0).clamp(0.0, self.max_scroll());
        self.hover = self.hit(g, x, y);
        self.redraw();
    }

    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        self.hit(g, x, y) != Hit::None
    }

    fn tip(&self) -> Option<(Rect, String)> {
        match self.hover {
            Hit::Folder => {
                Some((self.folder_rect(), t!("Settings and measurements folder", "Ayar ve ölçüm klasörü").into()))
            }
            _ => None,
        }
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        unsafe {
            if visible {
                self.bat = wmi::battery();
                SetTimer(Some(self.hwnd), TIMER_UI, 3000, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_UI);
                self.hover = Hit::None;
            }
        }
    }
}

impl Drop for Dormouse {
    /// Araç kaldırılırken ya da hive kapanırken: olay kayıtları ve zamanlayıcılar bırakılır.
    /// Ayarlar olduğu gibi kalır (hive bir sonraki açılışta vitesi yeniden uygular); kaldırmada
    /// `uninstall` hepsini geri koyar.
    fn drop(&mut self) {
        unsafe {
            for h in self.notifications.drain(..) {
                let _ = UnregisterPowerSettingNotification(h);
            }
            let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
            let _ = KillTimer(Some(self.hwnd), TIMER_UI);
        }
    }
}
