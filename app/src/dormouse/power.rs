//! Windows güç ayarları: güç katmanı (Verimlilik / Dengeli / Performans), pildeyken kapak eylemi ve
//! işlemci üst sınırı, ekranın yenileme hızı, pil durumu. Hiçbiri yönetici izni istemez.

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Power::*;
use windows::Win32::System::SystemServices::{
    GUID_LIDCLOSE_ACTION, GUID_PROCESSOR_SETTINGS_SUBGROUP, GUID_PROCESSOR_THROTTLE_MAXIMUM,
    GUID_SYSTEM_BUTTON_SUBGROUP,
};
use windows::core::{GUID, PCWSTR};

use crate::log;

/// Windows 11'in güç modu katmanları (Ayarlar › Güç modu). Dengeli = sıfır GUID.
pub const OVERLAY_EFFICIENCY: GUID = GUID::from_u128(0x961cc777_2547_4f9d_8174_7d86181b8a7a);
pub const OVERLAY_BALANCED: GUID = GUID::zeroed();
pub const OVERLAY_PERFORMANCE: GUID = GUID::from_u128(0xded574b5_45a0_4f42_8737_46345c09c238);

/// Kapak kapanınca: 0 hiçbir şey, 1 uyku, 2 hazırda beklet, 3 kapat.
pub const LID_HIBERNATE: u32 = 2;

// windows crate'inde bağlanmamış: powrprof.dll'in Windows 10/11'de sabit dışa aktarımları.
#[link(name = "powrprof")]
unsafe extern "system" {
    fn PowerSetActiveOverlayScheme(overlay: *const GUID) -> u32;
    fn PowerGetEffectiveOverlayScheme(overlay: *mut GUID) -> u32;
}

/// Şu anki güç kaynağı için etkin katman.
pub fn overlay() -> Option<GUID> {
    let mut g = GUID::zeroed();
    (unsafe { PowerGetEffectiveOverlayScheme(&mut g) } == 0).then_some(g)
}

/// Şu anki güç kaynağının katmanını değiştirir (Windows AC ve pil için ayrı tutar).
pub fn set_overlay(g: &GUID) -> bool {
    let ok = unsafe { PowerSetActiveOverlayScheme(g) } == 0;
    if !ok {
        log!("dormouse: güç katmanı ayarlanamadı");
    }
    ok
}

pub fn overlay_name(g: &GUID) -> &'static str {
    match *g {
        OVERLAY_EFFICIENCY => t!("best power efficiency", "en iyi güç verimliliği"),
        OVERLAY_PERFORMANCE => t!("best performance", "en yüksek performans"),
        _ => t!("balanced", "dengeli"),
    }
}

/// Etkin güç planı.
fn scheme() -> Option<GUID> {
    let mut p: *mut GUID = std::ptr::null_mut();
    unsafe {
        if PowerGetActiveScheme(None, &mut p).0 != 0 || p.is_null() {
            return None;
        }
        let g = *p;
        let _ = LocalFree(Some(HLOCAL(p as *mut _)));
        Some(g)
    }
}

fn dc_index(sub: &GUID, setting: &GUID) -> Option<u32> {
    let s = scheme()?;
    let mut v = 0u32;
    (unsafe { PowerReadDCValueIndex(None, Some(&s), Some(sub), Some(setting), &mut v) } == 0).then_some(v)
}

fn set_dc_index(sub: &GUID, setting: &GUID, v: u32) -> bool {
    let Some(s) = scheme() else { return false };
    unsafe {
        PowerWriteDCValueIndex(None, &s, Some(sub), Some(setting), v) == 0
            && PowerSetActiveScheme(None, Some(&s)).0 == 0
    }
}

/// Pildeyken kapak kapanınca ne olur.
pub fn lid_dc() -> Option<u32> {
    dc_index(&GUID_SYSTEM_BUTTON_SUBGROUP, &GUID_LIDCLOSE_ACTION)
}

pub fn set_lid_dc(v: u32) -> bool {
    lid_dc() == Some(v) || set_dc_index(&GUID_SYSTEM_BUTTON_SUBGROUP, &GUID_LIDCLOSE_ACTION, v)
}

/// Pildeyken işlemcinin üst sınırı (yüzde).
pub fn cpu_max_dc() -> Option<u32> {
    dc_index(&GUID_PROCESSOR_SETTINGS_SUBGROUP, &GUID_PROCESSOR_THROTTLE_MAXIMUM)
}

pub fn set_cpu_max_dc(v: u32) -> bool {
    cpu_max_dc() == Some(v) || set_dc_index(&GUID_PROCESSOR_SETTINGS_SUBGROUP, &GUID_PROCESSOR_THROTTLE_MAXIMUM, v)
}

fn current_mode() -> Option<DEVMODEW> {
    let mut dm = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    unsafe { EnumDisplaySettingsW(PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut dm).as_bool() }.then_some(dm)
}

/// Ana ekranın yenileme hızı (Hz).
pub fn refresh_rate() -> Option<u32> {
    current_mode().map(|dm| dm.dmDisplayFrequency)
}

/// Yalnızca hızı değiştirir; çözünürlük ve konum olduğu gibi kalır. Kalıcı değildir (kayıt
/// defterine yazılmaz): hive kapanınca Windows bir sonraki açılışta kendi ayarına döner.
pub fn set_refresh_rate(hz: u32) -> bool {
    let Some(mut dm) = current_mode() else { return false };
    if dm.dmDisplayFrequency == hz {
        return true;
    }
    dm.dmDisplayFrequency = hz;
    dm.dmFields = DM_DISPLAYFREQUENCY;
    let ok = unsafe { ChangeDisplaySettingsW(Some(&dm), CDS_TYPE(0)) } == DISP_CHANGE_SUCCESSFUL;
    if !ok {
        log!("dormouse: {hz} Hz ayarlanamadı");
    }
    ok
}

#[derive(Clone, Copy, Debug)]
pub struct Status {
    pub ac: bool,
    pub percent: u8,
    /// Makinede pil var mı (masaüstünde yok).
    pub battery: bool,
}

pub fn status() -> Status {
    let mut s = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut s) }.is_err() {
        return Status { ac: true, percent: 100, battery: false };
    }
    Status {
        ac: s.ACLineStatus == 1,
        percent: if s.BatteryLifePercent > 100 { 100 } else { s.BatteryLifePercent },
        battery: s.BatteryFlag & 128 == 0,
    }
}
