//! engram'ın winloom'a tanımı.

use ortak::tool::Tool;

pub static TOOL: Tool = Tool {
    id: "engram",
    names: ["engram", "engram"],
    tagline: [
        "A voice second brain: takes notes as you talk, answers from them",
        "Sesli ikinci beyin: konuştukça not tutar, notlarından cevap verir",
    ],
    note: [
        "Runs as its own process · needs an AI API key",
        "Kendi sürecinde çalışır · bir yapay zekâ API anahtarı ister",
    ],
    off_note: [
        "engram closes; its settings, keys and voice cache are deleted. Your notes stay in their folder.",
        "engram kapanır; ayarları, anahtarları ve ses önbelleği silinir. Notların kendi klasöründe kalır.",
    ],
    icons: ortak::icons!("engram"),
    installed: crate::installed,
    install: crate::install,
    uninstall: crate::uninstall,
    leftovers: crate::leftovers,
    start: |hwnd| Box::new(crate::Engram::start(hwnd)),
    ..Tool::BASE
};
