//! Son 10 saniye: varsayılan çıkışta çalan sesi (loopback) bellekte tutar; istenince
//! baştaki ve sondaki sessizliği atıp wav olarak yazar. Kaydet denmedikçe diske bir şey
//! yazılmaz.
//!
//! Ses yokken dinlemez: çıkışın seviye ölçeri 200 ms'de bir okunur, ses başlayınca yakalama
//! açılır, 12 saniye sessizlikten sonra kapanır. Böylece boştayken ses motorunu tutmaz,
//! bilgisayarın uykuya geçmesini engellemez. Loopback sessizlikte paket vermez: aradaki süre
//! saate göre sessizlikle doldurulur, "son 10 saniye" gerçekten son 10 saniyedir.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree};

use crate::log;
use crate::util::Res;

/// Kaydedilen süre.
pub const SECONDS: u32 = 10;
const POLL: Duration = Duration::from_millis(200);
const IDLE_CLOSE: Duration = Duration::from_secs(12);
/// Yakalarken paketleri bu aralıkla toplar; varsayılan cihaz değişti mi diye de bakar.
const READ: Duration = Duration::from_millis(30);
const DEVICE_CHECK: Duration = Duration::from_secs(1);
/// Bunun altı sessizlik sayılır (kırpma ve "ses var mı").
const QUIET: f32 = 0.003;

/// Son `SECONDS + 1` saniyelik halka.
struct Ring {
    data: Vec<[f32; 2]>,
    pos: usize,
    len: usize,
    rate: u32,
    /// Son yazılan karenin anı: sonrası sessizliktir.
    last: Option<Instant>,
}

impl Ring {
    fn reset(&mut self, rate: u32) {
        self.data = vec![[0.0; 2]; (rate * (SECONDS + 1)) as usize];
        (self.pos, self.len, self.rate, self.last) = (0, 0, rate, None);
    }

    fn push(&mut self, f: [f32; 2]) {
        let cap = self.data.len();
        self.data[self.pos] = f;
        self.pos = (self.pos + 1) % cap;
        self.len = (self.len + 1).min(cap);
    }

    /// Son `n` kare, eskiden yeniye.
    fn tail(&self, n: usize) -> Vec<[f32; 2]> {
        let (cap, n) = (self.data.len(), n.min(self.len));
        (0..n).map(|i| self.data[(self.pos + cap - n + i) % cap]).collect()
    }
}

pub struct Clipper {
    ring: Arc<Mutex<Ring>>,
    stop: Arc<AtomicBool>,
}

impl Clipper {
    pub fn start() -> Self {
        let ring = Arc::new(Mutex::new(Ring { data: Vec::new(), pos: 0, len: 0, rate: 0, last: None }));
        let stop = Arc::new(AtomicBool::new(false));
        let (r, s) = (ring.clone(), stop.clone());
        let _ = std::thread::Builder::new().name("klip".into()).spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            if let Err(e) = run(&r, &s) {
                log!("son 10 saniye: {e}");
            }
        });
        Self { ring, stop }
    }

    /// Son 10 saniyeyi baştaki ve sondaki sessizliği atıp 16 bit wav olarak yazar. Duyulan bir
    /// ses yoksa `None`, varsa süresi (saniye).
    pub fn save(&self, path: &Path) -> Res<Option<f32>> {
        let (frames, rate) = {
            let ring = self.ring.lock().unwrap();
            let Some(last) = ring.last else { return Ok(None) };
            let rate = ring.rate as f32;
            let want = SECONDS as f32 - last.elapsed().as_secs_f32();
            if want <= 0.0 {
                return Ok(None);
            }
            (ring.tail((want * rate) as usize), ring.rate)
        };
        let loud = |f: &[f32; 2]| f[0].abs() > QUIET || f[1].abs() > QUIET;
        let (Some(a), Some(b)) = (frames.iter().position(loud), frames.iter().rposition(loud)) else {
            return Ok(None);
        };
        let margin = rate as usize / 20;
        let clip = &frames[a.saturating_sub(margin)..(b + margin).min(frames.len())];
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, wav(clip, rate))?;
        Ok(Some(clip.len() as f32 / rate as f32))
    }
}

