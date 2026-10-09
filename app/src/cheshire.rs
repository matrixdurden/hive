//! cheshire: GPU ile çizilen canlı duvar kâğıdı. Motor ayrı bir süreç (`cheshire.exe --hub`):
//! GPU tarafındaki bir çökme hive'ı götürmesin. Komutlar `WM_COPYDATA` ile gider, motor
//! durumunu `durum.txt`'ye yazıp WM_STATE ile haber verir (bkz. cheshire/src/hub.rs).
//!
//! Sayfa: üstte durum ve düğmeler, ortada küçük resimli galeri, altta seçili duvar kâğıdının
//! ayarları. Küçük resimleri motorun `--onizleme` komutu üretir, önbellekte tutulur.

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
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteKeyValueW, RegDeleteTreeW};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::gfx::{Color, Gfx, ImageId, Rect};
use crate::log;
use crate::ui::*;
use crate::util::{self, Res};


/// cheshire/src/hub.rs ile aynı olmalı.
pub const WM_STATE: u32 = WM_APP + 40;
const COPYDATA_COMMAND: usize = 0xD0FB;
/// Bir küçük resim hazır.
pub const WM_THUMB: u32 = WM_APP + 41;
/// Motor başlatılmaya hazır (eskisi kapandı, exe güncel).
pub const WM_RELAUNCH: u32 = WM_APP + 42;

/// Motor hive'ın içinde gelir: kurulum ve güncelleme bu kopyayı yazmaktır, internet gerekmez.
static ENGINE: &[u8] = include_bytes!(env!("CHESHIRE_EXE"));

const SWATCHES: [&str; 6] = ["#4fc3f7", "#ff6ec7", "#b388ff", "#69f0ae", "#ffd740", "#ffffff"];
const FPS: [u32; 4] = [30, 60, 120, 144];
const CARD_MIN: f32 = 160.0;
const GAP: f32 = 16.0;
const ROW: f32 = 52.0;
/// Panel başlığının yüksekliği ve satır etiketlerinin genişliği.
const PANEL_HEAD: f32 = 56.0;
const LABEL_W: f32 = 180.0;
/// Panellerin zemini: sayfadan bir ton açık.
/// Bu kadar seçeneğe kadar bölmeli seçici; fazlası ‹ › ile.
const MAX_SEGMENTS: usize = 5;

struct Layout {
    cards: Vec<Rect>,
    panel1: Rect,
    reset: Option<Rect>,
    /// (parametre sırası, satır)
    rows: Vec<(usize, Rect)>,
    panel2: Rect,
    fps_row: Rect,
    battery_row: Rect,
    /// İçeriğin bittiği y (kaydırma sınırı için).
    bottom: f32,
}

/// %LOCALAPPDATA%\Programs\cheshire — motorun exe'si, ayarları ve duvar kâğıtları.
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
                    log!("cheshire motoru kapanmadı, sonlandırılıyor");
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

/// Exe hive'daki sürümle aynı değilse yazar (çalışan motor kapalı olmalı).
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

/// Ayrı cheshire'ın kendi kayıtları (Windows ile başlama, Programlar listesi): artık hive yönetiyor.
fn remove_standalone_registrations() {
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Run"), w!("cheshire"));
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\cheshire"));
    }
}

/// Eski sürümün klasörü (%APPDATA%\cheshire), taşıma sonrası kalmış olabilir.
fn legacy_dir() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default().join("cheshire")
}

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\cheshire";
const CLASSES: [&str; 2] = [r"Software\Classes\.cheshire", r"Software\Classes\cheshire.dosya"];

/// Kurulumdan geriye kalan her şey; boşsa iz yok.
pub fn leftovers() -> Vec<String> {
    let mut left: Vec<String> =
        [app_dir(), thumbs_dir(), legacy_dir()].iter().filter(|p| p.exists()).map(|p| p.display().to_string()).collect();
    if util::reg_value_exists(HKEY_CURRENT_USER, RUN, "cheshire") {
        left.push(format!(r"HKCU\{RUN}\cheshire"));
    }
    for key in std::iter::once(UNINSTALL).chain(CLASSES) {
        if util::reg_key_exists(HKEY_CURRENT_USER, key) {
            left.push(format!(r"HKCU\{key}"));
        }
    }
    if engine_window().is_some() {
        left.push(t!("running wallpaper engine", "çalışan duvar kâğıdı motoru").into());
    }
    left
}

