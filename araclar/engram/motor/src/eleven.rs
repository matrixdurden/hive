//! ElevenLabs, ham HTTP: konuşmadan yazıya (Scribe), akışlı konuşma üretimi, ses listesi. Anahtar
//! yalnızca `xi-api-key` başlığında gider, hiçbir yerde loglanmaz.
//!
//! Belgeler: https://elevenlabs.io/docs/api-reference/speech-to-text/convert,
//! https://elevenlabs.io/docs/api-reference/text-to-speech/stream,
//! https://elevenlabs.io/docs/api-reference/voices/search
#![allow(dead_code)]

use serde_json::{Value, json};

use crate::net;
use crate::util::Res;

const API: &str = "https://api.elevenlabs.io";
/// Konuşmadan yazıya modeli (`scribe_v1` kullanımdan kalktı).
const STT_MODEL: &str = "scribe_v2";

pub const TTS_RATE: u32 = 16_000;
/// Varsayılan ses: "George" (çok dilli hazır ses, belgelerin örneklerindeki kimlik). ElevenLabs'in
/// eski varsayılan sesleri yalnızca Mart 2026'dan önce açılmış hesaplarda var ve 31 Aralık 2026'da
/// kalkıyor; bulunamazsa `tts_stream` hesaptaki ilk sese geçer.
pub const DEFAULT_VOICE: &str = "JBFqnCBsd6RMkjVDRZzb";

fn check_key(key: &str) -> Res<()> {
    if key.trim().is_empty() {
        return Err(t!("ElevenLabs API key is not set", "ElevenLabs API anahtarı girilmemiş").into());
    }
    Ok(())
}

/// `net`'in "HTTP 401: …" hatasını kısa, anlaşılır bir metne çevirir.
fn friendly(e: Box<dyn std::error::Error>) -> Box<dyn std::error::Error> {
    let s = e.to_string();
    let low = s.to_lowercase();
    if low.contains("invalid api key") || low.contains("invalid_api_key") {
        t!("ElevenLabs API key is invalid", "ElevenLabs API anahtarı geçersiz").into()
    } else if low.contains("quota") {
        t!("ElevenLabs quota is exhausted", "ElevenLabs kotası doldu").into()
    } else if s.starts_with("HTTP 429") {
        t!("ElevenLabs: too many requests, try again shortly", "ElevenLabs: çok fazla istek, biraz sonra tekrar dene")
            .into()
    } else {
        format!("ElevenLabs: {s}").into()
    }
}

/// 16 bit mono WAV.
fn wav(pcm: &[i16], rate: u32) -> Vec<u8> {
    let data = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// multipart/form-data gövdesi: (gövde, Content-Type başlığı). `file`: (alan, dosya adı, tür, içerik).
pub fn multipart(fields: &[(&str, &str)], file: (&str, &str, &str, &[u8])) -> (Vec<u8>, String) {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let boundary = format!("----engram{nanos:x}");
    let mut body = Vec::with_capacity(file.3.len() + 1024);
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
        );
    }
    let (name, filename, kind, data) = file;
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: {kind}\r\n\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (body, format!("multipart/form-data; boundary={boundary}"))
}

/// Konuşmayı yazıya çevirir (Scribe). `lang`: ISO-639-3 ("tur") ya da ISO-639-1; `None` = kendisi
/// bulsun. 100 ms'den kısa ses (API reddeder) boş metin döndürür.
pub fn stt(key: &str, pcm: &[i16], rate: u32, lang: Option<&str>) -> Res<String> {
    check_key(key)?;
    if pcm.len() < rate as usize / 10 {
        return Ok(String::new());
    }
    let mut fields = vec![("model_id", STT_MODEL), ("tag_audio_events", "false")];
    if let Some(l) = lang.filter(|l| !l.is_empty()) {
        fields.push(("language_code", l));
    }
    let audio = wav(pcm, rate);
    let (body, ctype) = multipart(&fields, ("file", "ses.wav", "audio/wav", &audio));
    let headers = [("xi-api-key", key), ("content-type", ctype.as_str()), ("accept", "application/json")];
    let resp = net::request("POST", &format!("{API}/v1/speech-to-text"), &headers, &body)?.ok().map_err(friendly)?;
    let v: Value = serde_json::from_slice(&resp.body)
        .map_err(|_| t!("ElevenLabs: unreadable response", "ElevenLabs: yanıt okunamadı"))?;
    Ok(v["text"].as_str().unwrap_or("").trim().to_string())
}

/// Ses kimliği adrese girer: yalnızca harf ve rakam. Boşsa varsayılan ses.
fn check_voice(voice: &str) -> Res<&str> {
    let v = if voice.trim().is_empty() { DEFAULT_VOICE } else { voice.trim() };
    if !v.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(t!("invalid ElevenLabs voice id", "geçersiz ElevenLabs ses kimliği").into());
    }
    Ok(v)
}

