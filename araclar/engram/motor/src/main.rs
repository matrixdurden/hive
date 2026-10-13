// Sürüm derlemesinde konsol penceresi açılmaz.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// t! makrosu bütün modüllerde görünsün diye en önce.
#[macro_use]
mod i18n;

mod agent;
mod app;
mod audio;
mod brain;
mod claude;
mod computer;
mod config;
mod edit;
mod eleven;
mod frame;
mod gemini;
mod gfx;
mod groq;
mod hotkey;
mod log;
mod media;
mod net;
mod notes;
mod osd;
mod overlay;
mod pages;
mod reminders;
mod shell;
mod spotify;
mod system;
mod tray;
mod ui;
mod usage;
mod util;
mod voice;
mod wake;
mod xtts;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::core::w;

// kullanım (Türkçe adlar da geçer: --sekme, --gizli, --kur, --kaldir):
//   engram                    pencereyi aç (son sekmede)
//   engram --tab <ad>         o sekmede aç: notes, chat, settings
//   engram --hidden           tepside başlat
//   engram --hub              winloom'ın aracı olarak (ayarlar winloom\engram'da, tepsi ikonu yok)
//   engram --adopt            tek başına kurulumu winloom'a taşı (winloom engram'ı açarken çağırır)
//   engram --install          kendini %LOCALAPPDATA%\Programs\engram'a kur ve başlat
//   engram --uninstall        engram'ı iz bırakmadan kaldır (notlara dokunmaz)
//   engram --chat-test "..."  pencere açmadan bir mesaj gönder, cevabı ve araç çağrılarını yazdır
//   engram --vault-test       not klasörünü tarayıp indeksi yazdır
//   engram --media-test       açık medya oturumlarını yazdır (Spotify, tarayıcı...)

/// Bayrak İngilizce ya da Türkçe adıyla verilmiş mi.
fn flag(args: &[String], en: &str, tr: &str) -> bool {
    args.iter().any(|a| a == en || a == tr)
}

fn console() {
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Ayarlar okunmadan önce: veri klasörü buna göre seçilir. Komutlar ilk argümana bakar,
    // `--hub` aradan çıkarılır.
    util::set_hub(flag(&args, "--hub", "--hub"));
    args.retain(|a| a != "--hub");
    let mut cfg = config::Config::load();
    // Ortam değişkeni ayar dosyasının önüne geçmez, boşluğunu doldurur (komut satırı denemeleri).
    if cfg.anthropic_key.is_empty()
        && let Ok(k) = std::env::var("ANTHROPIC_API_KEY")
    {
        cfg.anthropic_key = k;
    }
    if cfg.elevenlabs_key.is_empty()
        && let Ok(k) = std::env::var("ELEVENLABS_API_KEY")
    {
        cfg.elevenlabs_key = k;
    }
    if cfg.groq_key.is_empty()
        && let Ok(k) = std::env::var("GROQ_API_KEY")
    {
        cfg.groq_key = k;
    }
    i18n::set_turkish(cfg.turkish);
    match args.first().map(String::as_str) {
        Some("--install" | "--kur") => {
            log::init("kurulum.log");
            if let Err(e) = shell::install_self(flag(&args, "--hidden", "--gizli")) {
                log!("kurulamadı: {e}");
                std::process::exit(1);
            }
            std::process::exit(0);
        }
        Some("--adopt" | "--sahiplen") => {
            // winloom engram'ı açarken: tek başına kurulumu kapatıp izlerini siler, ayarları
            // winloom'ın klasörüne taşır.
            log::init("kurulum.log");
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            }
            shell::adopt();
            std::process::exit(0);
        }
        Some("--vault-test" | "--notlar-dene") => {
            console();
            print!("{}", notes::probe(&cfg.vault));
            std::process::exit(0);
        }
        Some("--media-test" | "--muzik-dene") => {
            console();
            match media::status() {
                Ok(s) => println!("{s}"),
                Err(e) => println!("hata: {e}"),
            }
            std::process::exit(0);
        }
        Some("--chat-test" | "--sohbet-dene") => {
            console();
            let text = args.get(1).cloned().unwrap_or_else(|| "Merhaba, neler biliyorsun?".into());
            std::process::exit(agent::probe(&cfg, &text));
        }
        _ => {}
    }
    // İndirilip çift tıklanan exe de kendini kurar (WSL'deki geliştirme kopyası hariç).
    if util::installed_copy().is_some_and(|exe| !exe.eq_ignore_ascii_case(&util::data_dir().join("engram.exe").display().to_string())) {
        log::init("kurulum.log");
        if let Err(e) = shell::install_self(false) {
            util::error_box(&format!("{}\n\n{e}", t!("engram could not be installed:", "engram kurulamadı:")));
        }
        return;
    }
    let hidden = flag(&args, "--hidden", "--gizli");
    let tab = if flag(&args, "--uninstall", "--kaldir") {
        Some("kaldir".to_string())
    } else {
        args.iter().position(|a| a == "--tab" || a == "--sekme").and_then(|i| args.get(i + 1)).cloned()
    };
    // Tek kopya: zaten çalışıyorsa sekmeyi ona ilet.
    let _mutex = unsafe { CreateMutexW(None, true, w!("Local\\engram-tek-kopya")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        app::forward(tab.as_deref());
        return;
    }
    log::init("engram.log");
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if let Err(e) = app::run(cfg, hidden, tab) {
        log!("hata: {e}");
        util::error_box(&format!("{}\n\n{e}", t!("engram could not start:", "engram başlatılamadı:")));
    }
    // Direct2D'nin tek iş parçacıklı fabrikası süreç çıkışında ~35 sn bekletiyor (tek kopya kilidi
    // de o süre boyunca tutuluyor): ayarlar kaydedildi, süreç doğrudan sonlandırılır.
    xtts::shutdown();
    log!("kapandı");
    unsafe {
        use windows::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
        let _ = TerminateProcess(GetCurrentProcess(), 0);
    }
}
