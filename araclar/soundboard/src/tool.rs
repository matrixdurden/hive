//! Blast'ın (soundboard) winloom'a tanımı. Açmak mikrofon efektini bağlamaktır (yönetici).

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteKeyValueW};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId};
use windows::core::{PCWSTR, w};

use ortak::tool::{self, Tool};
use ortak::{log, util};

use crate::{ARG_INSTALL, ARG_UNINSTALL, Soundboard};

pub static TOOL: Tool = Tool {
    id: "soundboard",
    old_ids: &["lyrebird"],
    names: ["Blast", "Blast"],
    tagline: ["A soundboard that plays straight into your microphone", "Sesi doğrudan mikrofona veren soundboard"],
    note: ["Adds an effect to your microphone · needs admin", "Mikrofonuna bir ses efekti takar · yönetici izni ister"],
    off_note: [
        "Your microphones go back to their previous settings and the audio service restarts for a second. Your sound list is deleted too.",
        "Mikrofonların eski ayarlarına döner, ses hizmeti bir saniyeliğine yeniden başlar. Ses listen de silinir.",
    ],
    icons: ortak::icons!("soundboard"),
    installed: crate::installed,
    install: || setup(true),
    uninstall,
    leftovers,
    start: |hwnd| {
        retire_standalone_lyrebird();
        Box::new(Soundboard::start(hwnd))
    },
    // Motor da winloom'ın log dosyasına yazsın.
    init: || soundboard_engine::log::set_sink(log::write),
    cli,
    ..Tool::BASE
};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn uninstall() -> Result<(), String> {
    setup(false)?;
    // Kullanıcı tarafı: ses listesi, eski tek başına sürüm, onun Windows ile başlaması.
    let _ = std::fs::remove_dir_all(crate::data_dir());
    let _ = std::fs::remove_dir_all(util::local_programs().join("lyrebird"));
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Run"), w!("lyrebird"));
    }
    Ok(())
}

fn leftovers() -> Vec<String> {
    let exists = |p: std::path::PathBuf| p.exists().then(|| p.display().to_string());
    let mut l = crate::install::leftovers();
    l.extend([crate::data_dir(), util::local_programs().join("lyrebird")].into_iter().filter_map(exists));
    if util::reg_value_exists(HKEY_CURRENT_USER, RUN, "lyrebird") {
        l.push(format!(r"HKCU\{RUN}\lyrebird"));
    }
    l
}

/// Mikrofon efektini yükseltilmiş bir kopyayla (`--soundboard-kur` / `--soundboard-kaldir`) bağlar ya da çözer.
fn setup(install: bool) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let arg = if install { ARG_INSTALL } else { ARG_UNINSTALL };
    match crate::install::run_elevated(arg) {
        Ok(true) => Ok(()),
        Ok(false) => Err(t!("the microphone effect could not be set up · see the log", "mikrofon efekti ayarlanamadı · ayrıntılar logda").into()),
        Err(_) => Err(t!("administrator permission was not given", "yönetici izni verilmedi").into()),
    }
}

fn cli(args: &[String]) -> Option<i32> {
    match args.first()?.as_str() {
        a @ (ARG_INSTALL | ARG_UNINSTALL) => {
            log::init("kurulum.log");
            Some(crate::setup(a == ARG_INSTALL))
        }
        "--soundboard-test" | "--soundboard-dene" => {
            tool::console(false);
            Some(match crate::probe::run() {
                Ok(true) => 0,
                Ok(false) => 1,
                Err(e) => {
                    println!("{} {e}", t!("ERROR", "HATA"));
                    2
                }
            })
        }
        _ => None,
    }
}

/// soundboard artık winloom içinde çalışıyor: ayrı kopya açıksa kapatılır (ortak belleğe aynı anda
/// iki yazar olamaz), Windows ile başlaması kaldırılır.
fn retire_standalone_lyrebird() {
    unsafe {
        if let Ok(hwnd) = FindWindowW(w!("Lyrebird"), PCWSTR::null()) {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                let _ = TerminateProcess(h, 0);
                let _ = CloseHandle(h);
                log!("ayrı soundboard kapatıldı (pid {pid})");
            }
        }
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Run"), w!("lyrebird"));
    }
}
