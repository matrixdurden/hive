//! Ana sayfa: engram'ın yüzü. Ortada canlı küre (bekler, dinler, düşünür, konuşur), altında
//! bugünün günlüğü ve önerilerle karşılama; konuşma başlayınca küre küçülüp yukarı çekilir,
//! döküm akar. Yazılı ve sesli turlar aynı dökümde. Onay isteyen adımlar (komut, bilgisayar
//! kontrolü) burada kart olarak görünür.

use std::cell::{Cell, RefCell};
use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer, WM_TIMER};

use crate::agent::Mode;
use crate::app::Shared;
use crate::brain::Role;
use crate::gfx::{Color, Gfx, Rect};
use crate::ui::*;
use crate::voice;

/// Kürenin canlandırma zamanlayıcısı.
const TIMER: usize = 0x686f;
const MAX_W: f32 = 760.0;
const SEND: f32 = 36.0;
/// Onay kartının yüksekliği (dökümün sonunda).
const ASK_H: f32 = 136.0;

/// Kürenin renkleri: ekran ışığıyla aynı aile.
pub const ORB: [u32; 4] = [0xff8a4c, 0xff4d8d, 0xb061ff, 0x4d8dff];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Input,
    Send,
    Mic,
    Clear,
    Orb,
    /// Öneri çipi.
    Chip(usize),
    /// Bugünün günlüğü.
    Today,
    /// Dökümdeki [[bağlantı]]: mesaj, sıra.
    Link(usize, usize),
    Yes,
    No,
}

/// Çizilmiş mesaj: düzen ve yükseklik; metin, genişlik ya da adımlar değişince yenilenir.
struct Drawn {
    key: (usize, i32, usize, usize, bool),
    layout: Option<IDWriteTextLayout>,
    h: f32,
    /// Adımların kapladığı yükseklik (metin bunun altında başlar).
    steps_h: f32,
}

pub struct HomePage {
    shared: Shared,
    hwnd: HWND,
    input: TextField,
    drawn: RefCell<Vec<Drawn>>,
    scroll: f32,
    stick_bottom: bool,
    hover: Hit,
    pressed: Hit,
    w: f32,
    h: f32,
    start: Instant,
    /// Kürenin yumuşatılmış seviyesi (zamanlayıcıda güncellenir).
    level: Cell<f32>,
    timer: bool,
    /// Zamanlayıcı tıkları: boştayken her üçünde bir çizilir.
    ticks: u32,
}

/// Öneriler: tıklanınca gönderilir.
fn chips() -> [&'static str; 3] {
    [
        t!("What's on my screen?", "Ekranımda ne var?"),
        t!("What did I do today?", "Bugün ne yaptım?"),
        t!("What's on my to-do list?", "Yapılacaklarımda ne var?"),
    ]
}

impl HomePage {
    pub fn new(shared: Shared, hwnd: HWND) -> Self {
        let mut input = TextField::new("", t!("Write something…", "Bir şey yaz…"));
        input.multiline = true;
        input.max_lines = 6;
        HomePage {
            shared,
            hwnd,
            input,
            drawn: Default::default(),
            scroll: 0.0,
            stick_bottom: true,
            hover: Hit::None,
            pressed: Hit::None,
            w: 0.0,
            h: 0.0,
            start: Instant::now(),
            level: Cell::new(0.0),
            timer: false,
            ticks: 0,
        }
    }

    fn redraw(&self) {
        crate::app::redraw(self.hwnd);
    }

    fn column(&self) -> (f32, f32) {
        let w = (self.w - 2.0 * PAD).min(MAX_W);
        let l = (self.w - w) / 2.0;
        (l, l + w)
    }

    fn empty(&self) -> bool {
        self.shared.borrow().brain.msgs.is_empty()
    }

    // --- Yerleşim ---

    fn bar_rect(&self, g: &Gfx) -> Rect {
        let (l, r) = self.column();
        let field_w = r - l - 2.0 * SEND - 20.0;
        let ih = self.input.height(g, field_w).max(44.0);
        Rect::new(l, self.h - 20.0 - ih, r, self.h - 20.0)
    }

    fn input_rect(&self, g: &Gfx) -> Rect {
        let b = self.bar_rect(g);
        Rect::new(b.l, b.t, b.r - 2.0 * SEND - 20.0, b.b)
    }

