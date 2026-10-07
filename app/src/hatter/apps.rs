//! Pencereler ve uygulamalar: görev çubuğunda görünecek pencereleri bulur, uygulamaya göre
//! gruplar, dock'a sabitlenen öğeleri (kısayol, exe, klasör, Store uygulaması) çözer, ikonlarını
//! büyük boyda alır, açar ve öne getirir.
//!
//! Bir pencerenin uygulaması önce AppUserModelID'sidir (pencerenin kendi kimliği, yoksa paketli
//! sürecin kimliği), yoksa exe'sinin adı. Sabitlenmiş kısayolun hedefi başka bir exe olabilir
//! (Discord'un kısayolu Update.exe'yi açar), o yüzden kısayolun kimliği ve `--processStart`
//! argümanı da okunur.

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{DeleteObject, HPALETTE};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IPersistFile, STGM_READ};
use windows::Win32::System::SystemServices::{SFGAO_FOLDER, SFGAO_STREAM};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{GUID, Interface, PCWSTR, PWSTR, w};

use crate::util::wide;

/// PKEY_AppUserModel_ID
const PKEY_AUMID: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3), pid: 5 };

/// Başka uygulamaların penceresini taşıyan süreçler: bunların her penceresi ayrı bir öğedir,
/// ikonu ve adı pencerenin kendisinden gelir (Minecraft'ın penceresi javaw.exe'nindir).
const HOSTS: [&str; 5] = ["javaw.exe", "java.exe", "pythonw.exe", "python.exe", "applicationframehost.exe"];

/// Görev çubuğunun da göstermediği kabuk pencereleri.
const SHELL_CLASSES: [&str; 5] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd", "hive-hatter"];

pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn is_shell_window(hwnd: HWND) -> bool {
    SHELL_CLASSES.contains(&class_name(hwnd).as_str())
}

/// Pencerenin ekranda görünen sınırı (gölgesiz).
pub fn frame(hwnd: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as _, size_of::<RECT>() as u32)
            .is_err()
        {
            let _ = GetWindowRect(hwnd, &mut r);
        }
    }
    r
}

/// Alt+Tab'da ve görev çubuğunda görünen türden bir pencere mi.
fn listed(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || GetWindowTextLengthW(hwnd) == 0 {
            return false;
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let app = ex & WS_EX_APPWINDOW.0 != 0;
        if !app {
            if ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
                return false;
            }
            if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
                return false;
            }
        }
        // Başka sanal masaüstündeki ya da askıdaki Store uygulamalarının pencereleri gizlidir.
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as _, 4).is_ok() && cloaked != 0 {
            return false;
        }
    }
    !is_shell_window(hwnd)
}

pub struct Win {
    pub hwnd: HWND,
    /// Süreç exe'sinin tam yolu.
    pub exe: String,
    pub aumid: Option<String>,
    /// HOSTS'tan biri: pencere kendi başına bir uygulama sayılır.
    pub host: bool,
}

impl Win {
    pub fn exe_name(&self) -> String {
        file_name(&self.exe)
    }

    /// Kimliği olmayan bir taşıyıcı sürecin penceresi: kendi başına bir öğe.
    pub fn standalone(&self) -> bool {
        self.host && self.aumid.is_none()
    }

    /// Gruplama anahtarı.
    pub fn key(&self) -> String {
        match &self.aumid {
            Some(a) => a.to_lowercase(),
            None if self.host => format!("pencere:{}", self.hwnd.0 as usize),
            None => self.exe_name(),
        }
    }
}

pub fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_lowercase()
}

/// Listelenen pencereler, üstteki önce (z sırası).
pub fn windows() -> Vec<Win> {
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> windows::core::BOOL {
        let list = unsafe { &mut *(lp.0 as *mut Vec<HWND>) };
        if listed(hwnd) {
            list.push(hwnd);
        }
        true.into()
    }
    let mut hwnds: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut hwnds as *mut _ as isize));
    }
    hwnds.into_iter().filter_map(win).collect()
}

fn win(hwnd: HWND) -> Option<Win> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let (exe, process_aumid) = process_info(pid)?;
    let host = HOSTS.contains(&file_name(&exe).as_str());
    let aumid = window_aumid(hwnd).or(process_aumid);
    Some(Win { hwnd, exe, aumid, host })
}

