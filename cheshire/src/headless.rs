//! Pencere açmadan çalışan araçlar: `--dogrula` ve `--onizleme`.

use std::path::Path;
use std::time::Instant;

use crate::format::{self, Duvar, ParamKind};
use crate::gpu::Gpu;
use crate::png;
use crate::render::{self, Engine, Inputs};
use crate::util::{self, Res};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const STEP: f32 = 1.0 / 60.0;
pub const BUDGET_MS: f64 = 2.0;

pub struct PreviewOptions {
    pub time: f32,
    /// Ekran oranı, sol üst (0,0) — sağ alt (1,1).
    pub mouse: Option<[f32; 2]>,
    pub pressed: bool,
    pub size: (u32, u32),
    pub params: Vec<(String, String)>,
}

fn load(path: &Path) -> Result<Duvar, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("{}: okunamadı: {e}", path.display()))?;
    format::parse(&src).map_err(|e| format!("{}: {e}", path.display()))
}

fn build(gpu: &Gpu, path: &Path, duvar: Duvar, size: (u32, u32)) -> Result<Engine, String> {
    Engine::new(gpu, duvar, FORMAT, size).map_err(|diags| {
        diags.iter().map(|d| format!("{}: {d}", path.display())).collect::<Vec<_>>().join("\n")
    })
}

fn target(gpu: &Gpu, (w, h): (u32, u32)) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cikti"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn inputs(frame: u32, mouse: [f32; 4]) -> Inputs {
    let (date, local_time) = util::local_clock();
    Inputs {
        time: frame as f32 * STEP,
        time_delta: STEP,
        frame,
        frame_rate: 60.0,
        mouse,
        date,
        battery: util::battery_level(),
        local_time,
    }
}

/// Müziğe tepki veren duvar kâğıtlarını sessiz ortamda da görebilmek için sahte ses:
/// 120 BPM vuruş, bası güçlü, tizde azalan bir spektrum.
pub fn synthetic_audio(t: f32) -> [u8; render::AUDIO_WIDTH as usize * 2] {
    let n = render::AUDIO_WIDTH as usize;
    let beat = (1.0 - (t * 2.0).fract()).powi(3);
    let mut out = [0u8; render::AUDIO_WIDTH as usize * 2];
    for i in 0..n {
        let f = i as f32 / n as f32;
        let bass = beat * (-f * 40.0).exp();
        let body = (-f * 5.0).exp() * (0.55 + 0.25 * (t * 3.0 + f * 40.0).sin());
        out[i] = ((bass * 0.9 + body * 0.6).clamp(0.0, 1.0) * 255.0) as u8;
        let wave = (f * 60.0 + t * 20.0).sin() * (0.2 + 0.6 * beat);
        out[n + i] = ((wave * 0.5 + 0.5) * 255.0) as u8;
    }
    out
}

fn readback(gpu: &Gpu, tex: &wgpu::Texture, (w, h): (u32, u32)) -> Res<Vec<u8>> {
    let row = (w * 4).div_ceil(256) * 256;
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("okuma"),
        size: (row * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    gpu.queue.submit(Some(enc.finish()));
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely())?;
    let data = slice.get_mapped_range()?;
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for r in 0..h {
        let start = (r * row) as usize;
        out.extend_from_slice(&data[start..start + (w * 4) as usize]);
    }
    Ok(out)
}

fn apply_params(gpu: &Gpu, engine: &mut Engine, params: &[(String, String)]) -> Result<(), String> {
    for (name, value) in params {
        let (i, p) = engine
            .duvar
            .params
            .iter()
            .enumerate()
            .find(|(_, p)| &p.name == name)
            .ok_or_else(|| format!("'{name}' diye bir parametre yok"))?;
        let v = match p.kind {
            ParamKind::Float { .. } => [value.parse().map_err(|_| format!("{name}: '{value}' sayı değil"))?, 0.0, 0.0, 0.0],
            ParamKind::Color => format::parse_color(value).ok_or_else(|| format!("{name}: renk #rrggbb olmalı"))?,
        };
        engine.set_param(gpu, i, v);
    }
    Ok(())
}

