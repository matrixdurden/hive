//! Otomatik güncelleme. GitHub Releases'teki son sürüm bu exe'den yeniyse indirir, SHA-256'sını
//! doğrular ve kurulum klasörüne `cheshire.exe.yeni` olarak koyar. Geçiş `apply` ile olur: çalışan
//! exe silinemez ama yeniden adlandırılabilir, bu yüzden ayrı bir güncelleyici programa gerek yok.
//!
//! Motorun eski, tek başına sürümünden kalma: hive'ın içinde (hub modu) çalışmaz, motoru hive
//! günceller. Yayın düzeni: etiket `vX.Y.Z`, varlıklar `cheshire.exe` ve `cheshire.exe.sha256`.
//! Taslak ve ön sürümler `latest` sayılmaz, kullanıcılara gitmez.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
use windows::Win32::System::Threading::PROCESS_CREATION_FLAGS;
use windows::core::{PCWSTR, w};

use crate::log;
use crate::util::{self, Res, wide};

const ASSET: &str = "cheshire.exe";
/// Açılışta ağın ve masaüstünün oturmasını bekle.
const FIRST_CHECK: Duration = Duration::from_secs(30);
const EVERY: Duration = Duration::from_secs(24 * 3600);
const MAX_SIZE: usize = 64 << 20;

/// Menüdeki "Otomatik güncelle"; kapalıyken denetim yapılmaz.
pub static ENABLED: AtomicBool = AtomicBool::new(true);

fn new_exe() -> PathBuf {
    util::app_dir().join("cheshire.exe.yeni")
}

fn old_exe() -> PathBuf {
    util::app_dir().join("cheshire.exe.eski")
}

/// `sahip/repo`, Cargo.toml'daki `repository` alanından.
fn repo() -> &'static str {
    env!("CARGO_PKG_REPOSITORY").trim_start_matches("https://github.com/")
}

fn version(s: &str) -> Option<(u32, u32, u32)> {
    let mut parts = s.trim_start_matches('v').split('.').map(|p| p.parse().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// Arka planda günde bir denetler. Yeni sürüm indirilince `ready` bir kez çağrılır.
pub fn spawn(ready: impl Fn() + Send + 'static) {
    let _ = std::thread::Builder::new().name("guncelleme".into()).spawn(move || {
        std::thread::sleep(FIRST_CHECK);
        // Duvar saatiyle: bilgisayar uykudayken geçen süre de sayılsın.
        let mut last: Option<SystemTime> = None;
        loop {
            let due = last.is_none_or(|t| t.elapsed().is_ok_and(|e| e >= EVERY));
            if due && ENABLED.load(Ordering::Relaxed) {
                last = Some(SystemTime::now());
                match check() {
                    Ok(Some(tag)) => {
                        log!("güncelleme indirildi: {tag}");
                        ready();
                        return;
                    }
                    Ok(None) => {}
                    Err(e) => log!("güncelleme denetlenemedi: {e}"),
                }
            }
            std::thread::sleep(Duration::from_secs(3600));
        }
    });
}

/// Yeni sürüm varsa indirip doğrular; indirdiği sürümün etiketini döndürür.
fn check() -> Res<Option<String>> {
    let (status, body) = get(&format!("https://api.github.com/repos/{}/releases/latest", repo()), true)?;
    if status == 404 {
        return Ok(None); // henüz yayın yok
    }
    if status != 200 {
        return Err(format!("GitHub API: HTTP {status}").into());
    }
    let json = String::from_utf8_lossy(&body);
    let tag = json
        .split_once("\"tag_name\":")
        .and_then(|(_, rest)| rest.trim_start().strip_prefix('"'))
        .and_then(|rest| rest.split('"').next())
        .ok_or("yanıtta tag_name yok")?
        .to_string();
    match (version(&tag), version(env!("CARGO_PKG_VERSION"))) {
        (Some(latest), Some(current)) if latest > current => {}
        _ => return Ok(None),
    }

    let base = format!("https://github.com/{}/releases/download/{tag}/", repo());
    let sums = download(&format!("{base}{ASSET}.sha256"))?;
    let expected = String::from_utf8_lossy(&sums).split_whitespace().next().unwrap_or_default().to_ascii_lowercase();
    if expected.len() != 64 {
        return Err(format!("{ASSET}.sha256 okunamadı").into());
    }
    let exe = download(&format!("{base}{ASSET}"))?;
    let actual = sha256(&exe)?;
    if !exe.starts_with(b"MZ") || actual != expected {
        return Err(format!("{tag}: SHA-256 tutmadı (beklenen {expected}, gelen {actual})").into());
    }
    std::fs::write(new_exe(), &exe)?;
    Ok(Some(tag))
}

/// İndirilen sürüme geçer: exe'leri yer değiştirir ve yenisini başlatır. `Ok` dönerse çağıran çıkmalı;
/// yeni süreç bu sürecin bitmesini bekler (`--sonra`), sonra `cleanup` ile eskisini siler.
pub fn apply() -> Res<()> {
    let (exe, new, old) = (util::installed_exe(), new_exe(), old_exe());
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old)?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(e.into());
    }
    let line = format!("\"{}\" --guncellendi --sonra {}", exe.display(), std::process::id());
    if let Err(e) = util::spawn_detached(&line, None, PROCESS_CREATION_FLAGS(0)) {
        let _ = std::fs::rename(&exe, &new);
        let _ = std::fs::rename(&old, &exe);
        return Err(e);
    }
    Ok(())
}

/// Önceki sürümün exe'si ve yarım kalmış indirmeler.
pub fn cleanup() {
    let _ = std::fs::remove_file(old_exe());
    let _ = std::fs::remove_file(new_exe());
}

fn download(url: &str) -> Res<Vec<u8>> {
    match get(url, false)? {
        (200, body) => Ok(body),
        (status, _) => Err(format!("HTTP {status}: {url}").into()),
    }
}

fn sha256(data: &[u8]) -> Res<String> {
    let mut out = [0u8; 32];
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out).ok()? };
    Ok(out.iter().map(|b| format!("{b:02x}")).collect())
}

/// WinHTTP tutamacı; düşünce kapanır.
struct Handle(*mut core::ffi::c_void);

impl Handle {
    fn new(h: *mut core::ffi::c_void) -> windows::core::Result<Self> {
        if h.is_null() { Err(windows::core::Error::from_thread()) } else { Ok(Self(h)) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// HTTPS GET; yönlendirmeleri (github.com → CDN) WinHTTP kendisi izler. Proxy ayarı sistemden gelir.
pub fn get(url: &str, api: bool) -> Res<(u32, Vec<u8>)> {
    let rest = url.strip_prefix("https://").ok_or("yalnızca https")?;
    let (host, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    let agent = wide(concat!("cheshire/", env!("CARGO_PKG_VERSION")));
    let (host, path) = (wide(host), wide(&path));
    unsafe {
        let session = Handle::new(WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ))?;
        let connect = Handle::new(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT, 0))?;
        let request = Handle::new(WinHttpOpenRequest(
            connect.0,
            w!("GET"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        let headers: Vec<u16> = "Accept: application/vnd.github+json\r\n".encode_utf16().collect();
        WinHttpSendRequest(request.0, api.then_some(&headers[..]), None, 0, 0, 0)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())?;

        let (mut status, mut len) = (0u32, 4u32);
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut len,
            std::ptr::null_mut(),
        )?;
        let mut body = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut n = 0u32;
            WinHttpReadData(request.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut n)?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n as usize]);
            if body.len() > MAX_SIZE {
                return Err("yanıt çok büyük".into());
            }
        }
        Ok((status, body))
    }
}