/// Sürecin exe yolu ve (paketliyse) uygulama kimliği.
fn process_info(pid: u32) -> Option<(String, Option<String>)> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let exe = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
            .ok()
            .map(|_| String::from_utf16_lossy(&buf[..len as usize]));
        let mut len = buf.len() as u32;
        let aumid = (GetApplicationUserModelId(h, &mut len, Some(PWSTR(buf.as_mut_ptr()))) == ERROR_SUCCESS)
            .then(|| String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)]));
        let _ = CloseHandle(h);
        Some((exe?, aumid))
    }
}

fn store_string(store: &IPropertyStore, key: &PROPERTYKEY) -> Option<String> {
    unsafe {
        let v = store.GetValue(key).ok()?;
        let p = PropVariantToStringAlloc(&v).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.filter(|s| !s.is_empty())
    }
}

fn window_aumid(hwnd: HWND) -> Option<String> {
    let store: IPropertyStore = unsafe { SHGetPropertyStoreForWindow(hwnd) }.ok()?;
    store_string(&store, &PKEY_AUMID)
}

// --- Sabitlenmiş öğeler ---

/// dock.txt'deki bir satır: dosya yolu (.lnk, .exe, klasör) ya da kabuk adı
/// (`shell:AppsFolder\<kimlik>`, `shell:RecycleBinFolder`).
#[derive(Clone)]
pub struct Pin {
    pub target: String,
    pub name: String,
    pub folder: bool,
    pub aumid: Option<String>,
    /// Açtığı exe'nin adı (küçük harf).
    pub exe: Option<String>,
    /// İkonun alınacağı kabuk adı.
    pub icon: String,
}

impl Pin {
    pub fn resolve(target: &str) -> Option<Pin> {
        let item = shell_item(target)?;
        let name = display_name(&item)?;
        let attrs = unsafe { item.GetAttributes(SFGAO_FOLDER | SFGAO_STREAM) }.unwrap_or_default();
        let folder = attrs & SFGAO_FOLDER == SFGAO_FOLDER && attrs & SFGAO_STREAM != SFGAO_STREAM;
        let mut pin = Pin { target: target.to_string(), name, folder, aumid: None, exe: None, icon: target.to_string() };
        let lower = target.to_lowercase();
        if let Some(id) = target.strip_prefix(r"shell:AppsFolder\") {
            pin.aumid = Some(id.to_string());
        } else if lower.ends_with(".lnk") {
            if let Some((exe, args, aumid, custom_icon)) = read_link(target) {
                // Squirrel kurulumları (Discord, ...) Update.exe'yi asıl exe'nin adıyla çağırır.
                let started = args
                    .split("--processStart")
                    .nth(1)
                    .and_then(|s| s.split_whitespace().next())
                    .map(|s| s.trim_matches('"').to_lowercase());
                pin.exe = started.or_else(|| (!exe.is_empty()).then(|| file_name(&exe)));
                // Başlat menüsündeki kaydının ikonu daha temiz: kısayolun ikonu küçükse kabuk onu
                // çerçeveli bir küçük resim olarak verir (PWA'lar).
                // Kısayola elle ikon konduysa o kullanılır. Yoksa tarayıcı uygulamasının (PWA)
                // tarayıcıda saklanan en büyük ikonu, o da yoksa Başlat menüsündeki kaydının ikonu.
                if !custom_icon {
                    if let Some(png) = pwa_icon(&exe, &args) {
                        pin.icon = png;
                    } else if let Some(a) = &aumid {
                        let apps = format!(r"shell:AppsFolder\{a}");
                        if shell_item(&apps).is_some() {
                            pin.icon = apps;
                        }
                    }
                }
                pin.aumid = aumid;
            }
        } else if lower.ends_with(".exe") {
            pin.exe = Some(file_name(target));
            if let Some(d) = file_description(target) {
                pin.name = d;
            }
        }
        Some(pin)
    }

    /// Pencere bu öğenin uygulamasına mı ait.
    pub fn owns(&self, w: &Win) -> bool {
        if self.folder {
            return false;
        }
        // Taşıyıcı süreçteki pencere (Minecraft javaw.exe'de): sabitlenen öğenin adı pencere
        // başlığında geçiyorsa onundur. Prism'in "26.1.2" kısayolu "Minecraft 26.1.2 - ..."
        // penceresini sahiplenir.
        if w.standalone() {
            let name = self.name.trim().to_lowercase();
            return name.chars().count() >= 3 && title(w.hwnd).to_lowercase().contains(&name);
        }
        match (&self.aumid, &w.aumid) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => self.exe.as_deref().is_some_and(|e| e == w.exe_name()),
        }
    }
}

