//! Kurulum ve kaldırma. Ayrı bir kurulum programı yok: indirilen exe çalışınca kendini
//! `%LOCALAPPDATA%\Programs\cheshire`'a kopyalar, oradaki kopyayı başlatır ve çıkar. Kayıtları
//! (kısayol, ilişkilendirme, "Uygulamalar ve özellikler") kurulu kopya açılışta kendisi yapar.

use std::path::{Path, PathBuf};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, OpenProcess, PROCESS_CREATION_FLAGS, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowThreadProcessId, IDYES, MB_ICONQUESTION, MB_YESNO, MessageBoxW, PostMessageW, WM_CLOSE,
};
use windows::core::{PCWSTR, w};

use crate::util::{self, Res};
use crate::{log, shell, tray};

/// Bu exe'yi kurulum klasörüne kopyalar ve kurulu kopyayı başlatır. Çalışan eski kopya önce kapatılır.
pub fn install(file: Option<&str>) -> Res<()> {
    let dst = util::installed_exe();
    std::fs::create_dir_all(util::app_dir())?;
    close_running();
    copy_retrying(&std::env::current_exe()?, &dst)?;
    let mut line = format!("\"{}\" --kuruldu", dst.display());
    if let Some(f) = file {
        line.push_str(&format!(" \"{f}\""));
    }
    util::spawn_detached(&line, None, PROCESS_CREATION_FLAGS(0))
}

/// Kapanan sürecin exe'si bir an kilitli kalabilir.
fn copy_retrying(src: &Path, dst: &Path) -> Res<()> {
    for _ in 0..20 {
        if std::fs::copy(src, dst).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    std::fs::copy(src, dst)?;
    Ok(())
}

/// "Uygulamalar ve özellikler"den ya da `cheshire --kaldir` ile. Onay alınırsa her şeyi siler.
pub fn uninstall() -> Res<bool> {
    let answer = unsafe {
        MessageBoxW(
            None,
            w!("cheshire kaldırılsın mı?\n\nAyarların ve duvarlar klasöründeki dosyalar da silinir."),
            w!("cheshire"),
            MB_YESNO | MB_ICONQUESTION,
        )
    };
    if answer != IDYES {
        return Ok(false);
    }
    close_running();
    shell::unregister();
    if let Some(old) = legacy_dir() {
        let _ = std::fs::remove_dir_all(old);
    }
    // Bu exe klasörün içinden çalışıyor olabilir: klasörü biz çıktıktan sonra cmd siler.
    let dir = util::app_dir();
    let line = format!("cmd.exe /c ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display());
    util::spawn_detached(&line, Some(&std::env::temp_dir()), CREATE_NO_WINDOW)?;
    Ok(true)
}

/// Çalışan bir kopya varsa düzgünce kapatır (duvar kâğıdını geri koysun diye); 5 sn'de kapanmazsa sonlandırır.
pub fn close_running() {
    unsafe {
        let Ok(hwnd) = FindWindowW(tray::CLASS, PCWSTR::null()) else { return };
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) else { return };
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        if WaitForSingleObject(process, 5000) != WAIT_OBJECT_0 {
            log!("çalışan kopya kapanmadı, sonlandırılıyor");
            let _ = TerminateProcess(process, 1);
            WaitForSingleObject(process, 2000);
        }
        let _ = CloseHandle(process);
    }
}

/// Güncellemeden sonra: eski sürecin çıkmasını bekler (tek örnek kilidini bıraksın).
pub fn wait_for(pid: u32) {
    unsafe {
        if let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(process, 10_000);
            let _ = CloseHandle(process);
        }
    }
}

/// Eski sürümlerin veri klasörü.
fn legacy_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("cheshire"))
}

/// Eski sürümler veriyi %APPDATA%\cheshire'da tutuyordu: ayarları ve duvar kâğıtlarını
/// kurulum klasörüne taşır, eski klasörü siler.
pub fn migrate() {
    let Some(old) = legacy_dir().filter(|d| d.exists()) else { return };
    let new = util::app_dir();
    let ini = new.join("cheshire.ini");
    if !ini.exists() {
        let _ = std::fs::copy(old.join("ayarlar.txt"), &ini);
    }
    for entry in std::fs::read_dir(old.join("duvarlar")).into_iter().flatten().flatten() {
        let dst = util::wallpapers_dir().join(entry.file_name());
        if !dst.exists() {
            let _ = std::fs::create_dir_all(util::wallpapers_dir());
            let _ = std::fs::copy(entry.path(), dst);
        }
    }
    match std::fs::remove_dir_all(&old) {
        Ok(()) => log!("eski veri taşındı: {}", old.display()),
        Err(e) => log!("eski veri klasörü silinemedi ({}): {e}", old.display()),
    }
}
