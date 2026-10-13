// ikon/icon.ico'yu exe'ye gömer (kaynak 1: pencere, görev çubuğu, tepsi).
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=ikon/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc = out.join("icon.rc");
    std::fs::write(&rc, format!("1 ICON \"{}\"\n", manifest.join("ikon/icon.ico").display())).unwrap();
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
