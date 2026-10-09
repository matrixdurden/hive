//! Win+1…9: dock'taki sırayla uygulamalar. Görev çubuğu gizli olduğundan Windows'un kendi
//! Win+rakamı dock'la uyuşmaz. Kendi iş parçacığındaki düşük seviye klavye kancası her tuşta
//! yalnızca "Win+rakam mı" diye bakar, öyleyse yutar ve dock'a haber verir; diğer her tuş (Alt+Tab
//! dahil) olduğu gibi geçer.
//!
//! Etkin köşeler (Linux masaüstlerindeki gibi): imleç ekranın sol üst köşesine gidince görev
//! görünümü (Win+Tab), sağ alt köşesine gidince masaüstü (Win+D); imleç köşede kısa bir süre
//! beklemeli. Aynı iş parçacığındaki fare
//! kancası yalnızca bir köşe açıkken kurulur; her harekette iki karşılaştırma yapar. Tuş basılıyken
//! (pencere köşeye sürüklenip yapıştırılırken) ve tam ekranda çalışmaz.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicUsize, Ordering};

use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Kancadan dock'a: Win+rakam (wParam: dock'taki sıra, 0'dan).
pub const WM_DOCK_KEY: u32 = WM_APP + 9;

static DOCK: AtomicIsize = AtomicIsize::new(0);
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);

static CORNER_TL: AtomicBool = AtomicBool::new(false);
static CORNER_BR: AtomicBool = AtomicBool::new(false);
/// Dock'tan: öndeki pencere tam ekran (oyun, video); köşeler susar.
pub static FULLSCREEN: AtomicBool = AtomicBool::new(false);
/// Köşeden çıkılana kadar yeniden tetiklenmez.
static ARMED: AtomicBool = AtomicBool::new(true);
/// Kanca iş parçacığına: fare kancasını köşe ayarlarına göre kur ya da kaldır.
const WM_CORNERS: u32 = WM_APP + 1;
/// İmleç köşeye girdi: hangisi (sağ alt mı) ve bekleme zamanlayıcısı.
static PENDING_BR: AtomicBool = AtomicBool::new(false);
static DWELL_TIMER: AtomicUsize = AtomicUsize::new(0);
/// Köşeye girilen ve (beklerken) çıkılan an, fare olayının zamanıyla (ms; 0: çıkılmadı).
/// Zamanlayıcı geç gelse de imlecin yeterince beklediği buradan bilinir.
static ENTERED: AtomicU32 = AtomicU32::new(0);
static LEFT: AtomicU32 = AtomicU32::new(0);
/// Köşede bu kadar beklenince tetiklenir (ms): yanlışlıkla savrulan imleç saymasın. Sağ alt
/// saatin yanında, imleç oraya daha sık kaçar.
const DWELL_TL: u32 = 200;
const DWELL_BR: u32 = 350;
/// Köşeden bu kadar uzaklaşınca yeniden kurulur (piksel).
const REARM: i32 = 40;

/// Zararsız sahte tuş: rakam yutulunca Windows, Win bırakılınca yalnız Win'e basıldı sanıp
/// Başlat'ı açmasın.
fn break_win() {
    let key = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0xFF), dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() }, ..Default::default() },
        },
    };
    unsafe {
        SendInput(&[key(false), key(true)], size_of::<INPUT>() as i32);
    }
}

