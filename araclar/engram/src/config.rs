//! `ayarlar.ini`: satır başına `anahtar=değer`, %LOCALAPPDATA%\Programs\engram altında. Bilinmeyen
//! satırlar yok sayılır. API anahtarları DPAPI ile (kullanıcıya bağlı) şifreli, base64 olarak
//! durur: dosya başka kullanıcıya ya da makineye taşınsa açılmaz. Asla loglanmaz.

use std::path::PathBuf;

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData};

use crate::util::data_dir;

/// Komutlardan ve bilgisayar kontrolünden önce onay.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Confirm {
    /// Her komutta ve kontrolü almadan önce sorar.
    Always,
    /// Yalnızca riskli komutlarda sorar (silme, kurma, kapatma, push...).
    Risky,
    Never,
}

impl Confirm {
    pub fn id(self) -> &'static str {
        match self {
            Confirm::Always => "her",
            Confirm::Risky => "riskli",
            Confirm::Never => "hic",
        }
    }
}

/// Bas-konuş kısayolunun çalışma biçimi.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PttMode {
    /// Basılıyken dinler, bırakınca gönderir.
    Hold,
    /// Bir basış başlatır, bir basış ya da sessizlik bitirir.
    Toggle,
}

#[derive(Clone)]
pub struct Config {
    pub narrow: bool,
    /// Son açık sekme.
    pub tab: String,
    pub turkish: bool,
    /// Not klasörü.
    pub vault: PathBuf,
    pub anthropic_key: String,
    pub elevenlabs_key: String,
    pub picovoice_key: String,
    /// Groq: konuşmayı yazıya çevirmek için (ücretsiz).
    pub groq_key: String,
    /// Gemini: yazıya çevirme ve isteğe bağlı ses.
    pub gemini_key: String,
    pub gemini_voice: String,
    pub model: String,
    /// Sohbette düşünme derinliği: low | medium | high.
    pub effort: String,
    /// ElevenLabs ses kimliği ve modeli.
    pub voice: String,
    pub voice_model: String,
    /// Bas-konuş kısayolu (metin, `hotkey::Hotkey::parse`).
    pub hotkey: String,
    pub ptt: PttMode,
    /// Asistanın adı (hitap) ve uyandırma sözü.
    pub name: String,
    pub wake_word: String,
    pub wake: bool,
    /// Aylık API bütçesi (dolar), yalnızca gösterim için.
    pub budget: f64,
    /// Fare/klavye kontrolü açık mı (ekrana bakmak hep açık).
    pub control: bool,
    /// Komutlardan ve bilgisayar kontrolünden önce onay.
    pub confirm: Confirm,
    /// Sesli cevaplar: ElevenLabs anahtarı varsa o, yoksa Windows'un sesi. "windows" zorlar.
    pub tts: String,
    /// Mikrofonun kimliği; boşsa Windows'un varsayılanı.
    pub mic: String,
    /// Yalnızca kullanıcının sesine tepki ver (müzik, başkaları yok sayılır).
    pub voice_lock: bool,
    /// Yerel sesin (XTTS) adı ve hızı.
    pub local_voice: String,
    pub local_speed: f32,
}

impl Default for Config {
    fn default() -> Self {
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
        Config {
            narrow: false,
            tab: String::new(),
            turkish: crate::i18n::system_turkish(),
            vault: home.join("Documents").join("engram"),
            anthropic_key: String::new(),
            elevenlabs_key: String::new(),
            picovoice_key: String::new(),
            groq_key: String::new(),
            gemini_key: String::new(),
            gemini_voice: "Charon".into(),
            model: "claude-opus-5-5".into(),
            effort: "medium".into(),
            voice: String::new(),
            voice_model: "eleven_flash_v2_5".into(),
            hotkey: "F8".into(),
            ptt: PttMode::Hold,
            name: "Matrix".into(),
            wake_word: "hey matrix".into(),
            wake: true,
            budget: 200.0,
            control: true,
            confirm: Confirm::Risky,
            tts: "xtts".into(),
            mic: String::new(),
            voice_lock: true,
            local_voice: "Suad Qasim".into(),
            local_speed: 1.05,
        }
    }
}

fn ini() -> PathBuf {
    data_dir().join("ayarlar.ini")
}

