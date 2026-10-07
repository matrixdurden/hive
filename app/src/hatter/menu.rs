//! Dock'un sağ tık menüsü, Windows 11 bağlam menüsü gibi: cam (acrylic) zemin, tema renkleri,
//! yuvarlak köşeler, her satırda Fluent simgesi, üstte uygulamanın simgesi ve adı. Simgenin tam
//! üstünde açılır, açılırken hafifçe yukarı kayar. Fareyle ya da oklar / Enter / Esc ile
//! kullanılır; başka yere tıklanınca kapanır. Seçilen satır dock'a mesajla bildirilir.

use std::cell::RefCell;
use std::time::Instant;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use super::apps::{self, IconSource};
use super::theme::{self, Theme};
use crate::gfx::{Gfx, ImageId, Rect};

const CLASS: PCWSTR = w!("hive-hatter-menu");
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_SLIDE: usize = 1;

/// Dock'a: seçilen satır (wParam; 0 = vazgeçildi).
pub const WM_MENU_DONE: u32 = WM_APP + 13;

const PAD: f32 = 4.0;
const ROW: f32 = 34.0;
const HEAD: f32 = 44.0;
const SEP: f32 = 9.0;
const MIN_W: f32 = 220.0;
const MAX_W: f32 = 340.0;
const SLIDE: f32 = 8.0;
const SLIDE_MS: f32 = 120.0;

