//! Windows'a kayıt: Başlat menüsü kısayolu, Uygulamalar listesi, oturum açınca başlama (Görev
//! Zamanlayıcı), kendini kurma ve iz bırakmadan kaldırma. WSL'den çalışan geliştirme kopyası
//! (\\wsl.localhost\...) kalıcı kayıt yapmaz.

use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW,
    RegDeleteTreeW, RegSetValueExW,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{BSTR, Interface, PCWSTR, w};

use crate::log;
use crate::util::{self, wide};

const NAME: &str = "engram";

fn programs_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs"))
}

/// COM başlatılmış iş parçacığında çağrılmalı.
fn create_link(link: &Path, exe: &str, args: &str, description: &str) -> windows::core::Result<()> {
    unsafe {
        let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let (exe_w, args_w, desc_w) = (wide(exe), wide(args), wide(description));
        sl.SetPath(PCWSTR(exe_w.as_ptr()))?;
        sl.SetArguments(PCWSTR(args_w.as_ptr()))?;
        sl.SetDescription(PCWSTR(desc_w.as_ptr()))?;
        let link_w = wide(&link.display().to_string());
        sl.cast::<IPersistFile>()?.Save(PCWSTR(link_w.as_ptr()), true)
    }
}

/// Başlat menüsüne "engram" kısayolu.
pub fn sync_shortcut() {
    let (Some(exe), Some(dir)) = (util::installed_copy(), programs_dir()) else { return };
    let desc = t!("Your second brain: notes that write themselves", "İkinci beynin: kendi kendine yazılan notlar");
    if let Err(e) = create_link(&dir.join(format!("{NAME}.lnk")), &exe, "", desc) {
        log!("Başlat menüsü kısayolu yazılamadı: {e}");
    }
}

const UNINSTALL: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\engram");

fn set_str(key: HKEY, name: PCWSTR, value: &str) {
    let v = wide(value);
    unsafe {
        let _ = RegSetValueExW(key, name, None, REG_SZ, Some(std::slice::from_raw_parts(v.as_ptr().cast(), v.len() * 2)));
    }
}

fn set_dword(key: HKEY, name: PCWSTR, value: u32) {
    unsafe {
        let _ = RegSetValueExW(key, name, None, REG_DWORD, Some(&value.to_le_bytes()));
    }
}

/// Windows'un Uygulamalar listesine kayıt: oradan kaldırınca `engram --uninstall` çalışır.
pub fn register_app() {
    let Some(exe) = util::installed_copy() else { return };
    let size_kb = std::fs::metadata(&exe).map_or(0, |m| m.len() / 1024) as u32;
    unsafe {
        let mut key = HKEY::default();
        if RegCreateKeyExW(HKEY_CURRENT_USER, UNINSTALL, None, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut key, None)
            .is_err()
        {
            return;
        }
        set_str(key, w!("DisplayName"), NAME);
        set_str(key, w!("DisplayVersion"), env!("CARGO_PKG_VERSION"));
        set_str(key, w!("Publisher"), "matrixdurden");
        set_str(key, w!("DisplayIcon"), &format!("\"{exe}\",0"));
        set_str(key, w!("InstallLocation"), &util::data_dir().display().to_string());
        set_str(key, w!("UninstallString"), &format!("\"{exe}\" --uninstall"));
        set_dword(key, w!("EstimatedSize"), size_kb);
        set_dword(key, w!("NoModify"), 1);
        set_dword(key, w!("NoRepair"), 1);
        let _ = RegCloseKey(key);
    }
}

/// Oturum açınca başlama: Görev Zamanlayıcı'da kullanıcıya özel görev, yönetici izni gerekmez,
/// masaüstü yüklenmeden çalışır.
fn task_name() -> String {
    format!("{NAME} ({})", std::env::var("USERNAME").unwrap_or_default())
}

