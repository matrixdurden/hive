//! Ne zaman çizmeyi bırakacağımıza karar verir. Tamamen olay tabanlıdır: duraklatılmış motor
//! hiçbir zamanlayıcıyla uyanmaz; yalnızca pencere, güç ve oturum olaylarında yeniden bakar.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromWindow};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::tray::{self, Event};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    User,
    Locked,
    DisplayOff,
    Fullscreen,
    Battery,
}

impl Reason {
    /// hive'a giden, dilden bağımsız ad.
    pub fn key(self) -> &'static str {
        match self {
            Reason::User => "user",
            Reason::Locked => "locked",
            Reason::DisplayOff => "display_off",
            Reason::Fullscreen => "fullscreen",
            Reason::Battery => "battery",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Reason::User => "elle duraklatıldı",
            Reason::Locked => "ekran kilitli",
            Reason::DisplayOff => "ekran kapalı",
            Reason::Fullscreen => "masaüstü görünmüyor",
            Reason::Battery => "pilde",
        }
    }
}

pub struct Policy {
    pub user_paused: bool,
    pub locked: bool,
    pub display_off: bool,
    pub on_battery: bool,
    covered: bool,
}

impl Policy {
    pub fn new() -> Self {
        Self { user_paused: false, locked: false, display_off: false, on_battery: on_battery(), covered: desktop_covered() }
    }

    /// Pencere olayından sonra çağrılır.
    pub fn refresh_windows(&mut self) {
        self.covered = desktop_covered();
    }

    pub fn reason(&self, pause_on_battery: bool) -> Option<Reason> {
        if self.user_paused {
            Some(Reason::User)
        } else if self.locked {
            Some(Reason::Locked)
        } else if self.display_off {
            Some(Reason::DisplayOff)
        } else if self.covered {
            Some(Reason::Fullscreen)
        } else if pause_on_battery && self.on_battery {
            Some(Reason::Battery)
        } else {
            None
        }
    }
}

pub fn on_battery() -> bool {
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut s).is_ok() && s.ACLineStatus == 0 }
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// Öndeki pencere bulunduğu monitörü tamamen kaplıyor mu? Tek monitörde büyütülmüş
/// (maximize) pencere de duvar kâğıdını tamamen gizlediği için sayılır.
fn desktop_covered() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || !IsWindowVisible(fg).as_bool() || IsIconic(fg).as_bool() {
            return false;
        }
        if matches!(class_name(fg).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return false;
        }
        let mon = MonitorFromWindow(fg, MONITOR_DEFAULTTONULL);
        if mon.is_invalid() {
            return false;
        }
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let mut r = RECT::default();
        if !GetMonitorInfoW(mon, &mut mi).as_bool() || GetWindowRect(fg, &mut r).is_err() {
            return false;
        }
        let m = mi.rcMonitor;
        let fullscreen = r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom;
        fullscreen || (GetSystemMetrics(SM_CMONITORS) == 1 && IsZoomed(fg).as_bool())
    }
}

const OBJID_WINDOW: i32 = 0;

unsafe extern "system" fn on_win_event(_: HWINEVENTHOOK, event: u32, hwnd: HWND, object: i32, _: i32, _: u32, _: u32) {
    // Konum değişimi her pencere ve imleç için gelir; yalnızca öndeki pencereninki ilgilendirir.
    if event == EVENT_OBJECT_LOCATIONCHANGE && (object != OBJID_WINDOW || hwnd != unsafe { GetForegroundWindow() }) {
        return;
    }
    tray::push(Event::WindowsChanged);
}

/// Ön plan, küçültme ve konum olaylarını dinler. Olaylar mesaj döngüsü üzerinden gelir.
pub struct Hooks(Vec<HWINEVENTHOOK>);

impl Hooks {
    pub fn install() -> Self {
        let ranges = [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
        ];
        let hooks = ranges
            .iter()
            .map(|&(min, max)| unsafe {
                SetWinEventHook(min, max, None, Some(on_win_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS)
            })
            .filter(|h| !h.is_invalid())
            .collect();
        Self(hooks)
    }
}

impl Drop for Hooks {
    fn drop(&mut self) {
        for h in &self.0 {
            unsafe {
                let _ = UnhookWinEvent(*h);
            }
        }
    }
}
