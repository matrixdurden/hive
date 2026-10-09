//! Müzik widget'ı: masaüstünde çalan şarkı. Kapak, şarkı, sanatçı, önceki / çal / sonraki ve
//! ilerleme çubuğu; Spotify için, istenirse başka oynatıcılar da. Boyu, arka planı, saydamlığı,
//! katmanı ayarlanır.
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
use widget::{Place, Style};

pub const ACCENT: u32 = 0xfb7185;

const ROW: f32 = 64.0;
const SIZES: [(f32, &str, &str); 3] = [(0.82, "Small", "Küçük"), (1.0, "Medium", "Orta"), (1.22, "Large", "Büyük")];

/// Sayfanın satırları, yukarıdan aşağı.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Row {
    Show,
    Place,
    Size,
    Style,
    Opacity,
    Progress,
    Others,
    HideIdle,
    Position,
}

const ROWS: [Row; 9] =
    [Row::Show, Row::Place, Row::Style, Row::Opacity, Row::Progress, Row::Others, Row::HideIdle, Row::Size, Row::Position];

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
    /// Widget masaüstünde.
    show: bool,
    /// Spotify çalmıyorken başka oynatıcılar (tarayıcı, ...) da görünsün.
    others: bool,
    /// Hiçbir şey açık değilken widget gizlensin.
    hide_idle: bool,
    /// Widget'ın sol üst köşesi (piksel); yoksa sağ üst.
    pos: Option<(i32, i32)>,
    /// Son görülen Spotify'ın uygulama kimliği (masaüstü, Store ya da tarayıcı uygulaması).
    app: String,
    /// SIZES içindeki sıra.
    size: usize,
    style: Style,
    /// Arka planın opaklığı, yüzde (30..100).
    opacity: u32,
    place: Place,
    progress: bool,
}

impl Settings {
    fn load() -> Self {
        let mut s = Settings {
            show: true,
            others: false,
            hide_idle: false,
            pos: None,
            app: String::new(),
            size: 1,
            style: Style::Cover,
            opacity: 92,
            place: Place::DockLeft,
            progress: true,
        };
        let text = std::fs::read_to_string(ini()).unwrap_or_default();
        let (mut x, mut y) = (None, None);
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "goster" => s.show = v == "1",
                "diger" => s.others = v == "1",
                "bosken_gizle" => s.hide_idle = v == "1",
                "uygulama" => s.app = v.to_string(),
                "boy" => s.size = v.parse::<usize>().unwrap_or(1).min(SIZES.len() - 1),
                "arka_plan" => s.style = Style::from_id(v).unwrap_or(s.style),
                "opaklik" => s.opacity = v.parse::<u32>().unwrap_or(92).clamp(30, 100),
                "konum" => s.place = Place::from_id(v).unwrap_or(s.place),
                "ilerleme" => s.progress = v == "1",
                "x" => x = v.parse().ok(),
                "y" => y = v.parse().ok(),
                _ => {}
            }
        }
        s.pos = x.zip(y);
        s
    }

    fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(dir())?;
        let b = |v: bool| v as u8;
        let pos = self.pos.map(|(x, y)| format!("x={x}\ny={y}\n")).unwrap_or_default();
        std::fs::write(
            ini(),
            format!(
                "goster={}\ndiger={}\nbosken_gizle={}\nuygulama={}\nboy={}\narka_plan={}\nopaklik={}\nkonum={}\nilerleme={}\n{pos}",
                b(self.show),
                b(self.others),
                b(self.hide_idle),
                self.app,
                self.size,
                self.style.id(),
                self.opacity,
                self.place.id(),
                b(self.progress),
            ),
        )
    }

    fn options(&self) -> widget::Options {
        widget::Options {
            hide_idle: self.hide_idle,
            app: self.app.clone(),
            pos: self.pos,
            on_moved: save_position,
            scale: SIZES[self.size].0,
            style: self.style,
            opacity: self.opacity as f32 / 100.0,
            place: self.place,
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
        let all = Style::ALL.into_iter().flat_map(|st| [(st, true), (st, false)]);
        for (style, strip) in all {
            let name = format!("muzik-{}-{}.png", if strip { "dock" } else { "masaustu" }, style.id());
            let path = std::path::Path::new(dir).join(name);
            match widget::preview(&path, style, strip, now.clone()) {
                Ok(()) => s += &format!("önizleme: {}\n", path.display()),
                Err(e) => s += &format!("önizleme çizilemedi: {e}\n"),
            }
        }
    }
    s
}

/// Widget taşındı: yeni yer ayar dosyasına.
fn save_position(x: i32, y: i32) {
    let mut s = Settings::load();
    s.pos = Some((x, y));
    if let Err(e) = s.save() {
        crate::log!("müzik: yer yazılamadı: {e}");
    }
}

