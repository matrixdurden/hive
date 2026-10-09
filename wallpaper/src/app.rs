//! Masaüstü motoru: tek iş parçacığı, tek bekleme noktası.
//!
//! Ne zaman çizileceği shader'ın kullandığı girdilere göre seçilir:
//!   Sürekli  — iTime / iAudio / iChannel okuyan buffer var: fps sınırında
//!   Fare     — yalnızca iMouse: imleç değişince
//!   Saat     — yalnızca iDate / iLocalTime / iBattery / iWeather: saniyede bir
//!   Durağan  — hiçbiri: yalnızca boyut ya da parametre değişince
//! Duraklatılınca (tam ekran, kilit, pil...) süresiz beklenir; hiçbir zamanlayıcı kurulmaz.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::audio::Capture;
use crate::config::Config;
use crate::desktop::WallpaperWindow;
use crate::format::{self, ParamKind};
use crate::gpu::{Gpu, Target};
use crate::policy::{Hooks, Policy, Reason};
use crate::render::{self, Engine, Inputs};
use crate::timer::{DirWatch, Waiter};
use crate::tray::{self, Event, Item, Tray};
use crate::util::{self, Res, wide};
use crate::{hub, log, shell, update};

/// Motorla gelen örnek duvar kâğıtları (`ensure_wallpapers` duvarlar klasörüne koyar).
pub const BUNDLED: &[(&str, &str)] = &[
    ("akis.cheshire", include_str!("../ornekler/akis.cheshire")),
    ("nabiz.cheshire", include_str!("../ornekler/nabiz.cheshire")),
    ("domore.cheshire", include_str!("../ornekler/domore.cheshire")),
    ("pus.cheshire", include_str!("../ornekler/pus.cheshire")),
    ("kum.cheshire", include_str!("../ornekler/kum.cheshire")),
    ("deco.cheshire", include_str!("../ornekler/deco.cheshire")),
];

mod id {
    pub const WALLPAPER: u32 = 1000; // + katalog sırası
    pub const FPS: u32 = 2000; // + fps
    pub const PAUSE: u32 = 3001;
    pub const BATTERY: u32 = 3002;
    pub const AUTOSTART: u32 = 3003;
    pub const OPEN_FOLDER: u32 = 3004;
    pub const AUTO_UPDATE: u32 = 3005;
    pub const QUIT: u32 = 3999;
    pub const PARAM: u32 = 4000; // + parametre * 16 + hazır değer
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Continuous,
    Mouse,
    Clock,
    Static,
}

struct Entry {
    file: String,
    name: String,
    name_tr: Option<String>,
}

/// Shadertoy iMouse: xy konum (biz fare masaüstündeyken üzerinde gezinmeyi de izliyoruz),
/// z,w tıklama konumu: z basılıyken pozitif, bırakınca negatif; w yalnızca tıklama karesinde pozitif.
#[derive(Default)]
struct Mouse {
    state: [f32; 4],
    down: bool,
}

impl Mouse {
    /// Değiştiyse `true`.
    fn update(&mut self, window: &WallpaperWindow) -> bool {
        let before = self.state;
        let mut pt = POINT::default();
        let over = unsafe { GetCursorPos(&mut pt).is_ok() && over_desktop(pt) };
        let pressed = over && unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } < 0;
        if over {
            let [x, y] = window.cursor();
            let (x, y) = (x, window.height as f32 - y);
            self.state[0] = x;
            self.state[1] = y;
            if pressed && !self.down {
                self.state[2] = x;
                self.state[3] = y;
            } else {
                self.state[3] = -self.state[3].abs();
            }
        }
        if !pressed && self.down {
            self.state[2] = -self.state[2].abs();
        }
        self.down = pressed;
        self.state != before
    }
}

/// İmleç masaüstünün (ikon katmanı ya da bizim pencere) üzerinde mi?
fn over_desktop(pt: POINT) -> bool {
    unsafe {
        let hwnd = WindowFromPoint(pt);
        let mut buf = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut buf) as usize;
        matches!(
            String::from_utf16_lossy(&buf[..n]).as_str(),
            "SysListView32" | "SHELLDLL_DefView" | "WorkerW" | "Progman" | "CheshireWallpaper"
        )
    }
}

