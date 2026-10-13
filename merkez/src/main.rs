// Sürüm derlemesinde konsol penceresi açılmaz.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// t! ve log! makroları (ortak) bütün modüllerde görünsün diye en önce.
#[macro_use]
extern crate ortak;

mod app;
mod config;
mod shell;
mod tools;
mod tray;
mod update;

use ortak::{hub, i18n, log, util};

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::core::w;

// kullanım (Türkçe adlar da geçer: --sekme, --gizli, --kur, --kaldir, --kalinti):
//   winloom                  pencereyi aç (son sekmede)
//   winloom --tab <ad>       o sekmede aç: soundboard, wallpaper, tunnel, battery, audio, dock, clicker, tools, settings
//   winloom --hidden         tepside başlat
//   winloom --install        kendini %LOCALAPPDATA%\Programs\winloom'a kur ve başlat (güncelleme de;
//                         --hidden ile pencere açmadan)
//   winloom --uninstall      winloom'ı ve kurulu araçları iz bırakmadan kaldır
//   winloom --leftovers      kurulu olmayan araçlardan kalan iz var mı, listele
//   winloom --update-test  son yayına bak, yeniyse indirip doğrula (kurmaz)
// Araçların kendi komutları kendi src/tool.rs'lerinde (`cli`):
//   winloom --soundboard-test  her mikrofona test sesi gönder, geri geliyor mu ölç
//   winloom --battery-test  Pil aracının okuduğu pil, ekran ve ekran kartı bilgilerini yazdır
//   winloom --audio-test   Ses aygıtları aracının gördüğü çıkışları ve medya oturumlarını yazdır
//   winloom --dock-test    dock'un gördüğü pencereleri ve dock'taki öğeleri yazdır
//   winloom --dock-pin <yol>  dock'a sabitle (sağ tık menüsünden)

/// Bayrak İngilizce ya da Türkçe adıyla verilmiş mi.
fn flag(args: &[String], en: &str, tr: &str) -> bool {
    args.iter().any(|a| a == en || a == tr)
}

fn main() {
    tools::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    i18n::set_turkish(config::Config::load().turkish);
    // Araçların yükseltilmiş kurulum adımları ve komut satırı araçları: pencere açmadan çıkar.
    if let Some(code) = tools::cli(&args) {
        std::process::exit(code);
    }
    match args.first().map(String::as_str) {
        Some("--install" | "--kur") => {
            log::init("kurulum.log");
            // Kurulum komutundan çağrılır: hata kutusu açıp beklemez, çıkış koduyla bildirir.
            if let Err(e) = shell::install_self(flag(&args, "--hidden", "--gizli")) {
                log!("kurulamadı: {e}");
                std::process::exit(1);
            }
            std::process::exit(0);
        }
        Some("--leftovers" | "--kalinti") => {
            // Kurulu olmayan araçlardan kalan iz var mı (hepsi boş olmalı).
            ortak::tool::console(true);
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
        Some("--update-test" | "--guncelleme-dene") => {
            unsafe {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            }
            print!("{}", update::probe());
            std::process::exit(0);
        }
        _ => {}
    }
    // İndirilip çift tıklanan exe de kendini kurar (WSL'deki geliştirme kopyası hariç).
    if util::installed_copy().is_some_and(|exe| !exe.eq_ignore_ascii_case(&util::data_dir().join("winloom.exe").display().to_string())) {
        log::init("kurulum.log");
        if let Err(e) = shell::install_self(false) {
            util::error_box(&format!("{}\n\n{e}", t!("winloom could not be installed:", "winloom kurulamadı:")));
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

    // İndirilmiş güncelleme varsa önce o kurulur (o da bu sürümü kapatıp yerine geçer).
    if !flag(&args, "--uninstall", "--kaldir") && update::install_pending_at_start(true) {
        return;
    }

    // Tek kopya: zaten çalışıyorsa sekmeyi ona ilet.
    let _mutex = unsafe { CreateMutexW(None, true, w!("Local\\winloom-tek-kopya")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        hub::forward(tab.as_deref());
        return;
    }
    log::init("winloom.log");
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if let Err(e) = app::run(hidden, tab) {
        log!("hata: {e}");
        util::error_box(&format!("{}\n\n{e}", t!("winloom could not start:", "winloom başlatılamadı:")));
    }
}
