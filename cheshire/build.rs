// Windows hedefinde assets/icon.ico'yu exe'ye gömer (kaynak 1). Explorer, görev çubuğu,
// `.cheshire` dosyalarının DefaultIcon'u ve tepsi ikonu bunu kullanır.
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc = out.join("icon.rc");
    let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon.ico");
    std::fs::write(&rc, format!("1 ICON \"{}\"\n", icon.display())).unwrap();
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
