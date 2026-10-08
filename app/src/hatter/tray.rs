//! Windows'un kendi gizli simgeler penceresi (tepsi taşması), dock'un üstünde açılır.
//!
//! Pencere görev çubuğundaki "Gizli simgeleri göster" düğmesine bağlıdır; çubuk saklıyken
//! doğrudan açılamaz ve çubuk saklanınca kapanır. Bu yüzden açarken görev çubuğunun pencere
//! bölgesi boşaltılır (Explorer onu gösterse de ekranda hiçbir şey görünmez, tıklanamaz),
//! Win+B ile odak düğmeye alınır, Enter'la açılır ve pencere dock'un üstüne taşınır. Kapanınca
//! (başka yere ya da yeniden dock'taki düğmeye tıklayınca) çubuk saklanır ve bölgesi geri verilir.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{CreateRectRgn, SetWindowRgn};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_B, VK_ESCAPE, VK_LWIN, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

/// Açılıyor ya da açık: görev çubuğu bu sürede saklanmaz.
static OPEN: AtomicBool = AtomicBool::new(false);
/// Son kapanış (ms, süreç başından): dock'a tıklamak pencereyi kapatır, aynı tıklama
/// yeniden açmasın.
static CLOSED_AT: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

pub fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

fn overflow() -> Option<HWND> {
    unsafe { FindWindowW(w!("TopLevelWindowForOverflowXamlIsland"), PCWSTR::null()).ok() }
}

fn visible(h: HWND) -> bool {
    unsafe { IsWindowVisible(h).as_bool() }
}

/// Odaktaki öğe "Gizli simgeleri göster" düğmesi mi.
fn chevron_focused(uia: &IUIAutomation) -> bool {
    unsafe { uia.GetFocusedElement().and_then(|e| e.CurrentAutomationId()).is_ok_and(|id| id == "SystemTrayIcon") }
}

fn wait(ms: u64, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    done()
}

/// Taşma penceresini açar ya da açıksa kapatır; alt orta noktası `(x, bottom)` (fiziksel piksel).
pub fn toggle(x: i32, bottom: i32) {
    if OPEN.load(Ordering::Relaxed) {
        // Dock odak almadığından ona tıklamak pencereyi kapatmaz; pencere öndeyken Esc
        // kapatır. Bekleyen iş parçacığı gerisini toparlar.
        if let Some(h) = overflow().filter(|&h| visible(h)) {
            unsafe {
                if GetForegroundWindow() != h {
                    let _ = SetForegroundWindow(h);
                }
            }
            super::press(&[VK_ESCAPE]);
        }
        return;
    }
    // Başka yere tıklanıp az önce kapandıysa aynı tıklama yeniden açmasın.
    let closed = CLOSED_AT.load(Ordering::Relaxed);
    if (closed != 0 && now_ms().saturating_sub(closed) < 250) || OPEN.swap(true, Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let Ok(tray) = (unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }) else {
            OPEN.store(false, Ordering::Relaxed);
            return;
        };
        unsafe {
            SetWindowRgn(tray, Some(CreateRectRgn(0, 0, 0, 0)), false);
        }
        let uia = unsafe { CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }.ok();
        super::press(&[VK_LWIN, VK_B]);
        // Gizli simge yoksa düğme de yoktur; Enter o zaman başka bir simgeye basardı.
        let ok = uia.as_ref().is_some_and(|u| wait(800, || chevron_focused(u)));
        let ovf = if ok {
            super::press(&[VK_RETURN]);
            overflow().filter(|&h| wait(800, || visible(h)))
        } else {
            crate::log!("hatter: gizli simgeler düğmesi bulunamadı");
            super::press(&[VK_ESCAPE]);
            None
        };
        if let Some(h) = ovf {
            // Pencere içerik değişince boyunu ve yerini kendisi yeniler: kapanana dek yerinde tutulur.
            while visible(h) {
                let mut r = RECT::default();
                unsafe {
                    let _ = GetWindowRect(h, &mut r);
                }
                let (nx, ny) = (x - (r.right - r.left) / 2, bottom - (r.bottom - r.top));
                if (r.left - nx).abs() > 1 || (r.top - ny).abs() > 1 {
                    unsafe {
                        let _ = SetWindowPos(h, None, nx, ny, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                    }
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            CLOSED_AT.store(now_ms(), Ordering::Relaxed);
        }
        unsafe {
            let _ = ShowWindow(tray, SW_HIDE);
            SetWindowRgn(tray, None, false);
        }
        OPEN.store(false, Ordering::Relaxed);
    });
}
