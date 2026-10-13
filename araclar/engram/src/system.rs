//! Bilgisayarın kendisi: ses (ana ses, uygulama başına ses, çıkış cihazı), pencere yerleşimi,
//! Başlat menüsündeki uygulamalar, kullanıcı klasörlerinde dosya arama, pano, tarayıcıdaki sekme
//! ve sistem durumu (pil, işlemci, bellek, disk), kilit/uyku/ekran.
//!
//! Hepsi ekran görüntüsüne bakmadan, doğrudan Win32/COM ile ve hızlı çalışır; sonuç model için
//! kısa bir Türkçe metindir. İş parçacığından çağrılır: COM her çağrıda açılıp kapatılır.
#![allow(dead_code)]

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{CloseHandle, FILETIME, HWND, LPARAM, PROPERTYKEY, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize, STGM_READ,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, GUID, HRESULT, IUnknown, Interface, PCWSTR, PWSTR};

use crate::log;
use crate::util::{Res, wide};

// ---------------------------------------------------------------------------------------------
// Ortak yardımcılar

/// COM'u bu iş parçacığında açar; başarılıysa kapanışta dengeler. İş parçacığı başka kipte
/// açılmışsa (RPC_E_CHANGED_MODE) o kip kullanılır.
struct Com(bool);

impl Com {
    fn init(mode: COINIT) -> Self {
        Self(unsafe { CoInitializeEx(None, mode) }.is_ok())
    }

    fn mta() -> Self {
        Self::init(COINIT_MULTITHREADED)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

/// İş parçacığını geçici olarak monitör başına DPI farkında v2'ye alır (fiziksel pikseller).
struct Dpi(DPI_AWARENESS_CONTEXT);

impl Dpi {
    fn set() -> Self {
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}

impl Drop for Dpi {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            unsafe {
                SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
}

/// Türkçe harfleri de doğru katlayan küçük harf + aksan düşürme (İ/I/ı/i → i, ş → s ...):
/// "Discord", "DİSCORD", "dıscord" aynı olur.
fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'İ' | 'I' | 'ı' | 'i' | 'î' | 'Î' => out.push('i'),
            'Ş' | 'ş' => out.push('s'),
            'Ğ' | 'ğ' => out.push('g'),
            'Ü' | 'ü' | 'û' | 'Û' => out.push('u'),
            'Ö' | 'ö' => out.push('o'),
            'Ç' | 'ç' => out.push('c'),
            'â' | 'Â' | 'á' | 'à' | 'ä' => out.push('a'),
            'é' | 'è' | 'ê' | 'ë' => out.push('e'),
            _ => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// PWSTR'yi okuyup CoTaskMemFree ile bırakır.
unsafe fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}

/// Sürecin exe yolu.
fn process_path(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    unsafe {
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let r = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n);
        let _ = CloseHandle(p);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..n as usize]))
    }
}

/// Sürecin exe adı uzantısız ("brave", "Discord").
fn process_stem(pid: u32) -> Option<String> {
    let path = process_path(pid)?;
    let file = path.rsplit('\\').next().unwrap_or(&path);
    Some(file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file).to_string())
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn ft_u64(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

/// Yerel saat ile UTC arasındaki fark (saniye, şu anki yaz/kış saatine göre).
fn local_offset() -> i64 {
    use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    use windows::Win32::System::Time::SystemTimeToFileTime;
    unsafe {
        let (l, u) = (GetLocalTime(), GetSystemTime());
        let (mut a, mut b) = (FILETIME::default(), FILETIME::default());
        if SystemTimeToFileTime(&l, &mut a).is_err() || SystemTimeToFileTime(&u, &mut b).is_err() {
            return 0;
        }
        let d = (ft_u64(a) as i64 - ft_u64(b) as i64) / 10_000_000;
        // Saniye kaymalarını dakikaya yuvarla.
        (d as f64 / 60.0).round() as i64 * 60
    }
}

/// "bugün 14:02", "dün 09:15", "3 gün önce" ya da "12 Eki".
fn when(secs: u64) -> String {
    let off = local_offset();
    let day = |s: u64| (s as i64 + off).div_euclid(86_400);
    let tod = (secs as i64 + off).rem_euclid(86_400);
    let hm = format!("{:02}:{:02}", tod / 3600, tod % 3600 / 60);
    match day(now_secs()) - day(secs) {
        ..=0 => format!("bugün {hm}"),
        1 => format!("dün {hm}"),
        n @ 2..=30 => format!("{n} gün önce"),
        _ => crate::util::short_date(secs),
    }
}

/// "532 B", "14 KB", "3,2 MB", "1,1 GB".
fn size_text(b: u64) -> String {
    let f = b as f64;
    let s = if b < 1024 {
        format!("{b} B")
    } else if f < 1024.0 * 1024.0 {
        format!("{:.0} KB", f / 1024.0)
    } else if f < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", f / 1024.0 / 1024.0)
    } else {
        format!("{:.1} GB", f / 1024.0 / 1024.0 / 1024.0)
    };
    s.replace('.', ",")
}

// ---------------------------------------------------------------------------------------------
// Ses

fn enumerator() -> Res<IMMDeviceEnumerator> {
    Ok(unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? })
}

fn device_id(d: &IMMDevice) -> String {
    unsafe { d.GetId().map(|p| take_pwstr(p)).unwrap_or_default() }
}

/// Cihazın görünen adı (PKEY_Device_FriendlyName).
fn device_name(d: &IMMDevice) -> String {
    let key = PROPERTYKEY { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };
    unsafe { d.OpenPropertyStore(STGM_READ).and_then(|s| s.GetValue(&key)).map(|v| v.to_string()).unwrap_or_default() }
}

/// Etkin çıkış cihazları: (cihaz, kimlik, ad).
fn render_devices(en: &IMMDeviceEnumerator) -> Vec<(IMMDevice, String, String)> {
    let mut out = Vec::new();
    unsafe {
        let Ok(list) = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else { return out };
        for i in 0..list.GetCount().unwrap_or(0) {
            let Ok(d) = list.Item(i) else { continue };
            let (id, name) = (device_id(&d), device_name(&d));
            out.push((d, id, name));
        }
    }
    out
}

fn default_render_id(en: &IMMDeviceEnumerator, role: ERole) -> String {
    unsafe { en.GetDefaultAudioEndpoint(eRender, role).map(|d| device_id(&d)).unwrap_or_default() }
}

/// Yeni düzey (yüzde): set için `value`, up/down için `value` kadar adım (verilmezse 10).
fn next_level(action: &str, cur: f64, value: f64) -> Res<f64> {
    let step = if value.is_finite() && value > 0.0 { value } else { 10.0 };
    let v = match action {
        "set" => {
            if !value.is_finite() {
                return Err("ses düzeyi verilmedi (0-100)".into());
            }
            value
        }
        "up" => cur + step,
        "down" => cur - step,
        _ => cur,
    };
    Ok(v.clamp(0.0, 100.0).round())
}

fn pct(v: f64) -> String {
    format!("%{v:.0}")
}

/// Ses düzeyi. `action`: get, set, up, down, mute, unmute; `value` yüzde (up/down için adım,
/// varsayılan 10). `target` boşsa varsayılan çıkışın ana sesi, doluysa o uygulamanın sesi
/// ("discord", "spotify", "brave"; süreç ya da oturum adında geçen, büyük/küçük harf duyarsız;
/// "*" ile get bütün oturumları listeler). Kısılmış ses set/up ile açılır.
pub fn volume(action: &str, value: f64, target: &str) -> Res<String> {
    let action = action.trim().to_lowercase();
    if !matches!(action.as_str(), "get" | "set" | "up" | "down" | "mute" | "unmute") {
        return Err(format!("bilinmeyen ses eylemi: {action} (get, set, up, down, mute, unmute)").into());
    }
    let _com = Com::mta();
    let en = enumerator()?;
    let target = target.trim();
    if target.is_empty() { master_volume(&en, &action, value) } else { app_volume(&en, &action, value, target) }
}

