//! Yakalama: bütün ekranlar bir kez kopyalanır (donmuş görüntü), üstünde tam ekran bir pencere
//! kararmış hâlini gösterir; seçilen alan aydınlık, köşesinde boyutu. Fare kıpırdadıkça yalnızca
//! değişen bölge yeniden çizilir (4K'da da akıcı). Bırakınca seçilen alan panoya (istenirse
//! PNG olarak klasöre) gider, pencere kapanır. Hepsi arayüz iş parçacığında, GDI ile.

use std::cell::RefCell;
use std::path::PathBuf;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_DIB;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use ortak::util::wide;

const CLASS: PCWSTR = w!("winloom-ekran-goruntusu");
/// Karartma: donmuş görüntünün üstüne bu saydamlıkta siyah (0..255).
const DIM: u8 = 110;

/// Donmuş görüntü ve seçim (koordinatlar pencerenin, yani sanal ekranın köşesine göre piksel).
struct Shot {
    hwnd: HWND,
    w: i32,
    h: i32,
    /// Donmuş görüntü, kararmış hâli ve çizim tamponu (her biri kendi DC'sinde seçili).
    src: (HDC, HBITMAP),
    dim: (HDC, HBITMAP),
    buf: (HDC, HBITMAP),
    font: HFONT,
    anchor: Option<POINT>,
    sel: Option<RECT>,
    /// Bırakınca kaydedilecek klasör (yoksa yalnızca panoya).
    dir: Option<PathBuf>,
}

thread_local! {
    static SHOT: RefCell<Option<Shot>> = const { RefCell::new(None) };
}

/// Ekranı dondurur ve seçimi başlatır (zaten açıksa bir şey yapmaz).
pub fn start(dir: Option<PathBuf>) {
    if SHOT.with_borrow(|s| s.is_some()) {
        return;
    }
    unsafe {
        let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (w, h) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        let screen = GetDC(None);
        let make = || {
            let dc = CreateCompatibleDC(Some(screen));
            let bmp = CreateCompatibleBitmap(screen, w, h);
            SelectObject(dc, bmp.into());
            (dc, bmp)
        };
        let (src, dim, buf) = (make(), make(), make());
        let _ = BitBlt(src.0, 0, 0, w, h, Some(screen), x, y, SRCCOPY | CAPTUREBLT);
        ReleaseDC(None, screen);
        // Kararmış kopya: donmuş görüntünün üstüne yarı saydam siyah.
        let _ = BitBlt(dim.0, 0, 0, w, h, Some(src.0), 0, 0, SRCCOPY);
        let black = CreateCompatibleDC(Some(dim.0));
        let one = CreateCompatibleBitmap(dim.0, 1, 1);
        SelectObject(black, one.into());
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: DIM, AlphaFormat: 0 };
        let _ = AlphaBlend(dim.0, 0, 0, w, h, black, 0, 0, 1, 1, blend);
        let _ = DeleteDC(black);
        let _ = DeleteObject(one.into());
        let _ = BitBlt(buf.0, 0, 0, w, h, Some(dim.0), 0, 0, SRCCOPY);

        let instance = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS,
            w!(""),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(instance.into()),
            None,
        ) else {
            for (dc, bmp) in [src, dim, buf] {
                let _ = DeleteDC(dc);
                let _ = DeleteObject(bmp.into());
            }
            return;
        };
        let px = (13 * GetDpiForWindow(hwnd).max(96) / 72) as i32;
        let font = CreateFontW(-px, 0, 0, 0, FW_SEMIBOLD.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
        SHOT.with_borrow_mut(|s| *s = Some(Shot { hwnd, w, h, src, dim, buf, font, anchor: None, sel: None, dir }));
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
    }
}

fn norm(a: POINT, b: POINT) -> RECT {
    RECT { left: a.x.min(b.x), top: a.y.min(b.y), right: a.x.max(b.x) + 1, bottom: a.y.max(b.y) + 1 }
}

fn union(a: RECT, b: RECT) -> RECT {
    RECT { left: a.left.min(b.left), top: a.top.min(b.top), right: a.right.max(b.right), bottom: a.bottom.max(b.bottom) }
}

/// Seçimin ve boyut etiketinin kapladığı yer (yeniden çizim için biraz geniş).
fn footprint(r: RECT) -> RECT {
    RECT { left: r.left - 4, top: r.top - 48, right: r.right + 240, bottom: r.bottom + 48 }
}

