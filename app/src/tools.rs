//! Araç kataloğu ve eklenti gibi kurma / kaldırma. Kurulum işleri arka plan iş parçacığında
//! çalışır (yönetici izni, indirme, motoru durdurma beklemesi arayüzü kilitlemesin).
//!
//! - soundboard: motoru hive'ın içinde; kurmak mikrofon efektini bağlamaktır (yönetici).
//! - wallpaper: motoru hive'ın içinde gömülü; kurmak exe'yi yazmaktır.
//! - tunnel: GitHub'daki son sürüm indirilir, SHA-256'sı doğrulanır, kendi kurulumu çalışır.
//! - battery: motoru hive'ın içinde; kurmak mevcut güç ayarlarını yedekleyip uygulamaların GPU tercihini yazmaktır.
//! - audio: motoru hive'ın içinde; kurmak yalnızca kısayol dosyasını yazmaktır.
//! - dock: motoru (dock) hive'ın içinde; kurmak ayarları ve dock'un ilk listesini yazmaktır.
//! - music: motoru (widget) hive'ın içinde; kurmak yalnızca ayar dosyasını yazmaktır.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RegDeleteKeyValueW};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId};
use windows::core::{PCWSTR, w};

use crate::{wallpaper, battery, dock, log, soundboard, music, net, tunnel, shell, audio, util};

pub const SOUNDBOARD: usize = 0;
pub const WALLPAPER: usize = 1;
pub const TUNNEL: usize = 2;
pub const BATTERY: usize = 3;
pub const AUDIO: usize = 4;
pub const DOCK: usize = 5;
pub const MUSIC: usize = 6;
pub const COUNT: usize = 7;

pub struct Tool {
    pub id: &'static str,
    /// Görünen ad (İngilizce, Türkçe): aracın ne işe yaradığı. Kod adı `id`'de kalır.
    pub names: [&'static str; 2],
    /// [İngilizce, Türkçe]; `tagline()` seçili dildekini verir.
    tagline: [&'static str; 2],
    /// Araçlar sayfasında kurulumun ne getirdiği: [İngilizce, Türkçe].
    note: [&'static str; 2],
    /// (piksel, PNG) — çizilecek boyuta en yakın olan seçilir.
    pub icons: &'static [(u32, &'static [u8])],
}

impl Tool {
    pub fn name(&self) -> &'static str {
        t!(self.names[0], self.names[1])
    }

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
        id: "soundboard",
        names: ["Soundboard", "Soundboard"],
        tagline: ["A soundboard that plays straight into your microphone", "Sesi doğrudan mikrofona veren soundboard"],
        note: ["Adds an effect to your microphone · needs admin", "Mikrofonuna bir ses efekti takar · yönetici izni ister"],
        icons: icons!("soundboard"),
    },
    Tool {
        id: "wallpaper",
        names: ["Wallpaper", "Duvar kâğıdı"],
        tagline: ["Live wallpapers drawn by your GPU", "GPU ile çizilen canlı duvar kâğıdı"],
        note: ["No internet needed", "İnternet gerekmez"],
        icons: icons!("wallpaper"),
    },
    Tool {
        id: "tunnel",
        names: ["Tunnel", "Tünel"],
        tagline: ["Tunnels your whole computer past network blocks", "Bütün bilgisayarı ağ engellerinin ötesine geçiren tünel"],
        note: ["Downloaded from GitHub when turned on (~40 MB) · needs admin", "Açınca GitHub'dan indirilir (~40 MB) · yönetici izni ister"],
        icons: icons!("tunnel"),
    },
    Tool {
        id: "battery",
        names: ["Battery", "Pil"],
        tagline: ["Three gears for your laptop's battery", "Dizüstünün pili için üç vites"],
        note: ["Laptops only", "Yalnızca dizüstü"],
        icons: icons!("battery"),
    },
    Tool {
        id: "audio",
        names: ["Audio devices", "Ses aygıtları"],
        tagline: ["Audio outputs and inputs, one key away", "Ses çıkışları ve girişleri, tek tuş uzağında"],
        note: ["No internet needed", "İnternet gerekmez"],
        icons: icons!("audio"),
    },
    Tool {
        id: "dock",
        names: ["Dock", "Dock"],
        tagline: ["A dock in place of the taskbar", "Görev çubuğunun yerine bir dock"],
        note: ["Replaces the taskbar · turn it off and the taskbar is back", "Görev çubuğunun yerine geçer · kapatınca görev çubuğu geri gelir"],
        icons: icons!("dock"),
    },
    Tool {
        id: "music",
        names: ["Music widget", "Müzik widget'ı"],
        tagline: ["What Spotify is playing, right next to the dock", "Spotify'da çalan, dock'un hemen yanında"],
        note: ["No Spotify login needed", "Spotify'a giriş gerekmez"],
        icons: icons!("music"),
    },
];

