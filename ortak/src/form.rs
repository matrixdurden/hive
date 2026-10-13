//! Sayfa kiti: bütün sayfalar aynı görünsün diye. Sayfa ne göstereceğini bir öğe listesiyle
//! söyler (`Item`), kit yerleştirir, çizer ve tıklananı bulur.
//!
//! Düzen: sol kenara yaslı, en çok `COL_MAX` genişliğinde tek sütun. Her bölüm başlığın altında
//! tek bir kart; kartın satırları arasında ince çizgi. Satır: solda (varsa) simge, başlık ve
//! altında açıklama; sağda tek kontrol. Kontroller her sayfada aynı ölçüde ve aynı hizada.

use crate::gfx::{Gfx, Rect};
use crate::ui::*;

/// İçerik sütununun en geniş hali.
pub const COL_MAX: f32 = 860.0;
/// Satır yüksekliği: açıklamasız ve açıklamalı.
const ROW: f32 = 52.0;
const ROW_SUB: f32 = 64.0;
/// Bölüm başlığının yüksekliği (kartın üstünde) ve kartlar arası boşluk.
const SECTION: f32 = 36.0;
const GAP: f32 = 24.0;
/// Kartın iç kenar boşluğu.
const IN: f32 = 16.0;
/// Kaydırıcının izi ve değer yazısının yeri.
const TRACK: f32 = 200.0;
const VALUE: f32 = 56.0;

/// Satırın sağındaki kontrol.
pub enum Ctl {
    None,
    Toggle(bool),
    /// Seçenekler ve seçili olan.
    Seg(Vec<String>, usize),
    /// 0..1 ve yanında gösterilen değer.
    Slider(f32, String),
    Button(String),
    /// Ana eylem (dolu düğme).
    Primary(String),
    /// Kısayol kutucuğu; `true`: atanıyor (vurgulu).
    Chip(String, bool),
    /// Sağda soluk yazı (durum, değer).
    Text(String),
    /// Listede seçili olan.
    Check(bool),
    /// Sağda bu genişlikte yer: sayfa kendi kontrolünü (yazı kutusu gibi) `Form::control`
    /// rect'ine çizer; tıklama `Hit::Row` olarak gelir.
    Space(f32),
}

pub struct Row {
    /// Sayfanın bu satır için verdiği kimlik: tıklamada geri gelir.
    pub id: u32,
    pub icon: Option<&'static str>,
    pub title: String,
    pub sub: String,
    pub ctl: Ctl,
    /// Satırın tamamı tıklanır (listeler): üstüne gelince belirginleşir.
    pub click: bool,
    /// Soluk (kullanılamıyor).
    pub dim: bool,
}

impl Row {
    pub fn new(id: u32, title: impl Into<String>, sub: impl Into<String>, ctl: Ctl) -> Self {
        Row { id, icon: None, title: title.into(), sub: sub.into(), ctl, click: false, dim: false }
    }

    pub fn icon(mut self, glyph: &'static str) -> Self {
        self.icon = Some(glyph);
        self
    }

    pub fn clickable(mut self) -> Self {
        self.click = true;
        self
    }

    pub fn dim(mut self, dim: bool) -> Self {
        self.dim = dim;
        self
    }
}

pub enum Item {
    /// Yeni bölüm: başlığı (boşsa başlıksız) ve altında kartı.
    Section(String),
    Row(Row),
    /// Kartın altında soluk açıklama ya da hata (`true`: kırmızı).
    Note(String, bool),
    /// Sayfanın kendisinin çizdiği alan (galeri gibi): kimlik ve yükseklik.
    Custom(u32, f32),
}

/// Tıklanan / üstüne gelinen.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    None,
    /// Satırın kendisi (kontrol dışı).
    Row(u32),
    Toggle(u32),
    Seg(u32, usize),
    /// Kaydırıcı ve imlecin olduğu değer (0..1).
    Slider(u32, f32),
    Button(u32),
    Chip(u32),
    Custom(u32),
}

