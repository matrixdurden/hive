//! rabbithole: ağ engellerini aşan tünel. Motoru bir Windows hizmeti (`rabbithole`); kullanıcılar
//! onu yönetici izni olmadan başlatıp durdurabilir. Sayfa durumu hizmet yöneticisinden ve
//! `%ProgramData%\rabbithole` dosyalarından okur; yönetici isteyen işler (sunucu bağlantısı,
//! Windows ile başlat) ve ağ testi için `rabbithole.exe` çalıştırılır.

use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Services::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::gfx::{Color, Gfx, Rect};
use crate::log;
use crate::ui::*;

const ACCENT: Color = Color::rgb(0xa78bfa);

pub const WM_DONE: u32 = WM_APP + 30;
pub const WM_LINE: u32 = WM_APP + 31;
pub const TIMER_POLL: usize = 301;
pub const TIMER_PING: usize = 302;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const LINE_H: f32 = 22.0;

pub fn install_dir() -> PathBuf {
    let pf = std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| r"C:\Program Files".into());
    pf.join("rabbithole")
}

pub fn exe() -> PathBuf {
    let pf = std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| r"C:\Program Files".into());
    pf.join(r"rabbithole\rabbithole.exe")
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("ProgramData").map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from).join("rabbithole")
}

/// Hizmet kurulu mu (hive'da "kurulu" sayılmak için).
pub fn installed() -> bool {
    Service::open(SERVICE_QUERY_STATUS).is_some()
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Server,
    Dpi,
}

impl Mode {
    fn arg(self) -> PCWSTR {
        match self {
            Mode::Server => w!("server"),
            Mode::Dpi => w!("dpi"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    Off,
    Starting,
    On,
    Stopping,
}

#[derive(Clone, Default)]
struct Status {
    installed: bool,
    state: Option<State>,
    mode: Option<Mode>,
    autostart: bool,
    /// Sunucu bağlantısı varsa adresi ve adı.
    host: Option<String>,
    port: Option<String>,
    name: Option<String>,
    /// Son başlatma hatası (hizmet kendine özel kodla durduysa log'un son satırı).
    error: Option<String>,
}

// --- Hizmet yöneticisi ---

struct Service {
    scm: SC_HANDLE,
    svc: SC_HANDLE,
}

impl Service {
    fn open(access: u32) -> Option<Self> {
        unsafe {
            let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT).ok()?;
            match OpenServiceW(scm, w!("rabbithole"), access) {
                Ok(svc) => Some(Self { scm, svc }),
                Err(_) => {
                    let _ = CloseServiceHandle(scm);
                    None
                }
            }
        }
    }

    fn status(&self) -> Option<SERVICE_STATUS_PROCESS> {
        let mut st = SERVICE_STATUS_PROCESS::default();
        let mut needed = 0u32;
        unsafe {
            let buf = std::slice::from_raw_parts_mut(
                &mut st as *mut _ as *mut u8,
                size_of::<SERVICE_STATUS_PROCESS>(),
            );
            QueryServiceStatusEx(self.svc, SC_STATUS_PROCESS_INFO, Some(buf), &mut needed).ok()?;
        }
        Some(st)
    }

    fn autostart(&self) -> bool {
        unsafe {
            let mut needed = 0u32;
            let _ = QueryServiceConfigW(self.svc, None, 0, &mut needed);
            if needed == 0 {
                return false;
            }
            let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
            let cfg = buf.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW;
            if QueryServiceConfigW(self.svc, Some(cfg), needed, &mut needed).is_err() {
                return false;
            }
            (*cfg).dwStartType == SERVICE_AUTO_START
        }
    }

    fn wait(&self, want: SERVICE_STATUS_CURRENT_STATE, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self.status().is_some_and(|s| s.dwCurrentState == want) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        false
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.svc);
            let _ = CloseServiceHandle(self.scm);
        }
    }
}

