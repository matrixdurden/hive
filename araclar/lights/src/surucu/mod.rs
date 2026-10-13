//! Desteklenen ışık modelleri. Her model bir `Model`: Bluetooth adından tanınır, komutların
//! hangi GATT özelliğine ve hangi baytlarla yazılacağını verir. Sayfa, müzik ve bağlantı modeli
//! bilmez; yalnızca `Cmd` gönderir.
//!
//! Yeni ürün: `surucu/<model>.rs` içinde bir `pub static MODEL: Model` ve `MODELS`'e bir satır.

pub mod starlight;

use windows_core::GUID;

/// Işığa giden komut. Modelin desteklemediği komutu `encode` `None` döndürür, gönderilmez.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Cmd {
    Power(bool),
    Color([u8; 3]),
    /// 0..1
    Brightness(f32),
    /// Modelin `effects` listesindeki kimlik.
    Effect(u16),
    /// 0..1
    Speed(f32),
    Reverse(bool),
}

impl Cmd {
    /// Aynı türden iki komut sıradaysa yalnızca sonuncusu gider (kaydırıcı sürüklerken, müzikte).
    pub fn same_kind(&self, other: &Cmd) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

/// Işığın kendi içinde çalan hazır efekti.
pub struct Effect {
    pub id: u16,
    /// İngilizce ad; Türkçesi `effect_name` ile.
    pub name: &'static str,
}

pub struct Model {
    /// Ayar dosyasındaki kimlik.
    pub id: &'static str,
    /// Görünen ad.
    pub name: &'static str,
    /// Reklamdaki Bluetooth adı bu modelin mi.
    pub matches: fn(&str) -> bool,
    /// Komutların yazıldığı özellik.
    pub write: GUID,
    /// Cihazın cevap verdiği özellik (yalnızca log).
    pub notify: Option<GUID>,
    /// Bağlanınca ilk gönderilen (durum sorgusu gibi); boş olabilir.
    pub hello: &'static [u8],
    pub effects: &'static [Effect],
    /// Efekt yönü değişebiliyor mu.
    pub reverse: bool,
    pub encode: fn(Cmd) -> Option<Vec<u8>>,
}

/// Desteklenenler; tarama bu sırayla eşleştirir.
pub static MODELS: &[&Model] = &[&starlight::MODEL];

/// Bluetooth adı desteklenen bir modelinse o model.
pub fn for_name(name: &str) -> Option<&'static Model> {
    MODELS.iter().copied().find(|m| (m.matches)(name))
}

pub fn by_id(id: &str) -> Option<&'static Model> {
    MODELS.iter().copied().find(|m| m.id == id)
}

/// Desteklenenlerin adları ("STARLIGHT" gibi), açıklamalarda.
pub fn names() -> String {
    MODELS.iter().map(|m| m.name).collect::<Vec<_>>().join(", ")
}

/// Efektin seçili dildeki adı. Adlar renk ve hareket sözcüklerinden oluşur ("Red / Green Wave"):
/// Türkçesi sözcük sözcük çevrilir, kısaltmalar (RGB) olduğu gibi kalır.
pub fn effect_name(e: &Effect) -> String {
    if !ortak::i18n::turkish() {
        return e.name.to_string();
    }
    let mut s = e.name.replace("Open / Close", "aç-kapa");
    for (en, tr) in PHRASES {
        s = s.replace(en, tr);
    }
    let words: Vec<String> = s.split(' ').map(|w| WORDS.iter().find(|(en, _)| *en == w).map_or(w.to_string(), |(_, tr)| tr.to_string())).collect();
    let s = words.join(" ").replace("aç-kapa", "aç / kapa");
    let mut c = s.chars();
    c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str())
}

const PHRASES: &[(&str, &str)] = &[
    ("Color Energy", "renk enerjisi"),
    ("Color Bounce", "renk sekmesi"),
    ("Color Flicker", "renk titremesi"),
    ("Soft Rainbow", "yumuşak gökkuşağı"),
];

const WORDS: &[(&str, &str)] = &[
    ("Rainbow", "gökkuşağı"),
    ("Symphony", "senfoni"),
    ("Red", "kırmızı"),
    ("Green", "yeşil"),
    ("Blue", "mavi"),
    ("Yellow", "sarı"),
    ("Cyan", "turkuaz"),
    ("Purple", "mor"),
    ("White", "beyaz"),
    ("Black", "siyah"),
    ("Bounce", "sekme"),
    ("Flash", "flaş"),
    ("Breathing", "nefes"),
    ("Fade", "geçiş"),
    ("Runner", "koşucu"),
    ("Chase", "kovalamaca"),
    ("Flicker", "titreme"),
    ("Scan", "tarama"),
    ("Center", "merkez"),
    ("Pulse", "nabız"),
    ("Transition", "geçiş"),
    ("Wave", "dalga"),
    ("Tail", "kuyruk"),
    ("Run", "koşu"),
    ("Dot", "nokta"),
    ("Dots", "noktalar"),
];

/// `0x0000fff3_...` gibi 128 bitlik UUID.
pub const fn uuid(v: u128) -> GUID {
    GUID::from_u128(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turkce_adlar() {
        ortak::i18n::set_turkish(true);
        let name = |id| effect_name(starlight::MODEL.effects.iter().find(|e| e.id == id).unwrap());
        for id in [1, 3, 16, 35, 58, 93, 96, 108] {
            println!("{id}: {}", name(id));
        }
        assert_eq!(name(58), "Kırmızı / yeşil dalga");
        assert_eq!(name(35), "Gökkuşağı aç / kapa");
        assert_eq!(name(93), "PCY koşu");
        ortak::i18n::set_turkish(false);
        assert_eq!(name(58), "Red / Green Wave");
    }
}
