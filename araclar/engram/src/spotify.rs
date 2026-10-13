//! Spotify Web API: çalma listeleri, arama, zevk sinyalleri (en çok dinlenenler, son çalınanlar)
//! ve çalma denetimi (Premium). Giriş OAuth Authorization Code + PKCE ile: gizli anahtar yok,
//! kullanıcı kendi uygulamasını açar ve yalnızca Client ID verir. Belirteçler `spotify.ini`'de
//! DPAPI ile şifreli durur, süresi dolmadan 60 sn önce kendiliğinden yenilenir. Belirteçler
//! asla loglanmaz.
//!
//! Şubat 2026 Geliştirici Modu değişiklikleri: liste şarkıları `/playlists/{id}/items` (alan
//! `item`; yalnızca kendi ya da ortak listeler), kitaplığa ekleme `/me/library?uris=`, aramada
//! en çok 10 sonuç. Öneri (recommendations) ve ses özellikleri (audio-features) kapalı:
//! model seçimi liste adları, şarkı listeleri, en çok dinlenenler ve aramayla yapar.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};

use crate::net;
use crate::util::Res;

const API: &str = "https://api.spotify.com/v1";
const AUTHORIZE: &str = "https://accounts.spotify.com/authorize";
const TOKEN: &str = "https://accounts.spotify.com/api/token";
pub const REDIRECT: &str = "http://127.0.0.1:8737/spotify";
const PORT: u16 = 8737;
const SCOPES: &str = "user-read-playback-state user-modify-playback-state user-read-currently-playing playlist-read-private \
                      playlist-read-collaborative user-library-read user-library-modify user-read-recently-played user-top-read";
/// Yanıt metinleri en çok bu kadar satır (modele kısa bağlam).
const MAX_LINES: usize = 60;

const NOT_CONNECTED: &str = "Spotify bağlı değil: Ayarlar'dan Spotify'a bağlan.";
/// `api` bu metinle döner; `with_device` tanıyıp cihazı uyandırır.
const NO_DEVICE: &str = "Spotify'da etkin cihaz yok.";

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    /// UNIX saniyesi.
    pub expires_at: u64,
}

struct State {
    client_id: String,
    tokens: Tokens,
    /// `/me` kimliği; ilk gerekince alınır.
    user: String,
}

/// Bellekteki oturum; ilk kullanımda `spotify.ini`'den yüklenir.
static STATE: Mutex<Option<State>> = Mutex::new(None);

fn lock() -> std::sync::MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// --- Kalıcı kayıt ---

fn ini() -> std::path::PathBuf {
    crate::util::data_dir().join("spotify.ini")
}

fn load() -> Option<State> {
    let text = std::fs::read_to_string(ini()).ok()?;
    let mut s = State { client_id: String::new(), tokens: Tokens::default(), user: String::new() };
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k.trim() {
            "client_id" => s.client_id = v.to_string(),
            "access" => s.tokens.access = crate::config::unprotect(v),
            "refresh" => s.tokens.refresh = crate::config::unprotect(v),
            "expires_at" => s.tokens.expires_at = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    Some(s)
}

fn save(s: &State) {
    let _ = std::fs::create_dir_all(crate::util::data_dir());
    let text = format!(
        "client_id={}\naccess={}\nrefresh={}\nexpires_at={}\n",
        s.client_id,
        crate::config::protect(&s.tokens.access),
        crate::config::protect(&s.tokens.refresh),
        s.tokens.expires_at
    );
    let _ = std::fs::write(ini(), text);
}

/// Bağlı mı (yenileme belirteci var mı; geçerliliği ilk istekte anlaşılır).
pub fn connected() -> bool {
    let mut g = lock();
    if g.is_none() {
        *g = load();
    }
    g.as_ref().is_some_and(|s| !s.tokens.refresh.is_empty() && !s.client_id.is_empty())
}

/// Kayıtlı Client ID (Ayarlar'da göstermek için); yoksa boş.
pub fn client_id() -> String {
    let mut g = lock();
    if g.is_none() {
        *g = load();
    }
    g.as_ref().map(|s| s.client_id.clone()).unwrap_or_default()
}

/// Belirteçleri siler; Client ID kalır (yeniden bağlanmak kolay olsun).
pub fn logout() {
    let mut g = lock();
    let client_id = g.take().or_else(load).map(|s| s.client_id).unwrap_or_default();
    let s = State { client_id, tokens: Tokens::default(), user: String::new() };
    save(&s);
    *g = Some(s);
    crate::log!("spotify: oturum kapatıldı");
}

// --- Belirteçler ---

/// Geçerli erişim belirteci; süresi dolmak üzereyse yeniler.
fn token() -> Res<String> {
    let mut g = lock();
    if g.is_none() {
        *g = load();
    }
    let s = g.as_mut().filter(|s| !s.tokens.refresh.is_empty()).ok_or(NOT_CONNECTED)?;
    if s.tokens.access.is_empty() || s.tokens.expires_at <= unix() + 60 {
        refresh(s)?;
    }
    Ok(s.tokens.access.clone())
}

/// 401 sonrası: başka bir iş parçacığı zaten yenilemediyse yeniler.
fn renew(failed: &str) -> Res<String> {
    let mut g = lock();
    let s = g.as_mut().filter(|s| !s.tokens.refresh.is_empty()).ok_or(NOT_CONNECTED)?;
    if s.tokens.access == failed {
        refresh(s)?;
    }
    Ok(s.tokens.access.clone())
}

/// refresh_token ile yeniler (PKCE: client_id gerekir); dönen yeni refresh_token saklanır.
/// Kilit tutulurken çalışır: aynı anda iki yenileme olmaz.
fn refresh(s: &mut State) -> Res<()> {
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", s.tokens.refresh.as_str()),
        ("client_id", s.client_id.as_str()),
    ];
    match token_request(&form) {
        Ok(t) => {
            s.tokens.access = t.access;
            if !t.refresh.is_empty() {
                s.tokens.refresh = t.refresh;
            }
            s.tokens.expires_at = t.expires_at;
            save(s);
            Ok(())
        }
        Err(e) if e.to_string().contains("invalid_grant") => {
            // İzin geri alınmış ya da belirteç eskimiş: yeniden bağlanmak gerekir.
            s.tokens = Tokens::default();
            save(s);
            crate::log!("spotify: yenileme reddedildi, oturum düştü");
            Err("Spotify oturumu düştü: Ayarlar'dan yeniden bağlan.".into())
        }
        Err(e) => Err(e),
    }
}

