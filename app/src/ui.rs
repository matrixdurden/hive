//! Ortak renkler ve parçalar: kenar çubuğu ve bütün araç sayfaları aynı görünür.

use crate::gfx::{Color, Gfx, Rect};

pub const BG: Color = Color::rgb(0x0f0f10);
pub const HOVER: Color = Color::rgb(0x1b1b1e);
pub const SEL: Color = Color::rgb(0x222226);
pub const LINE: Color = Color::rgb(0x2a2a2e);
pub const TEXT: Color = Color::rgb(0xe8e8ea);
pub const MUTED: Color = Color::rgb(0x8b8b93);
pub const FAINT: Color = Color::rgb(0x55555c);
pub const GREEN: Color = Color::rgb(0x4ade80);
pub const RED: Color = Color::rgb(0xf87171);
pub const ON_ACCENT: Color = Color::rgb(0x111111);

/// Araç sayfasının üst çubuğu ve kenar boşluğu.
pub const HEADER: f32 = 48.0;
/// Başlık bandındaki öğelerin dikey ortası (bandın tam ortası; pencere düğmeleri bandı doldurur).
pub const HEAD_CY: f32 = HEADER / 2.0;
pub const PAD: f32 = 24.0;

pub const ICON_ADD: &str = "\u{E710}";
pub const ICON_MUTE: &str = "\u{E74F}";
pub const ICON_AUDIO: &str = "\u{E8D6}";
pub const ICON_REMOVE: &str = "\u{E711}";
pub const ICON_MIC: &str = "\u{E720}";
pub const ICON_EAR: &str = "\u{E7F6}";
pub const ICON_PLAY: &str = "\u{E768}";
pub const ICON_PAUSE: &str = "\u{E769}";
pub const ICON_SETTINGS: &str = "\u{E713}";
pub const ICON_MENU: &str = "\u{E700}";
pub const ICON_DOWNLOAD: &str = "\u{E896}";
pub const ICON_REFRESH: &str = "\u{E72C}";
pub const ICON_FOLDER: &str = "\u{E8B7}";

/// Yazıya göre genişleyen düğme; `icon` varsa yazının solunda.
pub fn button_rect(g: &Gfx, l: f32, t: f32, label: &str, icon: bool) -> Rect {
    let tw = g.measure(label, &g.f.button);
    Rect::new(l, t, l + tw + if icon { 54.0 } else { 32.0 }, t + 34.0)
}

/// Sağ kenarı `r` olan düğme.
pub fn button_rect_right(g: &Gfx, r: f32, t: f32, label: &str, icon: bool) -> Rect {
    let b = button_rect(g, 0.0, t, label, icon);
    Rect::new(r - b.w(), b.t, r, b.b)
}

/// `accent` varsa dolu (ana eylem), yoksa çerçeveli.
pub fn button(g: &Gfx, r: Rect, label: &str, icon: Option<&str>, accent: Option<Color>, hovered: bool) {
    let fg = match accent {
        Some(a) => {
            g.fill(r, 6.0, a.alpha(if hovered { 0.88 } else { 1.0 }));
            ON_ACCENT
        }
        None => {
            g.fill(r, 6.0, if hovered { SEL } else { HOVER });
            g.stroke(r, 6.0, LINE, 1.0);
            TEXT
        }
    };
    match icon {
        Some(ic) => {
            g.text(ic, &g.f.icon_small, Rect::new(r.l + 12.0, r.t, r.l + 30.0, r.b), fg);
            g.text(label, &g.f.button, Rect::new(r.l + 26.0, r.t, r.r - 4.0, r.b), fg);
        }
        None => g.text(label, &g.f.button, r, fg),
    }
}

/// Yalnızca ikon: üstüne gelince zemin belirir.
pub fn icon_button(g: &Gfx, r: Rect, icon: &str, color: Color, hovered: bool) {
    if hovered {
        g.fill(r, 6.0, HOVER);
    }
    g.text(icon, &g.f.icon, r, if hovered { TEXT } else { color });
}

/// Sağa yaslı aç/kapa, 40×22.
pub fn toggle_rect(right: f32, cy: f32) -> Rect {
    Rect::new(right - 40.0, cy - 11.0, right, cy + 11.0)
}