impl Shot {
    /// Tamponun `area` bölgesini baştan kurar: karartılmış zemin, aydınlık seçim, çerçeve, boyut.
    fn compose(&self, area: RECT) {
        unsafe {
            let (l, t) = (area.left.max(0), area.top.max(0));
            let (r, b) = (area.right.min(self.w), area.bottom.min(self.h));
            if r <= l || b <= t {
                return;
            }
            let dc = self.buf.0;
            let _ = BitBlt(dc, l, t, r - l, b - t, Some(self.dim.0), l, t, SRCCOPY);
            let Some(s) = self.sel else { return };
            let _ = BitBlt(dc, s.left, s.top, s.right - s.left, s.bottom - s.top, Some(self.src.0), s.left, s.top, SRCCOPY);
            let white = CreateSolidBrush(COLORREF(0x00ededed));
            let _ = FrameRect(dc, &s, white);
            // Boyut: seçimin sol üstünün hemen üstünde (yer yoksa içinde).
            let mut text = wide(&format!("{} × {}", s.right - s.left, s.bottom - s.top));
            text.pop();
            let old = SelectObject(dc, self.font.into());
            let mut m = RECT::default();
            DrawTextW(dc, &mut text, &mut m, DT_CALCRECT | DT_SINGLELINE);
            let (tw, th) = (m.right - m.left + 16, m.bottom - m.top + 8);
            let top = if s.top - th - 6 >= 0 { s.top - th - 6 } else { s.top + 6 };
            let mut label = RECT { left: s.left, top, right: s.left + tw, bottom: top + th };
            let back = CreateSolidBrush(COLORREF(0x00161616));
            FillRect(dc, &label, back);
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00ededed));
            DrawTextW(dc, &mut text, &mut label, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            SelectObject(dc, old);
            let _ = DeleteObject(white.into());
            let _ = DeleteObject(back.into());
        }
    }

    fn set_sel(&mut self, sel: Option<RECT>) {
        let old = self.sel.map(footprint);
        self.sel = sel;
        let new = self.sel.map(footprint);
        let area = match (old, new) {
            (Some(a), Some(b)) => union(a, b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => return,
        };
        self.compose(area);
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), Some(&area), false);
        }
    }

    /// Seçilen alanın pikselleri: 32 bit BGRA, yukarıdan aşağı.
    fn pixels(&self, r: RECT) -> (i32, i32, Vec<u8>) {
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let mut out = vec![0u8; (w * h * 4) as usize];
        unsafe {
            let dc = CreateCompatibleDC(Some(self.src.0));
            let bmp = CreateCompatibleBitmap(self.src.0, w, h);
            SelectObject(dc, bmp.into());
            let _ = BitBlt(dc, 0, 0, w, h, Some(self.src.0), r.left, r.top, SRCCOPY);
            let mut bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            GetDIBits(dc, bmp, 0, h as u32, Some(out.as_mut_ptr().cast()), &mut bi, DIB_RGB_COLORS);
            let _ = DeleteDC(dc);
            let _ = DeleteObject(bmp.into());
        }
        for px in out.chunks_exact_mut(4) {
            px[3] = 255;
        }
        (w, h, out)
    }

    /// Seçileni panoya (ve istenirse klasöre) verir.
    fn deliver(&self, r: RECT) {
        let (w, h, px) = self.pixels(r);
        if let Err(e) = to_clipboard(self.hwnd, w, h, &px) {
            ortak::log!("ekran görüntüsü panoya konamadı: {e}");
            return;
        }
        let mut text = t!("Copied", "Kopyalandı").to_string();
        if let Some(dir) = &self.dir {
            match save_png(dir, w, h, &px) {
                Ok(name) => text = t!(format!("Copied · saved as {name}"), format!("Kopyalandı · {name} olarak kaydedildi")),
                Err(e) => ortak::log!("ekran görüntüsü kaydedilemedi: {e}"),
            }
        }
        ortak::osd::show("\u{E722}", 0xededed, &text);
    }
}

/// İmlecin olduğu ekranın tamamı (pencere koordinatlarında).
fn monitor_at(p: POINT, hwnd: HWND) -> RECT {
    unsafe {
        let mut screen = p;
        let _ = ClientToScreen(hwnd, &mut screen);
        let mon = MonitorFromPoint(screen, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let m = mi.rcMonitor;
        RECT { left: m.left - x, top: m.top - y, right: m.right - x, bottom: m.bottom - y }
    }
}

fn to_clipboard(hwnd: HWND, w: i32, h: i32, px: &[u8]) -> windows::core::Result<()> {
    unsafe {
        // CF_DIB: başlık + aşağıdan yukarıya satırlar.
        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (w * h * 4) as u32,
            ..Default::default()
        };
        let size = size_of::<BITMAPINFOHEADER>() + px.len();
        let mem = GlobalAlloc(GMEM_MOVEABLE, size)?;
        let p = GlobalLock(mem) as *mut u8;
        std::ptr::copy_nonoverlapping(&header as *const _ as *const u8, p, size_of::<BITMAPINFOHEADER>());
        let rows = p.add(size_of::<BITMAPINFOHEADER>());
        let stride = (w * 4) as usize;
        for y in 0..h as usize {
            let src = &px[y * stride..(y + 1) * stride];
            std::ptr::copy_nonoverlapping(src.as_ptr(), rows.add((h as usize - 1 - y) * stride), stride);
        }
        let _ = GlobalUnlock(mem);
        OpenClipboard(Some(hwnd))?;
        let _ = EmptyClipboard();
        let r = SetClipboardData(CF_DIB.0 as u32, Some(HANDLE(mem.0)));
        let _ = CloseClipboard();
        r.map(|_| ())
    }
}

