//! Win+1…9: dock'taki sırayla uygulamalar. Görev çubuğu gizli olduğundan Windows'un kendi
//! Win+rakamı dock'la uyuşmaz. Kendi iş parçacığındaki düşük seviye klavye kancası her tuşta
//! yalnızca "Win+rakam mı" diye bakar, öyleyse yutar ve dock'a haber verir; diğer her tuş (Alt+Tab
//! dahil) olduğu gibi geçer.

use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Kancadan dock'a: Win+rakam (wParam: dock'taki sıra, 0'dan).
pub const WM_DOCK_KEY: u32 = WM_APP + 9;

static DOCK: AtomicIsize = AtomicIsize::new(0);
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);

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

/// Klavye kancasını kurar (kendi iş parçacığında).
pub fn register(hwnd: HWND) {
    DOCK.store(hwnd.0 as isize, Ordering::Relaxed);
    if HOOK_THREAD.load(Ordering::Relaxed) != 0 {
        return;
    }
    std::thread::spawn(|| unsafe {
        use windows::Win32::System::Threading::GetCurrentThreadId;
        let inst = GetModuleHandleW(None).ok();
        let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), inst.map(|i| i.into()), 0) else {
            crate::log!("hatter: klavye kancası kurulamadı (Win+rakam Windows'unki kalır)");
            return;
        };
        HOOK_THREAD.store(GetCurrentThreadId(), Ordering::Relaxed);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
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
