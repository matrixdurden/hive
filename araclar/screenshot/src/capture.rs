//! Yakalama: bütün ekranlar bir kez kopyalanır (donmuş görüntü), üstünde tam ekran bir pencere
//! kararmış hâlini gösterir; seçilen alan aydınlık, ince çerçeveli; yazı yok. Seçim bırakılınca
//! yerinde kalır: içinde tek renk kalemle çizilir, Ctrl+C (ya da Enter) panoya kopyalar, Ctrl+S
//! kaydetme yerini sorup PNG olarak kaydeder (panoya da kopyalar), Ctrl+Z son çizgiyi geri alır,
//! Esc vazgeçer; seçimin dışında sürüklemek yeni seçim başlatır. Fare kıpırdadıkça yalnızca
//! değişen bölge yeniden çizilir (4K'da da akıcı). Açıkken öbür araçlar ekran köşelerine tepki
//! vermez. Hepsi arayüz iş parçacığında, GDI.

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
/// Kalemin rengi (COLORREF 0x00BBGGRR: kırmızı) ve kalınlığı (DIP).
const PEN: u32 = 0x003a45ff;
const PEN_W: i32 = 3;

/// Donmuş görüntü ve seçim (koordinatlar pencerenin, yani sanal ekranın köşesine göre piksel).
struct Shot {
    hwnd: HWND,
    w: i32,
    h: i32,
    /// Donmuş görüntü, kararmış hâli ve çizim tamponu (her biri kendi DC'sinde seçili).
    src: (HDC, HBITMAP),
    dim: (HDC, HBITMAP),
    buf: (HDC, HBITMAP),
    /// Kalem (ekran pikseli kalınlığında).
    pen: HPEN,
    pen_w: i32,
    /// Sürüklenen seçimin başladığı yer.
    anchor: Option<POINT>,
    sel: Option<RECT>,
    /// Seçim bırakıldı: içinde çizilir, kısayollar çalışır.
    placed: bool,
    /// Çizgiler (pencere pikseli); sonuncusu çizilmekte olabilir.
    strokes: Vec<Vec<POINT>>,
    drawing: bool,
    /// Kaydetme penceresinin ilk açtığı klasör.
    dir: PathBuf,
}

thread_local! {
    static SHOT: RefCell<Option<Shot>> = const { RefCell::new(None) };
}