fn master_volume(en: &IMMDeviceEnumerator, action: &str, value: f64) -> Res<String> {
    unsafe {
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|_| "varsayılan ses çıkışı yok")?;
        let name = device_name(&dev);
        let ep: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None)?;
        let cur = (ep.GetMasterVolumeLevelScalar()? as f64 * 100.0).round();
        let muted = ep.GetMute()?.as_bool();
        let state = |v: f64, m: bool| format!("{}{}", pct(v), if m { " (sessiz)" } else { "" });
        match action {
            "get" => Ok(format!("Ana ses: {} — {name}", state(cur, muted))),
            "mute" | "unmute" => {
                let m = action == "mute";
                ep.SetMute(m, std::ptr::null())?;
                Ok(format!("Ana ses {}: {} — {name}", if m { "kısıldı" } else { "açıldı" }, state(cur, m)))
            }
            _ => {
                let new = next_level(action, cur, value)?;
                if new != cur {
                    ep.SetMasterVolumeLevelScalar((new / 100.0) as f32, std::ptr::null())?;
                }
                let unmute = muted && new > 0.0 && action != "down";
                if unmute {
                    ep.SetMute(false, std::ptr::null())?;
                }
                Ok(format!(
                    "Ana ses: {} → {}{} — {name}",
                    pct(cur),
                    pct(new),
                    if unmute { " (sessizden çıkarıldı)" } else { "" }
                ))
            }
        }
    }
}

/// Bir uygulamanın ses oturumu.
struct Session {
    vol: ISimpleAudioVolume,
    /// Süreç adı ("brave") ya da "Sistem sesleri".
    name: String,
    /// Oturumun kendi görünen adı (çoğu zaman boş).
    display: String,
    device: String,
}

/// Bütün etkin çıkış cihazlarındaki canlı ses oturumları.
fn sessions(en: &IMMDeviceEnumerator) -> Vec<Session> {
    let mut out = Vec::new();
    for (dev, _, dev_name) in render_devices(en) {
        unsafe {
            let Ok(mgr) = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
            let Ok(list) = mgr.GetSessionEnumerator() else { continue };
            for i in 0..list.GetCount().unwrap_or(0) {
                let Ok(ctl) = list.GetSession(i) else { continue };
                if ctl.GetState().is_ok_and(|s| s == AudioSessionStateExpired) {
                    continue;
                }
                let Ok(ctl2) = ctl.cast::<IAudioSessionControl2>() else { continue };
                let Ok(vol) = ctl.cast::<ISimpleAudioVolume>() else { continue };
                let display = ctl.GetDisplayName().map(|p| take_pwstr(p)).unwrap_or_default();
                // "@%SystemRoot%\...,-123" biçimindeki kaynak adları okunmaz.
                let display = if display.starts_with('@') { String::new() } else { display };
                let name = if ctl2.IsSystemSoundsSession() == windows::Win32::Foundation::S_OK {
                    "Sistem sesleri".to_string()
                } else {
                    let pid = ctl2.GetProcessId().unwrap_or(0);
                    process_stem(pid)
                        .unwrap_or_else(|| if display.is_empty() { format!("süreç {pid}") } else { display.clone() })
                };
                out.push(Session { vol, name, display, device: dev_name.clone() });
            }
        }
    }
    out
}

fn app_volume(en: &IMMDeviceEnumerator, action: &str, value: f64, target: &str) -> Res<String> {
    let all = sessions(en);
    let t = fold(target);
    let every = t == "*" && action == "get";
    let hits: Vec<&Session> =
        all.iter().filter(|s| every || fold(&s.name).contains(&t) || fold(&s.display).contains(&t)).collect();
    if hits.is_empty() {
        let mut names: Vec<String> = all.iter().map(|s| s.name.clone()).collect();
        names.sort();
        names.dedup();
        return Err(format!(
            "\"{target}\" için ses oturumu yok (uygulama ses çalmıyor ya da kapalı olabilir). Açık oturumlar: {}",
            if names.is_empty() { "yok".to_string() } else { names.join(", ") }
        )
        .into());
    }
    let devices: HashSet<&str> = hits.iter().map(|s| s.device.as_str()).collect();
    let mut lines = Vec::new();
    for s in hits {
        unsafe {
            let cur = (s.vol.GetMasterVolume()? as f64 * 100.0).round();
            let muted = s.vol.GetMute()?.as_bool();
            let line = match action {
                "get" => format!("{}: {}{}", s.name, pct(cur), if muted { " (sessiz)" } else { "" }),
                "mute" | "unmute" => {
                    let m = action == "mute";
                    s.vol.SetMute(m, std::ptr::null())?;
                    format!("{}: {} ({})", s.name, pct(cur), if m { "sessize alındı" } else { "sesi açıldı" })
                }
                _ => {
                    let new = next_level(action, cur, value)?;
                    if new != cur {
                        s.vol.SetMasterVolume((new / 100.0) as f32, std::ptr::null())?;
                    }
                    let unmute = muted && new > 0.0 && action != "down";
                    if unmute {
                        s.vol.SetMute(false, std::ptr::null())?;
                    }
                    format!(
                        "{}: {} → {}{}",
                        s.name,
                        pct(cur),
                        pct(new),
                        if unmute { " (sessizden çıkarıldı)" } else { "" }
                    )
                }
            };
            let line = if devices.len() > 1 { format!("{line} [{}]", s.device) } else { line };
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
    }
    if action == "get" && devices.len() == 1 {
        lines.push(format!("(cihaz: {}; uygulama sesi ana sesin yüzdesidir)", devices.iter().next().unwrap()));
    }
    Ok(lines.join("\n"))
}

/// Etkin ses çıkışları; varsayılan (ve farklıysa iletişim varsayılanı) işaretli.
pub fn outputs() -> Res<String> {
    let _com = Com::mta();
    let en = enumerator()?;
    let def = default_render_id(&en, eConsole);
    let comm = default_render_id(&en, eCommunications);
    let lines: Vec<String> = render_devices(&en)
        .iter()
        .map(|(_, id, name)| {
            let mut s = format!("• {name}");
            if *id == def {
                s.push_str(" (varsayılan)");
            }
            if *id == comm && comm != def {
                s.push_str(" (iletişim varsayılanı)");
            }
            s
        })
        .collect();
    Ok(if lines.is_empty() { "Etkin ses çıkışı yok.".into() } else { lines.join("\n") })
}

// IPolicyConfig: belgelenmemiş ama Windows 7'den beri değişmeyen arayüz (Ses ayarlarının da
// kullandığı). Yalnızca SetDefaultEndpoint çağrılır; önceki yuvalar atlanır.
const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
const IID_POLICY_CONFIG: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);

#[repr(C)]
struct PolicyConfigVtbl {
    /// QueryInterface, AddRef, Release.
    unknown: [usize; 3],
    /// GetMixFormat, GetDeviceFormat, ResetDeviceFormat, SetDeviceFormat, GetProcessingPeriod,
    /// SetProcessingPeriod, GetShareMode, SetShareMode, GetPropertyValue, SetPropertyValue.
    skipped: [usize; 10],
    set_default_endpoint: unsafe extern "system" fn(this: *mut core::ffi::c_void, id: PCWSTR, role: ERole) -> HRESULT,
}

