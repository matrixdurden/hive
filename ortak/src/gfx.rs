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

/// Simge yazı tipi: Windows 11'de Segoe Fluent Icons, 10'da Segoe MDL2 Assets. İkisi aynı kod
/// noktalarını kullanır; Fluent olmayan sistemde onu istemek simgeleri kutucuk yapar.
pub fn icon_family(dw: &IDWriteFactory) -> PCWSTR {
    let fluent = unsafe {
        let mut fonts = None;
        let (mut index, mut found) = (0u32, BOOL(0));
        if dw.GetSystemFontCollection(&mut fonts, false).is_ok()
            && let Some(f) = &fonts
        {
            let _ = f.FindFamilyName(w!("Segoe Fluent Icons"), &mut index, &mut found);
        }
        found.as_bool()
    };
    if fluent { w!("Segoe Fluent Icons") } else { w!("Segoe MDL2 Assets") }
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
    /// Not gövdesi: 14 px, satır kaydırır (düzenle kullanılır).
    pub body: IDWriteTextFormat,
    /// Not içi başlıklar.
    pub h1: IDWriteTextFormat,
    pub h2: IDWriteTextFormat,
    /// Kod: eş aralıklı 13 px.
    pub mono: IDWriteTextFormat,
    /// Karşılama: 30 px, ince.
    pub display: IDWriteTextFormat,
    /// Okuma (not gövdesi, cevaplar): 15 px, geniş satır aralığı.
    pub read: IDWriteTextFormat,
    pub read_h1: IDWriteTextFormat,
    pub read_h2: IDWriteTextFormat,
    pub read_h3: IDWriteTextFormat,
    pub read_mono: IDWriteTextFormat,
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

            let icons = icon_family(&dw);

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
                body: format(ui, 14.0, normal, lead)?,
                h1: format(w!("Segoe UI Variable Display"), 24.0, semi, lead)?,
                h2: format(w!("Segoe UI Variable Display"), 17.0, semi, lead)?,
                mono: format(w!("Cascadia Mono"), 13.0, normal, lead)?,
                display: format(w!("Segoe UI Variable Display"), 30.0, DWRITE_FONT_WEIGHT_SEMI_LIGHT, lead)?,
                read: format(ui, 15.0, normal, lead)?,
                read_h1: format(w!("Segoe UI Variable Display"), 28.0, semi, lead)?,
                read_h2: format(w!("Segoe UI Variable Display"), 20.0, semi, lead)?,
                read_h3: format(ui, 16.0, semi, lead)?,
                read_mono: format(w!("Cascadia Mono"), 13.5, normal, lead)?,
            };
            for f in [&f.body, &f.h1, &f.h2, &f.mono, &f.read, &f.read_h1, &f.read_h2, &f.read_h3, &f.read_mono] {
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
                let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            }
            // Okuma yazılarında satırlar nefes alsın.
            for (fm, size) in [(&f.read, 15.0), (&f.read_h1, 28.0), (&f.read_h2, 20.0), (&f.read_h3, 16.0), (&f.read_mono, 13.5)] {
                let ratio = if size > 18.0 { 1.25 } else { 1.62 };
                let _ = fm.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, size * ratio, size * ratio * 0.8);
            }
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

    /// Radyal degradeli elips: `stops` (konum 0..1, renk), merkez `cx, cy`; `ox, oy` degradenin
    /// merkezden kayması (ışık yönü).
    pub fn radial(&self, cx: f32, cy: f32, rx: f32, ry: f32, ox: f32, oy: f32, stops: &[(f32, Color)]) {
        let Some((rt, _)) = &self.rt else { return };
        let stops: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|&(p, c)| D2D1_GRADIENT_STOP { position: p, color: c.d2d() }).collect();
        unsafe {
            let Ok(coll) = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
                center: windows_numerics::Vector2 { X: cx, Y: cy },
                gradientOriginOffset: windows_numerics::Vector2 { X: ox, Y: oy },
                radiusX: rx,
                radiusY: ry,
            };
            let Ok(brush) = rt.CreateRadialGradientBrush(&props, None, &coll) else { return };
            let e = D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: cx, Y: cy }, radiusX: rx, radiusY: ry };
            rt.FillEllipse(&e, &brush);
        }
    }

    /// Doğrusal degradeli dikdörtgen (yuvarlak köşeli): `a`'dan `b`'ye.
    pub fn linear(&self, r: Rect, radius: f32, a: (f32, f32), b: (f32, f32), stops: &[(f32, Color)]) {
        let Some((rt, _)) = &self.rt else { return };
        let stops: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|&(p, c)| D2D1_GRADIENT_STOP { position: p, color: c.d2d() }).collect();
        unsafe {
            let Ok(coll) = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                startPoint: windows_numerics::Vector2 { X: a.0, Y: a.1 },
                endPoint: windows_numerics::Vector2 { X: b.0, Y: b.1 },
            };
            let Ok(brush) = rt.CreateLinearGradientBrush(&props, None, &coll) else { return };
            rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius }, &brush);
        }
    }

    /// Çizimi daireyle kırpar (yumuşak kenarlı).
    pub fn clip_circle(&self, cx: f32, cy: f32, r: f32, f: impl FnOnce()) {
        let Some((rt, _)) = &self.rt else { return };
        unsafe {
            let e = D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r };
            let (Ok(geo), Ok(layer)) = (self.d2d.CreateEllipseGeometry(&e), rt.CreateLayer(None)) else {
                f();
                return;
            };
            let params = D2D1_LAYER_PARAMETERS {
                contentBounds: D2D_RECT_F { left: -1e6, top: -1e6, right: 1e6, bottom: 1e6 },
                geometricMask: std::mem::ManuallyDrop::new(Some(geo.cast().unwrap())),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: windows_numerics::Matrix3x2::identity(),
                opacity: 1.0,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS_NONE,
            };
            rt.PushLayer(&params, &layer);
            f();
            rt.PopLayer();
            let _ = std::mem::ManuallyDrop::into_inner(params.geometricMask);
        }
    }

    pub fn circle(&self, x: f32, y: f32, radius: f32, c: Color) {
        self.fill(Rect::new(x - radius, y - radius, x + radius, y + radius), radius, c);
    }

    pub fn ring(&self, x: f32, y: f32, radius: f32, c: Color, width: f32) {
        self.stroke(Rect::new(x - radius, y - radius, x + radius, y + radius), radius, c, width);
    }

    pub fn line(&self, x1: f32, y1: f32, x2: f32, y2: f32, c: Color, width: f32) {
        self.with(c, |rt, b| unsafe {
            rt.DrawLine(
                windows_numerics::Vector2 { X: x1, Y: y1 },
                windows_numerics::Vector2 { X: x2, Y: y2 },
                b,
                width,
                None,
            )
        });
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

    /// Şu anki kaydırma (`translate`'in verdiği): iç içe sayfalar kendi köşelerini buna göre kurar.
    pub fn offset(&self) -> (f32, f32) {
        let Some((rt, _)) = &self.rt else { return (0.0, 0.0) };
        let mut m = windows_numerics::Matrix3x2::default();
        unsafe { rt.GetTransform(&mut m) };
        (m.M31, m.M32)
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

    /// Satır kaydıran düzen: not gövdesi, sohbet balonu. Çizim hedefinden bağımsız, saklanabilir.
    pub fn layout(&self, s: &str, format: &IDWriteTextFormat, width: f32) -> Option<IDWriteTextLayout> {
        let s: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let l = self.dw.CreateTextLayout(&s, format, width.max(1.0), 100_000.0).ok()?;
            let _ = l.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            let _ = l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
            let _ = l.SetTrimming(&DWRITE_TRIMMING::default(), None);
            Some(l)
        }
    }

    /// Düzenin yüksekliği.
    pub fn layout_height(l: &IDWriteTextLayout) -> f32 {
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe {
            let _ = l.GetMetrics(&mut m);
        }
        m.height
    }

    /// Düzende UTF-16 aralığına yazı tipi ağırlığı / eğiklik / altı çizili / boy uygular.
    pub fn style_range(l: &IDWriteTextLayout, start: u32, len: u32, bold: bool, italic: bool, underline: bool, mono: Option<&IDWriteTextFormat>) {
        let r = DWRITE_TEXT_RANGE { startPosition: start, length: len };
        unsafe {
            if bold {
                let _ = l.SetFontWeight(DWRITE_FONT_WEIGHT_SEMI_BOLD, r);
            }
            if italic {
                let _ = l.SetFontStyle(DWRITE_FONT_STYLE_ITALIC, r);
            }
            if underline {
                let _ = l.SetUnderline(true, r);
            }
            if mono.is_some() {
                let _ = l.SetFontFamilyName(w!("Cascadia Mono"), r);
            }
        }
    }

    /// Düzeni çizer; `colors`: (başlangıç, uzunluk, renk) ile renklenen aralıklar (bağlantılar).
    pub fn draw_layout(&self, l: &IDWriteTextLayout, x: f32, y: f32, c: Color, colors: &[(u32, u32, Color)]) {
        let Some((rt, brush)) = &self.rt else { return };
        unsafe {
            for &(start, len, col) in colors {
                if let Ok(b) = rt.CreateSolidColorBrush(&col.d2d(), None) {
                    let _ = l.SetDrawingEffect(&b, DWRITE_TEXT_RANGE { startPosition: start, length: len });
                }
            }
            brush.SetColor(&c.d2d());
            rt.DrawTextLayout(windows_numerics::Vector2 { X: x, Y: y }, l, brush, D2D1_DRAW_TEXT_OPTIONS_CLIP);
        }
    }

    /// Düzende noktanın denk geldiği UTF-16 konumu (metnin üstündeyse).
    pub fn hit_layout(l: &IDWriteTextLayout, x: f32, y: f32) -> Option<u32> {
        let (mut trailing, mut inside) = (BOOL(0), BOOL(0));
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            l.HitTestPoint(x, y, &mut trailing, &mut inside, &mut m).ok()?;
        }
        inside.as_bool().then_some(m.textPosition)
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
