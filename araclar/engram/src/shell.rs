//! engram'ın eski, tek başına kurulumundan winloom'a geçiş. Tek başınayken engram kendini
//! Başlat menüsüne, Uygulamalar listesine ve Görev Zamanlayıcı'ya (oturumda başlama) yazıyordu;
//! winloom'da bunlar winloom'ın.

use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteTreeW};
use windows::core::{BSTR, PCWSTR, w};

use crate::log;
use crate::util;

/// Tek başına engram'ın oturumda başlama görevi.
fn task_name() -> String {
    format!("engram ({})", std::env::var("USERNAME").unwrap_or_default())
}

fn delete_task() {
    use windows::Win32::System::TaskScheduler::{ITaskService, TaskScheduler};
    use windows::Win32::System::Variant::VARIANT;
    unsafe {
        let Ok(svc) = CoCreateInstance::<_, ITaskService>(&TaskScheduler, None, CLSCTX_INPROC_SERVER) else { return };
        let none = VARIANT::default();
        if svc.Connect(&none, &none, &none, &none).is_err() {
            return;
        }
        if let Ok(root) = svc.GetFolder(&BSTR::from("\\")) {
            let _ = root.DeleteTask(&BSTR::from(task_name()), 0);
        }
    }
}

/// Tek başına engram'ın izleri: oturumda başlama, Uygulamalar kaydı, Başlat menüsü kısayolu.
fn unregister_standalone() {
    delete_task();
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\engram"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let link = std::path::PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\engram.lnk");
        let _ = std::fs::remove_file(link);
    }
}

/// Ayrı süreçte çalışan engram'ı (tek başına kurulum) kapatır: önce kibarca, 5 saniyede
/// kapanmazsa süreci sonlandırır.
fn stop_running() {
    use windows::Win32::Foundation::{CloseHandle, LPARAM, WPARAM};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId, PostMessageW, WM_APP};
    // Tek başına engram'ın penceresi ve tamamen kapanma mesajı.
    let find = || unsafe { FindWindowW(w!("engram"), PCWSTR::null()).ok() };
    let Some(h) = find() else { return };
    unsafe {
        let _ = PostMessageW(Some(h), WM_APP + 5, WPARAM(0), LPARAM(0));
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

/// engram açılırken: tek başına kurulu engram varsa kapatılır, kendi izleri silinir, ayarları ve
/// önbellekleri winloom'ın klasörüne taşınır, eski klasör silinir. Notlar kullanıcının klasöründe,
/// dokunulmaz. COM başlatılmış iş parçacığında çağrılmalı.
pub fn adopt() {
    let (old, new) = (util::standalone_dir(), util::data_dir());
    stop_running();
    unregister_standalone();
    let _ = std::fs::create_dir_all(&new);
    // Ayrı süreçli ara sürümden kalanlar.
    for f in ["engram.exe", "engram.log", "kurulum.log"] {
        let _ = std::fs::remove_file(new.join(f));
    }
    if !old.is_dir() {
        return;
    }
    let mut moved = true;
    for e in std::fs::read_dir(&old).into_iter().flatten().flatten() {
        let name = e.file_name();
        let skip = ["engram.exe", "engram.log", "kurulum.log"].iter().any(|n| name.eq_ignore_ascii_case(n));
        let dst = new.join(&name);
        if skip {
            continue;
        }
        // winloom'daki klasör yeni: orada bir şey varsa tek başına kurulumdaki asıl olanın yerini
        // bırakır.
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
