//! Duvar kâğıdının winloom'a tanımı.

use ortak::tool::Tool;

pub static TOOL: Tool = Tool {
    id: "wallpaper",
    names: ["Wallpaper", "Duvar kâğıdı"],
    tagline: ["Live wallpapers drawn by your GPU", "GPU ile çizilen canlı duvar kâğıdı"],
    note: ["No internet needed", "İnternet gerekmez"],
    off_note: [
        "Your wallpaper goes back to the Windows one. Wallpapers you added and their settings are deleted too.",
        "Duvar kâğıdı Windows'unkine döner. Eklediğin duvar kâğıtları ve ayarları da silinir.",
    ],
    glyph: "\u{E771}",
    icons: ortak::icons!("wallpaper"),
    installed: crate::installed,
    install: crate::install,
    uninstall: crate::uninstall,
    leftovers: crate::leftovers,
    start: |hwnd| Box::new(crate::Wallpaper::start(hwnd)),
    ..Tool::BASE
};
