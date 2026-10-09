//! Uygulama ile mikrofon efekti (APO) arasındaki ortak bellek.
//!
//! `%ProgramData%\soundboard\bus` dosyasının eşlemi. Tek yazar (uygulama), çok okur (mikrofonu
//! açan her akıştaki APO ve kulaklık çıkışı), kilit yok. Her ses yuvası bir halka tamponudur:
//! yazar dosyayı gerçek zamandan biraz önde çözer, okurlar kendi imleçlerini tutar ve kaynak
//! hızından kendi cihaz hızlarına doğrusal aradeğerlemeyle geçer.
//!
//! Bu dosya hem `soundboard.exe` hem `lyrebird_apo.dll` içine derlenir.

#![allow(dead_code)]

use std::cell::UnsafeCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicU32, AtomicU64};

use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_TEMPORARY, FILE_BEGIN, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, GetFileSizeEx, OPEN_ALWAYS, OPEN_EXISTING, SetEndOfFile, SetFilePointerEx,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MapViewOfFile, PAGE_READONLY, PAGE_READWRITE,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::core::{GUID, PCWSTR, w};

pub const APO_CLSID: GUID = GUID::from_u128(0xcee022e9_7c89_429e_a04c_eda55e405e62);
/// Aynı APO'nun "yedek" kimliği: aynı mikrofonda ana örnek (SFX) çalışıyorsa karıştırmaz.
/// SFX'in atlandığı raw akışlar için EFX'e takılır.
pub const APO_CLSID_FALLBACK: GUID = GUID::from_u128(0x7c9545d7_a140_4bc6_b6d4_62927f5ec8d3);
/// FxProperties içindeki kendi değerlerimiz: `{FMTID},1` yerini aldığımız sürücü efektinin CLSID'i.
pub const FMTID: GUID = GUID::from_u128(0xe4be4264_ef9f_417d_83dc_78d895805e3f);

const MAGIC: u32 = u32::from_le_bytes(*b"lyre");
const VERSION: u32 = 1;
pub const VOICES: usize = 8;
/// Yuva başına halka boyu (kare). 48 kHz'de 1.36 s: yazar 0.3 s önde, okur 1 s geride kalabilir.
pub const RING: usize = 1 << 16;
const MASK: u64 = RING as u64 - 1;
/// Yazarın gerçek zamandan ne kadar önde çözdüğü (saniye).
pub const LEAD: f64 = 0.3;

const IDLE: u32 = 0;
const PLAYING: u32 = 1;

#[repr(C, align(64))]
pub struct Header {
    magic: AtomicU32,
    version: AtomicU32,
    /// Mikrofona giden seslerin kazancı (f32 bitleri).
    gain: AtomicU32,
    /// APO'nun son çalıştığı an (QPC). Arayüzdeki "bağlı" noktası buradan okunur.
    heartbeat: AtomicI64,
}

#[repr(C, align(64))]
pub struct Voice {
    /// Yuvada her yeni ses başladığında bir artar; okur değişimi görünce imlecini yeniden kurar.
    seq: AtomicU32,
    state: AtomicU32,
    rate: AtomicU32,
    /// Sesin başladığı an (QPC). Sonradan açılan akış sesi baştan değil, o anki yerinden duyar.
    start: AtomicI64,
    /// Halkaya şimdiye dek yazılan kare sayısı (mutlak).
    written: AtomicU64,
    /// Dosya bitince toplam kare sayısı; o zamana dek `u64::MAX`.
    total: AtomicU64,
    ring: UnsafeCell<[[f32; 2]; RING]>,
}

#[repr(C)]
pub struct Bus {
    header: Header,
    pub voices: [Voice; VOICES],
}

unsafe impl Sync for Bus {}

pub const SIZE: usize = size_of::<Bus>();

pub fn qpc() -> i64 {
    let mut t = 0;
    unsafe {
        let _ = QueryPerformanceCounter(&mut t);
    }
    t
}

fn qpc_freq() -> f64 {
    let mut f = 0;
    unsafe {
        let _ = QueryPerformanceFrequency(&mut f);
    }
    f.max(1) as f64
}

impl Bus {
    /// Uygulama açılışında: önceki oturumdan kalan yuvaları susturur.
    pub fn reset(&self, gain: f32) {
        for v in &self.voices {
            v.stop();
        }
        self.set_gain(gain);
        self.header.version.store(VERSION, Relaxed);
        self.header.magic.store(MAGIC, Release);
    }

    pub fn valid(&self) -> bool {
        self.header.magic.load(Acquire) == MAGIC && self.header.version.load(Relaxed) == VERSION
    }