/// Chromium tabanlı tarayıcının (Brave, Chrome, Edge) uygulaması için sakladığı en büyük ikon:
/// `<User Data>\<profil>\Web Applications\Manifest Resources\<app-id>\Icons\<boy>.png`.
fn pwa_icon(exe: &str, args: &str) -> Option<String> {
    let arg = |name: &str| {
        args.split_whitespace().find_map(|a| a.strip_prefix(name)).map(|v| v.trim_matches('"').to_string())
    };
    let app = arg("--app-id=")?;
    let profile = arg("--profile-directory=").unwrap_or_else(|| "Default".into());
    let lower = exe.to_lowercase();
    let vendor = [
        (r"\bravesoftware\brave-browser\", r"BraveSoftware\Brave-Browser"),
        (r"\google\chrome\", r"Google\Chrome"),
        (r"\microsoft\edge\", r"Microsoft\Edge"),
    ]
    .into_iter()
    .find(|(k, _)| lower.contains(k))?
    .1;
    let base = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    let dir = base.join(vendor).join("User Data").join(profile).join(r"Web Applications\Manifest Resources").join(app).join("Icons");
    let best = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.strip_suffix(".png")?.parse::<u32>().ok().map(|size| (size, e.path()))
        })
        .max_by_key(|(size, _)| *size)?;
    Some(best.1.display().to_string())
}

/// .lnk: hedef exe, argümanlar, uygulama kimliği, elle ikon konmuş mu.
fn read_link(path: &str) -> Option<(String, String, Option<String>, bool)> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let p = wide(path);
        link.cast::<IPersistFile>().ok()?.Load(PCWSTR(p.as_ptr()), STGM_READ).ok()?;
        let mut buf = [0u16; 1024];
        let _ = link.GetPath(&mut buf, std::ptr::null_mut(), 0);
        let exe = String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)]);
        let mut abuf = [0u16; 2048];
        let _ = link.GetArguments(&mut abuf);
        let args = String::from_utf16_lossy(&abuf[..abuf.iter().position(|&c| c == 0).unwrap_or(0)]);
        let aumid = link.cast::<IPropertyStore>().ok().and_then(|s| store_string(&s, &PKEY_AUMID));
        let mut ibuf = [0u16; 1024];
        let mut index = 0i32;
        let _ = link.GetIconLocation(&mut ibuf, &mut index);
        let icon = utf16z_local(&ibuf);
        let custom = !icon.is_empty() && !icon.eq_ignore_ascii_case(&exe) && std::path::Path::new(&icon).exists();
        Some((exe, args, aumid, custom))
    }
}

fn utf16z_local(buf: &[u16]) -> String {
    String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())])
}

fn shell_item(target: &str) -> Option<IShellItem> {
    let t = wide(target);
    unsafe { SHCreateItemFromParsingName(PCWSTR(t.as_ptr()), None) }.ok()
}

