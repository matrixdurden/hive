//! engram: konuştukça not tutan, notlarından cevap veren sesli ikinci beyin. Ağır bir araç:
//! motor ayrı bir süreç (`engram.exe --hub`, motor/), kendi penceresi var. Mikrofon, ses ve
//! yapay zekâ işi winloom'ı yavaşlatmasın, bir çökme onu götürmesin.
//!
//! winloom motoru içinde taşır, araç açılınca `%LOCALAPPDATA%\Programs\winloom\engram`'a yazar ve
//! winloom açılırken arka planda başlatır. Sayfa: durum ve engram'ın penceresini o sekmede açan
//! satırlar. Pencereyi açmak `engram.exe --hub --tab <sekme>` çalıştırmaktır: engram zaten
//! açıksa isteği çalışan kopyaya iletir.

#[macro_use]
extern crate ortak;

mod tool;
pub use tool::TOOL;

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use ortak::gfx::{Gfx, Rect};
use ortak::ui::*;
use ortak::{log, util};

static ENGINE: &[u8] = include_bytes!(env!("ENGRAM_EXE"));

/// Sayfa açıkken engram çalışıyor mu diye bakan zamanlayıcı.
pub const TIMER_POLL: usize = 801;
/// engram'ın tamamen kapanma mesajı (motor/src/app.rs: WM_EXIT).
const WM_ENGRAM_EXIT: u32 = WM_APP + 5;
const ROW: f32 = 64.0;
const ICON_PLAY: &str = "\u{E768}";

/// engram'ın sekmeleri: (kimlik, ad, açıklama, simge).
const TABS: [(&str, [&str; 2], [&str; 2], &str); 3] = [
    (
        "home",
        ["Home", "Ana"],
        ["Talk or type; it answers from your notes", "Konuş ya da yaz; notlarından cevap verir"],
        "\u{E8BD}",
    ),
    ("memory", ["Memory", "Hafıza"], ["Your notes, in your own folder", "Notların, kendi klasöründe"], "\u{E8F1}"),
    ("settings", ["Settings", "Ayarlar"], ["Keys, voice, shortcut", "Anahtarlar, ses, kısayol"], "\u{E713}"),
];

pub fn dir() -> PathBuf {
    util::data_dir().join("engram")
}

fn exe() -> PathBuf {
    dir().join("engram.exe")
}

pub fn installed() -> bool {
    exe().is_file()
}

fn window() -> Option<HWND> {
    unsafe { FindWindowW(w!("engram"), PCWSTR::null()).ok() }
}

/// `engram.exe --hub <args>`; beklemez.
fn launch(args: &[&str]) -> Result<(), String> {
    Command::new(exe())
        .arg("--hub")
        .args(args)
        .stdin(Stdio::null())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn()
        .map(drop)
        .map_err(|e| format!("{} {e}", t!("could not start engram:", "engram başlatılamadı:")))
}

/// Çalışan engram'a tamamen kapan der ve sürecin bitmesini bekler (exe ancak o zaman yazılır).
fn stop() {
    let Some(h) = window() else { return };
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let process = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid).ok();
        let _ = PostMessageW(Some(h), WM_ENGRAM_EXIT, WPARAM(0), LPARAM(0));
        if let Some(p) = process {
            if WaitForSingleObject(p, 6000) == WAIT_TIMEOUT {
                log!("engram kapanmadı, sonlandırılıyor");
                let _ = TerminateProcess(p, 1);
                WaitForSingleObject(p, 2000);
            }
            let _ = CloseHandle(p);
        }
    }
}