fn mode_of(e: &Engine) -> Mode {
    let u = &e.duvar.usage;
    if u.time || u.audio || u.feedback {
        Mode::Continuous
    } else if u.mouse {
        Mode::Mouse
    } else if u.clock || u.battery || u.weather {
        Mode::Clock
    } else {
        Mode::Static
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn first_line(s: &str) -> String {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or("bilinmeyen hata").chars().take(180).collect()
}

/// Klasördeki `.wallpaper` dosyaları; ad olarak metadata'daki `ad`, yoksa dosya adı.
fn catalog() -> Vec<Entry> {
    let dir = util::wallpapers_dir();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("cheshire")))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let file = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let parsed = std::fs::read_to_string(&p).ok().and_then(|s| format::parse(&s).ok());
            let name_tr = parsed.as_ref().and_then(|d| d.name_tr.clone());
            let name = parsed
                .map(|d| d.name)
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| p.file_stem().unwrap_or_default().to_string_lossy().into_owned());
            Entry { file, name, name_tr }
        })
        .collect()
}

fn fnv(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// Örnekleri duvarlar klasörüyle eşler. `.sunulan` dosyası her örneğin en son yazılan sürümünün
/// özetini tutar:
/// - hiç sunulmamış örnek eklenir (yeni sürümle gelen duvar kâğıtları),
/// - kullanıcının dokunmadığı (özeti kayıtlı olanla aynı) eski örnek yenisiyle değişir,
/// - kullanıcının sildiği örnek geri gelmez, değiştirdiği ezilmez.
/// `.sunulan` hiç yoksa (önceki sürümlerden kalma klasör) adı tutan örnekler bizimdir: yenilenir.
pub fn ensure_wallpapers() {
    let dir = util::wallpapers_dir();
    let _ = std::fs::create_dir_all(&dir);
    let manifest = dir.join(".sunulan");
    let legacy = !manifest.exists();
    let mut offered: Vec<(String, u64)> = std::fs::read_to_string(&manifest)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(' ').and_then(|(n, h)| Some((n.to_string(), u64::from_str_radix(h, 16).ok()?))))
        .collect();
    let mut changed = legacy;
    for (name, src) in BUNDLED {
        let path = dir.join(name);
        let new = fnv(src.as_bytes());
        let known = offered.iter().find(|(n, _)| n == name).map(|(_, h)| *h);
        let current = std::fs::read(&path).ok().map(|d| fnv(&d));
        let write = match (current, known) {
            (None, None) => true,                         // ilk kez sunuluyor
            (None, Some(_)) => false,                     // kullanıcı silmiş
            (Some(c), _) if c == new => false,            // zaten güncel
            (Some(c), Some(k)) => c == k,                 // dokunulmamış eski sürüm
            (Some(_), None) => legacy,                    // eski klasör: örnek bizim
        };
        if write && std::fs::write(&path, src).is_ok() {
            log!("örnek duvar kâğıdı yazıldı: {name}");
        }
        if known != Some(new) && (write || current == Some(new)) {
            offered.retain(|(n, _)| n != name);
            offered.push((name.to_string(), new));
            changed = true;
        }
    }
    if changed {
        let text: String = offered.iter().map(|(n, h)| format!("{n} {h:016x}\n")).collect();
        let _ = std::fs::write(&manifest, text);
    }
}

/// Arka planda biten bir derleme. `gen` eski sonuçları ayıklar: kullanıcı derleme sürerken başka
/// bir duvar kâğıdı seçtiyse ilk sonuç atılır.
struct Compiled {
    seq: u64,
    file: String,
    result: Result<Engine, String>,
}