/// Araçların eski kod adları ve bugünkü kimlikleri: kayıtlı sekme, eski kısayollar ve veri
/// klasörleri bunlarla taşınır.
const RENAMED: [(&str, &str); 7] = [
    ("lyrebird", "soundboard"),
    ("cheshire", "wallpaper"),
    ("rabbithole", "tunnel"),
    ("dormouse", "battery"),
    ("tweedle", "audio"),
    ("hatter", "dock"),
    ("mockturtle", "music"),
];

/// Kimliği (ya da eski kod adı) bu olan araç.
pub fn find(id: &str) -> Option<usize> {
    let id = RENAMED.iter().find(|(old, _)| old.eq_ignore_ascii_case(id)).map_or(id, |(_, new)| new);
    TOOLS.iter().position(|t| t.id.eq_ignore_ascii_case(id))
}

/// Eski kod adlı veri klasörleri yeni adlarına taşınır (araçların ayarları kaybolmasın), eski
/// adlı kısayol simgeleri silinir (yenileri kısayollarla yazılır).
pub fn migrate() {
    for (old, new) in RENAMED {
        let (o, n) = (util::data_dir().join(old), util::data_dir().join(new));
        if o.is_dir() && !n.exists() {
            match std::fs::rename(&o, &n) {
                Ok(()) => log!("{old} → {new} taşındı"),
                Err(e) => log!("{old} → {new} taşınamadı: {e}"),
            }
        }
        let _ = std::fs::remove_file(util::data_dir().join("ikonlar").join(format!("{old}.ico")));
    }
}

/// Aracın motorunu ve sayfasını başlatır (araç açıkken hive'ın açılışında ya da açılınca).
pub fn start(i: usize, hwnd: windows::Win32::Foundation::HWND) -> Box<dyn crate::ui::ToolPage> {
    match i {
        SOUNDBOARD => {
            retire_standalone_lyrebird();
            Box::new(soundboard::Soundboard::start(hwnd))
        }
        WALLPAPER => Box::new(wallpaper::Wallpaper::start(hwnd)),
        BATTERY => Box::new(battery::Battery::start(hwnd)),
        AUDIO => Box::new(audio::Audio::start(hwnd)),
        DOCK => Box::new(dock::Dock::start(hwnd)),
        MUSIC => Box::new(music::Music::start(hwnd)),
        _ => Box::new(tunnel::Tunnel::start(hwnd)),
    }
}

/// Araç bu bilgisayarda kurulu mu.
pub fn installed(i: usize) -> bool {
    match i {
        SOUNDBOARD => soundboard::installed(),
        WALLPAPER => wallpaper::installed(),
        BATTERY => battery::installed(),
        AUDIO => audio::installed(),
        DOCK => dock::installed(),
        MUSIC => music::installed(),
        _ => tunnel::installed(),
    }
}

/// Kurulum ya da kaldırma sırasında düğmede görünen yazı.
pub fn busy_label(i: usize, install: bool) -> &'static str {
    match (i, install) {
        (TUNNEL, true) => t!("Downloading…", "İndiriliyor…"),
        (_, true) => t!("Turning on…", "Açılıyor…"),
        _ => t!("Turning off…", "Kapatılıyor…"),
    }
}

/// Arka planda çalışır. Kaldırma, aracın bıraktığı her izi silip denetlemeden başarılı sayılmaz.
pub fn run(i: usize, install: bool) -> Result<(), String> {
    match (i, install) {
        (SOUNDBOARD, true) => soundboard_setup(true),
        (WALLPAPER, true) => wallpaper::install(),
        (BATTERY, true) => battery::install(),
        (AUDIO, true) => audio::install(),
        (DOCK, true) => dock::install(),
        (MUSIC, true) => music::install(),
        (_, true) => rabbithole_install(),
        (_, false) => uninstall(i),
    }
}

