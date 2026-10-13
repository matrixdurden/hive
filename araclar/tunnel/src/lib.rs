//! tunnel (VPN): ağ engellerini aşan tünel. Motoru bir Windows hizmeti (`rabbithole`); kullanıcılar
//! onu yönetici izni olmadan başlatıp durdurabilir. Sayfa durumu hizmet yöneticisinden ve
//! `%ProgramData%\tunnel` dosyalarından okur; yönetici isteyen işler (sunucu bağlantısı,
//! Windows ile başlat) ve ağ testi için `tunnel.exe` çalıştırılır.

#[macro_use]
extern crate ortak;

mod tool;
pub use tool::TOOL;

use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::Services::*;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use ortak::form::{self, Ctl, Form, Item, Row};
use ortak::gfx::{Gfx, Rect};
use ortak::log;
use ortak::ui::*;

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

/// Hizmet kurulu mu (winloom'da "kurulu" sayılmak için).
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
        .ok_or(t!("could not open the rabbithole service", "rabbithole hizmeti açılamadı"))?;
    let st = svc.status().ok_or(t!("could not read the service status", "hizmet durumu okunamadı"))?;
    if st.dwCurrentState != SERVICE_STOPPED {
        let mut ss = SERVICE_STATUS::default();
        unsafe {
            let _ = ControlService(svc.svc, SERVICE_CONTROL_STOP, &mut ss);
        }
        if !svc.wait(SERVICE_STOPPED, Duration::from_secs(15)) {
            return Err(t!("the VPN did not stop within 15 seconds", "VPN 15 saniyede durmadı").into());
        }
    }
    let Some(mode) = target else { return Ok(t!("turned off", "kapatıldı").into()) };
    unsafe { StartServiceW(svc.svc, Some(&[mode.arg()])) }.map_err(|e| format!("{} {}", t!("could not start:", "başlatılamadı:"), e.message()))?;
    Ok(String::new())
}

/// `tunnel.exe <args>`: yönetici isteyenler kendi UAC penceresini açar. Konsol açılmaz,
/// stdin kapalı (çift tıklama yolundaki "Enter'a bas" beklemesine düşmesin).
fn command(args: &[&str]) -> Command {
    let mut c = Command::new(exe());
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(CREATE_NO_WINDOW);
    c
}

