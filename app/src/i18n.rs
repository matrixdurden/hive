//! Arayüz dili: İngilizce (ana) ya da Türkçe. İlk açılışta Windows'un arayüz dilinden seçilir,
//! Ayarlar'dan değiştirilir (`ayarlar.ini`: `dil=en|tr`).
//!
//! Metinler yerinde iki dilli yazılır: `t!("Install", "Kur")`.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Globalization::GetUserDefaultUILanguage;

static TURKISH: AtomicBool = AtomicBool::new(false);

pub fn turkish() -> bool {
    TURKISH.load(Ordering::Relaxed)
}

pub fn set_turkish(on: bool) {
    TURKISH.store(on, Ordering::Relaxed);
}

/// Ayarda dil yoksa: Windows'un arayüz dili Türkçe mi (birincil dil kimliği 0x1F).
pub fn system_turkish() -> bool {
    unsafe { GetUserDefaultUILanguage() & 0x3FF == 0x1F }
}

/// `t!(english, türkçe)`: seçili dildeki metin. İki dal aynı türde olmalı (`&str` ya da `String`).
#[macro_export]
macro_rules! t {
    ($en:expr, $tr:expr $(,)?) => {
        if $crate::i18n::turkish() { $tr } else { $en }
    };
}