fn set_default_endpoint(id: &str) -> Res<()> {
    unsafe {
        let unk: IUnknown = CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL)?;
        let mut raw = std::ptr::null_mut();
        unk.query(&IID_POLICY_CONFIG, &mut raw).ok()?;
        // Sahiplik IUnknown'a geçer: düşünce Release edilir.
        let _owner = IUnknown::from_raw(raw);
        let vt = &**(raw as *const *const PolicyConfigVtbl);
        let w = wide(id);
        for role in [eConsole, eMultimedia, eCommunications] {
            (vt.set_default_endpoint)(raw, PCWSTR(w.as_ptr()), role).ok()?;
        }
    }
    Ok(())
}

/// Varsayılan ses çıkışını (bütün roller) adında `name_part` geçen cihaza alır.
pub fn set_output(name_part: &str) -> Res<String> {
    let q = fold(name_part.trim());
    if q.is_empty() {
        return Err("çıkış cihazı adı verilmedi".into());
    }
    let _com = Com::mta();
    let en = enumerator()?;
    let devs = render_devices(&en);
    let names = || devs.iter().map(|(_, _, n)| n.as_str()).collect::<Vec<_>>().join(", ");
    let exact: Vec<_> = devs.iter().filter(|(_, _, n)| fold(n) == q).collect();
    let hits: Vec<_> =
        if exact.is_empty() { devs.iter().filter(|(_, _, n)| fold(n).contains(&q)).collect() } else { exact };
    let (_, id, name) = match hits.as_slice() {
        [one] => *one,
        [] => return Err(format!("adında \"{name_part}\" geçen ses çıkışı yok. Çıkışlar: {}", names()).into()),
        many => {
            let list = many.iter().map(|(_, _, n)| n.as_str()).collect::<Vec<_>>().join(", ");
            return Err(format!("\"{name_part}\" birden çok çıkışa uyuyor: {list}; daha belirgin bir ad ver").into());
        }
    };
    if default_render_id(&en, eConsole) == *id && default_render_id(&en, eCommunications) == *id {
        return Ok(format!("Zaten varsayılan çıkış: {name}"));
    }
    set_default_endpoint(id).map_err(|e| format!("çıkış değiştirilemedi: {e}"))?;
    log!("ses çıkışı: {name}");
    Ok(format!("Varsayılan ses çıkışı artık: {name}"))
}

// ---------------------------------------------------------------------------------------------
// Pencereler

struct Win {
    hwnd: HWND,
    title: String,
    process: String,
}

/// Görünür, başlıklı, sahipsiz üst düzey pencereler (Z sırasıyla, en üstteki önce).
fn top_windows() -> Vec<Win> {
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        unsafe {
            let v = &mut *(l.0 as *mut Vec<HWND>);
            v.push(h);
        }
        true.into()
    }
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut all as *mut Vec<HWND> as isize));
    }
    all.into_iter()
        .filter_map(|h| {
            let title = unsafe { usable(h) }?;
            Some(Win { hwnd: h, title, process: window_process(h) })
        })
        .collect()
}

unsafe fn usable(h: HWND) -> Option<String> {
    unsafe {
        if !IsWindowVisible(h).as_bool() || GetWindow(h, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return None;
        }
        if GetWindowLongPtrW(h, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return None;
        }
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(h, DWMWA_CLOAKED, (&mut cloaked as *mut u32).cast(), 4).is_ok() && cloaked != 0 {
            return None;
        }
        if class_name(h) == "Progman" {
            return None;
        }
        let len = GetWindowTextLengthW(h);
        if len <= 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(h, &mut buf);
        let t = String::from_utf16_lossy(&buf[..n.max(0) as usize]).trim().to_string();
        (!t.is_empty()).then_some(t)
    }
}

fn class_name(h: HWND) -> String {
    let mut class = [0u16; 128];
    let n = unsafe { GetClassNameW(h, &mut class) };
    String::from_utf16_lossy(&class[..n.max(0) as usize])
}

fn window_process(h: HWND) -> String {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    process_stem(pid).unwrap_or_default()
}

/// Başlığında (yoksa süreç adında) `part` geçen en üstteki pencere.
fn find_window(part: &str) -> Res<Win> {
    let q = fold(part.trim());
    if q.is_empty() {
        return Err("pencere başlığı verilmedi".into());
    }
    let wins = top_windows();
    let i = wins
        .iter()
        .position(|w| fold(&w.title).contains(&q))
        .or_else(|| wins.iter().position(|w| fold(&w.process).contains(&q)))
        .ok_or_else(|| format!("başlığında ya da süreç adında \"{part}\" geçen pencere yok"))?;
    Ok(wins.into_iter().nth(i).unwrap())
}

/// Pencereyi bulunduğu monitörün çalışma alanının sol ya da sağ yarısına yerleştirir. Görünmez
/// kenarlıklar (DWM) hesaba katılır: görünen çerçeve tam yarıyı kaplar.
fn snap(h: HWND, left: bool) -> Res<()> {
    let _dpi = Dpi::set();
    unsafe {
        if IsZoomed(h).as_bool() || IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let mon = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return Err("monitör bilgisi alınamadı".into());
        }
        let wa = mi.rcWork;
        let mut wr = RECT::default();
        GetWindowRect(h, &mut wr)?;
        let mut fr = RECT::default();
        if DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut fr as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        )
        .is_err()
        {
            fr = wr;
        }
        // Görünmez kenar payları (Windows 10/11'de genelde sol/sağ/alt ~7 px).
        let (ml, mt, mr, mb) = (fr.left - wr.left, fr.top - wr.top, wr.right - fr.right, wr.bottom - fr.bottom);
        let half = (wa.right - wa.left) / 2;
        let (x, w) = if left { (wa.left, half) } else { (wa.left + half, wa.right - wa.left - half) };
        SetWindowPos(
            h,
            None,
            x - ml,
            wa.top - mt,
            w + ml + mr,
            wa.bottom - wa.top + mt + mb,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        )?;
        let _ = SetForegroundWindow(h);
    }
    Ok(())
}

/// Masaüstünü gösterir: Shell.Application MinimizeAll; olmazsa Win+D.
fn minimize_all() -> Res<()> {
    use windows::Win32::UI::Shell::{IShellDispatch, Shell};
    let _com = Com::init(COINIT_APARTMENTTHREADED);
    let shell: windows::core::Result<IShellDispatch> = unsafe { CoCreateInstance(&Shell, None, CLSCTX_ALL) };
    if let Ok(s) = shell
        && unsafe { s.MinimizeAll() }.is_ok()
    {
        return Ok(());
    }
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_LWIN,
    };
    let key = |vk: u16, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let d = b'D' as u16;
    let inputs = [key(VK_LWIN.0, false), key(d, false), key(d, true), key(VK_LWIN.0, true)];
    if unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) } as usize != inputs.len() {
        return Err("masaüstü gösterilemedi".into());
    }
    Ok(())
}

