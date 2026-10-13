// engram motorunun (motor/, engram.exe) yolunu verir: sayfa onu içinde taşır, araç açılınca
// diske yazar. Motor önce derlenmeli (`make build`).
use std::path::Path;

fn main() {
    let target = std::env::var("TARGET").unwrap();
    let engine = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../target/{target}/release/engram.exe"));
    println!("cargo:rerun-if-changed={}", engine.display());
    assert!(engine.exists(), "{} yok: önce engram motoru derlenmeli (`make build`)", engine.display());
    println!("cargo:rustc-env=ENGRAM_EXE={}", engine.display());
}
