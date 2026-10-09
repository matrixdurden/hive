//! Win+1…9: dock'taki sırayla uygulamalar. Görev çubuğu gizli olduğundan Windows'un kendi
//! Win+rakamı dock'la uyuşmaz. Kendi iş parçacığındaki düşük seviye klavye kancası her tuşta
//! yalnızca "Win+rakam mı" diye bakar, öyleyse yutar ve dock'a haber verir; diğer her tuş (Alt+Tab
//! dahil) olduğu gibi geçer.
//!
//! Etkin köşeler (Linux masaüstlerindeki gibi): imleç ekranın sol üst köşesine gidince görev
//! görünümü (Win+Tab), sağ alt köşesine gidince masaüstü (Win+D); imleç köşeye sertçe gitmeli ya
//! da köşeye itilmeli (basınç), köşeden geçip giden imleç saymaz. Aynı iş parçacığındaki fare
//! kancası yalnızca bir köşe açıkken kurulur; her harekette iki karşılaştırma yapar. Tuş basılıyken
//! (pencere köşeye sürüklenip yapıştırılırken) ve tam ekranda çalışmaz.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Kancadan dock'a: Win+rakam (wParam: dock'taki sıra, 0'dan).
pub const WM_DOCK_KEY: u32 = WM_APP + 9;

static BAR: AtomicIsize = AtomicIsize::new(0);
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);

static CORNER_TL: AtomicBool = AtomicBool::new(false);
static CORNER_BR: AtomicBool = AtomicBool::new(false);
/// Dock'tan: öndeki pencere tam ekran (oyun, video); köşeler susar.
pub static FULLSCREEN: AtomicBool = AtomicBool::new(false);
/// Köşeden çıkılana kadar yeniden tetiklenmez.
static ARMED: AtomicBool = AtomicBool::new(true);
/// Kanca iş parçacığına: fare kancasını köşe ayarlarına göre kur ya da kaldır.
const WM_CORNERS: u32 = WM_APP + 1;
/// Köşeye dayanırken ekranın dışına taşan hareket (GNOME'daki basınç): bu kadar piksel birikince
/// tetiklenir, bu süre içinde birikmezse sayaç sıfırlanır. Köşeden geçip giden imleç taşmaz, köşeye
/// sertçe giden ya da itilen imleç taşar.
const PRESSURE: i32 = 100;
const PRESSURE_MS: u32 = 1000;
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
                    let h = BAR.load(Ordering::Relaxed);
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

thread_local! {
    /// Biriken basınç ve başladığı an (fare olayının zamanı, ms).
    static PUSH: std::cell::Cell<(i32, u32)> = const { std::cell::Cell::new((0, 0)) };
}

unsafe extern "system" fn mouse(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && wp.0 as u32 == WM_MOUSEMOVE {
        // Kancadaki konum ekrana kırpılmadan öncekidir: köşeye dayanınca ekranın dışına taşar.
        let (p, time) = unsafe {
            let m = &*(lp.0 as *const MSLLHOOKSTRUCT);
            (m.pt, m.time)
        };
        let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        let over = if p.x <= 0 && p.y <= 0 {
            Some((false, -p.x - p.y))
        } else if p.x >= w - 1 && p.y >= h - 1 {
            Some((true, p.x - (w - 1) + p.y - (h - 1)))
        } else {
            None
        };
        match over {
            Some((br, push)) => {
                if ARMED.load(Ordering::Relaxed) {
                    let (mut sum, start) = PUSH.get();
                    let start = if sum == 0 || time.wrapping_sub(start) > PRESSURE_MS {
                        sum = 0;
                        time
                    } else {
                        start
                    };
                    sum += push.max(0);
                    if sum >= PRESSURE {
                        ARMED.store(false, Ordering::Relaxed);
                        PUSH.set((0, 0));
                        fire(br);
                    } else {
                        PUSH.set((sum, start));
                    }
                }
            }
            None => {
                PUSH.set((0, 0));
                if (p.x > REARM || p.y > REARM) && (p.x < w - 1 - REARM || p.y < h - 1 - REARM) {
                    // İki köşenin de çevresinden çıktı.
                    ARMED.store(true, Ordering::Relaxed);
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

/// Köşe tetiklendi: ayar açıksa, tam ekranda değilse, tuş basılı değilse ve köşenin ötesinde başka
/// ekran yoksa köşenin kısayoluna basılır. Tuşlar kancanın dışında (kendi iş parçacığında)
/// gönderilir: kanca dönmeden klavye kancası çalışamaz.
fn fire(br: bool) {
    if !(if br { CORNER_BR.load(Ordering::Relaxed) } else { CORNER_TL.load(Ordering::Relaxed) }) || FULLSCREEN.load(Ordering::Relaxed) {
        return;
    }
    let held = |v: VIRTUAL_KEY| unsafe { GetAsyncKeyState(v.0 as i32) } as u16 & 0x8000 != 0;
    if held(VK_LBUTTON) || held(VK_RBUTTON) || held(VK_MBUTTON) {
        return;
    }
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let beyond = if br { [(w, h - 1), (w - 1, h)] } else { [(-1, 0), (0, -1)] };
    if beyond.iter().any(|&(x, y)| unsafe { !MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL).is_invalid() }) {
        return;
    }
    std::thread::spawn(move || super::press(&[VK_LWIN, if br { VK_D } else { VK_TAB }]));
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
    BAR.store(hwnd.0 as isize, Ordering::Relaxed);
    if HOOK_THREAD.load(Ordering::Relaxed) != 0 {
        return;
    }
    std::thread::spawn(|| unsafe {
        let inst = GetModuleHandleW(None).ok();
        let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), inst.map(|i| i.into()), 0) else {
            crate::log!("dock: klavye kancası kurulamadı (Win+rakam Windows'unki kalır)");
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
    BAR.store(0, Ordering::Relaxed);
}
