//! dock: Windows'un görev çubuğunun yerine ekranın altında yüzen bir dock. Tek liste:
//! sabitlenenler (uygulama, klasör, ne sabitlenirse), yanlarında açık olanlar; sağ uçta ağ, ses,
//! pil ve saat. Masaüstü simgeleri de istenirse gizlenir.
//!
//! Dock'un solunda çalan şarkının şeridi (music/): Spotify, kapak, düğmeler.
//!
//! Motor hive sürecinin içinde (bar.rs). Kurmak ayar dosyasını ve dock'un ilk listesini
//! (görev çubuğuna sabitlenmiş kısayollar) yazar.
//! Kaldırınca (ya da hive kapanınca) görev çubuğu ve masaüstü simgeleri eski haline döner.

mod apps;
mod bar;
pub mod jump;
mod menu;
pub mod music;
pub mod stack;
mod status;
mod keys;
mod taskbar;
pub(crate) mod theme;
mod toasts;
mod tray;

use std::path::PathBuf;

pub use bar::{Geometry, bench, geometry};

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::InvalidateRect;

use crate::gfx::{Gfx, Rect};
use crate::ui::*;
use crate::util;


const ROW: f32 = 64.0;
const SIZES: (u32, u32, u32) = (36, 64, 4);

pub fn dir() -> PathBuf {
    util::data_dir().join("dock")
}

fn ini() -> PathBuf {
    dir().join("ayarlar.ini")
}

fn pins_file() -> PathBuf {
    dir().join("dock.txt")
}

pub fn installed() -> bool {
    dir().is_dir()
}

#[derive(Clone, Copy, PartialEq)]
pub struct Settings {
    /// Simge boyu (DIP).
    pub size: u32,
    /// Etkin köşeler: sol üst görev görünümü, sağ alt masaüstü.
    pub corner_tl: bool,
    pub corner_br: bool,
    /// Dock'un ve müzik şeridinin arka planının opaklığı, yüzde (30..100).
    pub opacity: u32,
    /// Simgeler kilitli: sürükleyerek yerleri değişmez.
    pub locked: bool,
}

impl Settings {
    fn load() -> Self {
        let mut s = Settings { size: 48, corner_tl: true, corner_br: true, opacity: 88, locked: false };
        let text = std::fs::read_to_string(ini()).unwrap_or_default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "kose_sol_ust" => s.corner_tl = v == "1",
                "kose_sag_alt" => s.corner_br = v == "1",
                "opaklik" => s.opacity = v.parse::<u32>().unwrap_or(88).clamp(30, 100),
                "kilitli" => s.locked = v == "1",
                "boyut" => {
                    if let Ok(n) = v.parse::<u32>() {
                        s.size = n.clamp(SIZES.0, SIZES.1);
                    }
                }
                _ => {}
            }
        }
        s
    }

    fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(dir())?;
        let b = |v: bool| v as u8;
        std::fs::write(
            ini(),
            format!(
                "boyut={}\nkose_sol_ust={}\nkose_sag_alt={}\nopaklik={}\nkilitli={}\n",
                self.size,
                b(self.corner_tl),
                b(self.corner_br),
                self.opacity,
                b(self.locked)
            ),
        )
    }
}

fn load_pins() -> Vec<String> {
    std::fs::read_to_string(pins_file())
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

fn save_pins(lines: &[String]) {
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    if let Err(e) = std::fs::write(pins_file(), text) {
        crate::log!("dock: {} yazılamadı: {e}", pins_file().display());
    }
}


/// İlk liste: görev çubuğuna sabitlenmiş kısayollar.
fn seed() -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let pinned = PathBuf::from(appdata).join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar");
        if let Ok(rd) = std::fs::read_dir(pinned) {
            let mut links: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")))
                .collect();
            links.sort();
            lines.extend(links.iter().map(|p| p.display().to_string()));
        }
    }
    lines
}

/// Sağ tık menüsündeki "Dock'a sabitle" (her dosya, kısayol, program ve klasör). Windows 11 bu
/// klasik komutları "Daha fazla seçenek göster"in altında gösterir. Geliştirme kopyası (WSL'den)
/// kalıcı kayıt yapmaz.
const VERB_KEYS: [&str; 2] = [r"Software\Classes\*\shell\hatter.pin", r"Software\Classes\Directory\shell\hatter.pin"];