/// Pencere işlemi. `action`: snap_left, snap_right, maximize, minimize, restore, close (nazikçe
/// WM_CLOSE; uygulama kaydetmeyi sorabilir), top (her zaman üstte aç/kapa), minimize_all
/// (masaüstünü göster; `title_part` gerekmez). Pencere başlığında ya da süreç adında
/// `title_part` geçen en üstteki penceredir.
pub fn window(action: &str, title_part: &str) -> Res<String> {
    let action = action.trim().to_lowercase();
    if action == "minimize_all" {
        minimize_all()?;
        return Ok("Bütün pencereler simge durumuna küçültüldü (masaüstü).".into());
    }
    if !matches!(action.as_str(), "snap_left" | "snap_right" | "maximize" | "minimize" | "restore" | "close" | "top") {
        return Err(format!(
            "bilinmeyen pencere eylemi: {action} (snap_left, snap_right, maximize, minimize, restore, close, top, minimize_all)"
        )
        .into());
    }
    let w = find_window(title_part)?;
    let h = w.hwnd;
    if unsafe { IsHungAppWindow(h) }.as_bool() {
        return Err(format!("pencere yanıt vermiyor: {}", w.title).into());
    }
    let what = unsafe {
        match action.as_str() {
            "snap_left" | "snap_right" => {
                let left = action == "snap_left";
                snap(h, left)?;
                if left { "sola yaslandı" } else { "sağa yaslandı" }
            }
            "maximize" => {
                let _ = ShowWindowAsync(h, SW_MAXIMIZE);
                let _ = SetForegroundWindow(h);
                "ekranı kaplayacak biçimde büyütüldü"
            }
            "minimize" => {
                let _ = ShowWindowAsync(h, SW_MINIMIZE);
                "simge durumuna küçültüldü"
            }
            "restore" => {
                let _ = ShowWindowAsync(h, SW_RESTORE);
                let _ = SetForegroundWindow(h);
                "eski boyutuna getirildi"
            }
            "close" => {
                PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0))?;
                "kapatma isteği gönderildi"
            }
            _ => {
                let topmost = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 != 0;
                let after = if topmost { HWND_NOTOPMOST } else { HWND_TOPMOST };
                SetWindowPos(h, Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)?;
                if topmost { "artık her zaman üstte değil" } else { "artık her zaman üstte" }
            }
        }
    };
    Ok(format!("{} — {}: {what}", w.title, w.process))
}

// ---------------------------------------------------------------------------------------------
// Uygulamalar (Başlat menüsü)

#[derive(Clone)]
struct App {
    name: String,
    /// .lnk/.url yolu ya da "shell:AppsFolder\<AUMID>" (Mağaza uygulamaları).
    path: String,
    key: String,
}

static APPS: Mutex<Option<(Instant, Vec<App>)>> = Mutex::new(None);

/// Başlat menüsü kısayolları (ortak ve kullanıcınınki) ve Uygulamalar klasöründeki Mağaza
/// uygulamaları; 60 sn önbellekte.
fn app_list() -> Vec<App> {
    let mut cache = APPS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, list)) = cache.as_ref()
        && t.elapsed() < Duration::from_secs(60)
    {
        return list.clone();
    }
    let mut list = Vec::new();
    let mut seen = HashSet::new();
    let roots = [
        ("ProgramData", r"Microsoft\Windows\Start Menu\Programs"),
        ("APPDATA", r"Microsoft\Windows\Start Menu\Programs"),
    ];
    for (var, sub) in roots {
        let Some(base) = std::env::var_os(var) else { continue };
        let mut stack = vec![(PathBuf::from(base).join(sub), 0)];
        while let Some((dir, depth)) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    if depth < 6 {
                        stack.push((p, depth + 1));
                    }
                    continue;
                }
                let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
                if ext != "lnk" && ext != "url" {
                    continue;
                }
                let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let key = fold(&name);
                if !name.is_empty() && seen.insert(key.clone()) {
                    list.push(App { name, path: p.display().to_string(), key });
                }
            }
        }
    }
    for (name, aumid) in store_apps() {
        let key = fold(&name);
        if seen.insert(key.clone()) {
            list.push(App { name, path: format!(r"shell:AppsFolder\{aumid}"), key });
        }
    }
    *cache = Some((Instant::now(), list.clone()));
    list
}

/// Uygulamalar klasöründeki (shell:AppsFolder) öğeler: (ad, AUMID). Kısayolu olmayan Mağaza
/// uygulamalarını (WhatsApp, Hesap Makinesi...) da kapsar.
fn store_apps() -> Vec<(String, String)> {
    use windows::Win32::UI::Shell::{
        BHID_EnumItems, IEnumShellItems, IShellItem, SHCreateItemFromParsingName, SIGDN_NORMALDISPLAY,
        SIGDN_PARENTRELATIVEPARSING,
    };
    let _com = Com::mta();
    let mut out = Vec::new();
    unsafe {
        let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(windows::core::w!("shell:AppsFolder"), None)
        else {
            return out;
        };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else { return out };
        loop {
            let mut batch: [Option<IShellItem>; 32] = Default::default();
            let mut n = 0u32;
            if items.Next(&mut batch, Some(&mut n)).is_err() || n == 0 {
                break;
            }
            for it in batch.iter().take(n as usize).flatten() {
                let name = it.GetDisplayName(SIGDN_NORMALDISPLAY).map(|p| take_pwstr(p)).unwrap_or_default();
                let id = it.GetDisplayName(SIGDN_PARENTRELATIVEPARSING).map(|p| take_pwstr(p)).unwrap_or_default();
                // Yalnızca paketli uygulamalar (AUMID "Paket_yayıncı!Uygulama"); klasik olanlar
                // zaten kısayollardan geliyor.
                if !name.is_empty() && id.contains('!') {
                    out.push((name, id));
                }
            }
            if (n as usize) < batch.len() {
                break;
            }
        }
    }
    out
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + (ca != cb) as usize).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// `q` `n` içinde sırayla geçiyorsa, kelime başından başlayan en kısa geçişin uzunluğu.
fn subsequence_span(q: &[char], n: &[char], starts: &[usize]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for &s in starts {
        if n.get(s) != q.first() {
            continue;
        }
        let mut qi = 0;
        let mut end = s;
        for (j, c) in n.iter().enumerate().skip(s) {
            if *c == q[qi] {
                qi += 1;
                end = j;
                if qi == q.len() {
                    break;
                }
            }
        }
        if qi == q.len() {
            let span = end - s + 1;
            best = Some(best.map_or(span, |b| b.min(span)));
        }
    }
    best
}

/// Bulanık eşleşme puanı (0 = eşleşmez, 100 = tam). `q` ve `name` katlanmış (`fold`).
fn fuzzy(q: &str, name: &str) -> f64 {
    let alnum = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    let (qc, nc) = (alnum(q), alnum(name));
    if qc.is_empty() || nc.is_empty() {
        return 0.0;
    }
    if name == q || nc == qc {
        return 100.0;
    }
    if name.starts_with(q) || nc.starts_with(&qc) {
        return 90.0;
    }
    let words: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    if words.iter().any(|w| w.starts_with(q)) {
        return 85.0;
    }
    let qv: Vec<char> = qc.chars().collect();
    let nv: Vec<char> = nc.chars().collect();
    // Kelime başları (`nc` içinde karakter sırası).
    let mut starts = Vec::new();
    let mut pos = 0;
    for w in &words {
        starts.push(pos);
        pos += w.chars().count();
    }
    let cover = qv.len() as f64 / nv.len() as f64;
    let mut score: f64 = 0.0;
    // Bitişik geçiş: kelime başındaysa güçlü; kelime ortasındaysa ("stem" → "İstemcisi",
    // "System") yazım hatası eşleşmelerinin altında kalır.
    if nc.contains(&qc) {
        let at_word = nv.windows(qv.len()).enumerate().any(|(i, w)| w == qv.as_slice() && starts.contains(&i));
        if at_word {
            return 75.0;
        }
        score = 50.0 + 10.0 * cover;
    }
    // Her sorgu kelimesi bir kelimenin başı ya da baş harflerin parçası ("vs code" → Visual Studio Code).
    let initials: String = words.iter().filter_map(|w| w.chars().next()).collect();
    let qwords: Vec<&str> = q.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    if qwords
        .iter()
        .all(|qw| words.iter().any(|w| w.starts_with(qw)) || (qw.chars().count() >= 2 && initials.contains(qw)))
    {
        return 72.0;
    }
    if qv.len() >= 2 && initials.starts_with(&qc) {
        return 70.0;
    }
    // Sırayla geçen harfler, kelime başından: sıkı ve adın çoğunu kaplıyorsa yüksek.
    if let Some(span) = subsequence_span(&qv, &nv, &starts) {
        let tight = qv.len() as f64 / span as f64;
        score = score.max(30.0 + 15.0 * tight + 10.0 * cover);
    }
    // Yazım hatası: bir kelimeye ya da bütün ada düzenleme uzaklığı.
    if qv.len() >= 3 {
        let mut targets: Vec<Vec<char>> = words.iter().map(|w| w.chars().collect()).collect();
        targets.push(nv.clone());
        for t in targets {
            let d = levenshtein(&qv, &t);
            let sim = 1.0 - d as f64 / qv.len().max(t.len()) as f64;
            if sim >= 0.6 {
                score = score.max(70.0 * sim);
            }
        }
    }
    score
}

