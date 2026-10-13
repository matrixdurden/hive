//! Müzikle yanan ışık: varsayılan çıkış cihazının sesi (WASAPI loopback) dinlenir, vuruşlar
//! bulunur, her vuruşta ışığa komut gider. Vuruş bulma matrix-led'inkiyle aynı: 1024 örneklik
//! bloklarda bas (40–180 Hz), orta (180–2500) ve tiz (2500–9000) bantların enerjisi son 48 bloğun
//! ortalamasının `ortalama + sapma × hassasiyet` üstüne çıkınca vuruş sayılır.
//!
//! Ses yokken loopback paket göndermez; ölçer söner. Varsayılan cihaz değişirse yenisine geçilir.

use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};

use crate::link::Tx;
use crate::surucu::Cmd;

const N: usize = 1024;
const HISTORY: usize = 48;
/// Varsayılan cihaz bu aralıkla yeniden sorulur.
const DEVICE_CHECK: Duration = Duration::from_secs(2);
/// Flaşta ışık bu kadar parlak kalır.
const FLASH: Duration = Duration::from_millis(90);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum React {
    /// Her vuruşta sıradaki renk.
    Colors,
    /// Loş bekler, her vuruşta parlar.
    Flash,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// Yalnızca bas davul.
    Bass,
    /// Bas, trampet ya da zil: hangisi olursa.
    All,
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub react: React,
    pub source: Source,
    /// Eşik çarpanı: küçüldükçe daha çok vuruş.
    pub sensitivity: f32,
    /// Flaşın tepe parlaklığı (kullanıcının parlaklığı).
    pub brightness: f32,
}

/// Sayfadaki ölçer için son durum.
#[derive(Clone, Copy, Default)]
pub struct Meter {
    /// Bas enerjisinin eşiğe oranı (1 = eşikte).
    pub bass: f32,
    pub bpm: f32,
    pub last_beat: Option<Instant>,
    /// Ses geliyor mu.
    pub sound: bool,
}

pub struct Music {
    cfg: Arc<Mutex<Config>>,
    meter: Arc<Mutex<Meter>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Music {
    pub fn start(tx: Tx, cfg: Config) -> Self {
        let cfg = Arc::new(Mutex::new(cfg));
        let meter = Arc::new(Mutex::new(Meter::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (c, m, s) = (cfg.clone(), meter.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("lights-muzik".into())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }
                let mut beat = Beat::new(tx, c.lock().unwrap().react, c.lock().unwrap().brightness);
                while !s.load(Ordering::Relaxed) {
                    if let Err(e) = unsafe { capture(&c, &m, &s, &mut beat) } {
                        ortak::log!("lights: ses yakalanamadı · {}", e.message());
                        *m.lock().unwrap() = Meter::default();
                        std::thread::sleep(Duration::from_secs(2));
                    }
                }
                unsafe {
                    CoUninitialize();
                }
            })
            .ok();
        ortak::log!("lights: müzik modu başladı");
        Self { cfg, meter, stop, thread }
    }

    pub fn set(&self, cfg: Config) {
        *self.cfg.lock().unwrap() = cfg;
    }

    pub fn meter(&self) -> Meter {
        *self.meter.lock().unwrap()
    }
}

impl Drop for Music {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        ortak::log!("lights: müzik modu durdu");
    }
}

/// Vuruşa tepki: komutları gönderir.
struct Beat {
    tx: Tx,
    react: React,
    brightness: f32,
    hue: f32,
    /// Flaştan sonra loşa dönülecek an.
    dim_at: Option<Instant>,
}

impl Beat {
    fn new(tx: Tx, react: React, brightness: f32) -> Self {
        let mut b = Beat { tx, react, brightness, hue: 0.0, dim_at: None };
        b.enter();
        b
    }

    fn low(&self) -> f32 {
        (self.brightness * 0.15).max(0.02)
    }

    /// Tepki türüne girerken ışığın başlangıç hali.
    fn enter(&mut self) {
        match self.react {
            React::Colors => self.tx.send(Cmd::Brightness(self.brightness)),
            React::Flash => self.tx.send(Cmd::Brightness(self.low())),
        }
    }

    fn config(&mut self, cfg: &Config) {
        let changed = cfg.react != self.react || cfg.brightness != self.brightness;
        self.react = cfg.react;
        self.brightness = cfg.brightness;
        if changed {
            self.dim_at = None;
            self.enter();
        }
    }

    fn hit(&mut self, now: Instant) {
        match self.react {
            React::Colors => {
                // Altın açıyla ilerleyen ton: art arda gelen renkler hep belirgin farklı.
                self.hue = (self.hue + 137.5) % 360.0;
                self.tx.send(Cmd::Color(hsv(self.hue, 1.0)));
            }
            React::Flash => {
                self.tx.send(Cmd::Brightness(self.brightness));
                self.dim_at = Some(now + FLASH);
            }
        }
    }