/// Kur: motoru yazar. Örnek duvar kâğıtlarını motor ilk açılışta kendisi koyar.
pub fn install() -> Result<(), String> {
    write_engine()?;
    remove_standalone_registrations();
    Ok(())
}

/// Kaldır: motoru durdurur; klasörü (duvar kâğıtları dahil), önizlemeleri ve kayıtları siler.
pub fn uninstall() -> Result<(), String> {
    stop_engine();
    remove_standalone_registrations();
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, w!(r"Software\Classes\.cheshire"));
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, w!(r"Software\Classes\cheshire.dosya"));
    }
    let _ = std::fs::remove_dir_all(thumbs_dir());
    let _ = std::fs::remove_dir_all(legacy_dir());
    // Explorer dosya simgelerini ve "birlikte aç" listesini unutsun.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    std::fs::remove_dir_all(app_dir()).map_err(|e| format!("{} {}: {e}", t!("could not delete", "silinemedi:"), app_dir().display()))
}


fn send(line: &str) -> bool {
    let Some(hwnd) = engine_window() else { return false };
    let data = line.as_bytes();
    let cds = COPYDATASTRUCT { dwData: COPYDATA_COMMAND, cbData: data.len() as u32, lpData: data.as_ptr() as *mut _ };
    unsafe { SendMessageW(hwnd, WM_COPYDATA, None, Some(LPARAM(&cds as *const _ as isize))).0 != 0 }
}

/// `WM_COPYDATA` gönderilmek zorunda (veri çağrı süresince yaşamalı) ve gönderen, motor mesajı
/// işleyene kadar bekler. Motor o sırada shader derliyorsa bu saniyeler sürer: hive'ın arayüzü
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
                    log!("cheshire'a iletilemedi: {line}");
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
    if crate::i18n::turkish() && !tr.is_empty() { tr } else { en }
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
    Reset,
    Slider(usize),
    Choice(usize, usize),
    Toggle(usize),
    Swatch(usize, usize),
    Fps(usize),
    Battery,
}

pub enum Modal {
    AddFiles,
}

pub struct Cheshire {
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
    drag: Option<usize>,
    /// Sürüklerken komutlar seyreltilir.
    last_send: Option<Instant>,
    w: f32,
    h: f32,
    head_x: f32,
    head_r: f32,
}