/// Kaldırma, yardım gibi asıl uygulama olmayan kısayollar geri plana düşer.
fn noise_penalty(key: &str, q: &str) -> f64 {
    const NOISE: [&str; 10] = [
        "uninstall",
        "kaldir",
        "remove",
        "readme",
        "help",
        "yardim",
        "documentation",
        "website",
        "release notes",
        "manual",
    ];
    if NOISE.iter().any(|n| key.contains(n) && !q.contains(n)) { 25.0 } else { 0.0 }
}

/// En iyi eşleşmeler: (puan, uygulama), iyiden kötüye.
fn ranked(query: &str) -> Vec<(f64, App)> {
    let q = fold(query.trim());
    let mut v: Vec<(f64, App)> = app_list()
        .into_iter()
        .map(|a| (fuzzy(&q, &a.key) - noise_penalty(&a.key, &q), a))
        .filter(|(s, _)| *s >= 30.0)
        .collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.name.chars().count().cmp(&b.1.name.chars().count())));
    v
}

/// Başlat menüsünde adı `query`'ye en çok benzeyen uygulamalar (en çok 10): "ad — yol".
pub fn apps(query: &str) -> Res<String> {
    if query.trim().is_empty() {
        let n = app_list().len();
        return Err(format!("aranacak uygulama adı verilmedi ({n} uygulama var)").into());
    }
    let lines: Vec<String> =
        ranked(query).into_iter().take(10).map(|(_, a)| format!("{} — {}", a.name, a.path)).collect();
    Ok(if lines.is_empty() { format!("\"{query}\" ile eşleşen uygulama yok.") } else { lines.join("\n") })
}

/// Bu puanın üstü "iyi eşleşme" sayılır ve doğrudan açılır.
const LAUNCH_SCORE: f64 = 44.0;

/// Adı `query`'ye en çok benzeyen uygulamayı açar; iyi bir eşleşme yoksa `query`'yi olduğu gibi
/// açmayı dener (App Paths, PATH, "spotify:" gibi adresler).
pub fn launch(query: &str) -> Res<String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("açılacak uygulama verilmedi".into());
    }
    if let Some((_, app)) = ranked(query).into_iter().next().filter(|(s, _)| *s >= LAUNCH_SCORE) {
        crate::computer::open(&app.path, None)?;
        log!("uygulama açıldı: {} ({})", app.name, app.path);
        return Ok(format!("Açıldı: {}", app.name));
    }
    crate::computer::open(query, None).map_err(|e| format!("Başlat menüsünde \"{query}\" bulunamadı; {e}"))?;
    log!("uygulama açıldı (doğrudan): {query}");
    Ok(format!("Başlat menüsünde eşleşme yok; doğrudan açıldı: {query}"))
}

// ---------------------------------------------------------------------------------------------
// Dosyalar

/// Bilinen klasörün gerçek yolu (yeniden yönlendirilmiş, OneDrive'a taşınmış olabilir).
fn known_folder(id: &GUID) -> Option<PathBuf> {
    use windows::Win32::UI::Shell::{KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    let p = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.ok()?;
    let s = unsafe { take_pwstr(p) };
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// Aranan kullanıcı klasörleri; iç içe olanlar (Resimler\Ekran Görüntüleri) bir kez.
fn user_folders() -> Vec<PathBuf> {
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures,
        FOLDERID_Screenshots, FOLDERID_Videos,
    };
    let ids = [
        FOLDERID_Desktop,
        FOLDERID_Downloads,
        FOLDERID_Documents,
        FOLDERID_Pictures,
        FOLDERID_Screenshots,
        FOLDERID_Videos,
        FOLDERID_Music,
    ];
    let mut out: Vec<PathBuf> = Vec::new();
    for id in &ids {
        let Some(p) = known_folder(id) else { continue };
        if !p.is_dir() {
            continue;
        }
        let low = p.display().to_string().to_lowercase();
        let inside = |o: &PathBuf| {
            let ol = o.display().to_string().to_lowercase();
            low == ol || low.starts_with(&format!("{}\\", ol.trim_end_matches('\\')))
        };
        if out.iter().any(inside) {
            continue;
        }
        // Daha önce eklenen bir klasör bunun içindeyse onu çıkar.
        out.retain(|o| {
            !o.display().to_string().to_lowercase().starts_with(&format!("{}\\", low.trim_end_matches('\\')))
        });
        out.push(p);
    }
    out
}

fn kind_exts(kind: &str) -> Res<Option<&'static [&'static str]>> {
    Ok(Some(match kind {
        "" | "any" | "all" => return Ok(None),
        "pdf" => &["pdf"],
        "image" => &[
            "jpg", "jpeg", "png", "gif", "bmp", "webp", "heic", "heif", "tif", "tiff", "svg", "ico", "avif", "raw",
            "cr2", "nef", "arw", "dng",
        ],
        "video" => &["mp4", "mkv", "avi", "mov", "wmv", "webm", "m4v", "flv", "mpg", "mpeg", "3gp", "ts"],
        "audio" => &["mp3", "wav", "flac", "aac", "ogg", "m4a", "wma", "opus", "aiff", "aif", "mid", "midi"],
        "document" => &[
            "pdf", "doc", "docx", "odt", "rtf", "txt", "md", "xls", "xlsx", "ods", "csv", "ppt", "pptx", "odp", "epub",
            "pages", "key", "numbers", "xps",
        ],
        "archive" => &["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "tgz", "zst", "iso", "cab"],
        "code" => &[
            "rs", "py", "js", "mjs", "ts", "tsx", "jsx", "java", "kt", "c", "cc", "cpp", "h", "hpp", "cs", "go", "rb",
            "php", "swift", "lua", "sql", "sh", "ps1", "bat", "cmd", "html", "css", "scss", "json", "toml", "yaml",
            "yml", "xml", "ipynb", "vue", "svelte",
        ],
        _ => {
            return Err(
                format!("bilinmeyen dosya türü: {kind} (pdf, image, video, audio, document, archive, code)").into()
            );
        }
    }))
}

const MAX_DEPTH: usize = 6;
const MAX_SCANNED: usize = 50_000;

/// Kullanıcı klasörlerinde (Masaüstü, İndirilenler, Belgeler, Resimler ve Ekran Görüntüleri,
/// Videolar, Müzik) dosya arar. `query` adın parçası (kelimeleri herhangi sırada; boşsa son
/// dosyalar), `kind` "" | pdf | image | video | audio | document | archive | code, `days` 0 ya
/// da son N gün. En yeniden eskiye "yol | boyut | değişme zamanı" satırları.
pub fn find_files(query: &str, kind: &str, days: u32, limit: usize) -> Res<String> {
    use std::os::windows::fs::MetadataExt;
    let exts = kind_exts(kind.trim().to_lowercase().as_str())?;
    let words: Vec<String> = fold(query.trim()).split_whitespace().map(String::from).collect();
    let limit = if limit == 0 { 20 } else { limit.min(200) };
    let since = if days == 0 { 0 } else { now_secs().saturating_sub(days as u64 * 86_400) };
    let roots = user_folders();
    if roots.is_empty() {
        return Err("kullanıcı klasörleri bulunamadı".into());
    }
    const SKIP: [&str; 5] = ["node_modules", "appdata", "$recycle.bin", "__pycache__", "system volume information"];
    let mut found: Vec<(u64, PathBuf, u64)> = Vec::new();
    let mut scanned = 0usize;
    // Genişlik öncelikli: sınıra ulaşılırsa sığ (çoğu zaman yeni) dosyalar taranmış olur.
    let mut queue: VecDeque<(PathBuf, usize)> = roots.iter().map(|r| (r.clone(), 0)).collect();
    'scan: while let Some((dir, depth)) = queue.pop_front() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            scanned += 1;
            if scanned > MAX_SCANNED {
                break 'scan;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let Ok(md) = e.metadata() else { continue };
            // Gizli ve sistem dosyaları (desktop.ini...) atlanır.
            if md.file_attributes() & 0x6 != 0 {
                continue;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                let low = name.to_lowercase();
                if depth + 1 < MAX_DEPTH && !low.starts_with('.') && !SKIP.contains(&low.as_str()) {
                    queue.push_back((e.path(), depth + 1));
                }
                continue;
            }
            let modified =
                md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
            if modified < since {
                continue;
            }
            if let Some(exts) = exts {
                let ext = Path::new(&name).extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
                if !exts.contains(&ext.as_str()) {
                    continue;
                }
            }
            if !words.is_empty() {
                let key = fold(&name);
                if !words.iter().all(|w| key.contains(w.as_str())) {
                    continue;
                }
            }
            found.push((modified, e.path(), md.len()));
        }
    }
    found.sort_by_key(|a| std::cmp::Reverse(a.0));
    let total = found.len();
    let mut out: Vec<String> = found
        .into_iter()
        .take(limit)
        .map(|(t, p, size)| format!("{} | {} | {}", p.display(), size_text(size), when(t)))
        .collect();
    if out.is_empty() {
        out.push("Eşleşen dosya yok.".into());
    } else if total > limit {
        out.push(format!("(toplam {total} eşleşme; en yeni {limit} tanesi)"));
    }
    if scanned > MAX_SCANNED {
        out.push(format!("(tarama {MAX_SCANNED} öğede kesildi; daha belirgin bir arama gerekebilir)"));
    }
    Ok(out.join("\n"))
}

