//! Işıklar'ın winloom'a tanımı.

use ortak::tool::Tool;

pub static TOOL: Tool = Tool {
    id: "lights",
    names: ["Lights", "Işıklar"],
    tagline: ["Your Bluetooth LED lights: color, effects and lights that move with your music", "Bluetooth LED ışıkların: renk, efekt ve müzikle yanan ışık"],
    note: ["Needs Bluetooth · for now STARLIGHT controllers", "Bluetooth gerekir · şimdilik STARLIGHT kontrolcüleri"],
    off_note: ["The light is disconnected and its settings are deleted.", "Işıkla bağlantı kesilir, ayarları silinir."],
    glyph: "\u{E706}",
    icons: ortak::icons!("lights"),
    installed: crate::installed,
    install: crate::install,
    uninstall: crate::uninstall,
    leftovers: crate::leftovers,
    start: |hwnd| Box::new(crate::Lights::start(hwnd)),
    cli: crate::cli,
    ..Tool::BASE
};