    pub fn set_gain(&self, gain: f32) {
        self.header.gain.store(gain.to_bits(), Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.header.gain.load(Relaxed))
    }

    pub fn beat(&self) {
        self.header.heartbeat.store(qpc(), Relaxed);
    }

    /// APO'nun son çalışmasından bu yana geçen süre (saniye).
    pub fn since_beat(&self) -> f64 {
        (qpc() - self.header.heartbeat.load(Relaxed)) as f64 / qpc_freq()
    }
}

// Yazar tarafı (uygulama).
impl Voice {
    pub fn begin(&self, rate: u32) {
        self.state.store(IDLE, Release);
        self.rate.store(rate, Relaxed);
        self.total.store(u64::MAX, Relaxed);
        self.written.store(0, Release);
    }

    pub fn written(&self) -> u64 {
        self.written.load(Relaxed)
    }

    /// En fazla `RING` kare.
    pub fn push(&self, frames: &[[f32; 2]]) {
        let w = self.written.load(Relaxed);
        let n = frames.len().min(RING);
        let at = (w & MASK) as usize;
        let first = n.min(RING - at);
        let ring = self.ring.get() as *mut [f32; 2];
        unsafe {
            std::ptr::copy_nonoverlapping(frames.as_ptr(), ring.add(at), first);
            std::ptr::copy_nonoverlapping(frames.as_ptr().add(first), ring, n - first);
        }
        self.written.store(w + n as u64, Release);
    }

    /// İlk parça yazıldıktan sonra: okurlar sesi bu andan itibaren çalar.
    pub fn publish(&self) {
        self.start.store(qpc(), Relaxed);
        self.state.store(PLAYING, Relaxed);
        self.seq.fetch_add(1, Release);
    }

    pub fn finish(&self) {
        self.total.store(self.written.load(Relaxed), Release);
    }

    pub fn stop(&self) {
        self.state.store(IDLE, Release);
    }

    fn frame(&self, i: u64) -> [f32; 2] {
        unsafe { (self.ring.get() as *const [f32; 2]).add((i & MASK) as usize).read_volatile() }
    }
}

/// Okur tarafı. Her APO örneği ve kulaklık çıkışı kendi `Mixer`ını tutar. Bellek ayırmaz.
pub struct Mixer {
    seen: [u32; VOICES],
    live: [bool; VOICES],
    pos: [f64; VOICES],
    freq: f64,
}

impl Default for Mixer {
    fn default() -> Self {
        Self { seen: [0; VOICES], live: [false; VOICES], pos: [0.0; VOICES], freq: qpc_freq() }
    }
}

impl Mixer {
    /// Çalan ya da yeni başlamış bir ses var mı (sessiz tamponu sıfırlamaya değer mi)?
    pub fn busy(&self, bus: &Bus) -> bool {
        (0..VOICES).any(|i| self.live[i] || bus.voices[i].seq.load(Relaxed) != self.seen[i])
    }

    /// Çalan sesleri `out`a ekler (`channels` kanallı, iç içe f32). Bir şey eklediyse `true`.
    pub fn mix(&mut self, bus: &Bus, out: &mut [f32], channels: usize, rate: f32, gain: f32) -> bool {
        if channels == 0 || rate <= 0.0 || !bus.valid() {
            return false;
        }
        let frames = out.len() / channels;
        let now = qpc();
        let mut any = false;
        for (i, v) in bus.voices.iter().enumerate() {
            let seq = v.seq.load(Acquire);
            if seq != self.seen[i] {
                self.seen[i] = seq;
                self.live[i] = false;
                if v.state.load(Acquire) == PLAYING {
                    let vr = v.rate.load(Relaxed) as f64;
                    let at = (now - v.start.load(Relaxed)) as f64 / self.freq * vr;
                    let (written, total) = (v.written.load(Acquire), v.total.load(Acquire));
                    // Yazar çökmüş ya da bilgisayar yeniden başlamışsa ses bayattır: çalma.
                    if at > -0.5 * vr && at < written.min(total) as f64 {
                        self.pos[i] = at.max(0.0);
                        self.live[i] = true;
                    }
                }
            }
            if !self.live[i] {
                continue;
            }
            let playing = v.state.load(Acquire) == PLAYING;
            let written = v.written.load(Acquire);
            let total = v.total.load(Acquire);
            let step = v.rate.load(Relaxed) as f64 / rate as f64;
            // Okur çok geride kaldıysa (halka üzerine yazıldı) ileri atla.
            let oldest = written.saturating_sub(RING as u64 - 1024) as f64;
            let mut p = self.pos[i].max(oldest);
            for f in 0..frames {
                let i0 = p as u64;
                if i0 + 1 >= written {
                    if written >= total {
                        self.live[i] = false;
                    }
                    break;
                }
                let t = (p - i0 as f64) as f32;
                let (a, b) = (v.frame(i0), v.frame(i0 + 1));
                // Durdurulan ses bu tampon boyunca sönümlenir: tık sesi olmaz.
                let g = if playing { gain } else { gain * (1.0 - f as f32 / frames as f32) };
                let l = (a[0] + (b[0] - a[0]) * t) * g;
                let r = (a[1] + (b[1] - a[1]) * t) * g;
                p += step;
                any = true;
                // Kazanç 0: yalnızca imleç ilerler (yedek örnek susarken yerini kaybetmesin).
                if gain <= 0.0 {
                    continue;
                }
                let o = &mut out[f * channels..f * channels + channels];
                if channels == 2 {
                    o[0] = (o[0] + l).clamp(-1.0, 1.0);
                    o[1] = (o[1] + r).clamp(-1.0, 1.0);
                } else {
                    let m = (l + r) * 0.5;
                    for s in o {
                        *s = (*s + m).clamp(-1.0, 1.0);
                    }
                }
            }
            self.pos[i] = p;
            if !playing {
                self.live[i] = false;
            }
        }
        any
    }
}

