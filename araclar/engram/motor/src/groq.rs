//! Groq üzerinden Whisper (large-v3-turbo): konuşmayı yazıya çevirir. Ücretsiz katmanı kişisel
//! kullanıma fazlasıyla yeter, çok hızlıdır ve Türkçesi iyidir. Anahtar yalnızca başlıkta gider.

use serde_json::Value;

use crate::net;
use crate::util::Res;

const URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const MODEL: &str = "whisper-large-v3-turbo";

/// Whisper'ın sessizlikte ve gürültüde uydurduğu bilinen kalıplar (Türkçe altyazı verisinden).
const GHOSTS: [&str; 8] = [
    "altyazı m.k.",
    "altyazı",
    "izlediğiniz için teşekkürler",
    "izlediğiniz için teşekkür ederim",
    "abone olmayı unutmayın",
    "teşekkürler.",
    "thank you.",
    "thanks for watching",
];

/// Konuşmayı yazıya çevirir; `lang` ISO-639-1 ("tr") ya da `None` (kendisi bulur).
pub fn stt(key: &str, pcm: &[i16], rate: u32, lang: Option<&str>) -> Res<String> {
    if key.trim().is_empty() {
        return Err(t!("Groq API key is not set", "Groq API anahtarı girilmemiş").into());
    }
    if pcm.len() < rate as usize / 10 {
        return Ok(String::new());
    }
    // İpucu: özel adlar doğru yazılsın.
    let mut fields = vec![("model", MODEL), ("response_format", "json"), ("temperature", "0"), ("prompt", "Matrix, engram, justrises, WSL, ssh, terminal, not, günlük.")];
    if let Some(l) = lang.filter(|l| !l.is_empty()) {
        fields.push(("language", l));
    }
    let audio = crate::audio::wav(pcm, rate);
    let (body, ctype) = crate::eleven::multipart(&fields, ("file", "ses.wav", "audio/wav", &audio));
    let auth = format!("Bearer {}", key.trim());
    let headers = [("authorization", auth.as_str()), ("content-type", ctype.as_str()), ("accept", "application/json")];
    let resp = net::request("POST", URL, &headers, &body)?;
    if resp.status == 401 {
        return Err(t!("Groq: invalid API key", "Groq: API anahtarı geçersiz").into());
    }
    if resp.status == 429 {
        return Err(t!("Groq: rate limit, try again shortly", "Groq: sınıra takıldı, biraz sonra tekrar dene").into());
    }
    let resp = resp.ok()?;
    let v: Value = serde_json::from_slice(&resp.body).map_err(|_| t!("Groq: unreadable response", "Groq: yanıt okunamadı"))?;
    let text = v["text"].as_str().unwrap_or("").trim().to_string();
    let low = crate::notes::key(&text);
    if GHOSTS.iter().any(|g| low == *g || low.trim_end_matches('.') == g.trim_end_matches('.')) {
        return Ok(String::new());
    }
    Ok(text)
}
