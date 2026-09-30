use std::path::{Path, PathBuf};

use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{PCWSTR, PWSTR, w};

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

/// %LOCALAPPDATA%\Programs\cheshire — kurulu exe, ayarlar, log ve duvar kâğıtları hep burada.
/// Kaldırınca bu klasör silinir, geride bir şey kalmaz.
pub fn app_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Programs").join("cheshire")
}

pub fn installed_exe() -> PathBuf {
    app_dir().join("cheshire.exe")
}

pub fn wallpapers_dir() -> PathBuf {
    app_dir().join("duvarlar")
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

/// Süreci tutamaç mirası olmadan başlatır. std `Command` bütün miras alınabilir tutamaçları
/// geçirir; o zaman çağıran (ör. WSL'den `make install`) başlatılan kopya kapanana dek bekler.
/// `command_line` ilk öğesi exe olan tam komut satırıdır, tırnaklama çağıranda.
pub fn spawn_detached(command_line: &str, cwd: Option<&Path>, flags: PROCESS_CREATION_FLAGS) -> Res<()> {
    let mut line = wide(command_line);
    let cwd = cwd.map(|d| wide(&d.to_string_lossy()));
    let si = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            flags,
            None,
            cwd.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            &si,
            &mut pi,
        )?;
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
    }
    Ok(())
}