/// /api/token'a form gönderir. Hata: "Spotify girişi: <kod>: <açıklama>".
fn token_request(form: &[(&str, &str)]) -> Res<Tokens> {
    let body = form_encode(form);
    let headers = [("content-type", "application/x-www-form-urlencoded"), ("accept", "application/json")];
    let resp = net::request("POST", TOKEN, &headers, body.as_bytes())?;
    let v: Value = serde_json::from_slice(&resp.body).unwrap_or(Value::Null);
    if !(200..300).contains(&resp.status) {
        let code = v["error"].as_str().unwrap_or("");
        let desc = v["error_description"].as_str().unwrap_or("");
        return Err(format!("Spotify girişi: HTTP {}: {code}: {desc}", resp.status).into());
    }
    let access = v["access_token"].as_str().unwrap_or("").to_string();
    if access.is_empty() {
        return Err("Spotify girişi: yanıtta belirteç yok".into());
    }
    Ok(Tokens {
        access,
        refresh: v["refresh_token"].as_str().unwrap_or("").to_string(),
        expires_at: unix() + v["expires_in"].as_u64().unwrap_or(3600),
    })
}

// --- Giriş (PKCE) ---

/// Tarayıcıda Spotify izin sayfasını açar, 127.0.0.1:8737'de dönüşü 3 dk bekler, kodu
/// belirteçle değiştirir ve kaydeder. Bloklar: iş parçacığından çağrılır.
pub fn login(client_id: &str) -> Res<Tokens> {
    let client_id = client_id.trim();
    if client_id.is_empty() || !client_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("Spotify Client ID geçersiz (developer.spotify.com'daki uygulamanın Client ID'si).".into());
    }
    let verifier = b64url(&random(48)?);
    let state = b64url(&random(16)?);
    let listener = TcpListener::bind(("127.0.0.1", PORT))
        .map_err(|_| format!("{PORT} portu kullanımda (başka bir Spotify bağlanması sürüyor olabilir)."))?;
    let url = format!(
        "{AUTHORIZE}?client_id={}&response_type=code&redirect_uri={}&code_challenge_method=S256&code_challenge={}&state={}&scope={}",
        enc(client_id),
        enc(REDIRECT),
        challenge(&verifier),
        enc(&state),
        enc(SCOPES)
    );
    crate::log!("spotify: izin sayfası açılıyor");
    crate::util::open_url(&url);
    let code = wait_code(&listener, &state, Duration::from_secs(180))?;
    drop(listener);
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", REDIRECT),
        ("client_id", client_id),
        ("code_verifier", verifier.as_str()),
    ];
    let tokens = token_request(&form)?;
    if tokens.refresh.is_empty() {
        return Err("Spotify girişi: yenileme belirteci gelmedi".into());
    }
    let s = State { client_id: client_id.to_string(), tokens: tokens.clone(), user: String::new() };
    save(&s);
    *lock() = Some(s);
    crate::log!("spotify: bağlandı");
    Ok(tokens)
}

const PAGE_OK: &str = "engram Spotify'a bağlandı, bu sekmeyi kapatabilirsin.";
const PAGE_DENIED: &str = "Spotify izni verilmedi. engram'a dönüp yeniden deneyebilirsin.";
const PAGE_BAD: &str = "Bağlantı doğrulanamadı (state uyuşmadı). engram'dan yeniden dene.";

/// Tarayıcının dönüşünü bekler; `code`'u verir. Başka yollara (favicon...) 404 der ve bekler.
fn wait_code(listener: &TcpListener, state: &str, timeout: Duration) -> Res<String> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() > deadline {
            return Err("Spotify bağlanması 3 dakikada tamamlanmadı.".into());
        }
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        // Windows'ta kabul edilen soket dinleyicinin bloklamazlığını devralır.
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let Some(target) = read_target(&mut stream) else { continue };
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        if path != "/spotify" {
            reply(&mut stream, "404 Not Found", "");
            continue;
        }
        let q = parse_query(query);
        let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        if get("state") != Some(state) {
            reply(&mut stream, "400 Bad Request", PAGE_BAD);
            return Err("Spotify dönüşü doğrulanamadı (state uyuşmadı).".into());
        }
        if let Some(err) = get("error") {
            reply(&mut stream, "200 OK", PAGE_DENIED);
            return Err(format!("Spotify izni verilmedi ({err}).").into());
        }
        let Some(code) = get("code").filter(|c| !c.is_empty()) else {
            reply(&mut stream, "400 Bad Request", PAGE_BAD);
            return Err("Spotify dönüşünde kod yok.".into());
        };
        reply(&mut stream, "200 OK", PAGE_OK);
        return Ok(code.to_string());
    }
}