pub struct App {
    cfg: Config,
    policy: Policy,
    _hooks: Hooks,
    catalog: Vec<Entry>,
    watch: Option<DirWatch>,
    /// Klasör olayından sonra editörlerin art arda yazmaları bitsin diye kısa bekleme.
    rescan_at: Option<Instant>,
    stamp: Option<SystemTime>,
    // Alanlar yukarıdan aşağı düşürülür: yüzey (target) pencereden önce gitmeli.
    audio: Option<Capture>,
    engine: Option<Engine>,
    target: Target,
    gpu: Gpu,
    window: WallpaperWindow,
    tray: Tray,
    mouse: Mouse,
    time: f32,
    frame: u32,
    last_frame: Option<Instant>,
    next_frame: Instant,
    dirty: bool,
    status: String,
    /// Son yükleme hatası (hive'a bildirilir).
    error: Option<String>,
    /// En son yayımlanan durum: değişmediyse dosya yeniden yazılmaz.
    published: String,
    /// Yer denetimi en çok saniyede bir.
    placed_at: Instant,
    /// Süren derleme (dosya) ve sıra numarası; sonuç `compiled`e düşer, WM_COMPILED ile haber gelir.
    compiling: Option<String>,
    load_gen: u64,
    compiled: Arc<Mutex<Option<Compiled>>>,
    /// Açılışta kayıtlı seçim bozuksa sırayla denenecekler.
    fallback: Vec<String>,
    /// Pencere ilk kareden sonra gösterildi.
    shown: bool,
    quit: bool,
}

impl App {
    pub fn new() -> Res<Self> {
        let cfg = Config::load();
        crate::weather::set_location(cfg.location.as_deref());
        ensure_wallpapers();
        // hive modunda kayıtlar ve güncelleme hive'da.
        if !hub::active() {
            shell::register();
        }
        let tray = Tray::new()?;
        // Geliştirme kopyası güncellenmez: kurulu olmayan exe'nin üstüne yazmak anlamsız.
        update::ENABLED.store(cfg.auto_update, std::sync::atomic::Ordering::Relaxed);
        if shell::installed_copy() && !hub::active() {
            update::spawn(tray.update_waker());
        }
        let window = WallpaperWindow::attach(&cfg.host)?;
        let (gpu, surface) = Gpu::new(Some(window.hwnd), cfg.high_performance_gpu)?;
        let target = Target::new(&gpu, surface.ok_or("yüzey oluşturulamadı")?, window.width, window.height)?;

        let mut app = Self {
            policy: Policy::new(),
            _hooks: Hooks::install(),
            catalog: catalog(),
            watch: DirWatch::new(&util::wallpapers_dir()).ok(),
            rescan_at: None,
            stamp: None,
            audio: None,
            engine: None,
            target,
            gpu,
            window,
            tray,
            mouse: Mouse::default(),
            time: 0.0,
            frame: 0,
            last_frame: None,
            next_frame: Instant::now(),
            dirty: true,
            status: String::new(),
            error: None,
            published: String::new(),
            placed_at: Instant::now(),
            compiling: None,
            load_gen: 0,
            compiled: Arc::default(),
            fallback: Vec::new(),
            shown: false,
            quit: false,
            cfg,
        };
        let file = app.cfg.wallpaper.clone();
        // Kayıtlı seçim bozuksa listedekiler sırayla denenir (derleme bitince, `finish_load`).
        app.fallback = app.catalog.iter().map(|e| e.file.clone()).filter(|f| *f != file).rev().collect();
        app.load(&file);
        Ok(app)
    }

    /// Tepsiden bilgi balonu (kuruldu, güncellendi).
    pub fn announce(&self, title: &str, text: &str) {
        self.tray.inform(title, text);
    }

    fn mode(&self) -> Mode {
        self.engine.as_ref().map_or(Mode::Static, mode_of)
    }

    fn fps(&self) -> u32 {
        let own = self.engine.as_ref().and_then(|e| e.duvar.fps).unwrap_or(u32::MAX);
        self.cfg.fps.min(own).max(1)
    }

