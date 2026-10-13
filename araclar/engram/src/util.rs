#![allow(dead_code)]
use std::path::PathBuf;

use windows::Win32::System::Registry::{
    HKEY, KEY_READ, RRF_NOEXPAND, RRF_RT_ANY, RRF_RT_REG_DWORD, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey, RegGetValueW,
    RegOpenKeyExW,
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
        MessageBoxW(None, PCWSTR(text.as_ptr()), w!("engram"), MB_ICONERROR | MB_OK);
    }
}

/// %LOCALAPPDATA%\Programs\winloom\engram: ayarlar, anahtarlar, ses önbelleği. Notlar kullanıcının
/// kendi klasöründe.
pub fn data_dir() -> PathBuf {
    ortak::util::data_dir().join("engram")
}

/// engram'ın eski, tek başına kurulumu (%LOCALAPPDATA%\Programs\engram).
pub fn standalone_dir() -> PathBuf {
    local_programs().join("engram")
}

/// Kayıt defterinden DWORD.
pub fn reg_dword(root: HKEY, path: &str, name: &str) -> Option<u32> {
    let (p, n) = (wide(path), wide(name));
    let mut v = 0u32;
    let mut len = 4u32;
    unsafe {
        RegGetValueW(
            root,
            PCWSTR(p.as_ptr()),
            PCWSTR(n.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut v as *mut u32).cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
    }
    Some(v)
}

/// Dosyayı ya da adresi varsayılan uygulamayla açar.
pub fn open_url(target: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let t = wide(target);
    unsafe {
        let _ = ShellExecuteW(None, w!("open"), PCWSTR(t.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Dosyayı Gezgin'de seçili gösterir.
pub fn reveal(path: &std::path::Path) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let args = wide(&format!("/select,\"{}\"", path.display()));
    unsafe {
        let _ = ShellExecuteW(None, w!("open"), w!("explorer.exe"), PCWSTR(args.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Panodaki metin.
pub fn clipboard_text() -> Option<String> {
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    unsafe {
        OpenClipboard(None).ok()?;
        let r = (|| {
            let h = GetClipboardData(13).ok()?; // CF_UNICODETEXT
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(g);
            Some(s)
        })();
        let _ = CloseClipboard();
        r
    }
}

/// UNIX saniyesinden yerel takvim günü (yıl, ay, gün); saat dilimi için yerel-UTC farkı eklenir.
pub fn civil(secs: u64) -> (i64, u32, u32) {
    let offset = local_offset_secs();
    let days = (secs as i64 + offset).div_euclid(86_400);
    // Howard Hinnant'ın days_from_civil tersi.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn local_offset_secs() -> i64 {
    use windows::Win32::System::Time::GetTimeZoneInformation;
    let mut tz = Default::default();
    let r = unsafe { GetTimeZoneInformation(&mut tz) };
    // Bias dakika, UTC = yerel + bias; yaz saatinde DaylightBias eklenir (r == 2).
    let bias = tz.Bias + if r == 2 { tz.DaylightBias } else { tz.StandardBias };
    -(bias as i64) * 60
}

/// "12 Eki" gibi kısa tarih; bugün ise "bugün", dün ise "dün".
pub fn short_date(secs: u64) -> String {
    let (y, m, d) = civil(secs);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |t| t.as_secs());
    let (ny, nm, nd) = civil(now);
    if (y, m, d) == (ny, nm, nd) {
        return t!("today", "bugün").into();
    }
    if civil(now.saturating_sub(86_400)) == (y, m, d) {
        return t!("yesterday", "dün").into();
    }
    let months = if crate::i18n::turkish() {
        ["Oca", "Şub", "Mar", "Nis", "May", "Haz", "Tem", "Ağu", "Eyl", "Eki", "Kas", "Ara"]
    } else {
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
    };
    let mon = months[(m as usize).clamp(1, 12) - 1];
    if y == ny { format!("{d} {mon}") } else { format!("{d} {mon} {y}") }
}

/// Bugünün tarihi `YYYY-AA-GG` ve saati `SS:DD` (yerel saat).
pub fn now() -> (String, String) {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    (format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay), format!("{:02}:{:02}", t.wHour, t.wMinute))
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

/// Evet/hayır sorusu (uyarı simgesiyle).
pub fn ask(text: &str) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{IDYES, MB_ICONQUESTION, MB_YESNO};
    let t = wide(text);
    unsafe { MessageBoxW(None, PCWSTR(t.as_ptr()), w!("engram"), MB_YESNO | MB_ICONQUESTION) == IDYES }
}

/// Klasör seçme penceresi; `start` klasöründe açılır. Vazgeçilirse `None`.
pub fn pick_folder(owner: windows::Win32::Foundation::HWND, start: &std::path::Path) -> Option<PathBuf> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
    use windows::Win32::UI::Shell::{FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH};
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dlg.GetOptions().ok()?;
        dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        let title = wide(t!("Choose the brain folder", "Beyin klasörünü seç"));
        let _ = dlg.SetTitle(PCWSTR(title.as_ptr()));
        // Klasör yoksa üst klasörden aç.
        let mut dir = start.to_path_buf();
        while !dir.exists() && dir.pop() {}
        let p = wide(&dir.display().to_string());
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(p.as_ptr()), None) {
            let _ = dlg.SetFolder(&item);
        }
        dlg.Show(Some(owner)).ok()?;
        let item = dlg.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        s.map(PathBuf::from)
    }
}

/// İmlecin yanında açılan menü; seçilen satırın sırası. `checked` satırı işaretli.
pub fn popup_menu(owner: windows::Win32::Foundation::HWND, items: &[String], checked: Option<usize>) -> Option<usize> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, MF_CHECKED, MF_STRING, SetForegroundWindow, TPM_NONOTIFY, TPM_RETURNCMD,
        TrackPopupMenu,
    };
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for (i, it) in items.iter().enumerate() {
            let w = wide(it);
            let flags = if checked == Some(i) { MF_STRING | MF_CHECKED } else { MF_STRING };
            let _ = AppendMenuW(menu, flags, i + 1, PCWSTR(w.as_ptr()));
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(owner);
        let r = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY, pt.x, pt.y, None, owner, None);
        let _ = DestroyMenu(menu);
        (r.0 > 0).then(|| r.0 as usize - 1)
    }
}