/// `{XXXXXXXX-...}`: kayıt defterinin CLSID biçimi.
pub fn guid_str(g: &GUID) -> String {
    format!("{{{g:?}}}")
}

pub fn parse_guid(s: &str) -> Option<GUID> {
    GUID::try_from(s.trim().trim_start_matches('{').trim_end_matches('}')).ok()
}

/// Bir mikrofonun efekt ayarlarının kayıt defteri yolu (HKLM altında).
pub fn fx_key(endpoint: &str) -> String {
    format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\{endpoint}\FxProperties")
}

/// FxProperties'te yerini aldığımız efektin CLSID'inin saklandığı değer adı.
pub fn saved_value() -> String {
    format!("{},1", guid_str(&FMTID).to_lowercase())
}

pub fn dir() -> PathBuf {
    std::env::var_os("ProgramData").map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from).join("lyrebird")
}

/// SYSTEM, Yöneticiler, Kullanıcılar ve LOCAL SERVICE (audiodg.exe) tam erişim.
const SDDL: PCWSTR = w!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;BU)(A;OICI;FA;;;LS)");

fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str().to_string_lossy().encode_utf16().chain(Some(0)).collect()
}

/// Uygulama ve kurulum: klasörü ve dosyayı gerekirse oluşturur, yazılabilir eşler.
pub fn create() -> windows::core::Result<&'static Bus> {
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(SDDL, SDDL_REVISION_1, &mut sd, None)?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let _ = CreateDirectoryW(PCWSTR(wide(&dir()).as_ptr()), Some(&sa));
        let result = map(Some(&sa), true);
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        result
    }
}

/// APO: var olan dosyayı açar. Yazılamıyorsa salt okunur eşler (`false`).
pub fn open() -> windows::core::Result<(&'static Bus, bool)> {
    match map(None, true) {
        Ok(bus) => Ok((bus, true)),
        Err(_) => map(None, false).map(|bus| (bus, false)),
    }
}

fn map(create: Option<&SECURITY_ATTRIBUTES>, write: bool) -> windows::core::Result<&'static Bus> {
    let path = wide(&dir().join("bus"));
    unsafe {
        let access = if write { GENERIC_READ.0 | GENERIC_WRITE.0 } else { GENERIC_READ.0 };
        let file = CreateFileW(
            PCWSTR(path.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            create.map(|sa| sa as *const _),
            if create.is_some() { OPEN_ALWAYS } else { OPEN_EXISTING },
            FILE_ATTRIBUTE_TEMPORARY,
            None,
        )?;
        let result = (|| {
            let mut len = 0i64;
            GetFileSizeEx(file, &mut len)?;
            if (len as usize) < SIZE {
                if !write {
                    return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL));
                }
                SetFilePointerEx(file, SIZE as i64, None, FILE_BEGIN)?;
                SetEndOfFile(file)?;
            }
            let section =
                CreateFileMappingW(file, None, if write { PAGE_READWRITE } else { PAGE_READONLY }, 0, 0, None)?;
            let view = MapViewOfFile(section, if write { FILE_MAP_WRITE } else { FILE_MAP_READ }, 0, 0, SIZE);
            let _ = CloseHandle(section);
            if view.Value.is_null() {
                return Err(windows::core::Error::from_thread());
            }
            // Görünüm süreç ömrünce açık kalır.
            Ok(&*(view.Value as *const Bus))
        })();
        let _ = CloseHandle(file);
        result
    }
}