/// İstek satırındaki hedef ("/spotify?code=..."); yalnızca GET.
fn read_target(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&buf);
    let mut parts = text.lines().next()?.split_whitespace();
    (parts.next()? == "GET").then(|| parts.next().map(String::from))?
}

fn reply(stream: &mut TcpStream, status: &str, text: &str) {
    let html = if text.is_empty() {
        String::new()
    } else {
        format!(
            "<!doctype html><html lang=\"tr\"><head><meta charset=\"utf-8\"><title>engram</title></head>\
             <body style=\"font-family:Segoe UI,sans-serif;background:#1f1e1d;color:#f0eee6;display:flex;\
             align-items:center;justify-content:center;height:100vh;margin:0\"><p style=\"font-size:20px\">{text}</p></body></html>"
        )
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        html.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(html.as_bytes());
    let _ = stream.flush();
}

/// İşletim sisteminin güvenli rastgele baytları.
fn random(n: usize) -> Res<Vec<u8>> {
    let mut buf = vec![0u8; n];
    unsafe { BCryptGenRandom(None, &mut buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG).ok()? };
    Ok(buf)
}

/// PKCE S256: base64url(sha256(verifier)), dolgusuz.
fn challenge(verifier: &str) -> String {
    b64url(&sha256(verifier.as_bytes()))
}

fn b64url(data: &[u8]) -> String {
    crate::config::base64(data).trim_end_matches('=').replace('+', "-").replace('/', "_")
}

/// SHA-256 (FIPS 180-4).
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            (hh, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 32];
    for (i, x) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

/// URL yüzde kodlaması (RFC 3986: harf, rakam ve `-._~` dışındaki her bayt).
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| p.split_once('=').unwrap_or((p, "")))
        .map(|(k, v)| (dec(k), dec(v)))
        .collect()
}