fn task_root() -> windows::core::Result<windows::Win32::System::TaskScheduler::ITaskFolder> {
    use windows::Win32::System::TaskScheduler::{ITaskService, TaskScheduler};
    use windows::Win32::System::Variant::VARIANT;
    unsafe {
        let svc: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
        let none = VARIANT::default();
        svc.Connect(&none, &none, &none, &none)?;
        svc.GetFolder(&BSTR::from("\\"))
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// COM başlatılmış iş parçacığında çağrılmalı.
pub fn autostart() -> bool {
    task_root().and_then(|r| unsafe { r.GetTask(&BSTR::from(task_name())) }).is_ok()
}

/// `Some(exe)`: o exe ile oturumda başla; `None`: başlama. COM başlatılmış iş parçacığında çağrılmalı.
pub fn set_autostart(exe: Option<&str>) {
    use windows::Win32::System::TaskScheduler::{TASK_CREATE_OR_UPDATE, TASK_LOGON_INTERACTIVE_TOKEN};
    use windows::Win32::System::Variant::VARIANT;
    let root = match task_root() {
        Ok(r) => r,
        Err(e) => {
            log!("Görev Zamanlayıcı açılamadı: {e}");
            return;
        }
    };
    let name = BSTR::from(task_name());
    let Some(exe) = exe else {
        let _ = unsafe { root.DeleteTask(&name, 0) };
        return;
    };
    let user = xml_escape(&format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").unwrap_or_default()
    ));
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Author>engram</Author><Description>{desc}</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>--hidden</Arguments></Exec></Actions>
</Task>"#,
        desc = xml_escape(t!("Starts engram when you sign in", "Oturum açınca engram'ı başlatır")),
        exe = xml_escape(exe),
    );
    let none = VARIANT::default();
    let r = unsafe {
        root.RegisterTask(&name, &BSTR::from(xml), TASK_CREATE_OR_UPDATE.0, &none, &none, TASK_LOGON_INTERACTIVE_TOKEN, &none)
    };
    if let Err(e) = r {
        log!("oturumda başlama görevi yazılamadı: {e}");
    }
}

/// engram'ın kendi izleri: Uygulamalar kaydı, oturumda başlama, Başlat menüsü kısayolu.
/// Klasörü süreç kapandıktan sonra `remove_self_later` siler.
pub fn unregister_app() {
    set_autostart(None);
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, UNINSTALL);
    }
    if let Some(dir) = programs_dir() {
        let _ = std::fs::remove_file(dir.join(format!("{NAME}.lnk")));
    }
}

/// Çalışan exe kendini silemez: bu süreç kapandıktan birkaç saniye sonra klasörü silen, pencere
/// açmayan bir komut bırakır. Notlar kullanıcının klasöründe, onlara dokunulmaz.
pub fn remove_self_later() {
    use std::os::windows::process::CommandExt;
    let dir = util::data_dir();
    let line = format!("ping -n 4 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display());
    let _ = std::process::Command::new("cmd.exe")
        .args(["/c", &line])
        .creation_flags(0x0800_0000 | 0x0000_0008) // CREATE_NO_WINDOW | DETACHED_PROCESS
        .spawn();
}

/// Çalışan engram'ı kapatır: önce kibarca (WM_EXIT), 5 saniyede kapanmazsa süreci sonlandırır.
fn stop_running() {
    use windows::Win32::Foundation::{CloseHandle, LPARAM, WPARAM};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId, PostMessageW};
    let find = || unsafe { FindWindowW(crate::app::CLASS, PCWSTR::null()).ok() };
    let Some(h) = find() else { return };
    unsafe {
        let _ = PostMessageW(Some(h), crate::app::WM_EXIT, WPARAM(0), LPARAM(0));
    }
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if find().is_none() {
            return;
        }
    }
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(h, Some(&mut pid));
        if let Ok(p) = OpenProcess(PROCESS_TERMINATE, false, pid) {
            let _ = TerminateProcess(p, 0);
            let _ = CloseHandle(p);
        }
    }
    log!("çalışan engram kapanmadı, sonlandırıldı (pid {pid})");
}