impl Hit {
    /// Hangi satırda (ya da özel alanda).
    pub fn id(self) -> Option<u32> {
        match self {
            Hit::None => None,
            Hit::Row(i) | Hit::Toggle(i) | Hit::Seg(i, _) | Hit::Slider(i, _) | Hit::Button(i) | Hit::Chip(i) | Hit::Custom(i) => Some(i),
        }
    }

    /// Aynı yer mi (kaydırıcının değeri ve seçeneğin sırası dışında).
    pub fn same(self, other: Hit) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other) && self.id() == other.id()
    }
}

/// Yerleştirilmiş sayfa.
pub struct Form {
    items: Vec<Item>,
    rects: Vec<Rect>,
    cards: Vec<Rect>,
    bottom: f32,
}

impl Form {
    /// `l..r` arasında, `top`'tan başlayarak yerleştirir (kaydırma `top`'a katılır).
    pub fn new(items: Vec<Item>, l: f32, r: f32, top: f32) -> Self {
        let r = r.min(l + COL_MAX);
        let mut rects = Vec::with_capacity(items.len());
        let mut cards = Vec::new();
        let mut y = top;
        let mut card: Option<f32> = None;
        let close = |card: &mut Option<f32>, y: &mut f32, cards: &mut Vec<Rect>| {
            if let Some(t) = card.take() {
                cards.push(Rect::new(l, t, r, *y));
                *y += GAP;
            }
        };
        for (k, it) in items.iter().enumerate() {
            match it {
                Item::Section(title) => {
                    close(&mut card, &mut y, &mut cards);
                    let h = if title.is_empty() { 0.0 } else { SECTION };
                    rects.push(Rect::new(l, y, r, y + h));
                    y += h;
                }
                Item::Row(row) => {
                    if card.is_none() {
                        card = Some(y);
                    }
                    let h = if row.sub.is_empty() { ROW } else { ROW_SUB };
                    rects.push(Rect::new(l, y, r, y + h));
                    y += h;
                }
                Item::Note(..) => {
                    // Kartın hemen altında, aradaki boşluğu kullanır.
                    let had = card.is_some();
                    close(&mut card, &mut y, &mut cards);
                    let t = if had { y - GAP + 6.0 } else { y };
                    rects.push(Rect::new(l + 4.0, t, r, t + 20.0));
                    y = t + 20.0 + GAP - 6.0;
                }
                Item::Custom(_, h) => {
                    close(&mut card, &mut y, &mut cards);
                    rects.push(Rect::new(l, y, r, y + h));
                    y += h + GAP;
                    let _ = k;
                }
            }
        }
        close(&mut card, &mut y, &mut cards);
        Form { items, rects, cards, bottom: y }
    }

    /// İçeriğin bittiği y (kaydırma sınırı için).
    pub fn bottom(&self) -> f32 {
        self.bottom
    }

    /// Özel alanın yeri.
    pub fn custom(&self, id: u32) -> Option<Rect> {
        self.items.iter().zip(&self.rects).find(|(it, _)| matches!(it, Item::Custom(i, _) if *i == id)).map(|(_, r)| *r)
    }

    /// Satırın yeri.
    pub fn row(&self, id: u32) -> Option<Rect> {
        self.items.iter().zip(&self.rects).find(|(it, _)| matches!(it, Item::Row(r) if r.id == id)).map(|(_, r)| *r)
    }

    fn seg_rects(g: &Gfx, r: Rect, opts: &[String]) -> Vec<Rect> {
        let widths: Vec<f32> = opts.iter().map(|o| g.measure(o, &g.f.button) + 28.0).collect();
        let mut x = r.r - IN - 3.0 - widths.iter().sum::<f32>();
        widths
            .iter()
            .map(|w| {
                let s = Rect::new(x, r.cy() - 15.0, x + w, r.cy() + 15.0);
                x += w;
                s
            })
            .collect()
    }

    fn track(r: Rect) -> (f32, f32) {
        let b = r.r - IN - VALUE;
        (b - TRACK, b)
    }

    fn button_rect(g: &Gfx, r: Rect, label: &str) -> Rect {
        button_rect_right(g, r.r - IN, r.cy() - 17.0, label, false)
    }

