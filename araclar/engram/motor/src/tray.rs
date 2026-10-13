//! Bildirim alanı simgesi. Menüsü yok: tıklamak pencereyi açar ya da gizler.
#![allow(dead_code)]

use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

pub struct Tray {
    hwnd: HWND,
    msg: u32,
}

/// winloom modunda bildirim için açılan simgeyi kaldıran zamanlayıcı.
pub const TIMER_HIDE: usize = 0x7472;

impl Tray {
    pub fn new(hwnd: HWND, msg: u32) -> Self {
        let t = Self { hwnd, msg };
        t.add();
        t
    }

    /// winloom modunda tepside simge durmaz: bildirim gösterilecekse simge birkaç saniyeliğine
    /// eklenir (Windows bildirimi bir simgeye bağlar), sonra kaldırılır.
    pub fn hide(&self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data());
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
        }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut d = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: self.msg,
            ..Default::default()
        };
        unsafe {
            let (cx, cy) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
            if let Ok(inst) = GetModuleHandleW(None)
                && let Ok(h) =
                    LoadImageW(Some(inst.into()), PCWSTR(1 as *const u16), IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR)
            {
                d.hIcon = HICON(h.0);
            }
        }
        for (dst, src) in d.szTip.iter_mut().zip("engram".encode_utf16()) {
            *dst = src;
        }
        d
    }

    /// Bildirim balonu (araçların uyarıları: Pil aracının vites değişimi, RTX bekçisi).
    pub fn notify(&self, title: &str, text: &str) {
        let mut d = self.data();
        d.uFlags = NIF_INFO;
        d.dwInfoFlags = NIIF_INFO;
        for (dst, src) in d.szInfoTitle.iter_mut().zip(title.encode_utf16().take(63).chain(Some(0))) {
            *dst = src;
        }
        for (dst, src) in d.szInfo.iter_mut().zip(text.encode_utf16().take(255).chain(Some(0))) {
            *dst = src;
        }
        unsafe {
            if crate::util::hub() {
                let _ = Shell_NotifyIconW(NIM_ADD, &self.data());
                SetTimer(Some(self.hwnd), TIMER_HIDE, 10_000, None);
            }
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    /// Explorer yeniden başlayınca da çağrılır. winloom modunda simge yok.
    pub fn add(&self) {
        if crate::util::hub() {
            return;
        }
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &self.data());
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data());
        }
    }
}