/// `"anahtar": "değer"` — client.json için yeterli, tam JSON çözücü gerekmez.
fn json_str(text: &str, key: &str) -> Option<String> {
    let at = text.find(&format!("\"{key}\""))?;
    let rest = &text[at + key.len() + 2..];
    let rest = rest[rest.find(':')? + 1..].trim_start();
    if let Some(r) = rest.strip_prefix('"') {
        Some(r[..r.find('"')?].to_string())
    } else {
        let end = rest.find([',', '}', '\n']).unwrap_or(rest.len());
        Some(rest[..end].trim().to_string())
    }
}

fn read_status() -> Status {
    let mut s = Status::default();
    let Some(svc) = Service::open(SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG) else {
        return s;
    };
    s.installed = true;
    s.autostart = svc.autostart();
    let st = svc.status();
    s.state = st.map(|st| match st.dwCurrentState {
        SERVICE_RUNNING => State::On,
        SERVICE_START_PENDING => State::Starting,
        SERVICE_STOP_PENDING => State::Stopping,
        _ => State::Off,
    });
    let dir = data_dir();
    if let Ok(text) = std::fs::read_to_string(dir.join("client.json")) {
        s.host = json_str(&text, "host");
        s.port = json_str(&text, "port");
        s.name = json_str(&text, "name").filter(|n| !n.is_empty());
    }
    s.mode = match std::fs::read_to_string(dir.join("mode")).unwrap_or_default().trim() {
        "server" => Some(Mode::Server),
        "dpi" => Some(Mode::Dpi),
        _ if s.host.is_some() => Some(Mode::Server),
        _ => Some(Mode::Dpi),
    };
    if s.state == Some(State::Off) && st.is_some_and(|st| st.dwServiceSpecificExitCode == 1) {
        let log = std::fs::read_to_string(dir.join("rabbithole.log")).unwrap_or_default();
        s.error = log.lines().rev().find_map(|l| l.split_once("rabbithole: ").map(|(_, e)| e.to_string()));
    }
    s
}

/// Modu değiştirir ya da kapatır: çalışıyorsa önce durdurur, sonra istenen modla başlatır.
fn switch(target: Option<Mode>) -> Result<String, String> {
    let svc = Service::open(SERVICE_QUERY_STATUS | SERVICE_START | SERVICE_STOP)
        .ok_or("rabbithole hizmeti açılamadı")?;
    let st = svc.status().ok_or("hizmet durumu okunamadı")?;
    if st.dwCurrentState != SERVICE_STOPPED {
        let mut ss = SERVICE_STATUS::default();
        unsafe {
            let _ = ControlService(svc.svc, SERVICE_CONTROL_STOP, &mut ss);
        }
        if !svc.wait(SERVICE_STOPPED, Duration::from_secs(15)) {
            return Err("tünel 15 saniyede durmadı".into());
        }
    }
    let Some(mode) = target else { return Ok("kapatıldı".into()) };
    unsafe { StartServiceW(svc.svc, Some(&[mode.arg()])) }.map_err(|e| format!("başlatılamadı: {}", e.message()))?;
    Ok(String::new())
}

/// `rabbithole.exe <args>`: yönetici isteyenler kendi UAC penceresini açar. Konsol açılmaz,
/// stdin kapalı (çift tıklama yolundaki "Enter'a bas" beklemesine düşmesin).
fn command(args: &[&str]) -> Command {
    let mut c = Command::new(exe());
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(CREATE_NO_WINDOW);
    c
}

pub fn run(args: &[&str]) -> Result<String, String> {
    let out = command(args).output().map_err(|e| format!("rabbithole çalıştırılamadı: {e}"))?;
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.success() {
        Ok(String::new())
    } else {
        Err(err.lines().rev().find_map(|l| l.strip_prefix("error: ")).unwrap_or(err.trim()).to_string())
    }
}

fn clipboard_text(hwnd: HWND) -> Option<String> {
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let text = (|| {
            let h = GetClipboardData(13).ok()?; // CF_UNICODETEXT
            let hg = HGLOBAL(h.0);
            let p = GlobalLock(hg) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(hg);
            Some(s)
        })();
        let _ = CloseClipboard();
        text
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Mode(usize),
    Link,
    Autostart,
    Doctor,
}

