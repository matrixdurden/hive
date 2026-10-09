//! Ortak renkler ve parçalar: kenar çubuğu ve bütün araç sayfaları aynı görünür. Renkler
//! `pal()`'dan gelir (Claude ya da Windows teması); düğme, anahtar ve kaydırıcılar temanın vurgu
//! rengindedir. Araçların kendi renkleri yalnızca ikonlarda kalır.

use std::cell::Cell;

use crate::gfx::{Color, Gfx, Rect};

/// Arayüzün renkleri. İki tema var: Claude (sıcak koyu, kil turuncusu vurgu, opak) ve Windows
/// (Windows 11 Ayarlar uygulaması: Mica zemin, yarı saydam katmanlar, sistemin vurgu rengi).
#[derive(Clone, Copy)]
pub struct Palette {
    /// Pencerenin zemini (kenar çubuğu); Mica'da saydam.
    pub bg: Color,
    /// İçerik alanının katmanı ve kenarlığı.
    pub layer: Color,
    pub layer_line: Color,
    pub hover: Color,
    pub sel: Color,
    pub line: Color,
    /// Kart ve panel zemini.
    pub panel: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub green: Color,
    pub red: Color,
    /// İpucu kutusu (opak, içeriğin üstünde okunsun).
    pub tip: Color,
    /// Kaydırıcı tutamağının çevresi (opak).
    pub knob: Color,
    pub accent: Color,
    pub on_accent: Color,
    /// Zemin Windows'un Mica camı mı.
    pub mica: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Claude,
    Windows,
}

impl Theme {
    pub fn id(self) -> &'static str {
        match self {
            Theme::Claude => "claude",
            Theme::Windows => "windows",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        [Theme::Claude, Theme::Windows].into_iter().find(|t| t.id() == s)
    }
}

const CLAUDE: Palette = Palette {
    bg: Color::rgb(0x1f1e1d),
    layer: Color::rgb(0x262624),
    layer_line: Color(0xdedcd1, 0.08),
    hover: Color(0xf0eee6, 0.06),
    sel: Color(0xf0eee6, 0.09),
    line: Color(0xdedcd1, 0.12),
    panel: Color(0xf0eee6, 0.045),
    text: Color::rgb(0xfaf9f5),
    muted: Color::rgb(0xc2c0b6),
    faint: Color::rgb(0x8f8d86),
    green: Color::rgb(0x7ec27a),
    red: Color::rgb(0xf08a7e),
    tip: Color::rgb(0x30302e),
    knob: Color::rgb(0x3a3a37),
    accent: Color::rgb(0xd97757),
    on_accent: Color::rgb(0xffffff),
    mica: false,
};

/// Windows 11 (WinUI) koyu tema değerleri; vurgu rengi sistemden okunup `windows()` içinde konur.
const WINDOWS: Palette = Palette {
    bg: Color::rgb(0x202020),
    layer: Color(0x3a3a3a, 0.3),
    layer_line: Color(0x000000, 0.1),
    hover: Color(0xffffff, 0.06),
    sel: Color(0xffffff, 0.09),
    line: Color(0xffffff, 0.08),
    panel: Color(0xffffff, 0.05),
    text: Color::rgb(0xffffff),
    muted: Color(0xffffff, 0.786),
    faint: Color(0xffffff, 0.5),
    green: Color::rgb(0x6ccb5f),
    red: Color::rgb(0xff99a4),
    tip: Color::rgb(0x2c2c2c),
    knob: Color::rgb(0x454545),
    accent: Color::rgb(0x60cdff),
    on_accent: Color(0x000000, 0.9),
    mica: false,
};