fn existing(path: &str) -> Res<PathBuf> {
    let p = PathBuf::from(path.trim().trim_matches('"'));
    if p.as_os_str().is_empty() {
        return Err("dosya yolu verilmedi".into());
    }
    if !p.exists() {
        return Err(format!("böyle bir dosya yok: {}", p.display()).into());
    }
    Ok(p)
}

/// Dosyayı varsayılan uygulamasıyla açar.
pub fn open_file(path: &str) -> Res<String> {
    let p = existing(path)?;
    crate::computer::open(&p.display().to_string(), None)?;
    Ok(format!("Açıldı: {}", p.display()))
}

/// Dosyayı Gezgin'de seçili gösterir.
pub fn reveal_file(path: &str) -> Res<String> {
    let p = existing(path)?;
    crate::util::reveal(&p);
    Ok(format!("Gezgin'de gösterildi: {}", p.display()))
}

// ---------------------------------------------------------------------------------------------
// Pano

const CF_BITMAP: u32 = 2;
const CF_DIB: u32 = 8;
const CF_HDROP: u32 = 15;
const CF_DIBV5: u32 = 17;
const MAX_CLIP_CHARS: usize = 20_000;

/// Panoyu açık tutan bekçi; birkaç kez dener.
struct Clip;

impl Clip {
    fn open() -> Res<Self> {
        use windows::Win32::System::DataExchange::OpenClipboard;
        for _ in 0..10 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(Clip);
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        Err("pano başka bir uygulamada açık, okunamadı".into())
    }
}

impl Drop for Clip {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::DataExchange::CloseClipboard();
        }
    }
}

fn clip_has(fmt: u32) -> bool {
    unsafe { windows::Win32::System::DataExchange::IsClipboardFormatAvailable(fmt) }.is_ok()
}

/// Panodaki dosyaların yolları (Gezgin'de kopyalanmışsa).
fn clip_files() -> Res<Vec<String>> {
    use windows::Win32::System::DataExchange::GetClipboardData;
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    let _c = Clip::open()?;
    unsafe {
        let h = GetClipboardData(CF_HDROP)?;
        let drop = HDROP(h.0);
        let n = DragQueryFileW(drop, u32::MAX, None);
        let mut out = Vec::new();
        for i in 0..n {
            let len = DragQueryFileW(drop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let got = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
            out.push(String::from_utf16_lossy(&buf[..got.min(len)]));
        }
        Ok(out)
    }
}

/// Panodaki resmin boyutu (DIB başlığından).
fn clip_image_size() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::Graphics::Gdi::BITMAPINFOHEADER;
    use windows::Win32::System::DataExchange::GetClipboardData;
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    let _c = Clip::open().ok()?;
    unsafe {
        let h = GetClipboardData(CF_DIB).ok()?;
        let g = HGLOBAL(h.0);
        if GlobalSize(g) < size_of::<BITMAPINFOHEADER>() {
            return None;
        }
        let p = GlobalLock(g) as *const BITMAPINFOHEADER;
        if p.is_null() {
            return None;
        }
        let r = ((*p).biWidth, (*p).biHeight.abs());
        let _ = GlobalUnlock(g);
        Some(r)
    }
}

/// Panodaki içerik: metin; dosyalar kopyalanmışsa yolları; resimse boyutu.
pub fn clipboard_get() -> Res<String> {
    if clip_has(CF_HDROP) {
        let files = clip_files()?;
        if !files.is_empty() {
            return Ok(format!("Panoda {} dosya var:\n{}", files.len(), files.join("\n")));
        }
    }
    if let Some(text) = crate::util::clipboard_text() {
        if text.trim().is_empty() {
            return Ok("Panodaki metin boş.".into());
        }
        let n = text.chars().count();
        if n > MAX_CLIP_CHARS {
            let head: String = text.chars().take(MAX_CLIP_CHARS).collect();
            return Ok(format!("{head}\n…(pano metni {n} karakter; ilk {MAX_CLIP_CHARS} karakter)"));
        }
        return Ok(text);
    }
    if clip_has(CF_DIB) || clip_has(CF_DIBV5) || clip_has(CF_BITMAP) {
        return Ok(match clip_image_size() {
            Some((w, h)) => format!("Panoda bir resim var ({w}×{h}); metin yok."),
            None => "Panoda bir resim var; metin yok.".into(),
        });
    }
    Ok("Pano boş (ya da metin olmayan bir içerik var).".into())
}

/// Panoya metin koyar.
pub fn clipboard_set(text: &str) -> Res<String> {
    if !crate::edit::set_clipboard_text(text) {
        return Err("panoya yazılamadı (başka bir uygulama panoyu tutuyor olabilir)".into());
    }
    Ok(format!("Panoya kopyalandı ({} karakter).", text.chars().count()))
}

// ---------------------------------------------------------------------------------------------
// Tarayıcı

const BROWSERS: [&str; 8] = ["brave", "chrome", "msedge", "vivaldi", "opera", "chromium", "arc", "thorium"];