    /// Kontrolün kapladığı yer (satırın sağı); başlığın sığacağı sınır için.
    fn ctl_left(g: &Gfx, r: Rect, ctl: &Ctl) -> f32 {
        match ctl {
            Ctl::None => r.r - IN,
            Ctl::Toggle(_) => r.r - IN - 40.0 - 16.0,
            Ctl::Seg(o, _) => Self::seg_rects(g, r, o).first().map_or(r.r, |s| s.l) - 16.0,
            Ctl::Slider(..) => Self::track(r).0 - 24.0,
            Ctl::Button(t) | Ctl::Primary(t) => Self::button_rect(g, r, t).l - 16.0,
            Ctl::Chip(t, _) => chip_rect(g, r.r - IN, r.cy(), t).l - 16.0,
            Ctl::Text(t) => r.r - IN - g.measure(t, &g.f.small).min(240.0) - 16.0,
            Ctl::Check(_) => r.r - IN - 28.0,
            Ctl::Space(w) => r.r - IN - w - 16.0,
        }
    }

    /// `Ctl::Space` satırının kontrol alanı (34 px yüksek, sağa yaslı).
    pub fn control(&self, id: u32) -> Option<Rect> {
        let (it, r) = self.items.iter().zip(&self.rects).find(|(it, _)| matches!(it, Item::Row(r) if r.id == id))?;
        let Item::Row(row) = it else { return None };
        let Ctl::Space(w) = row.ctl else { return None };
        Some(Rect::new(r.r - IN - w, r.cy() - 17.0, r.r - IN, r.cy() + 17.0))
    }

