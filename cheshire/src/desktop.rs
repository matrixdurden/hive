//! Masaüstü ikonlarının arkasına yerleşme.
//!
//! Progman'a belgelenmemiş 0x052C mesajı gönderilince Explorer, ikon katmanının
//! (SHELLDLL_DefView) arkasında boş bir `WorkerW` penceresi oluşturur. Bizim çizim
//! penceremiz onun çocuğu olursa ikonların arkasında, duvar kâğıdının yerinde görünür.
//!
//! İki düzen var:
//! - Eski (Windows 10 / 11 23H2 ve öncesi): DefView üst seviye bir WorkerW'nin içine taşınır,
//!   hemen arkasında ikinci bir üst seviye WorkerW oluşur → onun çocuğu oluruz.
//! - Yükseltilmiş (Windows 11 24H2+): Progman `WS_EX_NOREDIRECTIONBITMAP` ile yaratılır (GDI
//!   içeriği yoktur), DefView onun katmanlı bir çocuğudur ve yalnızca ikonlarla metni çizer;
//!   duvar kâğıdını DefView'ın altındaki `WorkerW` çocuğu çizer. Microsoft'un tavsiyesi (Lively'nin
//!   alıntısıyla): kendi `WS_EX_LAYERED`, alfa 255 çocuk penceremizi Progman'a, DefView'ın altına
//!   ve WorkerW'nin üstüne koymak. Explorer'ın WorkerW'sinin içine girmek, masaüstünün üstüne
//!   çizdiği seçim dikdörtgeni gibi şeyleri örter. Çizim penceresi bu katmanlı tutucunun içinde
//!   sıradan bir çocuktur (takas zinciri katmanlı pencereyi sevmez).

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

use crate::log;
use crate::util::Res;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKind {
    /// Eski düzen: üst seviye WorkerW'nin çocuğu.
    Legacy,
    /// 24H2+: Progman içindeki WorkerW'nin çocuğu (Explorer'ın duvar kâğıdı katmanı; `yerlesim = worker`).
    Worker,
    /// 24H2+: Progman'ın katmanlı çocuğu, z-sırasında DefView'ın hemen altında (varsayılan).
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
    /// Yükseltilmiş masaüstünde çizim penceresini saran katmanlı tutucu.
    holder: Option<HWND>,
    raised: bool,
}