pub fn toggle(g: &Gfx, r: Rect, on: bool, accent: Color, enabled: bool) {
    let on = on && enabled;
    g.fill(r, 11.0, if on { accent } else { LINE });
    let knob = if on { BG } else if enabled { MUTED } else { FAINT };
    g.circle(if on { r.r - 11.0 } else { r.l + 11.0 }, r.cy(), 7.0, knob);
}

/// Yatay kaydırıcı: `a..b` izi, `v` 0..1.
pub fn slider(g: &Gfx, a: f32, b: f32, cy: f32, v: f32, accent: Color, active: bool) {
    let x = a + (b - a) * v.clamp(0.0, 1.0);
    g.fill(Rect::new(a, cy - 2.0, b, cy + 2.0), 2.0, LINE);
    g.fill(Rect::new(a, cy - 2.0, x, cy + 2.0), 2.0, if active { accent } else { accent.alpha(0.8) });
    g.circle(x, cy, if active { 7.0 } else { 6.0 }, TEXT);
}

/// Ayar satırı: başlık, altında açıklama, altında ince çizgi. Sağ taraf kontrol için boş kalır.
pub fn setting_row(g: &Gfx, r: Rect, title: &str, sub: &str, right_space: f32) {
    if sub.is_empty() {
        g.text(title, &g.f.strong, Rect::new(r.l, r.t, r.r - right_space, r.b), TEXT);
    } else {
        g.text(title, &g.f.strong, Rect::new(r.l, r.t + 12.0, r.r - right_space, r.cy()), TEXT);
        g.text(sub, &g.f.small, Rect::new(r.l, r.cy(), r.r - right_space, r.b - 12.0), MUTED);
    }
    g.fill(Rect::new(r.l, r.b - 1.0, r.r, r.b), 0.0, HOVER);
}

/// Kısayol ya da etiket kutucuğu.
pub fn chip(g: &Gfx, r: Rect, text: &str, color: Color, border: Color) {
    g.stroke(r, 6.0, border, 1.0);
    g.text(text, &g.f.small_center, r, color);
}

/// Sağ kenarı `right`, dikey ortası `cy` olan kutucuk.
pub fn chip_rect(g: &Gfx, right: f32, cy: f32, text: &str) -> Rect {
    let tw = g.measure(text, &g.f.small_center);
    Rect::new(right - tw - 18.0, cy - 11.0, right, cy + 11.0)
}

/// Durum noktası ve yazısı; yazının bittiği x'i döndürür.
pub fn status(g: &Gfx, l: f32, cy: f32, dot: Color, text: &str, color: Color, max_r: f32) -> f32 {
    g.circle(l + 4.0, cy, 4.0, dot);
    let tw = g.measure(text, &g.f.text);
    g.text(text, &g.f.text, Rect::new(l + 16.0, cy - 12.0, max_r, cy + 12.0), color);
    (l + 16.0 + tw).min(max_r)
}

/// `s`'yi `cx` etrafında ortalar (tek satır).
pub fn text_center(g: &Gfx, s: &str, format: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat, cx: f32, t: f32, b: f32, c: Color) {
    let tw = g.measure(s, format);
    g.text(s, format, Rect::new(cx - tw / 2.0 - 1.0, t, cx + tw / 2.0 + 2.0, b), c);
}

/// Araç sayfası: hive başlıkta ikonu ve adı çizer, olayları sayfanın kendi köşesine göre
/// (x - kenar çubuğu) iletir. Zamanlayıcı ve kendi mesajları ayrıca yönlendirilir.
pub trait ToolPage {
    /// Sayfanın boyutu, başlıktaki adın bittiği x ve başlık düğmelerinin sağ sınırı (pencere
    /// düğmelerinin solu); her olaydan önce verilir.
    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32);
    fn paint(&self, g: &Gfx);
    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32);
    fn mouse_leave(&mut self);
    fn mouse_down(&mut self, g: &Gfx, x: f32, y: f32);
    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32);
    fn wheel(&mut self, _g: &Gfx, _x: f32, _y: f32, _delta: f32) {}
    /// Tuşu kullandıysa `true`.
    fn key(&mut self, _vk: u16) -> bool {
        false
    }
    /// Bu noktada tıklanabilir bir şey var mı: yoksa başlık bandında pencere sürüklenir.
    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool;
    /// Üstüne gelinen ikon düğmesinin açıklaması: (düğme, yazı).
    fn tip(&self) -> Option<(Rect, String)> {
        None
    }
    /// Sayfa ekrana geldi ya da gitti.
    fn set_visible(&mut self, visible: bool);
}
