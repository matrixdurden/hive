//! Claude Messages API, ham HTTP: akışlı (SSE) istek, araç çağrıları, önbellek. Anahtar yalnızca
//! başlıkta gider, hiçbir yerde loglanmaz.
//!
//! Opus 5.5 kuralları: düşünme hep açık (`thinking` gönderilmez, derinlik `output_config.effort`
//! ile), `tool_choice` zorlanmaz (`auto`), asistan ön doldurması yok, araç girdileri JSON olarak
//! ayrıştırılır.

use serde_json::{Value, json};

use crate::net;
use crate::util::Res;

const URL: &str = "https://api.anthropic.com/v1/messages";

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON şeması (`additionalProperties: false` ve `required` ile).
    pub schema: Value,
}

/// API bir istekte en çok bu kadar `strict` aracı kabul ediyor (fazlası HTTP 400).
const MAX_STRICT: usize = 20;

impl Tool {
    /// `strict`: girdisi şemaya birebir uysun (ilk `MAX_STRICT` araçta).
    fn json(&self, strict: bool) -> Value {
        let mut v = json!({
            "name": self.name,
            "description": self.description,
            "input_schema": self.schema,
        });
        if strict {
            v["strict"] = json!(true);
        }
        v
    }
}

/// Akıştan gelen olaylar.
#[derive(Debug, Clone)]
pub enum Event {
    /// Metin parçası.
    Text(String),
    /// Araç çağrıları arasındaki kısa ilerleme notu (düşünme bloğunun özeti; düşüncenin kendisi değil).
    Progress(String),
    /// Bir araç çağrısı tamamlandı (girdisi ayrıştırıldı). `toolset`: araç takımının adı
    /// (bilgisayar kontrolünde "computer"), sonuçta aynen geri verilir.
    ToolUse { id: String, name: String, input: Value, toolset: Option<String> },
    /// Mesaj bitti.
    Stop,
}

#[derive(Debug, Default, Clone)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// Bir isteğin sonucu: asistanın içerik blokları (geçmişe aynen eklenir), durma nedeni, kullanım.
#[derive(Debug, Default, Clone)]
pub struct Reply {
    pub content: Vec<Value>,
    pub stop_reason: String,
    pub usage: Usage,
}

pub struct Request<'a> {
    pub api_key: &'a str,
    pub model: &'a str,
    pub max_tokens: u32,
    /// low | medium | high.
    pub effort: &'a str,
    /// Sistem blokları (metin, isteğe bağlı `cache_control`).
    pub system: Vec<Value>,
    pub messages: &'a [Value],
    pub tools: &'a [Tool],
    /// Anthropic'in tanımladığı araçlar (ör. `{"type": "computer_toolset_20260801"}`), olduğu gibi.
    pub raw_tools: &'a [Value],
}

/// Sistem bloğu; `cached` ise önbellek noktası.
pub fn system_block(text: &str, cached: bool) -> Value {
    let mut v = json!({"type": "text", "text": text});
    if cached {
        v["cache_control"] = json!({"type": "ephemeral"});
    }
    v
}

/// Mesajların kopyası, son mesajın son bloğunda önbellek noktası: sonraki istek (aynı turun araç
/// döngüsü ya da bir sonraki tur) geçmişi baştan ücretli okumaz, önbellekten okur.
fn cached_tail(messages: &[Value]) -> Vec<Value> {
    let mut out = messages.to_vec();
    if let Some(last) = out.last_mut() {
        if let Some(text) = last["content"].as_str().map(String::from) {
            last["content"] = json!([{"type": "text", "text": text}]);
        }
        if let Some(block) = last["content"].as_array_mut().and_then(|a| a.last_mut()) {
            block["cache_control"] = json!({"type": "ephemeral"});
        }
    }
    out
}