#[derive(Default)]
struct Doctor {
    lines: Vec<String>,
    running: bool,
}

pub struct Rabbithole {
    hwnd: HWND,
    st: Status,
    /// Sunucuya TCP bağlantı süresi (ms), sunucu modunda açıkken.
    latency: Arc<Mutex<Option<u32>>>,
    /// Süren iş ve sonucu.
    busy: Option<&'static str>,
    result: Arc<Mutex<Option<Result<String, String>>>>,
    message: Option<(String, bool)>,
    doctor: Option<Arc<Mutex<Doctor>>>,
    scroll: f32,
    hover: Hit,
    pressed: Hit,
    w: f32,
    h: f32,
    head_x: f32,
    head_r: f32,
    visible: bool,
}

impl Rabbithole {
    pub fn start(hwnd: HWND) -> Self {
        let r = Self {
            hwnd,
            st: read_status(),
            latency: Arc::default(),
            busy: None,
            result: Arc::default(),
            message: None,
            doctor: None,
            scroll: 0.0,
            hover: Hit::None,
            pressed: Hit::None,
            w: 0.0,
            h: 0.0,
            head_x: 0.0,
            head_r: 0.0,
            visible: false,
        };
        log!("rabbithole: {:?} {:?}, sunucu {}", r.st.state, r.st.mode, r.st.host.is_some());
        r
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    pub fn refresh(&mut self) {
        self.st = read_status();
        self.redraw();
    }

    /// Bir işi arka planda çalıştırır; bitince WM_DONE gelir.
    fn spawn(&mut self, label: &'static str, job: impl FnOnce() -> Result<String, String> + Send + 'static) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(label);
        self.message = None;
        let (result, hwnd) = (self.result.clone(), self.hwnd.0 as usize);
        std::thread::spawn(move || {
            let r = job();
            *result.lock().unwrap() = Some(r);
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_DONE, WPARAM(0), LPARAM(0));
            }
        });
        self.redraw();
    }

    pub fn done(&mut self) {
        self.busy = None;
        if let Some(r) = self.result.lock().unwrap().take() {
            self.message = match r {
                Ok(m) if m.is_empty() => None,
                Ok(m) => Some((m, false)),
                Err(e) => {
                    log!("rabbithole: {e}");
                    Some((e, true))
                }
            };
        }
        self.refresh();
    }

    fn set_mode(&mut self, target: Option<Mode>) {
        if target == Some(Mode::Server) && self.st.host.is_none() {
            self.message = Some(("Önce sunucundan aldığın bağlantıyı ekle".into(), true));
            self.redraw();
            return;
        }
        let label = if target.is_some() { "Bağlanıyor…" } else { "Kapatılıyor…" };
        self.spawn(label, move || switch(target));
    }

    fn add_link(&mut self) {
        let text = clipboard_text(self.hwnd).unwrap_or_default();
        let link = text.trim().to_string();
        if !link.starts_with("vless://") {
            self.message = Some(("Panoda vless:// bağlantısı yok: önce sunucundan aldığın bağlantıyı kopyala".into(), true));
            self.redraw();
            return;
        }
        self.spawn("Sunucu deneniyor…", move || run(&["client", &link]).map(|_| "Sunucu bağlantısı eklendi".into()));
    }

    fn toggle_autostart(&mut self) {
        let arg = if self.st.autostart { "off" } else { "on" };
        self.spawn("Ayarlanıyor…", move || run(&["autostart", arg]));
    }

    fn run_doctor(&mut self) {
        if self.doctor.as_ref().is_some_and(|d| d.lock().unwrap().running) {
            return;
        }
        let d = Arc::new(Mutex::new(Doctor { lines: Vec::new(), running: true }));
        self.doctor = Some(d.clone());
        self.scroll = 0.0;
        let hwnd = self.hwnd.0 as usize;
        std::thread::spawn(move || {
            let post = || unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_LINE, WPARAM(0), LPARAM(0));
            };
            match command(&["doctor"]).spawn() {
                Ok(mut child) => {
                    if let Some(out) = child.stdout.take() {
                        for line in BufReader::new(out).lines().map_while(Result::ok) {
                            // Rapor Masaüstüne de kaydediliyor: burada gösterildiği için o dosya
                            // silinir, geride iz kalmaz.
                            if let Some(path) = line.trim().strip_prefix("Saved to ") {
                                let _ = std::fs::remove_file(path.trim());
                                continue;
                            }
                            d.lock().unwrap().lines.push(line);
                            post();
                        }
                    }
                    let _ = child.wait();
                }
                Err(e) => d.lock().unwrap().lines.push(format!("rabbithole çalıştırılamadı: {e}")),
            }
            d.lock().unwrap().running = false;
            post();
        });
        self.redraw();
    }

    pub fn doctor_line(&mut self) {
        // Yeni satırlar gelince en alta kaydır.
        if let Some(d) = &self.doctor {
            let n = d.lock().unwrap().lines.len() as f32;
            self.scroll = (n * LINE_H + 40.0 - (self.h - HEADER - 16.0)).max(0.0);
        }
        self.redraw();
    }

    fn ping(&self) {
        let (Some(host), Some(port)) = (self.st.host.clone(), self.st.port.clone()) else { return };
        if !(self.st.state == Some(State::On) && self.st.mode == Some(Mode::Server)) {
            *self.latency.lock().unwrap() = None;
            return;
        }
        let (latency, hwnd) = (self.latency.clone(), self.hwnd.0 as usize);
        std::thread::spawn(move || {
            use std::net::{TcpStream, ToSocketAddrs};
            let ms = format!("{host}:{port}").to_socket_addrs().ok().and_then(|mut a| a.next()).and_then(|addr| {
                let t = Instant::now();
                TcpStream::connect_timeout(&addr, Duration::from_secs(3)).ok().map(|_| t.elapsed().as_millis() as u32)
            });
            *latency.lock().unwrap() = ms;
            unsafe {
                let _ = InvalidateRect(Some(HWND(hwnd as *mut _)), None, false);
            }
        });
    }

    pub fn timer(&mut self, id: usize) {
        match id {
            TIMER_POLL => {
                let before = (self.st.state, self.st.mode);
                self.refresh();
                if (self.st.state, self.st.mode) != before {
                    self.ping();
                }
            }
            TIMER_PING => self.ping(),
            _ => {}
        }
    }

    // --- Yerleşim ---

    fn doctor_open(&self) -> bool {
        self.doctor.is_some()
    }

    fn doctor_rect(&self, g: &Gfx) -> Rect {
        let label = if self.doctor_open() { "Kapat" } else { "Ağı test et" };
        button_rect_right(g, self.head_r, HEAD_CY - 17.0, label, !self.doctor_open())
    }

    fn circle_cy(&self) -> f32 {
        HEADER + 92.0
    }

    fn mode_rect(&self, k: usize) -> Rect {
        let cw = 112.0;
        let l = self.w / 2.0 - cw * 1.5 + k as f32 * cw;
        let t = self.circle_cy() + 128.0;
        Rect::new(l, t, l + cw, t + 36.0)
    }

    fn row(&self, n: usize) -> Rect {
        let t = self.mode_rect(0).b + 48.0 + n as f32 * 64.0;
        Rect::new(PAD, t, self.w - PAD, t + 64.0)
    }

    fn link_rect(&self, g: &Gfx) -> Rect {
        let r = self.row(0);
        let label = if self.st.host.is_some() { "Değiştir" } else { "Panodan ekle" };
        button_rect_right(g, r.r, r.cy() - 17.0, label, false)
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if self.doctor_rect(g).contains(x, y) {
            return Hit::Doctor;
        }
        if self.doctor_open() {
            return Hit::None;
        }
        if let Some(k) = (0..3).find(|&k| self.mode_rect(k).contains(x, y)) {
            return Hit::Mode(k);
        }
        if self.link_rect(g).contains(x, y) {
            return Hit::Link;
        }
        if self.row(1).contains(x, y) {
            return Hit::Autostart;
        }
        Hit::None
    }

    // --- Çizim ---

    fn paint_doctor(&self, g: &Gfx) {
        let Some(d) = &self.doctor else { return };
        let d = d.lock().unwrap();
        let area = Rect::new(0.0, HEADER, self.w, self.h);
        g.clip(area, || {
            let mut y = HEADER + 16.0 - self.scroll;
            for line in &d.lines {
                if y + LINE_H >= HEADER && y <= self.h {
                    let t = line.trim_start();
                    let (c, f) = match () {
                        _ if t.starts_with('✓') => (GREEN.alpha(0.9), &g.f.text),
                        _ if t.starts_with('✗') => (RED, &g.f.text),
                        _ if !line.starts_with(' ') && !line.is_empty() => (TEXT, &g.f.strong),
                        _ => (MUTED, &g.f.text),
                    };
                    g.text(line, f, Rect::new(PAD, y, self.w - PAD, y + LINE_H), c);
                }
                y += LINE_H;
            }
            if d.running {
                g.text("Ağ test ediliyor, 10–20 saniye sürer…", &g.f.small, Rect::new(PAD, y + 6.0, self.w - PAD, y + 28.0), FAINT);
            }
        });
    }

    pub fn paint(&self, g: &Gfx) {
        let (w, cy) = (self.w, self.circle_cy());
        let accent_live = self.st.state == Some(State::On);
        let dr = self.doctor_rect(g);
        if self.doctor_open() {
            button(g, dr, "Kapat", None, None, self.hover == Hit::Doctor);
        } else {
            button(g, dr, "Ağı test et", Some(ICON_REFRESH), None, self.hover == Hit::Doctor);
        }
        if self.doctor_open() {
            self.paint_doctor(g);
            return;
        }

        // Durum dairesi.
        let r = 48.0;
        let ring = Rect::new(w / 2.0 - r, cy - r, w / 2.0 + r, cy + r);
        match self.st.state {
            Some(State::On) => {
                g.fill(ring, r, ACCENT.alpha(0.12));
                g.stroke(ring, r, ACCENT, 2.0);
            }
            Some(State::Starting | State::Stopping) => g.stroke(ring, r, ACCENT.alpha(0.5), 2.0),
            _ => g.stroke(ring, r, LINE, 2.0),
        }
        g.text("\u{E7E8}", &g.f.icon_large, ring, if accent_live { ACCENT } else { FAINT });

        let (label, sub) = match (self.busy, self.st.state, self.st.mode) {
            (Some(b), _, _) => (b.to_string(), String::new()),
            (_, Some(State::On), Some(Mode::Server)) => {
                let lat = self.latency.lock().unwrap().map(|ms| format!(" · {ms} ms")).unwrap_or_default();
                ("Açık · sunucu üzerinden".into(), format!("{}{lat}", self.st.host.clone().unwrap_or_default()))
            }
            (_, Some(State::On), _) => ("Açık · DPI".into(), "Sunucusuz: DNS HTTPS üzerinden, el sıkışmaları bölünüyor".into()),
            (_, Some(State::Starting), _) => ("Bağlanıyor…".into(), "Ağ ya da sunucu bekleniyor".into()),
            (_, Some(State::Stopping), _) => ("Kapanıyor…".into(), String::new()),
            _ => ("Kapalı".into(), "Bilgisayar normal bağlantısını kullanıyor".into()),
        };
        text_center(g, &label, &g.f.heading, w / 2.0, cy + r + 14.0, cy + r + 44.0, TEXT);
        text_center(g, &sub, &g.f.small, w / 2.0, cy + r + 44.0, cy + r + 66.0, MUTED);

        // Mod seçici: sunucu · DPI · kapalı.
        let current = match (self.st.state, self.st.mode) {
            (Some(State::On | State::Starting), Some(Mode::Server)) => 0,
            (Some(State::On | State::Starting), Some(Mode::Dpi)) => 1,
            _ => 2,
        };
        let all = Rect::new(self.mode_rect(0).l, self.mode_rect(0).t, self.mode_rect(2).r, self.mode_rect(0).b);
        g.fill(all, 8.0, HOVER);
        for (k, name) in ["Sunucu", "DPI", "Kapalı"].iter().enumerate() {
            let mr = self.mode_rect(k);
            let inner = Rect::new(mr.l + 3.0, mr.t + 3.0, mr.r - 3.0, mr.b - 3.0);
            let dim = k == 0 && self.st.host.is_none();
            if k == current {
                g.fill(inner, 6.0, if k == 2 { SEL } else { ACCENT });
            } else if self.hover == Hit::Mode(k) {
                g.fill(inner, 6.0, SEL);
            }
            let c = match () {
                _ if k == current && k != 2 => ON_ACCENT,
                _ if k == current || self.hover == Hit::Mode(k) => TEXT,
                _ if dim => FAINT,
                _ => MUTED,
            };
            g.text(name, &g.f.button, mr, c);
        }

        if let Some((m, err)) = self.message.as_ref().map(|(m, e)| (m.clone(), *e)).or_else(|| {
            self.st.error.clone().filter(|_| self.busy.is_none()).map(|e| (format!("Son deneme başarısız: {e}"), true))
        }) {
            let t = self.mode_rect(0).b + 8.0;
            text_center(g, &m, &g.f.small, w / 2.0, t, t + 22.0, if err { RED } else { MUTED });
        }

        // Ayar satırları.
        let r0 = self.row(0);
        let sub = match (&self.st.host, &self.st.name) {
            (Some(h), Some(n)) => format!("{n} · {h}"),
            (Some(h), None) => h.clone(),
            _ => "Yok · sunucundan aldığın vless:// bağlantısını kopyalayıp buradan ekle".into(),
        };
        setting_row(g, r0, "Sunucu bağlantısı", &sub, 150.0);
        let lr = self.link_rect(g);
        let label = if self.st.host.is_some() { "Değiştir" } else { "Panodan ekle" };
        button(g, lr, label, None, None, self.hover == Hit::Link);

        let r1 = self.row(1);
        setting_row(g, r1, "Windows ile başlat", "Açılışta kaldığı moda döner · yönetici izni ister", 120.0);
        toggle(g, toggle_rect(r1.r, r1.cy()), self.st.autostart, ACCENT, true);
    }

    // --- Olaylar ---

    pub fn set_visible(&mut self, visible: bool) {
        if visible && !self.visible {
            self.refresh();
            self.ping();
        }
        self.visible = visible;
        unsafe {
            if visible {
                SetTimer(Some(self.hwnd), TIMER_POLL, 1000, None);
                SetTimer(Some(self.hwnd), TIMER_PING, 10_000, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_POLL);
                let _ = KillTimer(Some(self.hwnd), TIMER_PING);
                self.hover = Hit::None;
            }
        }
    }
}