pub fn run(args: &[&str]) -> Result<String, String> {
    let out = command(args).output().map_err(|e| format!("{} {e}", t!("could not run rabbithole:", "rabbithole çalıştırılamadı:")))?;
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.success() {
        Ok(String::new())
    } else {
        Err(err.lines().rev().find_map(|l| l.strip_prefix("error: ")).unwrap_or(err.trim()).to_string())
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    /// Sayfa kitindeki satırlar.
    Form(form::Hit),
    /// Sunucu bağlantısı yazılan kutu.
    Field,
    /// Kutudakini kaydet.
    Save,
}

impl Hit {
    /// Basılan ile bırakılan aynı yer mi (tıklama).
    fn same(self, other: Hit) -> bool {
        match (self, other) {
            (Hit::Form(a), Hit::Form(b)) => a.same(b),
            _ => self == other,
        }
    }
}

// Sayfa kitindeki satırlar.
const R_MODE: u32 = 0;
const R_SERVER: u32 = 1;
const R_LINK: u32 = 2;
const R_AUTOSTART: u32 = 3;
const R_TEST: u32 = 4;
/// Ağ testinin durum satırı (Kapat) ve raporu (kendi çizdiğimiz alan).
const D_STATUS: u32 = 10;
const D_REPORT: u32 = 11;

const LINK_MAX: usize = 4096;

#[derive(Default)]
struct Doctor {
    lines: Vec<String>,
    running: bool,
}

pub struct Tunnel {
    hwnd: HWND,
    st: Status,
    /// Sunucuya TCP bağlantı süresi (ms), sunucu modunda açıkken.
    latency: Arc<Mutex<Option<u32>>>,
    /// Süren iş ve sonucu.
    busy: Option<&'static str>,
    result: Arc<Mutex<Option<Result<String, String>>>>,
    message: Option<(String, bool)>,
    doctor: Option<Arc<Mutex<Doctor>>>,
    /// Sunucu bağlantısı (vless://) buraya yazılır ya da yapıştırılır.
    link: TextField,
    scroll: f32,
    hover: Hit,
    pressed: Hit,
    w: f32,
    h: f32,
    head_x: f32,
    head_r: f32,
    visible: bool,
}

impl Tunnel {
    pub fn start(hwnd: HWND) -> Self {
        let r = Self {
            hwnd,
            st: read_status(),
            latency: Arc::default(),
            busy: None,
            result: Arc::default(),
            message: None,
            doctor: None,
            link: TextField::new(LINK_MAX),
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
            self.message = Some((t!("Add the link from your server first", "Önce sunucundan aldığın bağlantıyı ekle").into(), true));
            self.redraw();
            return;
        }
        let label = if target.is_some() { t!("Connecting…", "Bağlanıyor…") } else { t!("Turning off…", "Kapatılıyor…") };
        self.spawn(label, move || switch(target));
    }

    /// Kutudaki bağlantıyı kaydeder (sunucu denenir); olduysa kutu boşalır.
    fn add_link(&mut self) {
        let link = self.link.text.trim().to_string();
        if !link.starts_with("vless://") {
            self.message = Some((
                t!(
                    "That is not a vless:// link: paste the link your server gave you",
                    "Bu bir vless:// bağlantısı değil: sunucunun verdiği bağlantıyı yapıştır"
                )
                .into(),
                true,
            ));
            self.redraw();
            return;
        }
        if self.busy.is_some() {
            return;
        }
        self.link.set("");
        self.link.blur();
        let added = t!("Server link saved", "Sunucu bağlantısı kaydedildi");
        self.spawn(t!("Trying the server…", "Sunucu deneniyor…"), move || run(&["client", &link]).map(|_| added.into()));
    }

    fn link_ok(&self) -> bool {
        self.link.text.trim().starts_with("vless://")
    }

    fn toggle_autostart(&mut self) {
        let arg = if self.st.autostart { "off" } else { "on" };
        self.spawn(t!("Applying…", "Ayarlanıyor…"), move || run(&["autostart", arg]));
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
                Err(e) => d.lock().unwrap().lines.push(format!("{} {e}", t!("could not run rabbithole:", "rabbithole çalıştırılamadı:"))),
            }
            d.lock().unwrap().running = false;
            post();
        });
        self.redraw();
    }

    pub fn doctor_line(&mut self) {
        // Yeni satırlar gelince en alta kaydır.
        self.scroll = self.max_scroll();
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

    /// Ağ testi bittiyse kapatır (sürerken kapanmaz).
    fn close_doctor(&mut self) {
        if !self.doctor.as_ref().is_some_and(|d| d.lock().unwrap().running) {
            self.doctor = None;
            self.scroll = 0.0;
            self.redraw();
        }
    }

    /// Mod seçicide seçili olan: sunucu · DPI · kapalı.
    fn current_mode(&self) -> usize {
        match (self.st.state, self.st.mode) {
            (Some(State::On | State::Starting), Some(Mode::Server)) => 0,
            (Some(State::On | State::Starting), Some(Mode::Dpi)) => 1,
            _ => 2,
        }
    }

    /// Son iş ya da son başlatmanın sonucu: (yazı, hata mı).
    fn note(&self) -> Option<(String, bool)> {
        self.message.clone().or_else(|| {
            self.st.error.clone().filter(|_| self.busy.is_none()).map(|e| (format!("{} {e}", t!("Last attempt failed:", "Son deneme başarısız:")), true))
        })
    }

    /// Sayfa: bağlantı (mod), sunucu (şimdiki, yeni bağlantı), başlangıç. Ağ testi açıkken onun raporu.
    fn form(&self) -> Form {
        let items = if let Some(d) = &self.doctor {
            let d = d.lock().unwrap();
            // Sürerken kapanmaz: Kapat test bitince gelir.
            let status = match d.running {
                true => Row::new(D_STATUS, t!("Testing the network…", "Ağ test ediliyor…"), t!("Takes 10–20 seconds", "10–20 saniye sürer"), Ctl::None),
                false => Row::new(D_STATUS, t!("Test finished", "Test bitti"), "", Ctl::Button(t!("Close", "Kapat").into())),
            };
            let mut items = vec![Item::Section(t!("Network test", "Ağ testi").into()), Item::Row(status)];
            if !d.lines.is_empty() {
                items.push(Item::Custom(D_REPORT, d.lines.len() as f32 * LINE_H + 32.0));
            }
            items
        } else {
            // Açıklamasız satır daha alçak: iş sürerken seçici yerinden oynamasın.
            let sub = Some(self.state_text().1).filter(|s| !s.is_empty()).unwrap_or_else(|| t!("Please wait…", "Biraz bekle…").into());
            let modes = vec![t!("Server", "Sunucu").into(), "DPI".into(), t!("Off", "Kapalı").into()];
            let mut items = vec![
                Item::Section(t!("Connection", "Bağlantı").into()),
                Item::Row(Row::new(R_MODE, t!("Mode", "Mod"), sub, Ctl::Seg(modes, self.current_mode()))),
                Item::Row(Row::new(
                    R_TEST,
                    t!("Network test", "Ağ testi"),
                    t!("Finds where the block is, step by step", "Engel nerede, adım adım bakar"),
                    Ctl::Button(t!("Test", "Test et").into()),
                )),
            ];
            if let Some((m, err)) = self.note() {
                items.push(Item::Note(m, err));
            }
            let now = match (&self.st.host, &self.st.name) {
                (Some(h), Some(n)) => format!("{} {n} · {h}", t!("Now:", "Şimdiki:")),
                (Some(h), None) => format!("{} {h}", t!("Now:", "Şimdiki:")),
                _ => t!("None yet · paste the vless:// link your server gave you", "Henüz yok · sunucunun verdiği vless:// bağlantısını yapıştır").into(),
            };
            // Bağlantı kutusu ve Kaydet düğmesi satırın sağında; başlığa yer kalır.
            let space = (self.w - 2.0 * PAD - 32.0 - 150.0).clamp(160.0, 520.0);
            items.extend([
                Item::Section(t!("Server", "Sunucu").into()),
                Item::Row(Row::new(R_SERVER, t!("Server", "Sunucu"), now, Ctl::None)),
                Item::Row(Row::new(R_LINK, t!("New link", "Yeni bağlantı"), "", Ctl::Space(space))),
                Item::Section(t!("Startup", "Başlangıç").into()),
                Item::Row(Row::new(
                    R_AUTOSTART,
                    t!("Start with Windows", "Windows ile başlat"),
                    t!("Comes back in the mode it was left · needs administrator", "Açılışta kaldığı moda döner · yönetici izni ister"),
                    Ctl::Toggle(self.st.autostart),
                )),
            ]);
            items
        };
        Form::new(items, PAD, self.w - PAD, HEADER + 16.0 - self.scroll)
    }

    fn max_scroll(&self) -> f32 {
        (self.form().bottom() - 24.0 + self.scroll + 16.0 - self.h).max(0.0)
    }

    /// Yeni bağlantı satırının sağı: kutu ve Kaydet düğmesi.
    fn link_rects(&self, g: &Gfx, f: &Form) -> Option<(Rect, Rect)> {
        let c = f.control(R_LINK)?;
        let save = button_rect_right(g, c.r, c.t, t!("Save", "Kaydet"), false);
        Some((Rect::new(c.l, c.t, save.l - 8.0, c.b), save))
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return Hit::None;
        }
        let f = self.form();
        if let Some((field, save)) = self.link_rects(g, &f) {
            if save.contains(x, y) {
                return Hit::Save;
            }
            if field.contains(x, y) {
                return Hit::Field;
            }
        }
        match f.hit(g, x, y) {
            form::Hit::None | form::Hit::Row(_) | form::Hit::Custom(_) => Hit::None,
            h => Hit::Form(h),
        }
    }

    // --- Çizim ---

    /// Ağ testinin satırları, kart içinde.
    fn paint_report(&self, g: &Gfx, r: Rect) {
        let Some(d) = &self.doctor else { return };
        g.fill(r, 8.0, pal().panel);
        g.stroke(r, 8.0, pal().line, 1.0);
        let mut y = r.t + 16.0;
        for line in &d.lock().unwrap().lines {
            if y + LINE_H >= HEADER && y <= self.h {
                let t = line.trim_start();
                let (c, f) = match () {
                    _ if t.starts_with('✓') => (pal().green.alpha(0.9), &g.f.text),
                    _ if t.starts_with('✗') => (pal().red, &g.f.text),
                    _ if !line.starts_with(' ') && !line.is_empty() => (pal().text, &g.f.strong),
                    _ => (pal().muted, &g.f.text),
                };
                g.text(line, f, Rect::new(r.l + 16.0, y, r.r - 16.0, y + LINE_H), c);
            }
            y += LINE_H;
        }
    }

    /// Durum: büyük satır ve altındaki açıklama (sayfada ve Ana sayfadaki kutucukta).
    fn state_text(&self) -> (String, String) {
        match (self.busy, self.st.state, self.st.mode) {
            (Some(b), _, _) => (b.to_string(), String::new()),
            (_, Some(State::On), Some(Mode::Server)) => {
                let lat = self.latency.lock().unwrap().map(|ms| format!(" · {ms} ms")).unwrap_or_default();
                (t!("On · through the server", "Açık · sunucu üzerinden").into(), format!("{}{lat}", self.st.host.clone().unwrap_or_default()))
            }
            (_, Some(State::On), _) => (
                t!("On · DPI", "Açık · DPI").into(),
                t!("No server: DNS over HTTPS, handshakes split", "Sunucusuz: DNS HTTPS üzerinden, el sıkışmaları bölünüyor").into(),
            ),
            (_, Some(State::Starting), _) => {
                (t!("Connecting…", "Bağlanıyor…").into(), t!("Waiting for the network or the server", "Ağ ya da sunucu bekleniyor").into())
            }
            (_, Some(State::Stopping), _) => (t!("Turning off…", "Kapanıyor…").into(), String::new()),
            _ => (t!("Off", "Kapalı").into(), t!("The computer uses its normal connection", "Bilgisayar normal bağlantısını kullanıyor").into()),
        }
    }

    pub fn paint(&self, g: &Gfx) {

        let f = self.form();
        let hover = if let Hit::Form(h) = self.hover { h } else { form::Hit::None };
        g.clip(Rect::new(0.0, HEADER, self.w, self.h), || {
            f.paint(g, hover, None);
            if let Some(r) = f.custom(D_REPORT) {
                self.paint_report(g, r);
            }
            if let Some((field, save)) = self.link_rects(g, &f) {
                self.link.paint(g, field, "vless://…");
                let primary = self.link_ok().then(accent);
                button(g, save, t!("Save", "Kaydet"), None, primary, self.hover == Hit::Save);
            }
        });
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
                self.link.blur();
            }
        }
    }
}