    fn mic_rect(&self, g: &Gfx) -> Rect {
        let b = self.bar_rect(g);
        let r = b.r - SEND - 8.0;
        Rect::new(r - SEND, b.b - SEND - 4.0, r, b.b - 4.0)
    }

    fn send_rect(&self, g: &Gfx) -> Rect {
        let b = self.bar_rect(g);
        Rect::new(b.r - SEND, b.b - SEND - 4.0, b.r, b.b - 4.0)
    }

    /// Küre: boşken büyük ve ortada, konuşmada küçük ve üstte. (merkez x, merkez y, yarıçap)
    fn orb_pos(&self) -> (f32, f32, f32) {
        if self.empty() {
            (self.w / 2.0, (HEADER + (self.h - HEADER) * 0.28).max(HEADER + 90.0), 62.0)
        } else {
            (self.w / 2.0, HEADER + 30.0, 17.0)
        }
    }

    fn content_rect(&self, g: &Gfx) -> Rect {
        let (l, r) = self.column();
        Rect::new(l, HEADER + 64.0, r, self.bar_rect(g).t - 14.0)
    }

    fn clear_rect(&self) -> Rect {
        let (_, r) = self.column();
        Rect::new(r - 34.0, HEADER + 14.0, r, HEADER + 46.0)
    }

    fn chip_rects(&self, g: &Gfx) -> Vec<Rect> {
        let (_, cy, rad) = self.orb_pos();
        let top = cy + rad + 112.0;
        let labels = chips();
        let widths: Vec<f32> = labels.iter().map(|c| g.measure(c, &g.f.text) + 28.0).collect();
        let total: f32 = widths.iter().sum::<f32>() + 8.0 * (labels.len() as f32 - 1.0);
        let mut x = self.w / 2.0 - total / 2.0;
        widths
            .iter()
            .map(|w| {
                let r = Rect::new(x, top, x + w, top + 32.0);
                x += w + 8.0;
                r
            })
            .collect()
    }

    /// Bugünün günlüğü: (saat, metin), en yeni dört satır.
    fn today(&self) -> Vec<(String, String)> {
        let (date, _) = crate::util::now();
        let s = self.shared.borrow();
        let Some(n) = s.vault.get(&format!("gunluk/{date}.md")) else { return Vec::new() };
        let mut rows: Vec<(String, String)> = crate::notes::md::strip_frontmatter(&n.body)
            .lines()
            .filter_map(|l| l.trim().strip_prefix("- "))
            .map(|l| {
                // Satır "SS:DD metin"; metnin başında saat bir daha yazılmışsa o da atılır.
                let (time, rest) = split_time(l).unwrap_or(("", l));
                let rest = split_time(rest).map_or(rest, |(_, r)| r);
                (time.to_string(), crate::notes::md::plain(rest).trim().to_string())
            })
            .collect();
        let n = rows.len();
        rows.drain(..n.saturating_sub(4));
        rows
    }

    /// Giriş kutusunun üstüne sığan günlük satırları.
    fn today_fit(&self, g: &Gfx) -> Vec<(String, String)> {
        let mut rows = self.today();
        let top = self.today_rect(0).b;
        let room = ((self.bar_rect(g).t - 16.0 - top) / 26.0).floor().max(0.0) as usize;
        let n = rows.len();
        rows.drain(..n.saturating_sub(room));
        rows
    }

    fn today_rect(&self, rows: usize) -> Rect {
        let (_, cy, rad) = self.orb_pos();
        let (l, r) = self.column();
        let w = (r - l).min(560.0);
        let l = self.w / 2.0 - w / 2.0;
        let t = cy + rad + 172.0;
        Rect::new(l, t, l + w, t + 28.0 + rows as f32 * 26.0)
    }

