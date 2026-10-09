//! Kısayollar: `Ctrl+Shift+F5` gibi okunur metin ⇄ (değiştirici, sanal tuş).

use windows::Win32::UI::Input::KeyboardAndMouse::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hotkey {
    pub mods: HOT_KEY_MODIFIERS,
    pub vk: u16,
}

const MODS: [(HOT_KEY_MODIFIERS, &str); 4] =
    [(MOD_CONTROL, "Ctrl"), (MOD_ALT, "Alt"), (MOD_SHIFT, "Shift"), (MOD_WIN, "Win")];

fn key_name(vk: u16) -> Option<String> {
    let v = VIRTUAL_KEY(vk);
    let fixed = match v {
        VK_SPACE => "Space",
        VK_RETURN => "Enter",
        VK_TAB => "Tab",
        VK_INSERT => "Ins",
        VK_HOME => "Home",
        VK_END => "End",
        VK_PRIOR => "PgUp",
        VK_NEXT => "PgDn",
        VK_UP => "Up",
        VK_DOWN => "Down",
        VK_LEFT => "Left",
        VK_RIGHT => "Right",
        VK_PAUSE => "Pause",
        VK_SCROLL => "ScrLk",
        VK_MULTIPLY => "Num*",
        VK_ADD => "Num+",
        VK_SUBTRACT => "Num-",
        VK_DECIMAL => "Num.",
        VK_DIVIDE => "Num/",
        VK_MEDIA_PLAY_PAUSE => "Media",
        _ => "",
    };
    if !fixed.is_empty() {
        return Some(fixed.into());
    }
    match vk {
        0x30..=0x39 | 0x41..=0x5A => Some(char::from(vk as u8).to_string()),
        _ if (VK_F1.0..=VK_F24.0).contains(&vk) => Some(format!("F{}", vk - VK_F1.0 + 1)),
        _ if (VK_NUMPAD0.0..=VK_NUMPAD9.0).contains(&vk) => Some(format!("Num{}", vk - VK_NUMPAD0.0)),
        _ => {
            // Noktalama vb. klavye düzenine göre adlandırılır (Türkçe Q'da "Ç", "Ö"...).
            let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) };
            if scan == 0 {
                return None;
            }
            let mut buf = [0u16; 32];
            let n = unsafe { GetKeyNameTextW((scan << 16) as i32, &mut buf) };
            (n > 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
        }
    }
}

/// Tek başına kısayol olamayan tuşlar (değiştiriciler, fare).
pub fn is_modifier(vk: u16) -> bool {
    matches!(
        VIRTUAL_KEY(vk),
        VK_SHIFT
            | VK_CONTROL
            | VK_MENU
            | VK_LSHIFT
            | VK_RSHIFT
            | VK_LCONTROL
            | VK_RCONTROL
            | VK_LMENU
            | VK_RMENU
            | VK_LWIN
            | VK_RWIN
    )
}

impl Hotkey {
    /// Şu an basılı değiştiricilerle birlikte.
    pub fn pressed(vk: u16) -> Self {
        let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
        let mut mods = HOT_KEY_MODIFIERS(0);
        for (m, k) in [(MOD_CONTROL, VK_CONTROL), (MOD_ALT, VK_MENU), (MOD_SHIFT, VK_SHIFT)] {
            if down(k) {
                mods |= m;
            }
        }
        if down(VK_LWIN) || down(VK_RWIN) {
            mods |= MOD_WIN;
        }
        Self { mods, vk }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let (mods_part, key) = match s.rsplit_once('+') {
            // "Num+" ya da "Ctrl+Num+": son '+' tuşun kendisi.
            Some((head, "")) => match head.rsplit_once('+') {
                Some((m, k)) => (m, format!("{k}+")),
                None => ("", format!("{head}+")),
            },
            Some((m, k)) => (m, k.to_string()),
            None => ("", s.to_string()),
        };
        let mut mods = HOT_KEY_MODIFIERS(0);
        for part in mods_part.split('+').filter(|p| !p.is_empty()) {
            mods |= MODS.iter().find(|(_, n)| n.eq_ignore_ascii_case(part.trim()))?.0;
        }
        let vk = match key.strip_prefix("0x") {
            Some(hex) => u16::from_str_radix(hex, 16).ok()?,
            None => {
                (1..=254).find(|&vk| !is_modifier(vk) && key_name(vk).is_some_and(|n| n.eq_ignore_ascii_case(&key)))?
            }
        };
        Some(Self { mods, vk })
    }
}

impl std::fmt::Display for Hotkey {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        for (m, name) in MODS {
            if self.mods.contains(m) {
                write!(f, "{name}+")?;
            }
        }
        match key_name(self.vk) {
            Some(n) => write!(f, "{n}"),
            None => write!(f, "0x{:02X}", self.vk),
        }
    }
}
