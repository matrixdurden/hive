//! Ses: WASAPI ile mikrofon kaydı ve çalma (paylaşımlı mod, olay güdümlü), WAV yardımcıları ve
//! Windows'un yerleşik konuşma sentezi. İç biçim 16 kHz mono i16; cihazın biçimi ne olursa olsun
//! (float32/i16/i24/i32, her kanal sayısı ve hız) çevrilir. Ses iş parçacıkları COM'u kendileri
//! açar, hata olursa loglayıp yeniden dener; asla çökmez.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, PROPERTYKEY, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, STGM_READ,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows::core::{GUID, PCWSTR};

use crate::log;
use crate::util::{Res, wide};

/// Uygulamanın iç ses biçimi: 16 kHz, mono, i16.
pub const RATE: u32 = 16_000;
/// Bir çerçeve 20 ms.
pub const FRAME: usize = 320;
/// Çalma kuyruğunun hızı: çalınacak ses önce buna çevrilir, cihaz da (gerekirse Windows'un
/// dönüştürücüsüyle) bu hızda açılır.
const QRATE: u32 = 48_000;
/// Varsayılan cihaz bu aralıkla yoklanır; değiştiyse akış yeniden açılır.
const DEVICE_CHECK: Duration = Duration::from_secs(2);
/// Deneme için: Windows'un biçim dönüştürmesini kapatıp cihazın karışım biçimiyle açar.
static RAW_FORMAT: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------------------------
// Yardımcılar

/// İş parçacığında COM'u (MTA) açar, düşünce kapatır. Başka modda açılmışsa dokunmaz.
struct Com(bool);

