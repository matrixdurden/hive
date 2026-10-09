// Sürüm derlemesinde konsol penceresi açılmaz.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// t! makrosu bütün modüllerde görünsün diye en önce.
#[macro_use]
mod i18n;

mod app;
mod cheshire;
mod config;
mod dormouse;
mod gfx;
mod hatter;
mod music;
mod log;
mod lyrebird;
mod myinstants;
mod net;
mod osd;
mod rabbithole;
mod shell;
mod tools;
mod tray;
mod tweedle;
mod ui;
mod util;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::core::w;

// kullanım (Türkçe adlar da geçer: --sekme, --gizli, --kur, --kaldir, --kalinti):
//   hive                  pencereyi aç (son sekmede)
//   hive --tab <ad>       o sekmede aç: lyrebird, cheshire, rabbithole, tools, settings
//   hive --hidden         tepside başlat
//   hive --install        kendini %LOCALAPPDATA%\Programs\hive'a kur ve başlat (güncelleme de)
//   hive --uninstall      hive'ı ve kurulu araçları iz bırakmadan kaldır
//   hive --leftovers      kurulu olmayan araçlardan kalan iz var mı, listele
//   hive --lyrebird-test  her mikrofona test sesi gönder, geri geliyor mu ölç
//   hive --dormouse-test  dormouse'un okuduğu pil, ekran ve ekran kartı bilgilerini yazdır
//   hive --tweedle-test   tweedle'ın gördüğü ses çıkışlarını ve medya oturumlarını yazdır
//   hive --hatter-test    hatter'ın gördüğü pencereleri ve dock'taki öğeleri yazdır
//   hive --hatter-pin <yol>  dock'a sabitle (sağ tık menüsünden)
//   hive --music-test [klasör]  medya oturumlarını yazdır; klasöre widget önizlemesi çiz

/// Bayrak İngilizce ya da Türkçe adıyla verilmiş mi.
fn flag(args: &[String], en: &str, tr: &str) -> bool {
    args.iter().any(|a| a == en || a == tr)
}

fn main() {
    // lyrebird motoru da hive'ın log dosyasına yazsın.
    lyrebird_motor::log::set_sink(|args| log::write(args));
    let args: Vec<String> = std::env::args().skip(1).collect();
    i18n::set_turkish(config::Config::load().turkish);
    // Araçların yükseltilmiş kurulum adımları ve komut satırı araçları: pencere açmadan çıkar.
    match args.first().map(String::as_str) {
        Some(lyrebird::ARG_INSTALL) | Some(lyrebird::ARG_UNINSTALL) => {
            log::init("kurulum.log");
            std::process::exit(lyrebird::setup(args[0] == lyrebird::ARG_INSTALL));
        }
        Some("--install" | "--kur") => {
            log::init("kurulum.log");
            // Kurulum komutundan çağrılır: hata kutusu açıp beklemez, çıkış koduyla bildirir.
            if let Err(e) = shell::install_self() {
                log!("kurulamadı: {e}");
                std::process::exit(1);
            }
            std::process::exit(0);
        }
        Some("--leftovers" | "--kalinti") => {
            // Kurulu olmayan araçlardan kalan iz var mı (hepsi boş olmalı).
            unsafe {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            }
            let mut clean = true;
            for (i, t) in tools::TOOLS.iter().enumerate() {
                if tools::installed(i) {
                    println!("{}: {}", t.name(), t!("installed", "kurulu"));
                    continue;
                }
                let left = tools::leftovers(i);
                if left.is_empty() {
                    println!("{}: {}", t.name(), t!("not installed, no trace", "kurulu değil, iz yok"));
                } else {
                    clean = false;
                    println!("{}: {}", t.name(), t!("not installed, left behind:", "kurulu değil, kalanlar:"));
                    for l in left {
                        println!("  {l}");
                    }
                }
            }
            std::process::exit(if clean { 0 } else { 1 });
        }
        Some("--dormouse-test" | "--dormouse-dene") => {
            unsafe {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            }
            print!("{}", dormouse::probe());
            std::process::exit(0);
        }
        Some("--tweedle-test" | "--tweedle-dene") => {
            unsafe {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            }
            print!("{}", tweedle::probe());
            std::process::exit(0);
        }
        Some("--hatter-test" | "--hatter-dene") => {
            unsafe {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            }
            print!("{}", hatter::probe());
            std::process::exit(0);
        }
        Some("--music-test" | "--muzik-dene") => {
            unsafe {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
                let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            }
            print!("{}", music::probe(args.get(1).map(String::as_str)));
            std::process::exit(0);
        }
        Some("--hatter-pin") => {
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            }
            hatter::pin_from_cli(args.get(1).map(String::as_str).unwrap_or(""));
            std::process::exit(0);
        }
        Some("--lyrebird-test" | "--lyrebird-dene") => {
            unsafe {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            }
            std::process::exit(match lyrebird::probe::run() {
                Ok(true) => 0,
                Ok(false) => 1,
                Err(e) => {
                    println!("{} {e}", t!("ERROR", "HATA"));
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
            util::error_box(&format!("{}\n\n{e}", t!("hive could not be installed:", "hive kurulamadı:")));
        }
        return;
    }
    let hidden = flag(&args, "--hidden", "--gizli");
    // Windows'un Uygulamalar listesinden kaldırma `--uninstall` ile gelir.
    let tab = if flag(&args, "--uninstall", "--kaldir") {
        Some("kaldir".to_string())
    } else {
        args.iter().position(|a| a == "--tab" || a == "--sekme").and_then(|i| args.get(i + 1)).cloned()
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
    tools::migrate();
    if let Err(e) = app::run(hidden, tab) {
        log!("hata: {e}");
        util::error_box(&format!("{}\n\n{e}", t!("hive could not start:", "hive başlatılamadı:")));
    }
}
