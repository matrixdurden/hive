// Bağlayıcı DLL'e zaman damgası basmasın: kaynak değişmedikçe DLL bayt bayt aynı kalır, motor
// "kurulu efekt bu sürüm mü" sorusuna yanlışlıkla hayır demez (yoksa her derlemede yeniden bağlamak gerekir).
fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,--no-insert-timestamp");
    }
}