/// Exe winloom'daki sürümle aynı değilse yazar; çalışan engram önce kapatılır.
fn write_engine() -> Result<(), String> {
    if std::fs::read(exe()).is_ok_and(|d| d == ENGINE) {
        return Ok(());
    }
    stop();
    std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    let mut last = String::new();
    for _ in 0..20 {
        match std::fs::write(exe(), ENGINE) {
            Ok(()) => return Ok(()),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    Err(format!("{} {last}", t!("could not write engram.exe:", "engram.exe yazılamadı:")))
}

/// Aç: motoru yazar, tek başına kurulu engram varsa onu winloom'a taşır (`--adopt`: kapatır,
/// kayıtlarını siler, ayarlarını ve anahtarlarını buraya alır).
pub fn install() -> Result<(), String> {
    write_engine()?;
    let status = Command::new(exe())
        .args(["--hub", "--adopt"])
        .stdin(Stdio::null())
        .creation_flags(0x0800_0000)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        log!("engram --adopt: {status}");
    }
    Ok(())
}

/// Kapat: engram'ı durdurur, klasörünü (ayarlar, anahtarlar, ses önbelleği) siler. Notlar
/// kullanıcının klasöründe, onlara dokunulmaz.
pub fn uninstall() -> Result<(), String> {
    stop();
    let mut last = None;
    for _ in 0..20 {
        match std::fs::remove_dir_all(dir()) {
            Ok(()) => return Ok(()),
            Err(_) if !dir().exists() => return Ok(()),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    Err(format!("{} {}: {}", t!("could not delete", "silinemedi:"), dir().display(), last.map(|e| e.to_string()).unwrap_or_default()))
}

pub fn leftovers() -> Vec<String> {
    let d = dir();
    if d.exists() { vec![d.display().to_string()] } else { Vec::new() }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Start,
    Tab(usize),
}

pub struct Engram {
    hwnd: HWND,
    w: f32,
    h: f32,
    head_x: f32,
    head_r: f32,
    running: bool,
    error: Option<String>,
    hover: Hit,
    pressed: Hit,
}

impl Engram {
    /// winloom açılırken (araç açıksa) ya da araç açılınca: motor güncel değilse yazılır,
    /// çalışmıyorsa arka planda başlatılır.
    pub fn start(hwnd: HWND) -> Self {
        let mut error = write_engine().err();
        if window().is_none()
            && error.is_none()
            && let Err(e) = launch(&["--hidden"])
        {
            error = Some(e);
        }
        if let Some(e) = &error {
            log!("engram: {e}");
        }
        Engram { hwnd, w: 0.0, h: 0.0, head_x: 0.0, head_r: 0.0, running: true, error, hover: Hit::None, pressed: Hit::None }
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn refresh(&mut self) {
        let running = window().is_some();
        if running != self.running {
            self.running = running;
            self.redraw();
        }
    }

    fn start_rect(&self, g: &Gfx) -> Rect {
        button_rect_right(g, self.head_r, HEAD_CY - 17.0, t!("Start", "Başlat"), true)
    }

    fn panel(&self) -> Rect {
        let top = HEADER + 16.0;
        Rect::new(PAD, top, self.w - PAD, top + 12.0 + TABS.len() as f32 * ROW)
    }

    fn row(&self, i: usize) -> Rect {
        let p = self.panel();
        Rect::new(p.l + 6.0, p.t + 6.0 + i as f32 * ROW, p.r - 6.0, p.t + 6.0 + (i + 1) as f32 * ROW)
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return if !self.running && self.start_rect(g).contains(x, y) { Hit::Start } else { Hit::None };
        }
        (0..TABS.len()).find(|&i| self.row(i).contains(x, y)).map_or(Hit::None, Hit::Tab)
    }

    fn open(&mut self, tab: &str) {
        self.error = launch(&["--tab", tab]).err();
        self.running = true;
        self.redraw();
    }
}

impl ToolPage for Engram {
    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
    }

    fn paint(&self, g: &Gfx) {
        let mut right = self.head_r - 8.0;
        let (dot, text, color) = match (&self.error, self.running) {
            (Some(e), _) => (pal().red, e.clone(), pal().muted),
            (None, true) => (
                pal().green,
                t!("Running · listening in the background", "Çalışıyor · arka planda dinliyor").to_string(),
                pal().text,
            ),
            (None, false) => (pal().faint, t!("Not running", "Çalışmıyor").to_string(), pal().muted),
        };
        if !self.running {
            let r = self.start_rect(g);
            button(g, r, t!("Start", "Başlat"), Some(ICON_PLAY), Some(accent()), self.hover == Hit::Start);
            right = r.l - 12.0;
        }
        status(g, self.head_x + 16.0, HEAD_CY, dot, &text, color, right);

        let p = self.panel();
        g.fill(p, 12.0, pal().panel);
        g.stroke(p, 12.0, pal().line, 1.0);
        for (i, (_, name, sub, icon)) in TABS.iter().enumerate() {
            let r = self.row(i);
            if i > 0 {
                g.fill(Rect::new(r.l + 8.0, r.t, r.r - 8.0, r.t + 1.0), 0.0, pal().hover);
            }
            let hovered = self.hover == Hit::Tab(i);
            if hovered {
                g.fill(Rect::new(r.l + 2.0, r.t + 3.0, r.r - 2.0, r.b - 3.0), 6.0, pal().hover);
            }
            g.text(icon, &g.f.icon, Rect::new(r.l + 10.0, r.t, r.l + 42.0, r.b), pal().muted);
            let x = r.l + 52.0;
            g.text(t!(name[0], name[1]), &g.f.strong, Rect::new(x, r.t + 10.0, r.r - 48.0, r.cy()), pal().text);
            g.text(t!(sub[0], sub[1]), &g.f.small, Rect::new(x, r.cy() - 2.0, r.r - 48.0, r.b - 8.0), pal().muted);
            // "Pencerede aç" oku.
            let arrow = Rect::new(r.r - 40.0, r.t, r.r - 12.0, r.b);
            g.text("\u{E8A7}", &g.f.icon_small, arrow, if hovered { pal().text } else { pal().faint });
        }
    }

    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        let hit = self.hit(g, x, y);
        if hit != self.hover {
            self.hover = hit;
            self.redraw();
        }
    }

    fn mouse_leave(&mut self) {
        if self.hover != Hit::None {
            self.hover = Hit::None;
            self.redraw();
        }
    }

    fn mouse_down(&mut self, g: &Gfx, x: f32, y: f32) {
        self.pressed = self.hit(g, x, y);
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        let hit = self.hit(g, x, y);
        if hit != std::mem::replace(&mut self.pressed, Hit::None) {
            return;
        }
        match hit {
            Hit::Start => {
                self.error = launch(&["--hidden"]).err();
                self.running = true;
                self.redraw();
            }
            Hit::Tab(i) => self.open(TABS[i].0),
            Hit::None => {}
        }
    }

    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        self.hit(g, x, y) != Hit::None
    }

    fn set_visible(&mut self, visible: bool) {
        unsafe {
            if visible {
                SetTimer(Some(self.hwnd), TIMER_POLL, 1500, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_POLL);
            }
        }
        if visible {
            self.refresh();
        }
    }

    fn message(&mut self, _g: &mut Gfx, msg: u32, wp: WPARAM, _lp: LPARAM) -> Option<LRESULT> {
        if msg == WM_TIMER && wp.0 == TIMER_POLL {
            self.refresh();
            return Some(LRESULT(0));
        }
        None
    }
}