    fn tick(&mut self, now: Instant) {
        if self.dim_at.is_some_and(|t| now >= t) {
            self.dim_at = None;
            self.tx.send(Cmd::Brightness(self.low()));
        }
    }
}

/// Ton (derece) ve doygunluktan tam parlak RGB.
pub fn hsv(h: f32, s: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let m = 1.0 - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [((r + m) * 255.0).round() as u8, ((g + m) * 255.0).round() as u8, ((b + m) * 255.0).round() as u8]
}

/// Bir bandın son blokları ve son vuruşu.
struct Band {
    lo: f32,
    hi: f32,
    cooldown: Duration,
    history: VecDeque<f32>,
    last: Option<Instant>,
}

impl Band {
    fn new(lo: f32, hi: f32, cooldown_ms: u64) -> Self {
        Band { lo, hi, cooldown: Duration::from_millis(cooldown_ms), history: VecDeque::with_capacity(HISTORY), last: None }
    }

    /// Bloğun enerjisini ekler: (vuruş mu, eşiğe oran).
    fn push(&mut self, energy: f32, sensitivity: f32, now: Instant) -> (bool, f32) {
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(energy);
        let n = self.history.len() as f32;
        let mean = self.history.iter().sum::<f32>() / n;
        let sd = (self.history.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n).sqrt();
        let threshold = mean + sd * sensitivity;
        let ratio = if threshold > 0.0 { energy / threshold } else { 0.0 };
        let beat = self.history.len() >= 12 && energy > threshold && self.last.is_none_or(|t| now - t > self.cooldown);
        if beat {
            self.last = Some(now);
        }
        (beat, ratio)
    }
}

/// Varsayılan çıkış cihazını dinler; cihaz değişince ya da durdurulunca döner.
unsafe fn capture(cfg: &Mutex<Config>, meter: &Mutex<Meter>, stop: &AtomicBool, beat: &mut Beat) -> windows::core::Result<()> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let id = device.GetId()?;
        let device_id = id.to_string().unwrap_or_default();
        CoTaskMemFree(Some(id.0 as *const _));

        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = client.GetMixFormat()?;
        let fmt = *format;
        let channels = fmt.nChannels.max(1) as usize;
        let rate = fmt.nSamplesPerSec.max(1) as f32;
        let bits = fmt.wBitsPerSample;
        let float = bits == 32 && {
            let tag = fmt.wFormatTag as u32;
            tag == WAVE_FORMAT_IEEE_FLOAT
                || (tag == WAVE_FORMAT_EXTENSIBLE
                    && std::ptr::addr_of!((*(format as *const WAVEFORMATEXTENSIBLE)).SubFormat).read_unaligned() == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT)
        };
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 200_000, 0, format, None);
        CoTaskMemFree(Some(format as *const _));
        init?;
        if !float && bits != 16 {
            return Err(windows::core::Error::new(windows::core::HRESULT(-1), format!("{bits} bit ses")));
        }
        let cap: IAudioCaptureClient = client.GetService()?;
        client.Start()?;

        let window: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / (N - 1) as f32).cos()).collect();
        let mut bands = [Band::new(40.0, 180.0, 180), Band::new(180.0, 2500.0, 120), Band::new(2500.0, 9000.0, 80)];
        let mut block: Vec<f32> = Vec::with_capacity(N);
        let mut kicks: VecDeque<Instant> = VecDeque::with_capacity(12);
        let mut last_packet = Instant::now();
        let mut last_check = Instant::now();

        let result = loop {
            if stop.load(Ordering::Relaxed) {
                break Ok(());
            }
            std::thread::sleep(Duration::from_millis(8));
            let now = Instant::now();
            let c = *cfg.lock().unwrap();
            beat.config(&c);
            beat.tick(now);

            if now - last_check >= DEVICE_CHECK {
                last_check = now;
                let current = enumerator
                    .GetDefaultAudioEndpoint(eRender, eConsole)
                    .and_then(|d| d.GetId())
                    .map(|p| {
                        let s = p.to_string().unwrap_or_default();
                        CoTaskMemFree(Some(p.0 as *const _));
                        s
                    })
                    .unwrap_or_default();
                if current != device_id {
                    ortak::log!("lights: varsayılan ses cihazı değişti");
                    break Ok(());
                }
            }

            let mut got = false;
            loop {
                let size = match cap.GetNextPacketSize() {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = client.Stop();
                        return Err(e);
                    }
                };
                if size == 0 {
                    break;
                }
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                for f in 0..frames as usize {
                    let mut s = 0.0;
                    if !silent {
                        for ch in 0..channels {
                            let i = f * channels + ch;
                            s += if float { *(data as *const f32).add(i) } else { *(data as *const i16).add(i) as f32 / 32768.0 };
                        }
                    }
                    block.push(s / channels as f32);
                    if block.len() == N {
                        analyze(&block, &window, rate, &c, &mut bands, &mut kicks, meter, beat, now);
                        block.clear();
                    }
                }
                cap.ReleaseBuffer(frames)?;
                got = true;
            }
            if got {
                last_packet = now;
            } else if now - last_packet > Duration::from_millis(200) {
                let mut m = meter.lock().unwrap();
                m.sound = false;
                m.bass = 0.0;
            }
        };
        let _ = client.Stop();
        result
    }
}

