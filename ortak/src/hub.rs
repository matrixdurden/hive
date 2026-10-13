//! winloom'ın penceresiyle konuşmak: araçlar ve ikinci kopyalar onu bu yolla öne getirir.

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, SendMessageW, WM_APP, WM_COPYDATA,
};
use windows::core::{PCWSTR, w};

/// winloom'ın pencere sınıfı.
pub const CLASS: PCWSTR = w!("winloom");
/// WM_COPYDATA türü: "şu sekmeyi aç" isteği (boşsa yalnızca öne gel).
pub const COPY_TAB: usize = 0x6d74;
/// Yeni sürüm kuruluyor: çalışan kopya tamamen kapansın (WM_CLOSE yalnızca gizler).
pub const WM_EXIT: u32 = WM_APP + 5;

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
