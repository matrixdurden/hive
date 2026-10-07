//! Direct2D + DirectWrite çizim yardımcıları. Birim DIP: 96 dpi'da 1 piksel.

use std::cell::RefCell;

use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{BOOL, Interface, PCWSTR, w};

use crate::util::Res;

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Rect {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

impl Rect {
    pub const fn new(l: f32, t: f32, r: f32, b: f32) -> Self {
        Self { l, t, r, b }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }

    pub fn cy(&self) -> f32 {
        (self.t + self.b) / 2.0
    }

    pub fn h(&self) -> f32 {
        self.b - self.t
    }

    pub fn w(&self) -> f32 {
        self.r - self.l
    }

    fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.l, top: self.t, right: self.r, bottom: self.b }
    }
}

/// 0xRRGGBB ve saydamlık.
#[derive(Clone, Copy)]
pub struct Color(pub u32, pub f32);

impl Color {
    pub const fn rgb(c: u32) -> Self {
        Self(c, 1.0)
    }

    /// Saydamlıkla çarpar: opak renkte `a` olur, yarı saydam tema renklerinde orantılı kalır.
    pub const fn alpha(self, a: f32) -> Self {
        Self(self.0, self.1 * a)
    }

    fn d2d(self) -> D2D1_COLOR_F {
        let c = |s: u32| ((self.0 >> s) & 0xFF) as f32 / 255.0;
        D2D1_COLOR_F { r: c(16), g: c(8), b: c(0), a: self.1 }
    }
}

pub struct Fonts {
    /// Araç sayfasının başlığı: 18 px yarı kalın.
    pub heading: IDWriteTextFormat,
    /// Uygulama adı, ayar başlıkları: 14 px yarı kalın.
    pub strong: IDWriteTextFormat,
    /// Menü ve satır yazıları: 13 px, sığmazsa "…".
    pub text: IDWriteTextFormat,
    /// Açıklamalar: 12 px.
    pub small: IDWriteTextFormat,
    pub small_center: IDWriteTextFormat,
    pub small_right: IDWriteTextFormat,
    /// Düğme yazısı: 13 px yarı kalın, ortalı.
    pub button: IDWriteTextFormat,
    pub icon: IDWriteTextFormat,
    pub icon_small: IDWriteTextFormat,
    pub icon_large: IDWriteTextFormat,
    /// Pencere düğmelerinin simgeleri: 10 px.
    pub icon_caption: IDWriteTextFormat,
}

/// Gömülü PNG'nin farklı boyutları; çizim hedefi yeniden yaratılınca bitmap'ler de yenilenir.
struct Image {
    sizes: Vec<(u32, IWICBitmapSource)>,
    cache: RefCell<Vec<Option<ID2D1Bitmap>>>,
}

#[derive(Clone, Copy)]
pub struct ImageId(usize);

pub struct Gfx {
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
    wic: IWICImagingFactory,
    rt: Option<(ID2D1HwndRenderTarget, ID2D1SolidColorBrush)>,
    dpi: f32,
    /// Önçarpımlı alfa: pencerenin camı (acrylic) saydam yerlerden görünür.
    transparent: bool,
    images: Vec<Image>,
    pub f: Fonts,
}

