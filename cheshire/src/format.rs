//! `.cheshire` dosya formatı: yorum satırlarında metadata, Shadertoy lehçesinde GLSL gövde.
//!
//! ```text
//! // @cheshire
//! // ad: Akış
//! // fps: 30
//! // param hiz: 1.0 [0.1, 3]
//! // ad.tr: Akış                                       (isteğe bağlı Türkçe ad)
//! // param hiz "Speed" "Hız": 1.0 [0.1, 3]               (görünen ad: İngilizce, isteğe bağlı Türkçe)
//! // param tema "Theme" "Tema": night [charcoal/kömür, night/gece]   (seçenek: değer sıra no, 0..)
//! // param saat24 "24-hour clock" "24 saat": on         (aç/kapa: on/off ya da açık/kapalı; 1 ya da 0)
//! // param renk "Color" "Renk": #4fc3f7
//! //--- ortak      (isteğe bağlı, her geçişin başına eklenir)
//! //--- buf A      (isteğe bağlı, A..D)
//! //--- image      (bölüm yoksa tüm gövde image sayılır)
//! ```

use std::collections::HashSet;

use crate::util::Res;

pub const MAX_PARAMS: usize = 16;

#[derive(Clone, Debug, PartialEq)]
pub enum ParamKind {
    Float { min: f32, max: f32 },
    Color,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub kind: ParamKind,
    /// Float: [değer, 0, 0, 0] · Color: [r, g, b, 1] (0..1)
    pub default: [f32; 4],
    /// Arayüzde görünen ad (`param ad "Görünen ad" "Türkçe ad": ...`); yoksa GLSL adı.
    pub label: Option<String>,
    pub label_tr: Option<String>,
    /// Seçenekli parametre: değer seçeneğin sırası (Float, 0..n-1). Öğe `english/türkçe` olabilir.
    pub choices: Vec<String>,
    /// Aç/kapa: değer 0 ya da 1 (Float).
    pub toggle: bool,
}

/// Dosyanın bir bölümü ve dosyadaki başlangıç satırı (1 tabanlı), hata satırlarını eşlemek için.
#[derive(Clone, Debug, Default)]
pub struct Section {
    pub code: String,
    pub line: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub time: bool,
    pub mouse: bool,
    pub audio: bool,
    pub clock: bool,
    pub battery: bool,
    pub weather: bool,
    /// Bir buffer `iChannel` okuyor: önceki kareye bağlı, her kare yeniden çizilmeli.
    pub feedback: bool,
}


#[derive(Clone, Debug)]
pub struct Duvar {
    pub name: String,
    /// `// ad.tr:` ile verilen Türkçe ad.
    pub name_tr: Option<String>,
    pub author: String,
    pub fps: Option<u32>,
    pub scale: f32,
    pub params: Vec<Param>,
    pub common: Section,
    /// A..D sırasıyla; olmayan buffer `None`.
    pub buffers: [Option<Section>; 4],
    pub image: Section,
    pub usage: Usage,
}


pub fn parse_color(s: &str) -> Option<[f32; 4]> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    let c = |shift: u32| ((v >> shift) & 0xFF) as f32 / 255.0;
    Some([c(16), c(8), c(0), 1.0])
}

pub fn color_hex(c: [f32; 4]) -> String {
    let b = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Aç/kapa değeri: `açık`/`kapalı` (ya da `evet`/`hayır`).
fn parse_switch(s: &str) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        "on" | "yes" | "açık" | "acik" | "evet" => Some(true),
        "off" | "no" | "kapalı" | "kapali" | "hayır" | "hayir" => Some(false),
        _ => None,
    }
}