    /// Mesaj düzenlerini tazeler; toplam yüksekliği döndürür (onay kartı dahil).
    fn layout_msgs(&self, g: &Gfx) -> f32 {
        let c = self.content_rect(g);
        let s = self.shared.borrow();
        let msgs = &s.brain.msgs;
        let mut drawn = self.drawn.borrow_mut();
        drawn.truncate(msgs.len());
        let mut total = 0.0;
        for (i, m) in msgs.iter().enumerate() {
            let width = if m.role == Role::User { (c.w() * 0.78).max(120.0) - 28.0 } else { c.w() };
            let key = (i, width as i32, m.text.len(), m.steps.len(), m.error.is_some());
            if drawn.get(i).is_none_or(|d| d.key != key) {
                let shown = display_text(&m.text);
                let font = if m.role == Role::User { &g.f.body } else { &g.f.read };
                let layout = if shown.is_empty() { None } else { g.layout(&shown, font, width) };
                let mut text_h = layout.as_ref().map_or(0.0, Gfx::layout_height);
                if m.role == Role::User {
                    text_h = text_h.max(20.0) + 20.0;
                }
                let steps_h = if m.role == Role::Assistant && !m.steps.is_empty() { steps_shown(m.steps.len()) as f32 * 20.0 + 8.0 } else { 0.0 };
                let busy_h = if m.role == Role::Assistant && m.text.is_empty() { 24.0 } else { 0.0 };
                let err_h = if m.error.is_some() { 24.0 } else { 0.0 };
                let d = Drawn { key, layout, h: steps_h + text_h + busy_h + err_h, steps_h };
                if i < drawn.len() {
                    drawn[i] = d;
                } else {
                    drawn.push(d);
                }
            }
            total += drawn[i].h + gap(m.role);
        }
        if s.brain.ask.is_some() && s.brain.mode == Mode::Text {
            total += ASK_H + 14.0;
        }
        total
    }

    fn max_scroll(&self, g: &Gfx, total: f32) -> f32 {
        (total - self.content_rect(g).h()).max(0.0)
    }

    fn cur_scroll(&self, g: &Gfx) -> f32 {
        let total = self.layout_msgs(g);
        let max = self.max_scroll(g, total);
        if self.stick_bottom { max } else { self.scroll.min(max) }
    }

    /// Onay kartı (yazılı turda): dökümün sonunda.
    fn ask_rect(&self, g: &Gfx) -> Option<Rect> {
        {
            let s = self.shared.borrow();
            if s.brain.ask.is_none() || s.brain.mode != Mode::Text {
                return None;
            }
        }
        let c = self.content_rect(g);
        let total = self.layout_msgs(g);
        let top = c.t - self.cur_scroll(g) + total - ASK_H;
        Some(Rect::new(c.l, top, c.r, top + ASK_H))
    }

    fn ask_buttons(&self, g: &Gfx) -> Option<(Rect, Rect)> {
        let r = self.ask_rect(g)?;
        let yes = button_rect_right(g, r.r - 16.0, r.b - 50.0, t!("Run", "Çalıştır"), false);
        let no = button_rect_right(g, yes.l - 8.0, r.b - 50.0, t!("Cancel", "Vazgeç"), false);
        Some((yes, no))
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if self.input_rect(g).contains(x, y) {
            return Hit::Input;
        }
        if self.send_rect(g).contains(x, y) {
            return Hit::Send;
        }
        if self.mic_rect(g).contains(x, y) {
            return Hit::Mic;
        }
        let (cx, cy, rad) = self.orb_pos();
        if ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() < rad + 6.0 {
            return Hit::Orb;
        }
        if self.empty() {
            if let Some(k) = self.chip_rects(g).iter().position(|r| r.contains(x, y)) {
                return Hit::Chip(k);
            }
            let rows = self.today_fit(g).len();
            if rows > 0 && self.today_rect(rows).contains(x, y) {
                return Hit::Today;
            }
            return Hit::None;
        }
        if self.clear_rect().contains(x, y) && !self.shared.borrow().brain.busy {
            return Hit::Clear;
        }
        if let Some((yes, no)) = self.ask_buttons(g) {
            if yes.contains(x, y) {
                return Hit::Yes;
            }
            if no.contains(x, y) {
                return Hit::No;
            }
        }
        let c = self.content_rect(g);
        if !c.contains(x, y) {
            return Hit::None;
        }
        let scroll = self.cur_scroll(g);
        let s = self.shared.borrow();
        let drawn = self.drawn.borrow();
        let mut yy = c.t - scroll;
        for (i, m) in s.brain.msgs.iter().enumerate() {
            let Some(d) = drawn.get(i) else { break };
            if m.role == Role::Assistant
                && y >= yy
                && y < yy + d.h
                && let Some(l) = &d.layout
                && let Some(pos) = Gfx::hit_layout(l, x - c.l, y - (yy + d.steps_h))
            {
                for (k, (st, n)) in link_ranges(&m.text).into_iter().enumerate() {
                    if pos >= st && pos < st + n {
                        return Hit::Link(i, k);
                    }
                }
            }
            yy += d.h + gap(m.role);
        }
        Hit::None
    }

