// Duvar kâğıdı motorunun (motor/, wallpaper.exe) yolunu verir: sayfa onu içinde taşır, araç
// açılınca diske yazar. Motor önce derlenmeli (`make build`).
use std::path::Path;

fn main() {
    let target = std::env::var("TARGET").unwrap();
    let engine = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../target/{target}/release/wallpaper.exe"));
    println!("cargo:rerun-if-changed={}", engine.display());
    assert!(engine.exists(), "{} yok: önce duvar kâğıdı motoru derlenmeli (`make build`)", engine.display());
    println!("cargo:rustc-env=WALLPAPER_EXE={}", engine.display());
}