/// Akıştaki açık içerik bloğu.
#[derive(Default, Clone)]
struct Open {
    /// `content_block_start`'taki blok (kimlik, ad, araç takımı, gizli düşünce verisi).
    start: Value,
    kind: String,
    /// Biriken metin, araç girdisi JSON'u ya da düşünce özeti.
    acc: String,
    sig: String,
}

/// SSE akışını okur; her olayı `on_event`'e verir (`false` dönerse keser). Metin, araç çağrıları
/// ve kullanım `Reply`'da toplanır.
pub fn stream(req: &Request, on_event: &mut dyn FnMut(Event) -> bool) -> Res<Reply> {
    if req.api_key.is_empty() {
        return Err(t!("Anthropic API key is not set", "Anthropic API anahtarı girilmemiş").into());
    }
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "stream": true,
        "system": req.system,
        "messages": cached_tail(req.messages),
        // Düşünme hep açık; araç çağrıları arasındaki ilerleme notları özet olarak gelsin.
        "thinking": {"type": "adaptive", "display": "updates"},
        "output_config": {"effort": req.effort},
        // Güvenlik sınıflandırıcısı reddederse sunucu uygun modelle yeniden dener.
        "fallbacks": "default",
    });
    if !req.tools.is_empty() || !req.raw_tools.is_empty() {
        let mut tools: Vec<Value> = req.tools.iter().enumerate().map(|(i, t)| t.json(i < MAX_STRICT)).collect();
        tools.extend(req.raw_tools.iter().cloned());
        body["tools"] = Value::Array(tools);
        body["tool_choice"] = json!({"type": "auto"});
    }
    let headers = [
        ("x-api-key", req.api_key),
        ("anthropic-version", "2023-06-01"),
        ("anthropic-beta", "thinking-display-updates-2026-08-18,server-side-fallback-2026-07-01"),
        ("content-type", "application/json"),
        ("accept", "text/event-stream"),
    ];
    let payload = body.to_string();
    // Aşırı yük / hız sınırı: henüz bir şey gelmediyse birkaç kez bekleyip yeniden dene.
    let mut wait = 1500;
    for attempt in 0.. {
        let mut started = false;
        let r = stream_once(&headers, &payload, &mut |ev| {
            started = true;
            on_event(ev)
        });
        match r {
            Err(e) if !started && attempt < 3 && is_transient(&e.to_string()) => {
                crate::log!("claude geçici hata, {wait} ms sonra yeniden: {e}");
                std::thread::sleep(std::time::Duration::from_millis(wait));
                wait *= 2;
            }
            r => return r,
        }
    }
    unreachable!()
}

fn is_transient(e: &str) -> bool {
    ["HTTP 429", "HTTP 500", "HTTP 502", "HTTP 503", "HTTP 529", "overloaded", "Overloaded"].iter().any(|k| e.contains(k))
}

