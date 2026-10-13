//! Ses aygıtlarının winloom'a tanımı.

use ortak::tool::{self, Tool};

pub static TOOL: Tool = Tool {
    id: "audio",
    old_ids: &["tweedle"],
    names: ["Audio devices", "Ses aygıtları"],
    tagline: ["Audio outputs and inputs, one key away", "Ses çıkışları ve girişleri, tek tuş uzağında"],
    note: ["No internet needed", "İnternet gerekmez"],
    off_note: [
        "Its shortcuts are released, and music keeps playing from the speakers again when your headphones drop.",
        "Kısayolları bırakılır; kulaklık kopunca müzik yine hoparlörden çalmaya devam eder.",
    ],
    icons: ortak::icons!("audio"),
    installed: crate::installed,
    install: crate::install,
    uninstall: crate::uninstall,
    leftovers: crate::leftovers,
    start: |hwnd| Box::new(crate::Audio::start(hwnd)),
    cli,
    migrate: || tool::rename_data_dir("tweedle", "audio"),
    ..Tool::BASE
};

fn cli(args: &[String]) -> Option<i32> {
    match args.first()?.as_str() {
        "--audio-test" | "--audio-dene" => {
            tool::console(false);
            print!("{}", crate::probe());
            Some(0)
        }
        _ => None,
    }
}
