//! Araç kataloğu ve eklenti gibi kurma / kaldırma. Kurulum işleri arka plan iş parçacığında
//! çalışır (yönetici izni, indirme, motoru durdurma beklemesi arayüzü kilitlemesin).
//!
//! - lyrebird: motoru hive'ın içinde; kurmak mikrofon efektini bağlamaktır (yönetici).
//! - cheshire: motoru hive'ın içinde gömülü; kurmak exe'yi yazmaktır.
//! - rabbithole: GitHub'daki son sürüm indirilir, SHA-256'sı doğrulanır, kendi kurulumu çalışır.
//! - dormouse: motoru hive'ın içinde; kurmak mevcut güç ayarlarını yedekleyip uygulamaların GPU tercihini yazmaktır.
//! - tweedle: motoru hive'ın içinde; kurmak yalnızca kısayol dosyasını yazmaktır.
//! - hatter: motoru (dock) hive'ın içinde; kurmak ayarları ve dock'un ilk listesini yazmaktır.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RegDeleteKeyValueW};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId};
use windows::core::{PCWSTR, w};

use crate::{cheshire, dormouse, hatter, log, lyrebird, net, rabbithole, shell, tweedle, util};

pub const LYREBIRD: usize = 0;
pub const CHESHIRE: usize = 1;
pub const RABBITHOLE: usize = 2;
pub const DORMOUSE: usize = 3;
pub const TWEEDLE: usize = 4;
pub const HATTER: usize = 5;
pub const COUNT: usize = 6;

pub struct Tool {
    pub id: &'static str,
    pub name: &'static str,
    pub accent: u32,
    /// [İngilizce, Türkçe]; `tagline()` seçili dildekini verir.
    tagline: [&'static str; 2],
    /// Araçlar sayfasında kurulumun ne getirdiği: [İngilizce, Türkçe].
    note: [&'static str; 2],
    /// (piksel, PNG) — çizilecek boyuta en yakın olan seçilir.
    pub icons: &'static [(u32, &'static [u8])],
}

impl Tool {
    pub fn tagline(&self) -> &'static str {
        t!(self.tagline[0], self.tagline[1])
    }

    pub fn note(&self) -> &'static str {
        t!(self.note[0], self.note[1])
    }
}

macro_rules! icons {
    ($name:literal) => {
        &[
            (24, include_bytes!(concat!("../assets/araclar/", $name, "-24.png"))),
            (32, include_bytes!(concat!("../assets/araclar/", $name, "-32.png"))),
            (40, include_bytes!(concat!("../assets/araclar/", $name, "-40.png"))),
            (48, include_bytes!(concat!("../assets/araclar/", $name, "-48.png"))),
            (64, include_bytes!(concat!("../assets/araclar/", $name, "-64.png"))),
            (128, include_bytes!(concat!("../assets/araclar/", $name, "-128.png"))),
        ]
    };
}

pub static TOOLS: [Tool; COUNT] = [
    Tool {
        id: "lyrebird",
        name: "lyrebird",
        accent: 0xfbbf24,
        tagline: ["A soundboard that plays straight into your microphone", "Sesi doğrudan mikrofona veren soundboard"],
        note: ["Adds an effect to your microphone · needs admin", "Mikrofonuna bir ses efekti takar · yönetici izni ister"],
        icons: icons!("lyrebird"),
    },
    Tool {
        id: "cheshire",
        name: "cheshire",
        accent: 0xf472b6,
        tagline: ["Live wallpapers drawn by your GPU", "GPU ile çizilen canlı duvar kâğıdı"],
        note: ["Comes with hive · no internet needed", "hive ile birlikte gelir · internet gerekmez"],
        icons: icons!("cheshire"),
    },
    Tool {
        id: "rabbithole",
        name: "rabbithole",
        accent: 0xa78bfa,
        tagline: ["Tunnels your whole computer past network blocks", "Bütün bilgisayarı ağ engellerinin ötesine geçiren tünel"],
        note: ["Downloaded from GitHub (~40 MB) · needs admin", "GitHub'dan indirilir (~40 MB) · yönetici izni ister"],
        icons: icons!("rabbithole"),
    },
    Tool {
        id: "dormouse",
        name: "dormouse",
        accent: 0x34d399,
        tagline: ["Three gears for your laptop's battery", "Dizüstünün pili için üç vites"],
        note: ["Comes with hive · laptops only", "hive ile birlikte gelir · yalnızca dizüstü"],
        icons: icons!("dormouse"),
    },
    Tool {
        id: "tweedle",
        name: "tweedle",
        accent: 0x38bdf8,
        tagline: ["Audio outputs and inputs, one key away", "Ses çıkışları ve girişleri, tek tuş uzağında"],
        note: ["Comes with hive · no internet needed", "hive ile birlikte gelir · internet gerekmez"],
        icons: icons!("tweedle"),
    },
    Tool {
        id: "hatter",
        name: "hatter",
        accent: hatter::ACCENT,
        tagline: ["A dock in place of the taskbar", "Görev çubuğunun yerine bir dock"],
        note: ["Comes with hive · the taskbar comes back when removed", "hive ile birlikte gelir · kaldırınca görev çubuğu geri gelir"],
        icons: icons!("hatter"),
    },
];

