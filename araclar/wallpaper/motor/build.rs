// Windows hedefinde assets/icon.ico'yu exe'ye gömer (kaynak 1). Explorer, görev çubuğu,
// `.wallpaper` dosyalarının DefaultIcon'u ve tepsi ikonu bunu kullanır. Yanına Windows 8+ uyumluluğu
// bildiren bir manifest ekler: katmanlı çocuk pencere (24H2 yerleşimi) ancak o zaman yapılabilir.
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{e2011457-1546-43c5-a5fe-008deee3d3f0}"/>
      <supportedOS Id="{35138b9a-5d96-4fbd-8e2d-a2440225f93a}"/>
      <supportedOS Id="{4a2f28e3-53b9-4441-ba9c-d69d4a4a6e38}"/>
      <supportedOS Id="{1f676c76-80e1-4239-95bb-83d0f6d0da78}"/>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc = out.join("icon.rc");
    let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon.ico");
    let manifest = out.join("cheshire.manifest");
    std::fs::write(&manifest, MANIFEST).unwrap();
    std::fs::write(&rc, format!("1 ICON \"{}\"\n1 24 \"{}\"\n", icon.display(), manifest.display())).unwrap();
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
