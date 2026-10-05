// Mikrofon efekti DLL'inin yolunu verir: motor onu içinde taşır, kurulumda Program Files'a yazar.
// DLL önce derlenmeli (`cargo build -p lyrebird-apo`; hive'da `make build` bunu yapar).
use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // OUT_DIR = target/<hedef>/<profil>/build/lyrebird-*/out
    let dll = out.ancestors().nth(3).unwrap().join("lyrebird_apo.dll");
    println!("cargo:rerun-if-changed={}", dll.display());
    assert!(dll.exists(), "{} yok: önce `cargo build -p lyrebird-apo` (ya da hive'da `make build`)", dll.display());
    println!("cargo:rustc-env=LYREBIRD_APO_DLL={}", dll.display());
}