fn shell_verbs(on: bool) {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegDeleteTreeW, RegSetKeyValueW};
    use windows::core::PCWSTR;
    for key in VERB_KEYS {
        let k = util::wide(key);
        unsafe {
            if !on {
                let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()));
                continue;
            }
            let Some(exe) = util::installed_copy() else { return };
            let set = |sub: &str, name: Option<&str>, value: &str| {
                let sk = util::wide(&if sub.is_empty() { key.to_string() } else { format!("{key}\\{sub}") });
                let n = name.map(util::wide);
                let v = util::wide(value);
                let _ = RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    PCWSTR(sk.as_ptr()),
                    n.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
                    REG_SZ.0,
                    Some(v.as_ptr().cast()),
                    (v.len() * 2) as u32,
                );
            };
            set("", None, t!("Pin to dock", "Dock'a sabitle"));
            set("", Some("Icon"), &format!("\"{exe}\",0"));
            set("command", None, &format!("\"{exe}\" --dock-pin \"%1\""));
        }
    }
}

/// `hive --dock-pin <yol>`: sağ tık menüsünden. Yol dock'un sonuna eklenir, açık dock listeyi
/// yeniden okur.
pub fn pin_from_cli(path: &str) {
    if path.is_empty() || !installed() {
        return;
    }
    let mut lines = load_pins();
    if !lines.iter().any(|l| l.eq_ignore_ascii_case(path)) {
        lines.push(path.to_string());
        save_pins(&lines);
    }
    bar::notify_reload();
}

pub fn install() -> Result<(), String> {
    let err = |e: std::io::Error| format!("{}: {e}", dir().display());
    std::fs::create_dir_all(dir()).map_err(err)?;
    Settings::load().save().map_err(err)?;
    if !pins_file().exists() {
        save_pins(&seed());
    }
    shell_verbs(true);
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    // Motor durunca çoğu zaten geri alınır; Başlat hizası ve çökmüş bir oturumdan kalan burada.
    taskbar::restore(true);
    shell_verbs(false);
    match std::fs::remove_dir_all(dir()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{} {}: {e}", t!("could not delete", "silinemedi:"), dir().display()))
        }
        _ => Ok(()),
    }
}

pub fn leftovers() -> Vec<String> {
    let d = dir();
    let mut v: Vec<String> = d.exists().then(|| d.display().to_string()).into_iter().collect();
    for key in VERB_KEYS {
        if util::reg_key_exists(windows::Win32::System::Registry::HKEY_CURRENT_USER, key) {
            v.push(format!(r"HKCU\{key}"));
        }
    }
    v
}






/// Tuş birleşimini basıp bırakır (az önce tıklandı: girdi bu sürecin).
pub fn press(keys: &[windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY]) {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() }, ..Default::default() },
        },
    };
    let mut v: Vec<INPUT> = keys.iter().map(|&k| key(k, false)).collect();
    v.extend(keys.iter().rev().map(|&k| key(k, true)));
    unsafe {
        SendInput(&v, size_of::<INPUT>() as i32);
    }
}

/// Uygulama kimliği (AppUserModelID ya da exe adı, "Spotify.exe" gibi) bu olan açık pencereyi
/// öne getirir; zaten öndeyse küçültür (görev çubuğu gibi). Açık penceresi yoksa `false`.
pub fn focus_app(id: &str) -> bool {
    let Some(w) = apps::windows().into_iter().find(|w| {
        w.aumid.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(id)) || w.exe_name().eq_ignore_ascii_case(id)
    }) else {
        return false;
    };
    if unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() } == w.hwnd {
        apps::minimize(w.hwnd);
    } else {
        apps::activate(w.hwnd);
    }
    true
}

/// `hive --dock-test`
pub fn probe() -> String {
    let pins = if pins_file().exists() { load_pins() } else { seed() };
    apps::probe(&pins)
}

// --- Sayfa ---

/// Sayfanın satırları, yukarıdan aşağı. Müzik satırlarının üstünde bölüm başlığı.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Row {
    CornerTl,
    CornerBr,
    Size,
    Opacity,
    Lock,
    Music,
    Style,
    Progress,
    Others,
    HideIdle,
}

const ROWS: [Row; 10] = [
    Row::CornerTl,
    Row::CornerBr,
    Row::Size,
    Row::Opacity,
    Row::Lock,
    Row::Music,
    Row::Style,
    Row::Progress,
    Row::Others,
    Row::HideIdle,
];
/// Müzik bölümünün başlığı için boşluk.
const SECTION: f32 = 52.0;

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    None,
    Toggle(Row),
    /// Arka plan seçicisinin `n`. parçası.
    Style(usize),
    /// Kaydırıcı: simge boyu ya da müziğin saydamlığı.
    Slider(Row),
}

