//! winloom'ın penceresiyle konuşmak: araçlar ve ikinci kopyalar onu bu yolla öne getirir.

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use std::sync::Mutex;

use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, PostMessageW, SendMessageW, WM_APP, WM_COPYDATA,
};
use windows::core::{PCWSTR, w};

/// winloom'ın pencere sınıfı.
pub const CLASS: PCWSTR = w!("winloom");
/// WM_COPYDATA türü: "şu sekmeyi aç" isteği (boşsa yalnızca öne gel).
pub const COPY_TAB: usize = 0x6d74;
/// Bekleyen sekme isteği var (`take_tab`).
pub const WM_PENDING_TAB: u32 = WM_APP + 3;
/// Yeni sürüm kuruluyor: çalışan kopya tamamen kapansın (WM_CLOSE yalnızca gizler).
pub const WM_EXIT: u32 = WM_APP + 5;

static PENDING: Mutex<Option<String>> = Mutex::new(None);

/// winloom bir mesajı işlerken gelen sekme isteği: sonra uygulanmak üzere saklanır.
pub fn set_tab(tab: String) {
    *PENDING.lock().unwrap() = Some(tab);
}

pub fn take_tab() -> Option<String> {
    PENDING.lock().unwrap().take()
}

/// winloom'ın içinden (bir aracın sayfasından): winloom'ı o sekmede öne getirir. Mesaj olarak
/// gider; sayfa winloom'ın mesajını işlerken çağırabilir.
pub fn post_tab(tab: &str) {
    set_tab(tab.to_string());
    unsafe {
        if let Ok(h) = FindWindowW(CLASS, PCWSTR::null()) {
            let _ = PostMessageW(Some(h), WM_PENDING_TAB, WPARAM(0), LPARAM(0));
        }
    }
}

/// Çalışan winloom'a sekmeyi (araç kimliği, `tools`, `settings`) iletir ve onu öne getirir.
pub fn forward(tab: Option<&str>) {
    unsafe {
        let Ok(hwnd) = FindWindowW(CLASS, PCWSTR::null()) else { return };
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        // Ön plana geçme hakkı bu süreçte (kullanıcı başlattı); öbürüne devret.
        let _ = AllowSetForegroundWindow(pid);
        let data = tab.unwrap_or("").as_bytes();
        let cds = COPYDATASTRUCT { dwData: COPY_TAB, cbData: data.len() as u32, lpData: data.as_ptr() as *mut _ };
        SendMessageW(hwnd, WM_COPYDATA, Some(WPARAM(0)), Some(LPARAM(&cds as *const _ as isize)));
    }
}
