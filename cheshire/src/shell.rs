//! Kabukla bütünleşme: "Windows ile başlat" ve `.cheshire` dosya ilişkilendirmesi.
//! İkisi de HKCU altında: yönetici izni gerekmez, yalnızca bu kullanıcıyı etkiler.

use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
use windows::core::{PCWSTR, w};

use crate::log;
use crate::util::wide;

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("cheshire");

fn exe() -> Option<String> {
    std::env::current_exe().ok().map(|p| p.display().to_string())
}

/// WSL içinden (\\wsl.localhost\...) çalışan geliştirme kopyası kalıcı kayıt yapmamalı:
/// her açılışta WSL'i uyandırır, klasör silinince de bozuk kayıt kalır.
pub fn installed_copy() -> bool {
    exe().is_some_and(|p| !p.starts_with(r"\\"))
}

fn read(key: &str, value: Option<&str>) -> Option<String> {
    let key = wide(key);
    let value = value.map(wide);
    let value_ptr = value.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr()));
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            value_ptr,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
    }
    let n = (len as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..n]))
}

fn write(key: &str, value: Option<&str>, data: &str) {
    let key = wide(key);
    let value = value.map(wide);
    let data = wide(data);
    unsafe {
        let _ = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            value.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr())),
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        );
    }
}

pub fn autostart_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

pub fn set_autostart(on: bool) {
    match (on, exe()) {
        (true, Some(exe)) => write(r"Software\Microsoft\Windows\CurrentVersion\Run", Some("cheshire"), &format!("\"{exe}\"")),
        (true, None) => {}
        (false, _) => unsafe {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
        },
    }
}

/// `.cheshire` dosyalarını bu exe'ye bağlar: çift tıklama ya da exe'ye sürükleme = kur ve uygula.
/// Kayıt zaten doğruysa dokunulmaz.
pub fn associate() {
    let Some(exe) = exe().filter(|_| installed_copy()) else { return };
    let command = format!("\"{exe}\" \"%1\"");
    let open = r"Software\Classes\cheshire.dosya\shell\open\command";
    if read(open, None).as_deref() == Some(command.as_str()) {
        return;
    }
    write(r"Software\Classes\.cheshire", None, "cheshire.dosya");
    write(r"Software\Classes\cheshire.dosya", None, "cheshire duvar kâğıdı");
    write(r"Software\Classes\cheshire.dosya\DefaultIcon", None, &format!("\"{exe}\",0"));
    write(open, None, &command);
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    log!(".cheshire dosyaları ilişkilendirildi: {exe}");
}