    fn send(&mut self) {
        let text = self.input.text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let ok = {
            let mut s = self.shared.borrow_mut();
            let cfg = s.config.clone();
            s.brain.send(&text, Mode::Text, &cfg)
        };
        if ok {
            self.input.clear();
            self.stick_bottom = true;
        }
        self.redraw();
    }

    // --- Çizim ---

    /// Kürenin durumu: (canlılık 0..1, dönüş hızı, seviye).
    fn orb_state(&self) -> (f32, f32, f32) {
        let s = self.shared.borrow();
        let lv = self.level.get();
        match s.voice_state {
            voice::State::Listening | voice::State::FollowUp | voice::State::Training => (1.0, 0.35, lv),
            voice::State::Speaking => (0.95, 0.25, lv),
            voice::State::Thinking | voice::State::Confirm => (0.85, 1.4, 0.0),
            voice::State::Idle if s.brain.busy => (0.85, 1.4, 0.0),
            voice::State::Idle => (0.62, 0.12, 0.0),
        }
    }

    fn paint_orb(&self, g: &Gfx) {
        let (cx, cy, base) = self.orb_pos();
        let t = self.start.elapsed().as_secs_f32();
        let (vivid, spin, lv) = self.orb_state();
        let r = base * (1.0 + 0.025 * (t * 1.6).sin()) * (1.0 + 0.22 * lv);
        // Hale.
        let aura = r * (2.1 + 0.6 * lv);
        g.radial(cx, cy, aura, aura, 0.0, 0.0, &[(0.0, Color(ORB[0], 0.20 * vivid)), (0.45, Color(ORB[1], 0.08 * vivid)), (1.0, Color(ORB[2], 0.0))]);
        // Gövde: koyu cam, içinde dönen renk bulutları.
        g.clip_circle(cx, cy, r, || {
            g.radial(cx, cy, r, r, 0.0, 0.0, &[(0.0, Color(0x1c1420, 1.0)), (1.0, Color(0x0d0a0c, 1.0))]);
            for (k, &c) in ORB.iter().enumerate() {
                let k = k as f32;
                let a = t * spin * (0.6 + 0.25 * k) + k * 1.7;
                let d = r * (0.38 + 0.08 * (t * 0.7 + k).sin());
                let (bx, by) = (cx + a.cos() * d, cy + a.sin() * d * 0.85);
                let br = r * (0.82 + 0.1 * (t * 0.9 + k * 2.0).cos() + 0.2 * lv);
                g.radial(bx, by, br, br, 0.0, 0.0, &[(0.0, Color(c, 0.85 * vivid)), (0.55, Color(c, 0.28 * vivid)), (1.0, Color(c, 0.0))]);
            }
            // Parlak çekirdek ve üstten ışık.
            let core = r * (0.42 + 0.25 * lv);
            g.radial(cx, cy, core, core, 0.0, 0.0, &[(0.0, Color(0xfff4ec, 0.55 * vivid)), (1.0, Color(0xfff4ec, 0.0))]);
            g.radial(cx - r * 0.3, cy - r * 0.42, r * 0.55, r * 0.35, 0.0, 0.0, &[(0.0, Color(0xffffff, 0.22)), (1.0, Color(0xffffff, 0.0))]);
        });
        g.ring(cx, cy, r, Color(0xffffff, if self.hover == Hit::Orb { 0.22 } else { 0.09 }), 1.0);
    }

    /// Kürenin yanındaki durum: ne yapıyor.
    fn status_line(&self) -> String {
        let s = self.shared.borrow();
        match s.voice_state {
            voice::State::Listening | voice::State::FollowUp => t!("Listening…", "Dinliyorum…").into(),
            voice::State::Training => t!("Learning your voice…", "Sesini öğreniyorum…").into(),
            voice::State::Speaking => s.config.name.clone(),
            voice::State::Confirm => t!("Waiting for your OK", "Onayını bekliyorum").into(),
            _ if s.brain.busy => {
                if s.brain.status.is_empty() { t!("Thinking…", "Düşünüyorum…").into() } else { crate::notes::md::plain(&s.brain.status) }
            }
            _ => String::new(),
        }
    }

