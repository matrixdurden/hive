//! Ekran görüntüsü: kısayola (Print Screen) basınca ekran donar, sürükleyip seçtiğin alan
//! bırakınca panoya kopyalanır (istersen Resimler'e de kaydedilir). Tek tıklama imlecin olduğu
//! ekranın tamamını alır; Esc ya da sağ tık vazgeçer. Araç çubuğu, çizim yok.
//!
//! Sayfa: kısayol, bırakınca ne olacağı, kayıt klasörü.

#[macro_use]
extern crate ortak;

mod capture;
mod tool;
pub use tool::TOOL;

use std::path::PathBuf;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use ortak::form::{self, Ctl, Form, Item, Row};
use ortak::gfx::{Gfx, Rect};
use ortak::hotkey::{self, Hotkey};
use ortak::ui::*;
use ortak::util;

/// Kısayolun kimliği (winloom'ın penceresinde bu aracın aralığı: 9001–).
const HK_SHOT: i32 = 9001;
const DEFAULT_KEY: &str = "PrtScn";

/// Sayfanın satırları.
const R_KEY: u32 = 1;
const R_SAVE: u32 = 2;
const R_FOLDER: u32 = 3;

fn dir() -> PathBuf {
    util::data_dir().join("screenshot")
}

fn ini() -> PathBuf {
    dir().join("ayarlar.ini")
}

/// Kaydedilenlerin klasörü: Resimler\Ekran görüntüleri.
fn shots_dir() -> PathBuf {
    let pictures = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default().join("Pictures");
    pictures.join(t!("Screenshots", "Ekran görüntüleri"))
}

struct Settings {
    key: Option<Hotkey>,
    /// Bırakınca panoya kopyalamanın yanında dosyaya da kaydet.
    save: bool,
}

impl Settings {
    fn load() -> Self {
        let mut s = Settings { key: Hotkey::parse(DEFAULT_KEY), save: false };
        for line in std::fs::read_to_string(ini()).unwrap_or_default().lines() {
            match line.split_once('=') {
                Some(("kisayol", v)) => s.key = Hotkey::parse(v),
                Some(("kaydet", v)) => s.save = v.trim() == "1",
                _ => {}
            }
        }
        s
    }

    fn save(&self) {
        let key = self.key.map(|k| k.to_string()).unwrap_or_default();
        let _ = std::fs::create_dir_all(dir());
        let _ = std::fs::write(ini(), format!("kisayol={key}\nkaydet={}\n", self.save as u8));
    }
}

pub fn installed() -> bool {
    ini().is_file()
}

pub fn install() -> Result<(), String> {
    Settings::load().save();
    if installed() { Ok(()) } else { Err(t!("could not write the settings", "ayarlar yazılamadı").into()) }
}

/// Ayarlar silinir; kaydedilmiş görüntüler kullanıcının, klasöründe kalır.
pub fn uninstall() -> Result<(), String> {
    match std::fs::remove_dir_all(dir()) {
        Err(e) if dir().exists() => Err(format!("{} {}: {e}", t!("could not delete", "silinemedi:"), dir().display())),
        _ => Ok(()),
    }
}

pub fn leftovers() -> Vec<String> {
    let d = dir();
    if d.exists() { vec![d.display().to_string()] } else { Vec::new() }
}

pub struct Screenshot {
    hwnd: HWND,
    settings: Settings,
    /// Kısayol kayıtlı; tuş varken değilse başka bir uygulama tutuyor.
    registered: bool,
    /// Kısayol atanıyor.
    bind: bool,
    scroll: f32,
    hover: form::Hit,
    pressed: form::Hit,
    w: f32,
    h: f32,
    head_x: f32,
    head_r: f32,
}

