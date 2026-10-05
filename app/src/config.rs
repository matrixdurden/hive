//! `ayarlar.ini`: satır başına `anahtar=değer`. Bilinmeyen satırlar yok sayılır.

use crate::util::data_dir;

pub struct Config {
    /// Kenar çubuğu yalnızca ikon mu (pencere genişken de).
    pub narrow: bool,
    /// Son açık sekme: araç kimliği ya da "ayarlar".
    pub tab: String,
}

impl Config {
    pub fn load() -> Self {
        let mut c = Config { narrow: false, tab: String::new() };
        let text = std::fs::read_to_string(data_dir().join("ayarlar.ini")).unwrap_or_default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k.trim() {
                "dar" => c.narrow = v.trim() == "1",
                "sekme" => c.tab = v.trim().to_string(),
                _ => {}
            }
        }
        c
    }

    pub fn save(&self) {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        let text = format!("dar={}\nsekme={}\n", self.narrow as u8, self.tab);
        let _ = std::fs::write(dir.join("ayarlar.ini"), text);
    }
}
