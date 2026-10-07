//! Başlat menüsü: hive ve kurulu her araç için bir kısayol. Araç kısayolu hive'ı
//! o aracın sekmesinde açar (`--sekme`), simgesi aracın kendi simgesidir; Windows aramasında
//! "lyrebird" yazınca normal bir uygulama gibi çıkar.

use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{BSTR, Interface, PCWSTR, w};

use crate::log;
use crate::tools::TOOLS;
use crate::util::{self, wide};

fn programs_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs"))
}

/// Gömülü PNG'den tek görüntülü .ico (Vista'dan beri ICO içinde PNG geçerli).
fn write_ico(png: &[u8], size: u32, path: &Path) -> std::io::Result<()> {
    let s = if size >= 256 { 0 } else { size as u8 };
    let mut ico = vec![0, 0, 1, 0, 1, 0, s, s, 0, 0, 1, 0, 32, 0];
    ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes());
    ico.extend_from_slice(png);
    std::fs::write(path, ico)
}

/// COM başlatılmış iş parçacığında çağrılmalı.
fn create(link: &Path, exe: &str, args: &str, icon: Option<&Path>, description: &str) -> windows::core::Result<()> {
    unsafe {
        let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let (exe_w, args_w, desc_w) = (wide(exe), wide(args), wide(description));
        sl.SetPath(PCWSTR(exe_w.as_ptr()))?;
        sl.SetArguments(PCWSTR(args_w.as_ptr()))?;
        sl.SetDescription(PCWSTR(desc_w.as_ptr()))?;
        if let Some(icon) = icon {
            let icon_w = wide(&icon.display().to_string());
            sl.SetIconLocation(PCWSTR(icon_w.as_ptr()), 0)?;
        }
        let link_w = wide(&link.display().to_string());
        sl.cast::<IPersistFile>()?.Save(PCWSTR(link_w.as_ptr()), true)
    }
}

fn tool_link(i: usize) -> Option<PathBuf> {
    programs_dir().map(|d| d.join(format!("{}.lnk", TOOLS[i].name)))
}

fn tool_ico(i: usize) -> PathBuf {
    util::data_dir().join("ikonlar").join(format!("{}.ico", TOOLS[i].id))
}

/// Aracın Başlat menüsü kısayolu ve simgesi (kaldırınca silinir).
pub fn remove_tool(i: usize) {
    if let Some(l) = tool_link(i) {
        let _ = std::fs::remove_file(l);
    }
    let _ = std::fs::remove_file(tool_ico(i));
}

/// Aracın Başlat menüsünde kalan izleri.
pub fn tool_leftovers(i: usize) -> Vec<String> {
    tool_link(i).into_iter().chain([tool_ico(i)]).filter(|p| p.exists()).map(|p| p.display().to_string()).collect()
}

/// Kısayolları kurulu araçlara göre yazar, kaldırılanlarınkini siler. Geliştirme kopyası (WSL'den)
/// kalıcı kayıt yapmaz.
pub fn sync_shortcuts(installed: &[bool]) {
    let (Some(exe), Some(dir)) = (util::installed_copy(), programs_dir()) else { return };
    let _ = std::fs::remove_file(dir.join("matrixtools.lnk")); // eski adı
    if let Err(e) = create(&dir.join("hive.lnk"), &exe, "", None, t!("Your Windows tools in one place", "Windows araçların tek yerde")) {
        log!("Başlat menüsü kısayolu yazılamadı: {e}");
        return;
    }
    let icons = util::data_dir().join("ikonlar");
    let _ = std::fs::create_dir_all(&icons);
    for (tool, &on) in TOOLS.iter().zip(installed) {
        let link = dir.join(format!("{}.lnk", tool.name));
        if !on {
            let _ = std::fs::remove_file(&link);
            continue;
        }
        let ico = icons.join(format!("{}.ico", tool.id));
        let (size, png) = tool.icons.last().copied().unwrap_or((0, &[]));
        if write_ico(png, size, &ico).is_err() {
            continue;
        }
        if let Err(e) = create(&link, &exe, &format!("--tab {}", tool.id), Some(&ico), tool.tagline()) {
            log!("{} kısayolu yazılamadı: {e}", tool.name);
        }
    }
}

const UNINSTALL: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\hive");

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

/// Windows'un Uygulamalar listesine kayıt: oradan kaldırınca `hive --kaldir` çalışır.
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
        set_str(key, w!("DisplayName"), "hive");
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

const RUN: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_APPROVED: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run");