    /// Dosyayı okur, ayrıştırır ve derlemeyi arka plana atar; bitince `finish_load`. Derleme
    /// büyük shader'larda saniyeler sürer (Do more: ~20 s): o sırada önceki duvar kâğıdı çalışmaya
    /// devam eder, hive'ın arayüzü ve masaüstü donmaz.
    fn load(&mut self, file: &str) {
        let path = util::wallpapers_dir().join(file);
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| format!("okunamadı: {e}"))
            .and_then(|src| format::parse(&src).map_err(|e| e.to_string()));
        let duvar = match parsed {
            Ok(d) => d,
            Err(e) => {
                self.load_failed(file, &path, &e);
                return;
            }
        };
        self.load_gen += 1;
        self.compiling = Some(file.to_string());
        log!("derleniyor: {file}");
        let (seq, gpu, format, size) = (self.load_gen, self.gpu.clone(), self.target.config.format, (self.window.width, self.window.height));
        let (slot, wake, file) = (self.compiled.clone(), self.tray.compile_waker(), file.to_string());
        std::thread::spawn(move || {
            let result = Engine::new(&gpu, duvar, format, size)
                .map_err(|diags| diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n"));
            *slot.lock().unwrap() = Some(Compiled { seq, file, result });
            wake();
        });
    }

    /// WM_COMPILED: arka plandaki derleme bitti. Hata olursa önceki duvar kâğıdı kalır.
    fn finish_load(&mut self) {
        let Some(c) = self.compiled.lock().unwrap().take() else { return };
        if c.seq != self.load_gen {
            log!("eski derleme atıldı: {}", c.file);
            return;
        }
        self.compiling = None;
        let file = c.file;
        let path = util::wallpapers_dir().join(&file);
        match c.result {
            Ok(mut engine) => {
                for (i, p) in engine.duvar.params.clone().iter().enumerate() {
                    let saved = self.cfg.param(&file, &p.name).and_then(|v| match p.kind {
                        ParamKind::Float { .. } => v.parse().ok().map(|x| [x, 0.0, 0.0, 0.0]),
                        ParamKind::Color => format::parse_color(v),
                    });
                    if let Some(v) = saved {
                        engine.set_param(&self.gpu, i, v);
                    }
                }
                // Derleme sürerken ekran değiştiyse.
                let size = (self.window.width, self.window.height);
                if engine.size() != size {
                    engine.resize(&self.gpu, size);
                }
                log!("duvar kâğıdı: {file} ({:?})", mode_of(&engine));
                self.engine = Some(engine);
                self.cfg.wallpaper = file;
                self.cfg.save();
                self.stamp = modified(&path);
                self.frame = 0;
                self.dirty = true;
                self.error = None;
            }
            Err(e) => self.load_failed(&file, &path, &e),
        }
    }

    fn load_failed(&mut self, file: &str, path: &Path, e: &str) {
        log!("{file} yüklenemedi:\n{e}");
        self.tray.notify(&format!("{file} yüklenemedi"), &first_line(e));
        self.error = Some(format!("{file}: {}", first_line(e)));
        // Bozuk dosyayı da izle: düzeltip kaydedince kendiliğinden yüklensin.
        if self.cfg.wallpaper == file {
            self.stamp = modified(path);
        }
        // Açılışta hiçbir şey çalışmıyorsa listedeki bir sonrakini dene.
        if self.engine.is_none()
            && let Some(next) = self.fallback.pop()
        {
            self.load(&next);
        }
    }

    fn rescan(&mut self) {
        self.catalog = catalog();
        let path = util::wallpapers_dir().join(&self.cfg.wallpaper);
        let now = modified(&path);
        if now.is_some() && now != self.stamp {
            log!("değişti, yeniden yükleniyor: {}", self.cfg.wallpaper);
            self.stamp = now;
            let file = self.cfg.wallpaper.clone();
            self.load(&file);
        }
    }

    fn reattach(&mut self) -> Res<()> {
        let window = WallpaperWindow::attach(&self.cfg.host)?;
        let surface = self.gpu.surface_for(window.hwnd)?;
        // Önce eski yüzey, sonra eski pencere bırakılır.
        self.target = Target::new(&self.gpu, surface, window.width, window.height)?;
        self.window = window;
        self.shown = false;
        self.resized();
        Ok(())
    }

    /// Pencere öldüyse yeniden yerleşir, z-sırası kaydıysa düzeltir (bkz. desktop::in_place).
    fn ensure_placed(&mut self) {
        if self.placed_at.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.placed_at = Instant::now();
        if !self.window.alive() {
            log!("duvar kâğıdı penceresi yok olmuş, yeniden yerleşiliyor");
            if let Err(e) = self.reattach() {
                log!("yeniden yerleşilemedi: {e}");
            }
        } else if !self.window.in_place() {
            log!("yer kaymış (Explorer WorkerW'yi yeniledi), düzeltiliyor");
            self.window.restack();
            self.dirty = true;
        }
    }

