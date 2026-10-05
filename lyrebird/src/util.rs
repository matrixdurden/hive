pub type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// UTF-8 → null ile biten UTF-16 (Win32 W fonksiyonları için).
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
