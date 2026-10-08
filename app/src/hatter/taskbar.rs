//! Windows'un görev çubuğunu gizler, bırakırken eski haline getirir.
//!
//! Görev çubuğu iki adımda kaybolur: otomatik gizlemeye alınır (pencereler ekranın altına
//! kadar uzansın) ve penceresi saklanır (fare alta değince çıkmasın). Gizleme kalıcı bir ayar
//! olduğu için ilk değiştirmeden önceki hali `geri.ini`'ye yazılır: hive çökse ya da bilgisayar
//! kapansa da bir sonraki açılışta ve kaldırırken doğru hale dönülür. Pencereyi saklamak kalıcı
//! değildir; Explorer yeniden başlarsa çubuk geri gelir, dock onu yeniden saklar.
//! Masaüstü simgelerine dokunulmaz (masaüstünün sağ tık menüsünden açılıp kapanır); eski
//! sürümlerin `geri.ini`'ye yazdığı `simgeler_gizli` satırı yok sayılır.

use std::path::PathBuf;

use windows::Win32::Foundation::*;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW};
use windows::Win32::UI::Shell::{ABM_GETSTATE, ABM_SETSTATE, ABS_AUTOHIDE, APPBARDATA, SHAppBarMessage};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use super::apps::class_name;

fn state_file() -> PathBuf {
    super::dir().join("geri.ini")
}

/// Ana ve ikincil ekranlardaki görev çubukları.
fn trays() -> Vec<HWND> {
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> windows::core::BOOL {
        let list = unsafe { &mut *(lp.0 as *mut Vec<HWND>) };
        if is_tray(hwnd) {
            list.push(hwnd);
        }
        true.into()
    }
    let mut v = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

pub fn is_tray(hwnd: HWND) -> bool {
    matches!(class_name(hwnd).as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

fn appbar(state: Option<bool>) -> bool {
    let mut d = APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, ..Default::default() };
    unsafe {
        d.hWnd = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).unwrap_or_default();
        match state {
            Some(on) => {
                d.lParam = LPARAM(if on { ABS_AUTOHIDE as isize } else { 0 });
                SHAppBarMessage(ABM_SETSTATE, &mut d);
                on
            }
            None => SHAppBarMessage(ABM_GETSTATE, &mut d) as u32 & ABS_AUTOHIDE != 0,
        }
    }
}

const ADVANCED: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced");

fn dword(name: PCWSTR) -> Option<u32> {
    let mut v = 0u32;
    let mut len = 4u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, ADVANCED, name, RRF_RT_REG_DWORD, None, Some(&mut v as *mut _ as _), Some(&mut len)) }
        .is_ok()
        .then_some(v)
}

/// Başlat menüsünün hizası (TaskbarAl: 0 sol, 1 orta); `None` değeri siler (Windows varsayılanı).
fn set_align(v: Option<u32>) {
    unsafe {
        match v {
            Some(v) => {
                let _ = RegSetKeyValueW(HKEY_CURRENT_USER, ADVANCED, w!("TaskbarAl"), REG_DWORD.0, Some(&v as *const _ as _), 4);
            }
            None => {
                let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, ADVANCED, w!("TaskbarAl"));
            }
        }
        // Explorer ayarı yeniden okusun.
        let s = crate::util::wide("TraySettings");
        let _ = SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, WPARAM(0), LPARAM(s.as_ptr() as isize), SMTO_ABORTIFHUNG, 500, None);
    }
}

/// Değiştirmeden önceki hal yazılır: her ayar, dosyada yoksa (henüz değiştirilmeden) bir kez.
/// Başlat hizası hive kapanınca değil, yalnızca kaldırınca geri alındığından o satır dosyada
/// kalabilir; diğerleri ondan bağımsız eklenir.
fn save_state() {
    let text = std::fs::read_to_string(state_file()).unwrap_or_default();
    let has = |k: &str| text.lines().any(|l| l.starts_with(&format!("{k}=")));
    let mut add = String::new();
    if !has("otomatik_gizle") {
        add += &format!("otomatik_gizle={}\n", appbar(None) as u8);
    }
    if !has("hizalama") {
        add += &format!("hizalama={}\n", dword(w!("TaskbarAl")).map_or("yok".to_string(), |v| v.to_string()));
    }
    if add.is_empty() {
        return;
    }
    let _ = std::fs::create_dir_all(super::dir());
    if let Err(e) = std::fs::write(state_file(), text + &add) {
        crate::log!("hatter: {} yazılamadı: {e}", state_file().display());
    }
}

