//! Tıklayıcı'nın winloom'a tanımı.

use ortak::tool::Tool;

pub static TOOL: Tool = Tool {
    id: "clicker",
    names: ["Clicker", "Tıklayıcı"],
    tagline: ["Clicks for you while you hold a key", "Tuşu basılı tuttuğun sürece senin yerine tıklar"],
    note: ["No internet needed", "İnternet gerekmez"],
    off_note: ["Its trigger key is released and its settings are deleted.", "Tetik tuşu bırakılır, ayarları silinir."],
    glyph: "\u{E962}",
    icons: ortak::icons!("clicker"),
    installed: crate::installed,
    install: crate::install,
    uninstall: crate::uninstall,
    leftovers: crate::leftovers,
    start: |hwnd| Box::new(crate::Clicker::start(hwnd)),
    ..Tool::BASE
};