fn form_encode(form: &[(&str, &str)]) -> String {
    form.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

// --- API ---

/// Web API isteği. `path` "/me/player" gibi ya da sayfalamadaki tam `next` adresi. 401'de bir
/// kez yeniler, 429'da bir kez bekler; boş yanıt (204) `Null`.
fn api(method: &str, path: &str, body: Option<Value>) -> Res<Value> {
    let url = if path.starts_with("https://") { path.to_string() } else { format!("{API}{path}") };
    let data = body.map(|b| b.to_string()).unwrap_or_default();
    let mut access = token()?;
    let (mut renewed, mut waited) = (false, false);
    loop {
        let auth = format!("Bearer {access}");
        let mut headers = vec![("authorization", auth.as_str()), ("accept", "application/json")];
        if !data.is_empty() {
            headers.push(("content-type", "application/json"));
        }
        let resp = net::request(method, &url, &headers, data.as_bytes())?;
        match resp.status {
            401 if !renewed => {
                renewed = true;
                access = renew(&access)?;
            }
            // net::Response başlıkları vermiyor: Retry-After yerine bir kez 2 sn beklenir.
            429 if !waited => {
                waited = true;
                std::thread::sleep(Duration::from_secs(2));
            }
            s if (200..300).contains(&s) => {
                if resp.body.iter().all(u8::is_ascii_whitespace) {
                    return Ok(Value::Null);
                }
                return Ok(serde_json::from_slice(&resp.body).unwrap_or(Value::Null));
            }
            s => {
                let msg = error_text(s, &resp.body);
                crate::log!("spotify: {method} {} → {s}", path.split('?').next().unwrap_or(""));
                return Err(msg.into());
            }
        }
    }
}

/// Spotify hata gövdesinden kısa Türkçe metin.
fn error_text(status: u32, body: &[u8]) -> String {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let reason = v["error"]["reason"].as_str().unwrap_or("");
    let msg = v["error"]["message"].as_str().or(v["error"].as_str()).unwrap_or("").to_string();
    match reason {
        "NO_ACTIVE_DEVICE" => return NO_DEVICE.into(),
        "PREMIUM_REQUIRED" => return "Bu işlem Spotify Premium ister.".into(),
        "NO_PREV_TRACK" => return "Önceki şarkı yok.".into(),
        "NO_NEXT_TRACK" => return "Sonraki şarkı yok.".into(),
        "ALREADY_PAUSED" => return "Zaten duraklatılmış.".into(),
        "NOT_PAUSED" => return "Zaten çalıyor.".into(),
        "VOLUME_CONTROL_DISALLOW" => return "Bu cihazda ses düzeyi değiştirilemiyor.".into(),
        "DEVICE_NOT_CONTROLLABLE" => return "Bu cihaz uzaktan denetlenemiyor.".into(),
        _ => {}
    }
    let low = msg.to_lowercase();
    if low.contains("no active device") {
        return NO_DEVICE.into();
    }
    match status {
        401 => "Spotify oturumu geçersiz: Ayarlar'dan yeniden bağlan.".into(),
        403 if low.contains("not be registered") || low.contains("not registered") => {
            "Bu Spotify hesabı uygulamanın kullanıcı listesinde yok (developer.spotify.com > uygulama > User Management).".into()
        }
        403 => format!("Spotify izin vermedi{}", if msg.is_empty() { ".".into() } else { format!(": {msg}") }),
        404 => format!("Spotify'da bulunamadı{}", if msg.is_empty() { ".".into() } else { format!(": {msg}") }),
        429 => "Spotify istek sınırına takıldı, biraz sonra tekrar dene.".into(),
        500..=599 => format!("Spotify şu an yanıt vermiyor (HTTP {status})."),
        _ => format!("Spotify: HTTP {status}{}", if msg.is_empty() { String::new() } else { format!(": {msg}") }),
    }
}

/// `/me` kimliği (önbellekte).
fn user_id() -> Res<String> {
    if let Some(u) = lock().as_ref().map(|s| s.user.clone()).filter(|u| !u.is_empty()) {
        return Ok(u);
    }
    let me = api("GET", "/me", None)?;
    let id = me["id"].as_str().ok_or("Spotify: kullanıcı kimliği alınamadı")?.to_string();
    if let Some(s) = lock().as_mut() {
        s.user = id.clone();
    }
    Ok(id)
}

// --- Biçim yardımcıları ---

fn clock(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// Şarkının sanatçıları ya da bölümün programı.
fn who(item: &Value) -> String {
    let names: Vec<&str> =
        item["artists"].as_array().into_iter().flatten().filter_map(|a| a["name"].as_str()).collect();
    if names.is_empty() { s(&item["show"]["name"]).to_string() } else { names.join(", ") }
}

/// "Sanatçı — Şarkı".
fn track_line(item: &Value) -> String {
    format!("{} — {}", who(item), s(&item["name"]))
}

/// Uzun listeyi kısaltır.
fn cap(mut lines: Vec<String>, total: usize, what: &str) -> String {
    if lines.len() > MAX_LINES {
        lines.truncate(MAX_LINES - 1);
    }
    if total > lines.len() {
        lines.push(format!("… ve {} {what} daha (kısaltıldı)", total - lines.len()));
    }
    lines.join("\n")
}

/// Spotify URI'si, open.spotify.com bağlantısı ya da çıplak kimlikten (tür, kimlik). Çıplak
/// kimlikte tür boş.
fn parse_target(t: &str) -> Option<(String, String)> {
    let t = t.trim();
    if let Some(rest) = t.strip_prefix("spotify:") {
        let mut p = rest.split(':');
        let (kind, id) = (p.next()?, p.next()?);
        return (p.next().is_none() && !id.is_empty()).then(|| (kind.to_string(), id.to_string()));
    }
    if let Some(i) = t.find("open.spotify.com/") {
        let path = t[i + "open.spotify.com/".len()..].split(['?', '#']).next()?;
        let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty() && !s.starts_with("intl-")).collect();
        let pos =
            segs.iter().position(|s| matches!(*s, "track" | "album" | "artist" | "playlist" | "episode" | "show"))?;
        let id = segs.get(pos + 1)?;
        return Some((segs[pos].to_string(), id.to_string()));
    }
    (t.len() >= 16 && t.len() <= 40 && t.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| (String::new(), t.to_string()))
}

/// Kimlikten kimlik (URI ya da bağlantıysa içindeki).
fn bare_id(t: &str) -> String {
    parse_target(t).map(|(_, id)| id).unwrap_or_else(|| t.trim().to_string())
}

/// Tam Spotify URI'si; çıplak kimliğin türü sırayla denenerek bulunur.
fn resolve(t: &str) -> Res<String> {
    let t = t.trim();
    if t.starts_with("spotify:") && t.ends_with(":collection") {
        return Ok(t.to_string());
    }
    let (kind, id) =
        parse_target(t).ok_or_else(|| format!("'{t}' bir Spotify kimliği değil: önce spotify_search ile bul."))?;
    if !kind.is_empty() {
        return Ok(format!("spotify:{kind}:{id}"));
    }
    for (kind, path) in [
        ("playlist", format!("/playlists/{id}?fields=uri")),
        ("track", format!("/tracks/{id}")),
        ("album", format!("/albums/{id}")),
        ("artist", format!("/artists/{id}")),
    ] {
        if api("GET", &path, None).is_ok() {
            return Ok(format!("spotify:{kind}:{id}"));
        }
    }
    Err(format!("'{t}' Spotify'da bulunamadı.").into())
}

// --- Cihaz ---

/// Etkin cihaz yoksa birini uyandırıp `f`'i o cihazla yeniden dener.
fn with_device<T>(f: impl Fn(Option<&str>) -> Res<T>) -> Res<T> {
    match f(None) {
        Err(e) if e.to_string() == NO_DEVICE => {
            let id = wake_device()?;
            f(Some(&id))
        }
        r => r,
    }
}

/// "?device_id=..." ya da "&device_id=...".
fn dev_q(dev: Option<&str>, sep: char) -> String {
    dev.map(|d| format!("{sep}device_id={}", enc(d))).unwrap_or_default()
}

/// Kullanılabilir en iyi cihaz: web oynatıcı, sonra bu bilgisayar, sonra ilki.
fn pick_device() -> Res<Option<(String, String)>> {
    let v = api("GET", "/me/player/devices", None)?;
    let mut best: Option<(u8, String, String)> = None;
    for d in v["devices"].as_array().into_iter().flatten() {
        let (Some(id), false) = (d["id"].as_str(), d["is_restricted"].as_bool().unwrap_or(false)) else { continue };
        let name = s(&d["name"]).to_string();
        let rank = if name.to_lowercase().contains("web player") {
            0
        } else if s(&d["type"]).eq_ignore_ascii_case("computer") {
            1
        } else {
            2
        };
        if best.as_ref().is_none_or(|b| rank < b.0) {
            best = Some((rank, id.to_string(), name));
        }
    }
    Ok(best.map(|(_, id, name)| (id, name)))
}

/// Cihaz bulur (yoksa open.spotify.com'u açıp ~12 sn bekler) ve çalmayı ona aktarır.
fn wake_device() -> Res<String> {
    let (id, name) = match pick_device()? {
        Some(d) => d,
        None => {
            crate::log!("spotify: cihaz yok, web oynatıcı açılıyor");
            crate::computer::open("https://open.spotify.com", None)?;
            let t = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(1000));
                if let Some(d) = pick_device()? {
                    break d;
                }
                if t.elapsed() > Duration::from_secs(12) {
                    return Err("Spotify'da cihaz görünmüyor: open.spotify.com açılınca bir kez tekrar dene.".into());
                }
            }
        }
    };
    crate::log!("spotify: çalma {name} cihazına aktarılıyor");
    api("PUT", "/me/player", Some(json!({"device_ids": [id], "play": false})))?;
    std::thread::sleep(Duration::from_millis(600));
    Ok(id)
}