/// `hiz: 1.0 [0.1, 3]`, `tema "Tema": gece [kömür, gece]`, `saat24: açık` ya da `renk: #4fc3f7`
fn parse_param(spec: &str, line: u32) -> Res<Param> {
    let err = |m: &str| format!("satır {line}: {m} → `// param ad: 1.0 [0, 2]` ya da `// param ad: #rrggbb`");
    let (head, rest) = spec.split_once(':').ok_or_else(|| err("parametrede ':' yok"))?;
    // İsteğe bağlı görünen adlar: `ad "Görünen ad" "Türkçe ad"`.
    let (name, labels) = match head.split_once('"') {
        Some((n, l)) => {
            let labels: Vec<String> =
                l.split('"').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect();
            (n.trim(), labels)
        }
        None => (head.trim(), Vec::new()),
    };
    let (label, label_tr) = (labels.first().cloned(), labels.get(1).cloned());
    if !is_ident(name) {
        return Err(err(&format!("'{name}' geçerli bir GLSL adı değil")).into());
    }
    let base = |kind, default| Param {
        name: name.into(),
        kind,
        default,
        label: label.clone(),
        label_tr: label_tr.clone(),
        choices: Vec::new(),
        toggle: false,
    };
    let rest = rest.trim();
    if let Some(c) = parse_color(rest) {
        return Ok(base(ParamKind::Color, c));
    }
    if let Some(on) = parse_switch(rest) {
        let v = if on { 1.0 } else { 0.0 };
        return Ok(Param { toggle: true, ..base(ParamKind::Float { min: 0.0, max: 1.0 }, [v, 0.0, 0.0, 0.0]) });
    }
    // Seçenek listesi: aralıkta sayı olmayan öğeler var.
    if let Some((v, r)) = rest.split_once('[') {
        let items: Vec<String> =
            r.trim_end_matches(']').split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
        if items.len() >= 2 && items.iter().any(|x| x.parse::<f32>().is_err()) {
            let v = v.trim();
            let index = items
                .iter()
                .position(|x| x.split('/').any(|part| part.trim().eq_ignore_ascii_case(v)))
                .or_else(|| v.parse::<usize>().ok().filter(|&i| i < items.len()))
                .ok_or_else(|| err(&format!("'{v}' seçeneklerden biri değil")))?;
            let max = (items.len() - 1) as f32;
            return Ok(Param {
                choices: items,
                ..base(ParamKind::Float { min: 0.0, max }, [index as f32, 0.0, 0.0, 0.0])
            });
        }
    }
    let (value, range) = match rest.split_once('[') {
        Some((v, r)) => (v.trim(), Some(r.trim_end_matches(']'))),
        None => (rest, None),
    };
    let value: f32 = value.parse().map_err(|_| err(&format!("'{value}' sayı değil")))?;
    let (min, max) = match range {
        Some(r) => {
            let (a, b) = r.split_once(',').ok_or_else(|| err("aralık [min, max] biçiminde olmalı"))?;
            let a: f32 = a.trim().parse().map_err(|_| err("aralık sayı değil"))?;
            let b: f32 = b.trim().parse().map_err(|_| err("aralık sayı değil"))?;
            (a.min(b), a.max(b))
        }
        None => (value.min(0.0), value.abs().max(1.0) * 2.0),
    };
    Ok(base(ParamKind::Float { min, max }, [value, 0.0, 0.0, 0.0]))
}

/// Yorumlar ve dizgeler dışındaki tanımlayıcılar.
fn identifiers(code: &str) -> HashSet<&str> {
    let mut out = HashSet::new();
    let bytes = code.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                out.insert(&code[start..i]);
            }
            _ => i += 1,
        }
    }
    out
}

