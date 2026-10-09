// assets/icon.ico'yu exe'ye gömer (kaynak 1: pencere, görev çubuğu, tepsi) ve duvar kâğıdı motorunun
// yolunu verir: hive onu içinde taşır, duvar kâğıdı açılınca diske yazar. Motor önce derlenmeli (`make build`).
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let target = std::env::var("TARGET").unwrap();
    let engine = manifest_dir.join(format!("../target/{target}/release/wallpaper.exe"));
    println!("cargo:rerun-if-changed={}", engine.display());
    assert!(engine.exists(), "{} yok: önce duvar kâğıdı motoru derlenmeli (`make build`)", engine.display());
    println!("cargo:rustc-env=WALLPAPER_EXE={}", engine.display());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc = out.join("icon.rc");
    std::fs::write(&rc, format!("1 ICON \"{}\"\n", manifest.join("assets/icon.ico").display())).unwrap();
    let obj = out.join("icon.o");
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
    let ok = std::process::Command::new(&windres)
        .args(["-O", "coff", "-i"])
        .arg(&rc)
        .arg("-o")
        .arg(&obj)
        .status()
        .unwrap_or_else(|e| panic!("{windres} çalışmadı: {e}"));
    assert!(ok.success(), "{windres} başarısız");
    println!("cargo:rustc-link-arg-bins={}", obj.display());
}
