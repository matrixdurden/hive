//! Bildirim alanı ikonu ve menüsü. Aynı gizli pencere sistem yayınlarını da dinler
//! (monitör değişimi, Explorer yeniden başlaması, oturum kilidi).

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{POWERBROADCAST_SETTING, RegisterPowerSettingNotification};
use windows::Win32::System::RemoteDesktop::{NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification};
use windows::Win32::System::SystemServices::{GUID_ACDC_POWER_SOURCE, GUID_CONSOLE_DISPLAY_STATE};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::util::{Res, wide};

const WM_TRAY: u32 = WM_APP + 1;
const WTS_SESSION_LOCK: usize = 0x7;
const WTS_SESSION_UNLOCK: usize = 0x8;
const PBT_POWERSETTINGCHANGE: usize = 0x8013;
/// `WM_COPYDATA` ile gelen "bu dosyayı uygula" isteğinin kimliği.
pub const COPYDATA_INSTALL: usize = 0xD0FA;
pub const CLASS: PCWSTR = w!("CheshireTray");

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    MenuRequested,
    DisplayChanged,
    ExplorerRestarted,
    SessionLocked(bool),
    /// Öndeki pencere değişti, küçültüldü ya da taşındı.
    WindowsChanged,
    OnBattery(bool),
    ScreenOn(bool),
    /// Başka bir `cheshire.exe` sürecinden gelen dosya adı (zaten `duvarlar` klasörüne kopyalanmış).
    Install(String),
}

thread_local! {
    static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
}

pub fn take_events() -> Vec<Event> {
    EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut()))
}

/// Aynı olay kuyrukta zaten varsa tekrar eklenmez (konum olayları saniyede yüzlerce gelebilir).
pub fn push(ev: Event) {
    EVENTS.with(|e| {
        let mut q = e.borrow_mut();
        if !q.contains(&ev) {
            q.push(ev);
        }
    });
}

unsafe fn power_setting(lp: LPARAM) -> Option<(windows::core::GUID, u32)> {
    let s = unsafe { (lp.0 as *const POWERBROADCAST_SETTING).as_ref()? };
    (s.DataLength >= 4).then(|| (s.PowerSetting, unsafe { *(s.Data.as_ptr() as *const u32) }))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            let ev = (lp.0 as u32) & 0xFFFF;
            if ev == WM_RBUTTONUP || ev == WM_LBUTTONUP {
                push(Event::MenuRequested);
            }
        }
        WM_DISPLAYCHANGE => push(Event::DisplayChanged),
        WM_WTSSESSION_CHANGE => match wp.0 {
            WTS_SESSION_LOCK => push(Event::SessionLocked(true)),
            WTS_SESSION_UNLOCK => push(Event::SessionLocked(false)),
            _ => {}
        },
        WM_POWERBROADCAST if wp.0 == PBT_POWERSETTINGCHANGE => {
            match unsafe { power_setting(lp) } {
                Some((g, v)) if g == GUID_ACDC_POWER_SOURCE => push(Event::OnBattery(v != 0)),
                // 0 = kapalı, 1 = açık, 2 = kısık
                Some((g, v)) if g == GUID_CONSOLE_DISPLAY_STATE => push(Event::ScreenOn(v != 0)),
                _ => {}
            }
            return LRESULT(1);
        }
        WM_COPYDATA => {
            let cds = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
            if cds.dwData == COPYDATA_INSTALL && !cds.lpData.is_null() {
                let units = unsafe { std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2) };
                push(Event::Install(String::from_utf16_lossy(units)));
            }
            return LRESULT(1);
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => push(Event::ExplorerRestarted),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
    LRESULT(0)
}

/// Çalışan bir motor varsa dosya adını ona iletir. İletildiyse `true`.
pub fn send_to_running(file_name: &str) -> bool {
    unsafe {
        let Ok(hwnd) = FindWindowW(CLASS, PCWSTR::null()) else { return false };
        let data: Vec<u16> = file_name.encode_utf16().collect();
        let cds = COPYDATASTRUCT { dwData: COPYDATA_INSTALL, cbData: (data.len() * 2) as u32, lpData: data.as_ptr() as *mut _ };
        SendMessageW(hwnd, WM_COPYDATA, None, Some(LPARAM(&cds as *const _ as isize))).0 != 0
    }
}