impl Gfx {
    pub fn new() -> Res<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let format = |family: PCWSTR,
                          size: f32,
                          weight: DWRITE_FONT_WEIGHT,
                          align: DWRITE_TEXT_ALIGNMENT|
             -> Res<IDWriteTextFormat> {
                let f = dw.CreateTextFormat(
                    family,
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("tr-tr"),
                )?;
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                f.SetTextAlignment(align)?;
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                Ok(f)
            };
            let ellipsis = |f: &IDWriteTextFormat| -> Res<()> {
                let sign = dw.CreateEllipsisTrimmingSign(f)?;
                let trim = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                };
                f.SetTrimming(&trim, &sign)?;
                Ok(())
            };
            // Windows 11'in yazı tipi; olmayan sistemde DirectWrite Segoe UI'a düşer.
            let ui = w!("Segoe UI Variable Text");
            let (normal, semi) = (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD);
            let (lead, center, right) =
                (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_TRAILING);

            let heading = format(w!("Segoe UI Variable Display"), 20.0, semi, lead)?;
            let strong = format(ui, 14.0, semi, lead)?;
            let text = format(ui, 13.0, normal, lead)?;
            let small = format(ui, 12.0, normal, lead)?;
            for f in [&heading, &strong, &text, &small] {
                ellipsis(f)?;
            }

            // Windows 11'de Fluent, 10'da MDL2: ikisi de aynı kod noktalarını kullanır.
            let mut fonts = None;
            dw.GetSystemFontCollection(&mut fonts, false)?;
            let (mut index, mut fluent) = (0u32, BOOL(0));
            if let Some(f) = &fonts {
                f.FindFamilyName(w!("Segoe Fluent Icons"), &mut index, &mut fluent)?;
            }
            let icons = if fluent.as_bool() { w!("Segoe Fluent Icons") } else { w!("Segoe MDL2 Assets") };

            let f = Fonts {
                heading,
                strong,
                text,
                small,
                small_center: format(ui, 12.0, normal, center)?,
                small_right: format(ui, 12.0, normal, right)?,
                button: format(ui, 13.0, semi, center)?,
                icon: format(icons, 16.0, normal, center)?,
                icon_small: format(icons, 12.0, normal, center)?,
                icon_large: format(icons, 30.0, normal, center)?,
                icon_caption: format(icons, 10.0, normal, center)?,
            };
            Ok(Self { d2d, dw, wic, rt: None, dpi: 96.0, transparent: false, images: Vec::new(), f })
        }
    }

    /// PNG'leri (piksel, veri) olarak yükler. Veri program boyunca yaşamalı: WIC akışı onu kopyalamaz.
    pub fn load_image(&mut self, pngs: &[(u32, &'static [u8])]) -> Res<ImageId> {
        let mut sizes = Vec::new();
        unsafe {
            for &(px, data) in pngs {
                let stream = self.wic.CreateStream()?;
                stream.InitializeFromMemory(data)?;
                let decoder = self.wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
                let frame = decoder.GetFrame(0)?;
                let conv = self.wic.CreateFormatConverter()?;
                conv.Initialize(
                    &frame,
                    &GUID_WICPixelFormat32bppPBGRA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeMedianCut,
                )?;
                sizes.push((px, conv.cast()?));
            }
        }
        sizes.sort_by_key(|s| s.0);
        let n = sizes.len();
        self.images.push(Image { sizes, cache: RefCell::new(vec![None; n]) });
        Ok(ImageId(self.images.len() - 1))
    }

    /// Diskteki resmi (önizleme PNG'si) yükler. `reuse` verilirse o resmin yerine geçer: eskisi
    /// bırakılır, dosyası da kilitten kurtulur.
    pub fn load_image_file(&mut self, path: &std::path::Path, reuse: Option<ImageId>) -> Res<ImageId> {
        let p = crate::util::wide(&path.display().to_string());
        let conv = unsafe {
            let decoder = self.wic.CreateDecoderFromFilename(
                windows::core::PCWSTR(p.as_ptr()),
                None,
                windows::Win32::Foundation::GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )?;
            let frame = decoder.GetFrame(0)?;
            let conv = self.wic.CreateFormatConverter()?;
            conv.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeMedianCut,
            )?;
            conv
        };
        let img = Image { sizes: vec![(u32::MAX, conv.cast()?)], cache: RefCell::new(vec![None]) };
        if let Some(id) = reuse {
            self.images[id.0] = img;
            return Ok(id);
        }
        self.images.push(img);
        Ok(ImageId(self.images.len() - 1))
    }


    pub fn wic(&self) -> &IWICImagingFactory {
        &self.wic
    }


    /// WIC resmini kopyalayarak yükler.
    pub fn load_bitmap_source(&mut self, src: &IWICBitmapSource) -> Res<ImageId> {
        let bmp: IWICBitmapSource = unsafe { self.wic.CreateBitmapFromSource(src, WICBitmapCacheOnLoad)?.cast()? };
        self.images.push(Image { sizes: vec![(u32::MAX, bmp)], cache: RefCell::new(vec![None]) });
        Ok(ImageId(self.images.len() - 1))
    }


    /// `n`'den sonraki resimleri bırakır (pencere başına geçici resimler).
    pub fn truncate_images(&mut self, n: usize) {
        self.images.truncate(n);
    }

    /// Çizim hedefini bırakır: sonraki `begin` yeni pencereye yeni hedef kurar.
    pub fn reset_target(&mut self) {
        self.rt = None;
    }

    /// Saydam çizim (önçarpımlı alfa); hedef ilk çizimden önce ayarlanmalı.
    pub fn set_transparent(&mut self, on: bool) {
        if self.transparent != on {
            self.transparent = on;
            self.rt = None;
        }
    }

    /// Çizime başlar; hedef yoksa oluşturur, boyutu pencereyle eşitler. Çizilecek yoksa `false`.
    pub fn begin(&mut self, hwnd: HWND, dpi: f32, bg: Color) -> bool {
        self.dpi = dpi;
        unsafe {
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let size = D2D_SIZE_U { width: rc.right as u32, height: rc.bottom as u32 };
            if size.width == 0 || size.height == 0 {
                return false;
            }
            if self.rt.is_none() {
                let mut props = D2D1_RENDER_TARGET_PROPERTIES { dpiX: dpi, dpiY: dpi, ..Default::default() };
                if self.transparent {
                    props.pixelFormat = D2D1_PIXEL_FORMAT {
                        format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    };
                }
                let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                    hwnd,
                    pixelSize: size,
                    presentOptions: D2D1_PRESENT_OPTIONS_NONE,
                };
                let Ok(rt) = self.d2d.CreateHwndRenderTarget(&props, &hwnd_props) else {
                    return false;
                };
                let Ok(brush) = rt.CreateSolidColorBrush(&bg.d2d(), None) else {
                    return false;
                };
                rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                self.rt = Some((rt, brush));
                for img in &self.images {
                    img.cache.borrow_mut().iter_mut().for_each(|b| *b = None);
                }
            }
            let Some((rt, _)) = &self.rt else {
                return false;
            };
            let _ = rt.Resize(&size);
            rt.SetDpi(dpi, dpi);
            rt.BeginDraw();
            rt.Clear(Some(&bg.d2d()));
            true
        }
    }

    /// Hedef kaybolduysa (sürücü sıfırlandı vb.) `true`: yeniden çizilmeli.
    pub fn end(&mut self) -> bool {
        if let Some((rt, _)) = &self.rt
            && let Err(e) = unsafe { rt.EndDraw(None, None) }
            && e.code() == D2DERR_RECREATE_TARGET
        {
            self.rt = None;
            return true;
        }
        false
    }

    fn with(&self, c: Color, f: impl FnOnce(&ID2D1HwndRenderTarget, &ID2D1SolidColorBrush)) {
        if let Some((rt, brush)) = &self.rt {
            unsafe { brush.SetColor(&c.d2d()) };
            f(rt, brush);
        }
    }

    pub fn fill(&self, r: Rect, radius: f32, c: Color) {
        self.with(c, |rt, b| unsafe {
            if radius > 0.0 {
                rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius }, b)
            } else {
                rt.FillRectangle(&r.d2d(), b)
            }
        });
    }

    pub fn stroke(&self, r: Rect, radius: f32, c: Color, width: f32) {
        let h = width / 2.0;
        let r = Rect::new(r.l + h, r.t + h, r.r - h, r.b - h);
        self.with(c, |rt, b| unsafe {
            rt.DrawRoundedRectangle(
                &D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius },
                b,
                width,
                None,
            )
        });
    }

    pub fn circle(&self, x: f32, y: f32, radius: f32, c: Color) {
        self.fill(Rect::new(x - radius, y - radius, x + radius, y + radius), radius, c);
    }

    pub fn text(&self, s: &str, format: &IDWriteTextFormat, r: Rect, c: Color) {
        let s: Vec<u16> = s.encode_utf16().collect();
        self.with(c, |rt, b| unsafe {
            rt.DrawText(&s, format, &r.d2d(), b, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL)
        });
    }

    /// Bundan sonraki çizimler `dx, dy` kadar kayık: araç sayfaları kendi köşelerinden çizer.
    pub fn translate(&self, dx: f32, dy: f32) {
        if let Some((rt, _)) = &self.rt {
            let m = windows_numerics::Matrix3x2::translation(dx, dy);
            unsafe { rt.SetTransform(&m) };
        }
    }

    pub fn clip(&self, r: Rect, f: impl FnOnce()) {
        let Some((rt, _)) = &self.rt else { return };
        unsafe { rt.PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
        f();
        unsafe { rt.PopAxisAlignedClip() };
    }

    /// Ekrandaki piksel boyutuna en yakın (eşit ya da büyük) PNG'yi çizer.
    pub fn image(&self, id: ImageId, r: Rect, opacity: f32) {
        let Some((rt, _)) = &self.rt else { return };
        let img = &self.images[id.0];
        let want = (r.w() * self.dpi / 96.0).ceil() as u32;
        let n = img.sizes.iter().position(|s| s.0 >= want).unwrap_or(img.sizes.len() - 1);
        let mut cache = img.cache.borrow_mut();
        if cache[n].is_none() {
            cache[n] = unsafe { rt.CreateBitmapFromWicBitmap(&img.sizes[n].1, None) }.ok();
        }
        if let Some(bmp) = &cache[n] {
            unsafe {
                rt.DrawBitmap(bmp, Some(&r.d2d()), opacity, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }
        }
    }

    pub fn measure(&self, s: &str, format: &IDWriteTextFormat) -> f32 {
        let s: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let Ok(layout) = self.dw.CreateTextLayout(&s, format, 4000.0, 100.0) else {
                return 0.0;
            };
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = layout.GetMetrics(&mut m);
            m.widthIncludingTrailingWhitespace
        }
    }
}
