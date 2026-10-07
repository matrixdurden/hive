//! Ortak renkler ve parçalar: kenar çubuğu ve bütün araç sayfaları aynı görünür. Windows 11
//! teması (koyu): pencerenin zemini Mica, üstündeki her şey yarı saydam katmanlar; düğme, anahtar
//! ve kaydırıcılar sistemin vurgu rengindedir. Araçların kendi renkleri yalnızca ikonlarda kalır.

use std::cell::Cell;

use crate::gfx::{Color, Gfx, Rect};

/// Mica yoksa (Windows 10) pencerenin düz zemini.
pub const BG: Color = Color::rgb(0x141414);
pub const HOVER: Color = Color(0xffffff, 0.06);
pub const SEL: Color = Color(0xffffff, 0.09);
pub const LINE: Color = Color(0xffffff, 0.08);
/// Kart ve panel zemini.
pub const PANEL: Color = Color(0xffffff, 0.05);
pub const TEXT: Color = Color::rgb(0xffffff);
pub const MUTED: Color = Color(0xffffff, 0.786);
pub const FAINT: Color = Color(0xffffff, 0.5);
pub const GREEN: Color = Color::rgb(0x6ccb5f);
pub const RED: Color = Color::rgb(0xff99a4);
/// İpucu kutusu (opak, içeriğin üstünde okunsun).
pub const TIP: Color = Color::rgb(0x262626);

thread_local! {
    static ACCENT: Cell<Option<Color>> = const { Cell::new(None) };
}

/// Sistemin vurgu rengi (koyu temadaki açık tonu), bir kez okunur.
pub fn accent() -> Color {
    ACCENT.with(|a| {
        a.get().unwrap_or_else(|| {
            let c = crate::hatter::theme::current().accent;
            a.set(Some(c));
            c
        })
    })
}

/// Vurgu renginin üstündeki yazı: açık vurguda siyah, koyuda beyaz.
pub fn on_accent() -> Color {
    let c = accent().0;
    let ch = |s: u32| ((c >> s) & 0xff) as f32 / 255.0;
    let lum = 0.2126 * ch(16) + 0.7152 * ch(8) + 0.0722 * ch(0);
    if lum > 0.5 { Color(0x000000, 0.9) } else { Color::rgb(0xffffff) }
}

/// Windows'ta vurgu rengi değişti: bir sonraki çizimde yeniden okunur.
pub fn reset_accent() {
    ACCENT.with(|a| a.set(None));
}

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

/// `primary` verilirse dolu (ana eylem, sistemin vurgu renginde), yoksa Windows'un standart
/// düğmesi. Verilen renk yalnızca "ana eylem" işaretidir.
pub fn button(g: &Gfx, r: Rect, label: &str, icon: Option<&str>, primary: Option<Color>, hovered: bool) {
    let fg = match primary {
        Some(_) => {
            g.fill(r, 4.0, accent().alpha(if hovered { 0.9 } else { 1.0 }));
            on_accent()
        }
        None => {
            g.fill(r, 4.0, if hovered { Color(0xffffff, 0.084) } else { Color(0xffffff, 0.061) });
            g.stroke(r, 4.0, LINE, 1.0);
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
        g.fill(r, 4.0, HOVER);
    }
    g.text(icon, &g.f.icon, r, if hovered { TEXT } else { color });
}

/// Sağa yaslı aç/kapa, 40×22.
pub fn toggle_rect(right: f32, cy: f32) -> Rect {
    Rect::new(right - 40.0, cy - 11.0, right, cy + 11.0)
}

/// Windows'un anahtarı: açıkken vurgu renginde dolu, kapalıyken çerçeveli. (Renk parametresi
/// eski çağrılarla uyum için; Windows teması sistemin vurgu rengini kullanır.)
pub fn toggle(g: &Gfx, r: Rect, on: bool, _accent: Color, enabled: bool) {
    let on = on && enabled;
    if on {
        g.fill(r, r.h() / 2.0, accent());
        g.circle(r.r - 11.0, r.cy(), 6.0, on_accent());
    } else {
        let c = if enabled { MUTED } else { FAINT };
        g.stroke(r, r.h() / 2.0, c, 1.0);
        g.circle(r.l + 11.0, r.cy(), 5.0, c);
    }
}

/// Yatay kaydırıcı: `a..b` izi, `v` 0..1.
/// Windows'un kaydırıcısı: ince iz, vurgu renginde dolu kısım, ortası vurgu renginde tutamak.
pub fn slider(g: &Gfx, a: f32, b: f32, cy: f32, v: f32, _accent: Color, active: bool) {
    let x = a + (b - a) * v.clamp(0.0, 1.0);
    g.fill(Rect::new(a, cy - 2.0, b, cy + 2.0), 2.0, Color(0xffffff, 0.54));
    g.fill(Rect::new(a, cy - 2.0, x, cy + 2.0), 2.0, accent());
    g.circle(x, cy, 10.0, Color::rgb(0x454545));
    g.circle(x, cy, if active { 7.0 } else { 6.0 }, accent());
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
    /// Yazılan karakter (WM_CHAR); kullandıysa `true`.
    fn char(&mut self, _c: char) -> bool {
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