impl Config {
    pub fn load() -> Self {
        let mut c = Config::default();
        let text = std::fs::read_to_string(ini()).unwrap_or_default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "dar" => c.narrow = v == "1",
                "sekme" => c.tab = v.to_string(),
                "dil" => c.turkish = v == "tr",
                "notlar" if !v.is_empty() => c.vault = PathBuf::from(v),
                "anthropic" => c.anthropic_key = unprotect(v),
                "elevenlabs" => c.elevenlabs_key = unprotect(v),
                "picovoice" => c.picovoice_key = unprotect(v),
                "groq" => c.groq_key = unprotect(v),
                "gemini" => c.gemini_key = unprotect(v),
                "gemini_ses" if !v.is_empty() => c.gemini_voice = v.to_string(),
                "model" if !v.is_empty() => c.model = v.to_string(),
                "caba" if matches!(v, "low" | "medium" | "high") => c.effort = v.to_string(),
                "ses" => c.voice = v.to_string(),
                "ses_model" if !v.is_empty() => c.voice_model = v.to_string(),
                "kisayol" => c.hotkey = v.to_string(),
                "kisayol_mod" => c.ptt = if v == "ackapa" { PttMode::Toggle } else { PttMode::Hold },
                "hitap" if !v.is_empty() => c.name = v.to_string(),
                "uyandirma_sozu" if !v.is_empty() => c.wake_word = v.to_string(),
                // Eski "uyandirma" anahtarı arayüzü yokken kapalı kaydedilmişti: yeni anahtar "dinle".
                "dinle" => c.wake = v == "1",
                "butce" => c.budget = v.parse().unwrap_or(c.budget),
                "bilgisayar" => c.control = v != "0",
                "onay" => {
                    c.confirm = match v {
                        "her" => Confirm::Always,
                        "hic" | "0" => Confirm::Never,
                        _ => Confirm::Risky,
                    }
                }
                "ses_motoru" if !v.is_empty() => c.tts = v.to_string(),
                "mikrofon" => c.mic = v.to_string(),
                "sadece_ben" => c.voice_lock = v != "0",
                "yerel_ses" if !v.is_empty() => c.local_voice = v.to_string(),
                "yerel_hiz" => c.local_speed = v.parse::<f32>().unwrap_or(c.local_speed).clamp(0.6, 1.4),
                _ => {}
            }
        }
        c
    }

    pub fn save(&self) {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        let text = format!(
            "dar={}\nsekme={}\ndil={}\nnotlar={}\nanthropic={}\nelevenlabs={}\npicovoice={}\nmodel={}\ncaba={}\nses={}\nses_model={}\nkisayol={}\nkisayol_mod={}\nhitap={}\nuyandirma_sozu={}\ndinle={}\nbutce={}\nbilgisayar={}\nonay={}\nses_motoru={}\nmikrofon={}\ngroq={}\nyerel_ses={}\nyerel_hiz={}\ngemini={}\ngemini_ses={}\nsadece_ben={}\n",
            self.narrow as u8,
            self.tab,
            if self.turkish { "tr" } else { "en" },
            self.vault.display(),
            protect(&self.anthropic_key),
            protect(&self.elevenlabs_key),
            protect(&self.picovoice_key),
            self.model,
            self.effort,
            self.voice,
            self.voice_model,
            self.hotkey,
            if self.ptt == PttMode::Toggle { "ackapa" } else { "basili" },
            self.name,
            self.wake_word,
            self.wake as u8,
            self.budget,
            self.control as u8,
            self.confirm.id(),
            self.tts,
            self.mic,
            protect(&self.groq_key),
            self.local_voice,
            self.local_speed,
            protect(&self.gemini_key),
            self.gemini_voice,
            self.voice_lock as u8,
        );
        let _ = std::fs::write(ini(), text);
    }
}

// --- DPAPI ---

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.len();
        let v = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        s.push(B64[(v >> 18) as usize & 63] as char);
        s.push(B64[(v >> 12) as usize & 63] as char);
        s.push(if n > 1 { B64[(v >> 6) as usize & 63] as char } else { '=' });
        s.push(if n > 2 { B64[v as usize & 63] as char } else { '=' });
    }
    s
}

pub fn unbase64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| B64.iter().position(|&b| b == c).map(|p| p as u32);
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut v = 0u32;
        let mut n = 0;
        for &c in chunk {
            if c == b'=' {
                break;
            }
            v = v << 6 | val(c)?;
            n += 1;
        }
        match n {
            4 => out.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8]),
            3 => out.extend_from_slice(&[(v >> 10) as u8, (v >> 2) as u8]),
            2 => out.push((v >> 4) as u8),
            _ => return None,
        }
    }
    Some(out)
}

/// Kullanıcıya bağlı şifreleme; boş metin boş kalır.
pub(crate) fn protect(secret: &str) -> String {
    if secret.is_empty() {
        return String::new();
    }
    let input = CRYPT_INTEGER_BLOB { cbData: secret.len() as u32, pbData: secret.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        if CryptProtectData(&input, None, None, None, None, 0, &mut out).is_err() {
            return String::new();
        }
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
        base64(&bytes)
    }
}

pub(crate) fn unprotect(encoded: &str) -> String {
    let Some(bytes) = unbase64(encoded).filter(|b| !b.is_empty()) else { return String::new() };
    let input = CRYPT_INTEGER_BLOB { cbData: bytes.len() as u32, pbData: bytes.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        if CryptUnprotectData(&input, None, None, None, None, 0, &mut out).is_err() {
            return String::new();
        }
        let s = String::from_utf8_lossy(std::slice::from_raw_parts(out.pbData, out.cbData as usize)).into_owned();
        let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
        s
    }
}
