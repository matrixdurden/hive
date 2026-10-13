//! Ses efektlerinin (soundboard) winloom'a tanımı. Açmak mikrofon efektini bağlamaktır (yönetici);
//! aynı kurulum sonraki mikrofonlar izin sormadan bağlansın diye bir görev kaydeder (`task`).

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

use ortak::log;
use ortak::tool::{self, Tool};

use crate::{ARG_INSTALL, ARG_UNINSTALL, Soundboard};

pub static TOOL: Tool = Tool {
    id: "soundboard",
    names: ["Soundboard", "Ses efektleri"],
    tagline: ["A soundboard that plays straight into your microphone", "Sesi doğrudan mikrofona veren soundboard"],
    note: ["Adds an effect to your microphone · needs admin", "Mikrofonuna bir ses efekti takar · yönetici izni ister"],
    off_note: [
        "Your microphones go back to their previous settings and the audio service restarts for a second. Your sound list is deleted too.",
        "Mikrofonların eski ayarlarına döner, ses hizmeti bir saniyeliğine yeniden başlar. Ses listen de silinir.",
    ],
    glyph: "\u{E8D6}",
    icons: ortak::icons!("soundboard"),
    installed: crate::installed,
    install: || setup(true),
    uninstall,
    leftovers,
    start: |hwnd| Box::new(Soundboard::start(hwnd)),
    // Motor da winloom'ın log dosyasına yazsın.
    init: || soundboard_engine::log::set_sink(log::write),
    cli,
    ..Tool::BASE
};

fn uninstall() -> Result<(), String> {
    setup(false)?;
    // Kullanıcı tarafı: ses listesi.
    let _ = std::fs::remove_dir_all(crate::data_dir());
    Ok(())
}

fn leftovers() -> Vec<String> {
    let exists = |p: std::path::PathBuf| p.exists().then(|| p.display().to_string());
    let mut l = crate::install::leftovers();
    l.extend([crate::data_dir()].into_iter().filter_map(exists));
    if crate::task::exists() {
        l.push(format!(r"{}\{}", t!("Task Scheduler", "Görev Zamanlayıcı"), crate::task::name()));
    }
    l
}

/// Mikrofon efektini yükseltilmiş bir kopyayla (`--soundboard-kur` / `--soundboard-kaldir`) bağlar ya da çözer;
/// o kopya görevi de kaydeder ya da siler.
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
