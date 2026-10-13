//! Google Gemini: konuşmayı yazıya çevirme (`gemini-3.5-transcribe`) ve isteğe bağlı konuşan ses
//! (`gemini-3.8-flash-tts`). Anahtar URL'de değil başlıkta (`x-goog-api-key`) gider, loglanmaz.

use serde_json::{Value, json};

use crate::net;
use crate::util::Res;

const API: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const STT_MODEL: &str = "gemini-3.5-transcribe";
const TTS_MODEL: &str = "gemini-3.8-flash-tts";
pub const TTS_RATE: u32 = 24_000;

/// Hazır sesler (Gemini TTS); tok ve sakin olanlar başta.
pub const VOICES: [&str; 30] = [
    "Charon", "Algenib", "Orus", "Iapetus", "Alnilam", "Gacrux", "Rasalgethi", "Enceladus", "Umbriel", "Algieba", "Schedar", "Sadaltager",
    "Achird", "Zubenelgenubi", "Fenrir", "Puck", "Kore", "Zephyr", "Leda", "Aoede", "Callirrhoe", "Autonoe", "Despina", "Erinome", "Laomedeia",
    "Achernar", "Pulcherrima", "Vindemiatrix", "Sadachbia", "Sulafat",
];

fn post(key: &str, model: &str, body: &Value) -> Res<Value> {
    if key.trim().is_empty() {
        return Err(t!("Gemini API key is not set", "Gemini API anahtarı girilmemiş").into());
    }
    let url = format!("{API}/{model}:generateContent");
    let resp = net::post_json(&url, &[("x-goog-api-key", key.trim())], body)?;
    match resp.status {
        400 if resp.text().contains("API_KEY") => return Err(t!("Gemini: invalid API key", "Gemini: API anahtarı geçersiz").into()),
        429 => return Err(t!("Gemini: quota reached, try later", "Gemini: kota doldu, sonra tekrar dene").into()),
        _ => {}
    }
    let resp = resp.ok()?;
    serde_json::from_slice(&resp.body).map_err(|_| t!("Gemini: unreadable response", "Gemini: yanıt okunamadı").into())
}

/// Yanıttaki bütün parçaların metni (yazıya çevirme `audioTranscription` içinde gelir).
fn parts_text(v: &Value) -> String {
    let mut out = String::new();
    for p in v["candidates"][0]["content"]["parts"].as_array().into_iter().flatten() {
        if let Some(t) = p["audioTranscription"]["text"].as_str().or(p["text"].as_str()) {
            out.push_str(t);
        }
    }
    out.trim().to_string()
}

/// Konuşmayı yazıya çevirir.
pub fn stt(key: &str, pcm: &[i16], rate: u32) -> Res<String> {
    if pcm.len() < rate as usize / 10 {
        return Ok(String::new());
    }
    let audio = crate::config::base64(&crate::audio::wav(pcm, rate));
    let body = json!({"contents": [{"parts": [{"inline_data": {"mime_type": "audio/wav", "data": audio}}]}]});
    Ok(parts_text(&post(key, STT_MODEL, &body)?))
}

/// Seslendirir (tamamı; akış yok): mono i16 ve hızı.
pub fn tts(key: &str, voice: &str, text: &str) -> Res<(Vec<i16>, u32)> {
    let voice = if voice.is_empty() { VOICES[0] } else { voice };
    let body = json!({
        "contents": [{"parts": [{"text": text}]}],
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": {"voiceConfig": {"prebuiltVoiceConfig": {"voiceName": voice}}},
        },
    });
    let v = post(key, TTS_MODEL, &body)?;
    let data = v["candidates"][0]["content"]["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|p| p["inlineData"]["data"].as_str())
        .ok_or(t!("Gemini: no audio in response", "Gemini: yanıtta ses yok"))?;
    let bytes = crate::config::unbase64(data).ok_or(t!("Gemini: bad audio", "Gemini: ses bozuk"))?;
    // Bazen WAV, bazen ham 24 kHz s16le gelir.
    if bytes.starts_with(b"RIFF") {
        return crate::audio::decode_wav(&bytes).ok_or_else(|| t!("Gemini: bad audio", "Gemini: ses bozuk").into());
    }
    Ok((bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect(), TTS_RATE))
}
