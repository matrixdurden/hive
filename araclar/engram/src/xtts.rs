//! Yerel ses: XTTS v2, WSL'de ekran kartında çalışan küçük bir sunucu (`araclar/xtts_sunucu.py`,
//! kurulumu `make engram-ses`). engram açılınca sunucuyu başlatır ve stdin'ini açık tutar; engram
//! kapanınca boru kapanır, sunucu da kendini kapatır. Ses 24 kHz mono, üretildikçe akar.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use crate::net;
use crate::util::Res;

pub const RATE: u32 = 24_000;
const URL: &str = "http://127.0.0.1:5123";
/// WSL'de sunucuyu başlatan komut (`~` WSL'deki ev klasörü).
const CMD: &str = "exec ~/.local/share/engram-xtts/.venv/bin/python ~/.local/share/engram-xtts/sunucu.py";

/// Sunucunun durumu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Off = 0,
    Loading = 1,
    Ready = 2,
    /// Başlamadı (kurulu değil ya da hata).
    Failed = 3,
}

static STATUS: AtomicU8 = AtomicU8::new(0);
static CHILD: Mutex<Option<Child>> = Mutex::new(None);

pub fn status() -> Status {
    match STATUS.load(Ordering::Relaxed) {
        1 => Status::Loading,
        2 => Status::Ready,
        3 => Status::Failed,
        _ => Status::Off,
    }
}

fn set(s: Status) {
    STATUS.store(s as u8, Ordering::Relaxed);
}

fn healthy() -> bool {
    net::get(&format!("{URL}/saglik"), &[]).is_ok_and(|r| r.status == 200)
}

/// Sunucuyu başlatır (çalışıyorsa bir şey yapmaz); model arka planda yüklenir (~30–40 sn).
pub fn start() {
    if matches!(status(), Status::Loading | Status::Ready) {
        return;
    }
    set(Status::Loading);
    std::thread::spawn(|| {
        // Önceki bir engram'dan kalmış sunucu varsa onu kullan.
        if healthy() {
            set(Status::Ready);
            return;
        }
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let spawned = Command::new("wsl.exe")
            .args(["-e", "bash", "-lc", CMD])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
        match spawned {
            Ok(c) => *CHILD.lock().unwrap() = Some(c),
            Err(e) => {
                crate::log!("yerel ses başlatılamadı: {e}");
                set(Status::Failed);
                return;
            }
        }
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(180) {
            std::thread::sleep(Duration::from_millis(1000));
            let exited = CHILD.lock().unwrap().as_mut().is_some_and(|c| c.try_wait().ok().flatten().is_some());
            if exited {
                crate::log!("yerel ses sunucusu kapandı (kurulu mu? make engram-ses)");
                set(Status::Failed);
                return;
            }
            if healthy() {
                crate::log!("yerel ses hazır ({:.0} sn)", t.elapsed().as_secs_f32());
                set(Status::Ready);
                return;
            }
        }
        crate::log!("yerel ses 3 dakikada hazır olmadı");
        set(Status::Failed);
    });
}

/// Sunucuyu kapatır (yerel ses seçili değilse).
pub fn stop() {
    if let Some(mut c) = CHILD.lock().unwrap().take() {
        // stdin kapanınca sunucu kendini kapatır; olmazsa süreç öldürülür.
        drop(c.stdin.take());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            if c.try_wait().ok().flatten().is_none() {
                let _ = c.kill();
            }
            let _ = c.wait();
        });
    }
    set(Status::Off);
}

/// Hazır olana kadar bekler (en çok `max`).
pub fn wait_ready(max: Duration) -> bool {
    let t = Instant::now();
    while t.elapsed() < max {
        match status() {
            Status::Ready => return true,
            Status::Failed | Status::Off => return false,
            Status::Loading => std::thread::sleep(Duration::from_millis(300)),
        }
    }
    false
}