fn display_name(item: &IShellItem) -> Option<String> {
    unsafe {
        let p = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// Exe'nin "Dosya açıklaması" (Brave Browser, Visual Studio Code).
fn file_description(path: &str) -> Option<String> {
    let p = wide(path);
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(p.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(p.as_ptr()), None, size, data.as_mut_ptr().cast()).ok()?;
        let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
        if !VerQueryValueW(data.as_ptr().cast(), w!(r"\VarFileInfo\Translation"), &mut ptr, &mut len).as_bool()
            || len < 4
        {
            return None;
        }
        let pair = std::slice::from_raw_parts(ptr as *const u16, 2);
        let key = wide(&format!(r"\StringFileInfo\{:04x}{:04x}\FileDescription", pair[0], pair[1]));
        if !VerQueryValueW(data.as_ptr().cast(), PCWSTR(key.as_ptr()), &mut ptr, &mut len).as_bool() || len < 2 {
            return None;
        }
        let s = std::slice::from_raw_parts(ptr as *const u16, len as usize - 1);
        Some(String::from_utf16_lossy(s).trim().to_string()).filter(|s| !s.is_empty())
    }
}

/// Çalışan ama sabitlenmemiş bir uygulama: adı, ikonun kaynağı, sabitlenirse yazılacak satır.
pub struct Running {
    pub name: String,
    pub icon: IconSource,
    pub pin: Option<String>,
}

#[derive(Clone, PartialEq)]
pub enum IconSource {
    Shell(String),
    Window(isize),
}

impl IconSource {
    pub fn id(&self) -> String {
        match self {
            IconSource::Shell(s) => s.to_lowercase(),
            IconSource::Window(h) => format!("pencere:{h}"),
        }
    }
}

pub fn running(w: &Win) -> Running {
    if w.standalone() {
        let mut name = title(w.hwnd);
        if name.chars().count() > 40 {
            name = name.chars().take(39).collect::<String>() + "…";
        }
        return Running { name, icon: IconSource::Window(w.hwnd.0 as isize), pin: None };
    }
    // Başlat menüsünde kimliğiyle kayıtlı uygulamalar (Store, Discord, Brave, ...).
    if let Some(a) = &w.aumid {
        let target = format!(r"shell:AppsFolder\{a}");
        if let Some(name) = shell_item(&target).and_then(|i| display_name(&i)) {
            return Running { name, icon: IconSource::Shell(target.clone()), pin: Some(target) };
        }
    }
    let name = file_description(&w.exe).unwrap_or_else(|| {
        let n = file_name(&w.exe);
        n.strip_suffix(".exe").unwrap_or(&n).to_string()
    });
    Running { name, icon: IconSource::Shell(w.exe.clone()), pin: Some(w.exe.clone()) }
}

// --- İkonlar ---

/// İkonun `px` piksellik (ya da daha büyük) hali, 32bpp önçarpımlı.
pub fn icon(wic: &IWICImagingFactory, src: &IconSource, px: i32) -> Option<IWICBitmapSource> {
    let target_px = px;
    let bmp = match src {
        // Resim dosyası (tarayıcı uygulamasının ikonu): olduğu gibi okunur.
        IconSource::Shell(target) if target.to_lowercase().ends_with(".png") => unsafe {
            let t = wide(target);
            let d = wic
                .CreateDecoderFromFilename(PCWSTR(t.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnLoad)
                .ok()?;
            d.GetFrame(0).ok()?.cast::<IWICBitmapSource>().ok()?
        },
        IconSource::Shell(target) => unsafe {
            let t = wide(target);
            let f: IShellItemImageFactory = SHCreateItemFromParsingName(PCWSTR(t.as_ptr()), None).ok()?;
            let h = f.GetImage(SIZE { cx: px, cy: px }, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK).ok()?;
            let b = wic.CreateBitmapFromHBITMAP(h, HPALETTE::default(), WICBitmapUseAlpha);
            let _ = DeleteObject(h.into());
            b.ok()?.cast::<IWICBitmapSource>().ok()?
        },
        IconSource::Window(h) => unsafe {
            let hwnd = HWND(*h as *mut _);
            let mut r = 0usize;
            let _ = SendMessageTimeoutW(
                hwnd,
                WM_GETICON,
                WPARAM(ICON_BIG as usize),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                100,
                Some(&mut r),
            );
            if r == 0 {
                r = GetClassLongPtrW(hwnd, GCLP_HICON);
            }
            if r == 0 {
                return None;
            }
            wic.CreateBitmapFromHICON(HICON(r as *mut _)).ok()?.cast::<IWICBitmapSource>().ok()?
        },
    };
    unsafe {
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&bmp, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
            .ok()?;
        // Bazı ikonların (Brave'in PWA ikonları) en dış iki pikselinde yarı saydam gri bir halka
        // var; koyu zeminde kare gibi görünür. Kenardaki soluk pikseller ve neredeyse saydam
        // pikseller temizlenir; kenara kadar dolu ikonlar (tam opak) olduğu gibi kalır.
        let (mut w, mut h) = (0u32, 0u32);
        conv.GetSize(&mut w, &mut h).ok()?;
        let mut px = vec![0u8; (w * h * 4) as usize];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut px).ok()?;
        for (i, p) in px.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i as u32 % w, i as u32 / w);
            let edge = x < 2 || y < 2 || x + 2 >= w || y + 2 >= h;
            if p[3] < 12 || (edge && p[3] < 64) {
                p.fill(0);
            }
        }
        let (pixels, side) = normalize(&px, w as usize, h as usize)?;
        let side = side as u32;
        let norm = wic.CreateBitmapFromMemory(side, side, &GUID_WICPixelFormat32bppPBGRA, side * 4, &pixels).ok()?;
        // Çizileceği en büyük boya yüksek kaliteli kübik ölçekleme: D2D'nin doğrusal ölçeklemesi
        // küçük kaynağı bulanık, büyüğü tırtıklı yapar.
        let target = target_px.max(16) as u32;
        if side == target {
            return norm.cast().ok();
        }
        let scaler = wic.CreateBitmapScaler().ok()?;
        scaler.Initialize(&norm, target, target, WICBitmapInterpolationModeHighQualityCubic).ok()?;
        scaler.cast().ok()
    }
}

