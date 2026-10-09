//! Müzik: Spotify'da çalan şarkı, dock'un solunda dock boyunda bir şerit. Kapak, şarkı,
//! sanatçı, önceki / çal / sonraki ve ilerleme; istenirse başka oynatıcılar da.
//!
//! Motor hive sürecinin içinde: medya iş parçacığı (media.rs) Windows'un ortak medya
//! denetimini izler, widget (widget.rs) arayüz iş parçacığında kendi penceresinde çizer.
//! Açmak yalnızca ayar dosyasını yazmaktır.

mod look;
mod media;
mod widget;

use std::path::PathBuf;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::InvalidateRect;

use crate::gfx::{Color, Gfx, Rect};
use crate::ui::*;
use crate::util;

pub use media::WM_MEDIA;
use media::{Media, Now};
use widget::Style;

const ROW: f32 = 64.0;

/// Sayfanın satırları, yukarıdan aşağı.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Row {
    Style,
    Opacity,
    Progress,
    Others,
    HideIdle,
}

const ROWS: [Row; 5] = [Row::Style, Row::Opacity, Row::Progress, Row::Others, Row::HideIdle];

pub fn dir() -> PathBuf {
    util::data_dir().join("music")
}

fn ini() -> PathBuf {
    dir().join("ayarlar.ini")
}

pub fn installed() -> bool {
    dir().is_dir()
}

#[derive(Clone, PartialEq)]
struct Settings {
    style: Style,
    /// Arka planın opaklığı, yüzde (30..100).
    opacity: u32,
    progress: bool,
    /// Spotify çalmıyorken başka oynatıcılar (tarayıcı, ...) da görünsün.
    others: bool,
    /// Hiçbir şey açık değilken widget gizlensin.
    hide_idle: bool,
    /// Son görülen Spotify'ın uygulama kimliği (masaüstü, Store ya da tarayıcı uygulaması).
    app: String,
}

impl Settings {
    fn load() -> Self {
        let mut s =
            Settings { style: Style::Cover, opacity: 92, progress: true, others: false, hide_idle: false, app: String::new() };
        let text = std::fs::read_to_string(ini()).unwrap_or_default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "arka_plan" => s.style = Style::from_id(v).unwrap_or(s.style),
                "opaklik" => s.opacity = v.parse::<u32>().unwrap_or(92).clamp(30, 100),
                "ilerleme" => s.progress = v == "1",
                "diger" => s.others = v == "1",
                "bosken_gizle" => s.hide_idle = v == "1",
                "uygulama" => s.app = v.to_string(),
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
                "arka_plan={}\nopaklik={}\nilerleme={}\ndiger={}\nbosken_gizle={}\nuygulama={}\n",
                self.style.id(),
                self.opacity,
                b(self.progress),
                b(self.others),
                b(self.hide_idle),
                self.app,
            ),
        )
    }

    fn options(&self) -> widget::Options {
        widget::Options {
            hide_idle: self.hide_idle,
            app: self.app.clone(),
            style: self.style,
            opacity: self.opacity as f32 / 100.0,
            progress: self.progress,
        }
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

/// `hive --music-test [klasör]`: medya oturumlarını yazdırır; klasör verilirse widget'ı her arka
/// planla pencere açmadan PNG'ye çizer.
pub fn probe(out: Option<&str>) -> String {
    let (mut s, now) = media::probe();
    if let Some(dir) = out {
        for (style, hover) in Style::ALL.into_iter().map(|st| (st, false)).chain([(Style::Cover, true)]) {
            let name = format!("muzik-{}{}.png", style.id(), if hover { "-uzerinde" } else { "" });
            let path = std::path::Path::new(dir).join(name);
            match widget::preview(&path, style, hover, now.clone()) {
                Ok(()) => s += &format!("önizleme: {}\n", path.display()),
                Err(e) => s += &format!("önizleme çizilemedi: {e}\n"),
            }
        }
    }
    s
}

// --- Sayfa ---

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    None,
    Toggle(Row),
    /// Arka plan seçicisinin `n`. parçası.
    Style(usize),
    Opacity,
}