/// Araç bu bilgisayarda kurulu mu.
pub fn installed(i: usize) -> bool {
    match i {
        LYREBIRD => lyrebird::installed(),
        CHESHIRE => cheshire::installed(),
        DORMOUSE => dormouse::installed(),
        TWEEDLE => tweedle::installed(),
        HATTER => hatter::installed(),
        _ => rabbithole::installed(),
    }
}

/// Kurulum ya da kaldırma sırasında düğmede görünen yazı.
pub fn busy_label(i: usize, install: bool) -> &'static str {
    match (i, install) {
        (RABBITHOLE, true) => t!("Downloading…", "İndiriliyor…"),
        (_, true) => t!("Installing…", "Kuruluyor…"),
        _ => t!("Removing…", "Kaldırılıyor…"),
    }
}

/// Arka planda çalışır. Kaldırma, aracın bıraktığı her izi silip denetlemeden başarılı sayılmaz.
pub fn run(i: usize, install: bool) -> Result<(), String> {
    match (i, install) {
        (LYREBIRD, true) => lyrebird_setup(true),
        (CHESHIRE, true) => cheshire::install(),
        (DORMOUSE, true) => dormouse::install(),
        (TWEEDLE, true) => tweedle::install(),
        (HATTER, true) => hatter::install(),
        (_, true) => rabbithole_install(),
        (_, false) => uninstall(i),
    }
}