fn windows(mica: bool) -> Palette {
    // Vurgu rengi gri seçilmişse (doygunluk yok) düğmeler arka plandan ayrılmıyor: Windows'un
    // varsayılan mavisi.
    let sys = crate::hatter::theme::current().accent.0;
    let ch = |s: u32| ((sys >> s) & 0xff) as f32 / 255.0;
    let (r, g, b) = (ch(16), ch(8), ch(0));
    let sat = r.max(g).max(b) - r.min(g).min(b);
    let accent = if sat < 0.15 { WINDOWS.accent } else { Color::rgb(sys) };
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let on_accent = if sat < 0.15 || lum > 0.5 { Color(0x000000, 0.9) } else { Color::rgb(0xffffff) };
    Palette { bg: if mica { Color(0, 0.0) } else { WINDOWS.bg }, accent, on_accent, mica, ..WINDOWS }
}

thread_local! {
    static THEME: Cell<Theme> = const { Cell::new(Theme::Claude) };
    static PALETTE: Cell<Option<Palette>> = const { Cell::new(None) };
}

/// Geçerli tema.
pub fn theme() -> Theme {
    THEME.with(|t| t.get())
}

/// Temayı değiştirir; palet bir sonraki çizimde yeniden kurulur.
pub fn set_theme(t: Theme) {
    THEME.with(|c| c.set(t));
    reset_accent();
}

/// Geçerli palet (ilk çağrıda kurulur).
pub fn pal() -> Palette {
    PALETTE.with(|p| {
        p.get().unwrap_or_else(|| {
            let v = match theme() {
                Theme::Claude => CLAUDE,
                Theme::Windows => windows(crate::hatter::theme::mica()),
            };
            p.set(Some(v));
            v
        })
    })
}

pub fn accent() -> Color {
    pal().accent
}

/// Vurgu renginin üstündeki yazı.
pub fn on_accent() -> Color {
    pal().on_accent
}

/// Windows'ta vurgu rengi ya da tema değişti: bir sonraki çizimde yeniden okunur.
pub fn reset_accent() {
    PALETTE.with(|p| p.set(None));
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
            g.fill(r, 4.0, if hovered { pal().hover.alpha(1.4) } else { pal().hover });
            g.stroke(r, 4.0, pal().line, 1.0);
            pal().text
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
        g.fill(r, 4.0, pal().hover);
    }
    g.text(icon, &g.f.icon, r, if hovered { pal().text } else { color });
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
        let c = if enabled { pal().muted } else { pal().faint };
        g.stroke(r, r.h() / 2.0, c, 1.0);
        g.circle(r.l + 11.0, r.cy(), 5.0, c);
    }
}

/// Yatay kaydırıcı: `a..b` izi, `v` 0..1.
/// Windows'un kaydırıcısı: ince iz, vurgu renginde dolu kısım, ortası vurgu renginde tutamak.
pub fn slider(g: &Gfx, a: f32, b: f32, cy: f32, v: f32, _accent: Color, active: bool) {
    let x = a + (b - a) * v.clamp(0.0, 1.0);
    g.fill(Rect::new(a, cy - 2.0, b, cy + 2.0), 2.0, pal().faint);
    g.fill(Rect::new(a, cy - 2.0, x, cy + 2.0), 2.0, accent());
    g.circle(x, cy, 10.0, pal().knob);
    g.circle(x, cy, if active { 7.0 } else { 6.0 }, accent());
}

/// Ayar satırı: başlık, altında açıklama, altında ince çizgi. Sağ taraf kontrol için boş kalır.
pub fn setting_row(g: &Gfx, r: Rect, title: &str, sub: &str, right_space: f32) {
    if sub.is_empty() {
        g.text(title, &g.f.strong, Rect::new(r.l, r.t, r.r - right_space, r.b), pal().text);
    } else {
        g.text(title, &g.f.strong, Rect::new(r.l, r.t + 12.0, r.r - right_space, r.cy()), pal().text);
        g.text(sub, &g.f.small, Rect::new(r.l, r.cy(), r.r - right_space, r.b - 12.0), pal().muted);
    }
    g.fill(Rect::new(r.l, r.b - 1.0, r.r, r.b), 0.0, pal().hover);
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