fn stream_once(headers: &[(&str, &str)], payload: &str, on_event: &mut dyn FnMut(Event) -> bool) -> Res<Reply> {
    let mut reply = Reply::default();
    let mut buf = String::new();
    let mut pending = Vec::<u8>::new();
    // Açık bloklar: indeks → içerik bloğu (biriken metin / JSON / düşünce ve imzası).
    let mut blocks: Vec<Open> = Vec::new();
    let mut stop = false;
    let mut err: Option<String> = None;
    net::stream("POST", URL, headers, payload.as_bytes(), &mut |chunk| {
        pending.extend_from_slice(chunk);
        // UTF-8 sınırında kes: eksik karakter bir sonraki parçayla tamamlanır.
        let valid = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(e) => e.valid_up_to(),
        };
        buf.push_str(std::str::from_utf8(&pending[..valid]).unwrap_or(""));
        pending.drain(..valid);
        while let Some(end) = buf.find("\n\n") {
            let event: String = buf[..end].to_string();
            buf.drain(..end + 2);
            let data: String = event.lines().filter_map(|l| l.strip_prefix("data:")).map(str::trim).collect::<Vec<_>>().join("\n");
            if data.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
            match v["type"].as_str().unwrap_or("") {
                "message_start" => {
                    let u = &v["message"]["usage"];
                    reply.usage.input = u["input_tokens"].as_u64().unwrap_or(0);
                    reply.usage.cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                    reply.usage.cache_write = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                }
                "content_block_start" => {
                    let i = v["index"].as_u64().unwrap_or(0) as usize;
                    let b = &v["content_block"];
                    while blocks.len() <= i {
                        blocks.push(Open::default());
                    }
                    blocks[i] = Open {
                        start: b.clone(),
                        kind: b["type"].as_str().unwrap_or("").to_string(),
                        acc: b["text"].as_str().or(b["thinking"].as_str()).unwrap_or("").to_string(),
                        sig: b["signature"].as_str().unwrap_or("").to_string(),
                    };
                }
                "content_block_delta" => {
                    let i = v["index"].as_u64().unwrap_or(0) as usize;
                    let d = &v["delta"];
                    if let Some(b) = blocks.get_mut(i) {
                        match d["type"].as_str().unwrap_or("") {
                            "text_delta" => {
                                let t = d["text"].as_str().unwrap_or("");
                                b.acc.push_str(t);
                                if !on_event(Event::Text(t.to_string())) {
                                    stop = true;
                                }
                            }
                            "input_json_delta" => b.acc.push_str(d["partial_json"].as_str().unwrap_or("")),
                            "thinking_delta" => b.acc.push_str(d["thinking"].as_str().unwrap_or("")),
                            "signature_delta" => b.sig.push_str(d["signature"].as_str().unwrap_or("")),
                            _ => {}
                        }
                    }
                }
                "content_block_stop" => {
                    let i = v["index"].as_u64().unwrap_or(0) as usize;
                    if let Some(b) = blocks.get(i).cloned() {
                        match b.kind.as_str() {
                            "text" => {
                                if !b.acc.is_empty() {
                                    reply.content.push(json!({"type": "text", "text": b.acc}));
                                }
                            }
                            "tool_use" => {
                                let input: Value = if b.acc.trim().is_empty() { json!({}) } else { serde_json::from_str(&b.acc).unwrap_or(json!({})) };
                                let (id, name) = (b.start["id"].as_str().unwrap_or("").to_string(), b.start["name"].as_str().unwrap_or("").to_string());
                                let toolset = b.start["toolset_name"].as_str().map(String::from);
                                let mut block = json!({"type": "tool_use", "id": id, "name": name, "input": input});
                                if let Some(t) = &toolset {
                                    block["toolset_name"] = json!(t);
                                }
                                reply.content.push(block);
                                if !on_event(Event::ToolUse { id, name, input, toolset }) {
                                    stop = true;
                                }
                            }
                            // Düşünme blokları aynı modele aynen (imzasıyla) geri verilir.
                            "thinking" => {
                                if !b.acc.trim().is_empty() && !on_event(Event::Progress(b.acc.trim().to_string())) {
                                    stop = true;
                                }
                                reply.content.push(json!({"type": "thinking", "thinking": b.acc, "signature": b.sig}));
                            }
                            "redacted_thinking" => reply.content.push(b.start.clone()),
                            _ => {}
                        }
                    }
                }
                "message_delta" => {
                    if let Some(r) = v["delta"]["stop_reason"].as_str() {
                        reply.stop_reason = r.to_string();
                    }
                    if let Some(o) = v["usage"]["output_tokens"].as_u64() {
                        reply.usage.output = o;
                    }
                }
                "message_stop" => {
                    on_event(Event::Stop);
                }
                "error" => {
                    err = Some(v["error"]["message"].as_str().unwrap_or("API error").to_string());
                    stop = true;
                }
                _ => {}
            }
        }
        !stop
    })?;
    if let Some(e) = err {
        return Err(e.into());
    }
    Ok(reply)
}