pub struct Dock {
    hwnd: HWND,
    settings: Settings,
    music: music::Music,
    hover: Hit,
    pressed: Hit,
    /// Sürüklenen kaydırıcı (bırakınca uygulanır).
    dragging: Option<Row>,
    scroll: f32,
    w: f32,
    h: f32,
    head_x: f32,
    head_r: f32,
}

impl Dock {
    pub fn start(hwnd: HWND) -> Self {
        let settings = Settings::load();
        shell_verbs(true);
        taskbar::hide();
        bar::start(settings, load_pins());
        Self {
            hwnd,
            settings,
            music: music::Music::start(hwnd, settings.opacity),
            hover: Hit::None,
            pressed: Hit::None,
            dragging: None,
            scroll: 0.0,
            w: 0.0,
            h: 0.0,
            head_x: 0.0,
            head_r: 0.0,
        }
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn row(&self, r: Row) -> Rect {
        let i = ROWS.iter().position(|&x| x == r).unwrap_or(0);
        let section = if i >= 5 { SECTION } else { 0.0 };
        let t = HEADER + 12.0 + i as f32 * ROW + section - self.scroll;
        Rect::new(PAD, t, self.w - PAD, t + ROW)
    }

    fn max_scroll(&self) -> f32 {
        (HEADER + 24.0 + ROWS.len() as f32 * ROW + SECTION - self.h).max(0.0)
    }

    fn slider_x(&self, r: Row) -> (f32, f32) {
        let r = self.row(r);
        (r.r - 220.0, r.r - 52.0)
    }

    fn style_labels() -> [&'static str; 3] {
        [t!("Cover", "Kapak"), t!("Color", "Renk"), t!("Like the dock", "Dock gibi")]
    }

    /// Arka plan seçicisinin parçaları, satırın sağına yaslı.
    fn seg_rects(&self, g: &Gfx) -> Vec<Rect> {
        let r = self.row(Row::Style);
        let labels = Self::style_labels();
        let w = labels.iter().map(|l| g.measure(l, &g.f.button)).fold(0.0, f32::max) + 28.0;
        let (t, b) = (r.cy() - 15.0, r.cy() + 15.0);
        (0..3).map(|i| Rect::new(r.r - (3 - i) as f32 * w, t, r.r - (2 - i) as f32 * w, b)).collect()
    }

    /// Müzik kapalıyken altındaki ayarlar sönük durur.
    fn enabled(&self, r: Row) -> bool {
        !matches!(r, Row::Style | Row::Progress | Row::Others | Row::HideIdle) || self.music.settings.show
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return Hit::None;
        }
        for row in ROWS {
            let r = self.row(row);
            if !r.contains(x, y) || !self.enabled(row) {
                continue;
            }
            return match row {
                Row::Style => self.seg_rects(g).iter().position(|s| s.contains(x, y)).map_or(Hit::None, Hit::Style),
                Row::Size | Row::Opacity => {
                    let (a, b) = self.slider_x(row);
                    if x >= a - 10.0 && x <= b + 10.0 { Hit::Slider(row) } else { Hit::None }
                }
                _ if x > r.r - 120.0 => Hit::Toggle(row),
                _ => Hit::None,
            };
        }
        Hit::None
    }

    fn set_slider_from(&mut self, row: Row, x: f32) {
        let (a, b) = self.slider_x(row);
        let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
        if row == Row::Size {
            let (lo, hi, step) = SIZES;
            let v = lo as f32 + t * (hi - lo) as f32;
            let v = ((v / step as f32).round() as u32 * step).clamp(lo, hi);
            if v != self.settings.size {
                self.settings.size = v;
                self.redraw();
            }
        } else {
            let v = (30.0 + t * 70.0).round() as u32;
            if v != self.settings.opacity {
                // Sürüklerken dock da şerit de canlı değişsin (dosyaya bırakınca yazılır).
                self.settings.opacity = v;
                bar::apply(self.settings);
                self.music.set_opacity(v);
                self.redraw();
            }
        }
    }

    fn commit(&mut self) {
        if let Err(e) = self.settings.save() {
            crate::log!("dock: ayarlar yazılamadı: {e}");
        }
        bar::apply(self.settings);
        self.redraw();
    }

