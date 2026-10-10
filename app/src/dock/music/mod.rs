//! Dock'un müziği: Spotify'da çalan şarkı, dock'un solunda dock boyunda bir şerit. Kapak, şarkı,
//! sanatçı, önceki / çal / sonraki ve ilerleme; istenirse başka oynatıcılar da. Ayarları dock'un
//! sayfasında, dosyası dock'un klasöründe (`muzik.ini`).
//!
//! Medya iş parçacığı (media.rs) Windows'un ortak medya denetimini izler, şerit (widget.rs)
//! arayüz iş parçacığında kendi penceresinde çizer.

mod look;
mod media;
mod widget;

use std::path::PathBuf;

use windows::Win32::Foundation::HWND;

pub use media::{Now, WM_MEDIA};
pub use widget::Style;

fn ini() -> PathBuf {
    super::dir().join("muzik.ini")
}

#[derive(Clone, PartialEq)]
pub struct Settings {
    /// Şerit açık mı.
    pub show: bool,
    pub style: Style,
    pub progress: bool,
    /// Spotify çalmıyorken başka oynatıcılar (tarayıcı, ...) da görünsün.
    pub others: bool,
    /// Hiçbir şey açık değilken şerit gizlensin.
    pub hide_idle: bool,
    /// Son görülen Spotify'ın uygulama kimliği (masaüstü, Store ya da tarayıcı uygulaması).
    pub app: String,
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Settings {
            show: true,
            style: Style::Cover,
            progress: true,
            others: false,
            hide_idle: false,
            app: String::new(),
        };
        let text = std::fs::read_to_string(ini()).unwrap_or_default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "goster" => s.show = v == "1",
                "arka_plan" => s.style = Style::from_id(v).unwrap_or(s.style),
                "ilerleme" => s.progress = v == "1",
                "diger" => s.others = v == "1",
                "bosken_gizle" => s.hide_idle = v == "1",
                "uygulama" => s.app = v.to_string(),
                _ => {}
            }
        }
        s
    }

    fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(super::dir())?;
        let b = |v: bool| v as u8;
        std::fs::write(
            ini(),
            format!(
                "goster={}\narka_plan={}\nilerleme={}\ndiger={}\nbosken_gizle={}\nuygulama={}\n",
                b(self.show),
                self.style.id(),
                b(self.progress),
                b(self.others),
                b(self.hide_idle),
                self.app,
            ),
        )
    }

    fn options(&self, opacity: u32) -> widget::Options {
        widget::Options {
            hide_idle: self.hide_idle,
            app: self.app.clone(),
            style: self.style,
            opacity: opacity as f32 / 100.0,
            progress: self.progress,
        }
    }
}

/// Müzik eskiden ayrı bir araçtı (`music`, önce `mockturtle`): ayarları dock'un klasörüne
/// taşınır, kendi klasörü silinir.
pub fn migrate() {
    let old = crate::util::data_dir().join("music");
    if !old.is_dir() {
        return;
    }
    if super::installed() && !ini().exists() {
        // Eski dosyada yer / boy gibi artık olmayan satırlar da var: yükleyip yeniden yazılır.
        let _ = std::fs::copy(old.join("ayarlar.ini"), ini());
        let _ = Settings::load().save();
    }
    match std::fs::remove_dir_all(&old) {
        Ok(()) => crate::log!("müzik ayarları dock'a taşındı"),
        Err(e) => crate::log!("eski müzik klasörü silinemedi: {e}"),
    }
}

/// `hive --music-test [klasör]`: medya oturumlarını yazdırır; klasör verilirse şeridi her arka
/// planla pencere açmadan PNG'ye çizer.
pub fn probe(out: Option<&str>) -> String {
    let (mut s, now) = media::probe();
    if let Some(dir) = out {
        for (style, hover) in Style::ALL.into_iter().map(|st| (st, false)).chain([(Style::Cover, true)]) {
            let name = format!("muzik-{}{}.png", style.id(), if hover { "-uzerinde" } else { "" });
            let path = std::path::Path::new(dir).join(name);
            match widget::preview(&path, style, hover, now.clone()) {
                Ok(()) => s += &format!("önizleme: {}\n", path.display()),
                Err(e) => s += &format!("önizleme çizilemedi: {e}\n"),
            }
        }
    }
    s
}

/// Müziğin motoru: medya izleme ve şerit. Dock açıkken yaşar.
pub struct Music {
    hwnd: HWND,
    pub settings: Settings,
    /// Arka planın opaklığı (yüzde): dock'un ayarı, şeritle ortak.
    opacity: u32,
    media: Option<media::Media>,
    pub now: Now,
}

impl Music {
    pub fn start(hwnd: HWND, opacity: u32) -> Self {
        let mut m = Self { hwnd, settings: Settings::load(), opacity, media: None, now: Now::default() };
        m.apply();
        m
    }

    /// Ayarları yazar ve uygular: şerit açılır, güncellenir ya da kapanır.
    pub fn commit(&mut self) {
        if let Err(e) = self.settings.save() {
            crate::log!("müzik: ayarlar yazılamadı: {e}");
        }
        self.apply();
    }

    /// Ayarları dosyaya yazmadan uygular (kaydırıcı sürüklenirken).
    pub fn apply(&mut self) {
        if !self.settings.show {
            widget::stop();
            self.media = None;
            self.now = Now::default();
            return;
        }
        let media = self.media.get_or_insert_with(|| media::Media::start(self.hwnd, self.settings.others));
        media.set_others(self.settings.others);
        widget::start(media.remote(), self.settings.options(self.opacity), self.now.clone());
    }

    /// Dock'un saydamlığı değişti.
    pub fn set_opacity(&mut self, opacity: u32) {
        self.opacity = opacity;
        self.apply();
    }

    /// WM_MEDIA: çalan şarkı değişti.
    pub fn media_changed(&mut self) {
        let Some(media) = &self.media else { return };
        self.now = media.now();
        if self.now.spotify && self.now.app != self.settings.app {
            self.settings.app = self.now.app.clone();
            let _ = self.settings.save();
        }
        widget::update(self.now.clone());
    }
}

impl Drop for Music {
    fn drop(&mut self) {
        widget::stop();
    }
}
