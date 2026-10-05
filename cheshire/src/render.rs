//! Derlenmiş bir `.cheshire`'ı çizer. Pencereden bağımsızdır: masaüstü de başsız araçlar da
//! aynı `Engine`'i kullanır, yalnızca çıktı dokusu farklıdır.
//!
//! Geçiş sırası her karede: buf A → B → C → D → image (→ ölçek < 1 ise büyütme).
//! iChannel0..3 her zaman Buf A..D'dir. Bir geçiş, kendinden önceki buffer'ların bu karesini,
//! kendisinin ve sonrakilerin önceki karesini görür (Shadertoy ile aynı).

use bytemuck::{Pod, Zeroable};

use crate::compile::{self, Diag, PassKind};
use crate::format::{Duvar, MAX_PARAMS, Param, ParamKind};
use crate::gpu::Gpu;

const BUFFER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub const AUDIO_WIDTH: u32 = 512;

/// `DuvarGlobals` (std140) ile birebir.
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
struct Globals {
    resolution: [f32; 3],
    time: f32,
    mouse: [f32; 4],
    date: [f32; 4],
    time_delta: f32,
    frame: i32,
    battery: f32,
    local_time: f32,
    channel_resolution: [[f32; 4]; 4],
    frame_rate: f32,
    _pad: [f32; 3],
}

/// Bir kare için dış dünyadan gelen girdiler. Fare, çıktı pikselinde ve sol alt orijinlidir.
#[derive(Clone, Copy, Default)]
pub struct Inputs {
    pub time: f32,
    pub time_delta: f32,
    pub frame: u32,
    pub frame_rate: f32,
    pub mouse: [f32; 4],
    pub date: [f32; 4],
    pub battery: f32,
    pub local_time: f32,
}

const SUPPORT_WGSL: &str = r#"
@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(p * 2.0 - 1.0, 0.0, 1.0);
}

struct Blit { @builtin(position) pos: vec4f, @location(0) uv: vec2f }

@vertex
fn vs_blit(@builtin(vertex_index) i: u32) -> Blit {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    // Ekranın altı dokunun 0. satırını okur: buffer'lar Shadertoy düzeninde (y yukarı) saklanır.
    return Blit(vec4f(p * 2.0 - 1.0, 0.0, 1.0), p);
}

@group(0) @binding(0) var blit_src: texture_2d<f32>;
@group(0) @binding(1) var blit_smp: sampler;

@fragment
fn fs_blit(in: Blit) -> @location(0) vec4f {
    return textureSample(blit_src, blit_smp, in.uv);
}
"#;

struct Buffer {
    pipeline: wgpu::RenderPipeline,
    views: [wgpu::TextureView; 2],
}