// --- Üst düzey ---

/// Şu an çalan: sanatçı, şarkı, albüm, ilerleme, durum, cihaz, karıştırma, bağlam.
pub fn now() -> Res<String> {
    let v = api("GET", "/me/player?additional_types=track,episode", None)?;
    if v.is_null() || (v["item"].is_null() && v["device"].is_null()) {
        return Ok("Spotify'da şu an çalan bir şey yok.".into());
    }
    let item = &v["item"];
    let mut out = Vec::new();
    if item.is_null() {
        out.push("Parça bilgisi yok.".to_string());
    } else {
        out.push(track_line(item));
        let album = s(&item["album"]["name"]);
        if !album.is_empty() {
            out.push(format!("Albüm: {album}"));
        }
        out.push(format!("URI: {}", s(&item["uri"])));
    }
    let playing = v["is_playing"].as_bool().unwrap_or(false);
    out.push(format!(
        "{}, {} / {}",
        if playing { "Çalıyor" } else { "Duraklatıldı" },
        clock(v["progress_ms"].as_u64().unwrap_or(0)),
        clock(item["duration_ms"].as_u64().unwrap_or(0))
    ));
    let dev = &v["device"];
    if !dev.is_null() {
        let vol = dev["volume_percent"].as_u64().map(|p| format!(", ses %{p}")).unwrap_or_default();
        out.push(format!("Cihaz: {} ({}){vol}", s(&dev["name"]), s(&dev["type"])));
    }
    let repeat = match s(&v["repeat_state"]) {
        "track" => "şarkı",
        "context" => "liste",
        _ => "kapalı",
    };
    out.push(format!(
        "Karıştırma: {}, tekrar: {repeat}",
        if v["shuffle_state"].as_bool().unwrap_or(false) { "açık" } else { "kapalı" }
    ));
    if let Some(c) = context_name(&v["context"]) {
        out.push(c);
    }
    Ok(out.join("\n"))
}

/// Çalınan bağlam: liste, albüm, sanatçı ya da beğenilenler.
fn context_name(ctx: &Value) -> Option<String> {
    let uri = ctx["uri"].as_str()?;
    if uri.ends_with(":collection") || s(&ctx["type"]) == "collection" {
        return Some("Bağlam: Beğenilen şarkılar".into());
    }
    let (kind, id) = parse_target(uri)?;
    let name = match kind.as_str() {
        "playlist" => api("GET", &format!("/playlists/{id}?fields=name"), None).ok().map(|v| s(&v["name"]).to_string()),
        "album" => api("GET", &format!("/albums/{id}"), None).ok().map(|v| s(&v["name"]).to_string()),
        "artist" => api("GET", &format!("/artists/{id}"), None).ok().map(|v| s(&v["name"]).to_string()),
        _ => None,
    }
    .unwrap_or_default();
    let label = match kind.as_str() {
        "playlist" => "çalma listesi",
        "album" => "albüm",
        "artist" => "sanatçı",
        k => k,
    };
    Some(format!("Bağlam ({label}): {name} [{uri}]"))
}

/// Kısa tek satır: "Şimdi: Sanatçı — Şarkı (çalıyor, Cihaz)".
fn brief() -> String {
    match api("GET", "/me/player?additional_types=track,episode", None) {
        Ok(v) if !v["item"].is_null() => format!(
            "Şimdi: {} ({}, {})",
            track_line(&v["item"]),
            if v["is_playing"].as_bool().unwrap_or(false) { "çalıyor" } else { "duraklatıldı" },
            s(&v["device"]["name"])
        ),
        _ => String::new(),
    }
}

/// Bütün çalma listeleri (sayfa sayfa) ve beğenilen şarkılar.
pub fn playlists() -> Res<String> {
    let me = user_id().unwrap_or_default();
    let liked = api("GET", "/me/tracks?limit=1", None).ok().and_then(|v| v["total"].as_u64());
    let mut lines = vec![format!(
        "Beğenilen şarkılar | play hedefi: liked | {} şarkı",
        liked.map_or("?".into(), |n| n.to_string())
    )];
    let mut url = "/me/playlists?limit=50".to_string();
    let mut total = 0usize;
    for _ in 0..20 {
        let v = api("GET", &url, None)?;
        total = v["total"].as_u64().unwrap_or(0) as usize;
        for p in v["items"].as_array().into_iter().flatten().filter(|p| !p.is_null()) {
            // Şubat 2026: "tracks" alanı "items" oldu.
            let count =
                p["items"]["total"].as_u64().or(p["tracks"]["total"].as_u64()).map_or("?".into(), |n| n.to_string());
            let mut line = format!("{} | id: {} | {count} şarkı", s(&p["name"]), s(&p["id"]));
            if s(&p["owner"]["id"]) != me {
                let owner = p["owner"]["display_name"].as_str().unwrap_or(s(&p["owner"]["id"]));
                line.push_str(&format!(" | sahibi: {owner}"));
            }
            lines.push(line);
        }
        match v["next"].as_str() {
            Some(n) if lines.len() <= MAX_LINES => url = n.to_string(),
            _ => break,
        }
    }
    Ok(cap(lines, total + 1, "liste"))
}