pub enum Item {
    Label(String),
    Separator,
    Action { id: u32, label: String, checked: bool },
    Submenu { label: String, items: Vec<Item> },
}

pub struct Tray {
    pub hwnd: HWND,
    icon: HICON,
}

impl Tray {
    pub fn new() -> Res<Self> {
        unsafe {
            let hinstance = GetModuleHandleW(None)?;
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance.into(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassW(&wc);
            // Mesaj-yalnız (HWND_MESSAGE) pencere yayın mesajlarını almaz; o yüzden gizli üst seviye pencere.
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                w!("cheshire"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance.into()),
                None,
            )?;
            TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);
            let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
            // Kayıt anında mevcut durum da bir kez gönderilir.
            for guid in [&GUID_ACDC_POWER_SOURCE, &GUID_CONSOLE_DISPLAY_STATE] {
                let _ = RegisterPowerSettingNotification(HANDLE(hwnd.0), guid, DEVICE_NOTIFY_WINDOW_HANDLE);
            }

            let tray = Self { hwnd, icon: load_icon(hinstance.into())? };
            tray.add();
            Ok(tray)
        }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            ..Default::default()
        }
    }

    /// Explorer yeniden başladığında da çağrılır.
    pub fn add(&self) {
        let mut d = self.data();
        d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        d.uCallbackMessage = WM_TRAY;
        d.hIcon = self.icon;
        copy_into(&mut d.szTip, "cheshire");
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &d);
        }
    }

    pub fn set_tooltip(&self, text: &str) {
        let mut d = self.data();
        d.uFlags = NIF_TIP;
        copy_into(&mut d.szTip, text);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    pub fn notify(&self, title: &str, text: &str) {
        let mut d = self.data();
        d.uFlags = NIF_INFO;
        d.dwInfoFlags = NIIF_WARNING;
        copy_into(&mut d.szInfoTitle, title);
        copy_into(&mut d.szInfo, text);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    /// Menüyü imlecin yanında açar, seçilen öğenin kimliğini döndürür.
    pub fn show_menu(&self, items: &[Item]) -> Option<u32> {
        unsafe {
            let menu = build_menu(items).ok()?;
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            // Bu olmadan menü dışına tıklayınca kapanmaz (bilinen Win32 tuhaflığı).
            let _ = SetForegroundWindow(self.hwnd);
            let id = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_RIGHTALIGN,
                pt.x,
                pt.y,
                None,
                self.hwnd,
                None,
            );
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            (id.0 != 0).then_some(id.0 as u32)
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let d = self.data();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &d);
            let _ = DestroyIcon(self.icon);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn copy_into<const N: usize>(dst: &mut [u16; N], s: &str) {
    for (d, c) in dst.iter_mut().zip(s.encode_utf16().take(N - 1).chain(Some(0))) {
        *d = c;
    }
}

unsafe fn build_menu(items: &[Item]) -> Res<HMENU> {
    unsafe {
        let menu = CreatePopupMenu()?;
        for item in items {
            match item {
                Item::Label(text) => {
                    let t = wide(text);
                    AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(t.as_ptr()))?;
                }
                Item::Separator => AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?,
                Item::Action { id, label, checked } => {
                    let t = wide(label);
                    let flags = if *checked { MF_STRING | MF_CHECKED } else { MF_STRING };
                    AppendMenuW(menu, flags, *id as usize, PCWSTR(t.as_ptr()))?;
                }
                Item::Submenu { label, items } => {
                    let sub = build_menu(items)?;
                    let t = wide(label);
                    AppendMenuW(menu, MF_STRING | MF_POPUP, sub.0 as usize, PCWSTR(t.as_ptr()))?;
                }
            }
        }
        Ok(menu)
    }
}

/// Exe'ye gömülü ikonu (build.rs, kaynak 1) tepsi boyutunda yükler.
fn load_icon(hinstance: HINSTANCE) -> Res<HICON> {
    unsafe {
        let (cx, cy) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        let h = LoadImageW(Some(hinstance), PCWSTR(1 as *const u16), IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR)?;
        Ok(HICON(h.0))
    }
}