impl Drop for Clipper {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

/// 16 bit stereo PCM wav.
fn wav(frames: &[[f32; 2]], rate: u32) -> Vec<u8> {
    let data = frames.len() as u32 * 4;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for f in frames {
        for s in f {
            out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
        }
    }
    out
}

fn default_output(e: &IMMDeviceEnumerator) -> Option<(String, IMMDevice)> {
    unsafe {
        let d = e.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let p = d.GetId().ok()?;
        let id = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        Some((id?, d))
    }
}

/// Çıkışı izler: ses başlayınca yakalar, uzun sessizlikte bırakır.
fn run(ring: &Mutex<Ring>, stop: &AtomicBool) -> Res<()> {
    let e: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    // Ölçer cihaz değişene dek tutulur; cihaz saniyede bir yeniden sorulur.
    let mut meter: Option<(String, IAudioMeterInformation, Instant)> = None;
    while !stop.load(Relaxed) {
        std::thread::sleep(POLL);
        if meter.as_ref().is_none_or(|m| m.2.elapsed() > DEVICE_CHECK) {
            meter = default_output(&e).and_then(|(id, d)| {
                let same = meter.as_ref().filter(|m| m.0 == id).map(|m| m.1.clone());
                let m = same.or_else(|| unsafe { d.Activate(CLSCTX_ALL, None).ok() })?;
                Some((id, m, Instant::now()))
            });
        }
        let Some((id, m, _)) = &meter else { continue };
        if unsafe { m.GetPeakValue() }.unwrap_or(0.0) <= 0.0 {
            continue;
        }
        let Some((_, device)) = default_output(&e).filter(|(i, _)| i == id) else { continue };
        if let Err(err) = capture(&e, id, &device, ring, stop) {
            log!("son 10 saniye: yakalanamadı: {err}");
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    Ok(())
}

/// Bir yakalama oturumu: sessizlik sürene ya da varsayılan çıkış değişene dek.
fn capture(e: &IMMDeviceEnumerator, id: &str, device: &IMMDevice, ring: &Mutex<Ring>, stop: &AtomicBool) -> Res<()> {
    unsafe {
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let mix = client.GetMixFormat()?;
        let (tag, bits, rate, ch) = ((*mix).wFormatTag, (*mix).wBitsPerSample, (*mix).nSamplesPerSec, (*mix).nChannels);
        let float = tag == WAVE_FORMAT_IEEE_FLOAT as u16
            || (tag == WAVE_FORMAT_EXTENSIBLE as u16 && { (*(mix as *const WAVEFORMATEXTENSIBLE)).SubFormat }
                == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 10_000_000, 0, mix, None);
        CoTaskMemFree(Some(mix as *const _));
        init?;
        if !float || bits != 32 || ch == 0 {
            return Err("çıkışın ses biçimi desteklenmiyor".into());
        }
        let cap: IAudioCaptureClient = client.GetService()?;
        {
            let mut r = ring.lock().unwrap();
            if r.rate != rate {
                r.reset(rate);
            }
        }
        let ch = ch as usize;
        let start = Instant::now();
        let mut written = 0u64;
        let mut loud_at = Instant::now();
        let mut checked = Instant::now();
        client.Start()?;
        let result = (|| -> Res<()> {
            while !stop.load(Relaxed) && loud_at.elapsed() < IDLE_CLOSE {
                std::thread::sleep(READ);
                loop {
                    let n = cap.GetNextPacketSize()?;
                    if n == 0 {
                        break;
                    }
                    let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                    cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                    let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                    let s = std::slice::from_raw_parts(data as *const f32, frames as usize * ch);
                    let mut r = ring.lock().unwrap();
                    for f in s.chunks_exact(ch) {
                        let frame = if silent {
                            [0.0; 2]
                        } else if ch == 1 {
                            [f[0], f[0]]
                        } else {
                            [f[0], f[1]]
                        };
                        if frame[0].abs() > QUIET || frame[1].abs() > QUIET {
                            loud_at = Instant::now();
                        }
                        r.push(frame);
                    }
                    r.last = Some(Instant::now());
                    drop(r);
                    written += frames as u64;
                    cap.ReleaseBuffer(frames)?;
                }
                // Sessizlikte paket gelmez: saate göre sessizlik ekle.
                let expected = (start.elapsed().as_secs_f64() * rate as f64) as u64;
                if written + (rate as u64 / 10) < expected {
                    let mut r = ring.lock().unwrap();
                    for _ in written..expected {
                        r.push([0.0; 2]);
                    }
                    r.last = Some(Instant::now());
                    written = expected;
                }
                if checked.elapsed() > DEVICE_CHECK {
                    checked = Instant::now();
                    if default_output(e).is_none_or(|(i, _)| i != id) {
                        break;
                    }
                }
            }
            Ok(())
        })();
        let _ = client.Stop();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bilgisayarda ses çalarken: `cargo test -p lyrebird clip -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_clip() {
        let c = Clipper::start();
        std::thread::sleep(Duration::from_secs(4));
        let path = std::env::temp_dir().join("lyrebird-klip-deneme.wav");
        let secs = c.save(&path).unwrap();
        println!("klip: {secs:?} sn, {} bayt", std::fs::metadata(&path).map_or(0, |m| m.len()));
        let _ = std::fs::remove_file(&path);
        assert!(secs.is_some_and(|s| s > 2.0));
    }

    #[test]
    fn wav_header() {
        let w = wav(&[[0.5, -0.5]; 10], 48000);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(w.len(), 44 + 40);
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 48000);
    }
}