pub enum Glyph {
    Fluent(&'static str),
    /// Uygulamanın simgesi (pencere satırları).
    App,
}

/// `id` 0 olan satır ayraçtır.
pub struct Entry {
    pub id: usize,
    pub text: String,
    pub glyph: Option<Glyph>,
}

impl Entry {
    pub fn sep() -> Self {
        Entry { id: 0, text: String::new(), glyph: None }
    }
}

struct Menu {
    hwnd: HWND,
    dock: HWND,
    gfx: Gfx,
    t: Theme,
    title: String,
    icon: Option<ImageId>,
    entries: Vec<Entry>,
    rows: Vec<Rect>,
    width: f32,
    height: f32,
    hover: Option<usize>,
    tracking: bool,
    dpi: f32,
    anchor: (i32, i32),
    monitor: RECT,
    opened: Instant,
    done: bool,
}

thread_local! {
    static MENU: RefCell<Option<Menu>> = const { RefCell::new(None) };
    static SPARE: RefCell<Option<Gfx>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Menu) -> R) -> Option<R> {
    MENU.with(|m| m.try_borrow_mut().ok().and_then(|mut m| m.as_mut().map(f)))
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

impl Menu {
    fn k(&self) -> f32 {
        self.dpi / 96.0
    }

    fn layout(&mut self) {
        let g = &self.gfx;
        let text_w = self
            .entries
            .iter()
            .map(|e| g.measure(&e.text, &g.f.text))
            .chain([g.measure(&self.title, &g.f.strong) + 8.0])
            .fold(0.0, f32::max);
        self.width = (text_w + 16.0 + 16.0 + 14.0 + 24.0 + 2.0 * PAD).clamp(MIN_W, MAX_W);
        let mut y = PAD + HEAD;
        self.rows = self
            .entries
            .iter()
            .map(|e| {
                let h = if e.id == 0 { SEP } else { ROW };
                let r = Rect::new(PAD, y, self.width - PAD, y + h);
                y += h;
                r
            })
            .collect();
        self.height = y + PAD;
    }

    fn place(&self) {
        let k = self.k();
        let (wp, hp) = ((self.width * k).round() as i32, (self.height * k).round() as i32);
        let m = self.monitor;
        let x = (self.anchor.0 - wp / 2).clamp(m.left + 8, (m.right - wp - 8).max(m.left + 8));
        let progress = ease_out(self.opened.elapsed().as_secs_f32() * 1000.0 / SLIDE_MS);
        let y = (self.anchor.1 - hp).max(m.top + 8) + ((1.0 - progress) * SLIDE * k).round() as i32;
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, wp, hp, SWP_NOACTIVATE);
        }
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
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
        // Başlık: uygulamanın simgesi ve adı.
        let head = Rect::new(PAD + 10.0, PAD, self.width - PAD - 10.0, PAD + HEAD);
        let mut tx = head.l;
        if let Some(icon) = self.icon {
            g.image(icon, Rect::new(tx, head.cy() - 12.0, tx + 24.0, head.cy() + 12.0), 1.0);
            tx += 34.0;
        }
        g.text(&self.title, &g.f.strong, Rect::new(tx, head.t, head.r, head.b), t.text);
        g.fill(Rect::new(PAD + 8.0, head.b - 1.0, self.width - PAD - 8.0, head.b), 0.0, t.stroke);

        for (i, (e, r)) in self.entries.iter().zip(&self.rows).enumerate() {
            if e.id == 0 {
                g.fill(Rect::new(r.l + 8.0, r.cy(), r.r - 8.0, r.cy() + 1.0), 0.0, t.stroke);
                continue;
            }
            if self.hover == Some(i) {
                g.fill(*r, 4.0, t.fill_hover);
            }
            let icon_r = Rect::new(r.l + 10.0, r.cy() - 8.0, r.l + 26.0, r.cy() + 8.0);
            match &e.glyph {
                Some(Glyph::Fluent(ch)) => g.text(ch, &g.f.icon_small, Rect::new(icon_r.l - 2.0, r.t, icon_r.r + 2.0, r.b), t.text),
                Some(Glyph::App) => {
                    if let Some(icon) = self.icon {
                        g.image(icon, icon_r, 1.0);
                    }
                }
                None => {}
            }
            g.text(&e.text, &g.f.text, Rect::new(icon_r.r + 12.0, r.t, r.r - 10.0, r.b), t.text);
        }
        if self.gfx.end() {
            self.redraw();
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<usize> {
        self.rows.iter().zip(&self.entries).position(|(r, e)| e.id != 0 && r.contains(x, y))
    }

    /// Ok tuşlarıyla bir sonraki seçilebilir satır.
    fn step(&mut self, down: bool) {
        let n = self.entries.len();
        if n == 0 {
            return;
        }
        let mut i = self.hover.unwrap_or(if down { n - 1 } else { 0 });
        for _ in 0..n {
            i = if down { (i + 1) % n } else { (i + n - 1) % n };
            if self.entries[i].id != 0 {
                self.hover = Some(i);
                break;
            }
        }
        self.redraw();
    }
}

fn finish(choice: usize) {
    let dock = with(|m| {
        m.done = true;
        m.dock
    });
    close();
    if let Some(d) = dock {
        unsafe {
            let _ = PostMessageW(Some(d), WM_MENU_DONE, WPARAM(choice), LPARAM(0));
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let dip = |k: f32| ((lp.0 & 0xffff) as i16 as f32 / k, ((lp.0 >> 16) & 0xffff) as i16 as f32 / k);
    match msg {
        WM_PAINT => {
            if with(|m| m.paint()).is_none() {
                return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
            }
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_TIMER if wp.0 == TIMER_SLIDE => {
            with(|m| {
                m.place();
                if m.opened.elapsed().as_secs_f32() * 1000.0 >= SLIDE_MS {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_SLIDE);
                    }
                }
            });
        }
        WM_ACTIVATE => {
            if wp.0 & 0xffff == WA_INACTIVE as usize {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
        WM_CLOSE => finish(0),
        WM_KEYDOWN => match VIRTUAL_KEY(wp.0 as u16) {
            VK_ESCAPE => finish(0),
            VK_DOWN => {
                with(|m| m.step(true));
            }
            VK_UP => {
                with(|m| m.step(false));
            }
            VK_RETURN => {
                if let Some(id) = with(|m| m.hover.map(|i| m.entries[i].id)).flatten() {
                    finish(id);
                }
            }
            _ => {}
        },
        WM_MOUSEMOVE => {
            with(|m| {
                if !m.tracking {
                    let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    unsafe {
                        let _ = TrackMouseEvent(&mut t);
                    }
                    m.tracking = true;
                }
                let (x, y) = dip(m.k());
                let h = m.hit(x, y);
                if h != m.hover {
                    m.hover = h;
                    m.redraw();
                }
            });
        }
        WM_MOUSELEAVE => {
            with(|m| {
                m.tracking = false;
                m.hover = None;
                m.redraw();
            });
        }
        WM_LBUTTONUP => {
            let id = with(|m| {
                let (x, y) = dip(m.k());
                m.hit(x, y).map(|i| m.entries[i].id)
            })
            .flatten();
            if let Some(id) = id {
                finish(id);
            }
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
    LRESULT(0)
}

/// Menüyü açar. `anchor`: simgenin ortası (x) ve menünün alt kenarı (y), ekran pikseli.
pub fn open(dock: HWND, title: &str, icon: &IconSource, entries: Vec<Entry>, anchor: (i32, i32), monitor: RECT, dpi: f32) {
    close();
    let t = theme::current();
    let created = (|| -> windows::core::Result<Menu> {
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
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
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
            let img = apps::icon(gfx.wic(), icon, (32.0 * dpi / 96.0) as i32).and_then(|b| gfx.load_bitmap_source(&b).ok());
            Ok(Menu {
                hwnd,
                dock,
                gfx,
                t,
                title: title.to_string(),
                icon: img,
                entries,
                rows: Vec::new(),
                width: MIN_W,
                height: 0.0,
                hover: None,
                tracking: false,
                dpi,
                anchor,
                monitor,
                opened: Instant::now(),
                done: false,
            })
        }
    })();
    let mut m = match created {
        Ok(m) => m,
        Err(e) => {
            crate::log!("hatter: menü açılamadı: {e}");
            unsafe {
                let _ = PostMessageW(Some(dock), WM_MENU_DONE, WPARAM(0), LPARAM(0));
            }
            return;
        }
    };
    m.layout();
    let hwnd = m.hwnd;
    MENU.with_borrow_mut(|x| *x = Some(m));
    with(|m| {
        m.opened = Instant::now();
        m.place();
    });
    unsafe {
        SetTimer(Some(hwnd), TIMER_SLIDE, 8, None);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Menüyü kapatır; seçim yapılmadan kapandıysa dock'a "vazgeçildi" gider.
pub fn close() {
    let Some(m) = MENU.with(|m| m.try_borrow_mut().ok().and_then(|mut m| m.take())) else { return };
    unsafe {
        let _ = KillTimer(Some(m.hwnd), TIMER_SLIDE);
        let _ = DestroyWindow(m.hwnd);
        if !m.done {
            let _ = PostMessageW(Some(m.dock), WM_MENU_DONE, WPARAM(0), LPARAM(0));
        }
    }
    let Menu { gfx, .. } = m;
    SPARE.with(|s| *s.borrow_mut() = Some(gfx));
}