/// Listenin ilk `limit` şarkısı ("liked" = beğenilenler). Spotify yalnızca kendi ya da ortak
/// listelerin içeriğini verir.
pub fn playlist_tracks(id: &str, limit: usize) -> Res<String> {
    let limit = limit.clamp(1, MAX_LINES - 2);
    let liked = matches!(id.trim().to_lowercase().as_str(), "liked" | "beğenilenler" | "collection");
    let (base, title) = if liked {
        ("/me/tracks?".to_string(), "Beğenilen şarkılar".to_string())
    } else {
        let id = bare_id(id);
        let name = api("GET", &format!("/playlists/{}?fields=name", enc(&id)), None)
            .map(|v| s(&v["name"]).to_string())
            .unwrap_or_default();
        (format!("/playlists/{}/items?additional_types=track,episode&", enc(&id)), name)
    };
    let mut lines = Vec::new();
    let mut total = 0usize;
    while lines.len() < limit {
        let page = (limit - lines.len()).min(50);
        let v = match api("GET", &format!("{base}limit={page}&offset={}", lines.len()), None) {
            Ok(v) => v,
            Err(e) if e.to_string().starts_with("Spotify izin vermedi") => {
                return Ok(format!(
                    "{title}: şarkıları görülemiyor; Spotify artık yalnızca kendi ya da ortak listelerinin içeriğini veriyor. Yine de çalınabilir."
                ));
            }
            Err(e) => return Err(e),
        };
        total = v["total"].as_u64().unwrap_or(0) as usize;
        let items = v["items"].as_array().cloned().unwrap_or_default();
        if items.is_empty() {
            break;
        }
        for it in &items {
            let t = if it["item"].is_object() { &it["item"] } else { &it["track"] };
            lines.push(if t.is_null() { "(kullanılamayan şarkı)".into() } else { track_line(t) });
        }
    }
    lines.truncate(limit);
    let shown = lines.len();
    let mut out = vec![format!("{title} ({total} şarkı):")];
    out.extend(lines);
    Ok(cap(out, total.max(shown) + 1, "şarkı"))
}

/// En çok dinlenenler: `kind` artists|tracks, `range` short_term|medium_term|long_term.
pub fn top(kind: &str, range: &str) -> Res<String> {
    if !matches!(kind, "artists" | "tracks") {
        return Err("tür artists ya da tracks olmalı".into());
    }
    let range = if matches!(range, "short_term" | "medium_term" | "long_term") { range } else { "medium_term" };
    let v = api("GET", &format!("/me/top/{kind}?time_range={range}&limit=25"), None)?;
    let lines: Vec<String> = v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, it)| {
            if kind == "tracks" {
                format!("{}. {}", i + 1, track_line(it))
            } else {
                let genres: Vec<&str> =
                    it["genres"].as_array().into_iter().flatten().filter_map(Value::as_str).take(3).collect();
                let g = if genres.is_empty() { String::new() } else { format!(" ({})", genres.join(", ")) };
                format!("{}. {}{g} | {}", i + 1, s(&it["name"]), s(&it["uri"]))
            }
        })
        .collect();
    if lines.is_empty() {
        return Ok("Bu dönem için veri yok.".into());
    }
    let n = lines.len();
    Ok(cap(lines, n, ""))
}

/// Son çalınan 25 şarkı.
pub fn recent() -> Res<String> {
    let v = api("GET", "/me/player/recently-played?limit=25", None)?;
    let lines: Vec<String> = v["items"].as_array().into_iter().flatten().map(|it| track_line(&it["track"])).collect();
    Ok(if lines.is_empty() { "Son çalınan şarkı yok.".into() } else { lines.join("\n") })
}

/// Arama: `kind` track|artist|playlist|album; ilk 8 sonuç, URI'leriyle.
pub fn search(q: &str, kind: &str) -> Res<String> {
    if !matches!(kind, "track" | "artist" | "playlist" | "album") {
        return Err("tür track, artist, playlist ya da album olmalı".into());
    }
    let q = q.trim();
    if q.is_empty() {
        return Err("arama metni boş".into());
    }
    let v = api("GET", &format!("/search?q={}&type={kind}&limit=8", enc(q)), None)?;
    let lines: Vec<String> = v[format!("{kind}s")]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|it| !it.is_null())
        .map(|it| match kind {
            "track" => format!("{} | albüm: {} | {}", track_line(it), s(&it["album"]["name"]), s(&it["uri"])),
            "artist" => {
                let genres: Vec<&str> =
                    it["genres"].as_array().into_iter().flatten().filter_map(Value::as_str).take(3).collect();
                let g = if genres.is_empty() { String::new() } else { format!(" ({})", genres.join(", ")) };
                format!("{}{g} | {}", s(&it["name"]), s(&it["uri"]))
            }
            "playlist" => {
                let count = it["items"]["total"]
                    .as_u64()
                    .or(it["tracks"]["total"].as_u64())
                    .map(|n| format!(" | {n} şarkı"))
                    .unwrap_or_default();
                format!("{} | sahibi: {}{count} | {}", s(&it["name"]), s(&it["owner"]["display_name"]), s(&it["uri"]))
            }
            _ => {
                let year = s(&it["release_date"]).get(..4).unwrap_or("");
                format!("{} — {} ({year}) | {}", who(it), s(&it["name"]), s(&it["uri"]))
            }
        })
        .collect();
    Ok(if lines.is_empty() { format!("'{q}' için sonuç yok.") } else { lines.join("\n") })
}