/// Konuşma üretim isteğinin gövdesi. Türkçe metinde (ya da arayüz Türkçeyken) `language_code: tr`:
/// sayı/tarih okunuşu Türkçe olur. Desteklemeyen modeller onu yok sayar; `eleven_multilingual_v2`
/// için hiç gönderilmez (belgelere göre desteklemiyor).
fn tts_body(model: &str, text: &str) -> Value {
    let model = if model.trim().is_empty() { "eleven_flash_v2_5" } else { model.trim() };
    let mut body = json!({"text": text, "model_id": model});
    let turkish = text.chars().any(|c| "çğıöşüÇĞİÖŞÜ".contains(c)) || crate::i18n::turkish();
    if turkish && !model.starts_with("eleven_multilingual") {
        body["language_code"] = json!("tr");
    }
    body
}

/// Bayt akışından i16 örnekler; parçalar tek bayt sınırında bölünebilir, artan bayt bir sonrakiyle
/// birleşir.
#[derive(Default)]
struct Pcm16 {
    odd: Option<u8>,
}

impl Pcm16 {
    fn feed(&mut self, mut bytes: &[u8], out: &mut Vec<i16>) {
        if let Some(lo) = self.odd.take() {
            let Some((&hi, rest)) = bytes.split_first() else {
                self.odd = Some(lo);
                return;
            };
            out.push(i16::from_le_bytes([lo, hi]));
            bytes = rest;
        }
        let pairs = bytes.chunks_exact(2);
        if let [b] = pairs.remainder() {
            self.odd = Some(*b);
        }
        out.extend(pairs.map(|p| i16::from_le_bytes([p[0], p[1]])));
    }
}

/// Akışlı konuşma üretimi: PCM 16 kHz mono i16 parçaları geldikçe `on_pcm`'e verilir (`false`
/// dönerse okuma kesilir). `voice` boşsa `DEFAULT_VOICE`; varsayılan ses hesapta yoksa hesaptaki
/// ilk sesle bir kez daha denenir.
pub fn tts_stream(key: &str, voice: &str, model: &str, text: &str, on_pcm: &mut dyn FnMut(&[i16]) -> bool) -> Res<()> {
    check_key(key)?;
    let voice = check_voice(voice)?;
    if text.trim().is_empty() {
        return Ok(());
    }
    let mut started = false;
    let r = stream_once(key, voice, model, text, &mut |s| {
        started = true;
        on_pcm(s)
    });
    match r {
        Err(e) if !started && voice == DEFAULT_VOICE && e.to_string().to_lowercase().contains("voice") => {
            let Some((id, name)) = voices(key)?.into_iter().next() else { return Err(e) };
            crate::log!("varsayılan ses yok, hesaptaki ilk ses kullanılıyor: {name}");
            stream_once(key, &id, model, text, on_pcm)
        }
        r => r,
    }
}

fn stream_once(key: &str, voice: &str, model: &str, text: &str, on_pcm: &mut dyn FnMut(&[i16]) -> bool) -> Res<()> {
    // `optimize_streaming_latency` kullanımdan kalktı; gönderilmez.
    let url = format!("{API}/v1/text-to-speech/{voice}/stream?output_format=pcm_{TTS_RATE}");
    let body = tts_body(model, text).to_string();
    let headers = [("xi-api-key", key), ("content-type", "application/json"), ("accept", "audio/pcm")];
    let mut dec = Pcm16::default();
    let mut samples: Vec<i16> = Vec::with_capacity(16 * 1024);
    net::stream("POST", &url, &headers, body.as_bytes(), &mut |chunk| {
        samples.clear();
        dec.feed(chunk, &mut samples);
        samples.is_empty() || on_pcm(&samples)
    })
    .map_err(friendly)?;
    Ok(())
}

/// Tamamını bekleyen sürüm.
pub fn tts(key: &str, voice: &str, model: &str, text: &str) -> Res<Vec<i16>> {
    let mut out = Vec::new();
    tts_stream(key, voice, model, text, &mut |s| {
        out.extend_from_slice(s);
        true
    })?;
    Ok(out)
}

/// Hesaptaki sesler: (kimlik, ad). `GET /v2/voices`, sayfa sayfa (v1 belgelerden kalktı).
pub fn voices(key: &str) -> Res<Vec<(String, String)>> {
    check_key(key)?;
    let mut out = Vec::new();
    let mut token = String::new();
    for _ in 0..20 {
        let mut url = format!("{API}/v2/voices?page_size=100&include_total_count=false");
        if !token.is_empty() {
            url.push_str(&format!("&next_page_token={}", encode(&token)));
        }
        let resp = net::get(&url, &[("xi-api-key", key), ("accept", "application/json")])?.ok().map_err(friendly)?;
        let v: Value = serde_json::from_slice(&resp.body)
            .map_err(|_| t!("ElevenLabs: unreadable response", "ElevenLabs: yanıt okunamadı"))?;
        for voice in v["voices"].as_array().into_iter().flatten() {
            if let (Some(id), Some(name)) = (voice["voice_id"].as_str(), voice["name"].as_str()) {
                out.push((id.to_string(), name.to_string()));
            }
        }
        match v["next_page_token"].as_str() {
            Some(t) if v["has_more"].as_bool() == Some(true) && !t.is_empty() => token = t.to_string(),
            _ => break,
        }
    }
    Ok(out)
}

/// Adres sorgusu için yüzde kodlama.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