#[allow(clippy::too_many_arguments)]
fn analyze(
    block: &[f32],
    window: &[f32],
    rate: f32,
    cfg: &Config,
    bands: &mut [Band; 3],
    kicks: &mut VecDeque<Instant>,
    meter: &Mutex<Meter>,
    beat: &mut Beat,
    now: Instant,
) {
    let mut re: Vec<f32> = block.iter().zip(window).map(|(s, w)| s * w).collect();
    let mut im = vec![0f32; N];
    fft(&mut re, &mut im);
    let bin = rate / N as f32;
    let mut hits = [false; 3];
    let mut bass_ratio = 0.0;
    for (k, band) in bands.iter_mut().enumerate() {
        let (mut sum, mut n) = (0.0, 0);
        for i in 0..N / 2 + 1 {
            let f = i as f32 * bin;
            if f >= band.lo && f <= band.hi {
                sum += (re[i] * re[i] + im[i] * im[i]).sqrt();
                n += 1;
            }
        }
        let energy = if n > 0 { sum / n as f32 } else { 0.0 };
        let (hit, ratio) = band.push(energy, cfg.sensitivity, now);
        hits[k] = hit;
        if k == 0 {
            bass_ratio = ratio;
        }
    }
    if hits[0] {
        if kicks.len() == 12 {
            kicks.pop_front();
        }
        kicks.push_back(now);
    }
    let fire = match cfg.source {
        Source::Bass => hits[0],
        Source::All => hits.iter().any(|h| *h),
    };
    if fire {
        beat.hit(now);
    }
    let bpm = if kicks.len() >= 2 {
        let mut gaps: Vec<f32> = kicks.iter().zip(kicks.iter().skip(1)).map(|(a, b)| (*b - *a).as_secs_f32()).collect();
        gaps.sort_by(f32::total_cmp);
        let mid = gaps.len() / 2;
        let median = if gaps.len().is_multiple_of(2) { (gaps[mid - 1] + gaps[mid]) / 2.0 } else { gaps[mid] };
        if median > 0.0 { 60.0 / median } else { 0.0 }
    } else {
        0.0
    };
    let mut m = meter.lock().unwrap();
    m.bass = bass_ratio;
    m.bpm = bpm;
    m.sound = true;
    if fire {
        m.last_beat = Some(now);
    }
}

/// Yerinde, özyinelemesiz radix-2 FFT (duvar kâğıdı motorundakiyle aynı).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renkler() {
        assert_eq!(hsv(0.0, 1.0), [255, 0, 0]);
        assert_eq!(hsv(120.0, 1.0), [0, 255, 0]);
        assert_eq!(hsv(240.0, 1.0), [0, 0, 255]);
        assert_eq!(hsv(60.0, 0.0), [255, 255, 255]);
    }

    /// 120 BPM'lik yapay bas vuruşu: her vuruş bulunur, BPM ~120 çıkar.
    #[test]
    fn vurus_bulur() {
        let rate = 48_000.0;
        let window: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / (N - 1) as f32).cos()).collect();
        let mut bands = [Band::new(40.0, 180.0, 180), Band::new(180.0, 2500.0, 120), Band::new(2500.0, 9000.0, 80)];
        let mut kicks = VecDeque::new();
        let meter = Mutex::new(Meter::default());
        let mut beat = Beat { tx: Tx::test(), react: React::Colors, brightness: 1.0, hue: 0.0, dim_at: None };
        let cfg = Config { react: React::Colors, source: Source::Bass, sensitivity: 1.5, brightness: 1.0 };
        let start = Instant::now();
        let blocks_per_beat = (0.5 * rate / N as f32).round() as usize; // 500 ms
        let mut beats = 0;
        for b in 0..blocks_per_beat * 20 {
            let kick = b % blocks_per_beat == 0;
            let block: Vec<f32> = (0..N)
                .map(|i| {
                    let t = (b * N + i) as f32 / rate;
                    let hum = 0.02 * (2.0 * PI * 1000.0 * t).sin() + 0.01 * ((i * 7919 % 101) as f32 / 101.0 - 0.5);
                    if kick { hum + 0.8 * (2.0 * PI * 80.0 * t).sin() } else { hum }
                })
                .collect();
            let now = start + Duration::from_secs_f32(b as f32 * N as f32 / rate);
            analyze(&block, &window, rate, &cfg, &mut bands, &mut kicks, &meter, &mut beat, now);
            if meter.lock().unwrap().last_beat == Some(now) {
                beats += 1;
            }
        }
        assert!(beats >= 17, "{beats} vuruş");
        let bpm = meter.lock().unwrap().bpm;
        assert!((bpm - 120.0).abs() < 4.0, "{bpm} BPM");
    }
}
