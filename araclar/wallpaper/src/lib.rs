//! Duvar kâğıdı: GPU ile çizilen canlı duvar kâğıdı. Motor ayrı bir süreç (`cheshire.exe --hub`):
//! GPU tarafındaki bir çökme winloom'ı götürmesin. Komutlar `WM_COPYDATA` ile gider, motor
//! durumunu `durum.txt`'ye yazıp WM_STATE ile haber verir (bkz. wallpaper/src/hub.rs).
//!
//! Sayfa: başlıkta durum ve düğmeler; altında sayfa kitiyle küçük resimli galeri, seçili duvar
//! kâğıdının ayarları ve genel ayarlar. Küçük resimleri motorun `--onizleme` komutu üretir,
//! önbellekte tutulur.
//!
//! Diskteki adlar (`Programs\cheshire\cheshire.exe`, .cheshire dosyaları) motorun ilk adından
//! kalma; kurulu olanlar bulunabilsin diye değişmez.

#[macro_use]
extern crate ortak;

mod tool;
pub use tool::TOOL;

use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Instant, UNIX_EPOCH};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use ortak::form::{self, Ctl, Form, Item, Row};
use ortak::gfx::{Color, Gfx, ImageId, Rect};
use ortak::log;
use ortak::ui::*;
use ortak::util::{self, Res};

/// wallpaper/src/hub.rs ile aynı olmalı.
pub const WM_STATE: u32 = WM_APP + 40;
const COPYDATA_COMMAND: usize = 0xD0FB;
/// Bir küçük resim hazır.
pub const WM_THUMB: u32 = WM_APP + 41;
/// Motor başlatılmaya hazır (eskisi kapandı, exe güncel).
pub const WM_RELAUNCH: u32 = WM_APP + 42;

/// Motor winloom'ın içinde gelir: kurulum ve güncelleme bu kopyayı yazmaktır, internet gerekmez.
static ENGINE: &[u8] = include_bytes!(env!("WALLPAPER_EXE"));

const SWATCHES: [&str; 6] = ["#4fc3f7", "#ff6ec7", "#b388ff", "#69f0ae", "#ffd740", "#ffffff"];
const FPS: [u32; 4] = [30, 60, 120, 144];
/// Galeri: kutucuğun en dar hali, kutucuklar arası boşluk ve resmin altındaki ad şeridi.
const CARD_MIN: f32 = 160.0;
const GAP: f32 = 16.0;
const CAPTION: f32 = 34.0;
/// Bu kadar seçeneğe kadar bölmeli seçici; fazlası ‹ seçili › ile.
const MAX_SEGMENTS: usize = 5;
const STEPPER_W: f32 = 248.0;
/// Formdaki kimlikler: parametre satırları sıralarıyla (0, 1, …), ötekiler bunlar.
const GALLERY: u32 = 1000;
const RESET: u32 = 1001;
const FPS_ROW: u32 = 1002;
const BATTERY: u32 = 1003;

/// %LOCALAPPDATA%\Programs\wallpaper — motorun exe'si, ayarları ve duvar kâğıtları.
pub fn app_dir() -> PathBuf {
    util::local_programs().join("cheshire")
}

pub fn exe() -> PathBuf {
    app_dir().join("cheshire.exe")
}

fn wallpapers_dir() -> PathBuf {
    app_dir().join("duvarlar")
}

fn thumbs_dir() -> PathBuf {
    util::data_dir().join("onizleme")
}

/// Aynı duvar kâğıdının eski sürümlerine ait küçük resimleri siler.
fn remove_old_thumbs(file: &str, keep: &Path) {
    let prefix = format!("{file}-");
    for e in std::fs::read_dir(thumbs_dir()).into_iter().flatten().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p != keep && name.starts_with(&prefix) && name.ends_with(".png") {
            let _ = std::fs::remove_file(p);
        }
    }
}

pub fn installed() -> bool {
    exe().is_file()
}

fn quiet(c: &mut Command) -> &mut Command {
    c.stdin(Stdio::null()).creation_flags(0x0800_0000) // CREATE_NO_WINDOW
}

fn engine_window() -> Option<HWND> {
    unsafe { FindWindowW(w!("CheshireTray"), PCWSTR::null()).ok() }
}

