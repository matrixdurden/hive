//! winloom'ın merkezinin ve bütün araçlarının paylaştığı kod. Bir araca özgü olan buraya girmez;
//! iki araç aynı şeye ihtiyaç duyunca buraya taşınır.

// t! ve log! makroları bütün modüllerde görünsün diye en önce.
#[macro_use]
pub mod i18n;
#[macro_use]
pub mod log;

pub mod frame;
pub mod gfx;
pub mod hotkey;
pub mod hub;
pub mod net;
pub mod osd;
pub mod theme;
pub mod tool;
pub mod ui;
pub mod util;