/// İkonları aynı görsel boya getirir: boş kenarlar kırpılır, içerik kare bir tuvalin ortasına
/// sabit oranda yerleşir. Kenara kadar dolu kare karolar (Claude, Store uygulamaları) biraz daha
/// küçük tutulur: aynı genişlikte bir kare, bir daireden iri görünür.
fn normalize(px: &[u8], w: usize, h: usize) -> Option<(Vec<u8>, usize)> {
    let (mut l, mut t, mut r, mut b, mut solid) = (w, h, 0, 0, 0usize);
    for y in 0..h {
        for x in 0..w {
            if px[(y * w + x) * 4 + 3] >= 32 {
                l = l.min(x);
                r = r.max(x + 1);
                t = t.min(y);
                b = b.max(y + 1);
                solid += 1;
            }
        }
    }
    if r <= l || b <= t {
        return None;
    }
    let (bw, bh) = (r - l, b - t);
    let coverage = solid as f32 / (bw * bh) as f32;
    let fill = if coverage > 0.9 && bw.abs_diff(bh) * 10 < bw { 0.80 } else { 0.92 };
    let content = bw.max(bh);
    let side = ((content as f32 / fill).round() as usize).max(content);
    let (ox, oy) = ((side - bw) / 2, (side - bh) / 2);
    let mut out = vec![0u8; side * side * 4];
    for y in 0..bh {
        let src = ((t + y) * w + l) * 4;
        let dst = ((oy + y) * side + ox) * 4;
        out[dst..dst + bw * 4].copy_from_slice(&px[src..src + bw * 4]);
    }
    Some((out, side))
}

// --- Açmak, öne getirmek ---

/// Kabuğun açtığı her şeyi açar: kısayol, exe, klasör, `shell:` adı.
pub fn open(target: &str) {
    let Some(item) = shell_item(target) else {
        crate::log!("hatter: açılamadı (bulunamadı): {target}");
        return;
    };
    unsafe {
        let Ok(pidl) = SHGetIDListFromObject(&item) else { return };
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_IDLIST | SEE_MASK_FLAG_NO_UI | SEE_MASK_ASYNCOK,
            lpIDList: pidl as *mut _,
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        if let Err(e) = ShellExecuteExW(&mut info) {
            crate::log!("hatter: açılamadı: {target}: {e}");
        }
        CoTaskMemFree(Some(pidl as *const _));
    }
}

pub fn activate(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

pub fn minimize(hwnd: HWND) {
    unsafe {
        let _ = ShowWindowAsync(hwnd, SW_MINIMIZE);
    }
}

pub fn close(hwnd: HWND) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(SC_CLOSE as usize), LPARAM(0));
    }
}

/// `hive --hatter-test`: dock'un gördüğü pencereler ve sabitlenmiş öğeler.
pub fn probe(pins: &[String]) -> String {
    let mut s = String::from("pencereler:\n");
    for w in windows() {
        let r = running(&w);
        s += &format!(
            "  {:>8x}  {:<28} anahtar={}  aumid={}  exe={}\n",
            w.hwnd.0 as usize,
            r.name,
            w.key(),
            w.aumid.as_deref().unwrap_or("-"),
            w.exe
        );
    }
    s += "sabitlenenler:\n";
    for p in pins {
        match Pin::resolve(p) {
            Some(p) => {
                s += &format!(
                    "  {:<28} klasör={} aumid={} exe={}  ({})\n",
                    p.name,
                    p.folder,
                    p.aumid.as_deref().unwrap_or("-"),
                    p.exe.as_deref().unwrap_or("-"),
                    p.target
                )
            }
            None => s += &format!("  ÇÖZÜLEMEDİ: {p}\n"),
        }
    }
    s
}