// --- Sayfa ---

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    None,
    Toggle(Row),
    /// Bir seçicinin `n`. parçası (boy ya da arka plan).
    Seg(Row, usize),
    Opacity,
    Reset,
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
    scroll: f32,
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
            scroll: 0.0,
            w: 0.0,
            h: 0.0,
            head_x: 0.0,
            head_r: 0.0,
        };
        m.apply();
        m
    }

    /// Widget'ı ayarlara göre açar, günceller ya da kapatır.
    fn apply(&self) {
        if self.settings.show {
            widget::start(self.media.remote(), self.settings.options(), self.now.clone());
        } else {
            widget::stop();
        }
    }

    /// WM_MEDIA: çalan şarkı değişti.
    pub fn media_changed(&mut self) {
        self.now = self.media.now();
        if self.now.spotify && self.now.app != self.settings.app {
            self.settings.app = self.now.app.clone();
            self.settings.pos = Settings::load().pos;
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
        let t = HEADER + 12.0 + i as f32 * ROW - self.scroll;
        Rect::new(PAD, t, self.w - PAD, t + ROW)
    }

    fn max_scroll(&self) -> f32 {
        (HEADER + 24.0 + ROWS.len() as f32 * ROW - self.h).max(0.0)
    }

    fn labels(r: Row) -> Vec<&'static str> {
        match r {
            Row::Size => SIZES.iter().map(|s| t!(s.1, s.2)).collect(),
            Row::Place => vec![t!("Left of dock", "Dock solu"), t!("Right of dock", "Dock sağı"), t!("Desktop", "Masaüstü")],
            _ => vec![t!("Cover", "Kapak"), t!("Color", "Renk"), t!("Plain", "Düz")],
        }
    }

    /// Seçicinin parçaları, satırın sağına yaslı.
    fn seg_rects(g: &Gfx, r: Rect, row: Row) -> Vec<Rect> {
        let labels = Self::labels(row);
        let w = labels.iter().map(|l| g.measure(l, &g.f.button)).fold(0.0, f32::max) + 28.0;
        let (t, b) = (r.cy() - 15.0, r.cy() + 15.0);
        let n = labels.len() as f32;
        (0..labels.len()).map(|i| Rect::new(r.r - (n - i as f32) * w, t, r.r - (n - 1.0 - i as f32) * w, b)).collect()
    }

    fn slider_x(&self) -> (f32, f32) {
        let r = self.row(Row::Opacity);
        (r.r - 220.0, r.r - 52.0)
    }

    fn reset_rect(&self, g: &Gfx) -> Rect {
        let r = self.row(Row::Position);
        button_rect_right(g, r.r, r.cy() - 17.0, t!("Move back", "Yerine al"), false)
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return Hit::None;
        }
        let on = self.settings.show;
        for row in ROWS {
            let r = self.row(row);
            if !r.contains(x, y) {
                continue;
            }
            return match row {
                Row::Size | Row::Position if on && !self.desktop() => Hit::None,
                Row::Place | Row::Size | Row::Style if on => Self::seg_rects(g, r, row)
                    .iter()
                    .position(|s| s.contains(x, y))
                    .map_or(Hit::None, |i| Hit::Seg(row, i)),
                Row::Opacity if on => {
                    let (a, b) = self.slider_x();
                    if x >= a - 10.0 && x <= b + 10.0 { Hit::Opacity } else { Hit::None }
                }
                Row::Position if on && self.reset_rect(g).contains(x, y) => Hit::Reset,
                Row::Show | Row::Progress | Row::Others | Row::HideIdle
                    if (row == Row::Show || on) && x > r.r - 120.0 =>
                {
                    Hit::Toggle(row)
                }
                _ => Hit::None,
            };
        }
        Hit::None
    }

    /// Masaüstü kartı mı (boyut ve konum yalnızca onda); dock kapalıysa şerit de masaüstüne düşer.
    fn desktop(&self) -> bool {
        self.settings.place == Place::Desktop || crate::hatter::geometry().is_none()
    }

    fn set_opacity_from(&mut self, x: f32) {
        let (a, b) = self.slider_x();
        let v = (30.0 + ((x - a) / (b - a)).clamp(0.0, 1.0) * 70.0).round() as u32;
        if v != self.settings.opacity {
            self.settings.opacity = v;
            // Sürüklerken widget da canlı değişsin (dosyaya bırakınca yazılır).
            if self.settings.show {
                widget::start(self.media.remote(), self.settings.options(), self.now.clone());
            }
            self.redraw();
        }
    }

    fn commit(&mut self) {
        // Yer widget'ın kaydettiği gibi kalsın.
        self.settings.pos = Settings::load().pos;
        if let Err(e) = self.settings.save() {
            crate::log!("müzik: ayarlar yazılamadı: {e}");
        }
        self.media.set_others(self.settings.others);
        self.apply();
        self.redraw();
    }

    fn head(&self) -> (Color, String) {
        let n = &self.now;
        if !n.active {
            return (pal().faint, t!("Nothing playing", "Bir şey çalmıyor").into());
        }
        let who = if n.artist.is_empty() { n.title.clone() } else { format!("{} · {}", n.title, n.artist) };
        if n.playing { (pal().green, who) } else { (pal().faint, format!("{} · {who}", t!("Paused", "Duraklatıldı"))) }
    }

    fn texts(row: Row) -> (&'static str, &'static str) {
        match row {
            Row::Show => (
                t!("Show the widget", "Widget'ı göster"),
                t!("Click it to open Spotify", "Tıklayınca Spotify açılır"),
            ),
            Row::Place => (
                t!("Place", "Yeri"),
                t!("Next to the dock it's always visible; without the dock it goes to the desktop", "Dock'un yanında hep görünür; dock kapalıysa masaüstüne geçer"),
            ),
            Row::Size => (t!("Size on the desktop", "Masaüstündeki boyu"), ""),
            Row::Style => (
                t!("Background", "Arka plan"),
                t!("The cover blurred, its color, or plain like the dock", "Kapağın bulanık hali, rengi ya da dock gibi düz"),
            ),
            Row::Opacity => (t!("Opacity", "Saydamlık"), t!("Of the background; text stays sharp", "Arka planın; yazılar hep net")),
            Row::Progress => (t!("Progress bar", "İlerleme çubuğu"), t!("Elapsed and remaining time; click to seek", "Geçen ve kalan süre; tıklayıp atlayabilirsin")),
            Row::Others => (
                t!("Other players too", "Diğer oynatıcılar da"),
                t!("When Spotify isn't open, show what the browser or another app is playing", "Spotify açık değilken tarayıcının ya da başka bir uygulamanın çaldığını göster"),
            ),
            Row::HideIdle => (
                t!("Hide when nothing plays", "Bir şey çalmıyorken gizle"),
                t!("Otherwise it waits with a shortcut that opens Spotify", "Kapalıysa Spotify'ı açan bir kısayolla bekler"),
            ),
            Row::Position => (
                t!("Position on the desktop", "Masaüstündeki yeri"),
                t!("Drag the card anywhere, or put it back in the top-right corner", "Kartı sürükleyip istediğin yere koy ya da sağ üst köşeye geri al"),
            ),
        }
    }
}

