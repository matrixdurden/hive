//! Kulaklık: çalan sesleri varsayılan çıkış cihazında sana da çalar. Bir ses başlayınca
//! açılır, 2 saniye sessizlikten sonra kapanır; böylece boştayken ses motorunu tutmaz ve
//! varsayılan cihaz değişirse bir sonraki seste yenisine geçer. Oturum sürerken gelen
//! uyandırma olayda bekler; oturum kapanınca yenisi hemen açılır.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT;
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, INFINITE, SetEvent,
    WaitForSingleObject,
};
use windows::core::w;

use crate::bus::{Bus, Mixer};
use crate::log;
use crate::util::Res;

const IDLE_CLOSE: Duration = Duration::from_secs(2);

pub struct Monitor {
    wake: usize,
    gain: Arc<AtomicU32>,
}

impl Monitor {
    pub fn start(bus: &'static Bus, gain: f32) -> Self {
        let wake = unsafe { CreateEventW(None, false, false, None) }.map_or(0, |h| h.0 as usize);
        let g = Arc::new(AtomicU32::new(gain.to_bits()));
        let gain_ = g.clone();
        let _ = std::thread::Builder::new().name("kulaklik".into()).spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let wake = HANDLE(wake as *mut _);
            loop {
                WaitForSingleObject(wake, INFINITE);
                if f32::from_bits(gain_.load(Relaxed)) <= 0.0 {
                    continue;
                }
                if let Err(e) = session(bus, &gain_) {
                    log!("kulaklık: {e}");
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        });
        Self { wake, gain: g }
    }

    pub fn set_gain(&self, gain: f32) {
        self.gain.store(gain.to_bits(), Relaxed);
    }

    /// Diğer iş parçacıklarına verilebilen uyandırıcı.
    pub fn waker(&self) -> impl Fn() + Send + 'static {
        let h = self.wake;
        move || unsafe {
            let _ = SetEvent(HANDLE(h as *mut _));
        }
    }
}

/// Varsayılan çıkışa bir akış açar, sessizlik sürene dek çalar.
unsafe fn session(bus: &Bus, gain: &AtomicU32) -> Res<()> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let mix = client.GetMixFormat()?;
        let rate = (*mix).nSamplesPerSec;
        CoTaskMemFree(Some(mix as *const _));
        // Kendi biçimimiz (stereo float); gerekirse ses motoru dönüştürür.
        let fmt = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
            nChannels: 2,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * 8,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 200_000, 0, &fmt, None)?;
        let event = CreateEventW(None, false, false, None)?;
        let result = (|| -> Res<()> {
            client.SetEventHandle(event)?;
            let size = client.GetBufferSize()?;
            let render: IAudioRenderClient = client.GetService()?;
            let mut task = 0u32;
            let mmcss = AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task).ok();
            let mut mixer = Mixer::default();
            let mut quiet_since = Instant::now();
            client.Start()?;
            while quiet_since.elapsed() < IDLE_CLOSE {
                if WaitForSingleObject(event, 200) != WAIT_OBJECT_0 {
                    continue;
                }
                let free = size - client.GetCurrentPadding()?;
                if free == 0 {
                    continue;
                }
                let data = render.GetBuffer(free)?;
                let buf = std::slice::from_raw_parts_mut(data as *mut f32, free as usize * 2);
                buf.fill(0.0);
                if mixer.mix(bus, buf, 2, rate as f32, f32::from_bits(gain.load(Relaxed))) {
                    quiet_since = Instant::now();
                }
                render.ReleaseBuffer(free, 0)?;
            }
            let _ = client.Stop();
            if let Some(h) = mmcss {
                let _ = AvRevertMmThreadCharacteristics(h);
            }
            Ok(())
        })();
        let _ = CloseHandle(event);
        result
    }
}
