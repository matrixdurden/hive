//! soundboard motoru: sesi sanal mikrofon kurmadan doğrudan gerçek mikrofonun sinyaline ekler.
//!
//! - `player` sesleri Media Foundation ile çözer, `bus` ortak belleğine gerçek zamanın biraz
//!   önünde yazar; audiodg.exe içindeki mikrofon efekti (`apo/`) oradan okuyup karıştırır.
//! - `monitor` aynı sesi kulaklığa verir, `hotkey` kısayolları tutar, `install` efekti
//!   mikrofonlara takar ve çıkarır (yönetici ister).
//! - `clip` bilgisayarda çalan sesin son 10 saniyesini tutar, istenince wav'a yazar.
//!
//! Arayüz hive'da; motor onun sürecinde çalışır.

pub mod bus;
pub mod clip;
pub mod config;
pub mod decode;
pub mod hotkey;
pub mod install;
pub mod log;
pub mod monitor;
pub mod player;
pub mod probe;
mod util;

use std::path::PathBuf;

/// %APPDATA%\soundboard — ses listesi ve ayarlar.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("lyrebird")
}

/// Mikrofon efekti kurulu mu (sürümü ne olursa olsun; eskiyse yeniden bağlamak onu yeniler).
pub fn installed() -> bool {
    install::present()
}

/// Efekti bütün mikrofonlara takar ya da çıkarır. Yönetici olarak çalışan süreçte çağrılır.
pub fn setup(install: bool) -> util::Res<()> {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    if install { install::install() } else { install::uninstall() }
}