/// Çalışan motora kapan der (duvar kâğıdını Windows'unkine geri koyar) ve kapanmasını bekler.
fn stop_engine() {
    let Some(h) = engine_window() else { return };
    unsafe {
        // Pencere kapanınca süreç duvar kâğıdını geri yükleyip birkaç saniye daha yaşayabilir;
        // o sürede exe kilitli kalır ve yeni motor tek-kopya kilidine takılır. Sürecin kendisi beklenir.
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let process = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid).ok();
        let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
        match process {
            Some(p) => {
                if WaitForSingleObject(p, 8000) == WAIT_TIMEOUT {
                    log!("duvar kâğıdı motoru kapanmadı, sonlandırılıyor");
                    let _ = TerminateProcess(p, 1);
                    WaitForSingleObject(p, 2000);
                }
                let _ = CloseHandle(p);
            }
            None => {
                for _ in 0..60 {
                    if engine_window().is_none() {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    }
}

/// Exe winloom'daki sürümle aynı değilse yazar (çalışan motor kapalı olmalı).
fn write_engine() -> Result<(), String> {
    if std::fs::read(exe()).is_ok_and(|d| d == ENGINE) {
        return Ok(());
    }
    std::fs::create_dir_all(app_dir()).map_err(|e| e.to_string())?;
    let mut last = String::new();
    for i in 0..20 {
        match std::fs::write(exe(), ENGINE) {
            Ok(()) => return Ok(()),
            Err(e) => last = e.to_string(),
        }
        // Penceresi kapanan motor süreci birkaç saniye daha yaşayabilir. Çalışan exe silinemez
        // ama yeniden adlandırılabilir: kenara çekilir, motor açılınca `.eski`yi kendisi siler.
        if i == 3 {
            let old = app_dir().join("cheshire.exe.eski");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(exe(), &old);
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    Err(format!("{} {last}", t!("could not write cheshire.exe:", "cheshire.exe yazılamadı:")))
}

/// Kurulumdan geriye kalan her şey; boşsa iz yok.
pub fn leftovers() -> Vec<String> {
    let mut left: Vec<String> =
        [app_dir(), thumbs_dir()].iter().filter(|p| p.exists()).map(|p| p.display().to_string()).collect();
    if engine_window().is_some() {
        left.push(t!("running wallpaper engine", "çalışan duvar kâğıdı motoru").into());
    }
    left
}

/// Kur: motoru yazar. Örnek duvar kâğıtlarını motor ilk açılışta kendisi koyar.
pub fn install() -> Result<(), String> {
    write_engine()
}

/// Kaldır: motoru durdurur; klasörü (duvar kâğıtları dahil), önizlemeleri ve kayıtları siler.
pub fn uninstall() -> Result<(), String> {
    stop_engine();
    let _ = std::fs::remove_dir_all(thumbs_dir());
    std::fs::remove_dir_all(app_dir()).map_err(|e| format!("{} {}: {e}", t!("could not delete", "silinemedi:"), app_dir().display()))
}


fn send(line: &str) -> bool {
    let Some(hwnd) = engine_window() else { return false };
    let data = line.as_bytes();
    let cds = COPYDATASTRUCT { dwData: COPYDATA_COMMAND, cbData: data.len() as u32, lpData: data.as_ptr() as *mut _ };
    unsafe { SendMessageW(hwnd, WM_COPYDATA, None, Some(LPARAM(&cds as *const _ as isize))).0 != 0 }
}

/// `WM_COPYDATA` gönderilmek zorunda (veri çağrı süresince yaşamalı) ve gönderen, motor mesajı
/// işleyene kadar bekler. Motor o sırada shader derliyorsa bu saniyeler sürer: winloom'ın arayüzü
/// donmasın diye komutlar tek bir arka plan iş parçacığından sırayla gider.
fn send_async(line: String) {
    use std::sync::mpsc::{Sender, channel};
    use std::sync::{Mutex, OnceLock};
    static QUEUE: OnceLock<Mutex<Sender<String>>> = OnceLock::new();
    let q = QUEUE.get_or_init(|| {
        let (tx, rx) = channel::<String>();
        std::thread::spawn(move || {
            for line in rx {
                if !send(&line) {
                    log!("duvar kâğıdı motoruna iletilemedi: {line}");
                }
            }
        });
        Mutex::new(tx)
    });
    let _ = q.lock().unwrap().send(line);
}

/// Renk seçenekleri: önce duvar kâğıdının varsayılanı, sonra ondan farklı hazır renkler.
fn colors(default: &str) -> Vec<String> {
    std::iter::once(default.to_string())
        .chain(SWATCHES.iter().filter(|h| !h.eq_ignore_ascii_case(default)).map(|h| h.to_string()))
        .collect()
}

fn hex_color(s: &str) -> Option<Color> {
    let v = u32::from_str_radix(s.trim().trim_start_matches('#'), 16).ok()?;
    Some(Color::rgb(v))
}

#[derive(Clone, Debug)]
enum Kind {
    Slider { min: f32, max: f32, value: f32, default: f32 },
    /// Seçenekler `english/türkçe` biçiminde ham gelir; çizerken seçili dildeki alınır.
    Choice { options: Vec<String>, value: usize, default: usize },
    Toggle { value: bool, default: bool },
    Color { value: String, default: String },
}

#[derive(Clone, Debug)]
struct Param {
    index: usize,
    /// [İngilizce, Türkçe]
    label: [String; 2],
    kind: Kind,
}

impl Param {
    /// Varsayılan değer, motora gönderilecek biçimde.
    fn default_value(&self) -> String {
        match &self.kind {
            Kind::Slider { default, .. } => format!("{default}"),
            Kind::Choice { default, .. } => format!("{default}"),
            Kind::Toggle { default, .. } => if *default { "1" } else { "0" }.into(),
            Kind::Color { default, .. } => default.clone(),
        }
    }

    fn is_default(&self) -> bool {
        match &self.kind {
            Kind::Slider { value, default, .. } => (value - default).abs() < 1e-3,
            Kind::Choice { value, default, .. } => value == default,
            Kind::Toggle { value, default } => value == default,
            Kind::Color { value, default } => value.eq_ignore_ascii_case(default),
        }
    }
}

#[derive(Clone, Default, Debug)]
struct State {
    status: String,
    paused: String,
    user_paused: bool,
    battery: bool,
    fps: u32,
    selected: String,
    error: String,
    /// (dosya, ad, Türkçe ad)
    walls: Vec<(String, String, String)>,
    params: Vec<Param>,
    /// Duraklama sebebi (dilden bağımsız ad) ve duvar kâğıdının kendi sınırıyla kare hızı.
    reason: String,
    fps_eff: u32,
    engine: bool,
}

/// Seçili dildeki ad: Türkçesi yoksa İngilizcesi.
fn pick<'a>(en: &'a str, tr: &'a str) -> &'a str {
    if ortak::i18n::turkish() && !tr.is_empty() { tr } else { en }
}

/// `english/türkçe` seçeneğinden seçili dildeki, ilk harfi büyük.
fn choice_label(raw: &str) -> String {
    let mut parts = raw.splitn(2, '/');
    let en = parts.next().unwrap_or("").trim();
    let tr = parts.next().unwrap_or("").trim();
    title_case(pick(en, tr))
}

fn choice_labels(options: &[String]) -> Vec<String> {
    options.iter().map(|o| choice_label(o)).collect()
}

/// İlk harfi büyük (Türkçe i → İ).
fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some('i') => format!("İ{}", c.as_str()),
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn parse_state(text: &str) -> State {
    let mut s = State { fps: 60, battery: true, ..Default::default() };
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k {
            "durum" => s.status = v.into(),
            "duraklama" => s.paused = v.into(),
            "kullanici_duraklatti" => s.user_paused = v == "1",
            "pilde" => s.battery = v == "1",
            "fps" => s.fps = v.parse().unwrap_or(60),
            "secili" => s.selected = v.into(),
            "hata" => s.error = v.into(),
            "neden" => s.reason = v.into(),
            "fps_etkin" => s.fps_eff = v.parse().unwrap_or(0),
            "motor" => s.engine = v == "1",
            "duvar" => {
                let mut f = v.splitn(3, '|');
                if let (Some(file), Some(name)) = (f.next(), f.next()) {
                    s.walls.push((file.into(), name.into(), f.next().unwrap_or("").into()));
                }
            }
            // param=sıra|ad|tür|min|max|değer|varsayılan|görünen ad|seçenekler (; ile)|Türkçe ad
            "param" => {
                let f: Vec<&str> = v.split('|').collect();
                if f.len() < 6 {
                    continue;
                }
                let num = |i: usize| f.get(i).and_then(|x| x.parse::<f32>().ok()).unwrap_or(0.0);
                let value = num(5);
                let default = f.get(6).map_or(value, |_| num(6));
                let kind = match f[2] {
                    "renk" => Kind::Color { value: f[5].into(), default: f.get(6).unwrap_or(&f[5]).to_string() },
                    "anahtar" => Kind::Toggle { value: value >= 0.5, default: default >= 0.5 },
                    "secim" => Kind::Choice {
                        options: f.get(8).unwrap_or(&"").split(';').map(String::from).collect(),
                        value: value.round().max(0.0) as usize,
                        default: default.round().max(0.0) as usize,
                    },
                    _ => Kind::Slider { min: num(3), max: num(4), value, default },
                };
                let label = f.get(7).filter(|l| !l.is_empty()).unwrap_or(&f[1]);
                let label_tr = f.get(9).copied().unwrap_or("");
                let label = [title_case(label), title_case(label_tr)];
                s.params.push(Param { index: f[0].parse().unwrap_or(0), label, kind });
            }
            _ => {}
        }
    }
    s
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Pause,
    Folder,
    Add,
    Wall(usize),
    /// Renk seçeneği: parametre ve sırası.
    Swatch(usize, usize),
    /// ‹ › : parametre ve gidilecek seçenek.
    Step(usize, usize),
    Form(form::Hit),
}

impl Hit {
    /// Basılan ile bırakılan aynı yer mi (formda kaydırıcının değeri ve seçeneğin sırası dışında).
    fn same(self, other: Hit) -> bool {
        match (self, other) {
            (Hit::Form(a), Hit::Form(b)) => a.same(b),
            _ => self == other,
        }
    }
}

pub enum Modal {
    AddFiles,
}

pub struct Wallpaper {
    hwnd: HWND,
    st: State,
    /// Motor açılıyor: ilk durum gelene kadar.
    starting: bool,
    /// Yüklü küçük resimler: dosya → (PNG yolu, resim). Yol dosyanın değişme zamanını taşır.
    thumbs: HashMap<String, (PathBuf, ImageId)>,
    /// Üretilen küçük resimler: (dosya, PNG yolu); WM_THUMB'da yüklenir.
    ready: Arc<Mutex<Vec<(String, PathBuf)>>>,
    generating: Arc<Mutex<bool>>,
    modal: Option<Modal>,
    scroll: f32,
    hover: Hit,
    pressed: Hit,
    /// Sürüklenen kaydırıcının satırı.
    drag: Option<u32>,
    /// Sürüklerken komutlar seyreltilir.
    last_send: Option<Instant>,
    w: f32,
    h: f32,
    head_x: f32,
    head_r: f32,
}

impl Wallpaper {
    /// Sayfayı kurar ve motoru başlatır: önceki motor kapatılır, exe güncel değilse yazılır, sonra WM_RELAUNCH ile `--hub` açılır.
    pub fn start(hwnd: HWND) -> Self {
        let st = parse_state(&std::fs::read_to_string(app_dir().join("durum.txt")).unwrap_or_default());
        let raw = hwnd.0 as usize;
        std::thread::spawn(move || {
            stop_engine();
            if let Err(e) = write_engine() {
                log!("{e}");
            }
            unsafe {
                let _ = PostMessageW(Some(HWND(raw as *mut _)), WM_RELAUNCH, WPARAM(0), LPARAM(0));
            }
        });
        Self {
            hwnd,
            st,
            starting: true,
            thumbs: HashMap::new(),
            ready: Arc::default(),
            generating: Arc::default(),
            modal: None,
            scroll: 0.0,
            hover: Hit::None,
            pressed: Hit::None,
            drag: None,
            last_send: None,
            w: 0.0,
            h: 0.0,
            head_x: 0.0,
            head_r: 0.0,
        }
    }

    /// WM_RELAUNCH: motoru başlatır.
    pub fn launch(&self) {
        let h = (self.hwnd.0 as isize).to_string();
        match quiet(Command::new(exe()).args(["--hub", &h])).spawn() {
            Ok(_) => log!("duvar kâğıdı motoru başlatıldı"),
            Err(e) => log!("duvar kâğıdı motoru başlatılamadı: {e}"),
        }
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn cmd(&self, line: &str) {
        send_async(line.to_string());
    }

    /// Motor durum yayımladı.
    pub fn state_changed(&mut self) {
        self.starting = false;
        self.st = parse_state(&std::fs::read_to_string(app_dir().join("durum.txt")).unwrap_or_default());
        self.make_thumbs();
        self.redraw();
    }

    // --- Küçük resimler ---

    fn thumb_path(file: &str) -> Option<PathBuf> {
        let mtime = std::fs::metadata(wallpapers_dir().join(file)).ok()?.modified().ok()?;
        let secs = mtime.duration_since(UNIX_EPOCH).ok()?.as_secs();
        Some(thumbs_dir().join(format!("{file}-{secs}.png")))
    }

    /// Eksik ya da eskimiş küçük resimleri sırayla üretir (her biri ayrı bir `--onizleme` süreci).
    fn make_thumbs(&mut self) {
        let missing: Vec<(String, PathBuf)> = self
            .st
            .walls
            .iter()
            .filter_map(|(f, _, _)| Self::thumb_path(f).map(|p| (f.clone(), p)))
            .filter(|(f, p)| self.thumbs.get(f).is_none_or(|(old, _)| old != p))
            .collect();
        if missing.is_empty() || std::mem::replace(&mut *self.generating.lock().unwrap(), true) {
            return;
        }
        let (ready, generating, hwnd) = (self.ready.clone(), self.generating.clone(), self.hwnd.0 as usize);
        std::thread::spawn(move || {
            let _ = std::fs::create_dir_all(thumbs_dir());
            for (file, out) in missing {
                if !out.is_file() {
                    let src = wallpapers_dir().join(&file);
                    let ok = quiet(Command::new(exe()).arg("--onizleme").arg(&src).arg(&out).args([
                        "--boyut", "384x216", "--zaman", "2",
                    ]))
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success());
                    if !ok {
                        log!("önizleme üretilemedi: {file}");
                        continue;
                    }
                }
                ready.lock().unwrap().push((file, out));
                unsafe {
                    let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_THUMB, WPARAM(0), LPARAM(0));
                }
            }
            *generating.lock().unwrap() = false;
        });
    }

    /// WM_THUMB: hazır küçük resimleri yükler (resim yüklemek için Gfx'e yazma izni gerekir).
    pub fn load_thumbs(&mut self, g: &mut Gfx) {
        let ready = std::mem::take(&mut *self.ready.lock().unwrap());
        for (file, path) in ready {
            let reuse = self.thumbs.get(&file).map(|&(_, id)| id);
            match g.load_image_file(&path, reuse) {
                Ok(id) => {
                    remove_old_thumbs(&file, &path);
                    self.thumbs.insert(file, (path, id));
                }
                Err(e) => log!("önizleme yüklenemedi: {e}"),
            }
        }
        self.redraw();
    }

    /// `.wallpaper` dosyalarını duvarlar klasörüne kopyalar, sonuncusunu uygular.
    pub fn add(&mut self, paths: Vec<PathBuf>) {
        let mut last = None;
        for p in paths.iter().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("cheshire"))) {
            let Some(name) = p.file_name() else { continue };
            let dst = wallpapers_dir().join(name);
            if std::fs::copy(p, &dst).is_ok() {
                last = Some(name.to_string_lossy().into_owned());
            }
        }
        if let Some(name) = last {
            self.cmd(&format!("duvar {name}"));
        }
    }

    // --- Yerleşim (sayfanın kendi köşesine göre; içerik `scroll` kadar yukarı kayık) ---

    fn pause_rect(&self) -> Rect {
        Rect::new(self.head_r - 112.0, HEAD_CY - 18.0, self.head_r - 76.0, HEAD_CY + 18.0)
    }

    fn folder_rect(&self) -> Rect {
        Rect::new(self.head_r - 74.0, HEAD_CY - 18.0, self.head_r - 38.0, HEAD_CY + 18.0)
    }

    fn add_rect(&self) -> Rect {
        Rect::new(self.head_r - 36.0, HEAD_CY - 18.0, self.head_r, HEAD_CY + 18.0)
    }

    /// Galeri ızgarası bu genişlikte: sütun sayısı, kutucuğun genişliği ve yüksekliği.
    fn grid(width: f32) -> (usize, f32, f32) {
        let cols = (((width + GAP) / (CARD_MIN + GAP)).floor() as usize).max(1);
        let cw = (width - (cols - 1) as f32 * GAP) / cols as f32;
        (cols, cw, cw * 9.0 / 16.0 + CAPTION)
    }

    /// Galerinin kutucukları, galeri alanının içinde.
    fn cards(&self, area: Rect) -> Vec<Rect> {
        let (cols, cw, ch) = Self::grid(area.w());
        (0..self.st.walls.len())
            .map(|i| {
                let l = area.l + (i % cols) as f32 * (cw + GAP);
                let t = area.t + (i / cols) as f32 * (ch + GAP);
                Rect::new(l, t, l + cw, t + ch)
            })
            .collect()
    }

    /// Renk seçeneklerinin kapladığı genişlik.
    fn swatches_w(count: usize) -> f32 {
        count.saturating_sub(1) as f32 * 30.0 + 22.0
    }

    /// Renk seçeneği, satırın kontrol alanının sağına yaslı.
    fn swatch(c: Rect, count: usize, j: usize) -> Rect {
        let x = c.r - 11.0 - (count - 1 - j) as f32 * 30.0;
        Rect::new(x - 11.0, c.cy() - 11.0, x + 11.0, c.cy() + 11.0)
    }

    /// Çok seçenekte (örneğin sloganlar) bölmeli seçici sığmaz: ‹ seçili › ile gezilir.
    fn stepper(c: Rect) -> (Rect, Rect, Rect) {
        let next = Rect::new(c.r - 29.0, c.cy() - 14.0, c.r, c.cy() + 14.0);
        let label = Rect::new(next.l - 190.0, next.t, next.l, next.b);
        let prev = Rect::new(label.l - 29.0, next.t, label.l, next.b);
        (prev, label, next)
    }

    fn selected_name(&self) -> Option<&str> {
        self.st.walls.iter().find(|w| w.0 == self.st.selected).map(|w| pick(&w.1, &w.2))
    }

    /// Sayfanın içeriği: galeri, seçili duvar kâğıdının ayarları, bütün duvar kâğıtları için olanlar.
    fn form(&self) -> Form {
        let st = &self.st;
        let mut items = vec![Item::Section(t!("Wallpapers", "Duvar kâğıtları").into())];
        if st.walls.is_empty() {
            let msg = t!("No wallpapers · drop a .cheshire file here", "Duvar kâğıdı yok · bir .cheshire dosyasını buraya sürükle");
            items.push(Item::Note(msg.into(), false));
        } else {
            let (cols, _, ch) = Self::grid(self.w - 2.0 * PAD);
            let rows = st.walls.len().div_ceil(cols);
            items.push(Item::Custom(GALLERY, rows as f32 * (ch + GAP) - GAP));
        }

        items.push(Item::Section(self.selected_name().unwrap_or(t!("Wallpaper", "Duvar kâğıdı")).into()));
        for (n, p) in st.params.iter().enumerate() {
            let ctl = match &p.kind {
                Kind::Slider { min, max, value, .. } => {
                    Ctl::Slider(if max > min { (value - min) / (max - min) } else { 0.0 }, format!("{value:.2}"))
                }
                Kind::Choice { options, .. } if options.len() > MAX_SEGMENTS => Ctl::Space(STEPPER_W),
                Kind::Choice { options, value, .. } => Ctl::Seg(choice_labels(options), *value),
                Kind::Toggle { value, .. } => Ctl::Toggle(*value),
                Kind::Color { default, .. } => Ctl::Space(Self::swatches_w(colors(default).len())),
            };
            items.push(Item::Row(Row::new(n as u32, pick(&p.label[0], &p.label[1]), "", ctl)));
        }
        if st.params.is_empty() {
            items.push(Item::Note(t!("This wallpaper has no settings.", "Bu duvar kâğıdının ayarı yok.").into(), false));
        } else {
            // Satır hep yerinde (altındakiler kaymasın); değişiklik yoksa düğmesi yok.
            let changed = !st.params.iter().all(Param::is_default);
            let ctl = match changed {
                true => Ctl::Button(t!("Reset to defaults", "Varsayılana dön").into()),
                false => Ctl::Text(t!("In use", "Kullanılıyor").into()),
            };
            items.push(Item::Row(Row::new(RESET, t!("Default settings", "Varsayılan ayarlar"), "", ctl).dim(!changed)));
        }

        items.push(Item::Section(t!("General", "Genel").into()));
        let fps = FPS.iter().map(|f| f.to_string()).collect();
        let sel = FPS.iter().position(|&f| f == st.fps).unwrap_or(usize::MAX);
        items.push(Item::Row(Row::new(FPS_ROW, t!("Frame rate", "Kare hızı"), "", Ctl::Seg(fps, sel))));
        items.push(Item::Row(Row::new(BATTERY, t!("Pause on battery", "Pildeyken duraklat"), "", Ctl::Toggle(st.battery))));
        if !st.error.is_empty() {
            items.push(Item::Note(st.error.clone(), true));
        }
        Form::new(items, PAD, self.w - PAD, HEADER + 16.0 - self.scroll)
    }

    fn max_scroll(&self) -> f32 {
        (self.form().bottom() - 24.0 + self.scroll + 12.0 - self.h).max(0.0)
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return match () {
                _ if self.pause_rect().contains(x, y) => Hit::Pause,
                _ if self.folder_rect().contains(x, y) => Hit::Folder,
                _ if self.add_rect().contains(x, y) => Hit::Add,
                _ => Hit::None,
            };
        }
        let f = self.form();
        match f.hit(g, x, y) {
            form::Hit::None => Hit::None,
            form::Hit::Custom(_) => {
                let cards = f.custom(GALLERY).map(|a| self.cards(a)).unwrap_or_default();
                cards.iter().position(|c| c.contains(x, y)).map_or(Hit::None, Hit::Wall)
            }
            form::Hit::Row(id) => self.part(&f, id as usize, x, y),
            h => Hit::Form(h),
        }
    }

    /// Kitin çizmediği kontrollerde (renkler, ‹ ›) imlecin altındaki.
    fn part(&self, f: &Form, n: usize, x: f32, y: f32) -> Hit {
        let (Some(c), Some(p)) = (f.control(n as u32), self.st.params.get(n)) else { return Hit::None };
        match &p.kind {
            Kind::Choice { options, value, .. } => {
                let (prev, _, next) = Self::stepper(c);
                let len = options.len();
                match () {
                    _ if prev.contains(x, y) => Hit::Step(n, (value + len - 1) % len),
                    _ if next.contains(x, y) => Hit::Step(n, (value + 1) % len),
                    _ => Hit::None,
                }
            }
            Kind::Color { default, .. } => {
                let count = colors(default).len();
                (0..count).find(|&j| Self::swatch(c, count, j).contains(x, y)).map_or(Hit::None, |j| Hit::Swatch(n, j))
            }
            _ => Hit::None,
        }
    }

    fn slide_to(&mut self, id: u32, x: f32, force: bool) {
        let Some(t) = self.form().slider_value(id, x) else { return };
        let Some(p) = self.st.params.get_mut(id as usize) else { return };
        let Kind::Slider { min, max, value, .. } = &mut p.kind else { return };
        *value = *min + (*max - *min) * t;
        let (index, v) = (p.index, *value);
        self.redraw();
        // Sürüklerken saniyede en çok 25 komut; bırakınca son değer kesin gider.
        if force || self.last_send.is_none_or(|t| t.elapsed().as_millis() >= 40) {
            self.last_send = Some(Instant::now());
            self.cmd(&format!("param {index} {v:.4}"));
        }
    }

    fn choose(&mut self, n: usize, j: usize) {
        if let Some(p) = self.st.params.get_mut(n)
            && let Kind::Choice { value, .. } = &mut p.kind
        {
            *value = j;
            let line = format!("param {} {j}", p.index);
            self.cmd(&line);
            self.redraw();
        }
    }

    // --- Çizim ---

    /// Başlıktaki durum, seçili dilde (motor yalnızca ham bilgiyi bildirir).
    fn status_text(&self) -> String {
        let st = &self.st;
        if self.starting {
            return t!("starting…", "açılıyor…").into();
        }
        if !st.reason.is_empty() {
            let why = match st.reason.as_str() {
                "user" => t!("by you", "elle"),
                "locked" => t!("screen locked", "ekran kilitli"),
                "display_off" => t!("screen off", "ekran kapalı"),
                "fullscreen" => t!("desktop hidden", "masaüstü görünmüyor"),
                _ => t!("on battery", "pilde"),
            };
            return format!("{} ({why})", t!("paused", "duraklatıldı"));
        }
        if st.status.starts_with("derleniyor") {
            return t!("compiling…", "derleniyor…").into();
        }
        if !st.engine {
            return t!("no working wallpaper", "geçerli duvar kâğıdı yok").into();
        }
        format!("{} · {} FPS", self.selected_name().unwrap_or(""), st.fps_eff)
    }

    fn paint_gallery(&self, g: &Gfx, area: Rect) {
        let st = &self.st;
        for (i, ((file, en, tr), c)) in st.walls.iter().zip(self.cards(area)).enumerate() {
            if c.b < HEADER || c.t > self.h {
                continue;
            }
            let selected = *file == st.selected;
            let hovered = self.hover == Hit::Wall(i);
            let img = Rect::new(c.l, c.t, c.r, c.b - CAPTION);
            g.fill(img, 8.0, pal().hover);
            match self.thumbs.get(file).map(|&(_, id)| id) {
                Some(id) => g.clip(img, || g.image(id, img, if hovered || selected { 1.0 } else { 0.8 })),
                None => g.text(t!("preparing preview…", "önizleme hazırlanıyor…"), &g.f.small_center, img, pal().faint),
            }
            if selected {
                g.stroke(Rect::new(img.l - 3.0, img.t - 3.0, img.r + 3.0, img.b + 3.0), 10.0, accent(), 2.0);
            } else if hovered {
                g.stroke(img, 8.0, pal().line, 1.0);
            }
            let lc = if selected { accent() } else if hovered { pal().text } else { pal().muted };
            g.text(pick(en, tr), &g.f.text, Rect::new(c.l + 2.0, c.b - 30.0, c.r, c.b - 4.0), lc);
        }
    }

    /// Kitin yer ayırdığı kontroller: ‹ seçili › ve renkler.
    fn paint_parts(&self, g: &Gfx, f: &Form) {
        for (n, p) in self.st.params.iter().enumerate() {
            let Some(c) = f.control(n as u32) else { continue };
            match &p.kind {
                Kind::Choice { options, value, .. } => {
                    let (prev, label, next) = Self::stepper(c);
                    let len = options.len();
                    g.fill(Rect::new(prev.l - 3.0, prev.t - 3.0, next.r + 3.0, next.b + 3.0), 8.0, pal().hover);
                    icon_button(g, prev, "\u{E76B}", pal().muted, self.hover == Hit::Step(n, (value + len - 1) % len));
                    icon_button(g, next, "\u{E76C}", pal().muted, self.hover == Hit::Step(n, (value + 1) % len));
                    let current = options.get(*value).map(|o| choice_label(o)).unwrap_or_default();
                    g.text(&current, &g.f.button, label, pal().text);
                }
                Kind::Color { value, default } => {
                    let list = colors(default);
                    for (j, hex) in list.iter().enumerate() {
                        let s = Self::swatch(c, list.len(), j);
                        g.circle((s.l + s.r) / 2.0, s.cy(), 8.0, hex_color(hex).unwrap_or(pal().text));
                        if hex.eq_ignore_ascii_case(value) {
                            g.stroke(s, 11.0, pal().text, 2.0);
                        } else if self.hover == Hit::Swatch(n, j) {
                            g.stroke(s, 11.0, pal().muted, 1.0);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub fn paint(&self, g: &Gfx) {
        // Başlık: yalnızca durum ve düğmeler.
        let dot = if self.st.reason.is_empty() { pal().green } else { pal().muted };
        status(g, self.head_x + 16.0, HEAD_CY, dot, &self.status_text(), pal().muted, self.pause_rect().l - 8.0);
        let pause_icon = if self.st.user_paused { ICON_PLAY } else { ICON_PAUSE };
        icon_button(g, self.pause_rect(), pause_icon, pal().muted, self.hover == Hit::Pause);
        icon_button(g, self.folder_rect(), ICON_FOLDER, pal().muted, self.hover == Hit::Folder);
        icon_button(g, self.add_rect(), ICON_ADD, pal().muted, self.hover == Hit::Add);

        let f = self.form();
        let hover = if let Hit::Form(h) = self.hover { h } else { form::Hit::None };
        g.clip(Rect::new(0.0, HEADER, self.w, self.h), || {
            f.paint(g, hover, self.drag);
            if let Some(area) = f.custom(GALLERY) {
                self.paint_gallery(g, area);
            }
            self.paint_parts(g, &f);
        });
    }
}

impl ToolPage for Wallpaper {
    fn summary(&self) -> Option<(String, String)> {
        let name = self.selected_name().unwrap_or("").to_string();
        let status = self.status_text();
        let detail = status.strip_prefix(&name).map_or(status.clone(), |r| r.trim_start_matches(" · ").to_string());
        Some(if name.is_empty() { (status, String::new()) } else { (name, detail) })
    }

    fn message(
        &mut self,
        g: &mut ortak::gfx::Gfx,
        msg: u32,
        wp: windows::Win32::Foundation::WPARAM,
        _lp: windows::Win32::Foundation::LPARAM,
    ) -> Option<windows::Win32::Foundation::LRESULT> {
                let _ = wp;
        match msg {
            WM_STATE => self.state_changed(),
            WM_THUMB => self.load_thumbs(g),
            WM_RELAUNCH => self.launch(),
            _ => return None,
        }
        Some(windows::Win32::Foundation::LRESULT(0))
    }
    fn add_files(&mut self, paths: Vec<PathBuf>) {
        self.add(paths)
    }
    fn take_file_dialog(&mut self) -> Option<ortak::ui::FileDialog> {
        self.modal.take().map(|Modal::AddFiles| file_dialog as ortak::ui::FileDialog)
    }
    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
    }

    fn paint(&self, g: &Gfx) {
        Wallpaper::paint(self, g)
    }

    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        if let Some(id) = self.drag {
            self.slide_to(id, x, false);
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
        if let Hit::Form(form::Hit::Slider(id, _)) = self.pressed {
            self.drag = Some(id);
            self.slide_to(id, x, false);
        }
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        if let Some(id) = self.drag.take() {
            self.slide_to(id, x, true);
            self.pressed = Hit::None;
            return;
        }
        let hit = self.hit(g, x, y);
        if !hit.same(std::mem::replace(&mut self.pressed, Hit::None)) {
            return;
        }
        match hit {
            Hit::Pause => self.cmd(if self.st.user_paused { "duraklat 0" } else { "duraklat 1" }),
            Hit::Folder => self.cmd("klasor"),
            Hit::Add => self.modal = Some(Modal::AddFiles),
            Hit::Wall(i) => {
                if let Some((file, _, _)) = self.st.walls.get(i) {
                    self.st.selected = file.clone();
                    self.cmd(&format!("duvar {file}"));
                    self.redraw();
                }
            }
            Hit::Form(form::Hit::Button(RESET)) => {
                for p in &self.st.params {
                    self.cmd(&format!("param {} {}", p.index, p.default_value()));
                }
            }
            Hit::Form(form::Hit::Seg(FPS_ROW, k)) => self.cmd(&format!("fps {}", FPS[k])),
            Hit::Form(form::Hit::Toggle(BATTERY)) => self.cmd(if self.st.battery { "pilde 0" } else { "pilde 1" }),
            Hit::Form(form::Hit::Seg(n, j)) => self.choose(n as usize, j),
            Hit::Step(n, j) => self.choose(n, j),
            Hit::Form(form::Hit::Toggle(n)) => {
                if let Some(p) = self.st.params.get_mut(n as usize)
                    && let Kind::Toggle { value, .. } = &mut p.kind
                {
                    *value = !*value;
                    let line = format!("param {} {}", p.index, if *value { 1 } else { 0 });
                    self.cmd(&line);
                    self.redraw();
                }
            }
            Hit::Swatch(n, j) => {
                if let Some(p) = self.st.params.get(n)
                    && let Kind::Color { default, .. } = &p.kind
                    && let Some(hex) = colors(default).get(j)
                {
                    self.cmd(&format!("param {} {hex}", p.index));
                }
            }
            _ => {}
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
        let (r, t) = match self.hover {
            Hit::Pause => (self.pause_rect(), if self.st.user_paused { t!("Resume", "Devam et") } else { t!("Pause", "Duraklat") }),
            Hit::Folder => (self.folder_rect(), t!("Wallpaper folder", "Duvar kâğıdı klasörü")),
            Hit::Add => (self.add_rect(), t!("Add wallpapers", "Duvar kâğıdı ekle")),
            _ => return None,
        };
        Some((r, t.into()))
    }

    fn set_visible(&mut self, visible: bool) {
        if visible {
            self.make_thumbs();
        } else {
            self.hover = Hit::None;
        }
    }
}

impl Drop for Wallpaper {
    /// winloom kapanırken ya da araç kaldırılırken motor da kapanır (duvar kâğıdı Windows'unkine döner).
    fn drop(&mut self) {
        send("cik");
    }
}

fn add_dialog(hwnd: HWND) -> Res<Vec<PathBuf>> {
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dlg.SetOptions(dlg.GetOptions()? | FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)?;
        dlg.SetTitle(t!(w!("Add wallpapers"), w!("Duvar kâğıdı ekle")))?;
        dlg.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: t!(w!("winloom wallpaper"), w!("winloom duvar kâğıdı")), pszSpec: w!("*.cheshire") }])?;
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

/// "Dosya ekle" penceresi.
fn file_dialog(hwnd: HWND) -> Result<Vec<PathBuf>, String> {
    add_dialog(hwnd).map_err(|e| e.to_string())
}
