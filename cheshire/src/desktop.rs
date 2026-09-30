//! Masaüstü ikonlarının arkasına yerleşme.
//!
//! Progman'a belgelenmemiş 0x052C mesajı gönderilince Explorer, ikon katmanının
//! (SHELLDLL_DefView) arkasında boş bir `WorkerW` penceresi oluşturur. Bizim çizim
//! penceremiz onun çocuğu olursa ikonların arkasında, duvar kâğıdının yerinde görünür.
//!
//! İki düzen var:
//! - Eski (Windows 10 / 11 23H2 ve öncesi): DefView üst seviye bir WorkerW'nin içine taşınır,
//!   hemen arkasında ikinci bir üst seviye WorkerW oluşur → onun çocuğu oluruz.
//! - Yeni (Windows 11 24H2+): DefView ve WorkerW, Progman'ın çocuklarıdır.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

use crate::log;
use crate::util::Res;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKind {
    /// Eski düzen: üst seviye WorkerW'nin çocuğu.
    Legacy,
    /// 24H2+: Progman içindeki WorkerW'nin çocuğu.
    Worker,
    /// 24H2+: Progman'ın çocuğu, z-sırasında DefView'ın hemen altında.
    Progman,
}

struct Host {
    kind: HostKind,
    parent: HWND,
    below: Option<HWND>,
}

pub struct WallpaperWindow {
    pub hwnd: HWND,
    pub width: u32,
    pub height: u32,
}

unsafe fn find(parent: Option<HWND>, after: Option<HWND>, class: PCWSTR) -> Option<HWND> {
    unsafe { FindWindowExW(parent, after, class, PCWSTR::null()).ok() }
}

unsafe extern "system" fn find_legacy_worker(top: HWND, out: LPARAM) -> BOOL {
    unsafe {
        if find(Some(top), None, w!("SHELLDLL_DefView")).is_some() {
            // İkonları taşıyan pencerenin hemen arkasındaki kardeş WorkerW bizim hedefimiz.
            if let Some(worker) = find(None, Some(top), w!("WorkerW")) {
                *(out.0 as *mut Option<HWND>) = Some(worker);
                return BOOL(0);
            }
        }
        BOOL(1)
    }
}