struct Upscale {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

pub struct Engine {
    pub duvar: Duvar,
    format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    dummy: wgpu::TextureView,
    audio: wgpu::Texture,
    audio_view: wgpu::TextureView,
    globals: wgpu::Buffer,
    params: wgpu::Buffer,
    param_values: [[f32; 4]; MAX_PARAMS],
    buffers: [Option<Buffer>; 4],
    image: wgpu::RenderPipeline,
    upscale: Option<Upscale>,
    /// [geçiş: A..D, image][kare paritesi]
    bind_groups: Vec<[wgpu::BindGroup; 2]>,
    size: (u32, u32),
    out_size: (u32, u32),
}

fn texture(gpu: &Gpu, label: &str, (w, h): (u32, u32), format: wgpu::TextureFormat, usage: wgpu::TextureUsages) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn pipeline(
    gpu: &Gpu,
    label: &str,
    layout: &wgpu::PipelineLayout,
    vertex: &wgpu::ShaderModule,
    vs: &str,
    fragment: &wgpu::ShaderModule,
    fs: Option<&str>,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: vertex,
            entry_point: Some(vs),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: fragment,
            entry_point: fs,
            compilation_options: Default::default(),
            targets: &[Some(format.into())],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn pass<'a>(encoder: &'a mut wgpu::CommandEncoder, view: &'a wgpu::TextureView) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            // Tam ekran üçgen her pikseli yazar; Clear, eski içeriği belleğe geri okumaktan (Load) ucuzdur.
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

impl Engine {
    /// Tüm geçişleri derler. Hata varsa hiçbir GPU kaynağı oluşturulmadan döner.
    pub fn new(gpu: &Gpu, duvar: Duvar, format: wgpu::TextureFormat, out_size: (u32, u32)) -> Result<Self, Vec<Diag>> {
        let direct = duvar.scale >= 1.0;
        let image_mod = compile::compile(&duvar, &duvar.image, PassKind::Image { flip: direct })?;
        let mut buffer_mods: [Option<naga::Module>; 4] = Default::default();
        for (i, section) in duvar.buffers.iter().enumerate() {
            if let Some(s) = section {
                buffer_mods[i] = Some(compile::compile(&duvar, s, PassKind::Buffer)?);
            }
        }

        let d = &gpu.device;
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let uniform_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let sampler_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cheshire"),
            entries: &[
                uniform_entry(0),
                uniform_entry(1),
                sampler_entry(2),
                tex_entry(3),
                tex_entry(4),
                tex_entry(5),
                tex_entry(6),
                tex_entry(7),
            ],
        });
        let pipeline_layout = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });

        let support = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("destek"),
            source: wgpu::ShaderSource::Wgsl(SUPPORT_WGSL.into()),
        });
        let image_module = compile::shader_module(d, "image", image_mod);
        let image = pipeline(gpu, "image", &pipeline_layout, &support, "vs_full", &image_module, None, format);

        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let buffers = buffer_mods.map(|m| {
            m.map(|m| {
                let module = compile::shader_module(d, "buffer", m);
                Buffer {
                    pipeline: pipeline(gpu, "buffer", &pipeline_layout, &support, "vs_full", &module, None, BUFFER_FORMAT),
                    // Boyut `resize`'da gerçek değerine gelir.
                    views: [0, 1].map(|_| texture(gpu, "buf", (1, 1), BUFFER_FORMAT, usage).create_view(&Default::default())),
                }
            })
        });

        let upscale = (!direct).then(|| {
            let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("buyutme"),
                entries: &[tex_entry(0), sampler_entry(1)],
            });
            let pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let pipeline = pipeline(gpu, "buyutme", &pl, &support, "vs_blit", &support, Some("fs_blit"), format);
            let view = texture(gpu, "image", (1, 1), format, usage).create_view(&Default::default());
            let bind_group = d.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &layout, entries: &[] });
            Upscale { pipeline, layout, view, bind_group }
        });

        let sampler = d.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniform = |label, size| {
            d.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let audio = texture(gpu, "ses", (AUDIO_WIDTH, 2), wgpu::TextureFormat::R8Unorm, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);

        let mut param_values = [[0.0; 4]; MAX_PARAMS];
        for (v, p) in param_values.iter_mut().zip(&duvar.params) {
            *v = p.default;
        }

        let mut engine = Self {
            format,
            dummy: texture(gpu, "bos", (1, 1), wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureUsages::TEXTURE_BINDING)
                .create_view(&Default::default()),
            audio_view: audio.create_view(&Default::default()),
            audio,
            globals: uniform("globals", size_of::<Globals>() as u64),
            params: uniform("params", size_of::<[[f32; 4]; MAX_PARAMS]>() as u64),
            param_values,
            layout,
            sampler,
            buffers,
            image,
            upscale,
            bind_groups: Vec::new(),
            size: (0, 0),
            out_size: (0, 0),
            duvar,
        };
        engine.write_params(gpu);
        engine.resize(gpu, out_size);
        Ok(engine)
    }

    /// Çizim çözünürlüğü (ölçek uygulanmış).
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn resize(&mut self, gpu: &Gpu, out_size: (u32, u32)) {
        if out_size == self.out_size {
            return;
        }
        let s = self.duvar.scale;
        self.out_size = out_size;
        self.size = (((out_size.0 as f32 * s) as u32).max(1), ((out_size.1 as f32 * s) as u32).max(1));

        // Yeni dokular sıfırla başlar: buffer'lı efektler temiz bir sayfayla yeniden başlar.
        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        for b in self.buffers.iter_mut().flatten() {
            b.views = [0, 1].map(|_| texture(gpu, "buf", self.size, BUFFER_FORMAT, usage).create_view(&Default::default()));
        }
        if let Some(u) = &mut self.upscale {
            u.view = texture(gpu, "image", self.size, self.format, usage).create_view(&Default::default());
            u.bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &u.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&u.view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
        }

        // Geçiş p (0..3 buffer, 4 image), parite k'de: kanal i, bu karede yazıldıysa views[k],
        // henüz yazılmadıysa (kendisi ya da sonraki buffer) önceki karenin views[1-k]'sı.
        self.bind_groups = (0..5)
            .map(|p| {
                [0, 1].map(|k| {
                    let channel = |i: usize| match &self.buffers[i] {
                        None => &self.dummy,
                        Some(b) if i < p => &b.views[k],
                        Some(b) => &b.views[1 - k],
                    };
                    gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: None,
                        layout: &self.layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: self.globals.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: self.params.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(channel(0)) },
                            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(channel(1)) },
                            wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(channel(2)) },
                            wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(channel(3)) },
                            wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::TextureView(&self.audio_view) },
                        ],
                    })
                })
            })
            .collect();
    }

    pub fn param(&self, i: usize) -> [f32; 4] {
        self.param_values[i]
    }

    pub fn set_param(&mut self, gpu: &Gpu, i: usize, value: [f32; 4]) {
        if i < MAX_PARAMS && self.param_values[i] != value {
            self.param_values[i] = value;
            self.write_params(gpu);
        }
    }

    fn write_params(&self, gpu: &Gpu) {
        gpu.queue.write_buffer(&self.params, 0, bytemuck::cast_slice(&self.param_values));
    }

    /// `data`: 1. satır spektrum, 2. satır dalga biçimi; her biri `AUDIO_WIDTH` bayt.
    pub fn set_audio(&self, gpu: &Gpu, data: &[u8; AUDIO_WIDTH as usize * 2]) {
        gpu.queue.write_texture(
            self.audio.as_image_copy(),
            data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(AUDIO_WIDTH), rows_per_image: None },
            wgpu::Extent3d { width: AUDIO_WIDTH, height: 2, depth_or_array_layers: 1 },
        );
    }

    pub fn render(&self, gpu: &Gpu, encoder: &mut wgpu::CommandEncoder, out: &wgpu::TextureView, input: &Inputs) {
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        let s = self.size.0 as f32 / self.out_size.0.max(1) as f32;
        let mut channel_resolution = [[0.0; 4]; 4];
        for (r, b) in channel_resolution.iter_mut().zip(&self.buffers) {
            if b.is_some() {
                *r = [w, h, 1.0, 0.0];
            }
        }
        let g = Globals {
            resolution: [w, h, 1.0],
            time: input.time,
            mouse: input.mouse.map(|m| m * s),
            date: input.date,
            time_delta: input.time_delta,
            frame: input.frame as i32,
            battery: input.battery,
            local_time: input.local_time,
            channel_resolution,
            frame_rate: input.frame_rate,
            _pad: [0.0; 3],
        };
        gpu.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&g));

        let k = (input.frame % 2) as usize;
        for (p, b) in self.buffers.iter().enumerate() {
            let Some(b) = b else { continue };
            let mut rp = pass(encoder, &b.views[k]);
            rp.set_pipeline(&b.pipeline);
            rp.set_bind_group(0, &self.bind_groups[p][k], &[]);
            rp.draw(0..3, 0..1);
        }

        let target = self.upscale.as_ref().map_or(out, |u| &u.view);
        {
            let mut rp = pass(encoder, target);
            rp.set_pipeline(&self.image);
            rp.set_bind_group(0, &self.bind_groups[4][k], &[]);
            rp.draw(0..3, 0..1);
        }
        if let Some(u) = &self.upscale {
            let mut rp = pass(encoder, out);
            rp.set_pipeline(&u.pipeline);
            rp.set_bind_group(0, &u.bind_group, &[]);
            rp.draw(0..3, 0..1);
        }
    }
}