impl Com {
    fn init() -> Com {
        Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

/// Otomatik sıfırlanan Win32 olayı; WASAPI her periyotta işaretler.
struct Event(HANDLE);

impl Event {
    fn new() -> Res<Event> {
        Ok(Event(unsafe { CreateEventW(None, false, false, PCWSTR::null())? }))
    }

    /// İşaretlendiyse `true`, zaman aşımında `false`.
    fn wait(&self, ms: u32) -> bool {
        unsafe { WaitForSingleObject(self.0, ms) == WAIT_OBJECT_0 }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn to_i16(x: f32) -> i16 {
    (x * 32768.0).clamp(-32768.0, 32767.0) as i16
}

/// RMS (0..1) → algısal seviye: -55 dBFS ve altı 0, -5 dBFS ve üstü 1.
fn perceptual(rms: f32) -> f32 {
    if rms <= 1e-6 {
        return 0.0;
    }
    ((20.0 * rms.log10() + 55.0) / 50.0).clamp(0.0, 1.0)
}

/// Kilidi zehirlenmiş olsa da alır (ses iş parçacıkları panik yapmamalı).
fn relock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `stop` kalkana kadar en çok `d` kadar uyur.
fn nap(stop: &AtomicBool, d: Duration) {
    let end = Instant::now() + d;
    while !stop.load(Ordering::Relaxed) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------------------------
// Örnek biçimi ve yeniden örnekleme

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    U8,
    I16,
    I24,
    I32,
    F32,
}

/// Çerçeve düzeni: örnek türü, kanal sayısı, hız, çerçeve baytı.
#[derive(Clone, Copy, Debug)]
struct Fmt {
    kind: Kind,
    ch: usize,
    rate: u32,
    block: usize,
}

impl Fmt {
    fn mono16(rate: u32) -> Fmt {
        Fmt { kind: Kind::I16, ch: 1, rate, block: 2 }
    }

    /// WAVEFORMATEX / WAVEFORMATEXTENSIBLE baytlarından.
    fn parse(b: &[u8]) -> Option<Fmt> {
        let u16at = |i: usize| b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
        let u32at = |i: usize| b.get(i..i + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
        let mut tag = u16at(0)? as u32;
        let (ch, rate, block) = (u16at(2)? as usize, u32at(4)?, u16at(12)? as usize);
        // EXTENSIBLE: alt biçim GUID'inin ilk alanı asıl biçim etiketidir (1 PCM, 3 float).
        if tag == 0xFFFE {
            tag = u32at(24)?;
        }
        if ch == 0 || rate == 0 || block == 0 || block % ch != 0 {
            return None;
        }
        let kind = match (tag, block / ch) {
            (1, 1) => Kind::U8,
            (1, 2) => Kind::I16,
            (1, 3) => Kind::I24,
            (1, 4) => Kind::I32,
            (3, 4) => Kind::F32,
            _ => return None,
        };
        Some(Fmt { kind, ch, rate, block })
    }

    /// Tek örnek, -1..1.
    fn sample(&self, s: &[u8]) -> f32 {
        match self.kind {
            Kind::U8 => (s[0] as f32 - 128.0) / 128.0,
            Kind::I16 => i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0,
            Kind::I24 => i32::from_le_bytes([0, s[0], s[1], s[2]]) as f32 / 2_147_483_648.0,
            Kind::I32 => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
            Kind::F32 => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        }
    }

    /// Çerçevelerin kanal ortalamasını `out`a ekler.
    fn to_mono(&self, bytes: &[u8], out: &mut Vec<f32>) {
        let w = self.block / self.ch;
        out.extend(bytes.chunks_exact(self.block).map(|f| {
            let sum: f32 = f.chunks_exact(w).map(|s| self.sample(s)).sum();
            sum / self.ch as f32
        }));
    }

    /// Mono örnekleri bütün kanallara yazar; `out` en az `mono.len() * block` bayt.
    fn from_mono(&self, mono: &[f32], out: &mut [u8]) {
        let w = self.block / self.ch;
        for (f, &x) in out.chunks_exact_mut(self.block).zip(mono) {
            let x = x.clamp(-1.0, 1.0);
            let s: [u8; 4] = match self.kind {
                Kind::U8 => [(x * 127.0 + 128.0) as u8, 0, 0, 0],
                Kind::I16 => {
                    let v = ((x * 32767.0) as i16).to_le_bytes();
                    [v[0], v[1], 0, 0]
                }
                Kind::I24 => {
                    let v = ((x * 8_388_607.0) as i32).to_le_bytes();
                    [v[0], v[1], v[2], 0]
                }
                Kind::I32 => ((x as f64 * 2_147_483_647.0) as i32).to_le_bytes(),
                Kind::F32 => x.to_le_bytes(),
            };
            for c in f.chunks_exact_mut(w) {
                c.copy_from_slice(&s[..w]);
            }
        }
    }
}

/// Akış halinde doğrusal yeniden örnekleyici: parçalar arasında kaldığı yeri hatırlar. Hız
/// düşürürken örtüşmeyi azaltmak için önce kısa bir kayan ortalamadan geçirir.
struct Lerp {
    step: f64,
    t: f64,
    prev: Option<f32>,
    taps: usize,
    hist: [f32; 8],
    hi: usize,
}

impl Lerp {
    fn new(from: u32, to: u32) -> Lerp {
        let step = from.max(1) as f64 / to.max(1) as f64;
        let taps = if step >= 1.5 { (step.round() as usize).min(8) } else { 1 };
        Lerp { step, t: 0.0, prev: None, taps, hist: [0.0; 8], hi: 0 }
    }

    fn push(&mut self, input: impl IntoIterator<Item = f32>, out: &mut Vec<f32>) {
        for x in input {
            let x = if self.taps > 1 {
                self.hist[self.hi] = x;
                self.hi = (self.hi + 1) % self.taps;
                self.hist[..self.taps].iter().sum::<f32>() / self.taps as f32
            } else {
                x
            };
            let Some(p) = self.prev else {
                self.prev = Some(x);
                continue;
            };
            while self.t < 1.0 {
                out.push(p + (x - p) * self.t as f32);
                self.t += self.step;
            }
            self.t -= 1.0;
            self.prev = Some(x);
        }
    }

    /// Akış bitti: son örneğe kadar olan konumları da üretir.
    fn finish(&mut self, out: &mut Vec<f32>) {
        if let Some(p) = self.prev {
            self.push([p], out);
        }
    }
}

/// RMS seviyesi 0..1 (dB ölçeğinde: konuşma ≈ 0.3–0.7, sessizlik ≈ 0).
pub fn level(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let ms = pcm.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / pcm.len() as f64;
    perceptual((ms.sqrt() / 32768.0) as f32)
}

/// Basit doğrusal yeniden örnekleme.
pub fn resample(pcm: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to || from == 0 || to == 0 || pcm.is_empty() {
        return pcm.to_vec();
    }
    let mut l = Lerp::new(from, to);
    let mut out = Vec::with_capacity((pcm.len() as u64 * to as u64 / from as u64) as usize + 2);
    l.push(pcm.iter().map(|&s| s as f32 / 32768.0), &mut out);
    l.finish(&mut out);
    out.into_iter().map(to_i16).collect()
}

/// 16-bit PCM mono WAV dosyası (bayt).
pub fn wav(pcm: &[i16], rate: u32) -> Vec<u8> {
    let data = (pcm.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    for s in pcm {
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

/// WAV (PCM 8/16/24/32-bit ya da float32, her kanal sayısı → mono) çözer: (örnekler, hız).
pub fn decode_wav(bytes: &[u8]) -> Option<(Vec<i16>, u32)> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut fmt = None;
    let mut i = 12usize;
    while i + 8 <= bytes.len() {
        let len = u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        // Akışla yazılmış dosyalarda boyut eksik/şişkin olabilir: elde olanla yetinilir.
        let body = &bytes[i + 8..(i + 8).saturating_add(len).min(bytes.len())];
        match &bytes[i..i + 4] {
            b"fmt " => fmt = Fmt::parse(body),
            b"data" => {
                let f = fmt?;
                let mut mono = Vec::with_capacity(body.len() / f.block);
                f.to_mono(body, &mut mono);
                return Some((mono.into_iter().map(to_i16).collect(), f.rate));
            }
            _ => {}
        }
        i = i.saturating_add(8).saturating_add(len).saturating_add(len & 1);
    }
    None
}

// ---------------------------------------------------------------------------------------------
// WASAPI

fn enumerator() -> Res<IMMDeviceEnumerator> {
    Ok(unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? })
}

/// Varsayılan uç nokta ve kimliği.
fn default_device(en: &IMMDeviceEnumerator, flow: EDataFlow) -> Res<(IMMDevice, String)> {
    unsafe {
        let d = en.GetDefaultAudioEndpoint(flow, eConsole)?;
        let p = d.GetId()?;
        let id = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        Ok((d, id))
    }
}

/// Tercih edilen mikrofonun kimliği; boşsa Windows'un varsayılanı.
static MIC: Mutex<String> = Mutex::new(String::new());

/// Kayıtta kullanılacak mikrofon (kimlik, `mics`'ten); boş: varsayılan. Açık kayıt birkaç saniye
/// içinde yeni cihaza geçer.
pub fn set_mic(id: &str) {
    *relock(&MIC) = id.to_string();
}

/// Etkin mikrofonlar: (kimlik, ad).
pub fn mics() -> Vec<(String, String)> {
    let _com = Com::init();
    let Ok(en) = enumerator() else { return Vec::new() };
    let mut out = Vec::new();
    unsafe {
        let Ok(list) = en.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) else { return out };
        for i in 0..list.GetCount().unwrap_or(0) {
            let Ok(d) = list.Item(i) else { continue };
            let Ok(p) = d.GetId() else { continue };
            let id = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const _));
            out.push((id, device_name(&d)));
        }
    }
    out
}

/// Kayıt cihazı: tercih edilen etkinse o, değilse varsayılan.
fn capture_device(en: &IMMDeviceEnumerator) -> Res<(IMMDevice, String)> {
    let want = relock(&MIC).clone();
    if !want.is_empty() {
        let w = wide(&want);
        unsafe {
            if let Ok(d) = en.GetDevice(PCWSTR(w.as_ptr()))
                && d.GetState().is_ok_and(|st| st == DEVICE_STATE_ACTIVE)
            {
                return Ok((d, want));
            }
        }
    }
    default_device(en, eCapture)
}

/// Cihazın görünen adı (log için).
fn device_name(d: &IMMDevice) -> String {
    // PKEY_Device_FriendlyName
    let key = PROPERTYKEY { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };
    unsafe { d.OpenPropertyStore(STGM_READ).and_then(|s| s.GetValue(&key)).map(|v| v.to_string()).unwrap_or_default() }
}

/// Paylaşımlı modda, olay güdümlü açılmış akış.
struct Stream {
    client: IAudioClient,
    fmt: Fmt,
    event: Event,
    /// Cihaz arabelleği (çerçeve).
    frames: u32,
}

/// Önce `rate` hızında mono i16 ister (Windows çevirir); olmazsa cihazın karışım biçimiyle açar.
fn open_stream(device: &IMMDevice, rate: u32, buffer_ms: i64) -> Res<Stream> {
    let event = Event::new()?;
    let dur = buffer_ms * 10_000;
    let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
    unsafe {
        let mut opened = None;
        if !RAW_FORMAT.load(Ordering::Relaxed) {
            let want = WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_PCM as u16,
                nChannels: 1,
                nSamplesPerSec: rate,
                nAvgBytesPerSec: rate * 2,
                nBlockAlign: 2,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let auto = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            match client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags | auto, dur, 0, &want, None) {
                Ok(()) => opened = Some((client, Fmt::mono16(rate))),
                Err(e) => log!("ses: dönüştürmeli açılamadı ({e}), karışım biçimi deneniyor"),
            }
        }
        let (client, fmt) = match opened {
            Some(o) => o,
            None => {
                // Başarısız Initialize'dan sonra istemci yeniden kullanılamaz: yenisi alınır.
                let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
                let p = client.GetMixFormat()?;
                let bytes = std::slice::from_raw_parts(p as *const u8, 18 + (*p).cbSize as usize).to_vec();
                CoTaskMemFree(Some(p as *const _));
                let fmt = Fmt::parse(&bytes).ok_or("desteklenmeyen cihaz ses biçimi")?;
                client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, dur, 0, bytes.as_ptr().cast(), None)?;
                (client, fmt)
            }
        };
        client.SetEventHandle(event.0)?;
        let frames = client.GetBufferSize()?;
        Ok(Stream { client, fmt, event, frames })
    }
}

// ---------------------------------------------------------------------------------------------
// Kayıt

pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Varsayılan mikrofonu paylaşımlı modda açar; kendi iş parçacığında sesi 16 kHz mono i16'ya
    /// çevirip FRAME'lik parçalar halinde `on_frame`'e verir. Varsayılan cihaz değişirse ya da
    /// çıkarılırsa birkaç saniye içinde yeniden açar; cihaz yokken sessizce bekleyip yeniden dener.
    pub fn start(mut on_frame: Box<dyn FnMut(&[i16]) + Send>) -> Res<Capture> {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let thread = std::thread::Builder::new().name("ses-kayit".into()).spawn(move || {
            let _com = Com::init();
            let mut last = String::new();
            while !s.load(Ordering::Relaxed) {
                match capture_session(&s, &mut *on_frame) {
                    Ok(()) => last.clear(),
                    Err(e) => {
                        // Aynı hata (ör. mikrofon yok) her denemede loglanmaz.
                        let e = e.to_string();
                        if e != last {
                            log!("mikrofon: {e}");
                            last = e;
                        }
                        nap(&s, Duration::from_millis(1500));
                    }
                }
            }
        })?;
        Ok(Capture { stop, thread: Some(thread) })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Bir cihazla kayıt; durdurulunca ya da varsayılan cihaz değişince `Ok` döner.
fn capture_session(stop: &AtomicBool, on_frame: &mut dyn FnMut(&[i16])) -> Res<()> {
    let en = enumerator()?;
    let (device, id) = capture_device(&en)?;
    // Kayıtta gecikmeyi arabellek değil olay belirler: geniş arabellek yavaş geri çağrıya pay bırakır.
    let st = open_stream(&device, RATE, 500)?;
    let fmt = st.fmt;
    log!("mikrofon açıldı: {} ({} Hz, {} kanal, {:?})", device_name(&device), fmt.rate, fmt.ch, fmt.kind);
    unsafe {
        let capture: IAudioCaptureClient = st.client.GetService()?;
        let mut lerp = Lerp::new(fmt.rate, RATE);
        let (mut mono, mut out) = (Vec::new(), Vec::new());
        let mut pending: Vec<i16> = Vec::with_capacity(FRAME * 4);
        st.client.Start()?;
        let mut check = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            st.event.wait(200);
            while capture.GetNextPacketSize()? != 0 {
                let (mut data, mut n, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                capture.GetBuffer(&mut data, &mut n, &mut flags, None, None)?;
                mono.clear();
                if data.is_null() || flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                    mono.resize(n as usize, 0.0);
                } else {
                    fmt.to_mono(std::slice::from_raw_parts(data, n as usize * fmt.block), &mut mono);
                }
                capture.ReleaseBuffer(n)?;
                out.clear();
                lerp.push(mono.iter().copied(), &mut out);
                pending.extend(out.iter().map(|&x| to_i16(x)));
                let mut at = 0;
                while pending.len() - at >= FRAME {
                    on_frame(&pending[at..at + FRAME]);
                    at += FRAME;
                }
                pending.drain(..at);
            }
            if check.elapsed() >= DEVICE_CHECK {
                check = Instant::now();
                if capture_device(&en).ok().is_none_or(|(_, d)| d != id) {
                    log!("mikrofon değişti");
                    break;
                }
            }
        }
        let _ = st.client.Stop();
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Çalma

/// Çalma kuyruğu; QRATE hızında mono.
struct Queue {
    buf: VecDeque<i16>,
    /// `play` çağrıları arasında yeniden örnekleyicinin durumu (kaynak hızıyla).
    lerp: Option<(u32, Lerp)>,
    /// Her `stop`ta artar; çalma iş parçacığı görünce cihaz arabelleğini boşaltır.
    epoch: u64,
    /// Kuyrukta ya da cihaz arabelleğinde ses var.
    busy: bool,
    quit: bool,
    /// Açık akışın olayı (HANDLE değeri, yoksa 0): `play`/`stop` çalma iş parçacığını hemen uyandırır.
    wake: usize,
}

impl Queue {
    fn kick(&self) {
        if self.wake != 0 {
            unsafe {
                let _ = SetEvent(HANDLE(self.wake as *mut _));
            }
        }
    }
}

struct Shared {
    q: Mutex<Queue>,
    cv: Condvar,
    /// f32 bitleri.
    level: AtomicU32,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        relock(&self.q)
    }

    fn set_level(&self, v: f32) {
        self.level.store(v.to_bits(), Ordering::Relaxed);
    }
}

/// Son `Player` kopyası düşünce çalma iş parçacığına çıkmasını söyler.
struct Owner(Arc<Shared>);

impl Drop for Owner {
    fn drop(&mut self) {
        let mut q = self.0.lock();
        q.quit = true;
        q.buf.clear();
        q.busy = false;
        q.kick();
        self.0.cv.notify_all();
    }
}

/// Kendi iş parçacığında, ilk seste varsayılan hoparlörü açan çalar; ~2 sn boşta kalınca cihazı bırakır.
#[derive(Clone)]
pub struct Player(Arc<Owner>);

impl Player {
    pub fn new() -> Player {
        let s = Arc::new(Shared {
            q: Mutex::new(Queue { buf: VecDeque::new(), lerp: None, epoch: 0, busy: false, quit: false, wake: 0 }),
            cv: Condvar::new(),
            level: AtomicU32::new(0),
        });
        let t = s.clone();
        if let Err(e) = std::thread::Builder::new().name("ses-calma".into()).spawn(move || render_thread(t)) {
            log!("çalma iş parçacığı başlatılamadı: {e}");
            s.lock().quit = true;
        }
        Player(Arc::new(Owner(s)))
    }

    fn shared(&self) -> &Shared {
        &self.0.0
    }

    /// Sona ekler (her hız olur). Akış halinde parça parça çağrılabilir.
    pub fn play(&self, pcm: &[i16], rate: u32) {
        if pcm.is_empty() || rate == 0 {
            return;
        }
        let s = self.shared();
        let mut q = s.lock();
        if q.quit {
            return;
        }
        if q.lerp.as_ref().is_none_or(|(r, _)| *r != rate) {
            q.lerp = Some((rate, Lerp::new(rate, QRATE)));
        }
        let mut out = Vec::with_capacity((pcm.len() as u64 * QRATE as u64 / rate as u64) as usize + 2);
        if let Some((_, l)) = q.lerp.as_mut() {
            l.push(pcm.iter().map(|&x| x as f32 / 32768.0), &mut out);
        }
        q.buf.extend(out.into_iter().map(to_i16));
        q.busy = true;
        q.kick();
        s.cv.notify_all();
    }

    /// Kuyruğu hemen boşaltır, çalmayı keser (söz kesme).
    pub fn stop(&self) {
        let s = self.shared();
        let mut q = s.lock();
        q.buf = VecDeque::new();
        q.lerp = None;
        q.epoch += 1;
        q.busy = false;
        q.kick();
        s.set_level(0.0);
        s.cv.notify_all();
    }

    /// Çalan ya da sırada ses var mı.
    pub fn busy(&self) -> bool {
        self.shared().lock().busy
    }

    /// Şu an çalan sesin seviyesi 0..1 (son ~50 ms); boştayken 0.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.shared().level.load(Ordering::Relaxed))
    }

    /// Kuyruk boşalana kadar (ya da stop'a kadar) bekler.
    pub fn wait(&self) {
        let s = self.shared();
        let q = s.lock();
        drop(s.cv.wait_while(q, |q| q.busy && !q.quit).unwrap_or_else(|e| e.into_inner()));
    }
}

fn render_thread(s: Arc<Shared>) {
    let _com = Com::init();
    let mut fails = 0;
    loop {
        {
            let q = s.cv.wait_while(s.lock(), |q| q.buf.is_empty() && !q.quit).unwrap_or_else(|e| e.into_inner());
            if q.quit {
                return;
            }
        }
        match render_session(&s) {
            Ok(()) => fails = 0,
            Err(e) => {
                fails += 1;
                log!("hoparlör: {e}");
                // Cihaz hiç açılamıyorsa kuyruk atılır ki `wait` sonsuza dek beklemesin.
                if fails >= 3 {
                    let mut q = s.lock();
                    q.buf.clear();
                    q.busy = false;
                    s.cv.notify_all();
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        }
        s.set_level(0.0);
    }
}

/// Bir cihazla çalma; ~2 sn boşta kalınca, kapatılınca ya da varsayılan cihaz değişince `Ok` döner.
fn render_session(s: &Shared) -> Res<()> {
    let en = enumerator()?;
    let (device, id) = default_device(&en, eRender)?;
    let st = open_stream(&device, QRATE, 100)?;
    // Olay kapanmadan önce (st'den önce düşer) paylaşılan tutamaç silinir.
    struct Wake<'a>(&'a Shared);
    impl Drop for Wake<'_> {
        fn drop(&mut self) {
            self.0.lock().wake = 0;
        }
    }
    s.lock().wake = st.event.0.0 as usize;
    let _wake = Wake(s);
    let fmt = st.fmt;
    log!("hoparlör açıldı: {} ({} Hz, {} kanal, {:?})", device_name(&device), fmt.rate, fmt.ch, fmt.kind);
    // Cihazda en çok ~60 ms bekletilir; stop'ta Reset ile hemen atılır.
    let target = (fmt.rate as u64 * 60 / 1000).min(st.frames as u64) as u32;
    let mut lerp = (fmt.rate != QRATE).then(|| Lerp::new(QRATE, fmt.rate));
    let (mut chunk, mut pend): (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
    let mut epoch = s.lock().epoch;
    let (mut started, mut ms) = (false, 0f32);
    let mut idle: Option<Instant> = None;
    let mut check = Instant::now();
    unsafe {
        let render: IAudioRenderClient = st.client.GetService()?;
        loop {
            chunk.clear();
            let busy = {
                let mut q = s.lock();
                if q.quit {
                    break;
                }
                if q.epoch != epoch {
                    epoch = q.epoch;
                    pend.clear();
                    if let Some(l) = lerp.as_mut() {
                        *l = Lerp::new(QRATE, fmt.rate);
                    }
                    if started {
                        st.client.Stop()?;
                        st.client.Reset()?;
                        started = false;
                    }
                    ms = 0.0;
                }
                // Okuma ve kuyruktan alma aynı kilit altında: stop'tan sonra gelen ses yanlışlıkla atılmaz.
                let padding = st.client.GetCurrentPadding()?;
                let want = target.saturating_sub(padding) as usize;
                match lerp.as_mut() {
                    None => {
                        let n = want.min(q.buf.len());
                        chunk.extend(q.buf.drain(..n).map(|x| x as f32 / 32768.0));
                    }
                    Some(l) => {
                        while pend.len() < want && !q.buf.is_empty() {
                            let n = q.buf.len().min(480);
                            l.push(q.buf.drain(..n).map(|x| x as f32 / 32768.0), &mut pend);
                        }
                        let n = want.min(pend.len());
                        chunk.extend(pend.drain(..n));
                    }
                }
                if chunk.is_empty() && q.buf.is_empty() && pend.is_empty() && (padding == 0 || !started) && q.busy {
                    q.busy = false;
                    s.cv.notify_all();
                }
                q.busy
            };
            if !chunk.is_empty() {
                let n = chunk.len() as u32;
                let p = render.GetBuffer(n)?;
                fmt.from_mono(&chunk, std::slice::from_raw_parts_mut(p, chunk.len() * fmt.block));
                render.ReleaseBuffer(n, 0)?;
                if !started {
                    st.client.Start()?;
                    started = true;
                }
                // ~50 ms zaman sabitli kayan ortalama kare.
                let m = chunk.iter().map(|x| x * x).sum::<f32>() / chunk.len() as f32;
                let a = (-(chunk.len() as f32) / (fmt.rate as f32 * 0.05)).exp();
                ms = ms * a + m * (1.0 - a);
                s.set_level(perceptual(ms.sqrt()));
                idle = None;
            } else if !busy {
                s.set_level(0.0);
                if idle.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(2) {
                    break;
                }
            }
            if check.elapsed() >= DEVICE_CHECK {
                check = Instant::now();
                if default_device(&en, eRender).ok().is_none_or(|(_, d)| d != id) {
                    log!("varsayılan hoparlör değişti");
                    break;
                }
            }
            // Akış dururken WASAPI olay vermez; play/stop olayı kendisi işaretler.
            st.event.wait(if started { 50 } else { 20 });
        }
        let _ = st.client.Stop();
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Windows konuşma sentezi (OneCore sesleri)

/// Windows'un yerleşik sesiyle konuşma üretir: mono i16 ve hızı. `lang` "tr-TR" gibi: o dildeki
/// ilk ses, yoksa aynı ana dildeki, o da yoksa varsayılan ses. Bloklar: iş parçacığından çağrılır.
pub fn windows_tts(text: &str, lang: &str) -> Res<(Vec<i16>, u32)> {
    use windows::Media::SpeechSynthesis::SpeechSynthesizer;
    use windows::Storage::Streams::DataReader;
    let _com = Com::init();
    let synth = SpeechSynthesizer::new()?;
    if let Some(v) = find_voice(lang) {
        synth.SetVoice(&v)?;
    }
    let stream = synth.SynthesizeTextToStreamAsync(&windows::core::HSTRING::from(text))?.join()?;
    let size = u32::try_from(stream.Size()?)?;
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
    let got = reader.LoadAsync(size)?.join()?;
    let mut bytes = vec![0u8; got as usize];
    reader.ReadBytes(&mut bytes)?;
    decode_wav(&bytes).ok_or_else(|| "Windows konuşma sentezi tanınmayan biçimde ses verdi".into())
}

fn all_voices() -> Vec<windows::Media::SpeechSynthesis::VoiceInformation> {
    let Ok(all) = windows::Media::SpeechSynthesis::SpeechSynthesizer::AllVoices() else { return Vec::new() };
    (0..all.Size().unwrap_or(0)).filter_map(|i| all.GetAt(i).ok()).collect()
}

fn find_voice(lang: &str) -> Option<windows::Media::SpeechSynthesis::VoiceInformation> {
    let voices = all_voices();
    let lang_of = |v: &&windows::Media::SpeechSynthesis::VoiceInformation| v.Language().map(|l| l.to_string()).unwrap_or_default();
    let base = |l: &str| l.split('-').next().unwrap_or("").to_ascii_lowercase();
    voices
        .iter()
        .find(|v| lang_of(v).eq_ignore_ascii_case(lang))
        .or_else(|| voices.iter().find(|v| base(&lang_of(v)) == base(lang)))
        .cloned()
}

/// Kurulu Windows seslerinin adları ve dil etiketleri, ayarlarda göstermek için.
pub fn windows_voices() -> Vec<(String, String)> {
    let _com = Com::init();
    all_voices()
        .iter()
        .map(|v| {
            let name = v.DisplayName().map(|s| s.to_string()).unwrap_or_default();
            (name, v.Language().map(|s| s.to_string()).unwrap_or_default())
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Deneme
