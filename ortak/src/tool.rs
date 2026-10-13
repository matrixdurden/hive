//! Bir aracın merkeze kendini tanıttığı tanım. Her araç crate'i (`araclar/<id>/`) bir
//! `pub static TOOL: Tool` verir; merkez araçları `merkez/src/tools.rs`'deki listeden okur.
//! Ayrıntı ve adım adım yeni araç eklemek: ARAC-EKLEME.md.

use windows::Win32::Foundation::HWND;

use crate::ui::ToolPage;

pub struct Tool {
    /// Kimlik: klasörün adı, veri klasörünün adı, `--tab <id>`. Görünen ad değişse de değişmez.
    pub id: &'static str,
    /// Eski kimlikler: kayıtlı sekme ve eski kısayollar bunlarla da açılır.
    pub old_ids: &'static [&'static str],
    /// Görünen ad: [İngilizce, Türkçe].
    pub names: [&'static str; 2],
    /// Ne işe yaradığı, tek cümle: [İngilizce, Türkçe].
    pub tagline: [&'static str; 2],
    /// Araçlar sayfasında açmanın ne getirdiği (indirme, yönetici izni...): [İngilizce, Türkçe].
    pub note: [&'static str; 2],
    /// Kapatma onayında kapatınca ne olacağı: [İngilizce, Türkçe].
    pub off_note: [&'static str; 2],
    /// Açarken bir şey indirir (düğmede "İndiriliyor…").
    pub downloads: bool,
    /// Arayüzdeki simgesi: Segoe Fluent Icons kod noktası, tek renk (kenar çubuğu, sayfa başlığı,
    /// kutucuklar). Renkli PNG'ler yalnızca Başlat menüsü kısayollarında.
    pub glyph: &'static str,
    /// (piksel, PNG): `icons!("<id>")` aracın `ikon/` klasöründen gömer.
    pub icons: &'static [(u32, &'static [u8])],

    /// Araç bu bilgisayarda açık mı.
    pub installed: fn() -> bool,
    /// Açar. Arka plan iş parçacığında çalışır (yönetici izni, indirme arayüzü kilitlemesin).
    pub install: fn() -> Result<(), String>,
    /// Kapatır: aracın bıraktığı her izi siler. Arka plan iş parçacığında çalışır.
    pub uninstall: fn() -> Result<(), String>,
    /// Aracın bu bilgisayarda bıraktığı her iz; kapalıyken boş olmalı. COM başlatılmış iş
    /// parçacığında çağrılır.
    pub leftovers: fn() -> Vec<String>,
    /// Motorunu ve sayfasını başlatır: araç açıkken winloom açılınca ya da araç açılınca.
    pub start: fn(HWND) -> Box<dyn ToolPage>,

    /// winloom süreci başlarken, her şeyden önce (pencerede de komutlarda da).
    pub init: fn(),
    /// Pencere açmadan çalışan komutlar (`--dock-test` gibi): ilk argüman bu aracınsa çalıştırır,
    /// çıkış kodunu verir.
    pub cli: fn(&[String]) -> Option<i32>,
    /// Pencere açılırken bir kez: eski kurulumların izlerini (eski adlı klasörler...) taşır.
    pub migrate: fn(),
}

impl Tool {
    /// İsteğe bağlı alanların boş hali: `Tool { id: ..., ..Tool::BASE }`.
    pub const BASE: Tool = Tool {
        id: "",
        old_ids: &[],
        names: ["", ""],
        tagline: ["", ""],
        note: ["", ""],
        off_note: ["", ""],
        downloads: false,
        glyph: "\u{E74C}",
        icons: &[],
        installed: || false,
        install: || Ok(()),
        uninstall: || Ok(()),
        leftovers: Vec::new,
        start: |_| unreachable!("aracın start'ı yok"),
        init: || {},
        cli: |_| None,
        migrate: || {},
    };

    pub fn name(&self) -> &'static str {
        t!(self.names[0], self.names[1])
    }

    pub fn tagline(&self) -> &'static str {
        t!(self.tagline[0], self.tagline[1])
    }

    pub fn note(&self) -> &'static str {
        t!(self.note[0], self.note[1])
    }

    pub fn off_note(&self) -> &'static str {
        t!(self.off_note[0], self.off_note[1])
    }
}

/// Aracın `ikon/<id>-<piksel>.png` dosyalarını gömer; çizilecek boyuta en yakın olan seçilir.
#[macro_export]
macro_rules! icons {
    ($id:literal) => {
        &[
            (24, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/ikon/", $id, "-24.png"))),
            (32, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/ikon/", $id, "-32.png"))),
            (40, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/ikon/", $id, "-40.png"))),
            (48, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/ikon/", $id, "-48.png"))),
            (64, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/ikon/", $id, "-64.png"))),
            (128, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/ikon/", $id, "-128.png"))),
        ]
    };
}

/// Pencere açmadan çalışan komutlar için: çıktı başlatan konsola gitsin, bu iş parçacığında COM
/// başlasın (`apartment`: tek iş parçacıklı daire, değilse çok iş parçacıklı).
pub fn console(apartment: bool) {
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoInitializeEx};
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        let _ = CoInitializeEx(None, if apartment { COINIT_APARTMENTTHREADED } else { COINIT_MULTITHREADED });
    }
}

/// Eski adlı veri klasörünü yeni adına taşır (yeni ad zaten varsa dokunmaz).
pub fn rename_data_dir(old: &str, new: &str) {
    let (o, n) = (crate::util::data_dir().join(old), crate::util::data_dir().join(new));
    if o.is_dir() && !n.exists() {
        match std::fs::rename(&o, &n) {
            Ok(()) => log!("{old} → {new} taşındı"),
            Err(e) => log!("{old} → {new} taşınamadı: {e}"),
        }
    }
}