    fn resized(&mut self) {
        let size = (self.window.width, self.window.height);
        self.target.resize(&self.gpu, size.0, size.1);
        if let Some(e) = &mut self.engine {
            e.resize(&self.gpu, size);
        }
        self.dirty = true;
    }

    fn handle_events(&mut self) {
        for ev in tray::take_events() {
            match ev {
                Event::MenuRequested => self.menu(),
                Event::Quit => self.quit = true,
                Event::UpdateReady => match update::apply() {
                    Ok(()) => self.quit = true,
                    Err(e) => log!("güncelleme uygulanamadı: {e}"),
                },
                Event::SessionLocked(l) => self.policy.locked = l,
                Event::WindowsChanged => self.policy.refresh_windows(),
                Event::OnBattery(b) => self.policy.on_battery = b,
                Event::ScreenOn(on) => self.policy.display_off = !on,
                Event::Install(file) => {
                    self.catalog = catalog();
                    self.load(&file);
                }
                Event::Compiled => self.finish_load(),
                Event::Command(cmd) => self.command(&cmd),
                Event::ExplorerRestarted => {
                    log!("Explorer yeniden başladı");
                    self.tray.add();
                    if let Err(e) = self.reattach() {
                        log!("yeniden yerleşilemedi: {e}");
                    }
                }
                Event::DisplayChanged => {
                    if !self.window.alive() {
                        let _ = self.reattach();
                    } else if let Some((w, h)) = self.window.refresh_size() {
                        log!("ekran değişti: {w}x{h}");
                        self.resized();
                    }
                }
            }
        }
    }

