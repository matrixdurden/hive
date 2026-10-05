//! `.cheshire` bölümlerini Vulkan GLSL'e çevirip naga ile derler.
//!
//! naga'nın GLSL okuyucusu birleşik `uniform sampler2D` globali desteklemiyor; bu yüzden
//! doku ve sampler ayrı tanımlanır, `iChannel0` gibi adlar `sampler2D(doku, sampler)`
//! ifadesine açılan makrolardır. Hatalar, prelude payı düşülerek dosyadaki satıra çevrilir.

use std::borrow::Cow;
use std::fmt;

use crate::format::{Duvar, ParamKind, Section};

const PRELUDE: &str = r#"#version 450
layout(set = 0, binding = 0, std140) uniform DuvarGlobals {
    vec3 iResolution;
    float iTime;
    vec4 iMouse;
    vec4 iDate;
    float iTimeDelta;
    int iFrame;
    float iBattery;
    float iLocalTime;
    vec3 iChannelResolution[4];
    float iFrameRate;
    vec4 iWeather;
};
layout(set = 0, binding = 1, std140) uniform DuvarParams { vec4 duvar_p[16]; };
layout(set = 0, binding = 2) uniform sampler duvar_smp;
layout(set = 0, binding = 3) uniform texture2D duvar_ch0;
layout(set = 0, binding = 4) uniform texture2D duvar_ch1;
layout(set = 0, binding = 5) uniform texture2D duvar_ch2;
layout(set = 0, binding = 6) uniform texture2D duvar_ch3;
layout(set = 0, binding = 7) uniform texture2D duvar_audio;
#define iChannel0 sampler2D(duvar_ch0, duvar_smp)
#define iChannel1 sampler2D(duvar_ch1, duvar_smp)
#define iChannel2 sampler2D(duvar_ch2, duvar_smp)
#define iChannel3 sampler2D(duvar_ch3, duvar_smp)
#define iAudio sampler2D(duvar_audio, duvar_smp)
layout(location = 0) out vec4 duvar_out;
"#;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PassKind {
    Buffer,
    /// `flip`: doğrudan ekrana çiziliyorsa fragCoord'un y'si çevrilir (Shadertoy: orijin sol alt).
    Image { flip: bool },
}

/// Bir derleme hatası; `line` dosyadaki satırdır (prelude içindeyse `None`).
pub struct Diag {
    pub line: Option<u32>,
    pub msg: String,
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.line {
            Some(l) => write!(f, "satır {l}: {}", self.msg),
            None => write!(f, "{}", self.msg),
        }
    }
}

/// Birleştirilmiş kaynaktaki satır aralıklarını dosya satırlarına eşler.
struct LineMap(Vec<(u32, u32, u32)>); // (birleşik başlangıç, satır sayısı, dosya başlangıcı)

impl LineMap {
    fn file_line(&self, composed: u32) -> Option<u32> {
        self.0
            .iter()
            .find(|(start, len, _)| composed >= *start && composed < start + len)
            .map(|(start, _, file)| file + (composed - start))
    }
}

fn line_count(s: &str) -> u32 {
    s.matches('\n').count() as u32
}

fn assemble(d: &Duvar, body: &Section, kind: PassKind) -> (String, LineMap) {
    let mut src = String::from(PRELUDE);
    for (i, p) in d.params.iter().enumerate() {
        let swizzle = match p.kind {
            ParamKind::Float { .. } => "x",
            ParamKind::Color => "rgb",
        };
        src.push_str(&format!("#define {} (duvar_p[{i}].{swizzle})\n", p.name));
    }

    let mut map = Vec::new();
    for section in [&d.common, body] {
        if section.code.is_empty() {
            continue;
        }
        map.push((line_count(&src) + 1, line_count(&section.code), section.line));
        src.push_str(&section.code);
    }

    let (coord, finish) = match kind {
        PassKind::Buffer => ("gl_FragCoord.xy", "c"),
        PassKind::Image { flip: false } => ("gl_FragCoord.xy", "vec4(c.rgb, 1.0)"),
        PassKind::Image { flip: true } => ("vec2(gl_FragCoord.x, iResolution.y - gl_FragCoord.y)", "vec4(c.rgb, 1.0)"),
    };
    src.push_str(&format!(
        "\nvoid main() {{\n    vec4 c = vec4(0.0, 0.0, 0.0, 1.0);\n    mainImage(c, {coord});\n    duvar_out = {finish};\n}}\n"
    ));
    (src, LineMap(map))
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut msg = e.to_string();
    let mut cur = e.source();
    while let Some(s) = cur {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        cur = s.source();
    }
    msg
}

/// Doğrulanmış naga modülü üretir. wgpu'ya `ShaderSource::Naga` ile verilir, ikinci kez ayrıştırılmaz.
pub fn compile(d: &Duvar, body: &Section, kind: PassKind) -> Result<naga::Module, Vec<Diag>> {
    let (src, map) = assemble(d, body, kind);
    let at = |span: naga::Span| {
        if span == naga::Span::default() {
            return None;
        }
        map.file_line(span.location(&src).line_number)
    };

    let options = naga::front::glsl::Options::from(naga::ShaderStage::Fragment);
    let module = naga::front::glsl::Frontend::default().parse(&options, &src).map_err(|errs| {
        errs.errors.iter().map(|e| Diag { line: at(e.meta), msg: e.kind.to_string() }).collect::<Vec<_>>()
    })?;

    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::default())
        .validate(&module)
        .map_err(|e| {
            let line = e.spans().filter_map(|(span, _)| at(*span)).next();
            vec![Diag { line, msg: error_chain(e.as_inner()) }]
        })?;
    Ok(module)
}

pub fn shader_module(device: &wgpu::Device, label: &str, module: naga::Module) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Naga(Cow::Owned(module)),
    })
}
