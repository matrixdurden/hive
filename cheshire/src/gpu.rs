use std::num::NonZeroIsize;

use raw_window_handle::{RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle};
use windows::Win32::Foundation::HWND;

use crate::log;
use crate::util::Res;

pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// Komut kodlayıcı içinde zaman damgası yazılabiliyor mu (`--dogrula` ölçümü için).
    pub timestamps: bool,
}

pub struct Target {
    pub surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
}

fn create_surface(instance: &wgpu::Instance, hwnd: HWND) -> Res<wgpu::Surface<'static>> {
    let handle = Win32WindowHandle::new(NonZeroIsize::new(hwnd.0 as isize).ok_or("geçersiz pencere")?);
    let target = wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(RawDisplayHandle::Windows(WindowsDisplayHandle::new())),
        raw_window_handle: RawWindowHandle::Win32(handle),
    };
    // Güvenlik: pencere, yüzeyden önce yok edilmez — App bunu alan sırasıyla garanti eder.
    Ok(unsafe { instance.create_surface_unsafe(target)? })
}

impl Gpu {
    /// `hwnd` verilirse o pencereyle uyumlu adaptör seçilir ve yüzeyi de döner; yoksa başsız.
    pub fn new(hwnd: Option<HWND>, high_performance: bool) -> Res<(Self, Option<wgpu::Surface<'static>>)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = hwnd.map(|h| create_surface(&instance, h)).transpose()?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: if high_performance {
                wgpu::PowerPreference::HighPerformance
            } else {
                wgpu::PowerPreference::LowPower
            },
            force_fallback_adapter: false,
            compatible_surface: surface.as_ref(),
            apply_limit_buckets: false,
        }))?;
        log!("GPU: {} ({:?})", adapter.get_info().name, adapter.get_info().device_type);

        let wanted = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        let timestamps = hwnd.is_none() && adapter.features().contains(wanted);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("cheshire"),
            required_features: if timestamps { wanted } else { wgpu::Features::empty() },
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        }))?;
        device.on_uncaptured_error(std::sync::Arc::new(|e| log!("GPU hatası: {e}")));

        Ok((Self { instance, adapter, device, queue, timestamps }, surface))
    }

    pub fn surface_for(&self, hwnd: HWND) -> Res<wgpu::Surface<'static>> {
        create_surface(&self.instance, hwnd)
    }
}

impl Target {
    pub fn new(gpu: &Gpu, surface: wgpu::Surface<'static>, width: u32, height: u32) -> Res<Self> {
        let caps = surface.get_capabilities(&gpu.adapter);
        let mut config = surface
            .get_default_config(&gpu.adapter, width.max(1), height.max(1))
            .ok_or("yüzey bu GPU ile uyumlu değil")?;
        // sRGB olmayan format: shader'daki renkler Shadertoy'daki gibi yazıldığı değerle görünür.
        if let Some(f) = caps.formats.iter().find(|f| !f.is_srgb()) {
            config.format = *f;
        }
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 1;
        surface.configure(&gpu.device, &config);
        log!("yüzey: {}x{} {:?}", config.width, config.height, config.format);
        Ok(Self { surface, config })
    }

    pub fn resize(&mut self, gpu: &Gpu, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&gpu.device, &self.config);
    }
}