    fn paint_welcome(&self, g: &Gfx) {
        let (cx, cy, rad) = self.orb_pos();
        let hour: u32 = crate::util::now().1[..2].parse().unwrap_or(12);
        let hello = match hour {
            5..=11 => t!("Good morning", "Günaydın"),
            12..=17 => t!("Hello", "Merhaba"),
            18..=22 => t!("Good evening", "İyi akşamlar"),
            _ => t!("Still up?", "Hâlâ ayakta mısın?"),
        };
        text_center(g, hello, &g.f.display, cx, cy + rad + 24.0, cy + rad + 66.0, pal().text);
        let status = self.status_line();
        let sub = if !status.is_empty() {
            status
        } else {
            let s = self.shared.borrow();
            let wake = &s.wake_phrase;
            if s.config.wake && s.config.groq_key.is_empty() && s.config.gemini_key.is_empty() && s.config.elevenlabs_key.is_empty() {
                t!("Add a free Groq key in Settings to talk", "Konuşmak için Ayarlar'dan ücretsiz Groq anahtarı ekle").into()
            } else if s.config.wake {
                t!(format!("Say \"{wake}\", or write"), format!("\"{wake}\" de, ya da yaz"))
            } else {
                t!(format!("Write, or hold {} and talk", s.config.hotkey), format!("Yaz ya da {} tuşuna basılı tutup konuş", s.config.hotkey))
            }
        };
        text_center(g, &sub, &g.f.text, cx, cy + rad + 66.0, cy + rad + 90.0, pal().muted);
        for (k, (r, label)) in self.chip_rects(g).iter().zip(chips()).enumerate() {
            let hov = self.hover == Hit::Chip(k);
            g.fill(*r, 16.0, if hov { pal().sel } else { pal().panel });
            g.stroke(*r, 16.0, if hov { accent().alpha(0.6) } else { pal().line }, 1.0);
            text_center(g, label, &g.f.text, r.l + r.w() / 2.0, r.t, r.b, if hov { pal().text } else { pal().muted });
        }
        let today = self.today_fit(g);
        if !today.is_empty() {
            let r = self.today_rect(today.len());
            if self.hover == Hit::Today {
                g.fill(Rect::new(r.l - 12.0, r.t - 8.0, r.r + 12.0, r.b + 4.0), 10.0, pal().hover);
            }
            g.text(t!("TODAY", "BUGÜN"), &g.f.small, Rect::new(r.l, r.t, r.r, r.t + 20.0), pal().faint);
            let mut y = r.t + 28.0;
            for (time, text) in today {
                g.text(&time, &g.f.small, Rect::new(r.l, y, r.l + 44.0, y + 22.0), accent());
                g.text(&text, &g.f.text, Rect::new(r.l + 50.0, y, r.r, y + 22.0), pal().muted);
                y += 26.0;
            }
        }
    }