    pub fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        for (it, r) in self.items.iter().zip(&self.rects) {
            if !r.contains(x, y) {
                continue;
            }
            match it {
                Item::Custom(id, _) => return Hit::Custom(*id),
                Item::Row(row) => {
                    let id = row.id;
                    let hit = match &row.ctl {
                        Ctl::Toggle(_) => Rect::new(r.r - IN - 52.0, r.t, r.r, r.b).contains(x, y).then_some(Hit::Toggle(id)),
                        Ctl::Seg(o, _) => Self::seg_rects(g, *r, o).iter().position(|s| s.contains(x, y)).map(|k| Hit::Seg(id, k)),
                        Ctl::Slider(..) => {
                            let (a, b) = Self::track(*r);
                            (x >= a - 10.0 && x <= b + 10.0).then(|| Hit::Slider(id, ((x - a) / (b - a)).clamp(0.0, 1.0)))
                        }
                        Ctl::Button(t) | Ctl::Primary(t) => Self::button_rect(g, *r, t).contains(x, y).then_some(Hit::Button(id)),
                        Ctl::Chip(t, _) => chip_rect(g, r.r - IN, r.cy(), t).contains(x, y).then_some(Hit::Chip(id)),
                        _ => None,
                    };
                    return hit.unwrap_or(Hit::Row(id));
                }
                _ => return Hit::None,
            }
        }
        Hit::None
    }

    /// Kaydırıcı sürüklenirken imlecin değeri (0..1).
    pub fn slider_value(&self, id: u32, x: f32) -> Option<f32> {
        let r = self.row(id)?;
        let (a, b) = Self::track(r);
        Some(((x - a) / (b - a)).clamp(0.0, 1.0))
    }

    /// `hover`: üstüne gelinen; `drag`: sürüklenen kaydırıcının satırı.
    pub fn paint(&self, g: &Gfx, hover: Hit, drag: Option<u32>) {
        for c in &self.cards {
            g.fill(*c, 8.0, pal().panel);
            g.stroke(*c, 8.0, pal().line, 1.0);
        }
        let mut prev_row = false;
        for (it, r) in self.items.iter().zip(&self.rects) {
            let r = *r;
            match it {
                Item::Section(title) => {
                    if !title.is_empty() {
                        g.text(title, &g.f.strong, Rect::new(r.l + 2.0, r.t, r.r, r.b - 8.0), pal().text);
                    }
                    prev_row = false;
                }
                Item::Note(text, err) => {
                    g.text(text, &g.f.small, r, if *err { pal().red } else { pal().faint });
                    prev_row = false;
                }
                Item::Custom(..) => prev_row = false,
                Item::Row(row) => {
                    if prev_row {
                        g.fill(Rect::new(r.l + IN, r.t, r.r - IN, r.t + 1.0), 0.0, pal().line);
                    }
                    prev_row = true;
                    if row.click && hover.id() == Some(row.id) {
                        g.fill(Rect::new(r.l + 4.0, r.t + 4.0, r.r - 4.0, r.b - 4.0), 6.0, pal().hover);
                    }
                    self.paint_row(g, r, row, hover, drag);
                }
            }
        }
    }

    fn paint_row(&self, g: &Gfx, r: Rect, row: &Row, hover: Hit, drag: Option<u32>) {
        let mut x = r.l + IN;
        let fg = if row.dim { pal().faint } else { pal().text };
        if let Some(ic) = row.icon {
            g.text(ic, &g.f.icon_small, Rect::new(x, r.t, x + 20.0, r.b), if row.dim { pal().faint } else { pal().muted });
            x += 32.0;
        }
        let right = Self::ctl_left(g, r, &row.ctl);
        if row.sub.is_empty() {
            g.text(&row.title, &g.f.text, Rect::new(x, r.t, right, r.b), fg);
        } else {
            g.text(&row.title, &g.f.text, Rect::new(x, r.t + 11.0, right, r.cy() + 1.0), fg);
            g.text(&row.sub, &g.f.small, Rect::new(x, r.cy() + 1.0, right, r.b - 11.0), pal().muted);
        }
        let id = row.id;
        let hovered = |h: Hit| hover.same(h);
        match &row.ctl {
            Ctl::None | Ctl::Space(_) => {}
            Ctl::Toggle(on) => toggle(g, toggle_rect(r.r - IN, r.cy()), *on, accent(), !row.dim),
            Ctl::Seg(opts, sel) => {
                let segs = Self::seg_rects(g, r, opts);
                if let (Some(a), Some(b)) = (segs.first(), segs.last()) {
                    g.fill(Rect::new(a.l - 3.0, a.t - 3.0, b.r + 3.0, b.b + 3.0), 7.0, pal().hover);
                }
                for (k, (s, label)) in segs.iter().zip(opts).enumerate() {
                    let on = k == *sel;
                    let hov = hover == Hit::Seg(id, k);
                    if on {
                        g.fill(*s, 5.0, pal().sel.alpha(1.6));
                    } else if hov {
                        g.fill(*s, 5.0, pal().hover);
                    }
                    g.text(label, &g.f.button, *s, if on || hov { pal().text } else { pal().muted });
                }
            }
            Ctl::Slider(v, value) => {
                let (a, b) = Self::track(r);
                let active = drag == Some(id) || (drag.is_none() && matches!(hover, Hit::Slider(i, _) if i == id));
                slider(g, a, b, r.cy(), *v, accent(), active);
                g.text(value, &g.f.small_right, Rect::new(b + 8.0, r.t, r.r - IN, r.b), pal().muted);
            }
            Ctl::Button(t) => button(g, Self::button_rect(g, r, t), t, None, None, hovered(Hit::Button(id))),
            Ctl::Primary(t) => button(g, Self::button_rect(g, r, t), t, None, Some(accent()), hovered(Hit::Button(id))),
            Ctl::Chip(t, active) => {
                let c = chip_rect(g, r.r - IN, r.cy(), t);
                let (fg, border) = match () {
                    _ if *active => (accent(), accent()),
                    _ if hovered(Hit::Chip(id)) => (pal().text, pal().faint),
                    _ => (pal().muted, pal().line),
                };
                chip(g, c, t, fg, border);
            }
            Ctl::Text(t) => g.text(t, &g.f.small_right, Rect::new(r.r - IN - 240.0, r.t, r.r - IN, r.b), pal().muted),
            Ctl::Check(on) => {
                if *on {
                    g.text("\u{E73E}", &g.f.icon_small, Rect::new(r.r - IN - 20.0, r.t, r.r - IN, r.b), accent());
                }
            }
        }
    }
}
