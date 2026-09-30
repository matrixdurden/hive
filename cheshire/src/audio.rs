//! `iAudio`: sistem sesinin (WASAPI loopback) spektrumu ve dalga biçimi.
//! Yalnızca etkin duvar kâğıdı `iAudio` kullanıyorsa ve motor duraklatılmamışsa çalışır.
//!
//! Çıktı Shadertoy/WebAudio ile uyumlu 512x2 bayt: 1. satır frekans (AnalyserNode gibi
//! −100…−30 dB → 0…255, 0.8 zaman yumuşatması), 2. satır dalga biçimi (128 = sessizlik).

use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};

use crate::log;
use crate::render::AUDIO_WIDTH;
use crate::util::Res;

const N: usize = 1024; // FFT boyu → 512 frekans kutusu
const W: usize = AUDIO_WIDTH as usize;
pub type Frame = [u8; W * 2];

pub struct Capture {
    latest: Arc<Mutex<Frame>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn start() -> Self {
        let latest = Arc::new(Mutex::new(silence()));
        let stop = Arc::new(AtomicBool::new(false));
        let (l, s) = (latest.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("ses".into())
            .spawn(move || {
                // Varsayılan cihaz değişirse ya da hata olursa kısa bir aradan sonra yeniden bağlan.
                while !s.load(Ordering::Relaxed) {
                    if let Err(e) = unsafe { run(&l, &s) } {
                        log!("ses yakalama: {e}");
                        *l.lock().unwrap() = silence();
                        std::thread::sleep(Duration::from_secs(2));
                    }
                }
            })
            .ok();
        log!("ses yakalama başladı");
        Self { latest, stop, thread }
    }

    pub fn latest(&self) -> Frame {
        *self.latest.lock().unwrap()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        log!("ses yakalama durdu");
    }
}

fn silence() -> Frame {
    let mut f = [0u8; W * 2];
    f[W..].fill(128);
    f
}

unsafe fn run(latest: &Mutex<Frame>, stop: &AtomicBool) -> Res<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let result = capture_loop(latest, stop);
        CoUninitialize();
        result
    }
}

unsafe fn capture_loop(latest: &Mutex<Frame>, stop: &AtomicBool) -> Res<()> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = client.GetMixFormat()?;
        let fmt = *format;
        let channels = fmt.nChannels.max(1) as usize;
        let bits = fmt.wBitsPerSample;
        // Paylaşımlı modda karışım biçimi neredeyse her zaman 32 bit float; 16 bit tamsayıyı da destekle.
        let float = bits == 32 && {
            let tag = fmt.wFormatTag as u32;
            tag == WAVE_FORMAT_IEEE_FLOAT
                || (tag == WAVE_FORMAT_EXTENSIBLE
                    && std::ptr::addr_of!((*(format as *const WAVEFORMATEXTENSIBLE)).SubFormat).read_unaligned()
                        == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT)
        };
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 200_000, 0, format, None);
        CoTaskMemFree(Some(format as *const _));
        init?;
        if !float && bits != 16 {
            return Err(format!("desteklenmeyen ses biçimi: {bits} bit").into());
        }

        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;

        let mut ring = vec![0f32; N];
        let mut head = 0usize;
        let mut smooth = vec![0f32; N / 2];
        let window: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / N as f32).cos()).collect();
        let mut last_packet = Instant::now();
        let mut last_fft = Instant::now();

        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(8));
            let mut got = false;
            while capture.GetNextPacketSize()? > 0 {
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                for f in 0..frames as usize {
                    let mut s = 0.0;
                    if !silent {
                        for c in 0..channels {
                            let i = f * channels + c;
                            s += if float { *(data as *const f32).add(i) } else { *(data as *const i16).add(i) as f32 / 32768.0 };
                        }
                    }
                    ring[head] = s / channels as f32;
                    head = (head + 1) % N;
                }
                capture.ReleaseBuffer(frames)?;
                got = true;
            }
            // Hiçbir şey çalmıyorsa loopback paket göndermez: sessizlikle doldur.
            if got {
                last_packet = Instant::now();
            } else if last_packet.elapsed() > Duration::from_millis(100) {
                ring.fill(0.0);
            }
            if last_fft.elapsed() < Duration::from_millis(15) {
                continue;
            }
            last_fft = Instant::now();

            let mut re: Vec<f32> = (0..N).map(|i| ring[(head + i) % N] * window[i]).collect();
            let mut im = vec![0f32; N];
            fft(&mut re, &mut im);
            let mut out = [0u8; W * 2];
            for k in 0..N / 2 {
                let mag = (re[k] * re[k] + im[k] * im[k]).sqrt() / N as f32;
                smooth[k] = 0.8 * smooth[k] + 0.2 * mag;
                let db = 20.0 * (smooth[k] + 1e-12).log10();
                out[k] = ((db + 100.0) / 70.0 * 255.0).clamp(0.0, 255.0) as u8;
            }
            for i in 0..W {
                let s = ring[(head + N - W + i) % N];
                out[W + i] = (128.0 + s.clamp(-1.0, 1.0) * 127.0) as u8;
            }
            *latest.lock().unwrap() = out;
        }
        client.Stop()?;
        Ok(())
    }
}

/// Yerinde, özyinelemesiz radix-2 FFT. `re.len()` 2'nin kuvveti olmalı.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                (cr, ci) = (cr * wr - ci * wi, cr * wi + ci * wr);
            }
        }
        len <<= 1;
    }
}