fn is_browser(h: HWND) -> bool {
    class_name(h) == "Chrome_WidgetWin_1" && BROWSERS.contains(&window_process(h).to_lowercase().as_str())
}

/// Ön plandaki Chromium tabanlı tarayıcı penceresi; değilse en üstteki.
fn browser_window() -> Option<(HWND, String)> {
    unsafe {
        let fg = GetAncestor(GetForegroundWindow(), GA_ROOTOWNER);
        if !fg.is_invalid()
            && is_browser(fg)
            && let Some(t) = usable(fg)
        {
            return Some((fg, t));
        }
    }
    top_windows().into_iter().find(|w| is_browser(w.hwnd)).map(|w| (w.hwnd, w.title))
}

/// Başlıktan " - Brave" gibi tarayıcı ekini atar.
fn strip_browser_suffix(title: &str) -> String {
    const SUFFIXES: [&str; 7] = [
        " - Brave",
        " - Google Chrome",
        " - Microsoft\u{200b} Edge",
        " - Microsoft Edge",
        " - Vivaldi",
        " - Opera",
        " - Chromium",
    ];
    for s in SUFFIXES {
        if let Some(i) = title.rfind(s) {
            return title[..i].to_string();
        }
    }
    title.to_string()
}

/// Adres çubuğunun değeri (UI Automation: pencerenin ilk Edit denetimi).
fn omnibox_value(h: HWND) -> Res<String> {
    use windows::Win32::System::Variant::{VARIANT, VT_I4};
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomation2, IUIAutomationValuePattern, TreeScope_Descendants,
        UIA_ControlTypePropertyId, UIA_EditControlTypeId, UIA_ValuePatternId,
    };
    unsafe {
        let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL)?;
        if let Ok(u2) = uia.cast::<IUIAutomation2>() {
            let _ = u2.SetConnectionTimeout(2000);
            let _ = u2.SetTransactionTimeout(3000);
        }
        let root = uia.ElementFromHandle(h)?;
        let mut v = VARIANT::default();
        (*v.Anonymous.Anonymous).vt = VT_I4;
        (*v.Anonymous.Anonymous).Anonymous.lVal = UIA_EditControlTypeId.0;
        let cond = uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &v)?;
        // Chromium erişilebilirlik ağacını ilk istekte kurar: bulunamazsa kısa aralarla yeniden.
        let mut last = None;
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(200));
            }
            match root.FindFirst(TreeScope_Descendants, &cond) {
                Ok(edit) => {
                    let pattern: IUIAutomationValuePattern = edit.GetCurrentPatternAs(UIA_ValuePatternId)?;
                    return Ok(pattern.CurrentValue()?.to_string());
                }
                Err(e) => last = Some(e),
            }
        }
        Err(format!("adres çubuğu bulunamadı{}", last.map(|e| format!(": {}", e.message())).unwrap_or_default()).into())
    }
}

/// Ön plandaki (ya da en son kullanılan) Chromium tarayıcısında (Brave, Chrome, Edge) açık
/// sekme: "başlık | adres". Adres, adres çubuğunda görüldüğü gibidir ("https://" gizli olabilir).
pub fn browser_tab() -> Res<String> {
    let (h, title) = browser_window()
        .ok_or("açık bir Chromium tarayıcı penceresi yok (Brave, Chrome, Edge; Firefox desteklenmez)")?;
    let _com = Com::mta();
    let url = omnibox_value(h)?;
    let url = url.trim();
    Ok(format!("{} | {}", strip_browser_suffix(&title), if url.is_empty() { "(adres boş: yeni sekme)" } else { url }))
}

const MAX_PAGE_CHARS: usize = 20_000;

/// `s` içinde `from`dan sonra ASCII büyük/küçük harf duyarsız `needle`.
fn find_ci(hay: &str, needle: &str, from: usize) -> Option<usize> {
    hay.get(from..)?.find(needle).map(|i| i + from)
}