impl ToolPage for Rabbithole {
    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
    }

    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        self.hit(g, x, y) != Hit::None
    }

    fn paint(&self, g: &Gfx) {
        Rabbithole::paint(self, g)
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
            Hit::Mode(0) => self.set_mode(Some(Mode::Server)),
            Hit::Mode(1) => self.set_mode(Some(Mode::Dpi)),
            Hit::Mode(_) => self.set_mode(None),
            Hit::Link => self.add_link(),
            Hit::Autostart => self.toggle_autostart(),
            Hit::Doctor => {
                if self.doctor_open() {
                    if !self.doctor.as_ref().is_some_and(|d| d.lock().unwrap().running) {
                        self.doctor = None;
                        self.redraw();
                    }
                } else {
                    self.run_doctor();
                }
            }
            Hit::None => {}
        }
    }

    fn wheel(&mut self, _g: &Gfx, _x: f32, _y: f32, delta: f32) {
        if let Some(d) = &self.doctor {
            let n = d.lock().unwrap().lines.len() as f32;
            let max = (n * LINE_H + 40.0 - (self.h - HEADER - 16.0)).max(0.0);
            self.scroll = (self.scroll - delta * LINE_H * 3.0).clamp(0.0, max);
            self.redraw();
        }
    }

    fn set_visible(&mut self, visible: bool) {
        Rabbithole::set_visible(self, visible)
    }
}