impl Cheshire {
    /// Sayfayı kurar ve motoru başlatır: önceki motor (eski bir hive'dan ya da tepsideki ayrı
    /// cheshire'dan) kapatılır, exe güncel değilse yazılır, sonra WM_RELAUNCH ile `--hub` açılır.
    pub fn start(hwnd: HWND) -> Self {
        let st = parse_state(&std::fs::read_to_string(app_dir().join("durum.txt")).unwrap_or_default());
        let raw = hwnd.0 as usize;
        std::thread::spawn(move || {
            stop_engine();
            remove_standalone_registrations();
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
            Ok(_) => log!("cheshire motoru başlatıldı"),
            Err(e) => log!("cheshire başlatılamadı: {e}"),
        }
    }

    pub fn take_modal(&mut self) -> Option<Modal> {
        self.modal.take()
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

    /// `.cheshire` dosyalarını duvarlar klasörüne kopyalar, sonuncusunu uygular.
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

    fn layout_all(&self, g: &Gfx) -> Layout {
        let w = self.w;
        let avail = w - 2.0 * PAD;
        let cols = (((avail + GAP) / (CARD_MIN + GAP)).floor() as usize).max(1);
        let cw = (avail - (cols - 1) as f32 * GAP) / cols as f32;
        let ch = cw * 9.0 / 16.0 + 34.0;
        let top = HEADER + 16.0 - self.scroll;
        let gallery_t = top + 32.0;
        let cards: Vec<Rect> = (0..self.st.walls.len())
            .map(|i| {
                let (r, c) = (i / cols, i % cols);
                let l = PAD + c as f32 * (cw + GAP);
                let t = gallery_t + r as f32 * (ch + GAP);
                Rect::new(l, t, l + cw, t + ch)
            })
            .collect();
        let rows_n = self.st.walls.len().div_ceil(cols).max(1);
        let mut y = gallery_t + rows_n as f32 * (ch + GAP) + 12.0;

        // Seçili duvar kâğıdının paneli.
        let p1_t = y;
        y += PANEL_HEAD;
        let inner = |t: f32| Rect::new(PAD + 20.0, t, w - PAD - 20.0, t + ROW);
        let mut rows = Vec::new();
        for n in 0..self.st.params.len() {
            rows.push((n, inner(y)));
            y += ROW;
        }
        if self.st.params.is_empty() {
            y += ROW;
        }
        let panel1 = Rect::new(PAD, p1_t, w - PAD, y + 8.0);
        let reset = (!self.st.params.iter().all(Param::is_default))
            .then(|| button_rect_right(g, panel1.r - 16.0, p1_t + 12.0, t!("Reset to defaults", "Varsayılana dön"), false));

        // Motor paneli.
        y = panel1.b + GAP;
        let p2_t = y;
        y += PANEL_HEAD;
        let fps_row = inner(y);
        let battery_row = inner(y + ROW);
        let panel2 = Rect::new(PAD, p2_t, w - PAD, battery_row.b + 8.0);
        Layout { cards, panel1, reset, rows, panel2, fps_row, battery_row, bottom: panel2.b + 40.0 }
    }

    /// Satırın sağına yaslı bölmeli seçici.
    /// Çok seçenekte (örneğin sloganlar) bölmeli seçici sığmaz: ‹ seçili › ile gezilir.
    fn stepper(r: Rect) -> (Rect, Rect, Rect) {
        let next = Rect::new(r.r - 32.0, r.cy() - 14.0, r.r - 3.0, r.cy() + 14.0);
        let label = Rect::new(next.l - 190.0, next.t, next.l, next.b);
        let prev = Rect::new(label.l - 29.0, next.t, label.l, next.b);
        (prev, label, next)
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

    fn slider_track(r: Rect) -> (f32, f32) {
        (r.l + LABEL_W, r.r - 52.0)
    }

    fn swatch(r: Rect, count: usize, j: usize) -> Rect {
        let x = r.r - 11.0 - (count - 1 - j) as f32 * 30.0;
        Rect::new(x - 11.0, r.cy() - 11.0, x + 11.0, r.cy() + 11.0)
    }

    fn fps_labels() -> Vec<String> {
        FPS.iter().map(|f| f.to_string()).collect()
    }

    fn max_scroll(&self, g: &Gfx) -> f32 {
        let l = self.layout_all(g);
        (l.bottom + self.scroll - self.h).max(0.0)
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
        let l = self.layout_all(g);
        if let Some(i) = l.cards.iter().position(|c| c.contains(x, y)) {
            return Hit::Wall(i);
        }
        if l.reset.is_some_and(|r| r.contains(x, y)) {
            return Hit::Reset;
        }
        for &(n, r) in &l.rows {
            if !r.contains(x, y) {
                continue;
            }
            return match &self.st.params[n].kind {
                Kind::Slider { .. } => {
                    let (a, b) = Self::slider_track(r);
                    if x >= a - 10.0 && x <= b + 10.0 { Hit::Slider(n) } else { Hit::None }
                }
                Kind::Choice { options, value, .. } if options.len() > MAX_SEGMENTS => {
                    let (prev, _, next) = Self::stepper(r);
                    let len = options.len();
                    match () {
                        _ if prev.contains(x, y) => Hit::Choice(n, (value + len - 1) % len),
                        _ if next.contains(x, y) => Hit::Choice(n, (value + 1) % len),
                        _ => Hit::None,
                    }
                }
                Kind::Choice { options, .. } => Self::segments(g, r, &choice_labels(options))
                    .iter()
                    .position(|s| s.contains(x, y))
                    .map_or(Hit::None, |j| Hit::Choice(n, j)),
                Kind::Toggle { .. } => Hit::Toggle(n),
                Kind::Color { default, .. } => {
                    let count = colors(default).len();
                    (0..count).find(|&j| Self::swatch(r, count, j).contains(x, y)).map_or(Hit::None, |j| Hit::Swatch(n, j))
                }
            };
        }
        if let Some(k) = Self::segments(g, l.fps_row, &Self::fps_labels()).iter().position(|s| s.contains(x, y)) {
            return Hit::Fps(k);
        }
        if l.battery_row.contains(x, y) {
            return Hit::Battery;
        }
        Hit::None
    }

    fn slide_to(&mut self, g: &Gfx, n: usize, x: f32, force: bool) {
        let Some(&(_, r)) = self.layout_all(g).rows.iter().find(|(m, _)| *m == n) else { return };
        let (a, b) = Self::slider_track(r);
        let Some(p) = self.st.params.get_mut(n) else { return };
        let Kind::Slider { min, max, value, .. } = &mut p.kind else { return };
        let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
        *value = *min + (*max - *min) * t;
        let (index, v) = (p.index, *value);
        self.redraw();
        // Sürüklerken saniyede en çok 25 komut; bırakınca son değer kesin gider.
        if force || self.last_send.is_none_or(|t| t.elapsed().as_millis() >= 40) {
            self.last_send = Some(Instant::now());
            self.cmd(&format!("param {index} {v:.4}"));
        }
    }

    // --- Çizim ---

    fn paint_segments(&self, g: &Gfx, r: Rect, labels: &[String], selected: usize, hover: impl Fn(usize) -> bool) {
        let segs = Self::segments(g, r, labels);
        if let (Some(first), Some(last)) = (segs.first(), segs.last()) {
            g.fill(Rect::new(first.l - 3.0, first.t - 3.0, last.r + 3.0, last.b + 3.0), 8.0, pal().hover);
        }
        for (j, (s, label)) in segs.iter().zip(labels).enumerate() {
            let c = if j == selected {
                g.fill(*s, 6.0, accent());
                on_accent()
            } else if hover(j) {
                g.fill(*s, 6.0, pal().sel);
                pal().text
            } else {
                pal().muted
            };
            g.text(label, &g.f.button, *s, c);
        }
    }

    fn panel(g: &Gfx, r: Rect, title: &str, sub: &str) {
        g.fill(r, 12.0, pal().panel);
        g.stroke(r, 12.0, pal().line, 1.0);
        let t = Rect::new(r.l + 20.0, r.t + 10.0, r.r - 180.0, r.t + 34.0);
        g.text(title, &g.f.strong, t, pal().text);
        g.text(sub, &g.f.small, Rect::new(t.l, t.b - 2.0, t.r, t.b + 16.0), pal().muted);
    }

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
        let name = st.walls.iter().find(|w| w.0 == st.selected).map_or("", |w| pick(&w.1, &w.2));
        format!("{name} · {} FPS", st.fps_eff)
    }

    pub fn paint(&self, g: &Gfx) {
        let w = self.w;
        let st = &self.st;
        let paused = !st.reason.is_empty();

        // Başlık: durum ve düğmeler.
        let status_text = self.status_text();
        let dot = if paused { pal().muted } else { pal().green };
        status(g, self.head_x + 16.0, HEAD_CY, dot, &status_text, pal().muted, self.pause_rect().l - 8.0);
        let pause_icon = if st.user_paused { ICON_PLAY } else { ICON_PAUSE };
        icon_button(g, self.pause_rect(), pause_icon, pal().muted, self.hover == Hit::Pause);
        icon_button(g, self.folder_rect(), ICON_FOLDER, pal().muted, self.hover == Hit::Folder);
        icon_button(g, self.add_rect(), ICON_ADD, pal().muted, self.hover == Hit::Add);

        let l = self.layout_all(g);
        g.clip(Rect::new(0.0, HEADER, w, self.h), || {
            let top = HEADER + 16.0 - self.scroll;
            g.text(t!("Wallpapers", "Duvar kâğıtları"), &g.f.strong, Rect::new(PAD, top, w - PAD, top + 24.0), pal().text);

            // Galeri.
            for (i, ((file, en, tr), c)) in st.walls.iter().zip(&l.cards).enumerate() {
                let name = pick(en, tr);
                if c.b < HEADER || c.t > self.h {
                    continue;
                }
                let selected = *file == st.selected;
                let hovered = self.hover == Hit::Wall(i);
                let img = Rect::new(c.l, c.t, c.r, c.b - 34.0);
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
                g.text(name, &g.f.text, Rect::new(c.l + 2.0, c.b - 30.0, c.r, c.b - 4.0), lc);
            }
            if st.walls.is_empty() {
                let t = top + 32.0;
                let msg = t!("No wallpapers · drop a .cheshire file here", "Duvar kâğıdı yok · bir .cheshire dosyasını buraya sürükle");
                g.text(msg, &g.f.small, Rect::new(PAD, t, w - PAD, t + 24.0), pal().faint);
            }

            // Seçili duvar kâğıdının paneli.
            let name = st.walls.iter().find(|w| w.0 == st.selected).map_or(t!("Wallpaper", "Duvar kâğıdı"), |w| pick(&w.1, &w.2));
            Self::panel(g, l.panel1, name, t!("Settings for this wallpaper", "Bu duvar kâğıdının ayarları"));
            if let Some(r) = l.reset {
                button(g, r, t!("Reset to defaults", "Varsayılana dön"), None, None, self.hover == Hit::Reset);
            }
            if st.params.is_empty() {
                let t = l.panel1.t + PANEL_HEAD;
                let msg = t!("This wallpaper has no settings.", "Bu duvar kâğıdının ayarı yok.");
                g.text(msg, &g.f.small, Rect::new(PAD + 20.0, t, w - PAD - 20.0, t + ROW), pal().faint);
            }
            for (k, &(n, r)) in l.rows.iter().enumerate() {
                let p = &st.params[n];
                if k > 0 {
                    g.fill(Rect::new(r.l, r.t, r.r, r.t + 1.0), 0.0, pal().hover);
                }
                g.text(pick(&p.label[0], &p.label[1]), &g.f.text, Rect::new(r.l, r.t, r.l + LABEL_W - 16.0, r.b), pal().text);
                match &p.kind {
                    Kind::Slider { min, max, value, .. } => {
                        let (a, b) = Self::slider_track(r);
                        let active = self.drag == Some(n) || self.hover == Hit::Slider(n);
                        let v = if max > min { (value - min) / (max - min) } else { 0.0 };
                        slider(g, a, b, r.cy(), v, accent(), active);
                        g.text(&format!("{value:.2}"), &g.f.small_right, Rect::new(b + 8.0, r.t, r.r, r.b), pal().muted);
                    }
                    Kind::Choice { options, value, .. } if options.len() > MAX_SEGMENTS => {
                        let (prev, label, next) = Self::stepper(r);
                        let len = options.len();
                        g.fill(Rect::new(prev.l - 3.0, prev.t - 3.0, next.r + 3.0, next.b + 3.0), 8.0, pal().hover);
                        let hp = self.hover == Hit::Choice(n, (value + len - 1) % len);
                        let hn = self.hover == Hit::Choice(n, (value + 1) % len);
                        icon_button(g, prev, "\u{E76B}", pal().muted, hp);
                        icon_button(g, next, "\u{E76C}", pal().muted, hn);
                        let current = options.get(*value).map(|o| choice_label(o)).unwrap_or_default();
                        g.text(&current, &g.f.button, label, pal().text);
                    }
                    Kind::Choice { options, value, .. } => {
                        self.paint_segments(g, r, &choice_labels(options), *value, |j| self.hover == Hit::Choice(n, j));
                    }
                    Kind::Toggle { value, .. } => toggle(g, toggle_rect(r.r, r.cy()), *value, accent(), true),
                    Kind::Color { value, default } => {
                        let list = colors(default);
                        for (j, hex) in list.iter().enumerate() {
                            let s = Self::swatch(r, list.len(), j);
                            g.circle((s.l + s.r) / 2.0, s.cy(), 8.0, hex_color(hex).unwrap_or(pal().text));
                            if hex.eq_ignore_ascii_case(value) {
                                g.stroke(s, 11.0, pal().text, 2.0);
                            } else if self.hover == Hit::Swatch(n, j) {
                                g.stroke(s, 11.0, pal().muted, 1.0);
                            }
                        }
                    }
                }
            }

            // Motor paneli.
            Self::panel(g, l.panel2, t!("Engine", "Motor"), t!("For every wallpaper", "Bütün duvar kâğıtları için"));
            let r = l.fps_row;
            g.text(t!("Frame rate", "Kare hızı"), &g.f.text, Rect::new(r.l, r.t, r.l + LABEL_W, r.b), pal().text);
            let sel = FPS.iter().position(|&f| f == st.fps).unwrap_or(usize::MAX);
            self.paint_segments(g, r, &Self::fps_labels(), sel, |k| self.hover == Hit::Fps(k));
            let r = l.battery_row;
            g.fill(Rect::new(r.l, r.t, r.r, r.t + 1.0), 0.0, pal().hover);
            g.text(t!("Pause on battery", "Pildeyken duraklat"), &g.f.text, Rect::new(r.l, r.t, r.l + LABEL_W, r.b), pal().text);
            toggle(g, toggle_rect(r.r, r.cy()), st.battery, accent(), true);

            if !st.error.is_empty() {
                let t = l.panel2.b + 12.0;
                g.text(&st.error, &g.f.small, Rect::new(PAD, t, w - PAD, t + 22.0), pal().red);
            }
        });
    }
}

impl ToolPage for Cheshire {
    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
    }

    fn paint(&self, g: &Gfx) {
        Cheshire::paint(self, g)
    }

    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        if let Some(n) = self.drag {
            self.slide_to(g, n, x, false);
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
        if let Hit::Slider(n) = self.pressed {
            self.drag = Some(n);
            self.slide_to(g, n, x, false);
        }
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        if let Some(n) = self.drag.take() {
            self.slide_to(g, n, x, true);
            self.pressed = Hit::None;
            return;
        }
        let hit = self.hit(g, x, y);
        if hit != std::mem::replace(&mut self.pressed, Hit::None) {
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
            Hit::Reset => {
                for p in &self.st.params {
                    self.cmd(&format!("param {} {}", p.index, p.default_value()));
                }
            }
            Hit::Choice(n, j) => {
                if let Some(p) = self.st.params.get_mut(n)
                    && let Kind::Choice { value, .. } = &mut p.kind
                {
                    *value = j;
                    let line = format!("param {} {j}", p.index);
                    self.cmd(&line);
                    self.redraw();
                }
            }
            Hit::Toggle(n) => {
                if let Some(p) = self.st.params.get_mut(n)
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
            Hit::Fps(k) => self.cmd(&format!("fps {}", FPS[k])),
            Hit::Battery => self.cmd(if self.st.battery { "pilde 0" } else { "pilde 1" }),
            Hit::Slider(_) | Hit::None => {}
        }
    }

    fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        self.scroll = (self.scroll - delta * 60.0).clamp(0.0, self.max_scroll(g));
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

impl Drop for Cheshire {
    /// hive kapanırken ya da araç kaldırılırken motor da kapanır (duvar kâğıdı Windows'unkine döner).
    fn drop(&mut self) {
        send("cik");
    }
}

fn add_dialog(hwnd: HWND) -> Res<Vec<PathBuf>> {
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dlg.SetOptions(dlg.GetOptions()? | FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)?;
        dlg.SetTitle(t!(w!("Add wallpapers"), w!("Duvar kâğıdı ekle")))?;
        dlg.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: t!(w!("cheshire wallpaper"), w!("cheshire duvar kâğıdı")), pszSpec: w!("*.cheshire") }])?;
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

pub fn run_modal(hwnd: HWND, modal: Modal, apply: impl FnOnce(Box<dyn FnOnce(&mut Cheshire)>)) {
    match modal {
        Modal::AddFiles => match add_dialog(hwnd) {
            Ok(paths) => apply(Box::new(move |c| c.add(paths))),
            Err(e) => log!("dosya penceresi: {e}"),
        },
    }
}