/// Sayfanın düz metni: script, style, yorum ve etiketler atılır, varlıklar çözülür, boşluklar
/// sıkıştırılır; blok etiketleri satır sonu olur.
fn html_to_text(html: &str) -> (String, String) {
    // Küçük harfli kopya: ASCII küçültme bayt konumlarını değiştirmez.
    let low = html.to_ascii_lowercase();
    let title = find_ci(&low, "<title", 0)
        .and_then(|s| Some((s, find_ci(&low, ">", s)? + 1)))
        .and_then(|(_, s)| Some((s, find_ci(&low, "</title", s)?)))
        .map(|(s, e)| decode_entities(html[s..e].trim()))
        .unwrap_or_default();
    let mut out = String::with_capacity(html.len() / 3);
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < html.len() {
        if bytes[i] != b'<' {
            let next = find_ci(html, "<", i).unwrap_or(html.len());
            out.push_str(&html[i..next]);
            i = next;
            continue;
        }
        if low[i..].starts_with("<!--") {
            i = find_ci(&low, "-->", i + 4).map_or(html.len(), |e| e + 3);
            continue;
        }
        // Gövdesi atılan etiketler.
        let mut skipped = false;
        for tag in ["script", "style", "noscript", "svg", "template", "head"] {
            let open = format!("<{tag}");
            if low[i..].starts_with(&open)
                && low[i + open.len()..].starts_with(|c: char| c == '>' || c.is_ascii_whitespace() || c == '/')
            {
                let close = format!("</{tag}");
                i = find_ci(&low, &close, i + open.len())
                    .and_then(|e| find_ci(&low, ">", e))
                    .map_or(html.len(), |e| e + 1);
                skipped = true;
                break;
            }
        }
        if skipped {
            continue;
        }
        let end = find_ci(html, ">", i).map_or(html.len(), |e| e + 1);
        let name: String =
            low[i + 1..end].trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        let block = matches!(
            name.as_str(),
            "p" | "div"
                | "br"
                | "li"
                | "ul"
                | "ol"
                | "tr"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "section"
                | "article"
                | "header"
                | "footer"
                | "nav"
                | "table"
                | "blockquote"
                | "pre"
                | "hr"
                | "dt"
                | "dd"
                | "main"
                | "aside"
                | "figure"
        );
        out.push(if block { '\n' } else { ' ' });
        i = end;
    }
    let text = decode_entities(&out);
    // Satır içi boşlukları teke indir, boş satırları at.
    let lines: Vec<String> =
        text.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect();
    (title, lines.join("\n"))
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..].find(|c: char| c == ';' || c == '&' || c.is_whitespace()).map(|e| e + 1);
        let decoded = end.filter(|&e| rest.as_bytes().get(e) == Some(&b';') && e <= 12).and_then(|e| {
            let ent = &rest[1..e];
            let c = if let Some(num) = ent.strip_prefix('#') {
                let n = if let Some(h) = num.strip_prefix(['x', 'X']) {
                    u32::from_str_radix(h, 16).ok()
                } else {
                    num.parse().ok()
                };
                n.and_then(char::from_u32)
            } else {
                Some(match ent {
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    "nbsp" => ' ',
                    "ndash" => '–',
                    "mdash" => '—',
                    "hellip" => '…',
                    "lsquo" => '‘',
                    "rsquo" => '’',
                    "ldquo" => '“',
                    "rdquo" => '”',
                    "laquo" => '«',
                    "raquo" => '»',
                    "copy" => '©',
                    "reg" => '®',
                    "trade" => '™',
                    "euro" => '€',
                    "middot" => '·',
                    "bull" => '•',
                    "deg" => '°',
                    "times" => '×',
                    "ccedil" => 'ç',
                    "Ccedil" => 'Ç',
                    "ouml" => 'ö',
                    "Ouml" => 'Ö',
                    "uuml" => 'ü',
                    "Uuml" => 'Ü',
                    "iacute" => 'í',
                    "eacute" => 'é',
                    "agrave" => 'à',
                    "aacute" => 'á',
                    "acirc" => 'â',
                    "icirc" => 'î',
                    "ucirc" => 'û',
                    _ => return None,
                })
            };
            c.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Sayfayı indirip düz metnini verir (yalnızca https; en çok ~20 bin karakter). Şemasız adres
/// ("github.com/...") https sayılır.
pub fn fetch_page(url: &str) -> Res<String> {
    let url = url.trim();
    let url = if url.contains("://") { url.to_string() } else { format!("https://{url}") };
    if !url.to_lowercase().starts_with("https://") {
        return Err(format!("yalnızca https adresleri okunabilir: {url}").into());
    }
    let resp = crate::net::get(
        &url,
        &[("accept", "text/html,application/xhtml+xml;q=0.9,*/*;q=0.5"), ("accept-language", "tr,en;q=0.8")],
    )?
    .ok()?;
    let body = resp.text();
    let head = body.chars().take(2048).collect::<String>().to_ascii_lowercase();
    let is_html =
        head.contains("<html") || head.contains("<!doctype html") || head.contains("<head") || head.contains("<body");
    if !is_html {
        let binary = resp.body.iter().take(4096).any(|&b| b == 0);
        if binary {
            return Err(format!("yanıt HTML değil (ikili veri, {})", size_text(resp.body.len() as u64)).into());
        }
        let text: String = body.chars().take(MAX_PAGE_CHARS).collect();
        return Ok(format!("(yanıt HTML değil; düz metin olarak ilk kısmı)\n{text}"));
    }
    let (title, text) = html_to_text(&body);
    let n = text.chars().count();
    let mut out = String::new();
    if !title.is_empty() {
        out.push_str(&format!("Başlık: {title}\n\n"));
    }
    if n > MAX_PAGE_CHARS {
        out.extend(text.chars().take(MAX_PAGE_CHARS));
        out.push_str(&format!("\n…(sayfa metni {n} karakter; ilk {MAX_PAGE_CHARS})"));
    } else if text.is_empty() {
        out.push_str("(sayfada okunur metin yok; içerik JavaScript ile yükleniyor olabilir)");
    } else {
        out.push_str(&text);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Sistem

fn cpu_times() -> Option<(u64, u64)> {
    use windows::Win32::System::Threading::GetSystemTimes;
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    // Çekirdek süresi boşta geçeni de içerir.
    Some((ft_u64(idle), ft_u64(kernel) + ft_u64(user)))
}

/// Pil, işlemci kullanımı, bellek, açık kalma süresi, C: boş alanı.
pub fn status() -> Res<String> {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    use windows::Win32::System::SystemInformation::{GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut lines = Vec::new();
    // Pil
    let mut ps = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut ps) }.is_ok() {
        let plugged = ps.ACLineStatus == 1;
        if ps.BatteryFlag & 128 != 0 || ps.BatteryLifePercent == 255 {
            lines.push(format!("Pil: yok ({})", if plugged { "prize takılı" } else { "güç durumu bilinmiyor" }));
        } else {
            let mut s = format!("Pil: %{}", ps.BatteryLifePercent);
            if ps.BatteryFlag & 8 != 0 {
                s.push_str(", şarj oluyor");
            } else if plugged {
                s.push_str(", prize takılı");
            } else {
                s.push_str(", pilde");
                if ps.BatteryLifeTime != u32::MAX {
                    let m = ps.BatteryLifeTime / 60;
                    s.push_str(&format!(", yaklaşık {} sa {} dk kaldı", m / 60, m % 60));
                }
            }
            if ps.SystemStatusFlag == 1 {
                s.push_str(", pil tasarrufu açık");
            }
            lines.push(s);
        }
    }
    // İşlemci: 250 ms arayla iki ölçüm.
    if let Some((i1, t1)) = cpu_times() {
        std::thread::sleep(Duration::from_millis(250));
        if let Some((i2, t2)) = cpu_times() {
            let total = t2.saturating_sub(t1);
            if total > 0 {
                let busy = 100.0 * (1.0 - i2.saturating_sub(i1) as f64 / total as f64);
                lines.push(format!("İşlemci: %{:.0}", busy.clamp(0.0, 100.0)));
            }
        }
    }
    // Bellek
    let mut ms = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    if unsafe { GlobalMemoryStatusEx(&mut ms) }.is_ok() {
        let gb = |b: u64| b as f64 / (1024.0 * 1024.0 * 1024.0);
        let used = ms.ullTotalPhys.saturating_sub(ms.ullAvailPhys);
        lines.push(
            format!("Bellek: {:.1} / {:.1} GB kullanımda (%{})", gb(used), gb(ms.ullTotalPhys), ms.dwMemoryLoad)
                .replace('.', ","),
        );
    }
    // Açık kalma süresi
    let up = unsafe { GetTickCount64() } / 1000;
    let (d, h, m) = (up / 86_400, up % 86_400 / 3600, up % 3600 / 60);
    lines.push(if d > 0 {
        format!("Açık kalma: {d} gün {h} sa {m} dk")
    } else {
        format!("Açık kalma: {h} sa {m} dk")
    });
    // Disk
    let (mut free, mut total) = (0u64, 0u64);
    if unsafe { GetDiskFreeSpaceExW(windows::core::w!("C:\\"), Some(&mut free), Some(&mut total), None) }.is_ok()
        && total > 0
    {
        lines.push(format!("C: {} boş / {}", size_text(free), size_text(total)));
    }
    Ok(lines.join("\n"))
}

/// Güç: lock (ekranı kilitle), sleep (uyku; 1 sn sonra), display_off (ekranı kapat; ilk
/// fare/klavye girdisinde açılır).
pub fn power(action: &str) -> Res<String> {
    match action.trim().to_lowercase().as_str() {
        "lock" => {
            unsafe { windows::Win32::System::Shutdown::LockWorkStation() }
                .map_err(|e| format!("kilitlenemedi: {}", e.message()))?;
            log!("ekran kilitlendi");
            Ok("Bilgisayar kilitlendi.".into())
        }
        "sleep" => {
            // Sonuç modele ulaşsın diye kısa bir gecikmeyle; SetSuspendState uyanınca döner.
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(1000));
                log!("uyku moduna geçiliyor");
                if !unsafe { windows::Win32::System::Power::SetSuspendState(false, false, false) } {
                    log!("uyku başarısız: {}", windows::core::Error::from_thread().message());
                }
            });
            Ok("Bilgisayar 1 saniye içinde uyku moduna geçiyor.".into())
        }
        "display_off" => {
            // SendMessage herhangi bir asılı pencerede takılabilir: PostMessage.
            unsafe { PostMessageW(Some(HWND_BROADCAST), WM_SYSCOMMAND, WPARAM(SC_MONITORPOWER as usize), LPARAM(2)) }?;
            log!("ekran kapatıldı");
            Ok("Ekran kapatıldı (fare ya da klavyeyle açılır).".into())
        }
        other => Err(format!("bilinmeyen güç eylemi: {other} (lock, sleep, display_off)").into()),
    }
}

/// Ana sesin düzeyi (yüzde) ve kısık olup olmadığı.
fn master_level() -> Res<(f64, bool)> {
    let _com = Com::mta();
    let en = enumerator()?;
    unsafe {
        let ep: IAudioEndpointVolume = en.GetDefaultAudioEndpoint(eRender, eConsole)?.Activate(CLSCTX_ALL, None)?;
        Ok(((ep.GetMasterVolumeLevelScalar()? as f64 * 100.0).round(), ep.GetMute()?.as_bool()))
    }
}
