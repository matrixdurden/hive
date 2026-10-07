//! Pencere önizlemesi: çalışan bir uygulamanın dock simgesinin üstünde kısa bir süre durunca
//! (ya da birden çok penceresi varken tıklayınca) açılan, Windows görev çubuğunun önizlemesi
//! gibi küçük pano. Her pencere bir kart: uygulamanın simgesi ve başlığı, altında canlı küçük
//! resmi (DWM çizer, kopyalama yok). Tıklanan pencere öne gelir; çarpı ya da orta tık kapatır.
//! Küçültülmüş pencerelerin canlı görüntüsü olmadığından kartında simge durur.
//!
//! Windows panelleri gibi cam (acrylic) zemin, tema renkleri; açılırken aşağıdan kayar. Dock'a
//! fare girdi / çıktı / iş bitti haberini mesajla verir; kapanma kararını dock verir.

use std::cell::RefCell;
use std::time::Instant;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use super::apps::{self, IconSource};
use super::theme::{self, Theme};
use crate::gfx::{Color, Gfx, ImageId, Rect};

const CLASS: PCWSTR = w!("hive-hatter-stack");
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_SLIDE: usize = 1;

/// Dock'a giden haberler.
pub const WM_STACK_ENTER: u32 = WM_APP + 10;
pub const WM_STACK_LEAVE: u32 = WM_APP + 11;
pub const WM_STACK_DONE: u32 = WM_APP + 12;

// Ölçüler (DIP).
const THUMB_W: f32 = 200.0;
const THUMB_H: f32 = 120.0;
const CARD_PAD: f32 = 6.0;
const TITLE_H: f32 = 30.0;
const GAP: f32 = 4.0;
const PAD: f32 = 6.0;
const COLS: usize = 4;
const SLIDE: f32 = 10.0;
const SLIDE_MS: f32 = 140.0;

const ICON_CLOSE: &str = "\u{E8BB}";

struct Card {
    hwnd: HWND,
    title: String,
    /// DWM küçük resmi (küçültülmüşse yok).
    thumb: Option<isize>,
    rect: Rect,
}

struct Stack {
    hwnd: HWND,
    dock: HWND,
    item: String,
    gfx: Gfx,
    t: Theme,
    icon: Option<ImageId>,
    cards: Vec<Card>,
    hover: Option<usize>,
    close_hover: bool,
    tracking: bool,
    dpi: f32,
    /// Simgenin ortası ve panonun alt kenarı (ekran pikseli), ekranın sınırları.
    anchor: (i32, i32),
    monitor: RECT,
    opened: Instant,
}

