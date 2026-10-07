//! Windows'un teması: koyu/açık ve vurgu rengi kayıt defterinden okunur, renkler Windows 11'in
//! (Fluent) panel renkleridir. Dock, kontrol merkezi, tepsi ve yığın Windows'un kendi
//! panelleriyle (hızlı ayarlar, bildirimler) yan yana uyumlu görünsün diye.

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::Controls::MARGINS;
use windows::core::{PCWSTR, w};

use crate::gfx::Color;

#[derive(Clone, Copy)]
pub struct Theme {
    pub dark: bool,
    /// Panelin zemini (cam açılamazsa düz renk olarak).
    pub base: u32,
    /// Camın üstündeki renk (önçarpımsız saydamlık).
    pub tint: Color,
    /// Kart / düğme zemini ve üstüne gelince.
    pub fill: Color,
    pub fill_hover: Color,
    /// İnce kenarlık.
    pub stroke: Color,
    pub text: Color,
    pub text3: Color,
    /// Vurgu rengi (öndeki uygulamanın göstergesi).
    pub accent: Color,
}

fn dword(path: PCWSTR, name: PCWSTR) -> Option<u32> {
    let mut v = 0u32;
    let mut len = 4u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, path, name, RRF_RT_REG_DWORD, None, Some(&mut v as *mut _ as _), Some(&mut len)) }
        .is_ok()
        .then_some(v)
}

/// Vurgu paleti (8 renk, açıktan koyuya): Light3, Light2, Light1, Base, Dark1, Dark2, Dark3.
fn palette() -> Option<[u32; 8]> {
    let mut buf = [0u8; 32];
    let mut len = 32u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent"),
            w!("AccentPalette"),
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
    }
    let mut out = [0u32; 8];
    for (i, c) in buf.chunks_exact(4).enumerate() {
        out[i] = (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32;
    }
    Some(out)
}


pub fn current() -> Theme {
    let dark = dword(w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"), w!("SystemUsesLightTheme")) != Some(1);
    let pal = palette();
    // Koyu temada Windows açık düğmeleri Light2, açık temada Dark1 ile doldurur.
    let accent = pal.map(|p| if dark { p[1] } else { p[4] }).unwrap_or(if dark { 0x60cdff } else { 0x005fb8 });
    if dark {
        Theme {
            dark,
            base: 0x202020,
            tint: Color(0x202020, 0.72),
            fill: Color(0xffffff, 0.061),
            fill_hover: Color(0xffffff, 0.084),
            stroke: Color(0xffffff, 0.07),
            text: Color::rgb(0xffffff),
            text3: Color(0xffffff, 0.544),
            accent: Color::rgb(accent),
        }
    } else {
        Theme {
            dark,
            base: 0xf3f3f3,
            tint: Color(0xfcfcfc, 0.72),
            fill: Color(0xffffff, 0.7),
            fill_hover: Color(0xf9f9f9, 0.5),
            stroke: Color(0x000000, 0.0578),
            text: Color(0x000000, 0.896),
            text3: Color(0x000000, 0.446),
            accent: Color::rgb(accent),
        }
    }
}

/// Pencereyi Windows panelleri gibi yapar: tema rengi çerçeve, yuvarlak köşe, cam (acrylic)
/// zemin. İçerik saydam boyanmalı (önçarpımlı alfa); cam açılamazsa zemin düz renktir.
pub fn backdrop(hwnd: HWND, t: &Theme) {
    unsafe {
        let dark = t.dark as u32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        let round = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as _, 4);
        let margins = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        let acrylic = DWMSBT_TRANSIENTWINDOW;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &acrylic as *const _ as _, 4);
    }
}

/// Mica (pencere zemini) var mı: Windows 11 22H2 (yapı 22621) ve sonrası.
pub fn mica() -> bool {
    crate::util::reg_string(
        windows::Win32::System::Registry::HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
        "CurrentBuildNumber",
    )
    .and_then(|b| b.trim().parse::<u32>().ok())
    .is_some_and(|b| b >= 22621)
}
