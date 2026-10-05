//! `lyrebird --dene`: her mikrofonu üç modda (varsayılan, iletişim, raw) dinler, ortak belleğe
//! önce saf bir ton, sonra müziğe benzer bir sinyal yazar ve mikrofondan geri gelip gelmediğini
//! ölçer. Zincirin tamamını (bus → APO → sürücü efektleri → uygulama) sınar.

use std::f32::consts::PI;
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};

use crate::bus::{self, VOICES};
use crate::util::Res;

const TONE: f32 = 997.0;

/// Goertzel: `x` içindeki `f` frekanslı bileşenin genliği.
fn tone_level(x: &[f32], rate: f32, f: f32) -> f32 {
    let k = 2.0 * (2.0 * PI * f / rate).cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &v in x {
        let s = v + k * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    (s1 * s1 + s2 * s2 - k * s1 * s2).max(0.0).sqrt() * 2.0 / x.len().max(1) as f32
}

/// Mikrofondan `dur` boyunca ilk kanalı toplar.
unsafe fn record(
    client: &IAudioClient,
    capture: &IAudioCaptureClient,
    channels: usize,
    dur: Duration,
) -> Res<Vec<f32>> {
    unsafe {
        let mut out = Vec::new();
        let t = Instant::now();
        while t.elapsed() < dur {
            std::thread::sleep(Duration::from_millis(10));
            while capture.GetNextPacketSize()? > 0 {
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let s = std::slice::from_raw_parts(data as *const f32, frames as usize * channels);
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                    out.extend(std::iter::repeat_n(0.0, frames as usize));
                } else {
                    out.extend(s.iter().step_by(channels));
                }
                capture.ReleaseBuffer(frames)?;
            }
        }
        let _ = client;
        Ok(out)
    }
}

/// Tüm etkin mikrofonları sırayla sınar. Hepsinde ton geri geliyorsa `true`.
pub fn run() -> Res<bool> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let bus = bus::create()?;
        if !bus.valid() {
            bus.reset(crate::config::gain(70));
        }
        let mut all = true;
        for m in crate::install::mics(DEVICE_STATE_ACTIVE)? {
            println!("{}", m.name);
            for (mode, category, options) in [
                ("varsayılan", AudioCategory_Other, AUDCLNT_STREAMOPTIONS_NONE),
                ("iletişim  ", AudioCategory_Communications, AUDCLNT_STREAMOPTIONS_NONE),
                ("raw       ", AudioCategory_Other, AUDCLNT_STREAMOPTIONS_RAW),
            ] {
                match one(bus, &m.device, category, options) {
                    Ok((ok, line)) => {
                        all &= ok;
                        println!("  {mode} {line}");
                    }
                    Err(e) => println!("  {mode} açılamadı: {e}"),
                }
            }
        }
        Ok(all)
    }
}

/// Tek bir akış açar ve tonu arar: (geldi mi, özet satırı).
unsafe fn one(
    bus: &bus::Bus,
    device: &IMMDevice,
    category: AUDIO_STREAM_CATEGORY,
    options: AUDCLNT_STREAMOPTIONS,
) -> Res<(bool, String)> {
    unsafe {
        let client: IAudioClient2 = device.Activate(CLSCTX_ALL, None)?;
        let props = AudioClientProperties {
            cbSize: size_of::<AudioClientProperties>() as u32,
            bIsOffload: false.into(),
            eCategory: category,
            Options: options,
        };
        client.SetClientProperties(&props)?;
        let mix = client.GetMixFormat()?;
        let (rate, channels, bits) = ((*mix).nSamplesPerSec, (*mix).nChannels as usize, (*mix).wBitsPerSample);
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 10_000_000, 0, mix, None);
        CoTaskMemFree(Some(mix as *const _));
        init?;
        if bits != 32 {
            return Err(format!("beklenmeyen mikrofon biçimi: {bits} bit").into());
        }
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        let before = record(&client, &capture, channels, Duration::from_millis(400))?;
        // Uygulamanın kullanmadığı bir yuva: son yuva.
        let v = &bus.voices[VOICES - 1];
        v.begin(48_000);
        let tone: Vec<[f32; 2]> =
            (0..72_000).map(|i| (2.0 * PI * TONE * i as f32 / 48_000.0).sin() * 0.5).map(|s| [s, s]).collect();
        v.push(&tone);
        v.finish();
        v.publish();
        let during = record(&client, &capture, channels, Duration::from_millis(900))?;
        v.stop();
        let skip = during.len() / 6; // ilk parça gecikme payı
        let (a, b) = (tone_level(&before, rate as f32, TONE), tone_level(&during[skip..], rate as f32, TONE));
        let tone_ok = b > 0.01 && b > a * 4.0;

        // Müziğe benzer sinyal: kayan temel frekans, harmonikler, vuruşlu genlik.
        std::thread::sleep(Duration::from_millis(100));
        let quiet = record(&client, &capture, channels, Duration::from_millis(300))?;
        v.begin(48_000);
        let mut phase = 0.0f32;
        let music: Vec<[f32; 2]> = (0..72_000)
            .map(|i| {
                let t = i as f32 / 48_000.0;
                let f = 220.0 * 2f32.powf(((t * 4.0).floor() % 5.0) / 4.0);
                phase += 2.0 * PI * f / 48_000.0;
                let env = (1.0 - (t * 4.0).fract()).powi(2);
                let s = (phase.sin() + 0.5 * (2.0 * phase).sin() + 0.25 * (3.0 * phase).sin()) * 0.3 * env;
                [s, s]
            })
            .collect();
        v.push(&music);
        v.finish();
        v.publish();
        let loud = record(&client, &capture, channels, Duration::from_millis(900))?;
        v.stop();
        let _ = client.Stop();
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt();
        let (q, m) = (rms(&quiet), rms(&loud[loud.len() / 6..]));
        // Gürültü engelleme saf tonu bastırabilir; müzik geçiyorsa yeterli.
        let ok = tone_ok || (m > 0.02 && m > q * 3.0);
        Ok((ok, format!("{}  ton {a:.3} → {b:.3}, müzik {q:.3} → {m:.3}", if ok { "TAMAM" } else { "YOK  " })))
    }
}