    fn paint_transcript(&self, g: &Gfx) {
        let c = self.content_rect(g);
        let scroll = self.cur_scroll(g);
        let (cx, cy, rad) = self.orb_pos();
        let status = self.status_line();
        if !status.is_empty() {
            g.text(&status, &g.f.small, Rect::new(cx + rad + 12.0, cy - 10.0, c.r - 44.0, cy + 10.0), pal().muted);
        }
        if !self.shared.borrow().brain.busy {
            icon_button(g, self.clear_rect(), ICON_ADD, pal().faint, self.hover == Hit::Clear);
        }
        let s = self.shared.borrow();
        let busy = s.brain.busy;
        let t = self.start.elapsed().as_secs_f32();
        g.clip(Rect::new(0.0, c.t - 4.0, self.w, c.b + 4.0), || {
            let drawn = self.drawn.borrow();
            let mut y = c.t - scroll;
            let n = s.brain.msgs.len();
            for (i, m) in s.brain.msgs.iter().enumerate() {
                let Some(d) = drawn.get(i) else { break };
                if y + d.h >= c.t - 10.0 && y <= c.b + 10.0 {
                    match m.role {
                        Role::User => {
                            let lw = d.layout.as_ref().map_or(0.0, layout_width);
                            let bw = (lw + 28.0).min(c.w() * 0.78).max(48.0);
                            let r = Rect::new(c.r - bw, y, c.r, y + d.h);
                            g.fill(r, 16.0, accent().alpha(0.16));
                            if let Some(l) = &d.layout {
                                g.draw_layout(l, r.l + 14.0, y + 10.0, pal().text, &[]);
                            }
                            if m.mode == Mode::Voice {
                                g.text(ICON_MIC, &g.f.icon_small, Rect::new(r.l - 24.0, y, r.l - 6.0, y + 40.0), pal().faint);
                            }
                        }
                        Role::Assistant => {
                            let mut ty = y;
                            let shown = steps_shown(m.steps.len());
                            let skip = m.steps.len() - shown;
                            for (k, st) in m.steps.iter().skip(skip).enumerate() {
                                let plain = crate::notes::md::plain(st);
                                let label = if k == 0 && skip > 0 { format!("+{skip} · {plain}") } else { plain };
                                g.text("\u{E76C}", &g.f.icon_small, Rect::new(c.l, ty, c.l + 14.0, ty + 20.0), pal().faint);
                                g.text(&label, &g.f.small, Rect::new(c.l + 20.0, ty, c.r, ty + 20.0), pal().faint);
                                ty += 20.0;
                            }
                            ty = y + d.steps_h;
                            if let Some(l) = &d.layout {
                                let colors: Vec<(u32, u32, Color)> = link_ranges(&m.text)
                                    .into_iter()
                                    .enumerate()
                                    .map(|(k, (st, n))| (st, n, if self.hover == Hit::Link(i, k) { pal().text } else { accent() }))
                                    .collect();
                                g.draw_layout(l, c.l, ty, pal().text, &colors);
                            } else if busy && i + 1 == n {
                                // Bekleme: üç nokta dalgası.
                                for k in 0..3 {
                                    let a = 0.25 + 0.75 * ((t * 4.0 - k as f32 * 0.6).sin() * 0.5 + 0.5);
                                    g.circle(c.l + 6.0 + k as f32 * 12.0, ty + 12.0, 3.2, pal().muted.alpha(a));
                                }
                            }
                            if let Some(e) = &m.error {
                                g.text(e, &g.f.small, Rect::new(c.l, y + d.h - 22.0, c.r, y + d.h - 2.0), pal().red);
                            }
                        }
                    }
                }
                y += d.h + gap(m.role);
            }
        });
        drop(s);
        self.paint_ask(g);
    }

    fn paint_ask(&self, g: &Gfx) {
        let Some(r) = self.ask_rect(g) else { return };
        let Some(ask) = self.shared.borrow().brain.ask.clone() else { return };
        g.fill(r, 14.0, pal().panel);
        g.stroke(r, 14.0, accent().alpha(0.55), 1.0);
        g.text("\u{E7BA}", &g.f.icon, Rect::new(r.l + 16.0, r.t + 14.0, r.l + 36.0, r.t + 38.0), accent());
        g.text(&ask.question, &g.f.strong, Rect::new(r.l + 44.0, r.t + 14.0, r.r - 16.0, r.t + 38.0), pal().text);
        let code = Rect::new(r.l + 16.0, r.t + 46.0, r.r - 16.0, r.t + 76.0);
        g.fill(code, 8.0, Color(0x000000, 0.25));
        g.text(&ask.detail, &g.f.mono, Rect::new(code.l + 10.0, code.t, code.r - 10.0, code.b), pal().text);
        if let Some((yes, no)) = self.ask_buttons(g) {
            button(g, yes, t!("Run", "Çalıştır"), None, Some(accent()), self.hover == Hit::Yes);
            button(g, no, t!("Cancel", "Vazgeç"), None, None, self.hover == Hit::No);
        }
    }