unsafe extern "system" fn keyboard(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) };
        let alt = k.flags.0 & LLKHF_ALTDOWN.0 != 0;
        if (0x31..=0x39).contains(&k.vkCode) && !alt {
            let held = |v: VIRTUAL_KEY| unsafe { GetAsyncKeyState(v.0 as i32) } as u16 & 0x8000 != 0;
            if (held(VK_LWIN) || held(VK_RWIN)) && !held(VK_SHIFT) && !held(VK_CONTROL) {
                if matches!(wp.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
                    break_win();
                    let h = DOCK.load(Ordering::Relaxed);
                    if h != 0 {
                        unsafe {
                            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_DOCK_KEY, WPARAM((k.vkCode - 0x31) as usize), LPARAM(0));
                        }
                    }
                }
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

unsafe extern "system" fn mouse(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && wp.0 as u32 == WM_MOUSEMOVE {
        let (p, time) = unsafe {
            let m = &*(lp.0 as *const MSLLHOOKSTRUCT);
            (m.pt, m.time)
        };
        let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        let tl = p.x <= 0 && p.y <= 0;
        let br = p.x >= w - 1 && p.y >= h - 1;
        if tl || br {
            if ARMED.load(Ordering::Relaxed) {
                ARMED.store(false, Ordering::Relaxed);
                PENDING_BR.store(br, Ordering::Relaxed);
                ENTERED.store(time, Ordering::Relaxed);
                LEFT.store(0, Ordering::Relaxed);
                let id = unsafe { SetTimer(None, 0, if br { DWELL_BR } else { DWELL_TL }, None) };
                DWELL_TIMER.store(id, Ordering::Relaxed);
            }
        } else if DWELL_TIMER.load(Ordering::Relaxed) != 0 && LEFT.load(Ordering::Relaxed) == 0 {
            LEFT.store(time.max(1), Ordering::Relaxed);
        } else if (p.x > REARM || p.y > REARM) && (p.x < w - 1 - REARM || p.y < h - 1 - REARM) {
            // İki köşenin de çevresinden çıktı.
            ARMED.store(true, Ordering::Relaxed);
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

/// Köşede beklendi (kancanın dışında, iş parçacığının mesaj döngüsünde): imleç süre dolmadan
/// çıkmadıysa köşenin işi yapılır, çıktıysa köşe yeniden kurulur.
fn corner_hit(br: bool) {
    let left = LEFT.load(Ordering::Relaxed);
    let dwell = if br { DWELL_BR } else { DWELL_TL };
    let still = left == 0 || left.wrapping_sub(ENTERED.load(Ordering::Relaxed)) >= dwell;
    if !still {
        ARMED.store(true, Ordering::Relaxed);
        return;
    }
    if !(if br { CORNER_BR.load(Ordering::Relaxed) } else { CORNER_TL.load(Ordering::Relaxed) }) || FULLSCREEN.load(Ordering::Relaxed) {
        return;
    }
    let held = |v: VIRTUAL_KEY| unsafe { GetAsyncKeyState(v.0 as i32) } as u16 & 0x8000 != 0;
    if held(VK_LBUTTON) || held(VK_RBUTTON) || held(VK_MBUTTON) {
        return;
    }
    // Köşenin ötesinde başka bir ekran varsa orası köşe değil, imleç öbür ekrana geçiyor.
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let beyond = if br { [(w, h - 1), (w - 1, h)] } else { [(-1, 0), (0, -1)] };
    if beyond.iter().any(|&(x, y)| unsafe { !MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL).is_invalid() }) {
        return;
    }
    super::press(&[VK_LWIN, if br { VK_D } else { VK_TAB }]);
}

/// Etkin köşeleri açar ya da kapatır.
pub fn set_corners(top_left: bool, bottom_right: bool) {
    CORNER_TL.store(top_left, Ordering::Relaxed);
    CORNER_BR.store(bottom_right, Ordering::Relaxed);
    let t = HOOK_THREAD.load(Ordering::Relaxed);
    if t != 0 {
        unsafe {
            let _ = PostThreadMessageW(t, WM_CORNERS, WPARAM(0), LPARAM(0));
        }
    }
}

/// Klavye kancasını kurar (kendi iş parçacığında).
pub fn register(hwnd: HWND) {
    DOCK.store(hwnd.0 as isize, Ordering::Relaxed);
    if HOOK_THREAD.load(Ordering::Relaxed) != 0 {
        return;
    }
    std::thread::spawn(|| unsafe {
        let inst = GetModuleHandleW(None).ok();
        let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), inst.map(|i| i.into()), 0) else {
            crate::log!("hatter: klavye kancası kurulamadı (Win+rakam Windows'unki kalır)");
            return;
        };
        HOOK_THREAD.store(GetCurrentThreadId(), Ordering::Relaxed);
        let mut mouse_hook: Option<HHOOK> = None;
        let sync_mouse = |mouse_hook: &mut Option<HHOOK>| {
            let want = CORNER_TL.load(Ordering::Relaxed) || CORNER_BR.load(Ordering::Relaxed);
            match (want, mouse_hook.is_some()) {
                (true, false) => {
                    *mouse_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), inst.map(|i| i.into()), 0).ok();
                    ARMED.store(true, Ordering::Relaxed);
                }
                (false, true) => {
                    let _ = UnhookWindowsHookEx(mouse_hook.take().unwrap());
                }
                _ => {}
            }
        };
        sync_mouse(&mut mouse_hook);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            match (msg.hwnd.is_invalid(), msg.message) {
                (true, WM_CORNERS) => sync_mouse(&mut mouse_hook),
                (true, WM_TIMER) if msg.wParam.0 == DWELL_TIMER.load(Ordering::Relaxed) => {
                    let _ = KillTimer(None, msg.wParam.0);
                    DWELL_TIMER.store(0, Ordering::Relaxed);
                    corner_hit(PENDING_BR.load(Ordering::Relaxed));
                }
                _ => {
                    DispatchMessageW(&msg);
                }
            }
        }
        if let Some(m) = mouse_hook {
            let _ = UnhookWindowsHookEx(m);
        }
        let _ = UnhookWindowsHookEx(hook);
        HOOK_THREAD.store(0, Ordering::Relaxed);
    });
}

pub fn unregister() {
    let t = HOOK_THREAD.load(Ordering::Relaxed);
    if t != 0 {
        unsafe {
            let _ = PostThreadMessageW(t, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
    DOCK.store(0, Ordering::Relaxed);
}
