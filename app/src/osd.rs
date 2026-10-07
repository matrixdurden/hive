//! Ekran bildirimi: kısayolla yapılan bir şeyi (çıkış değişti, mikrofon kapandı, ses eklendi)
//! imlecin olduğu ekranın altında, ortada kısa süre gösteren küçük pencere. Odak almaz,
//! tıklamayı geçirir; her şeyin üstünde durur. Araçların ortak bildirimi.

use std::cell::RefCell;
use std::sync::Mutex;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const BG: u32 = 0x1b1b1e;
const TEXT: u32 = 0xe8e8ea;
/// İkon renkleri.
pub const RED: u32 = 0xf87171;
pub const GREEN: u32 = 0x4ade80;

/// DPI'ye göre piksel.
fn px(v: f32) -> i32 {
    (v * unsafe { GetDpiForSystem() } as f32 / 96.0).round() as i32
}

/// COLORREF: 0x00BBGGRR.
fn colorref(c: u32) -> COLORREF {
    COLORREF(((c & 0xff) << 16) | (c & 0xff00) | (c >> 16))
}

fn font(face: PCWSTR, size: i32, weight: i32) -> HFONT {
    unsafe {
        CreateFontW(
            -size,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            face,
        )
    }
}

fn icon_font(size: i32) -> HFONT {
    font(w!("Segoe MDL2 Assets"), size, 400)
}

fn text_font(size: i32) -> HFONT {
    font(w!("Segoe UI Semibold"), size, 600)
}

fn fill(dc: HDC, r: RECT, c: u32) {
    unsafe {
        let b = CreateSolidBrush(colorref(c));
        FillRect(dc, &r, b);
        let _ = DeleteObject(b.into());
    }
}

/// `r` içinde dikey ortalı tek satır.
fn draw_text(dc: HDC, f: HFONT, s: &str, mut r: RECT, c: u32, format: DRAW_TEXT_FORMAT) {
    let mut s: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        let old = SelectObject(dc, f.into());
        SetTextColor(dc, colorref(c));
        DrawTextW(dc, &mut s, &mut r, format | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
        SelectObject(dc, old);
    }
}

fn text_width(f: HFONT, s: &str) -> i32 {
    let s: Vec<u16> = s.encode_utf16().collect();
    let mut size = SIZE::default();
    unsafe {
        let dc = GetDC(None);
        let old = SelectObject(dc, f.into());
        let _ = GetTextExtentPoint32W(dc, &s, &mut size);
        SelectObject(dc, old);
        ReleaseDC(None, dc);
    }
    size.cx
}

/// Odak almayan, tıklamayı geçiren, en üstte duran küçük yuvarlak pencere. Çizimi pencere
/// işlevi kendi durumundan yapar; arka plan çift tamponla, titremesin.
struct Popup {
    hwnd: HWND,
}

impl Popup {
    fn new(class: PCWSTR, proc: WNDPROC, alpha: u8) -> Option<Self> {
        unsafe {
            let inst = GetModuleHandleW(None).ok()?;
            let wc =
                WNDCLASSW { lpfnWndProc: proc, hInstance: inst.into(), lpszClassName: class, ..Default::default() };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
                class,
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(inst.into()),
                None,
            )
            .ok()?;
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
            Some(Self { hwnd })
        }
    }

    fn place(&self, x: i32, y: i32, w: i32, h: i32) {
        unsafe {
            let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, h / 2, h / 2);
            SetWindowRgn(self.hwnd, Some(rgn), false);
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }
}

impl Drop for Popup {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Pencerenin çizimini bellekteki bir bitmap'e yaptırıp tek seferde kopyalar.
fn paint_buffered(hwnd: HWND, draw: impl FnOnce(HDC, RECT)) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let mem = CreateCompatibleDC(Some(dc));
        let bmp = CreateCompatibleBitmap(dc, rc.right, rc.bottom);
        let old = SelectObject(mem, bmp.into());
        fill(mem, rc, BG);
        SetBkMode(mem, TRANSPARENT);
        draw(mem, rc);
        let _ = BitBlt(dc, 0, 0, rc.right, rc.bottom, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}

/// Bildirim: imlecin olduğu ekranın altında ortada, kısa süre.
static OSD: Mutex<(String, u32, String)> = Mutex::new((String::new(), 0, String::new()));
const OSD_TIMER: usize = 1;
const OSD_MS: u32 = 1400;

/// Bildirim ölçüleri: yükseklik, kenar boşluğu, ikon ve yazı boyu.
fn osd_metrics() -> (i32, i32, i32, i32) {
    (px(48.0), px(18.0), px(18.0), px(15.0))
}

unsafe extern "system" fn osd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let (_, pad, icon, text) = osd_metrics();
                let (ic, color, s) = OSD.lock().unwrap().clone();
                paint_buffered(hwnd, |dc, rc| {
                    let (fi, ft) = (icon_font(icon), text_font(text));
                    draw_text(
                        dc,
                        fi,
                        &ic,
                        RECT { left: pad, top: 0, right: pad + icon, bottom: rc.bottom },
                        color,
                        DT_CENTER,
                    );
                    let x = pad + icon + pad / 2 + 4;
                    draw_text(
                        dc,
                        ft,
                        &s,
                        RECT { left: x, top: 0, right: rc.right - pad, bottom: rc.bottom },
                        TEXT,
                        DT_END_ELLIPSIS,
                    );
                    let _ = DeleteObject(fi.into());
                    let _ = DeleteObject(ft.into());
                });
                LRESULT(0)
            }
            WM_TIMER => {
                let _ = KillTimer(Some(hwnd), OSD_TIMER);
                let _ = ShowWindow(hwnd, SW_HIDE);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn place(popup: &Popup, icon: &str, color: u32, text: &str) {
    *OSD.lock().unwrap() = (icon.to_string(), color, text.to_string());
    let (h, pad, ic, size) = osd_metrics();
    let f = text_font(size);
    let tw = text_width(f, text);
    unsafe {
        let _ = DeleteObject(f.into());
    }
    let w = (pad + ic + pad / 2 + 4 + tw + pad).min(h * 12);
    let mut pt = POINT::default();
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let _ = GetCursorPos(&mut pt);
        let _ = GetMonitorInfoW(MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY), &mut mi);
    }
    let work = mi.rcWork;
    popup.place((work.left + work.right - w) / 2, work.bottom - 2 * h, w, h);
    unsafe {
        SetTimer(Some(popup.hwnd), OSD_TIMER, OSD_MS, None);
    }
}

thread_local! {
    /// Arayüz iş parçacığında ilk bildirimde açılır, hive kapanınca gider.
    static POPUP: RefCell<Option<Popup>> = const { RefCell::new(None) };
}

/// `icon`: Segoe MDL2 karakteri; `color`: 0xRRGGBB. Arayüz iş parçacığından çağrılır.
pub fn show(icon: &str, color: u32, text: &str) {
    POPUP.with_borrow_mut(|p| {
        if p.is_none() {
            *p = Popup::new(w!("hive-osd"), Some(osd_proc), 240);
        }
        if let Some(p) = p {
            place(p, icon, color, text);
        }
    });
}