    fn paint_bar(&self, g: &Gfx) {
        self.input.draw(g, self.input_rect(g), self.hover == Hit::Input);
        let (listening, busy) = {
            let s = self.shared.borrow();
            (matches!(s.voice_state, voice::State::Listening | voice::State::FollowUp), s.brain.busy)
        };
        // Mikrofon: dinlerken vurgu renginde.
        let m = self.mic_rect(g);
        let (mx, my) = (m.l + m.w() / 2.0, m.t + m.h() / 2.0);
        if listening {
            g.circle(mx, my, SEND / 2.0, accent().alpha(0.22));
        } else if self.hover == Hit::Mic {
            g.circle(mx, my, SEND / 2.0, pal().hover);
        }
        g.text(ICON_MIC, &g.f.icon, m, if listening { accent() } else { pal().muted });
        // Gönder; çalışırken durdur.
        let sr = self.send_rect(g);
        let (sx, sy) = (sr.l + sr.w() / 2.0, sr.t + sr.h() / 2.0);
        let can = busy || !self.input.text.trim().is_empty();
        let fill = if can { accent().alpha(if self.hover == Hit::Send { 0.88 } else { 1.0 }) } else { pal().hover };
        g.circle(sx, sy, SEND / 2.0, fill);
        let glyph = if busy { "\u{E71A}" } else { "\u{E74A}" };
        g.text(glyph, &g.f.icon_small, sr, if can { on_accent() } else { pal().faint });
    }
}

/// "14:28 metin" → ("14:28", "metin").
fn split_time(l: &str) -> Option<(&str, &str)> {
    let (t, r) = l.split_once(' ')?;
    (t.len() == 5 && t.as_bytes()[2] == b':' && t.bytes().filter(u8::is_ascii_digit).count() == 4).then_some((t, r))
}

fn gap(role: Role) -> f32 {
    if role == Role::User { 14.0 } else { 26.0 }
}

fn steps_shown(n: usize) -> usize {
    n.min(4)
}

fn layout_width(l: &IDWriteTextLayout) -> f32 {
    let mut m = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
    unsafe {
        let _ = l.GetMetrics(&mut m);
    }
    m.widthIncludingTrailingWhitespace
}

/// Gösterilen metin: `[[hedef|ad]]` → ad, `[[hedef]]` → hedef; `**` ve `` ` `` atılır. Yanında
/// bağlantıların gösterilen metindeki UTF-16 aralıkları ve hedefleri.
fn display(t: &str) -> (String, Vec<(u32, u32, String)>) {
    let mut out = String::new();
    let mut links = Vec::new();
    let mut rest = t;
    let mut u16len = 0u32;
    let push = |out: &mut String, s: &str, u16len: &mut u32| {
        let s = s.replace("**", "").replace('`', "");
        *u16len += s.encode_utf16().count() as u32;
        out.push_str(&s);
    };
    while let Some(i) = rest.find("[[") {
        push(&mut out, &rest[..i], &mut u16len);
        let after = &rest[i + 2..];
        let Some(j) = after.find("]]") else {
            push(&mut out, "[[", &mut u16len);
            rest = after;
            continue;
        };
        let inner = &after[..j];
        let (target, shown) = inner.split_once('|').map_or((inner, inner), |(t, a)| (t, a));
        let target = target.split('#').next().unwrap_or("").trim().to_string();
        let start = u16len;
        push(&mut out, shown.trim(), &mut u16len);
        links.push((start, u16len - start, target));
        rest = &after[j + 2..];
    }
    push(&mut out, rest, &mut u16len);
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    (out, links)
}

fn display_text(t: &str) -> String {
    display(t).0
}

fn link_targets(t: &str) -> Vec<String> {
    display(t).1.into_iter().map(|l| l.2).collect()
}

fn link_ranges(raw: &str) -> Vec<(u32, u32)> {
    display(raw).1.into_iter().map(|l| (l.0, l.1)).collect()
}

impl Page for HomePage {
    fn message(&mut self, _g: &mut Gfx, msg: u32, wp: WPARAM, _lp: LPARAM) -> Option<LRESULT> {
        if msg == WM_TIMER && wp.0 == TIMER {
            let (target, active) = {
                let s = self.shared.borrow();
                (s.level, s.voice_state != voice::State::Idle || s.brain.busy)
            };
            let cur = self.level.get();
            // Hızlı yüksel, yavaş in.
            let k = if target > cur { 0.5 } else { 0.12 };
            self.level.set(cur + (target - cur) * k);
            // Boştayken küre yavaş nefes alır: saniyede 10 kare yeter.
            self.ticks = self.ticks.wrapping_add(1);
            if active || cur > 0.01 || self.ticks % 3 == 0 {
                self.redraw();
            }
            return Some(LRESULT(0));
        }
        None
    }

    fn layout(&mut self, w: f32, h: f32, _head_x: f32, _head_r: f32) {
        self.w = w;
        self.h = h;
    }