/// Konuşur: ses parçaları (24 kHz) geldikçe `on_pcm`'e (`false` dönerse keser).
pub fn speak(text: &str, speaker: &str, speed: f32, on_pcm: &mut dyn FnMut(&[i16]) -> bool) -> Res<()> {
    if status() != Status::Ready {
        return Err(match status() {
            Status::Loading => t!("Local voice is still loading", "Yerel ses henüz yükleniyor"),
            _ => t!("Local voice is not running (make engram-ses)", "Yerel ses çalışmıyor (make engram-ses)"),
        }
        .into());
    }
    let body = serde_json::json!({"text": text, "speaker": speaker, "speed": speed}).to_string();
    let mut odd: Option<u8> = None;
    let mut buf: Vec<i16> = Vec::with_capacity(8192);
    let r = net::stream("POST", &format!("{URL}/konus"), &[("content-type", "application/json")], body.as_bytes(), &mut |chunk| {
        buf.clear();
        let mut bytes = chunk;
        if let Some(lo) = odd.take()
            && let Some((&hi, rest)) = bytes.split_first()
        {
            buf.push(i16::from_le_bytes([lo, hi]));
            bytes = rest;
        }
        let mut pairs = bytes.chunks_exact(2);
        buf.extend(pairs.by_ref().map(|p| i16::from_le_bytes([p[0], p[1]])));
        odd = pairs.remainder().first().copied();
        buf.is_empty() || on_pcm(&buf)
    });
    if let Err(e) = r {
        // Sunucu gittiyse yeniden başlatılabilsin.
        if !healthy() {
            set(Status::Failed);
        }
        return Err(e);
    }
    Ok(())
}

/// Tamamını bekleyen sürüm.
pub fn speak_all(text: &str, speaker: &str, speed: f32) -> Res<Vec<i16>> {
    let mut out = Vec::new();
    speak(text, speaker, speed, &mut |p| {
        out.extend_from_slice(p);
        true
    })?;
    Ok(out)
}

/// Hazır seslerin adları.
pub fn speakers() -> Res<Vec<String>> {
    let r = net::get(&format!("{URL}/sesler"), &[])?.ok()?;
    Ok(r.text().lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
}

/// engram kapanırken: sunucu boru kapanınca kendini kapatır; yine de bekletmeden haber ver.
pub fn shutdown() {
    if let Some(mut c) = CHILD.lock().unwrap().take()
        && let Some(mut stdin) = c.stdin.take()
    {
        let _ = stdin.flush();
        drop(stdin);
    }
}

// --- Konuşmacı tanıma: yalnızca kullanıcının sesi ---

fn pcm_bytes(pcm: &[i16]) -> Vec<u8> {
    pcm.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn post_pcm(path: &str, pcm: &[i16]) -> Res<serde_json::Value> {
    let r = net::request("POST", &format!("{URL}{path}"), &[("content-type", "application/octet-stream")], &pcm_bytes(pcm))?.ok()?;
    Ok(serde_json::from_slice(&r.body)?)
}

/// Ses (16 kHz) kullanıcının sesine ne kadar benziyor: (0..1, öğrenilmiş örnek sayısı).
pub fn who(pcm: &[i16]) -> Res<(f32, usize)> {
    let v = post_pcm("/kim", pcm)?;
    Ok((v["benzerlik"].as_f64().unwrap_or(1.0) as f32, v["kayit"].as_u64().unwrap_or(0) as usize))
}

/// Kullanıcının sesinden bir örnek daha öğrenir; örnek sayısını döndürür.
pub fn enroll(pcm: &[i16]) -> Res<usize> {
    Ok(post_pcm("/tani", pcm)?["kayit"].as_u64().unwrap_or(0) as usize)
}

/// Öğrenilmiş ses izini siler.
pub fn forget() -> Res<()> {
    post_pcm("/unut", &[])?;
    Ok(())
}

/// Öğrenilmiş örnek sayısı (sunucu hazır değilse `None`).
pub fn prints() -> Option<usize> {
    let r = net::get(&format!("{URL}/saglik"), &[]).ok().filter(|r| r.status == 200)?;
    r.text().split_whitespace().nth(1).and_then(|n| n.parse().ok())
}