impl ToolPage for Music {
    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn paint(&self, g: &Gfx) {
        let (dot, head) = self.head();
        status(g, self.head_x + 16.0, HEAD_CY, dot, &head, pal().muted, self.head_r - 8.0);
        let s = &self.settings;
        g.clip(Rect::new(0.0, HEADER, self.w, self.h), || {
            for row in ROWS {
                let r = self.row(row);
                let enabled = row == Row::Show || (s.show && (!matches!(row, Row::Size | Row::Position) || self.desktop()));
                let (title, sub) = Self::texts(row);
                match row {
                    Row::Place | Row::Size | Row::Style => {
                        let seg = Self::seg_rects(g, r, row);
                        setting_row(g, r, title, sub, r.r - seg[0].l + 16.0);
                        let all = Rect::new(seg[0].l - 3.0, seg[0].t - 3.0, seg[seg.len() - 1].r + 3.0, seg[0].b + 3.0);
                        g.fill(all, 6.0, pal().hover);
                        let current = match row {
                            Row::Size => s.size,
                            Row::Place => Place::ALL.iter().position(|&x| x == s.place).unwrap_or(0),
                            _ => Style::ALL.iter().position(|&x| x == s.style).unwrap_or(0),
                        };
                        for (i, (sr, label)) in seg.iter().zip(Self::labels(row)).enumerate() {
                            let c = if i == current && enabled {
                                g.fill(*sr, 4.0, accent());
                                on_accent()
                            } else if self.hover == Hit::Seg(row, i) {
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
                    Row::Opacity => {
                        setting_row(g, r, title, sub, 260.0);
                        let (a, b) = self.slider_x();
                        let v = (s.opacity as f32 - 30.0) / 70.0;
                        slider(g, a, b, r.cy(), v, accent(), enabled && (self.dragging || self.hover == Hit::Opacity));
                        g.text(&format!("%{}", s.opacity), &g.f.small_right, Rect::new(b + 8.0, r.t, r.r, r.b), pal().muted);
                    }
                    Row::Position => {
                        setting_row(g, r, title, sub, 140.0);
                        if enabled {
                            button(g, self.reset_rect(g), t!("Move back", "Yerine al"), None, None, self.hover == Hit::Reset);
                        }
                    }
                    _ => {
                        let on = match row {
                            Row::Show => s.show,
                            Row::Progress => s.progress,
                            Row::Others => s.others,
                            _ => s.hide_idle,
                        };
                        setting_row(g, r, title, sub, 120.0);
                        toggle(g, toggle_rect(r.r, r.cy()), on, accent(), enabled);
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
            Hit::Toggle(Row::Show) => s.show = !s.show,
            Hit::Toggle(Row::Progress) => s.progress = !s.progress,
            Hit::Toggle(Row::Others) => s.others = !s.others,
            Hit::Toggle(_) => s.hide_idle = !s.hide_idle,
            Hit::Seg(Row::Size, i) => s.size = i,
            Hit::Seg(Row::Place, i) => s.place = Place::ALL[i],
            Hit::Seg(_, i) => s.style = Style::ALL[i],
            Hit::Reset => {
                widget::reset_position();
                return;
            }
            Hit::Opacity | Hit::None => return,
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
            self.dragging = false;
        }
    }
}

impl Drop for Music {
    fn drop(&mut self) {
        widget::stop();
    }
}
