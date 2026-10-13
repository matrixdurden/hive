//! Araç listesi ve araçları açıp kapatma. Her araç `araclar/<id>/` altında kendi crate'i ve kendini
//! `src/tool.rs`'deki `TOOL` ile tanıtır (`ortak::tool::Tool`); merkez yalnızca bu listeyi bilir.
//! Yeni araç: ARAC-EKLEME.md. Kurulum işleri arka plan iş parçacığında çalışır (yönetici izni,
//! indirme, motoru durdurma beklemesi arayüzü kilitlemesin).

pub use ortak::tool::Tool;

use ortak::util;

use crate::shell;

/// Araçlar, kenar çubuğundaki ve Araçlar sayfasındaki sırayla.
pub const TOOLS: &[&Tool] = &[
    &soundboard::TOOL,
    &wallpaper::TOOL,
    &tunnel::TOOL,
    &battery::TOOL,
    &audio::TOOL,
    &dock::TOOL,
    &clicker::TOOL,
    &engram::TOOL,
];
pub const COUNT: usize = TOOLS.len();

/// Kimliği (ya da eski kimliği) bu olan araç.
pub fn find(id: &str) -> Option<usize> {
    TOOLS.iter().position(|t| t.id.eq_ignore_ascii_case(id) || t.old_ids.iter().any(|o| o.eq_ignore_ascii_case(id)))
}

/// winloom süreci başlarken: her aracın `init`'i.
pub fn init() {
    for t in TOOLS {
        (t.init)();
    }
}

/// Pencere açmadan çalışan araç komutları: biri karşılarsa çıkış kodu.
pub fn cli(args: &[String]) -> Option<i32> {
    TOOLS.iter().find_map(|t| (t.cli)(args))
}

/// Açılışta eski kurulumların izleri: araçlar kendi eski klasörlerini taşır, artık olmayan
/// araçların kısayolları ve simgeleri silinir.
pub fn migrate() {
    for t in TOOLS {
        (t.migrate)();
        for old in t.old_ids {
            let _ = std::fs::remove_file(util::data_dir().join("ikonlar").join(format!("{old}.ico")));
        }
    }
    for name in ["Music widget", "Müzik widget'ı"] {
        shell::remove_link(name);
    }
}

/// Aracın motorunu ve sayfasını başlatır (araç açıkken winloom'ın açılışında ya da açılınca).
pub fn start(i: usize, hwnd: windows::Win32::Foundation::HWND) -> Box<dyn ortak::ui::ToolPage> {
    (TOOLS[i].start)(hwnd)
}

/// Araç bu bilgisayarda kurulu mu.
pub fn installed(i: usize) -> bool {
    (TOOLS[i].installed)()
}

/// Kurulum ya da kaldırma sırasında düğmede görünen yazı.
pub fn busy_label(i: usize, install: bool) -> &'static str {
    match (TOOLS[i].downloads, install) {
        (true, true) => t!("Downloading…", "İndiriliyor…"),
        (_, true) => t!("Turning on…", "Açılıyor…"),
        _ => t!("Turning off…", "Kapatılıyor…"),
    }
}

/// Arka planda çalışır. Kaldırma, aracın bıraktığı her izi silip denetlemeden başarılı sayılmaz.
pub fn run(i: usize, install: bool) -> Result<(), String> {
    if install {
        return (TOOLS[i].install)();
    }
    (TOOLS[i].uninstall)()?;
    shell::remove_tool(i);
    let left = leftovers(i);
    if left.is_empty() {
        Ok(())
    } else {
        log!("{} kalıntıları: {left:?}", TOOLS[i].name());
        Err(format!("{} {}", t!("not fully removed, left behind:", "tam kaldırılamadı, kalanlar:"), left.join(" · ")))
    }
}

/// Aracın bu bilgisayarda bıraktığı her iz (merkezin yazdığı kısayollar dahil); kurulu değilken
/// boş olmalı. COM başlatılmış iş parçacığında çağrılmalı.
pub fn leftovers(i: usize) -> Vec<String> {
    let mut left = (TOOLS[i].leftovers)();
    left.extend(shell::tool_leftovers(i));
    left
}