thread_local! {
    static STACK: RefCell<Option<Stack>> = const { RefCell::new(None) };
    /// Kapanan panonun çizim araçları: bir sonraki açılışta yeniden kullanılır.
    static SPARE: RefCell<Option<Gfx>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Stack) -> R) -> Option<R> {
    STACK.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn card_size() -> (f32, f32) {
    (THUMB_W + 2.0 * CARD_PAD, TITLE_H + THUMB_H + CARD_PAD)
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

impl Stack {
    fn k(&self) -> f32 {
        self.dpi / 96.0
    }

    fn close_rect(c: &Card) -> Rect {
        let r = c.rect;
        Rect::new(r.r - CARD_PAD - 26.0, r.t + 3.0, r.r - CARD_PAD, r.t + TITLE_H - 3.0)
    }

    fn size(&self) -> (f32, f32) {
        let n = self.cards.len().max(1);
        let (cols, rows) = (n.min(COLS), n.div_ceil(COLS));
        let (cw, ch) = card_size();
        (cols as f32 * cw + (cols - 1) as f32 * GAP + 2.0 * PAD, rows as f32 * ch + (rows - 1) as f32 * GAP + 2.0 * PAD)
    }

    /// Pencerenin yeri (açılırken aşağıdan kayar) ve küçük resimlerin yeri.
    fn place(&self) {
        let (w, h) = self.size();
        let k = self.k();
        let (wp, hp) = ((w * k).round() as i32, (h * k).round() as i32);
        let m = self.monitor;
        let x = (self.anchor.0 - wp / 2).clamp(m.left + 8, (m.right - wp - 8).max(m.left + 8));
        let progress = ease_out(self.opened.elapsed().as_secs_f32() * 1000.0 / SLIDE_MS);
        let y = (self.anchor.1 - hp).max(m.top + 8) + ((1.0 - progress) * SLIDE * k).round() as i32;
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, wp, hp, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }

    /// Kartları dizer, küçük resimleri yerleştirir.
    fn layout(&mut self) {
        let (cw, ch) = card_size();
        for (i, c) in self.cards.iter_mut().enumerate() {
            let (col, row) = ((i % COLS) as f32, (i / COLS) as f32);
            let l = PAD + col * (cw + GAP);
            let t = PAD + row * (ch + GAP);
            c.rect = Rect::new(l, t, l + cw, t + ch);
        }
        let k = self.k();
        for c in &self.cards {
            let Some(t) = c.thumb else { continue };
            let src = unsafe { DwmQueryThumbnailSourceSize(t) }.unwrap_or(SIZE { cx: 16, cy: 9 });
            let (sw, sh) = (src.cx.max(1) as f32, src.cy.max(1) as f32);
            let s = (THUMB_W / sw).min(THUMB_H / sh);
            let (tw, th) = (sw * s, sh * s);
            let l = c.rect.l + CARD_PAD + (THUMB_W - tw) / 2.0;
            let top = c.rect.t + TITLE_H + (THUMB_H - th) / 2.0;
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: RECT {
                    left: (l * k).round() as i32,
                    top: (top * k).round() as i32,
                    right: ((l + tw) * k).round() as i32,
                    bottom: ((top + th) * k).round() as i32,
                },
                opacity: 255,
                fVisible: true.into(),
                fSourceClientAreaOnly: false.into(),
                ..Default::default()
            };
            if let Err(e) = unsafe { DwmUpdateThumbnailProperties(t, &props) } {
                crate::log!("hatter: önizleme yerleştirilemedi: {e}");
            }
        }
        self.place();
        self.redraw();
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn set_cards(&mut self, windows: &[HWND]) {
        for c in self.cards.drain(..) {
            if let Some(t) = c.thumb {
                unsafe {
                    let _ = DwmUnregisterThumbnail(t);
                }
            }
        }
        for &h in windows {
            let thumb = if unsafe { IsIconic(h).as_bool() } {
                None
            } else {
                match unsafe { DwmRegisterThumbnail(self.hwnd, h) } {
                    Ok(t) => Some(t),
                    Err(e) => {
                        crate::log!("hatter: önizleme kaydedilemedi: {e}");
                        None
                    }
                }
            };
            self.cards.push(Card { hwnd: h, title: apps::title(h), thumb, rect: Rect::default() });
        }
        self.hover = None;
        self.layout();
    }

    fn paint(&mut self) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(self.hwnd, &mut ps);
            let _ = EndPaint(self.hwnd, &ps);
        }
        let t = self.t;
        if !self.gfx.begin(self.hwnd, self.dpi, t.tint) {
            return;
        }
        let g = &self.gfx;
        for (i, c) in self.cards.iter().enumerate() {
            let hovered = self.hover == Some(i);
            if hovered {
                g.fill(c.rect, 6.0, t.fill_hover);
            }
            let mut x = c.rect.l + CARD_PAD + 2.0;
            if let Some(icon) = self.icon {
                let cy = c.rect.t + TITLE_H / 2.0;
                g.image(icon, Rect::new(x, cy - 8.0, x + 16.0, cy + 8.0), 1.0);
                x += 24.0;
            }
            let right = if hovered { c.rect.r - CARD_PAD - 30.0 } else { c.rect.r - CARD_PAD };
            g.text(&c.title, &g.f.small, Rect::new(x, c.rect.t, right, c.rect.t + TITLE_H), t.text);
            if hovered {
                let cr = Self::close_rect(c);
                if self.close_hover {
                    g.fill(cr, 4.0, Color::rgb(0xc42b1c));
                }
                g.text(ICON_CLOSE, &g.f.icon_caption, cr, t.text);
            }
            if c.thumb.is_none() {
                let top = c.rect.t + TITLE_H;
                let r = Rect::new(c.rect.l + CARD_PAD, top, c.rect.r - CARD_PAD, top + THUMB_H);
                g.fill(r, 4.0, t.fill);
                if let Some(icon) = self.icon {
                    let (cx, cy) = (r.l + r.w() / 2.0, r.cy());
                    g.image(icon, Rect::new(cx - 24.0, cy - 24.0, cx + 24.0, cy + 24.0), 0.9);
                }
            }
        }
        if self.gfx.end() {
            self.redraw();
        }
    }

    fn hit(&self, x: f32, y: f32) -> (Option<usize>, bool) {
        match self.cards.iter().position(|c| c.rect.contains(x, y)) {
            Some(i) => (Some(i), Self::close_rect(&self.cards[i]).contains(x, y)),
            None => (None, false),
        }
    }

    fn tell_dock(&self, msg: u32) {
        unsafe {
            let _ = PostMessageW(Some(self.dock), msg, WPARAM(0), LPARAM(0));
        }
    }
}

