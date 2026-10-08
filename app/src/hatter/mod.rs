//! hatter: Windows'un görev çubuğunun yerine ekranın altında yüzen bir dock. Tek liste:
//! sabitlenenler (uygulama, klasör, ne sabitlenirse), yanlarında açık olanlar; sağ uçta ağ, ses,
//! pil ve saat. Masaüstü simgeleri de istenirse gizlenir.
//!
//! Motor hive sürecinin içinde (dock.rs). Kurmak ayar dosyasını ve dock'un ilk listesini
//! (görev çubuğuna sabitlenmiş kısayollar) yazar.
//! Kaldırınca (ya da hive kapanınca) görev çubuğu ve masaüstü simgeleri eski haline döner.

mod apps;
mod dock;
mod menu;
mod stack;
mod status;
mod keys;
mod taskbar;
pub(crate) mod theme;
mod toasts;
mod tray;

use std::path::PathBuf;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::InvalidateRect;

use crate::gfx::{Gfx, Rect};
use crate::ui::*;
use crate::util;

pub const ACCENT: u32 = 0xfb923c;

const ROW: f32 = 68.0;
/// Simge boyu kaydırıcısının satırı (anahtarlardan sonra).
const SIZE_ROW: usize = 3;
const SIZES: (u32, u32, u32) = (36, 64, 4);

pub fn dir() -> PathBuf {
    util::data_dir().join("hatter")
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
    /// Pencere üstüne gelince saklan; kapalıysa ekranın altında yer ayırır.
    pub autohide: bool,
    /// Simge boyu (DIP).
    pub size: u32,
    /// Etkin köşeler: sol üst görev görünümü, sağ alt masaüstü.
    pub corner_tl: bool,
    pub corner_br: bool,
}

impl Settings {
    fn load() -> Self {
        let mut s = Settings { autohide: true, size: 48, corner_tl: true, corner_br: true };
        let text = std::fs::read_to_string(ini()).unwrap_or_default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "gizle" => s.autohide = v == "1",
                "kose_sol_ust" => s.corner_tl = v == "1",
                "kose_sag_alt" => s.corner_br = v == "1",
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
                "gizle={}\nboyut={}\nkose_sol_ust={}\nkose_sag_alt={}\n",
                b(self.autohide),
                self.size,
                b(self.corner_tl),
                b(self.corner_br)
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
        crate::log!("hatter: {} yazılamadı: {e}", pins_file().display());
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
            set("command", None, &format!("\"{exe}\" --hatter-pin \"%1\""));
        }
    }
}

/// `hive --hatter-pin <yol>`: sağ tık menüsünden. Yol dock'un sonuna eklenir, açık dock listeyi
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
    dock::notify_reload();
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

/// `hive --hatter-test`
pub fn probe() -> String {
    let pins = if pins_file().exists() { load_pins() } else { seed() };
    apps::probe(&pins)
}

// --- Sayfa ---

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    None,
    Toggle(usize),
    /// Dock modu: `true` akıllı gizle, `false` hep görünür.
    Mode(bool),
    Size,
}

pub struct Hatter {
    hwnd: HWND,
    settings: Settings,
    hover: Hit,
    pressed: Hit,
    /// Boyut kaydırıcısı sürükleniyor (bırakınca uygulanır).
    dragging: bool,
    w: f32,
    h: f32,
    head_x: f32,
    head_r: f32,
}