impl Screenshot {
    pub fn start(hwnd: HWND) -> Self {
        let mut s = Screenshot {
            hwnd,
            settings: Settings::load(),
            registered: false,
            bind: false,
            scroll: 0.0,
            hover: form::Hit::None,
            pressed: form::Hit::None,
            w: 0.0,
            h: 0.0,
            head_x: 0.0,
            head_r: 0.0,
        };
        s.register();
        s
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn register(&mut self) {
        unsafe {
            let _ = UnregisterHotKey(Some(self.hwnd), HK_SHOT);
        }
        self.registered = self.settings.key.is_some_and(|k| unsafe {
            RegisterHotKey(Some(self.hwnd), HK_SHOT, k.mods | MOD_NOREPEAT, k.vk as u32).is_ok()
        });
        if self.settings.key.is_some() && !self.registered {
            log!("ekran görüntüsü kısayolu alınamadı: başka bir uygulamada");
        }
    }

    fn key_name(&self) -> String {
        self.settings.key.map_or_else(|| t!("no key", "tuş yok").to_string(), |k| k.to_string())
    }

    fn conflict(&self) -> bool {
        self.settings.key.is_some() && !self.registered
    }

    fn form(&self) -> Form {
        let s = &self.settings;
        let chip = if self.bind { t!("press a key", "tuşa bas").to_string() } else { self.key_name() };
        let key_sub = if self.conflict() {
            t!("Another app is using this key: pick another", "Bu tuş başka bir uygulamada: başka tuş seç")
        } else {
            t!("The screen freezes; drag an area and let go", "Ekran donar; alanı sürükle, bırak")
        };
        let items = vec![
            Item::Section(t!("Shortcut", "Kısayol").into()),
            Item::Row(Row::new(R_KEY, t!("Take a screenshot", "Ekran görüntüsü al"), key_sub, Ctl::Chip(chip, self.bind))),
            Item::Section(t!("When you let go", "Bırakınca").into()),
            Item::Row(Row::new(
                R_SAVE,
                t!("Result", "Sonuç"),
                t!("A click without dragging takes the whole screen", "Sürüklemeden tıklamak bütün ekranı alır"),
                Ctl::Seg(vec![t!("Copy", "Kopyala").into(), t!("Copy and save", "Kopyala ve kaydet").into()], s.save as usize),
            )),
            Item::Row(
                Row::new(R_FOLDER, t!("Folder", "Klasör"), shots_dir().display().to_string(), Ctl::Button(t!("Open", "Aç").into()))
                    .dim(!s.save),
            ),
        ];
        Form::new(items, PAD, self.w - PAD, HEADER + 16.0 - self.scroll)
    }

    fn max_scroll(&self) -> f32 {
        (self.form().bottom() + self.scroll - self.h).max(0.0)
    }

    fn click(&mut self, hit: form::Hit) {
        match hit {
            form::Hit::Chip(R_KEY) => {
                self.bind = !self.bind;
                self.redraw();
            }
            form::Hit::Seg(R_SAVE, k) => {
                self.settings.save = k == 1;
                self.settings.save();
                self.redraw();
            }
            form::Hit::Button(R_FOLDER) => {
                let d = shots_dir();
                let _ = std::fs::create_dir_all(&d);
                let _ = std::process::Command::new("explorer.exe").arg(&d).spawn();
            }
            _ => {}
        }
    }

    fn set_key(&mut self, key: Option<Hotkey>) {
        self.bind = false;
        self.settings.key = key;
        self.settings.save();
        self.register();
        self.redraw();
    }

    fn shoot(&self) {
        let dir = self.settings.save.then(shots_dir);
        capture::start(dir);
    }
}

impl Drop for Screenshot {
    fn drop(&mut self) {
        unsafe {
            let _ = UnregisterHotKey(Some(self.hwnd), HK_SHOT);
        }
    }
}

impl ToolPage for Screenshot {
    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn paint(&self, g: &Gfx) {
        let (dot, text) = match () {
            _ if self.settings.key.is_none() => (pal().faint, t!("Pick a key to start", "Başlamak için bir tuş seç").to_string()),
            _ if self.conflict() => (pal().red, t!(format!("{} is taken by another app", self.key_name()), format!("{} başka bir uygulamada", self.key_name()))),
            _ => (pal().green, t!(format!("Press {} to take a screenshot", self.key_name()), format!("{} ile ekran görüntüsü al", self.key_name()))),
        };
        status(g, self.head_x + 16.0, HEAD_CY, dot, &text, pal().muted, self.head_r - 8.0);
        g.clip(Rect::new(0.0, HEADER, self.w, self.h), || self.form().paint(g, self.hover, None));
    }

    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        let hit = if y < HEADER { form::Hit::None } else { self.form().hit(g, x, y) };
        if hit != self.hover {
            self.hover = hit;
            self.redraw();
        }
    }

    fn mouse_leave(&mut self) {
        self.hover = form::Hit::None;
        self.redraw();
    }

    fn mouse_down(&mut self, g: &Gfx, x: f32, y: f32) {
        self.pressed = if y < HEADER { form::Hit::None } else { self.form().hit(g, x, y) };
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        let hit = if y < HEADER { form::Hit::None } else { self.form().hit(g, x, y) };
        let pressed = std::mem::replace(&mut self.pressed, form::Hit::None);
        if hit.same(pressed) {
            self.click(hit);
        } else if self.bind {
            self.bind = false;
            self.redraw();
        }
    }

    fn wheel(&mut self, _g: &Gfx, _x: f32, _y: f32, delta: f32) {
        self.scroll = (self.scroll - delta * 60.0).clamp(0.0, self.max_scroll());
        self.redraw();
    }

    /// Kısayol atanıyorsa tuşu yakalar: Esc vazgeçer, Backspace / Delete siler.
    fn key(&mut self, vk: u16) -> bool {
        if !self.bind {
            return false;
        }
        if !hotkey::is_modifier(vk) {
            match VIRTUAL_KEY(vk) {
                VK_ESCAPE => {
                    self.bind = false;
                    self.redraw();
                }
                VK_BACK | VK_DELETE => self.set_key(None),
                _ => self.set_key(Some(Hotkey::pressed(vk))),
            }
        }
        true
    }

    fn binding(&self) -> bool {
        self.bind
    }

    fn kill_focus(&mut self) {
        if self.bind {
            self.bind = false;
            self.redraw();
        }
    }

    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        y >= HEADER && self.form().hit(g, x, y) != form::Hit::None
    }

    fn set_visible(&mut self, _visible: bool) {}

    fn message(&mut self, _g: &mut Gfx, msg: u32, wp: WPARAM, _lp: LPARAM) -> Option<LRESULT> {
        if msg == WM_HOTKEY && wp.0 as i32 == HK_SHOT {
            self.shoot();
            return Some(LRESULT(0));
        }
        None
    }

    fn summary(&self) -> Option<(String, String)> {
        let what = if self.settings.save { t!("Copies and saves", "Kopyalar ve kaydeder") } else { t!("Copies to the clipboard", "Panoya kopyalar") };
        Some((self.key_name(), what.into()))
    }
}
