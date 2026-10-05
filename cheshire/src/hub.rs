//! hive modu (`cheshire --hub <pencere>`): tepsi simgesi, kendini güncelleme ve kayıt
//! defteri işleri hive'da. Motor komutları `WM_COPYDATA` ile alır, durumunu `durum.txt`
//! dosyasına yazıp hive penceresine haber verir.
//!
//! Komutlar (UTF-8, tek satır):
//!   hub <pencere>        bildirimlerin gideceği pencere (hive yeniden açılınca)
//!   duvar <dosya>        duvar kâğıdını uygula
//!   param <sıra> <değer> parametre: sayı ya da #rrggbb
//!   fps <n>              kare hızı sınırı
//!   duraklat <0|1>       kullanıcı duraklatması
//!   pilde <0|1>          pildeyken duraklat
//!   klasor               duvar kâğıdı klasörünü aç
//!   cik                  kapan

use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, PostMessageW, WM_APP};

use crate::util;

/// `WM_COPYDATA` ile gelen komutun kimliği.
pub const COPYDATA_COMMAND: usize = 0xD0FB;
/// hive'a giden "durum değişti" mesajı.
pub const WM_HUB_STATE: u32 = WM_APP + 40;

static HUB: AtomicIsize = AtomicIsize::new(0);

pub fn set(hwnd: isize) {
    HUB.store(hwnd, Ordering::Relaxed);
}

/// hive modunda mı (tepsi simgesi yok, güncelleme ve kayıtlar hive'da).
pub fn active() -> bool {
    HUB.load(Ordering::Relaxed) != 0
}

/// hive penceresi kapandıysa `false`: motor da kapanmalı.
pub fn alive() -> bool {
    let h = HUB.load(Ordering::Relaxed);
    h == 0 || unsafe { IsWindow(Some(HWND(h as *mut _))).as_bool() }
}

pub fn state_path() -> PathBuf {
    util::app_dir().join("durum.txt")
}

/// Durumu yazar (yarım dosya okunmasın diye önce geçici dosyaya) ve hive'a haber verir.
pub fn publish(state: &str) {
    let path = state_path();
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, state).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
    let h = HUB.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_HUB_STATE, WPARAM(0), LPARAM(0));
        }
    }
}
