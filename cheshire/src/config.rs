use std::path::PathBuf;

use crate::util::app_dir;

/// Basit `anahtar = değer` dosyası. Bilinmeyen satırlar yok sayılır, eksikler varsayılana düşer.
#[derive(Clone, Debug)]
pub struct Config {
    /// Seçili duvar kâğıdının dosya adı (`duvarlar` klasöründe).
    pub wallpaper: String,
    /// Genel kare hızı sınırı; dosyadaki `fps` daha düşükse o geçerli.
    pub fps: u32,
    pub pause_on_battery: bool,
    /// Yeni sürüm GitHub'a çıkınca kendini güncellesin mi?
    pub auto_update: bool,
    /// `true` ise harici GPU (NVIDIA) tercih edilir. Varsayılan: pil dostu iGPU.
    pub high_performance_gpu: bool,
    /// Masaüstüne yerleşme yöntemi (yalnızca 24H2+ için): `auto`/`progman` DefView'ın altına katmanlı
    /// pencere, `worker` Explorer'ın WorkerW'sinin içi.
    pub host: String,
    /// (dosya, parametre, değer) — tepsi menüsünden seçilen parametre değerleri.
    pub params: Vec<(String, String, String)>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            wallpaper: "akis.cheshire".into(),
            fps: 60,
            pause_on_battery: true,
            auto_update: true,
            high_performance_gpu: false,
            host: "auto".into(),
            params: Vec::new(),
        }
    }
}

fn path() -> PathBuf {
    app_dir().join("cheshire.ini")
}

impl Config {
    pub fn load() -> Self {
        let mut cfg = Self::default();
        let Ok(text) = std::fs::read_to_string(path()) else { return cfg };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "duvar_kagidi" => cfg.wallpaper = v.to_string(),
                "fps" => cfg.fps = v.parse().unwrap_or(cfg.fps).clamp(1, 240),
                "pilde_duraklat" => cfg.pause_on_battery = v == "evet",
                "otomatik_guncelle" => cfg.auto_update = v == "evet",
                "guclu_gpu" => cfg.high_performance_gpu = v == "evet",
                "yerlesim" => cfg.host = v.to_string(),
                _ => {
                    // param.<dosya>.<ad> — parametre adları GLSL tanımlayıcısıdır, nokta içermez.
                    if let Some((file, name)) = k.strip_prefix("param.").and_then(|r| r.rsplit_once('.')) {
                        cfg.params.push((file.into(), name.into(), v.into()));
                    }
                }
            }
        }
        cfg
    }

    pub fn save(&self) {
        let yn = |b: bool| if b { "evet" } else { "hayir" };
        let mut text = format!(
            "# cheshire ayarları — elle düzenlenebilir, uygulama yeniden başlayınca okunur\n\
             duvar_kagidi = {}\nfps = {}\npilde_duraklat = {}\notomatik_guncelle = {}\nguclu_gpu = {}\nyerlesim = {}\n",
            self.wallpaper,
            self.fps,
            yn(self.pause_on_battery),
            yn(self.auto_update),
            yn(self.high_performance_gpu),
            self.host,
        );
        for (file, name, value) in &self.params {
            text.push_str(&format!("param.{file}.{name} = {value}\n"));
        }
        let _ = std::fs::create_dir_all(app_dir());
        if let Err(e) = std::fs::write(path(), text) {
            crate::log!("ayarlar kaydedilemedi: {e}");
        }
    }

    pub fn param(&self, file: &str, name: &str) -> Option<&str> {
        self.params.iter().find(|(f, n, _)| f == file && n == name).map(|(_, _, v)| v.as_str())
    }

    pub fn set_param(&mut self, file: &str, name: &str, value: String) {
        match self.params.iter_mut().find(|(f, n, _)| f == file && n == name) {
            Some(entry) => entry.2 = value,
            None => self.params.push((file.into(), name.into(), value)),
        }
    }
}
