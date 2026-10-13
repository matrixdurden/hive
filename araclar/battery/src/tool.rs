//! Pil'in winloom'a tanımı.

use ortak::tool::{self, Tool};

pub static TOOL: Tool = Tool {
    id: "battery",
    names: ["Battery", "Pil"],
    tagline: ["Three gears for your laptop's battery", "Dizüstünün pili için üç vites"],
    note: ["Laptops only", "Yalnızca dizüstü"],
    off_note: [
        "Screen, power and lid settings go back to what they were; the GPU preferences it wrote and its measurements are deleted.",
        "Ekran, güç ve kapak ayarları eski haline döner; yazdığı GPU tercihleri ve ölçümleri silinir.",
    ],
    glyph: "\u{E83F}",
    icons: ortak::icons!("battery"),
    installed: crate::installed,
    install: crate::install,
    uninstall: crate::uninstall,
    leftovers: crate::leftovers,
    start: |hwnd| Box::new(crate::Battery::start(hwnd)),
    cli,
    ..Tool::BASE
};

fn cli(args: &[String]) -> Option<i32> {
    match args.first()?.as_str() {
        "--battery-test" | "--battery-dene" => {
            tool::console(true);
            print!("{}", crate::probe());
            Some(0)
        }
        _ => None,
    }
}
