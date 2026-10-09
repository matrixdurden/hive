//! Ses dosyasını Windows'un kendi çözücüsüyle (Media Foundation) stereo f32'ye açar.
//! mp3, wav, m4a/aac, wma, flac: ek kütüphane yok. Örnekleme hızı olduğu gibi kalır;
//! hız dönüşümünü her okur kendi cihazına göre yapar.

use std::path::Path;

use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PropVariantToUInt64;
use windows::core::PCWSTR;

use crate::util::{Res, wide};

pub const EXTENSIONS: &[&str] = &["mp3", "wav", "m4a", "aac", "wma", "flac"];

const STREAM: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;

pub struct Decoder {
    reader: IMFSourceReader,
    pub rate: u32,
    channels: usize,
    /// Saniye; bilinmiyorsa 0.
    pub duration: f64,
    buf: Vec<[f32; 2]>,
    pos: usize,
    eof: bool,
}

/// Çağıran iş parçacığında bir kez: COM ve Media Foundation.
pub fn startup() -> Res<()> {
    unsafe { Ok(MFStartup(MF_VERSION, MFSTARTUP_LITE)?) }
}

impl Decoder {
    pub fn open(path: &Path) -> Res<Self> {
        unsafe {
            let url = wide(&path.to_string_lossy());
            let reader = MFCreateSourceReaderFromURL(PCWSTR(url.as_ptr()), None)?;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(STREAM, true)?;
            let want = MFCreateMediaType()?;
            want.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            want.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
            reader.SetCurrentMediaType(STREAM, None, &want)?;
            let got = reader.GetCurrentMediaType(STREAM)?;
            let rate = got.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?;
            let channels = got.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)? as usize;
            if rate == 0 || channels == 0 {
                return Err("ses biçimi okunamadı".into());
            }
            let duration = reader
                .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
                .ok()
                .and_then(|v| PropVariantToUInt64(&v).ok())
                .map_or(0.0, |t| t as f64 / 1e7);
            Ok(Self { reader, rate, channels, duration, buf: Vec::new(), pos: 0, eof: false })
        }
    }

    /// En fazla `max` kare. Dosya bitince boş döner.
    pub fn read(&mut self, max: usize) -> &[[f32; 2]] {
        while self.pos >= self.buf.len() && !self.eof {
            if let Err(e) = self.refill() {
                crate::log!("çözme hatası: {e}");
                self.eof = true;
            }
        }
        let n = max.min(self.buf.len() - self.pos);
        self.pos += n;
        &self.buf[self.pos - n..self.pos]
    }

    fn refill(&mut self) -> Res<()> {
        self.buf.clear();
        self.pos = 0;
        unsafe {
            let mut flags = 0u32;
            let mut sample = None;
            self.reader.ReadSample(STREAM, 0, None, Some(&mut flags), None, Some(&mut sample))?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                self.eof = true;
            }
            let Some(sample) = sample else { return Ok(()) };
            let buffer = sample.ConvertToContiguousBuffer()?;
            let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&mut data, None, Some(&mut len))?;
            let samples = std::slice::from_raw_parts(data as *const f32, len as usize / 4);
            let ch = self.channels;
            self.buf.extend(samples.chunks_exact(ch).map(|f| if ch == 1 { [f[0], f[0]] } else { [f[0], f[1]] }));
            buffer.Unlock()?;
        }
        Ok(())
    }
}
