use std::path::PathBuf;

use windows::Win32::System::Registry::{
    HKEY, KEY_READ, RRF_NOEXPAND, RRF_RT_ANY, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey, RegGetValueW, RegOpenKeyExW,
};
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
        MessageBoxW(None, PCWSTR(text.as_ptr()), w!("hive"), MB_ICONERROR | MB_OK);
    }
}

/// %LOCALAPPDATA%\Programs\hive — kurulu exe, ayarlar ve log burada durur.
pub fn data_dir() -> PathBuf {
    local_programs().join("hive")
}

/// %LOCALAPPDATA%\Programs
pub fn local_programs() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Programs")
}

/// Bu kopyanın yolu; WSL içinden (\\wsl.localhost\...) çalışan geliştirme kopyasıysa `None`:
/// o kopya kalıcı kayıt (Windows ile başlat vb.) yapmamalı.
pub fn installed_copy() -> Option<String> {
    let exe = std::env::current_exe().ok()?.display().to_string();
    (!exe.starts_with(r"\\")).then_some(exe)
}

/// Kayıt defteri anahtarı var mı.
pub fn reg_key_exists(root: HKEY, path: &str) -> bool {
    let p = wide(path);
    let mut h = HKEY::default();
    unsafe {
        let ok = RegOpenKeyExW(root, PCWSTR(p.as_ptr()), None, KEY_READ, &mut h).is_ok();
        if ok {
            let _ = RegCloseKey(h);
        }
        ok
    }
}

/// Kayıt defteri değeri var mı.
pub fn reg_value_exists(root: HKEY, path: &str, name: &str) -> bool {
    let (p, n) = (wide(path), wide(name));
    unsafe { RegGetValueW(root, PCWSTR(p.as_ptr()), PCWSTR(n.as_ptr()), RRF_RT_ANY, None, None, None).is_ok() }
}

/// Metin değeri (genişletilmeden).
/// Windows'un yapı numarası (Windows 11: 22000 ve sonrası).
pub fn windows_build() -> u32 {
    static BUILD: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *BUILD.get_or_init(|| {
        reg_string(windows::Win32::System::Registry::HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", "CurrentBuildNumber")
            .and_then(|b| b.trim().parse().ok())
            .unwrap_or(0)
    })
}

pub fn windows11() -> bool {
    windows_build() >= 22000
}

pub fn reg_string(root: HKEY, path: &str, name: &str) -> Option<String> {
    let (p, n) = (wide(path), wide(name));
    let mut buf = vec![0u16; 16 * 1024];
    let mut len = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            root,
            PCWSTR(p.as_ptr()),
            PCWSTR(n.as_ptr()),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
    }
    Some(String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]))
}