fn find_host(pref: &str) -> Res<Host> {
    unsafe {
        let progman = FindWindowW(w!("Progman"), PCWSTR::null())
            .map_err(|_| "Progman penceresi bulunamadı (Explorer çalışıyor mu?)")?;

        // Explorer'dan ikonların arkasına WorkerW oluşturmasını iste.
        for (wp, lp) in [(0xD, 0x1), (0, 0)] {
            let mut r = 0usize;
            SendMessageTimeoutW(progman, 0x052C, WPARAM(wp), LPARAM(lp), SMTO_NORMAL, 1000, Some(&mut r));
        }

        let mut legacy: Option<HWND> = None;
        let _ = EnumWindows(Some(find_legacy_worker), LPARAM(&mut legacy as *mut _ as isize));
        if let Some(worker) = legacy {
            return Ok(Host { kind: HostKind::Legacy, parent: worker, below: None });
        }

        let defview = find(Some(progman), None, w!("SHELLDLL_DefView"));
        let worker = find(Some(progman), None, w!("WorkerW"));
        match (pref, worker) {
            ("progman", _) | (_, None) => Ok(Host { kind: HostKind::Progman, parent: progman, below: defview }),
            (_, Some(worker)) => Ok(Host { kind: HostKind::Worker, parent: worker, below: None }),
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            // Arka planı hiç silme: GPU zaten her pikseli çiziyor, titremeyi önler.
            WM_ERASEBKGND => LRESULT(1),
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    let (w, h) = ((rc.right - rc.left).max(0) as u32, (rc.bottom - rc.top).max(0) as u32);
    if w > 0 && h > 0 {
        return (w, h);
    }
    // Ebeveyn boyutu okunamazsa tüm sanal ekranı (bütün monitörler) kapla.
    unsafe { (GetSystemMetrics(SM_CXVIRTUALSCREEN) as u32, GetSystemMetrics(SM_CYVIRTUALSCREEN) as u32) }
}

impl WallpaperWindow {
    pub fn attach(pref: &str) -> Res<Self> {
        let host = find_host(pref)?;
        let (width, height) = client_size(host.parent);
        log!("yerleşim: {:?}, ebeveyn {:?}, boyut {width}x{height}", host.kind, host.parent);

        unsafe {
            let hinstance = GetModuleHandleW(None)?;
            let class = w!("CheshireWallpaper");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance.into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&wc); // İkinci kez kayıt başarısız olur, sorun değil.

            let hwnd = CreateWindowExW(
                WS_EX_NOACTIVATE,
                class,
                w!("cheshire"),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | WS_CLIPCHILDREN | WS_DISABLED,
                0,
                0,
                width as i32,
                height as i32,
                Some(host.parent),
                None,
                Some(hinstance.into()),
                None,
            )?;

            if let Some(defview) = host.below {
                // hWndInsertAfter = DefView → bizi ikon katmanının hemen altına koyar.
                SetWindowPos(hwnd, Some(defview), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)?;
            }

            Ok(Self { hwnd, width, height })
        }
    }

    /// Ebeveyn hâlâ yaşıyor mu? (Explorer yeniden başlarsa pencerelerimiz yok olur.)
    pub fn alive(&self) -> bool {
        unsafe { IsWindow(Some(self.hwnd)).as_bool() }
    }

    /// Monitör düzeni değiştiyse yeni boyutu döndürür.
    pub fn refresh_size(&mut self) -> Option<(u32, u32)> {
        let parent = unsafe { GetParent(self.hwnd).ok()? };
        let (w, h) = client_size(parent);
        if (w, h) == (self.width, self.height) {
            return None;
        }
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, 0, 0, w as i32, h as i32, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        (self.width, self.height) = (w, h);
        Some((w, h))
    }

    /// İmleç konumu, pencere koordinatlarında (piksel, sol üst köşe orijin).
    pub fn cursor(&self) -> [f32; 2] {
        let mut p = Default::default();
        unsafe {
            if GetCursorPos(&mut p).is_err() || !windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut p).as_bool() {
                return [-1.0, -1.0];
            }
        }
        [p.x as f32, p.y as f32]
    }
}

impl Drop for WallpaperWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
        restore_static_wallpaper();
    }
}

/// Mevcut sabit duvar kâğıdını yeniden uygulayarak Explorer'ı masaüstünü tazelemeye zorlar.
pub fn restore_static_wallpaper() {
    let mut buf = [0u16; 1024];
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            buf.len() as u32,
            Some(buf.as_mut_ptr().cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        let _ = SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            Some(buf.as_mut_ptr().cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
}

/// Hata ayıklama: masaüstüyle ilgili pencere ağacını döker (`cheshire.exe --agac`).
pub fn dump_tree() -> String {
    fn name(hwnd: HWND) -> String {
        let mut buf = [0u16; 256];
        let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
        String::from_utf16_lossy(&buf[..n])
    }
    fn walk(hwnd: HWND, depth: usize, out: &mut String) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetWindowRect(hwnd, &mut rc);
        }
        let vis = unsafe { IsWindowVisible(hwnd).as_bool() };
        out.push_str(&format!(
            "{}{} {:?} [{},{} {}x{}]{}\n",
            "  ".repeat(depth),
            name(hwnd),
            hwnd.0,
            rc.left,
            rc.top,
            rc.right - rc.left,
            rc.bottom - rc.top,
            if vis { "" } else { " (gizli)" }
        ));
        let mut child = unsafe { find(Some(hwnd), None, PCWSTR::null()) };
        while let Some(c) = child {
            walk(c, depth + 1, out);
            child = unsafe { find(Some(hwnd), Some(c), PCWSTR::null()) };
        }
    }

    let mut out = String::new();
    let mut top = unsafe { find(None, None, PCWSTR::null()) };
    while let Some(t) = top {
        let n = name(t);
        if n == "Progman" || n == "WorkerW" {
            walk(t, 0, &mut out);
        }
        top = unsafe { find(None, Some(t), PCWSTR::null()) };
    }
    out
}