    fn paint(&self, g: &Gfx) {
        self.paint_orb(g);
        if self.empty() {
            self.paint_welcome(g);
        } else {
            self.paint_transcript(g);
        }
        self.paint_bar(g);
    }

    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        let r = self.input_rect(g);
        self.input.mouse_move(g, r, x, y);
        let hit = self.hit(g, x, y);
        if hit != self.hover {
            self.hover = hit;
        }
        self.redraw();
    }

    fn mouse_leave(&mut self) {
        if self.hover != Hit::None {
            self.hover = Hit::None;
            self.redraw();
        }
    }

    fn mouse_down(&mut self, g: &Gfx, x: f32, y: f32) {
        self.pressed = self.hit(g, x, y);
        if self.pressed == Hit::Input {
            let r = self.input_rect(g);
            self.input.mouse_down(g, r, x, y);
        } else if !matches!(self.pressed, Hit::Send | Hit::Mic) {
            self.input.focused = false;
        }
        self.redraw();
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        self.input.mouse_up();
        let hit = self.hit(g, x, y);
        if hit != std::mem::replace(&mut self.pressed, Hit::None) {
            return;
        }
        let req = |s: &Shared, r: crate::pages::VoiceReq| s.borrow_mut().voice_req = Some(r);
        match hit {
            Hit::Send => {
                if self.shared.borrow().brain.busy {
                    req(&self.shared, crate::pages::VoiceReq::Stop);
                } else {
                    self.send();
                }
            }
            Hit::Mic | Hit::Orb => {
                let listening = matches!(self.shared.borrow().voice_state, voice::State::Listening | voice::State::FollowUp);
                req(&self.shared, if listening { crate::pages::VoiceReq::Stop } else { crate::pages::VoiceReq::Listen });
            }
            Hit::Clear => self.shared.borrow_mut().brain.clear(),
            Hit::Chip(k) => {
                self.input.set_text(chips()[k]);
                self.send();
            }
            Hit::Today => {
                let (date, _) = crate::util::now();
                let mut s = self.shared.borrow_mut();
                s.open_note = Some(format!("gunluk/{date}.md"));
                s.open_tab = Some(crate::app::Tab::Memory);
            }
            Hit::Yes | Hit::No => self.shared.borrow_mut().brain.answer(hit == Hit::Yes),
            Hit::Link(i, k) => {
                let target = self.shared.borrow().brain.msgs.get(i).map(|m| link_targets(&m.text)).and_then(|v| v.get(k).cloned());
                if let Some(t) = target {
                    let mut s = self.shared.borrow_mut();
                    s.open_note = Some(t);
                    s.open_tab = Some(crate::app::Tab::Memory);
                }
            }
            _ => {}
        }
        self.redraw();
    }

    fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        let ir = self.input_rect(g);
        if ir.contains(x, y) && self.input.wheel(g, ir, delta) {
            self.redraw();
            return;
        }
        let total = self.layout_msgs(g);
        let max = self.max_scroll(g, total);
        let cur = if self.stick_bottom { max } else { self.scroll };
        self.scroll = (cur - delta * 60.0).clamp(0.0, max);
        self.stick_bottom = self.scroll >= max - 1.0;
        self.hover = self.hit(g, x, y);
        self.redraw();
    }

    fn key(&mut self, vk: u16) -> bool {
        if !self.input.focused {
            return false;
        }
        if self.input.key(vk) {
            self.send();
        }
        self.redraw();
        true
    }

    fn char(&mut self, c: char) -> bool {
        // Ana sayfada yazılan her şey kutuya gider.
        if !self.input.focused && !c.is_control() {
            self.input.focused = true;
        }
        if self.input.char(c) {
            self.redraw();
            return true;
        }
        false
    }

    fn interactive(&self, _g: &Gfx, _x: f32, y: f32) -> bool {
        y >= HEADER
    }

    fn set_visible(&mut self, visible: bool) {
        if visible {
            self.input.focused = true;
            if !self.timer {
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER, 33, None);
                }
                self.timer = true;
            }
        } else {
            self.hover = Hit::None;
            if self.timer {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER);
                }
                self.timer = false;
            }
        }
    }

    fn kill_focus(&mut self) {
        self.input.focused = false;
    }
}