/// Derler, özetler, 120 kare çizip GPU süresini ölçer. Sorun yoksa `true`.
pub fn validate(path: &Path) -> Res<bool> {
    let duvar = match load(path) {
        Ok(d) => d,
        Err(e) => {
            println!("HATA {e}");
            return Ok(false);
        }
    };
    let (gpu, _) = Gpu::new(None, false)?;
    let size = (1920, 1080);
    let engine = match build(&gpu, path, duvar, size) {
        Ok(e) => e,
        Err(e) => {
            println!("HATA {e}");
            return Ok(false);
        }
    };

    let d = &engine.duvar;
    let passes: Vec<&str> = ["A", "B", "C", "D"]
        .iter()
        .zip(&d.buffers)
        .filter_map(|(n, b)| b.as_ref().map(|_| *n))
        .collect();
    let u = &d.usage;
    let used: Vec<&str> = [(u.time, "zaman"), (u.mouse, "fare"), (u.audio, "ses"), (u.clock, "saat"), (u.battery, "pil"), (u.feedback, "geri besleme")]
        .iter()
        .filter_map(|(on, n)| on.then_some(*n))
        .collect();
    println!("derlendi: {} — {}", if d.name.is_empty() { "(adsız)" } else { &d.name }, path.display());
    println!("  geçişler: {}image{}", passes.iter().map(|p| format!("buf {p} → ")).collect::<String>(), if d.scale < 1.0 { " → büyütme" } else { "" });
    println!("  girdiler: {}", if used.is_empty() { "yok (yalnızca değişiklikte çizilir)".into() } else { used.join(", ") });
    println!("  fps sınırı: {} · ölçek: {}", d.fps.map_or("ayar".into(), |f| f.to_string()), d.scale);
    for p in &d.params {
        match p.kind {
            ParamKind::Float { min, max } => println!("  param {} = {} [{min}, {max}]", p.name, p.default[0]),
            ParamKind::Color => println!("  param {} = {}", p.name, format::color_hex(p.default)),
        }
    }

    let tex = target(&gpu, size);
    let view = tex.create_view(&Default::default());
    let (warmup, frames) = (10u32, 120u32);
    let mouse = [size.0 as f32 * 0.5, size.1 as f32 * 0.5, 0.0, 0.0];
    let queries = gpu.timestamps.then(|| {
        gpu.device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: frames * 2 })
    });

    let mut cpu_ms = Vec::new();
    for f in 0..warmup + frames {
        if d.usage.audio {
            engine.set_audio(&gpu, &synthetic_audio(f as f32 * STEP));
        }
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        let measured = f.checked_sub(warmup);
        if let (Some(q), Some(i)) = (&queries, measured) {
            enc.write_timestamp(q, i * 2);
        }
        engine.render(&gpu, &mut enc, &view, &inputs(f, mouse));
        if let (Some(q), Some(i)) = (&queries, measured) {
            enc.write_timestamp(q, i * 2 + 1);
        }
        let start = Instant::now();
        gpu.queue.submit(Some(enc.finish()));
        if queries.is_none() {
            gpu.device.poll(wgpu::PollType::wait_indefinitely())?;
            if measured.is_some() {
                cpu_ms.push(start.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }

    let times: Vec<f64> = match &queries {
        Some(q) => {
            let bytes = (frames * 2 * 8) as u64;
            let resolve = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: bytes,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut enc = gpu.device.create_command_encoder(&Default::default());
            enc.resolve_query_set(q, 0..frames * 2, &resolve, 0);
            enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, bytes);
            gpu.queue.submit(Some(enc.finish()));
            read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            gpu.device.poll(wgpu::PollType::wait_indefinitely())?;
            let data = read.slice(..).get_mapped_range()?;
            let ticks: &[u64] = bytemuck::cast_slice(&data);
            let period = gpu.queue.get_timestamp_period() as f64;
            ticks.chunks_exact(2).map(|t| t[1].saturating_sub(t[0]) as f64 * period / 1e6).collect()
        }
        None => cpu_ms,
    };
    gpu.device.poll(wgpu::PollType::wait_indefinitely())?;

    let avg = times.iter().sum::<f64>() / times.len().max(1) as f64;
    let max = times.iter().cloned().fold(0.0, f64::max);
    let method = if queries.is_some() { "GPU zaman damgası" } else { "duvar saati (yaklaşık)" };
    let (rw, rh) = engine.size();
    println!(
        "  süre: ort {avg:.3} ms · en kötü {max:.3} ms / kare  ({rw}x{rh}, {frames} kare, {method}, {})",
        gpu.adapter.get_info().name
    );
    let ok_budget = avg <= BUDGET_MS;
    println!(
        "  bütçe ({BUDGET_MS} ms): {}",
        if ok_budget { "GEÇTİ".to_string() } else { format!("AŞILDI — `olcek` düşür ya da döngüleri azalt") }
    );

    // Son kare düz tek renkse büyük ihtimalle bir şey ters gidiyor (NaN, yanlış koordinat...).
    let pixels = readback(&gpu, &tex, size)?;
    let luma: Vec<f64> = pixels
        .chunks_exact(4)
        .step_by(97)
        .map(|p| (0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64) / 255.0)
        .collect();
    let mean = luma.iter().sum::<f64>() / luma.len() as f64;
    let sd = (luma.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / luma.len() as f64).sqrt();
    println!("  son kare: ortalama parlaklık {mean:.3}, sapma {sd:.3}");
    if sd < 0.003 {
        println!("  UYARI: görüntü neredeyse tek renk — NaN, sıfıra bölme ya da koordinat hatası olabilir");
    }
    Ok(ok_budget)
}

/// `opts.time` saniyesindeki kareyi PNG olarak yazar. Buffer'lı efektler için 0'dan itibaren
/// 60 FPS'le simüle edilir, böylece iz/sıvı gibi birikimli durumlar gerçek hâlini alır.
pub fn preview(path: &Path, out: &Path, opts: &PreviewOptions) -> Res<bool> {
    let duvar = match load(path) {
        Ok(d) => d,
        Err(e) => {
            println!("HATA {e}");
            return Ok(false);
        }
    };
    let (gpu, _) = Gpu::new(None, false)?;
    let mut engine = match build(&gpu, path, duvar, opts.size) {
        Ok(e) => e,
        Err(e) => {
            println!("HATA {e}");
            return Ok(false);
        }
    };
    if let Err(e) = apply_params(&gpu, &mut engine, &opts.params) {
        println!("HATA {e}");
        return Ok(false);
    }

    let (w, h) = (opts.size.0 as f32, opts.size.1 as f32);
    let mouse = match opts.mouse {
        Some([x, y]) => {
            let (px, py) = (x * w, (1.0 - y) * h);
            // Shadertoy: z,w tıklama konumu; basılıyken pozitif, bırakılınca negatif.
            let sign = if opts.pressed { 1.0 } else { -1.0 };
            [px, py, px * sign, py * sign]
        }
        None => [0.0; 4],
    };

    let tex = target(&gpu, opts.size);
    let view = tex.create_view(&Default::default());
    let last = (opts.time / STEP).round() as u32;
    let first = if engine.duvar.usage.feedback { 0 } else { last };
    for f in first..=last {
        let mut input = inputs(f, mouse);
        input.time = f as f32 * STEP;
        if engine.duvar.usage.audio {
            engine.set_audio(&gpu, &synthetic_audio(input.time));
        }
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        engine.render(&gpu, &mut enc, &view, &input);
        gpu.queue.submit(Some(enc.finish()));
        if f % 60 == 0 {
            gpu.device.poll(wgpu::PollType::wait_indefinitely())?;
        }
    }
    let pixels = readback(&gpu, &tex, opts.size)?;
    std::fs::write(out, png::encode(opts.size.0, opts.size.1, &pixels))?;
    println!("önizleme: {} ({}x{}, t = {:.2} s, {} kare)", out.display(), opts.size.0, opts.size.1, opts.time, last - first + 1);
    Ok(true)
}
