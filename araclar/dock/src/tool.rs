//! Dock'un (müzik şeridi dahil) winloom'a tanımı.

use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};

use ortak::tool::{self, Tool};

pub static TOOL: Tool = Tool {
    id: "dock",
    names: ["Dock", "Dock"],
    tagline: ["A dock in place of the taskbar", "Görev çubuğunun yerine bir dock"],
    note: [
        "Replaces the taskbar · turn it off and the taskbar is back",
        "Görev çubuğunun yerine geçer · kapatınca görev çubuğu geri gelir",
    ],
    off_note: [
        "The dock closes; the Windows taskbar and your desktop icons come back as they were.",
        "Dock kapanır; Windows görev çubuğu ve masaüstü simgelerin eski haline döner.",
    ],
    glyph: "\u{E90E}",
    icons: ortak::icons!("dock"),
    installed: crate::installed,
    install: crate::install,
    uninstall: crate::uninstall,
    leftovers: crate::leftovers,
    start: |hwnd| Box::new(crate::Dock::start(hwnd)),
    cli,
    ..Tool::BASE
};

fn cli(args: &[String]) -> Option<i32> {
    let arg = |i: usize| args.get(i).map(String::as_str);
    let dpi = || unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    };
    match args.first()?.as_str() {
        "--dock-test" | "--dock-dene" => {
            tool::console(true);
            print!("{}", crate::probe());
        }
        "--stack-test" => {
            tool::console(true);
            dpi();
            print!("{}", crate::stack::test(arg(1)));
        }
        "--jump-test" => {
            tool::console(true);
            print!("{}", crate::jump::probe(arg(1).unwrap_or(""), arg(2)));
        }
        "--dock-bench" => {
            tool::console(true);
            dpi();
            print!("{}", crate::bench());
        }
        // Sağ tık menüsündeki "Dock'a sabitle": konsol yok.
        "--dock-pin" => {
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            }
            crate::pin_from_cli(arg(1).unwrap_or(""));
        }
        _ => return None,
    }
    Some(0)
}