/// Explorer'ı düzgünce kapatıp yeniden açar: Başlat hizasını yalnızca açılışta okur. Görev
/// çubuğunun "Explorer'dan çık" komutu kullanılır, ardından explorer.exe başlatılır. `wait`:
/// bitene kadar bekle (kaldırırken; hive hemen ardından kapanır, Explorer kapalı kalmasın).
fn restart_explorer(wait: bool) {
    let run = || unsafe {
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
        let Ok(tray) = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) else { return };
        let mut pid = 0u32;
        GetWindowThreadProcessId(tray, Some(&mut pid));
        let process = OpenProcess(PROCESS_SYNCHRONIZE, false, pid).ok();
        crate::log!("hatter: Başlat hizası için Explorer yeniden başlatılıyor");
        let _ = PostMessageW(Some(tray), WM_USER + 436, WPARAM(0), LPARAM(0));
        if let Some(p) = process {
            WaitForSingleObject(p, 8000);
            let _ = CloseHandle(p);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        // Kendiliğinden yeniden başladıysa (AutoRestartShell) ikinci kez açılmaz.
        if FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).is_err() {
            windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), w!("explorer.exe"), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        }
    };
    if wait {
        run();
    } else {
        std::thread::spawn(run);
    }
}

/// Görev çubuğunu gizler. Explorer yeniden başlayınca da çağrılır.
pub fn hide() {
    save_state();
    if !appbar(None) {
        appbar(Some(true));
    }
    for t in trays() {
        unsafe {
            let _ = ShowWindow(t, SW_HIDE);
        }
    }
    // Başlat menüsü dock'un üstünden, ortadan açılsın. Explorer hizayı yalnızca açılışta
    // okuduğundan değişince bir kez yeniden başlatılır (kurulumda).
    if dword(w!("TaskbarAl")) != Some(1) {
        set_align(Some(1));
        restart_explorer(false);
    }
}

/// Görünür kalan görev çubuğu varsa saklar: Explorer otomatik gizlemeye geçerken ve bazı
/// ayar değişikliklerinde çubuğu kendisi yeniden gösterir.
pub fn ensure_hidden() {
    if super::tray::is_open() {
        return;
    }
    for t in trays() {
        unsafe {
            if IsWindowVisible(t).as_bool() {
                let _ = ShowWindow(t, SW_HIDE);
            }
        }
    }
}

/// Görev çubuğu yeniden göründüyse (Explorer bazen kendisi gösterir) saklar.
pub fn rehide(hwnd: HWND) {
    if is_tray(hwnd) && !super::tray::is_open() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// Her şeyi `geri.ini`'deki haline döndürür; dosya yoksa yalnızca görev çubuğunu gösterir.
/// Başlat hizası yalnızca `full` ise (kaldırırken) geri alınır: hive her kapanıp açıldığında
/// Explorer yeniden başlamasın.
pub fn restore(full: bool) {
    for t in trays() {
        unsafe {
            // Gizli simgeler açıkken kapanmışsa çubuğun bölgesi boş kalmasın.
            windows::Win32::Graphics::Gdi::SetWindowRgn(t, None, false);
            let _ = ShowWindow(t, SW_SHOWNA);
        }
    }
    let Ok(text) = std::fs::read_to_string(state_file()) else { return };
    let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')).map(|v| v.trim() == "1");
    if let Some(on) = get("otomatik_gizle") {
        appbar(Some(on));
    }
    let align = text.lines().find(|l| l.starts_with("hizalama=")).map(str::to_string);
    if full {
        if let Some(a) = align.as_deref().and_then(|l| l.strip_prefix("hizalama=")) {
            let old = a.trim().parse::<u32>().ok();
            if dword(w!("TaskbarAl")) != Some(old.unwrap_or(1)) {
                set_align(old);
                restart_explorer(true);
            }
        }
        let _ = std::fs::remove_file(state_file());
    } else {
        match align {
            Some(a) => {
                let _ = std::fs::write(state_file(), a + "\n");
            }
            None => {
                let _ = std::fs::remove_file(state_file());
            }
        }
    }
}