/// Windows ile başlama, Görev Zamanlayıcı'da oturum açılınca çalışan bir görevle: Run kaydındakiler
/// masaüstü yüklendikten sonra hep birlikte açılır, görev ise ondan önce ve gecikmesiz çalışır.
/// Böylece hive ilk açılan olur. Görev kullanıcıya özel, yönetici izni gerekmez.
fn task_name() -> String {
    format!("hive ({})", std::env::var("USERNAME").unwrap_or_default())
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

/// `Some(exe)`: o exe ile Windows ile başla; `None`: başlama. COM başlatılmış iş parçacığında çağrılmalı.
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
    // Öncelik 4 normal (görevlerin varsayılanı 7, düşük); süre sınırı yok, pilde de başlar.
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Author>hive</Author><Description>{desc}</Description></RegistrationInfo>
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
        desc = xml_escape(t!("Starts hive first when you sign in", "Oturum açınca hive'ı ilk sırada başlatır")),
        exe = xml_escape(exe),
    );
    let none = VARIANT::default();
    let r = unsafe {
        root.RegisterTask(&name, &BSTR::from(xml), TASK_CREATE_OR_UPDATE.0, &none, &none, TASK_LOGON_INTERACTIVE_TOKEN, &none)
    };
    if let Err(e) = r {
        log!("Windows ile başlama görevi yazılamadı: {e}");
    }
}

/// Eski sürümler Run kaydını kullanıyordu: görevle değiştir. Görev Yöneticisi'nden kapatılmışsa
/// kapalı kalır.
pub fn migrate_autostart() {
    let Some(exe) = util::installed_copy() else { return };
    unsafe {
        if RegGetValueW(HKEY_CURRENT_USER, RUN, w!("hive"), RRF_RT_REG_SZ, None, None, None).is_err() {
            return;
        }
        let mut flags = [0u8; 12];
        let mut size = flags.len() as u32;
        let disabled = RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_APPROVED,
            w!("hive"),
            RRF_RT_REG_BINARY,
            None,
            Some(flags.as_mut_ptr().cast()),
            Some(&mut size),
        )
        .is_ok()
            && flags[0] & 1 == 1;
        if !disabled {
            set_autostart(Some(&exe));
        }
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, w!("hive"));
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_APPROVED, w!("hive"));
    }
    log!("Windows ile başlama Run kaydından göreve taşındı");
}

/// hive'ın kendi izleri: Uygulamalar kaydı, Windows ile başlama, Başlat menüsü kısayolu.
/// Klasörü süreç kapandıktan sonra `remove_self_later` siler.
pub fn unregister_app() {
    set_autostart(None);
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, UNINSTALL);
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, w!("hive"));
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_APPROVED, w!("hive"));
    }
    if let Some(dir) = programs_dir() {
        let _ = std::fs::remove_file(dir.join("hive.lnk"));
    }
}

/// Çalışan exe kendini silemez: bu süreç kapandıktan birkaç saniye sonra klasörü silen, pencere
/// açmayan bir komut bırakır.
pub fn remove_self_later() {
    use std::os::windows::process::CommandExt;
    let dir = util::data_dir();
    let line = format!("ping -n 4 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display());
    let _ = std::process::Command::new("cmd.exe")
        .args(["/c", &line])
        .creation_flags(0x0800_0000 | 0x0000_0008) // CREATE_NO_WINDOW | DETACHED_PROCESS
        .spawn();
}

/// Çalışan hive'ı kapatır: önce kibarca (WM_EXIT), 5 saniyede kapanmazsa (eski sürüm mesajı
/// tanımıyorsa) süreci sonlandırır. cheshire motoru hive'ın gittiğini görüp kendisi kapanır.
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
    log!("çalışan hive kapanmadı, sonlandırıldı (pid {pid})");
}

/// İndirilen exe (ya da `hive --kur`): kendini %LOCALAPPDATA%\Programs\hive'a kurar ve oradan
/// başlatır. Çalışan bir hive varsa önce kapanır; aynı yol güncelleme için de kullanılır.
pub fn install_self() -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let src = std::env::current_exe().map_err(|e| e.to_string())?;
    let dst = util::data_dir().join("hive.exe");
    // İlk kurulumda Windows ile başlama açık gelir; güncelleme kullanıcının seçimine dokunmaz.
    let fresh = !dst.exists();
    stop_running();
    std::fs::create_dir_all(util::data_dir()).map_err(|e| e.to_string())?;
    if src != dst {
        // Kapanan kopya exe'yi birkaç saniye tutabilir.
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
    std::process::Command::new(&dst)
        .creation_flags(0x0000_0008) // DETACHED_PROCESS
        .spawn()
        .map_err(|e| e.to_string())?;
    log!("hive kuruldu: {}", dst.display());
    Ok(())
}
