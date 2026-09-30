use std::path::PathBuf;

use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{PCWSTR, w};

pub type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// UTF-8 → null ile biten UTF-16 (Win32 W fonksiyonları için).
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn error_box(msg: &str) {
    let text = wide(msg);
    unsafe {
        MessageBoxW(None, PCWSTR(text.as_ptr()), w!("cheshire"), MB_ICONERROR | MB_OK);
    }
}

/// %APPDATA%\cheshire — ayarlar, log ve duvar kâğıtları burada durur.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("cheshire")
}

pub fn wallpapers_dir() -> PathBuf {
    data_dir().join("duvarlar")
}

/// (iDate, iLocalTime). iDate Shadertoy'daki gibi: yıl, ay (0 tabanlı), gün, gece yarısından beri saniye.
pub fn local_clock() -> ([f32; 4], f32) {
    let t = unsafe { GetLocalTime() };
    let secs = t.wHour as f32 * 3600.0 + t.wMinute as f32 * 60.0 + t.wSecond as f32 + t.wMilliseconds as f32 / 1000.0;
    ([t.wYear as f32, t.wMonth as f32 - 1.0, t.wDay as f32, secs], secs / 3600.0)
}

/// 0..1 pil doluluğu; pil yoksa ya da bilinmiyorsa 1.
pub fn battery_level() -> f32 {
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe {
        if GetSystemPowerStatus(&mut s).is_ok() && s.BatteryLifePercent <= 100 {
            return s.BatteryLifePercent as f32 / 100.0;
        }
    }
    1.0
}