/// `dir`'e "2026-10-13 12.34.56.png" olarak yazar; dosyanın adını döndürür.
fn save_png(dir: &std::path::Path, w: i32, h: i32, px: &[u8]) -> windows::core::Result<String> {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let name = format!("{}-{:02}-{:02} {:02}.{:02}.{:02}.png", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond);
    let _ = std::fs::create_dir_all(dir);
    let path = wide(&dir.join(&name).display().to_string());
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let bmp = wic.CreateBitmapFromMemory(w as u32, h as u32, &GUID_WICPixelFormat32bppBGRA, w as u32 * 4, px)?;
        let stream = wic.CreateStream()?;
        stream.InitializeFromFilename(PCWSTR(path.as_ptr()), GENERIC_WRITE.0)?;
        let enc = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        enc.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(windows::core::Error::empty)?;
        frame.Initialize(None)?;
        frame.SetSize(w as u32, h as u32)?;
        let mut fmt = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut fmt)?;
        frame.WriteSource(&bmp, std::ptr::null())?;
        frame.Commit()?;
        enc.Commit()?;
    }
    Ok(name)
}

/// Pencereyi kapatır ve donmuş görüntüyü bırakır; `deliver` verilirse önce o alan gönderilir.
fn finish(deliver: Option<RECT>) {
    let Some(shot) = SHOT.with_borrow_mut(Option::take) else { return };
    unsafe {
        let _ = ShowWindow(shot.hwnd, SW_HIDE);
    }
    if let Some(r) = deliver.filter(|r| r.right > r.left && r.bottom > r.top) {
        shot.deliver(r);
    }
    unsafe {
        let _ = DestroyWindow(shot.hwnd);
        for (dc, bmp) in [shot.src, shot.dim, shot.buf] {
            let _ = DeleteDC(dc);
            let _ = DeleteObject(bmp.into());
        }
        let _ = DeleteObject(shot.font.into());
    }
}

fn point(lp: LPARAM) -> POINT {
    POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = unsafe { BeginPaint(hwnd, &mut ps) };
            SHOT.with_borrow(|s| {
                if let Some(s) = s {
                    let r = ps.rcPaint;
                    unsafe {
                        let _ = BitBlt(dc, r.left, r.top, r.right - r.left, r.bottom - r.top, Some(s.buf.0), r.left, r.top, SRCCOPY);
                    }
                }
            });
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN => {
            unsafe { SetCapture(hwnd) };
            SHOT.with_borrow_mut(|s| {
                if let Some(s) = s {
                    s.anchor = Some(point(lp));
                }
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            SHOT.with_borrow_mut(|s| {
                if let Some(s) = s
                    && let Some(a) = s.anchor
                {
                    s.set_sel(Some(norm(a, point(lp))));
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            unsafe {
                let _ = ReleaseCapture();
            }
            // Sürüklemeden tıklamak: imlecin olduğu ekranın tamamı.
            let pick = SHOT.with_borrow(|s| {
                s.as_ref().map(|s| match s.sel {
                    Some(r) if r.right - r.left > 3 && r.bottom - r.top > 3 => r,
                    _ => monitor_at(point(lp), hwnd),
                })
            });
            finish(pick);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            finish(None);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            match VIRTUAL_KEY(wp.0 as u16) {
                VK_ESCAPE => finish(None),
                VK_RETURN => {
                    let mut p = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut p);
                        let _ = ScreenToClient(hwnd, &mut p);
                    }
                    let pick = SHOT.with_borrow(|s| s.as_ref().map(|s| s.sel.unwrap_or_else(|| monitor_at(p, hwnd))));
                    finish(pick);
                }
                _ => {}
            }
            LRESULT(0)
        }
        // Başka bir pencere öne geçerse (Alt+Tab) yakalama biter.
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            if SHOT.with_borrow(|s| s.as_ref().is_some_and(|s| s.hwnd == hwnd)) {
                finish(None);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