/// İndirilen exe (ya da `engram --install`): kendini %LOCALAPPDATA%\Programs\engram'a kurar ve
/// oradan başlatır. Çalışan bir kopya varsa önce kapanır; aynı yol güncelleme için de kullanılır.
pub fn install_self(hidden: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let src = std::env::current_exe().map_err(|e| e.to_string())?;
    let dst = util::data_dir().join("engram.exe");
    // İlk kurulumda oturumda başlama açık gelir; güncelleme kullanıcının seçimine dokunmaz.
    let fresh = !dst.exists();
    stop_running();
    std::fs::create_dir_all(util::data_dir()).map_err(|e| e.to_string())?;
    if src != dst {
        let mut last = String::new();
        let copied = (0..40).any(|_| match std::fs::copy(&src, &dst) {
            Ok(_) => true,
            Err(e) => {
                last = e.to_string();
                std::thread::sleep(std::time::Duration::from_millis(250));
                false
            }
        });
        if !copied {
            return Err(format!("{} yazılamadı: {last}", dst.display()));
        }
    }
    if fresh {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        set_autostart(Some(&dst.display().to_string()));
    }
    let mut cmd = std::process::Command::new(&dst);
    if hidden {
        cmd.arg("--hidden");
    }
    cmd.creation_flags(0x0000_0008) // DETACHED_PROCESS
        .spawn()
        .map_err(|e| e.to_string())?;
    log!("engram kuruldu: {}", dst.display());
    Ok(())
}

/// `--adopt`: winloom engram'ı açarken. Tek başına kurulu engram varsa kapatılır, kendi izleri
/// (oturumda başlama, Uygulamalar kaydı, Başlat kısayolu) silinir, ayarları ve önbellekleri
/// winloom'ın klasörüne taşınır, eski klasör silinir. Notlar kullanıcının klasöründe, dokunulmaz.
/// COM başlatılmış iş parçacığında çağrılmalı.
pub fn adopt() {
    let (old, new) = (util::standalone_dir(), util::hub_dir());
    stop_running();
    unregister_app();
    if !old.is_dir() {
        return;
    }
    let _ = std::fs::create_dir_all(&new);
    let mut moved = true;
    for e in std::fs::read_dir(&old).into_iter().flatten().flatten() {
        let name = e.file_name();
        let skip = ["engram.exe", "engram.log", "kurulum.log"].iter().any(|n| name.eq_ignore_ascii_case(n));
        let dst = new.join(&name);
        if skip {
            continue;
        }
        // winloom'daki klasör yeni: orada bir şey varsa (boş ayarlarla açılmış bir kopyanınki)
        // tek başına kurulumdaki asıl olanın yerini bırakır.
        if dst.is_dir() {
            let _ = std::fs::remove_dir_all(&dst);
        } else if dst.exists() {
            let _ = std::fs::remove_file(&dst);
        }
        match std::fs::rename(e.path(), &dst) {
            Ok(()) => log!("{} winloom'a taşındı", name.to_string_lossy()),
            Err(err) => {
                log!("{} taşınamadı: {err}", name.to_string_lossy());
                moved = false;
            }
        }
    }
    // Taşınamayan bir şey (ayarlar, anahtarlar) varsa eski klasör silinmez.
    if !moved {
        log!("eski klasör yerinde bırakıldı: {}", old.display());
        return;
    }
    // Kapanan sürecin exe'si birkaç saniye kilitli kalabilir.
    for _ in 0..20 {
        if std::fs::remove_dir_all(&old).is_ok() || !old.exists() {
            log!("tek başına engram kaldırıldı: {}", old.display());
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    log!("eski klasör silinemedi: {}", old.display());
}
