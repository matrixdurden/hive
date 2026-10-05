// Sürüm derlemesinde konsol penceresi açılmaz.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cheshire;
mod config;
mod gfx;
mod log;
mod lyrebird;
mod net;
mod rabbithole;
mod shell;
mod tools;
mod tray;
mod ui;
mod util;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::core::w;

// kullanım:
//   hive                  pencereyi aç (son sekmede)
//   hive --sekme <ad>     o sekmede aç: lyrebird, cheshire, rabbithole, araclar, ayarlar
//   hive --gizli          tepside başlat
//   hive --kur            kendini %LOCALAPPDATA%\Programs\hive'a kur ve başlat (güncelleme de)
//   hive --kaldir         hive'ı ve kurulu araçları iz bırakmadan kaldır
//   hive --kalinti        kurulu olmayan araçlardan kalan iz var mı, listele
//   hive --lyrebird-dene  her mikrofona test sesi gönder, geri geliyor mu ölç
fn main() {
    // lyrebird motoru da hive'ın log dosyasına yazsın.
    lyrebird_motor::log::set_sink(|args| log::write(args));
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Araçların yükseltilmiş kurulum adımları ve komut satırı araçları: pencere açmadan çıkar.
    match args.first().map(String::as_str) {
        Some(lyrebird::ARG_INSTALL) | Some(lyrebird::ARG_UNINSTALL) => {
            log::init("kurulum.log");
            std::process::exit(lyrebird::setup(args[0] == lyrebird::ARG_INSTALL));
        }
        Some("--kur") => {
            log::init("kurulum.log");
            // Kurulum komutundan çağrılır: hata kutusu açıp beklemez, çıkış koduyla bildirir.
            if let Err(e) = shell::install_self() {
                log!("kurulamadı: {e}");
                std::process::exit(1);
            }
            std::process::exit(0);
        }
        Some("--kalinti") => {
            // Kurulu olmayan araçlardan kalan iz var mı (hepsi boş olmalı).
            unsafe {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            }
            let mut clean = true;
            for (i, t) in tools::TOOLS.iter().enumerate() {
                if tools::installed(i) {
                    println!("{}: kurulu", t.name);
                    continue;
                }
                let left = tools::leftovers(i);
                if left.is_empty() {
                    println!("{}: kurulu değil, iz yok", t.name);
                } else {
                    clean = false;
                    println!("{}: kurulu değil, kalanlar:", t.name);
                    for l in left {
                        println!("  {l}");
                    }
                }
            }
            std::process::exit(if clean { 0 } else { 1 });
        }
        Some("--lyrebird-dene") => {
            unsafe {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            }
            std::process::exit(match lyrebird::probe::run() {
                Ok(true) => 0,
                Ok(false) => 1,
                Err(e) => {
                    println!("HATA {e}");
                    2
                }
            });
        }
        _ => {}
    }
    // İndirilip çift tıklanan exe de kendini kurar (WSL'deki geliştirme kopyası hariç).
    if util::installed_copy().is_some_and(|exe| !exe.eq_ignore_ascii_case(&util::data_dir().join("hive.exe").display().to_string())) {
        log::init("kurulum.log");
        if let Err(e) = shell::install_self() {
            util::error_box(&format!("hive kurulamadı:\n\n{e}"));
        }
        return;
    }
    let hidden = args.iter().any(|a| a == "--gizli");
    // Windows'un Uygulamalar listesinden kaldırma `--kaldir` ile gelir.
    let tab = if args.iter().any(|a| a == "--kaldir") {
        Some("kaldir".to_string())
    } else {
        args.iter().position(|a| a == "--sekme").and_then(|i| args.get(i + 1)).cloned()
    };

    // Tek kopya: zaten çalışıyorsa sekmeyi ona ilet.
    let _mutex = unsafe { CreateMutexW(None, true, w!("Local\\hive-tek-kopya")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        app::forward(tab.as_deref());
        return;
    }
    log::init("hive.log");
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if let Err(e) = app::run(hidden, tab) {
        log!("hata: {e}");
        util::error_box(&format!("hive başlatılamadı:\n\n{e}"));
    }
}
