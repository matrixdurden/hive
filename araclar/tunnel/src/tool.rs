//! VPN'in (tunnel) winloom'a tanımı. Motoru rabbithole ayrı depoda: açınca GitHub'daki son sürümü
//! indirilir, SHA-256'sı doğrulanır, kendi kurulumu çalışır.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;

use ortak::tool::Tool;
use ortak::{log, net, util};

pub static TOOL: Tool = Tool {
    id: "tunnel",
    old_ids: &["rabbithole"],
    names: ["VPN", "VPN"],
    tagline: ["A VPN that takes your whole computer past network blocks", "Bütün bilgisayarı ağ engellerinin ötesine geçiren VPN"],
    note: [
        "Downloaded from GitHub when turned on (~40 MB) · needs admin",
        "Açınca GitHub'dan indirilir (~40 MB) · yönetici izni ister",
    ],
    off_note: [
        "The tunnel closes; the service, network adapter and your server link are deleted.",
        "Tünel kapanır; hizmet, ağ bağdaştırıcısı ve sunucu bağlantın silinir.",
    ],
    downloads: true,
    glyph: "\u{E705}",
    icons: ortak::icons!("tunnel"),
    installed: crate::installed,
    install,
    uninstall,
    leftovers,
    start: |hwnd| Box::new(crate::Tunnel::start(hwnd)),
    ..Tool::BASE
};

/// Son sürümü indirir, doğrular ve `tunnel dpi` ile kurar: hizmet kurulur, sunucusuz modda
/// açılır (rabbithole'un kendi kurulumu da bağlantı verilmezse böyle yapar).
fn install() -> Result<(), String> {
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

fn uninstall() -> Result<(), String> {
    crate::run(&["remove"])?;
    // Program Files'taki exe kendini silemez: tunnel arkasında birkaç saniye içinde
    // klasörü silen bir komut bırakır.
    let start = Instant::now();
    while crate::install_dir().exists() && start.elapsed() < Duration::from_secs(40) {
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}

fn leftovers() -> Vec<String> {
    let exists = |p: std::path::PathBuf| p.exists().then(|| p.display().to_string());
    let mut l: Vec<String> = [crate::install_dir(), crate::data_dir()].into_iter().filter_map(exists).collect();
    if crate::installed() {
        l.push(t!("rabbithole service", "rabbithole hizmeti").into());
    }
    let env = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
    let path = util::reg_string(HKEY_LOCAL_MACHINE, env, "Path").unwrap_or_default().to_lowercase();
    if path.split(';').any(|p| p.trim_end_matches('\\').ends_with(r"\rabbithole")) {
        l.push(t!("rabbithole in the system PATH", "sistem PATH'inde rabbithole").into());
    }
    l
}
