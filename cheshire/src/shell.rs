//! Kabukla bütünleşme: "Windows ile başlat", `.cheshire` dosya ilişkilendirmesi, Başlat menüsü
//! kısayolu ve "Uygulamalar ve özellikler" kaydı. Hepsi HKCU / kullanıcı klasörlerinde: yönetici
//! izni gerekmez, yalnızca bu kullanıcıyı etkiler. `unregister` hepsini geri alır.

use std::path::PathBuf;

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW,
    RegSetKeyValueW,
};
use windows::Win32::UI::Shell::{IShellLinkW, SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify, ShellLink};
use windows::core::{Interface, PCWSTR, w};

use crate::log;
use crate::util::{self, wide};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("cheshire");
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\cheshire";
const CLASSES: [&str; 2] = [r"Software\Classes\.cheshire", r"Software\Classes\cheshire.dosya"];

fn exe() -> Option<String> {
    std::env::current_exe().ok().map(|p| p.display().to_string())
}

/// Bu süreç `%LOCALAPPDATA%\Programs\cheshire\cheshire.exe` mi?
pub fn installed_copy() -> bool {
    let same = |a: PathBuf, b: PathBuf| a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy());
    match (std::env::current_exe().and_then(std::fs::canonicalize), std::fs::canonicalize(util::installed_exe())) {
        (Ok(a), Ok(b)) => same(a, b),
        _ => false,
    }
}

/// WSL içinden (\\wsl.localhost\...) çalışan geliştirme kopyası: kendini kurmaz, kalıcı kayıt yapmaz.
/// Kursaydı her açılışta WSL'i uyandırırdı, klasör silinince de bozuk kayıt kalırdı.
pub fn dev_copy() -> bool {
    exe().is_some_and(|p| p.starts_with(r"\\"))
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

fn write_dword(key: &str, value: &str, data: u32) {
    let (key, value) = (wide(key), wide(value));
    unsafe {
        let _ = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            REG_DWORD.0,
            Some((&data as *const u32).cast()),
            4,
        );
    }
}

fn delete_tree(key: &str) {
    let key = wide(key);
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()));
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

/// Kurulu kopya her açılışta çağırır: kayıtlar eksikse ya da eski sürümü gösteriyorsa düzeltir.
/// İlk kurulumda "Windows ile başlat" da açılır.
pub fn register() {
    let Some(exe) = exe().filter(|_| installed_copy()) else { return };
    let first = read(UNINSTALL_KEY, Some("DisplayName")).is_none();
    associate(&exe);

    let dir = util::app_dir().display().to_string();
    let size_kb = std::fs::metadata(&exe).map_or(0, |m| (m.len() / 1024) as u32);
    for (name, value) in [
        ("DisplayName", "cheshire"),
        ("DisplayVersion", env!("CARGO_PKG_VERSION")),
        ("Publisher", "matrixdurden"),
        ("DisplayIcon", &format!("\"{exe}\",0")),
        ("InstallLocation", &dir),
        ("UninstallString", &format!("\"{exe}\" --kaldir")),
        ("URLInfoAbout", env!("CARGO_PKG_REPOSITORY")),
    ] {
        write(UNINSTALL_KEY, Some(name), value);
    }
    write_dword(UNINSTALL_KEY, "EstimatedSize", size_kb);
    write_dword(UNINSTALL_KEY, "NoModify", 1);
    write_dword(UNINSTALL_KEY, "NoRepair", 1);

    let link = shortcut_path();
    if !link.exists()
        && let Err(e) = create_shortcut(&exe, &link)
    {
        log!("Başlat menüsü kısayolu oluşturulamadı: {e}");
    }
    if first {
        set_autostart(true);
        log!("kuruldu: {dir}");
    }
}

/// `register`'ın ve ilişkilendirmenin yaptığı her şeyi geri alır. Klasörü silmez.
pub fn unregister() {
    set_autostart(false);
    for key in CLASSES {
        delete_tree(key);
    }
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    delete_tree(UNINSTALL_KEY);
    let _ = std::fs::remove_file(shortcut_path());
}

/// `.cheshire` dosyalarını bu exe'ye bağlar: çift tıklama ya da exe'ye sürükleme = kur ve uygula.
/// Kayıt zaten doğruysa dokunulmaz.
fn associate(exe: &str) {
    let command = format!("\"{exe}\" \"%1\"");
    let open = r"Software\Classes\cheshire.dosya\shell\open\command";
    if read(open, None).as_deref() == Some(command.as_str()) {
        return;
    }
    write(CLASSES[0], None, "cheshire.dosya");
    write(CLASSES[1], None, "cheshire duvar kâğıdı");
    write(r"Software\Classes\cheshire.dosya\DefaultIcon", None, &format!("\"{exe}\",0"));
    write(open, None, &command);
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    log!(".cheshire dosyaları ilişkilendirildi: {exe}");
}

fn shortcut_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    base.join(r"Microsoft\Windows\Start Menu\Programs\cheshire.lnk")
}

fn create_shortcut(exe: &str, link: &std::path::Path) -> windows::core::Result<()> {
    unsafe {
        // Zaten başka kipte başlatılmışsa RPC_E_CHANGED_MODE döner; COM yine kullanılabilir.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let (exe_w, desc) = (wide(exe), wide("GPU shader canlı duvar kâğıdı"));
        sl.SetPath(PCWSTR(exe_w.as_ptr()))?;
        sl.SetDescription(PCWSTR(desc.as_ptr()))?;
        let link_w = wide(&link.display().to_string());
        sl.cast::<IPersistFile>()?.Save(PCWSTR(link_w.as_ptr()), true)
    }
}
