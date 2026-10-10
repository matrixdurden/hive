//! Otomatik güncelleme. Arka planda arada bir GitHub'daki son yayına bakar; bu sürümden yeniyse
//! `hive.exe`'yi indirir, yanındaki SHA-256 ile doğrular ve `guncelleme\` klasörüne koyar. Hazır
//! güncelleme hive'ın bir sonraki açılışında (oturum açınca) kendiliğinden kurulur, Ayarlar'dan
//! hemen de kurulabilir. Kurmak indirilen exe'yi `--install` ile çalıştırmaktır: çalışan hive'ı
//! kapatır, kendini yerine yazar, araçlarla birlikte yeniden başlatır (ilk kurulumla aynı yol).
//!
//! Geliştirme kopyası (WSL'den çalışan) güncellenmez.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::{log, net, util};

const REPO: &str = "matrixdurden/hive";
/// Açılıştan sonra ağın oturması beklenir; sonra altı saatte bir.
const FIRST: Duration = Duration::from_secs(45);
const EVERY: Duration = Duration::from_secs(6 * 3600);

/// Durum değişti (hive'ın penceresine): Ayarlar yeniden çizilir.
pub const WM_UPDATE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 6;

#[derive(Clone, PartialEq, Debug)]
pub enum Status {
    /// Henüz bakılmadı.
    Idle,
    Checking,
    /// Son yayın bu sürüm (ya da daha eski).
    Current,
    /// Bu sürüm indirildi, doğrulandı, kurulmayı bekliyor.
    Ready(String),
    Failed(String),
}

static STATUS: Mutex<Status> = Mutex::new(Status::Idle);
/// Ayarlar'daki "Otomatik güncelle".
static ENABLED: AtomicBool = AtomicBool::new(true);
static BUSY: AtomicBool = AtomicBool::new(false);

pub fn status() -> Status {
    STATUS.lock().unwrap().clone()
}

fn dir() -> PathBuf {
    util::data_dir().join("guncelleme")
}

fn file(version: &str) -> PathBuf {
    dir().join(format!("hive-{version}.exe"))
}

/// "v1.2.3" ya da "1.2.3" → (1, 2, 3).
fn parse(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u32>().ok());
    Some((it.next()??, it.next()??, it.next().flatten().unwrap_or(0)))
}

fn newer(v: &str) -> bool {
    match (parse(v), parse(env!("CARGO_PKG_VERSION"))) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// İndirilmiş, bu sürümden yeni güncelleme (en yenisi).
fn pending() -> Option<(String, PathBuf)> {
    let rd = std::fs::read_dir(dir()).ok()?;
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let v = name.strip_prefix("hive-")?.strip_suffix(".exe")?.to_string();
            newer(&v).then(|| (v, e.path()))
        })
        .max_by_key(|(v, _)| parse(v))
}

/// Son yayının etiketi (GitHub API).
fn latest_tag() -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = net::download(&url).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&body);
    // `"tag_name":"v0.4.0"`: tek alan için JSON çözücü gerekmez.
    let rest = text.split_once("\"tag_name\"").ok_or("tag_name yok")?.1;
    let tag = rest.split('"').nth(1).ok_or("tag_name okunamadı")?;
    Ok(tag.to_string())
}

/// Son yayına bakar; yeniyse indirip doğrular.
fn check() -> Result<Status, String> {
    let tag = latest_tag()?;
    let version = tag.trim_start_matches('v').to_string();
    if !newer(&version) {
        return Ok(Status::Current);
    }
    if file(&version).is_file() {
        return Ok(Status::Ready(version));
    }
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    let exe = net::download(&format!("{base}/hive.exe")).map_err(|e| e.to_string())?;
    let sum = net::download(&format!("{base}/hive.exe.sha256")).map_err(|e| e.to_string())?;
    let expected = String::from_utf8_lossy(&sum).split_whitespace().next().unwrap_or_default().to_lowercase();
    let actual = net::sha256(&exe).map_err(|e| e.to_string())?;
    if expected.is_empty() || actual != expected {
        return Err(t!("the download could not be verified", "indirilen dosya doğrulanamadı").into());
    }
    // Eski indirmeler silinir; yarım dosya kalmasın diye önce geçici adla yazılır.
    let _ = std::fs::remove_dir_all(dir());
    std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    let tmp = dir().join("indiriliyor.tmp");
    std::fs::write(&tmp, &exe).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, file(&version)).map_err(|e| e.to_string())?;
    log!("güncelleme indirildi: {version}");
    Ok(Status::Ready(version))
}

