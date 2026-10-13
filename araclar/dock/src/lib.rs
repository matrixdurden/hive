//! dock: Windows'un görev çubuğunun yerine ekranın altında yüzen bir dock. Tek liste:
//! sabitlenenler (uygulama, klasör, ne sabitlenirse), yanlarında açık olanlar; sağ uçta ağ, ses,
//! pil ve saat. Masaüstü simgeleri de istenirse gizlenir.
//!
//! Dock'un solunda çalan şarkının şeridi (music/): Spotify, kapak, düğmeler.
//!
//! Motor winloom sürecinin içinde (bar.rs). Kurmak ayar dosyasını ve dock'un ilk listesini
//! (görev çubuğuna sabitlenmiş kısayollar) yazar.
//! Kaldırınca (ya da winloom kapanınca) görev çubuğu ve masaüstü simgeleri eski haline döner.

#[macro_use]
extern crate ortak;

mod tool;
pub use tool::TOOL;

mod apps;
mod bar;
pub mod jump;
mod menu;
pub mod music;
pub mod stack;
mod status;
mod keys;
mod taskbar;
mod toasts;
mod tray;

use std::path::PathBuf;

pub use bar::{Geometry, bench, geometry, window};

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::InvalidateRect;

use ortak::form::{self, Ctl, Form, Item, Row};
use ortak::gfx::{Gfx, Rect};
use ortak::ui::*;
use ortak::{theme, util};

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
        ortak::log!("dock: {} yazılamadı: {e}", pins_file().display());
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

/// `winloom --dock-pin <yol>`: sağ tık menüsünden. Yol dock'un sonuna eklenir, açık dock listeyi
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

/// `winloom --dock-test`
pub fn probe() -> String {
    let pins = if pins_file().exists() { load_pins() } else { seed() };
    apps::probe(&pins)
}

// --- Sayfa ---

/// Sayfadaki satırların kimlikleri.
const R_SIZE: u32 = 0;
const R_OPACITY: u32 = 1;
const R_LOCK: u32 = 2;
const R_CORNER_TL: u32 = 3;
const R_CORNER_BR: u32 = 4;
const R_MUSIC: u32 = 5;
const R_STYLE: u32 = 6;
const R_PROGRESS: u32 = 7;
const R_OTHERS: u32 = 8;
const R_HIDE_IDLE: u32 = 9;

pub struct Dock {
    hwnd: HWND,
    settings: Settings,
    music: music::Music,
    hover: form::Hit,
    pressed: form::Hit,
    /// Sürüklenen kaydırıcının satırı (bırakınca kaydedilir).
    dragging: Option<u32>,
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
            hover: form::Hit::None,
            pressed: form::Hit::None,
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

    /// Müzik kapalıyken altındaki ayarlar sönük durur ve tıklanmaz.
    fn enabled(&self, id: u32) -> bool {
        !matches!(id, R_STYLE | R_PROGRESS | R_OTHERS | R_HIDE_IDLE) || self.music.settings.show
    }

    fn items(&self) -> Vec<Item> {
        let s = &self.settings;
        let m = &self.music.settings;
        let (lo, hi, _) = SIZES;
        let row = |id: u32, title: &str, sub: &str, ctl: Ctl| Item::Row(Row::new(id, title, sub, ctl).dim(!self.enabled(id)));
        let style = music::Style::ALL.iter().position(|&x| x == m.style).unwrap_or(0);
        vec![
            Item::Section("Dock".into()),
            row(
                R_SIZE,
                t!("Icon size", "Simge boyu"),
                "",
                Ctl::Slider((s.size - lo) as f32 / (hi - lo) as f32, s.size.to_string()),
            ),
            row(
                R_OPACITY,
                t!("Opacity", "Saydamlık"),
                t!("Of the dock and the music strip; icons and text stay sharp", "Dock'un ve müzik şeridinin; simgeler ve yazılar hep net"),
                Ctl::Slider((s.opacity as f32 - 30.0) / 70.0, format!("%{}", s.opacity)),
            ),
            row(
                R_LOCK,
                t!("Lock icons", "Simgeleri kilitle"),
                t!(
                    "Icons can't be moved by dragging; otherwise hold an icon a moment to move it",
                    "Simgeler sürüklenerek yer değiştirmez; kapalıyken simgeyi bir an basılı tutup taşırsın"
                ),
                Ctl::Toggle(s.locked),
            ),
            Item::Section(t!("Corners", "Köşeler").into()),
            row(
                R_CORNER_TL,
                t!("Top-left corner", "Sol üst köşe"),
                t!(
                    "Throw the cursor into the corner to see all windows (Win+Tab)",
                    "İmleci köşeye sertçe götürünce bütün pencereler görünür (Win+Tab)"
                ),
                Ctl::Toggle(s.corner_tl),
            ),
            row(
                R_CORNER_BR,
                t!("Bottom-right corner", "Sağ alt köşe"),
                t!(
                    "Throw the cursor into the corner for the desktop, again to come back (Win+D)",
                    "İmleci köşeye sertçe götürünce masaüstü, tekrar götürünce geri döner (Win+D)"
                ),
                Ctl::Toggle(s.corner_br),
            ),
            Item::Section(t!("Music strip", "Müzik şeridi").into()),
            row(
                R_MUSIC,
                t!("Now playing", "Çalan şarkı"),
                t!("Spotify left of the dock; click it to bring Spotify forward", "Dock'un solunda Spotify; tıklayınca Spotify öne gelir"),
                Ctl::Toggle(m.show),
            ),
            row(
                R_STYLE,
                t!("Background", "Arka plan"),
                t!("The cover blurred, its color, or plain like the dock", "Kapağın bulanık hali, rengi ya da dock gibi düz"),
                Ctl::Seg(
                    vec![t!("Cover", "Kapak").into(), t!("Color", "Renk").into(), t!("Like the dock", "Dock gibi").into()],
                    style,
                ),
            ),
            row(
                R_PROGRESS,
                t!("Progress line", "İlerleme çizgisi"),
                t!("A thin line under the song; click it to seek", "Şarkının altında ince bir çizgi; tıklayıp atlayabilirsin"),
                Ctl::Toggle(m.progress),
            ),
            row(
                R_OTHERS,
                t!("Other players too", "Diğer oynatıcılar da"),
                t!(
                    "When Spotify isn't open, show what the browser or another app is playing",
                    "Spotify açık değilken tarayıcının ya da başka bir uygulamanın çaldığını göster"
                ),
                Ctl::Toggle(m.others),
            ),
            row(
                R_HIDE_IDLE,
                t!("Hide when nothing plays", "Bir şey çalmıyorken gizle"),
                t!("Otherwise it waits with a shortcut that opens Spotify", "Kapalıysa Spotify'ı açan bir kısayolla bekler"),
                Ctl::Toggle(m.hide_idle),
            ),
        ]
    }