    fn texts(row: Row) -> (&'static str, &'static str) {
        match row {
            Row::CornerTl => (
                t!("Top-left corner", "Sol üst köşe"),
                t!(
                    "Throw the cursor into the corner to see all windows (Win+Tab)",
                    "İmleci köşeye sertçe götürünce bütün pencereler görünür (Win+Tab)"
                ),
            ),
            Row::CornerBr => (
                t!("Bottom-right corner", "Sağ alt köşe"),
                t!(
                    "Throw the cursor into the corner for the desktop, again to come back (Win+D)",
                    "İmleci köşeye sertçe götürünce masaüstü, tekrar götürünce geri döner (Win+D)"
                ),
            ),
            Row::Size => (t!("Icon size", "Simge boyu"), ""),
            Row::Lock => (
                t!("Lock icons", "Simgeleri kilitle"),
                t!(
                    "Icons can't be moved by dragging; otherwise hold an icon a moment to move it",
                    "Simgeler sürüklenerek yer değiştirmez; kapalıyken simgeyi bir an basılı tutup taşırsın"
                ),
            ),
            Row::Music => (
                t!("Now playing", "Çalan şarkı"),
                t!(
                    "Spotify left of the dock; click it to bring Spotify forward",
                    "Dock'un solunda Spotify; tıklayınca Spotify öne gelir"
                ),
            ),
            Row::Style => (
                t!("Background", "Arka plan"),
                t!("The cover blurred, its color, or plain like the dock", "Kapağın bulanık hali, rengi ya da dock gibi düz"),
            ),
            Row::Opacity => (
                t!("Opacity", "Saydamlık"),
                t!("Of the dock and the music strip; icons and text stay sharp", "Dock'un ve müzik şeridinin; simgeler ve yazılar hep net"),
            ),
            Row::Progress => (
                t!("Progress line", "İlerleme çizgisi"),
                t!("A thin line under the song; click it to seek", "Şarkının altında ince bir çizgi; tıklayıp atlayabilirsin"),
            ),
            Row::Others => (
                t!("Other players too", "Diğer oynatıcılar da"),
                t!(
                    "When Spotify isn't open, show what the browser or another app is playing",
                    "Spotify açık değilken tarayıcının ya da başka bir uygulamanın çaldığını göster"
                ),
            ),
            Row::HideIdle => (
                t!("Hide when nothing plays", "Bir şey çalmıyorken gizle"),
                t!("Otherwise it waits with a shortcut that opens Spotify", "Kapalıysa Spotify'ı açan bir kısayolla bekler"),
            ),
        }
    }

    pub fn paint(&self, g: &Gfx) {
        let s = &self.settings;
        let m = &self.music.settings;
        let n = &self.music.now;
        let head = if m.show && n.active {
            let who = if n.artist.is_empty() { n.title.clone() } else { format!("{} · {}", n.title, n.artist) };
            format!("{} · {who}", t!("Dock on", "Dock açık"))
        } else {
            t!("Dock on · always visible, maximized windows end above it", "Dock açık · hep görünür, büyütülen pencereler onun üstünde biter")
                .to_string()
        };
        status(g, self.head_x + 16.0, HEAD_CY, pal().green, &head, pal().muted, self.head_r - 8.0);

        g.clip(Rect::new(0.0, HEADER, self.w, self.h), || {
            // Müzik bölümünün başlığı.
            let first = self.row(Row::Music);
            let title = Rect::new(PAD, first.t - SECTION + 16.0, self.w - PAD, first.t - 8.0);
            g.text(t!("Music", "Müzik"), &g.f.strong, title, pal().text);

            for row in ROWS {
                let r = self.row(row);
                let (title, sub) = Self::texts(row);
                let enabled = self.enabled(row);
                match row {
                    Row::Style => {
                        let seg = self.seg_rects(g);
                        setting_row(g, r, title, sub, r.r - seg[0].l + 16.0);
                        g.fill(Rect::new(seg[0].l - 3.0, seg[0].t - 3.0, seg[2].r + 3.0, seg[0].b + 3.0), 6.0, pal().hover);
                        let current = music::Style::ALL.iter().position(|&x| x == m.style).unwrap_or(0);
                        for (i, (sr, label)) in seg.iter().zip(Self::style_labels()).enumerate() {
                            let c = if i == current && enabled {
                                g.fill(*sr, 4.0, accent());
                                on_accent()
                            } else if self.hover == Hit::Style(i) {
                                g.fill(*sr, 4.0, pal().sel);
                                pal().text
                            } else if enabled {
                                pal().muted
                            } else {
                                pal().faint
                            };
                            g.text(label, &g.f.button, *sr, c);
                        }
                    }
                    Row::Size | Row::Opacity => {
                        setting_row(g, r, title, sub, 260.0);
                        let (a, b) = self.slider_x(row);
                        let (v, label) = if row == Row::Size {
                            let (lo, hi, _) = SIZES;
                            ((s.size - lo) as f32 / (hi - lo) as f32, s.size.to_string())
                        } else {
                            ((s.opacity as f32 - 30.0) / 70.0, format!("%{}", s.opacity))
                        };
                        let active = enabled && (self.dragging == Some(row) || self.hover == Hit::Slider(row));
                        slider(g, a, b, r.cy(), v, accent(), active);
                        let c = if enabled { pal().muted } else { pal().faint };
                        g.text(&label, &g.f.small_right, Rect::new(b + 8.0, r.t, r.r, r.b), c);
                    }
                    _ => {
                        let on = match row {
                            Row::CornerTl => s.corner_tl,
                            Row::CornerBr => s.corner_br,
                            Row::Lock => s.locked,
                            Row::Music => m.show,
                            Row::Progress => m.progress,
                            Row::Others => m.others,
                            _ => m.hide_idle,
                        };
                        setting_row(g, r, title, sub, 120.0);
                        toggle(g, toggle_rect(r.r, r.cy()), on, accent(), enabled);
                    }
                }
            }
        });
    }
}