enum After {
    Activate(HWND),
    Close(HWND),
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let after = match msg {
        WM_PAINT => {
            if with(|s| s.paint()).is_none() {
                return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
            }
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER if wp.0 == TIMER_SLIDE => {
            with(|s| {
                s.place();
                if s.opened.elapsed().as_secs_f32() * 1000.0 >= SLIDE_MS {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_SLIDE);
                    }
                }
            });
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            with(|s| {
                if !s.tracking {
                    let mut t = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: s.hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut t);
                    }
                    s.tracking = true;
                    s.tell_dock(WM_STACK_ENTER);
                }
                let k = s.k();
                let x = (lp.0 & 0xffff) as i16 as f32 / k;
                let y = ((lp.0 >> 16) & 0xffff) as i16 as f32 / k;
                let (hover, close) = s.hit(x, y);
                if (hover, close) != (s.hover, s.close_hover) {
                    s.hover = hover;
                    s.close_hover = close;
                    s.redraw();
                }
            });
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with(|s| {
                s.tracking = false;
                s.hover = None;
                s.close_hover = false;
                s.redraw();
                s.tell_dock(WM_STACK_LEAVE);
            });
            return LRESULT(0);
        }
        WM_LBUTTONUP => with(|s| {
            let i = s.hover?;
            let h = s.cards[i].hwnd;
            Some(if s.close_hover { After::Close(h) } else { After::Activate(h) })
        })
        .flatten(),
        WM_MBUTTONUP => with(|s| s.hover.map(|i| After::Close(s.cards[i].hwnd))).flatten(),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    };
    match after {
        Some(After::Activate(h)) => {
            let dock = with(|s| s.dock);
            close();
            apps::activate(h);
            if let Some(d) = dock {
                unsafe {
                    let _ = PostMessageW(Some(d), WM_STACK_DONE, WPARAM(0), LPARAM(0));
                }
            }
        }
        Some(After::Close(h)) => {
            apps::close(h);
            // Kart hemen düşer; pencere kapanmayı reddederse (kaydedilmemiş belge) dock'un bir
            // sonraki yenilemesi onu geri getirir.
            let left = with(|s| {
                let rest: Vec<HWND> = s.cards.iter().map(|c| c.hwnd).filter(|&w| w != h).collect();
                if !rest.is_empty() {
                    s.set_cards(&rest);
                }
                (rest.len(), s.dock)
            });
            if let Some((0, d)) = left {
                close();
                unsafe {
                    let _ = PostMessageW(Some(d), WM_STACK_DONE, WPARAM(0), LPARAM(0));
                }
            }
        }
        None => {}
    }
    LRESULT(0)
}

/// Panoyu açar (açıksa içeriğini değiştirir). `anchor`: simgenin ortası ve panonun alt kenarı.
pub fn open(dock: HWND, item: &str, icon: &IconSource, windows: &[HWND], anchor: (i32, i32), monitor: RECT, dpi: f32) {
    let same = with(|s| s.item == item).unwrap_or(false);
    if same {
        with(|s| s.set_cards(windows));
        return;
    }
    close();
    let t = theme::current();
    let created = (|| -> windows::core::Result<Stack> {
        unsafe {
            let inst = GetModuleHandleW(None)?;
            let wc = WNDCLASSW {
                lpfnWndProc: Some(proc),
                hInstance: inst.into(),
                lpszClassName: CLASS,
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                ..Default::default()
            };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                CLASS,
                w!("hatter"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(inst.into()),
                None,
            )?;
            theme::backdrop(hwnd, &t);
            let mut gfx = match SPARE.with(|s| s.borrow_mut().take()) {
                Some(g) => g,
                None => {
                    let mut g = Gfx::new().map_err(|e| windows::core::Error::new(E_FAIL, e.to_string()))?;
                    g.set_transparent(true);
                    g
                }
            };
            gfx.reset_target();
            gfx.truncate_images(0);
            let icon = apps::icon(gfx.wic(), icon, (32.0 * dpi / 96.0) as i32).and_then(|b| gfx.load_bitmap_source(&b).ok());
            Ok(Stack {
                hwnd,
                dock,
                item: item.to_string(),
                gfx,
                t,
                icon,
                cards: Vec::new(),
                hover: None,
                close_hover: false,
                tracking: false,
                dpi,
                anchor,
                monitor,
                opened: Instant::now(),
            })
        }
    })();
    match created {
        Ok(s) => {
            let hwnd = s.hwnd;
            STACK.with_borrow_mut(|st| *st = Some(s));
            with(|s| {
                s.set_cards(windows);
                s.opened = Instant::now();
                s.place();
            });
            unsafe {
                SetTimer(Some(hwnd), TIMER_SLIDE, 8, None);
            }
        }
        Err(e) => crate::log!("hatter: önizleme açılamadı: {e}"),
    }
}

pub fn close() {
    let Some(s) = STACK.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.take())) else { return };
    unsafe {
        for c in &s.cards {
            if let Some(t) = c.thumb {
                let _ = DwmUnregisterThumbnail(t);
            }
        }
        let _ = KillTimer(Some(s.hwnd), TIMER_SLIDE);
        let _ = DestroyWindow(s.hwnd);
    }
    let Stack { gfx, .. } = s;
    SPARE.with(|sp| *sp.borrow_mut() = Some(gfx));
}

/// Açık panonun uygulaması.
pub fn current() -> Option<String> {
    with(|s| s.item.clone())
}

/// Dock yenilenince pencereler değiştiyse kartları günceller.
pub fn update(windows: &[HWND]) {
    with(|s| {
        if s.cards.len() != windows.len() || s.cards.iter().zip(windows).any(|(c, w)| c.hwnd != *w) {
            s.set_cards(windows);
        }
    });
}