/// Çalar. `target`: liste/albüm/sanatçı URI'si ya da kimliği (bağlam olarak), şarkı URI'si ya da
/// virgülle ayrılmış birkaçı, "liked" (beğenilenler), "" (kaldığı yerden). Etkin cihaz yoksa
/// birini uyandırır.
pub fn play(target: &str, shuffle: Option<bool>) -> Res<String> {
    let target = target.trim();
    let body = if target.is_empty() { None } else { Some(play_body(target)?) };
    let liked = body.as_ref().is_some_and(|b| s(&b["context_uri"]).ends_with(":collection"));
    with_device(|dev| {
        if let Some(on) = shuffle {
            api("PUT", &format!("/me/player/shuffle?state={on}{}", dev_q(dev, '&')), None)?;
        }
        let path = format!("/me/player/play{}", dev_q(dev, '?'));
        match api("PUT", &path, body.clone()) {
            // Beğenilenler bağlamı kabul edilmezse ilk 50 beğenilen şarkı.
            Err(e) if liked && e.to_string() != NO_DEVICE => {
                let v = api("GET", "/me/tracks?limit=50", None)?;
                let uris: Vec<&str> =
                    v["items"].as_array().into_iter().flatten().filter_map(|it| it["track"]["uri"].as_str()).collect();
                if uris.is_empty() {
                    return Err(e);
                }
                api("PUT", &path, Some(json!({ "uris": uris })))
            }
            r => r,
        }
    })?;
    std::thread::sleep(Duration::from_millis(800));
    let now = brief();
    Ok(if now.is_empty() { "Çalmaya başladı.".into() } else { now })
}

/// /me/player/play gövdesi.
fn play_body(target: &str) -> Res<Value> {
    let low = target.to_lowercase();
    if matches!(low.as_str(), "liked" | "beğenilenler" | "beğenilen şarkılar" | "collection") {
        return Ok(json!({ "context_uri": format!("spotify:user:{}:collection", user_id()?) }));
    }
    let parts: Vec<&str> = target.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.len() > 1 {
        let uris = parts.iter().map(|p| item_uri(p)).collect::<Res<Vec<_>>>()?;
        return Ok(json!({ "uris": uris }));
    }
    let uri = resolve(target)?;
    Ok(if uri.contains(":track:") || uri.contains(":episode:") {
        json!({ "uris": [uri] })
    } else {
        json!({ "context_uri": uri })
    })
}

/// Şarkı ya da bölüm URI'si (çıplak kimlik şarkı sayılır).
fn item_uri(t: &str) -> Res<String> {
    match parse_target(t) {
        Some((k, id)) if k.is_empty() || k == "track" => Ok(format!("spotify:track:{id}")),
        Some((k, id)) if k == "episode" => Ok(format!("spotify:episode:{id}")),
        _ => Err(format!("'{}' bir şarkı değil (sıraya ve şarkı listesine yalnızca şarkı girer).", t.trim()).into()),
    }
}

/// Denetim: pause, resume, next, previous, seek (sn), volume (0-100), shuffle (1/0),
/// repeat (0 kapalı, 1 liste, 2 şarkı).
pub fn control(action: &str, value: f64) -> Res<String> {
    let (method, path, done) = match action {
        "pause" => ("PUT", "/me/player/pause".to_string(), "Duraklatıldı.".to_string()),
        "resume" | "play" => ("PUT", "/me/player/play".into(), String::new()),
        "next" => ("POST", "/me/player/next".into(), String::new()),
        "previous" => ("POST", "/me/player/previous".into(), String::new()),
        "seek" => {
            let ms = (value.max(0.0) * 1000.0) as u64;
            ("PUT", format!("/me/player/seek?position_ms={ms}"), format!("{} konumuna sarıldı.", clock(ms)))
        }
        "volume" => {
            let p = value.round().clamp(0.0, 100.0) as u32;
            ("PUT", format!("/me/player/volume?volume_percent={p}"), format!("Ses %{p}."))
        }
        "shuffle" => {
            let on = value != 0.0;
            (
                "PUT",
                format!("/me/player/shuffle?state={on}"),
                format!("Karıştırma {}.", if on { "açık" } else { "kapalı" }),
            )
        }
        "repeat" => {
            let (state, tr) = match value.round() as i64 {
                1 => ("context", "liste"),
                2 => ("track", "şarkı"),
                _ => ("off", "kapalı"),
            };
            ("PUT", format!("/me/player/repeat?state={state}"), format!("Tekrar: {tr}."))
        }
        _ => return Err(format!("bilinmeyen eylem: {action}").into()),
    };
    with_device(|dev| api(method, &format!("{path}{}", dev_q(dev, if path.contains('?') { '&' } else { '?' })), None))?;
    if !done.is_empty() {
        return Ok(done);
    }
    std::thread::sleep(Duration::from_millis(700));
    let now = brief();
    Ok(if now.is_empty() { "Tamam.".into() } else { now })
}

/// Şarkıyı sıraya ekler.
pub fn queue(target: &str) -> Res<String> {
    let uri = item_uri(target)?;
    with_device(|dev| api("POST", &format!("/me/player/queue?uri={}{}", enc(&uri), dev_q(dev, '&')), None))?;
    Ok(format!("Sıraya eklendi: {uri}"))
}