impl ToolPage for Dock {
    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn paint(&self, g: &Gfx) {
        Dock::paint(self, g)
    }

    fn message(
        &mut self,
        _g: &mut Gfx,
        msg: u32,
        _wp: windows::Win32::Foundation::WPARAM,
        _lp: windows::Win32::Foundation::LPARAM,
    ) -> Option<windows::Win32::Foundation::LRESULT> {
        if msg != music::WM_MEDIA {
            return None;
        }
        self.music.media_changed();
        self.redraw();
        Some(windows::Win32::Foundation::LRESULT(0))
    }

    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        if let Some(row) = self.dragging {
            self.set_slider_from(row, x);
            return;
        }
        let hit = self.hit(g, x, y);
        if hit != self.hover {
            self.hover = hit;
            self.redraw();
        }
    }

    fn mouse_leave(&mut self) {
        if self.hover != Hit::None && self.dragging.is_none() {
            self.hover = Hit::None;
            self.redraw();
        }
    }

    fn mouse_down(&mut self, g: &Gfx, x: f32, y: f32) {
        self.pressed = self.hit(g, x, y);
        if let Hit::Slider(row) = self.pressed {
            self.dragging = Some(row);
            self.set_slider_from(row, x);
        }
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        let pressed = std::mem::replace(&mut self.pressed, Hit::None);
        if let Some(row) = self.dragging.take() {
            self.set_slider_from(row, x);
            self.commit();
            return;
        }
        if pressed != self.hit(g, x, y) {
            return;
        }
        let (s, m) = (&mut self.settings, &mut self.music.settings);
        match pressed {
            Hit::Toggle(Row::CornerTl) => s.corner_tl = !s.corner_tl,
            Hit::Toggle(Row::CornerBr) => s.corner_br = !s.corner_br,
            Hit::Toggle(Row::Lock) => s.locked = !s.locked,
            Hit::Toggle(row) => {
                match row {
                    Row::Music => m.show = !m.show,
                    Row::Progress => m.progress = !m.progress,
                    Row::Others => m.others = !m.others,
                    _ => m.hide_idle = !m.hide_idle,
                }
                self.music.commit();
                self.redraw();
                return;
            }
            Hit::Style(i) => {
                m.style = music::Style::ALL[i];
                self.music.commit();
                self.redraw();
                return;
            }
            Hit::Slider(_) | Hit::None => return,
        }
        self.commit();
    }

    fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        self.scroll = (self.scroll - delta * 60.0).clamp(0.0, self.max_scroll());
        self.hover = self.hit(g, x, y);
        self.redraw();
    }

    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        self.hit(g, x, y) != Hit::None
    }

    fn set_visible(&mut self, visible: bool) {
        if !visible {
            self.hover = Hit::None;
            self.dragging = None;
        }
    }
}

impl Drop for Dock {
    fn drop(&mut self) {
        bar::stop();
        taskbar::restore(false);
    }
}