    fn form(&self) -> Form {
        Form::new(self.items(), PAD, self.w - PAD, HEADER + 16.0 - self.scroll)
    }

    fn max_scroll(&self) -> f32 {
        (self.form().bottom() - 24.0 + self.scroll + 12.0 - self.h).max(0.0)
    }

    /// Başlık bandının altında, sönük satırlar hariç.
    fn hit(&self, g: &Gfx, x: f32, y: f32) -> form::Hit {
        if y < HEADER {
            return form::Hit::None;
        }
        let hit = self.form().hit(g, x, y);
        if hit.id().is_some_and(|id| self.enabled(id)) { hit } else { form::Hit::None }
    }

    fn set_slider(&mut self, id: u32, x: f32) {
        let Some(t) = self.form().slider_value(id, x) else { return };
        if id == R_SIZE {
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
            ortak::log!("dock: ayarlar yazılamadı: {e}");
        }
        bar::apply(self.settings);
        self.redraw();
    }

    fn click(&mut self, hit: form::Hit) {
        let (s, m) = (&mut self.settings, &mut self.music.settings);
        match hit {
            form::Hit::Toggle(R_CORNER_TL) => s.corner_tl = !s.corner_tl,
            form::Hit::Toggle(R_CORNER_BR) => s.corner_br = !s.corner_br,
            form::Hit::Toggle(R_LOCK) => s.locked = !s.locked,
            form::Hit::Toggle(id @ (R_MUSIC | R_PROGRESS | R_OTHERS | R_HIDE_IDLE)) => {
                let v = match id {
                    R_MUSIC => &mut m.show,
                    R_PROGRESS => &mut m.progress,
                    R_OTHERS => &mut m.others,
                    _ => &mut m.hide_idle,
                };
                *v = !*v;
                self.music.commit();
                self.redraw();
                return;
            }
            form::Hit::Seg(R_STYLE, k) => {
                m.style = music::Style::ALL[k];
                self.music.commit();
                self.redraw();
                return;
            }
            _ => return,
        }
        self.commit();
    }

    pub fn paint(&self, g: &Gfx) {
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
        let form = self.form();
        g.clip(Rect::new(0.0, HEADER, self.w, self.h), || form.paint(g, self.hover, self.dragging));
    }
}

impl ToolPage for Dock {
    fn summary(&self) -> Option<(String, String)> {
        let n = &self.music.now;
        if self.music.settings.show && n.active {
            return Some((n.title.clone(), n.artist.clone()));
        }
        Some((t!("On", "Açık").into(), t!("In place of the taskbar", "Görev çubuğunun yerinde").into()))
    }

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
        if let Some(id) = self.dragging {
            self.set_slider(id, x);
            return;
        }
        let hit = self.hit(g, x, y);
        if hit != self.hover {
            // Kaydırıcının üstünde gezinmek yalnız imlecin değerini değiştirir: çizim aynı.
            let same = matches!(hit, form::Hit::Slider(..)) && hit.same(self.hover);
            self.hover = hit;
            if !same {
                self.redraw();
            }
        }
    }

    fn mouse_leave(&mut self) {
        if self.hover != form::Hit::None && self.dragging.is_none() {
            self.hover = form::Hit::None;
            self.redraw();
        }
    }

    fn mouse_down(&mut self, g: &Gfx, x: f32, y: f32) {
        self.pressed = self.hit(g, x, y);
        if let form::Hit::Slider(id, _) = self.pressed {
            self.dragging = Some(id);
            self.set_slider(id, x);
        }
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        let pressed = std::mem::replace(&mut self.pressed, form::Hit::None);
        if let Some(id) = self.dragging.take() {
            self.set_slider(id, x);
            self.commit();
            return;
        }
        let hit = self.hit(g, x, y);
        if pressed.same(hit) {
            self.click(hit);
        }
    }

    fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        self.scroll = (self.scroll - delta * 60.0).clamp(0.0, self.max_scroll());
        self.hover = self.hit(g, x, y);
        self.redraw();
    }

    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        self.hit(g, x, y) != form::Hit::None
    }

    fn set_visible(&mut self, visible: bool) {
        if !visible {
            self.hover = form::Hit::None;
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