fn set(s: Status, hwnd: usize) {
    *STATUS.lock().unwrap() = s;
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_UPDATE, WPARAM(0), LPARAM(0));
    }
}

/// Şimdi bakar (arka planda); zaten bakılıyorsa bir şey yapmaz.
pub fn check_now(hwnd: HWND) {
    if util::installed_copy().is_none() || BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    let hwnd = hwnd.0 as usize;
    std::thread::spawn(move || {
        set(Status::Checking, hwnd);
        let s = check().unwrap_or_else(|e| {
            log!("güncelleme denetlenemedi: {e}");
            Status::Failed(e)
        });
        set(s, hwnd);
        BUSY.store(false, Ordering::SeqCst);
    });
}

/// Açılışta: eski indirmeleri temizler, arada bir bakan iş parçacığını başlatır.
pub fn start(hwnd: HWND, enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    if pending().is_none() {
        let _ = std::fs::remove_dir_all(dir());
    }
    if util::installed_copy().is_none() {
        return;
    }
    let h = hwnd.0 as usize;
    std::thread::spawn(move || {
        std::thread::sleep(FIRST);
        loop {
            if ENABLED.load(Ordering::Relaxed) {
                check_now(HWND(h as *mut _));
            }
            std::thread::sleep(EVERY);
        }
    });
}

pub fn set_enabled(hwnd: HWND, on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        check_now(hwnd);
    }
}

/// Hazır güncellemeyi kurar: indirilen exe çalışan hive'ı kapatıp yerine geçer. Başarıyla
/// başlatılırsa `true` (bu süreç birazdan kapatılır).
pub fn install(hidden: bool) -> bool {
    let Some((version, exe)) = pending() else { return false };
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--install");
    if hidden {
        cmd.arg("--hidden");
    }
    match cmd.creation_flags(0x0000_0008).spawn() {
        Ok(_) => {
            log!("güncelleme kuruluyor: {version}");
            true
        }
        Err(e) => {
            log!("güncelleme başlatılamadı: {e}");
            false
        }
    }
}

/// Açılışta: indirilmiş güncelleme varsa onu kurar (bu süreç başlamadan çıkar). Yalnızca bir
/// kez denenir: kurulum bir önceki açılışta da denendiyse ve hâlâ bu eski sürüm açılıyorsa
/// indirilen dosya bozuktur, silinir, hive olduğu gibi açılır.
pub fn install_pending_at_start(hidden: bool) -> bool {
    if util::installed_copy().is_none() || pending().is_none() {
        return false;
    }
    let tried = dir().join("denendi");
    if tried.exists() {
        log!("güncelleme kurulamadı, indirilen dosya siliniyor");
        let _ = std::fs::remove_dir_all(dir());
        return false;
    }
    let _ = std::fs::write(&tried, "");
    install(hidden)
}

/// `hive --update-test`: son yayına bakar, yeniyse indirip doğrular (kurmaz).
pub fn probe() -> String {
    let tag = latest_tag();
    let mut out = format!("bu sürüm: {}\nson yayın: {tag:?}\n", env!("CARGO_PKG_VERSION"));
    out += &match check() {
        Ok(s) => format!("sonuç: {s:?}\n"),
        Err(e) => format!("hata: {e}\n"),
    };
    if let Some((v, p)) = pending() {
        out += &format!("bekleyen: {v} ({})\n", p.display());
    }
    out
}