/// Tepsi menüsündeki hazır değerler.
pub fn presets(p: &Param) -> Vec<(String, [f32; 4])> {
    if p.toggle {
        return vec![("kapalı".into(), [0.0; 4]), ("açık".into(), [1.0, 0.0, 0.0, 0.0])];
    }
    if !p.choices.is_empty() {
        return p
            .choices
            .iter()
            .enumerate()
            .map(|(i, c)| (c.split('/').next().unwrap_or(c).trim().to_string(), [i as f32, 0.0, 0.0, 0.0]))
            .collect();
    }
    let (kind, default) = (&p.kind, p.default);
    match kind {
        ParamKind::Float { min, max } => (0..5)
            .map(|i| {
                let v = min + (max - min) * i as f32 / 4.0;
                (format!("{v:.2}"), [v, 0.0, 0.0, 0.0])
            })
            .collect(),
        ParamKind::Color => {
            let mut list = vec![("varsayılan".to_string(), default)];
            for hex in ["#4fc3f7", "#ff6ec7", "#b388ff", "#69f0ae", "#ffd740", "#ffffff"] {
                let v = u32::from_str_radix(&hex[1..], 16).unwrap();
                let c = |sh: u32| ((v >> sh) & 0xFF) as f32 / 255.0;
                list.push((hex.to_string(), [c(16), c(8), c(0), 1.0]));
            }
            list
        }
    }
}
