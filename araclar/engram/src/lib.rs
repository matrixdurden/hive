//! engram: konuştukça not tutan, notlarından cevap veren sesli ikinci beyin. winloom'ın içinde
//! çalışır; ekranları (Ana, Hafıza, Ayarlar) winloom'ın panelinde, engram sayfasında.
//!
//! - `brain` / `agent`: yapay zekâ turları (Claude, Gemini, Groq) ve araç çağrıları.
//! - `notes`: not klasörü (düz Markdown); `pages/notes.rs` Hafıza.
//! - `voice`, `wake`, `audio`, `xtts`, `eleven`: dinleme, uyandırma sözü, konuşma.
//! - `overlay`: konuşurken ekran kenarlarında ışık ve üstte durum hapı.
//! - `computer`, `system`, `media`, `spotify`: bilgisayarı kullanma araçları.
//!
//! Ayarlar ve anahtarlar `%LOCALAPPDATA%\Programs\winloom\engram`'da.

#[macro_use]
extern crate ortak;

mod tool;
pub use tool::TOOL;

mod agent;
mod audio;
mod brain;
mod claude;
mod computer;
mod config;
mod edit;
mod eleven;
mod gemini;
mod groq;
mod hotkey;
mod media;
mod net;
mod notes;
mod overlay;
mod page;
mod pages;
mod reminders;
mod shell;
mod spotify;
mod system;
mod ui;
mod usage;
mod util;
mod voice;
mod wake;
mod xtts;

// winloom'la paylaşılanlar: çizim, dil, log.
use ortak::{gfx, i18n, log};

pub use page::Engram;

pub fn installed() -> bool {
    util::data_dir().join("ayarlar.ini").is_file()
}

/// Aç: tek başına kurulu engram varsa onu winloom'a taşır (kapatır, kayıtlarını siler, ayarlarını
/// ve anahtarlarını buraya alır), yoksa boş ayarlarla başlar.
pub fn install() -> Result<(), String> {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
    }
    shell::adopt();
    if !installed() {
        config::Config::load().save();
    }
    if installed() { Ok(()) } else { Err(t!("could not write engram's settings", "engram'ın ayarları yazılamadı").into()) }
}

/// Kapat: ayarlar, anahtarlar ve ses önbelleği silinir. Notlar kullanıcının klasöründe, onlara
/// dokunulmaz.
pub fn uninstall() -> Result<(), String> {
    let dir = util::data_dir();
    let mut last = None;
    for _ in 0..20 {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => return Ok(()),
            Err(_) if !dir.exists() => return Ok(()),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    Err(format!("{} {}: {}", t!("could not delete", "silinemedi:"), dir.display(), last.map(|e| e.to_string()).unwrap_or_default()))
}

pub fn leftovers() -> Vec<String> {
    let d = util::data_dir();
    if d.exists() { vec![d.display().to_string()] } else { Vec::new() }
}

/// `--engram-chat-test "..."`, `--engram-vault-test`, `--engram-media-test`.
fn cli(args: &[String]) -> Option<i32> {
    let mut cfg = config::Config::load();
    // Ortam değişkeni ayar dosyasının önüne geçmez, boşluğunu doldurur (komut satırı denemeleri).
    for (key, var) in [
        (&mut cfg.anthropic_key, "ANTHROPIC_API_KEY"),
        (&mut cfg.elevenlabs_key, "ELEVENLABS_API_KEY"),
        (&mut cfg.groq_key, "GROQ_API_KEY"),
    ] {
        if key.is_empty()
            && let Ok(k) = std::env::var(var)
        {
            *key = k;
        }
    }
    match args.first()?.as_str() {
        "--engram-vault-test" => {
            ortak::tool::console(false);
            print!("{}", notes::probe(&cfg.vault));
            Some(0)
        }
        "--engram-media-test" => {
            ortak::tool::console(false);
            match media::status() {
                Ok(s) => println!("{s}"),
                Err(e) => println!("hata: {e}"),
            }
            Some(0)
        }
        "--engram-chat-test" => {
            ortak::tool::console(false);
            let text = args.get(1).cloned().unwrap_or_else(|| "Merhaba, neler biliyorsun?".into());
            Some(agent::probe(&cfg, &text))
        }
        _ => None,
    }
}