fn uninstall(i: usize) -> Result<(), String> {
    match i {
        SOUNDBOARD => {
            soundboard_setup(false)?;
            // Kullanıcı tarafı: ses listesi, eski tek başına sürüm, onun Windows ile başlaması.
            let _ = std::fs::remove_dir_all(soundboard::data_dir());
            let _ = std::fs::remove_dir_all(util::local_programs().join("lyrebird"));
            unsafe {
                let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Run"), w!("lyrebird"));
            }
        }
        WALLPAPER => wallpaper::uninstall()?,
        BATTERY => battery::uninstall()?,
        AUDIO => audio::uninstall()?,
        DOCK => dock::uninstall()?,
        MUSIC => music::uninstall()?,
        _ => {
            tunnel::run(&["remove"])?;
            // Program Files'taki exe kendini silemez: tunnel arkasında birkaç saniye içinde
            // klasörü silen bir komut bırakır.
            let start = Instant::now();
            while tunnel::install_dir().exists() && start.elapsed() < Duration::from_secs(40) {
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    shell::remove_tool(i);
    let left = leftovers(i);
    if left.is_empty() {
        Ok(())
    } else {
        log!("{} kalıntıları: {left:?}", TOOLS[i].name());
        Err(format!("{} {}", t!("not fully removed, left behind:", "tam kaldırılamadı, kalanlar:"), left.join(" · ")))
    }
}

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Aracın bu bilgisayarda bıraktığı her iz; kurulu değilken boş olmalı. COM başlatılmış iş
/// parçacığında çağrılmalı (soundboard mikrofonları dolaşır).
pub fn leftovers(i: usize) -> Vec<String> {
    let exists = |p: std::path::PathBuf| p.exists().then(|| p.display().to_string());
    let mut left = match i {
        SOUNDBOARD => {
            let mut l = soundboard::install::leftovers();
            l.extend([soundboard::data_dir(), util::local_programs().join("lyrebird")].into_iter().filter_map(exists));
            if util::reg_value_exists(HKEY_CURRENT_USER, RUN, "lyrebird") {
                l.push(format!(r"HKCU\{RUN}\lyrebird"));
            }
            l
        }
        WALLPAPER => wallpaper::leftovers(),
        BATTERY => battery::leftovers(),
        AUDIO => audio::leftovers(),
        DOCK => dock::leftovers(),
        MUSIC => music::leftovers(),
        _ => {
            let mut l: Vec<String> =
                [tunnel::install_dir(), tunnel::data_dir()].into_iter().filter_map(exists).collect();
            if tunnel::installed() {
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

fn soundboard_setup(install: bool) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let arg = if install { soundboard::ARG_INSTALL } else { soundboard::ARG_UNINSTALL };
    match soundboard::install::run_elevated(arg) {
        Ok(true) => Ok(()),
        Ok(false) => Err(t!("the microphone effect could not be set up · see the log", "mikrofon efekti ayarlanamadı · ayrıntılar logda").into()),
        Err(_) => Err(t!("administrator permission was not given", "yönetici izni verilmedi").into()),
    }
}

/// Son sürümü indirir, doğrular ve `tunnel dpi` ile kurar: hizmet kurulur, sunucusuz modda
/// açılır (tunnel'un kendi kurulumu da bağlantı verilmezse böyle yapar).
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

/// soundboard artık hive içinde çalışıyor: ayrı kopya açıksa kapatılır (ortak belleğe aynı anda
/// iki yazar olamaz), Windows ile başlaması kaldırılır.
pub fn retire_standalone_lyrebird() {
    unsafe {
        if let Ok(hwnd) = FindWindowW(w!("Lyrebird"), PCWSTR::null()) {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                let _ = TerminateProcess(h, 0);
                let _ = CloseHandle(h);
                log!("ayrı soundboard kapatıldı (pid {pid})");
            }
        }
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Run"), w!("lyrebird"));
    }
}