/// Yükseltilmiş masaüstü: Progman'ın GDI yüzeyi yok, DefView katmanlı.
pub fn raised_desktop() -> bool {
    unsafe {
        FindWindowW(w!("Progman"), PCWSTR::null())
            .map(|p| GetWindowLongPtrW(p, GWL_EXSTYLE) as u32 & WS_EX_NOREDIRECTIONBITMAP.0 != 0)
            .unwrap_or(false)
    }
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

/// Oturum açılışında Explorer masaüstünü bizden sonra kurar: Progman'ı, yükseltilmiş düzende
/// ikon katmanını (DefView) da bekleriz. Erken yerleşirsek sonradan gelen pencereler üstümüze biner.
const SETUP_WAIT: Duration = Duration::from_secs(30);

fn wait_for<T>(mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(v) = probe() {
            return Some(v);
        }
        if start.elapsed() >= SETUP_WAIT {
            return None;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn find_host(pref: &str) -> Res<Host> {
    unsafe {
        let progman = wait_for(|| FindWindowW(w!("Progman"), PCWSTR::null()).ok())
            .ok_or("Progman penceresi bulunamadı (Explorer çalışıyor mu?)")?;
        if raised_desktop() && find(Some(progman), None, w!("SHELLDLL_DefView")).is_none() {
            log!("ikon katmanı henüz yok, bekleniyor");
            if wait_for(|| find(Some(progman), None, w!("SHELLDLL_DefView"))).is_none() {
                log!("ikon katmanı gelmedi, yine de yerleşiliyor");
            }
        }

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
            ("worker", Some(worker)) => Ok(Host { kind: HostKind::Worker, parent: worker, below: None }),
            _ => Ok(Host { kind: HostKind::Progman, parent: progman, below: defview }),
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

            // Gizli yaratılır: ilk kare çizilene kadar (açılışta derleme sürerken) Explorer'ın
            // sabit duvar kâğıdı görünür, siyah pencere değil. `show` ilk karede çağrılır.
            let style = WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN | WS_DISABLED;
            let make = |ex: WINDOW_EX_STYLE, name: PCWSTR, parent: HWND| {
                CreateWindowExW(ex, class, name, style, 0, 0, width as i32, height as i32, Some(parent), None, Some(hinstance.into()), None)
            };

            // Yükseltilmiş masaüstü: önce katmanlı, mat tutucu; çizim penceresi onun içinde.
            let mut holder = None;
            if host.kind == HostKind::Progman {
                let h = make(WS_EX_NOACTIVATE | WS_EX_LAYERED, w!("cheshire-katman"), host.parent)?;
                let layered = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32 & WS_EX_LAYERED.0 != 0;
                if layered && SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA).is_ok() {
                    holder = Some(h);
                } else {
                    // Manifest Windows 8+ demiyorsa Windows katmanlı çocuk pencere yapmaz.
                    log!("katmanlı tutucu yapılamadı; çizim penceresi doğrudan Progman'a giriyor");
                    let _ = DestroyWindow(h);
                }
            }

            let parent = holder.unwrap_or(host.parent);
            let hwnd = make(WS_EX_NOACTIVATE, w!("cheshire"), parent)?;

            if let Some(defview) = host.below {
                // hWndInsertAfter = DefView → bizi ikon katmanının hemen altına koyar.
                let top = holder.unwrap_or(hwnd);
                SetWindowPos(top, Some(defview), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)?;
                // Explorer'ın duvar kâğıdı katmanı en altta kalsın.
                if let Some(worker) = find(Some(host.parent), None, w!("WorkerW")) {
                    let _ = SetWindowPos(worker, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
            }

            Ok(Self { hwnd, width, height, holder, raised: raised_desktop() })
        }
    }

    /// İlk kare çizildi: pencereyi göster (odağı almadan).
    pub fn show(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNA);
            if let Some(h) = self.holder {
                let _ = ShowWindow(h, SW_SHOWNA);
            }
        }
    }

    /// Ebeveyn hâlâ yaşıyor mu? (Explorer yeniden başlarsa pencerelerimiz yok olur.)
    pub fn alive(&self) -> bool {
        unsafe { IsWindow(Some(self.hwnd)).as_bool() }
    }

    /// Yükseltilmiş masaüstünde yerimiz doğru mu: ikon katmanının hemen altında, Explorer'ın
    /// WorkerW'si altımızda. Explorer duvar kâğıdını yeniden uygulayınca (oturum açılışı, tema,
    /// Spotlight) WorkerW'yi yeniden yaratır ve yeni pencere üstümüze gelir; çizim görünmez olur.
    pub fn in_place(&self) -> bool {
        let Some(holder) = self.holder else { return true };
        unsafe {
            let Ok(progman) = GetParent(holder) else { return false };
            let Some(defview) = find(Some(progman), None, w!("SHELLDLL_DefView")) else { return true };
            GetWindow(holder, GW_HWNDPREV).ok() == Some(defview)
        }
    }

    /// Yeri düzeltir: ikon katmanının altına, WorkerW'nin üstüne.
    pub fn restack(&self) {
        let Some(holder) = self.holder else { return };
        unsafe {
            let Ok(progman) = GetParent(holder) else { return };
            if let Some(defview) = find(Some(progman), None, w!("SHELLDLL_DefView")) {
                let _ = SetWindowPos(holder, Some(defview), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
            if let Some(worker) = find(Some(progman), None, w!("WorkerW")) {
                let _ = SetWindowPos(worker, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
    }

    /// Monitör düzeni değiştiyse yeni boyutu döndürür.
    pub fn refresh_size(&mut self) -> Option<(u32, u32)> {
        let outer = self.holder.unwrap_or(self.hwnd);
        let parent = unsafe { GetParent(outer).ok()? };
        let (w, h) = client_size(parent);
        if (w, h) == (self.width, self.height) {
            return None;
        }
        unsafe {
            for win in [Some(outer), self.holder.map(|_| self.hwnd)].into_iter().flatten() {
                let _ = SetWindowPos(win, None, 0, 0, w as i32, h as i32, SWP_NOZORDER | SWP_NOACTIVATE);
            }
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
            if let Some(h) = self.holder {
                let _ = DestroyWindow(h);
            }
        }
        if !self.raised {
            restore_static_wallpaper();
        }
    }
}

/// Mevcut sabit duvar kâğıdını yeniden uygulayarak Explorer'ı masaüstünü tazelemeye zorlar.
/// Yükseltilmiş masaüstünde çağrılmaz: orada bu çağrı Explorer'ın WorkerW'sini yok edip yeniden
/// yaratır ve çalışan başka bir kopyanın penceresini de götürür; duvar kâğıdını zaten WorkerW çizer.
pub fn restore_static_wallpaper() {
    if raised_desktop() {
        return;
    }
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
