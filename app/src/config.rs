//! `ayarlar.ini`: satır başına `anahtar=değer`. Bilinmeyen satırlar yok sayılır.

use crate::util::data_dir;

pub struct Config {
    /// Kenar çubuğu yalnızca ikon mu (pencere genişken de).
    pub narrow: bool,
    /// Son açık sekme: araç kimliği, "araclar" ya da "ayarlar".
    pub tab: String,
    /// Arayüz Türkçe mi; ayarda yoksa Windows'un dilinden.
    pub turkish: bool,
    /// Arayüz teması.
    pub theme: crate::ui::Theme,
    /// Yeni sürümler kendiliğinden indirilip kurulsun.
    pub auto_update: bool,
}

impl Config {
    pub fn load() -> Self {
        let mut c = Config { narrow: false, tab: String::new(), turkish: crate::i18n::system_turkish(), theme: crate::ui::Theme::Claude, auto_update: true };
        let text = std::fs::read_to_string(data_dir().join("ayarlar.ini")).unwrap_or_default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k.trim() {
                "dar" => c.narrow = v.trim() == "1",
                "sekme" => c.tab = v.trim().to_string(),
                "dil" => c.turkish = v.trim() == "tr",
                "tema" => c.theme = crate::ui::Theme::from_id(v.trim()).unwrap_or(c.theme),
                "guncelle" => c.auto_update = v.trim() != "0",
                _ => {}
            }
        }
        c
    }

    pub fn save(&self) {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        let lang = if self.turkish { "tr" } else { "en" };
        let text = format!(
            "dar={}\nsekme={}\ndil={lang}\ntema={}\nguncelle={}\n",
            self.narrow as u8,
            self.tab,
            self.theme.id(),
            self.auto_update as u8
        );
        let _ = std::fs::write(dir.join("ayarlar.ini"), text);
    }
}