pub fn parse(src: &str) -> Res<Duvar> {
    let mut d = Duvar {
        name: String::new(),
        name_tr: None,
        author: String::new(),
        fps: None,
        scale: 1.0,
        params: Vec::new(),
        common: Section::default(),
        buffers: Default::default(),
        image: Section { code: String::new(), line: 1 },
        usage: Usage::default(),
    };

    // Bölüm başlığı yoksa tüm dosya image sayılır. Her satır korunur (metadata boş satır olur),
    // böylece bölümdeki n. satır her zaman dosyadaki `section.line + n - 1`. satırdır.
    #[derive(Clone, Copy, PartialEq)]
    enum Target {
        Common,
        Buffer(usize),
        Image,
    }
    let mut target = Target::Image;
    let mut seen_section = false;
    let mut in_header = true;

    for (i, raw) in src.lines().enumerate() {
        let line = i as u32 + 1;
        let t = raw.trim();

        if let Some(name) = t.strip_prefix("//---") {
            let name = name.trim().to_lowercase();
            target = match name.as_str() {
                "ortak" | "common" => Target::Common,
                "image" | "görüntü" => Target::Image,
                n => match n.strip_prefix("buf").map(str::trim) {
                    Some("a") => Target::Buffer(0),
                    Some("b") => Target::Buffer(1),
                    Some("c") => Target::Buffer(2),
                    Some("d") => Target::Buffer(3),
                    _ => return Err(format!("satır {line}: bilinmeyen bölüm '{name}' (ortak, buf A-D, image)").into()),
                },
            };
            if !seen_section {
                // İlk başlıktan önce yalnızca metadata ve yorum olabilir.
                if !d.image.code.lines().all(|l| l.trim().is_empty() || l.trim().starts_with("//")) {
                    return Err(format!("satır {line}: ilk bölüm başlığından önce kod var; onu bir bölümün altına taşı").into());
                }
                d.image.code.clear();
            }
            seen_section = true;
            let section = Section { code: String::new(), line: line + 1 };
            match target {
                Target::Common => d.common = section,
                Target::Buffer(b) => d.buffers[b] = Some(section),
                Target::Image => d.image = section,
            }
            in_header = false;
            continue;
        }

        let mut is_meta = false;
        if in_header {
            match t.strip_prefix("//").map(str::trim) {
                Some("@cheshire") => is_meta = true,
                Some(meta) => {
                    if let Some(spec) = meta.strip_prefix("param ") {
                        if d.params.len() == MAX_PARAMS {
                            return Err(format!("satır {line}: en fazla {MAX_PARAMS} parametre").into());
                        }
                        d.params.push(parse_param(spec, line)?);
                        is_meta = true;
                    } else if let Some((k, v)) = meta.split_once(':') {
                        let v = v.trim();
                        is_meta = true;
                        match k.trim() {
                            "ad" | "name" => d.name = v.into(),
                            "ad.tr" | "name.tr" => d.name_tr = Some(v.into()),
                            "yazar" => d.author = v.into(),
                            "fps" => d.fps = Some(v.parse().map_err(|_| format!("satır {line}: fps sayı olmalı"))?),
                            "olcek" | "ölçek" => {
                                d.scale = v.parse().map_err(|_| format!("satır {line}: olcek sayı olmalı"))?
                            }
                            _ => is_meta = false,
                        }
                    }
                }
                None if t.is_empty() => {}
                None => in_header = false,
            }
        }

        let section = match target {
            Target::Common => &mut d.common,
            Target::Buffer(b) => d.buffers[b].as_mut().expect("bölüm başlığında oluşturuldu"),
            Target::Image => &mut d.image,
        };
        if !is_meta {
            section.code.push_str(raw);
        }
        section.code.push('\n');
    }

    for (i, p) in d.params.iter().enumerate() {
        if d.params[..i].iter().any(|q| q.name == p.name) {
            return Err(format!("'{}' parametresi iki kez tanımlanmış", p.name).into());
        }
    }
    if !identifiers(&d.image.code).contains("mainImage") {
        return Err("image bölümünde `void mainImage(out vec4 fragColor, in vec2 fragCoord)` yok".into());
    }
    d.scale = d.scale.clamp(0.1, 1.0);

    let mut all = d.common.code.clone();
    all.push_str(&d.image.code);
    for b in d.buffers.iter().flatten() {
        all.push_str(&b.code);
    }
    let ids = identifiers(&all);
    let any = |names: &[&str]| names.iter().any(|n| ids.contains(n));
    let mut bufs = d.common.code.clone();
    for b in d.buffers.iter().flatten() {
        bufs.push_str(&b.code);
    }
    let buf_ids = identifiers(&bufs);
    d.usage = Usage {
        time: any(&["iTime", "iTimeDelta", "iFrame", "iFrameRate"]),
        mouse: any(&["iMouse"]),
        audio: any(&["iAudio"]),
        clock: any(&["iDate", "iLocalTime"]),
        battery: any(&["iBattery"]),
        weather: any(&["iWeather"]),
        feedback: ["iChannel0", "iChannel1", "iChannel2", "iChannel3"].iter().any(|n| buf_ids.contains(n)),
    };
    Ok(d)
}
