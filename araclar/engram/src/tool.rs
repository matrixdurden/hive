//! engram'ın winloom'a tanımı.

use ortak::tool::Tool;

pub static TOOL: Tool = Tool {
    id: "engram",
    names: ["engram", "engram"],
    tagline: [
        "A voice second brain: takes notes as you talk, answers from them",
        "Sesli ikinci beyin: konuştukça not tutar, notlarından cevap verir",
    ],
    note: ["Needs an AI API key", "Bir yapay zekâ API anahtarı ister"],
    off_note: [
        "engram stops listening; its settings, keys and voice cache are deleted. Your notes stay in their folder.",
        "engram dinlemeyi bırakır; ayarları, anahtarları ve ses önbelleği silinir. Notların kendi klasöründe kalır.",
    ],
    top: true,
    glyph: "\u{EA80}",
    icons: ortak::icons!("engram"),
    installed: crate::installed,
    install: crate::install,
    uninstall: crate::uninstall,
    leftovers: crate::leftovers,
    start: |hwnd| Box::new(crate::Engram::start(hwnd)),
    cli: crate::cli,
    ..Tool::BASE
};