pub struct Music {
    hwnd: HWND,
    settings: Settings,
    media: Media,
    now: Now,
    hover: Hit,
    pressed: Hit,
    /// Saydamlık kaydırıcısı sürükleniyor.
    dragging: bool,
    w: f32,
    h: f32,
    head_x: f32,
    head_r: f32,
}

impl Music {
    pub fn start(hwnd: HWND) -> Self {
        let settings = Settings::load();
        let media = Media::start(hwnd, settings.others);
        let m = Self {
            hwnd,
            settings,
            media,
            now: Now::default(),
            hover: Hit::None,
            pressed: Hit::None,
            dragging: false,
            w: 0.0,
            h: 0.0,
            head_x: 0.0,
            head_r: 0.0,
        };
        widget::start(m.media.remote(), m.settings.options(), m.now.clone());
        m
    }

    /// WM_MEDIA: çalan şarkı değişti.
    pub fn media_changed(&mut self) {
        self.now = self.media.now();
        if self.now.spotify && self.now.app != self.settings.app {
            self.settings.app = self.now.app.clone();
            let _ = self.settings.save();
        }
        widget::update(self.now.clone());
        self.redraw();
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn row(&self, r: Row) -> Rect {
        let i = ROWS.iter().position(|&x| x == r).unwrap_or(0);
        let t = HEADER + 12.0 + i as f32 * ROW;
        Rect::new(PAD, t, self.w - PAD, t + ROW)
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

    fn slider_x(&self) -> (f32, f32) {
        let r = self.row(Row::Opacity);
        (r.r - 220.0, r.r - 52.0)
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        for row in ROWS {
            let r = self.row(row);
            if !r.contains(x, y) {
                continue;
            }
            return match row {
                Row::Style => self.seg_rects(g).iter().position(|s| s.contains(x, y)).map_or(Hit::None, Hit::Style),
                Row::Opacity => {
                    let (a, b) = self.slider_x();
                    if x >= a - 10.0 && x <= b + 10.0 { Hit::Opacity } else { Hit::None }
                }
                _ if x > r.r - 120.0 => Hit::Toggle(row),
                _ => Hit::None,
            };
        }
        Hit::None
    }

    fn set_opacity_from(&mut self, x: f32) {
        let (a, b) = self.slider_x();
        let v = (30.0 + ((x - a) / (b - a)).clamp(0.0, 1.0) * 70.0).round() as u32;
        if v != self.settings.opacity {
            self.settings.opacity = v;
            // Sürüklerken widget da canlı değişsin (dosyaya bırakınca yazılır).
            widget::start(self.media.remote(), self.settings.options(), self.now.clone());
            self.redraw();
        }
    }

    fn commit(&mut self) {
        if let Err(e) = self.settings.save() {
            crate::log!("müzik: ayarlar yazılamadı: {e}");
        }
        self.media.set_others(self.settings.others);
        widget::start(self.media.remote(), self.settings.options(), self.now.clone());
        self.redraw();
    }

    fn head(&self) -> (Color, String) {
        if crate::dock::geometry().is_none() {
            return (pal().faint, t!("Shows up next to the dock: turn the dock on", "Dock'un yanında durur: dock'u aç").into());
        }
        let n = &self.now;
        if !n.active {
            return (pal().faint, t!("Nothing playing", "Bir şey çalmıyor").into());
        }
        let who = if n.artist.is_empty() { n.title.clone() } else { format!("{} · {}", n.title, n.artist) };
        if n.playing { (pal().green, who) } else { (pal().faint, format!("{} · {who}", t!("Paused", "Duraklatıldı"))) }
    }

    fn texts(row: Row) -> (&'static str, &'static str) {
        match row {
            Row::Style => (t!("Background", "Arka plan"), t!("The cover blurred, its color, or plain like the dock", "Kapağın bulanık hali, rengi ya da dock gibi düz")),
            Row::Opacity => (t!("Opacity", "Saydamlık"), t!("Of the background; text stays sharp", "Arka planın; yazılar hep net")),
            Row::Progress => (t!("Progress line", "İlerleme çizgisi"), t!("A thin line under the song; click it to seek", "Şarkının altında ince bir çizgi; tıklayıp atlayabilirsin")),
            Row::Others => (
                t!("Other players too", "Diğer oynatıcılar da"),
                t!("When Spotify isn't open, show what the browser or another app is playing", "Spotify açık değilken tarayıcının ya da başka bir uygulamanın çaldığını göster"),
            ),
            Row::HideIdle => (
                t!("Hide when nothing plays", "Bir şey çalmıyorken gizle"),
                t!("Otherwise it waits with a shortcut that opens Spotify", "Kapalıysa Spotify'ı açan bir kısayolla bekler"),
            ),
        }
    }
}

impl ToolPage for Music {
    fn message(
        &mut self,
        _g: &mut Gfx,
        msg: u32,
        _wp: windows::Win32::Foundation::WPARAM,
        _lp: windows::Win32::Foundation::LPARAM,
    ) -> Option<windows::Win32::Foundation::LRESULT> {
        if msg != WM_MEDIA {
            return None;
        }
        self.media_changed();
        Some(windows::Win32::Foundation::LRESULT(0))
    }

    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
    }

    fn paint(&self, g: &Gfx) {
        let (dot, head) = self.head();
        status(g, self.head_x + 16.0, HEAD_CY, dot, &head, pal().muted, self.head_r - 8.0);
        let s = &self.settings;
        g.clip(Rect::new(0.0, HEADER, self.w, self.h), || {
            for row in ROWS {
                let r = self.row(row);
                let (title, sub) = Self::texts(row);
                match row {
                    Row::Style => {
                        let seg = self.seg_rects(g);
                        setting_row(g, r, title, sub, r.r - seg[0].l + 16.0);
                        g.fill(Rect::new(seg[0].l - 3.0, seg[0].t - 3.0, seg[2].r + 3.0, seg[0].b + 3.0), 6.0, pal().hover);
                        let current = Style::ALL.iter().position(|&x| x == s.style).unwrap_or(0);
                        for (i, (sr, label)) in seg.iter().zip(Self::style_labels()).enumerate() {
                            let c = if i == current {
                                g.fill(*sr, 4.0, accent());
                                on_accent()
                            } else if self.hover == Hit::Style(i) {
                                g.fill(*sr, 4.0, pal().sel);
                                pal().text
                            } else {
                                pal().muted
                            };
                            g.text(label, &g.f.button, *sr, c);
                        }
                    }
                    Row::Opacity => {
                        setting_row(g, r, title, sub, 260.0);
                        let (a, b) = self.slider_x();
                        let v = (s.opacity as f32 - 30.0) / 70.0;
                        slider(g, a, b, r.cy(), v, accent(), self.dragging || self.hover == Hit::Opacity);
                        g.text(&format!("%{}", s.opacity), &g.f.small_right, Rect::new(b + 8.0, r.t, r.r, r.b), pal().muted);
                    }
                    _ => {
                        let on = match row {
                            Row::Progress => s.progress,
                            Row::Others => s.others,
                            _ => s.hide_idle,
                        };
                        setting_row(g, r, title, sub, 120.0);
                        toggle(g, toggle_rect(r.r, r.cy()), on, accent(), true);
                    }
                }
            }
        });
    }

    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        if self.dragging {
            self.set_opacity_from(x);
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
        if self.pressed == Hit::Opacity {
            self.dragging = true;
            self.set_opacity_from(x);
        }
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        let pressed = std::mem::replace(&mut self.pressed, Hit::None);
        if std::mem::take(&mut self.dragging) {
            self.set_opacity_from(x);
            self.commit();
            return;
        }
        if pressed != self.hit(g, x, y) {
            return;
        }
        let s = &mut self.settings;
        match pressed {
            Hit::Toggle(Row::Progress) => s.progress = !s.progress,
            Hit::Toggle(Row::Others) => s.others = !s.others,
            Hit::Toggle(_) => s.hide_idle = !s.hide_idle,
            Hit::Style(i) => s.style = Style::ALL[i],
            Hit::Opacity | Hit::None => return,
        }
        self.commit();
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

impl Drop for Music {
    fn drop(&mut self) {
        widget::stop();
    }
}