impl Hatter {
    pub fn start(hwnd: HWND) -> Self {
        let settings = Settings::load();
        shell_verbs(true);
        taskbar::hide();
        dock::start(settings, load_pins());
        Self { hwnd, settings, hover: Hit::None, pressed: Hit::None, dragging: false, w: 0.0, h: 0.0, head_x: 0.0, head_r: 0.0 }
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn row(&self, i: usize) -> Rect {
        let t = HEADER + 12.0 + i as f32 * ROW;
        Rect::new(PAD, t, self.w - PAD, t + ROW)
    }

    fn slider_x(&self) -> (f32, f32) {
        let r = self.row(SIZE_ROW);
        (r.r - 220.0, r.r - 52.0)
    }

    /// Mod seçicisinin iki parçası: [hep görünür, akıllı gizle].
    fn mode_rects(g: &Gfx, r: Rect) -> [Rect; 2] {
        let labels = [t!("Always visible", "Hep görünür"), t!("Smart hide", "Akıllı gizle")];
        let w = labels.iter().map(|l| g.measure(l, &g.f.button)).fold(0.0, f32::max) + 28.0;
        let (t, b) = (r.cy() - 15.0, r.cy() + 15.0);
        [Rect::new(r.r - 2.0 * w - 3.0, t, r.r - w - 3.0, b), Rect::new(r.r - w, t, r.r, b)]
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        let [a, b] = Self::mode_rects(g, self.row(0));
        if a.contains(x, y) {
            return Hit::Mode(false);
        }
        if b.contains(x, y) {
            return Hit::Mode(true);
        }
        for i in [1, 2] {
            let r = self.row(i);
            if toggle_rect(r.r, r.cy()).contains(x, y) || (r.contains(x, y) && x > r.r - 120.0) {
                return Hit::Toggle(i);
            }
        }
        let r = self.row(SIZE_ROW);
        let (a, b) = self.slider_x();
        if Rect::new(a - 10.0, r.t + 14.0, b + 10.0, r.b - 14.0).contains(x, y) {
            return Hit::Size;
        }
        Hit::None
    }

    fn set_size_from(&mut self, x: f32) {
        let (a, b) = self.slider_x();
        let (lo, hi, step) = SIZES;
        let v = lo as f32 + ((x - a) / (b - a)).clamp(0.0, 1.0) * (hi - lo) as f32;
        let v = ((v / step as f32).round() as u32 * step).clamp(lo, hi);
        if v != self.settings.size {
            self.settings.size = v;
            self.redraw();
        }
    }

    fn commit(&mut self) {
        if let Err(e) = self.settings.save() {
            crate::log!("hatter: ayarlar yazılamadı: {e}");
        }
        dock::apply(self.settings);
        self.redraw();
    }

    pub fn paint(&self, g: &Gfx) {
        let s = &self.settings;
        let head = if s.autohide {
            t!("Dock on · hides when a window covers it", "Dock açık · pencere gelince saklanır")
        } else {
            t!("Dock on · always visible", "Dock açık · hep görünür")
        };
        status(g, self.head_x + 16.0, HEAD_CY, GREEN, head, MUTED, self.head_r - 8.0);

        g.clip(Rect::new(0.0, HEADER, self.w, self.h), || {
            let rows: [(&str, &str, bool); 3] = [
                (
                    "Dock",
                    if s.autohide {
                        t!(
                            "Slides away when a window covers it · touch the bottom edge to bring it back",
                            "Pencere üstüne gelince aşağı kayar · ekranın altına dokununca geri gelir"
                        )
                    } else {
                        t!(
                            "Always on screen like a Mac · maximized windows end above it",
                            "Mac gibi hep ekranda · büyütülen pencereler onun üstünde biter"
                        )
                    },
                    s.autohide,
                ),
                (
                    t!("Top-left corner", "Sol üst köşe"),
                    t!(
                        "Push the cursor into the corner to see all windows (Win+Tab)",
                        "İmleci köşeye götürünce bütün pencereler görünür (Win+Tab)"
                    ),
                    s.corner_tl,
                ),
                (
                    t!("Bottom-right corner", "Sağ alt köşe"),
                    t!(
                        "Push the cursor into the corner to show the desktop, again to come back (Win+D)",
                        "İmleci köşeye götürünce masaüstü, tekrar götürünce geri döner (Win+D)"
                    ),
                    s.corner_br,
                ),
            ];
            for (i, (title, sub, on)) in rows.iter().enumerate() {
                let r = self.row(i);
                if i == 0 {
                    let seg = Self::mode_rects(g, r);
                    setting_row(g, r, title, sub, r.r - seg[0].l + 16.0);
                    g.fill(Rect::new(seg[0].l - 3.0, seg[0].t - 3.0, seg[1].r + 3.0, seg[1].b + 3.0), 6.0, HOVER);
                    let labels = [t!("Always visible", "Hep görünür"), t!("Smart hide", "Akıllı gizle")];
                    for (j, (sr, label)) in seg.iter().zip(labels).enumerate() {
                        let selected = s.autohide == (j == 1);
                        let c = if selected {
                            g.fill(*sr, 4.0, accent());
                            on_accent()
                        } else if self.hover == Hit::Mode(j == 1) {
                            g.fill(*sr, 4.0, SEL);
                            TEXT
                        } else {
                            MUTED
                        };
                        g.text(label, &g.f.button, *sr, c);
                    }
                    continue;
                }
                setting_row(g, r, title, sub, 120.0);
                toggle(g, toggle_rect(r.r, r.cy()), *on, accent(), true);
            }

            let r = self.row(SIZE_ROW);
            setting_row(g, r, t!("Icon size", "Simge boyu"), "", 240.0);
            let (a, b) = self.slider_x();
            let (lo, hi, _) = SIZES;
            let v = (s.size - lo) as f32 / (hi - lo) as f32;
            slider(g, a, b, r.cy(), v, accent(), self.dragging || self.hover == Hit::Size);
            g.text(&s.size.to_string(), &g.f.small_right, Rect::new(b + 8.0, r.t, r.r, r.b), MUTED);
        });
    }
}

impl ToolPage for Hatter {
    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
    }

    fn paint(&self, g: &Gfx) {
        Hatter::paint(self, g)
    }

    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        if self.dragging {
            self.set_size_from(x);
            return;
        }
        let hit = self.hit(g, x, y);
        if hit != self.hover {
            self.hover = hit;
            self.redraw();
        }
    }

    fn mouse_leave(&mut self) {
        if self.hover != Hit::None && !self.dragging {
            self.hover = Hit::None;
            self.redraw();
        }
    }

    fn mouse_down(&mut self, g: &Gfx, x: f32, y: f32) {
        self.pressed = self.hit(g, x, y);
        if self.pressed == Hit::Size {
            self.dragging = true;
            self.set_size_from(x);
        }
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        let pressed = std::mem::replace(&mut self.pressed, Hit::None);
        if std::mem::take(&mut self.dragging) {
            self.set_size_from(x);
            self.commit();
            return;
        }
        if pressed != self.hit(g, x, y) {
            return;
        }
        if let Hit::Mode(auto) = pressed {
            if self.settings.autohide != auto {
                self.settings.autohide = auto;
                self.commit();
            }
            return;
        }
        if let Hit::Toggle(i) = pressed {
            let s = &mut self.settings;
            match i {
                1 => s.corner_tl = !s.corner_tl,
                _ => s.corner_br = !s.corner_br,
            }
            self.commit();
        }
    }

    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        self.hit(g, x, y) != Hit::None
    }

    fn set_visible(&mut self, visible: bool) {
        if !visible {
            self.hover = Hit::None;
            self.dragging = false;
        }
    }
}

impl Drop for Hatter {
    fn drop(&mut self) {
        dock::stop();
        taskbar::restore(false);
    }
}