/// Çalan şarkıyı beğenilenlere ekler ya da çıkarır.
pub fn like(on: bool) -> Res<String> {
    let v = api("GET", "/me/player/currently-playing?additional_types=track,episode", None)?;
    let item = &v["item"];
    let uri = s(&item["uri"]);
    if uri.is_empty() {
        return Err("Şu an çalan bir şarkı yok.".into());
    }
    // Şubat 2026: kitaplık /me/library?uris=... ile.
    api(if on { "PUT" } else { "DELETE" }, &format!("/me/library?uris={}", enc(uri)), None)?;
    Ok(format!("{}: {}", if on { "Beğenilenlere eklendi" } else { "Beğenilenlerden çıkarıldı" }, track_line(item)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn sha256_vectors() {
        assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(hex(&sha256(&[b'a'; 1000])), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");
    }

    #[test]
    fn pkce_rfc7636() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let v = b64url(&random(48).unwrap());
        assert_eq!(v.len(), 64);
        assert!(v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(v, b64url(&random(48).unwrap()));
    }

    #[test]
    fn url_coding() {
        assert_eq!(enc("a b&c/ç~"), "a%20b%26c%2F%C3%A7~");
        assert_eq!(enc(REDIRECT), "http%3A%2F%2F127.0.0.1%3A8737%2Fspotify");
        assert_eq!(dec("a%20b+c%C3%A7%2"), "a b cç%2");
        assert_eq!(dec("%zz%"), "%zz%");
        let q = parse_query("code=AQB%2Fx-y_z&state=s%3D1&error");
        assert_eq!(
            q,
            vec![("code".into(), "AQB/x-y_z".into()), ("state".into(), "s=1".into()), ("error".into(), String::new())]
        );
        assert_eq!(form_encode(&[("a", "1 2"), ("b", "x/y")]), "a=1%202&b=x%2Fy");
    }

    #[test]
    fn targets() {
        let p = |t: &str| parse_target(t).map(|(k, i)| format!("{k}:{i}"));
        assert_eq!(p("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M").as_deref(), Some("playlist:37i9dQZF1DXcBWIGoYBM5M"));
        assert_eq!(
            p("https://open.spotify.com/intl-tr/track/4uLU6hMCjMI75M1A2tKUQC?si=abc").as_deref(),
            Some("track:4uLU6hMCjMI75M1A2tKUQC")
        );
        assert_eq!(
            p("https://open.spotify.com/album/1DFixLWuPkv3KT3TnV35m3").as_deref(),
            Some("album:1DFixLWuPkv3KT3TnV35m3")
        );
        assert_eq!(p("4uLU6hMCjMI75M1A2tKUQC").as_deref(), Some(":4uLU6hMCjMI75M1A2tKUQC"));
        assert_eq!(p("lofi beats"), None);
        assert_eq!(item_uri("spotify:track:abc").unwrap(), "spotify:track:abc");
        assert!(item_uri("spotify:album:abc").is_err());
        assert_eq!(resolve("spotify:user:me:collection").unwrap(), "spotify:user:me:collection");
        assert_eq!(resolve("spotify:artist:0OdUWJ0sBjDrqHygGUXeCF").unwrap(), "spotify:artist:0OdUWJ0sBjDrqHygGUXeCF");
    }

    #[test]
    fn errors() {
        let e = |st, b: &str| error_text(st, b.as_bytes());
        assert_eq!(
            e(
                404,
                r#"{"error":{"status":404,"message":"Player command failed: No active device found","reason":"NO_ACTIVE_DEVICE"}}"#
            ),
            NO_DEVICE
        );
        assert!(e(403, r#"{"error":{"status":403,"message":"Player command failed: Premium required","reason":"PREMIUM_REQUIRED"}}"#).contains("Premium"));
        assert!(e(503, "").contains("503"));
    }

    /// Tarayıcı yerine yerel bir bağlantıyla dönüş akışı (8737 değil, boş bir port).
    #[test]
    fn callback() {
        let send = |port: u16, target: &'static str| {
            std::thread::spawn(move || {
                let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
                write!(s, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
                let mut out = String::new();
                let _ = s.read_to_string(&mut out);
                out
            })
        };
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        // Önce favicon (404, beklemeye devam), sonra asıl dönüş: sırayla.
        let client = std::thread::spawn(move || {
            let fav = send(port, "/favicon.ico").join().unwrap();
            let page = send(port, "/spotify?code=KOD%2F1&state=xyz").join().unwrap();
            (fav, page)
        });
        assert_eq!(wait_code(&l, "xyz", Duration::from_secs(10)).unwrap(), "KOD/1");
        let (fav, page) = client.join().unwrap();
        assert!(fav.starts_with("HTTP/1.1 404"));
        assert!(page.starts_with("HTTP/1.1 200") && page.contains("bağlandı"));

        let h = send(port, "/spotify?code=x&state=YANLIS");
        assert!(wait_code(&l, "xyz", Duration::from_secs(10)).is_err());
        assert!(h.join().unwrap().starts_with("HTTP/1.1 400"));
        let h = send(port, "/spotify?error=access_denied&state=xyz");
        assert!(wait_code(&l, "xyz", Duration::from_secs(10)).unwrap_err().to_string().contains("access_denied"));
        h.join().unwrap();
    }

    /// WinHTTP gövdesiz PUT/POST'ta Content-Length: 0 göndermeli (yoksa Spotify 411 der).
    #[test]
    fn empty_put_has_length() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = vec![0u8; 4096];
            let n = s.read(&mut buf).unwrap();
            let _ = s.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n");
            String::from_utf8_lossy(&buf[..n]).to_lowercase()
        });
        let r = net::request(
            "PUT",
            &format!("http://127.0.0.1:{port}/me/player/play"),
            &[("authorization", "Bearer x")],
            &[],
        )
        .unwrap();
        assert_eq!(r.status, 204);
        assert!(h.join().unwrap().contains("content-length: 0"));
    }
}