impl ToolPage for Tunnel {
    fn summary(&self) -> Option<(String, String)> {
        Some(self.state_text())
    }

    fn message(
        &mut self,
        _g: &mut ortak::gfx::Gfx,
        msg: u32,
        wp: windows::Win32::Foundation::WPARAM,
        _lp: windows::Win32::Foundation::LPARAM,
    ) -> Option<windows::Win32::Foundation::LRESULT> {
        use windows::Win32::UI::WindowsAndMessaging::WM_TIMER;
        match msg {
            WM_TIMER if matches!(wp.0, TIMER_POLL | TIMER_PING) => self.timer(wp.0),
            WM_DONE => self.done(),
            WM_LINE => self.doctor_line(),
            _ => return None,
        }
        Some(windows::Win32::Foundation::LRESULT(0))
    }
    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        self.hit(g, x, y) != Hit::None
    }

    fn paint(&self, g: &Gfx) {
        Tunnel::paint(self, g)
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
        let was = self.link.focused;
        if self.pressed == Hit::Field {
            self.link.focus(false);
            unsafe {
                let _ = SetFocus(Some(self.hwnd));
            }
        } else if self.pressed != Hit::Save {
            self.link.blur();
        }
        if was != self.link.focused {
            self.redraw();
        }
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        let hit = self.hit(g, x, y);
        if !hit.same(std::mem::replace(&mut self.pressed, Hit::None)) {
            return;
        }
        match hit {
            Hit::Form(form::Hit::Seg(R_MODE, 0)) => self.set_mode(Some(Mode::Server)),
            Hit::Form(form::Hit::Seg(R_MODE, 1)) => self.set_mode(Some(Mode::Dpi)),
            Hit::Form(form::Hit::Seg(R_MODE, _)) => self.set_mode(None),
            Hit::Form(form::Hit::Toggle(R_AUTOSTART)) => self.toggle_autostart(),
            Hit::Save => self.add_link(),
            Hit::Form(form::Hit::Button(R_TEST)) => self.run_doctor(),
            Hit::Form(form::Hit::Button(D_STATUS)) => self.close_doctor(),
            _ => {}
        }
    }

    fn key(&mut self, vk: u16) -> bool {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL};
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        match self.link.key(vk, ctrl) {
            FieldKey::Ignored => return false,
            FieldKey::Enter => self.add_link(),
            FieldKey::Escape => {
                if self.link.text.is_empty() {
                    self.link.blur();
                } else {
                    self.link.set("");
                }
            }
            FieldKey::Handled => {}
        }
        self.redraw();
        true
    }

    fn char(&mut self, c: char) -> bool {
        let used = self.link.char(c);
        if used {
            self.redraw();
        }
        used
    }

    fn kill_focus(&mut self) {
        if self.link.focused {
            self.link.blur();
            self.redraw();
        }
    }

    fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        self.scroll = (self.scroll - delta * 60.0).clamp(0.0, self.max_scroll());
        self.hover = self.hit(g, x, y);
        self.redraw();
    }

    fn set_visible(&mut self, visible: bool) {
        Tunnel::set_visible(self, visible)
    }
}
