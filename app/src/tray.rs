//! Bildirim alanı simgesi. Menüsü yok: tıklamak pencereyi açar ya da gizler.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

pub struct Tray {
    hwnd: HWND,
    msg: u32,
}

impl Tray {
    pub fn new(hwnd: HWND, msg: u32) -> Self {
        let t = Self { hwnd, msg };
        t.add();
        t
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
        for (dst, src) in d.szTip.iter_mut().zip("hive".encode_utf16()) {
            *dst = src;
        }
        d
    }

    /// Bildirim balonu (araçların uyarıları: dormouse'un vites değişimi, RTX bekçisi).
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
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    /// Explorer yeniden başlayınca da çağrılır.
    pub fn add(&self) {
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