    fn menu(&mut self) {
        let wallpapers = self
            .catalog
            .iter()
            .enumerate()
            .map(|(i, e)| Item::Action {
                id: id::WALLPAPER + i as u32,
                label: e.name.clone(),
                checked: e.file == self.cfg.wallpaper,
            })
            .collect();
        let fps = [15, 30, 60, 120, 144]
            .into_iter()
            .map(|f| Item::Action { id: id::FPS + f, label: format!("{f} FPS"), checked: self.cfg.fps == f })
            .collect();

        let mut items = vec![
            Item::Label(format!("cheshire {} — {}", env!("CARGO_PKG_VERSION"), self.status)),
            Item::Separator,
            Item::Submenu { label: "Duvar kâğıdı".into(), items: wallpapers },
        ];
        if let Some(e) = self.engine.as_ref().filter(|e| !e.duvar.params.is_empty()) {
            let params = e
                .duvar
                .params
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let current = e.param(i);
                    let presets = render::presets(&p)
                        .into_iter()
                        .enumerate()
                        .map(|(j, (label, v))| Item::Action {
                            id: id::PARAM + (i * 16 + j) as u32,
                            label,
                            checked: v.iter().zip(current).all(|(a, b)| (a - b).abs() < 1e-3),
                        })
                        .collect();
                    Item::Submenu { label: p.label.clone().unwrap_or_else(|| p.name.clone()), items: presets }
                })
                .collect();
            items.push(Item::Submenu { label: "Ayarlar".into(), items: params });
        }
        items.extend([
            Item::Submenu { label: "Kare hızı".into(), items: fps },
            Item::Separator,
            Item::Action { id: id::PAUSE, label: "Duraklat".into(), checked: self.policy.user_paused },
            Item::Action { id: id::BATTERY, label: "Pildeyken duraklat".into(), checked: self.cfg.pause_on_battery },
            Item::Action { id: id::AUTOSTART, label: "Windows ile başlat".into(), checked: shell::autostart_enabled() },
            Item::Action { id: id::AUTO_UPDATE, label: "Otomatik güncelle".into(), checked: self.cfg.auto_update },
            Item::Action { id: id::OPEN_FOLDER, label: "Duvar kâğıdı klasörünü aç".into(), checked: false },
            Item::Separator,
            Item::Action { id: id::QUIT, label: "Çıkış".into(), checked: false },
        ]);

        let Some(choice) = self.tray.show_menu(&items) else { return };
        match choice {
            id::PAUSE => self.policy.user_paused = !self.policy.user_paused,
            id::BATTERY => self.cfg.pause_on_battery = !self.cfg.pause_on_battery,
            id::AUTOSTART if !shell::installed_copy() => {
                self.tray.notify("Geliştirme kopyası", "Windows ile başlatmak için `make install` ile kurulan kopyayı kullan.")
            }
            id::AUTOSTART => shell::set_autostart(!shell::autostart_enabled()),
            id::AUTO_UPDATE => {
                self.cfg.auto_update = !self.cfg.auto_update;
                update::ENABLED.store(self.cfg.auto_update, std::sync::atomic::Ordering::Relaxed);
            }
            id::OPEN_FOLDER => {
                let dir = wide(&util::wallpapers_dir().to_string_lossy());
                unsafe {
                    ShellExecuteW(None, w!("open"), PCWSTR(dir.as_ptr()), None, None, SW_SHOWNORMAL);
                }
            }
            id::QUIT => self.quit = true,
            c if (id::FPS..id::PAUSE).contains(&c) => self.cfg.fps = c - id::FPS,
            c if (id::WALLPAPER..id::FPS).contains(&c) => {
                if let Some(file) = self.catalog.get((c - id::WALLPAPER) as usize).map(|e| e.file.clone()) {
                    self.load(&file);
                }
            }
            c if c >= id::PARAM => self.pick_param(((c - id::PARAM) / 16) as usize, ((c - id::PARAM) % 16) as usize),
            _ => {}
        }
        self.dirty = true;
        self.cfg.save();
    }

    fn pick_param(&mut self, i: usize, j: usize) {
        let Some(e) = &self.engine else { return };
        let Some(p) = e.duvar.params.get(i).cloned() else { return };
        let Some((_, v)) = render::presets(&p).into_iter().nth(j) else { return };
        self.set_param(i, v);
    }

    /// Parametreyi uygular ve bu duvar kâğıdı için kaydeder.
    fn set_param(&mut self, i: usize, v: [f32; 4]) {
        let Some(e) = &mut self.engine else { return };
        let Some(p) = e.duvar.params.get(i).cloned() else { return };
        e.set_param(&self.gpu, i, v);
        let text = match p.kind {
            ParamKind::Float { .. } => format!("{}", v[0]),
            ParamKind::Color => format::color_hex(v),
        };
        let file = self.cfg.wallpaper.clone();
        self.cfg.set_param(&file, &p.name, text);
    }

    /// hive'dan gelen komut (bkz. hub.rs).
    fn command(&mut self, line: &str) {
        let (cmd, arg) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
        let arg = arg.trim();
        let on = arg == "1";
        match cmd {
            "hub" => {
                if let Ok(h) = arg.parse::<isize>() {
                    hub::set(h);
                    self.tray.hide();
                    self.published.clear();
                    log!("hive'a bağlandı");
                }
            }
            "duvar" => {
                self.catalog = catalog();
                self.load(arg);
            }
            "param" => {
                let Some((i, v)) = arg.split_once(' ') else { return };
                let (Ok(i), Some(e)) = (i.parse::<usize>(), &self.engine) else { return };
                let Some(p) = e.duvar.params.get(i) else { return };
                let v = v.trim();
                let value = match p.kind {
                    ParamKind::Float { .. } => v.parse().ok().map(|x| [x, 0.0, 0.0, 0.0]),
                    ParamKind::Color => format::parse_color(v),
                };
                if let Some(value) = value {
                    self.set_param(i, value);
                }
            }
            "fps" => {
                if let Ok(n) = arg.parse::<u32>() {
                    self.cfg.fps = n.clamp(1, 240);
                }
            }
            "duraklat" => self.policy.user_paused = on,
            "pilde" => self.cfg.pause_on_battery = on,
            "klasor" => {
                let dir = wide(&util::wallpapers_dir().to_string_lossy());
                unsafe {
                    ShellExecuteW(None, w!("open"), PCWSTR(dir.as_ptr()), None, None, SW_SHOWNORMAL);
                }
            }
            "cik" => self.quit = true,
            _ => log!("bilinmeyen komut: {line}"),
        }
        self.dirty = true;
        self.cfg.save();
    }

    /// hive'a durum: satır başına `anahtar=değer`. Değişmediyse yazılmaz.
    fn publish(&mut self, paused: Option<Reason>) {
        if !hub::active() {
            return;
        }
        // neden: duraklama sebebi (dilden bağımsız), fps_etkin: duvar kâğıdının kendi sınırıyla.
        let mut s = format!(
            "surum={}\ndurum={}\nduraklama={}\nneden={}\nfps_etkin={}\nmotor={}\nkullanici_duraklatti={}\npilde={}\nfps={}\nsecili={}\nhata={}\n",
            env!("CARGO_PKG_VERSION"),
            self.status,
            paused.map_or("", |r| r.label()),
            paused.map_or("", |r| r.key()),
            self.fps(),
            self.engine.is_some() as u8,
            self.policy.user_paused as u8,
            self.cfg.pause_on_battery as u8,
            self.cfg.fps,
            self.cfg.wallpaper,
            self.error.as_deref().unwrap_or(""),
        );
        for e in &self.catalog {
            s.push_str(&format!("duvar={}|{}|{}\n", e.file, e.name, e.name_tr.as_deref().unwrap_or("")));
        }
        if let Some(e) = &self.engine {
            for (i, p) in e.duvar.params.iter().enumerate() {
                let v = e.param(i);
                // param=sıra|ad|tür|min|max|değer|varsayılan|görünen ad|seçenekler (; ile)|Türkçe ad
                let label = p.label.as_deref().unwrap_or(&p.name);
                let label_tr = p.label_tr.as_deref().unwrap_or("");
                let line = match p.kind {
                    ParamKind::Float { min, max } => {
                        let kind = if p.toggle { "anahtar" } else if p.choices.is_empty() { "sayi" } else { "secim" };
                        format!(
                            "param={i}|{}|{kind}|{min}|{max}|{}|{}|{label}|{}|{label_tr}\n",
                            p.name,
                            v[0],
                            p.default[0],
                            p.choices.join(";")
                        )
                    }
                    ParamKind::Color => format!(
                        "param={i}|{}|renk|||{}|{}|{label}||{label_tr}\n",
                        p.name,
                        format::color_hex(v),
                        format::color_hex(p.default)
                    ),
                };
                s.push_str(&line);
            }
        }
        if s != self.published {
            hub::publish(&s);
            self.published = s;
        }
    }

    fn render(&mut self) {
        let Some(engine) = &self.engine else { return };
        let now = Instant::now();
        let dt = self.last_frame.map_or(1.0 / 60.0, |t| (now - t).as_secs_f32()).min(0.25);
        self.last_frame = Some(now);
        self.time += dt;

        let frame = match self.target.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.target.resize(&self.gpu, self.window.width, self.window.height);
                return;
            }
            _ => return,
        };
        let view = frame.texture.create_view(&Default::default());
        if let Some(a) = &self.audio {
            engine.set_audio(&self.gpu, &a.latest());
        }
        let (date, local_time) = util::local_clock();
        let input = Inputs {
            time: self.time,
            time_delta: dt,
            frame: self.frame,
            frame_rate: 1.0 / dt.max(1e-4),
            mouse: self.mouse.state,
            date,
            battery: util::battery_level(),
            local_time,
            weather: crate::weather::current(),
        };
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        engine.render(&self.gpu, &mut encoder, &view, &input);
        self.gpu.queue.submit(Some(encoder.finish()));
        self.gpu.queue.present(frame);
        self.frame = self.frame.wrapping_add(1);
        self.dirty = false;
        if !self.shown {
            self.window.show();
            self.shown = true;
        }
    }

    fn set_status(&mut self, paused: Option<Reason>) {
        let name = self.catalog.iter().find(|e| e.file == self.cfg.wallpaper).map_or("—", |e| e.name.as_str());
        let status = match (paused, &self.engine) {
            (Some(r), _) => format!("duraklatıldı ({})", r.label()),
            (None, None) if self.compiling.is_some() => "derleniyor…".into(),
            (None, None) => "geçerli duvar kâğıdı yok".into(),
            (None, Some(_)) => format!("{name} · {} FPS", self.fps()),
        };
        if status != self.status {
            log!("durum: {status}");
            self.tray.set_tooltip(&format!("cheshire — {status}"));
            self.status = status;
        }
    }

    /// Ses yakalama yalnızca gerektiğinde ve motor çalışırken açıktır.
    fn sync_audio(&mut self, running: bool) {
        let wanted = running && self.engine.as_ref().is_some_and(|e| e.duvar.usage.audio);
        match (wanted, self.audio.is_some()) {
            (true, false) => self.audio = Some(Capture::start()),
            (false, true) => self.audio = None,
            _ => {}
        }
        crate::weather::want(running && self.engine.as_ref().is_some_and(|e| e.duvar.usage.weather));
    }

    pub fn run(mut self) -> Res<()> {
        let waiter = Waiter::new()?;
        let mut last_second = 0u64;

        while pump() && !self.quit {
            self.handle_events();
            // Duraklatılmışken aşağıda süresiz beklenir: çıkış isteği orada kaybolmasın.
            if self.quit {
                break;
            }
            if self.rescan_at.is_some_and(|t| Instant::now() >= t) {
                self.rescan_at = None;
                self.rescan();
            }
            self.ensure_placed();

            // hive kapandıysa (çöktüyse) motor da kapanır.
            if !hub::alive() {
                log!("hive kapandı, çıkılıyor");
                break;
            }
            let paused = self.policy.reason(self.cfg.pause_on_battery);
            self.set_status(paused);
            self.publish(paused);
            self.sync_audio(paused.is_none());

            // Ne kadar uyuyacağımız: None = bir olay gelene kadar.
            let mut delay: Option<Duration> = None;
            if paused.is_some() {
                self.last_frame = None; // devam edince zaman sıçramasın
                self.next_frame = Instant::now();
            } else {
                let now = Instant::now();
                let interval = Duration::from_secs_f64(1.0 / self.fps() as f64);
                let uses_mouse = self.engine.as_ref().is_some_and(|e| e.duvar.usage.mouse);
                let mouse_moved = uses_mouse && self.mouse.update(&self.window);
                match self.mode() {
                    Mode::Continuous => {
                        if now >= self.next_frame {
                            self.render();
                            self.next_frame += interval;
                            if self.next_frame < now {
                                self.next_frame = now + interval; // geride kaldıysak yetişmeye çalışma
                            }
                        }
                        delay = Some(self.next_frame.saturating_duration_since(Instant::now()));
                    }
                    Mode::Mouse => {
                        if mouse_moved || self.dirty {
                            self.render();
                        }
                        delay = Some(interval);
                    }
                    Mode::Clock => {
                        let second = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
                        if self.dirty || second.as_secs() != last_second {
                            last_second = second.as_secs();
                            self.render();
                        }
                        delay = Some(Duration::from_millis(1000 - second.subsec_millis() as u64));
                    }
                    Mode::Static => {
                        if self.dirty {
                            self.render();
                        }
                    }
                }
            }
            if let Some(t) = self.rescan_at {
                let left = t.saturating_duration_since(Instant::now());
                delay = Some(delay.map_or(left, |d| d.min(left)));
            }
            // Arada uyan: hive yaşıyor mu, masaüstündeki yerimiz duruyor mu.
            let tick = Duration::from_secs(2);
            delay = Some(delay.map_or(tick, |d| d.min(tick)));

            if waiter.wait(delay, self.watch.as_ref()) {
                if let Some(w) = &self.watch {
                    w.rearm();
                }
                self.rescan_at = Some(Instant::now() + Duration::from_millis(150));
            }
        }
        self.cfg.save();
        log!("çıkılıyor");
        Ok(())
    }
}

/// Bekleyen tüm pencere mesajlarını işler. WM_QUIT gelirse `false`.
fn pump() -> bool {
    let mut msg = MSG::default();
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                return false;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    true
}