fn uninstall(i: usize) -> Result<(), String> {
    match i {
        LYREBIRD => {
            lyrebird_setup(false)?;
            // Kullanıcı tarafı: ses listesi, eski tek başına sürüm, onun Windows ile başlaması.
            let _ = std::fs::remove_dir_all(lyrebird::data_dir());
            let _ = std::fs::remove_dir_all(util::local_programs().join("lyrebird"));
            unsafe {
                let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Run"), w!("lyrebird"));
            }
        }
        CHESHIRE => cheshire::uninstall()?,
        DORMOUSE => dormouse::uninstall()?,
        TWEEDLE => tweedle::uninstall()?,
        HATTER => hatter::uninstall()?,
        _ => {
            rabbithole::run(&["remove"])?;
            // Program Files'taki exe kendini silemez: rabbithole arkasında birkaç saniye içinde
            // klasörü silen bir komut bırakır.
            let start = Instant::now();
            while rabbithole::install_dir().exists() && start.elapsed() < Duration::from_secs(40) {
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    shell::remove_tool(i);
    let left = leftovers(i);
    if left.is_empty() {
        Ok(())
    } else {
        log!("{} kalıntıları: {left:?}", TOOLS[i].name);
        Err(format!("{} {}", t!("not fully removed, left behind:", "tam kaldırılamadı, kalanlar:"), left.join(" · ")))
    }
}

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Aracın bu bilgisayarda bıraktığı her iz; kurulu değilken boş olmalı. COM başlatılmış iş
/// parçacığında çağrılmalı (lyrebird mikrofonları dolaşır).
pub fn leftovers(i: usize) -> Vec<String> {
    let exists = |p: std::path::PathBuf| p.exists().then(|| p.display().to_string());
    let mut left = match i {
        LYREBIRD => {
            let mut l = lyrebird::install::leftovers();
            l.extend([lyrebird::data_dir(), util::local_programs().join("lyrebird")].into_iter().filter_map(exists));
            if util::reg_value_exists(HKEY_CURRENT_USER, RUN, "lyrebird") {
                l.push(format!(r"HKCU\{RUN}\lyrebird"));
            }
            l
        }
        CHESHIRE => cheshire::leftovers(),
        DORMOUSE => dormouse::leftovers(),
        TWEEDLE => tweedle::leftovers(),
        HATTER => hatter::leftovers(),
        _ => {
            let mut l: Vec<String> =
                [rabbithole::install_dir(), rabbithole::data_dir()].into_iter().filter_map(exists).collect();
            if rabbithole::installed() {
                l.push(t!("rabbithole service", "rabbithole hizmeti").into());
            }
            let env = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
            let path = util::reg_string(HKEY_LOCAL_MACHINE, env, "Path").unwrap_or_default().to_lowercase();
            if path.split(';').any(|p| p.trim_end_matches('\\').ends_with(r"\rabbithole")) {
                l.push(t!("rabbithole in the system PATH", "sistem PATH'inde rabbithole").into());
            }
            l
        }
    };
    left.extend(shell::tool_leftovers(i));
    left
}

fn lyrebird_setup(install: bool) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let arg = if install { lyrebird::ARG_INSTALL } else { lyrebird::ARG_UNINSTALL };
    match lyrebird::install::run_elevated(arg) {
        Ok(true) => Ok(()),
        Ok(false) => Err(t!("the microphone effect could not be set up · see the log", "mikrofon efekti ayarlanamadı · ayrıntılar logda").into()),
        Err(_) => Err(t!("administrator permission was not given", "yönetici izni verilmedi").into()),
    }
}

/// Son sürümü indirir, doğrular ve `rabbithole dpi` ile kurar: hizmet kurulur, sunucusuz modda
/// açılır (rabbithole'un kendi kurulumu da bağlantı verilmezse böyle yapar).
fn rabbithole_install() -> Result<(), String> {
    let asset = "rabbithole-windows-amd64.exe";
    let base = "https://github.com/matrixdurden/rabbithole/releases/latest/download/";
    let sums = net::download(&format!("{base}checksums.txt")).map_err(|e| e.to_string())?;
    let expected = String::from_utf8_lossy(&sums)
        .lines()
        .find(|l| l.trim_end().ends_with(asset))
        .and_then(|l| l.split_whitespace().next())
        .map(str::to_ascii_lowercase)
        .ok_or(t!("the exe is missing from checksums.txt", "checksums.txt içinde exe yok"))?;
    let exe = net::download(&format!("{base}{asset}")).map_err(|e| e.to_string())?;
    let actual = net::sha256(&exe).map_err(|e| e.to_string())?;
    if !exe.starts_with(b"MZ") || actual != expected {
        return Err(t!("the download could not be verified (SHA-256 mismatch)", "indirilen dosya doğrulanamadı (SHA-256 tutmadı)").into());
    }
    let tmp = std::env::temp_dir().join("rabbithole-kurulum.exe");
    std::fs::write(&tmp, &exe).map_err(|e| e.to_string())?;
    log!("rabbithole indirildi, kuruluyor");
    let out = Command::new(&tmp)
        .arg("dpi")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&tmp);
    let out = out?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().rev().find_map(|l| l.strip_prefix("error: ")).unwrap_or(err.trim()).to_string())
    }
}

/// lyrebird artık hive içinde çalışıyor: ayrı kopya açıksa kapatılır (ortak belleğe aynı anda
/// iki yazar olamaz), Windows ile başlaması kaldırılır.
pub fn retire_standalone_lyrebird() {
    unsafe {
        if let Ok(hwnd) = FindWindowW(w!("Lyrebird"), PCWSTR::null()) {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                let _ = TerminateProcess(h, 0);
                let _ = CloseHandle(h);
                log!("ayrı lyrebird kapatıldı (pid {pid})");
            }
        }
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Run"), w!("lyrebird"));
    }
}
