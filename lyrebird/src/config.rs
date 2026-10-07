use std::path::PathBuf;

use crate::hotkey::Hotkey;
use crate::data_dir;

#[derive(Clone, Debug)]
pub struct Sound {
    pub path: PathBuf,
    pub hotkey: Option<Hotkey>,
}

/// Basit `anahtar = değer` dosyası. Bilinmeyen satırlar yok sayılır, eksikler varsayılana düşer.
#[derive(Clone, Debug)]
pub struct Config {
    /// Mikrofona giden ses düzeyi, 0..100.
    pub mic: u32,
    /// Kendi kulaklığında duyduğun düzey, 0..100.
    pub ear: u32,
    pub stop: Option<Hotkey>,
    /// Son 10 saniyeyi listeye ekleyen kısayol.
    pub clip: Option<Hotkey>,
    pub sounds: Vec<Sound>,
}

impl Default for Config {
    fn default() -> Self {
        Self { mic: 70, ear: 50, stop: None, clip: Hotkey::parse("Ctrl+Alt+L"), sounds: Vec::new() }
    }
}

fn path() -> PathBuf {
    data_dir().join("ayarlar.txt")
}

impl Config {
    pub fn load() -> Self {
        let mut cfg = Self::default();
        let Ok(text) = std::fs::read_to_string(path()) else {
            return cfg;
        };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "mikrofon" => cfg.mic = v.parse().unwrap_or(cfg.mic).min(100),
                "kulaklik" => cfg.ear = v.parse().unwrap_or(cfg.ear).min(100),
                "durdur" => cfg.stop = Hotkey::parse(v),
                "klip_kisayol" => cfg.clip = Hotkey::parse(v),
                // ses = <kısayol> | <yol>
                "ses" => {
                    if let Some((key, p)) = v.split_once('|') {
                        cfg.sounds.push(Sound { path: PathBuf::from(p.trim()), hotkey: Hotkey::parse(key) });
                    }
                }
                _ => {}
            }
        }
        cfg
    }

    pub fn save(&self) {
        let key = |h: &Option<Hotkey>| h.map(|h| h.to_string()).unwrap_or_default();
        let mut text = format!(
            "# lyrebird ayarları: elle düzenlenebilir, uygulama yeniden başlayınca okunur\n\
             mikrofon = {}\nkulaklik = {}\ndurdur = {}\nklip_kisayol = {}\n",
            self.mic,
            self.ear,
            key(&self.stop),
            key(&self.clip)
        );
        for s in &self.sounds {
            text.push_str(&format!("ses = {} | {}\n", key(&s.hotkey), s.path.display()));
        }
        let _ = std::fs::create_dir_all(data_dir());
        if let Err(e) = std::fs::write(path(), text) {
            crate::log!("ayarlar kaydedilemedi: {e}");
        }
    }
}

/// 0..100 kaydırıcı → doğrusal kazanç. Kulak logaritmik duyduğu için kare eğri.
pub fn gain(level: u32) -> f32 {
    let x = level.min(100) as f32 / 100.0;
    x * x
}
