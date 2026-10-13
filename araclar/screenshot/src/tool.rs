//! Ekran görüntüsünün winloom'a tanımı.

use ortak::tool::Tool;

pub static TOOL: Tool = Tool {
    id: "screenshot",
    names: ["Screenshot", "Ekran görüntüsü"],
    tagline: ["Select an area, let go: it's on the clipboard", "Alanı seç, bırak: panoda"],
    note: ["No internet needed", "İnternet gerekmez"],
    off_note: [
        "The shortcut is released and its settings are deleted. Screenshots you saved stay in their folder.",
        "Kısayol bırakılır, ayarları silinir. Kaydettiğin görüntüler klasöründe kalır.",
    ],
    glyph: "\u{E722}",
    icons: ortak::icons!("screenshot"),
    installed: crate::installed,
    install: crate::install,
    uninstall: crate::uninstall,
    leftovers: crate::leftovers,
    start: |hwnd| Box::new(crate::Screenshot::start(hwnd)),
    ..Tool::BASE
};