/// Ekranı dondurur ve seçimi başlatır (zaten açıksa bir şey yapmaz).
pub fn start(dir: PathBuf) {
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
        let dpi = GetDpiForWindow(hwnd).max(96);
        let pen_w = (PEN_W * dpi as i32 / 96).max(2);
        let pen = CreatePen(PS_SOLID, pen_w, COLORREF(PEN));
        SHOT.with_borrow_mut(|s| {
            *s = Some(Shot {
                hwnd,
                w,
                h,
                src,
                dim,
                buf,
                pen,
                pen_w,
                anchor: None,
                sel: None,
                placed: false,
                strokes: Vec::new(),
                drawing: false,
                dir,
            })
        });
        ortak::hub::SCREEN_BUSY.store(true, std::sync::atomic::Ordering::Relaxed);
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

/// Seçimin ve çerçevesinin kapladığı yer.
fn footprint(r: RECT) -> RECT {
    RECT { left: r.left - 2, top: r.top - 2, right: r.right + 2, bottom: r.bottom + 2 }
}

impl Shot {
    /// Tamponun `area` bölgesini baştan kurar: karartılmış zemin, aydınlık seçim, çizgiler, çerçeve.
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
            self.draw_strokes(dc, 0, 0);
            let white = CreateSolidBrush(COLORREF(0x00ededed));
            let _ = FrameRect(dc, &s, white);
            let _ = DeleteObject(white.into());
        }
    }

    /// Çizgileri `dc`'ye çizer; `(dx, dy)`: pencere pikselinden hedefe kaydırma.
    fn draw_strokes(&self, dc: HDC, dx: i32, dy: i32) {
        unsafe {
            let old = SelectObject(dc, self.pen.into());
            for st in &self.strokes {
                let Some(first) = st.first() else { continue };
                let _ = MoveToEx(dc, first.x - dx, first.y - dy, None);
                if st.len() == 1 {
                    let _ = LineTo(dc, first.x - dx + 1, first.y - dy);
                }
                for p in &st[1..] {
                    let _ = LineTo(dc, p.x - dx, p.y - dy);
                }
            }
            SelectObject(dc, old);
        }
    }

    /// Çizginin son parçasını tampona ekler ve yalnızca o bölgeyi yeniden çizdirir.
    fn add_point(&mut self, p: POINT) {
        let Some(st) = self.strokes.last_mut() else { return };
        let prev = *st.last().unwrap_or(&p);
        st.push(p);
        let m = self.pen_w + 2;
        let area = RECT { left: prev.x.min(p.x) - m, top: prev.y.min(p.y) - m, right: prev.x.max(p.x) + m, bottom: prev.y.max(p.y) + m };
        self.compose(area);
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), Some(&area), false);
        }
    }

    /// Son çizgiyi geri alır.
    fn undo(&mut self) {
        if self.strokes.pop().is_some()
            && let Some(s) = self.sel
        {
            let area = footprint(s);
            self.compose(area);
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), Some(&area), false);
            }
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
            self.draw_strokes(dc, r.left, r.top);
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

    /// Seçileni panoya verir; `save`: klasöre de kaydeder.
    fn deliver(&self, r: RECT, save: bool) {
        let (w, h, px) = self.pixels(r);
        if let Err(e) = to_clipboard(self.hwnd, w, h, &px) {
            ortak::log!("ekran görüntüsü panoya konamadı: {e}");
            return;
        }
        let mut text = t!("Copied", "Kopyalandı").to_string();
        // Kaydetme yerini kullanıcı seçer (vazgeçerse yalnızca panoda kalır).
        if save && let Some(path) = ask_path(&self.dir) {
            match save_png(&path, w, h, &px) {
                Ok(()) => {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    text = t!(format!("Copied · saved as {name}"), format!("Kopyalandı · {name} olarak kaydedildi"));
                }
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

/// Windows'un "Farklı kaydet" penceresi: adı tarih-saat, ilk açılışta `dir` (sonra Windows son
/// seçilen klasörü hatırlar). Vazgeçilirse `None`.
fn ask_path(dir: &std::path::Path) -> Option<PathBuf> {
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileSaveDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH};
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let name = format!("{}-{:02}-{:02} {:02}.{:02}.{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond);
    let _ = std::fs::create_dir_all(dir);
    unsafe {
        let dlg: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let filter = [COMDLG_FILTERSPEC { pszName: w!("PNG"), pszSpec: w!("*.png") }];
        let _ = dlg.SetFileTypes(&filter);
        let _ = dlg.SetDefaultExtension(w!("png"));
        let name = wide(&name);
        let _ = dlg.SetFileName(PCWSTR(name.as_ptr()));
        let d = wide(&dir.display().to_string());
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(d.as_ptr()), None) {
            let _ = dlg.SetDefaultFolder(&item);
        }
        dlg.Show(None).ok()?;
        let path = dlg.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let out = PathBuf::from(path.to_string().ok()?);
        windows::Win32::System::Com::CoTaskMemFree(Some(path.0 as *const _));
        Some(out)
    }
}

/// Pikselleri `path`'e PNG olarak yazar.
fn save_png(path: &std::path::Path, w: i32, h: i32, px: &[u8]) -> windows::core::Result<()> {
    let path = wide(&path.display().to_string());
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
    Ok(())
}

/// Pencereyi kapatır ve donmuş görüntüyü bırakır; `deliver` verilirse önce o alan gönderilir
/// (`save`: klasöre de).
fn finish(deliver: Option<RECT>, save: bool) {
    let Some(shot) = SHOT.with_borrow_mut(Option::take) else { return };
    ortak::hub::SCREEN_BUSY.store(false, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        let _ = ShowWindow(shot.hwnd, SW_HIDE);
    }
    if let Some(r) = deliver.filter(|r| r.right > r.left && r.bottom > r.top) {
        shot.deliver(r, save);
    }
    unsafe {
        let _ = DestroyWindow(shot.hwnd);
        for (dc, bmp) in [shot.src, shot.dim, shot.buf] {
            let _ = DeleteDC(dc);
            let _ = DeleteObject(bmp.into());
        }
        let _ = DeleteObject(shot.pen.into());
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
            let p = point(lp);
            SHOT.with_borrow_mut(|s| {
                let Some(s) = s else { return };
                match s.sel {
                    // Yerleşmiş seçimin içinde: çizgi başlar.
                    Some(r) if s.placed && p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom => {
                        s.strokes.push(vec![p]);
                        s.drawing = true;
                        s.add_point(p);
                    }
                    // Dışında: yeni seçim (çizgiler gider).
                    _ => {
                        s.strokes.clear();
                        s.placed = false;
                        s.anchor = Some(p);
                    }
                }
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            SHOT.with_borrow_mut(|s| {
                let Some(s) = s else { return };
                let p = point(lp);
                if s.drawing {
                    // Çizgi seçimin dışına taşmaz.
                    if let Some(r) = s.sel {
                        s.add_point(POINT { x: p.x.clamp(r.left, r.right - 1), y: p.y.clamp(r.top, r.bottom - 1) });
                    }
                } else if let Some(a) = s.anchor {
                    s.set_sel(Some(norm(a, p)));
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            unsafe {
                let _ = ReleaseCapture();
            }
            SHOT.with_borrow_mut(|s| {
                let Some(s) = s else { return };
                if std::mem::take(&mut s.drawing) {
                    return;
                }
                if s.anchor.take().is_none() {
                    return;
                }
                // Sürüklemeden tıklamak: imlecin olduğu ekranın tamamı.
                let r = match s.sel {
                    Some(r) if r.right - r.left > 3 && r.bottom - r.top > 3 => r,
                    _ => monitor_at(point(lp), hwnd),
                };
                s.placed = true;
                s.set_sel(Some(r));
            });
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            finish(None, false);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            let sel = SHOT.with_borrow(|s| s.as_ref().and_then(|s| s.sel.filter(|_| s.placed)));
            match VIRTUAL_KEY(wp.0 as u16) {
                VK_ESCAPE => finish(None, false),
                VK_RETURN if sel.is_some() => finish(sel, false),
                // Seçim yoksa Enter imlecin olduğu ekranı alır.
                VK_RETURN => {
                    let mut p = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut p);
                        let _ = ScreenToClient(hwnd, &mut p);
                    }
                    finish(Some(monitor_at(p, hwnd)), false);
                }
                VK_C if ctrl && sel.is_some() => finish(sel, false),
                VK_S if ctrl && sel.is_some() => finish(sel, true),
                VK_Z if ctrl => SHOT.with_borrow_mut(|s| {
                    if let Some(s) = s {
                        s.undo();
                    }
                }),
                _ => {}
            }
            LRESULT(0)
        }
        // Başka bir pencere öne geçerse (Alt+Tab) yakalama biter.
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            if SHOT.with_borrow(|s| s.as_ref().is_some_and(|s| s.hwnd == hwnd)) {
                finish(None, false);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
