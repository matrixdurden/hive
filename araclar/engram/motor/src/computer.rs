//! Bilgisayarı kullanma: Claude'un computer toolset'i (ekran görüntüsü, yakınlaştırma, fare,
//! klavye) ile komut çalıştırma, terminal, uygulama açma ve pencere araçları.
//!
//! Her şey fiziksel piksellerde çalışır (iş parçacığı monitör başına DPI farkında v2'ye alınır).
//! `screenshot` imlecin bulunduğu monitörü yakalar, uzun kenarı en çok 1366 px olacak biçimde
//! küçültür ve bu eşlemeyi saklar; modelden gelen koordinatlar bu eşlemeyle gerçek piksele çevrilir.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::Value;
use windows::Win32::Foundation::{
    CloseHandle, GENERIC_READ, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, HGLOBAL, HWND, LPARAM, POINT, SetHandleInformation,
    WAIT_OBJECT_0,
};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GdiFlush, GetDC, GetMonitorInfoW, HDC, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
};
use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, PROPBAG2};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, COINIT_MULTITHREADED, CoCreateInstance,
    CoInitializeEx, CoUninitialize, STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET,
};
use windows::Win32::System::IO::CancelSynchronousIo;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    AttachThreadInput, CREATE_NEW_CONSOLE, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentThreadId, GetExitCodeProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::Win32::System::Variant::{VARIANT, VT_R4};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{
    SEE_MASK_DOENVSUBST, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW,
    GetCursorInfo, GetCursorPos, GetForegroundWindow, GetIconInfo, GetSystemMetrics, GetWindow,
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, HICON, ICONINFO, IsIconic,
    IsWindowVisible, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_RESTORE,
    SW_SHOWNORMAL, SetCursorPos, SetForegroundWindow, ShowWindow, WHEEL_DELTA, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, Interface, PCWSTR, PWSTR, w};

use crate::util::{Res, wide};

/// Modele giden görüntünün uzun kenarı (px).
const MAX_EDGE: u32 = 1366;
/// JPEG kalitesi (0..1).
const JPEG_QUALITY: f32 = 0.8;
/// Komut çıktısından modele giden en çok karakter (sondan).
const MAX_OUTPUT_CHARS: usize = 30_000;

/// Ekran görüntüsü: base64 veri ve türü, piksel boyutu.
pub struct Shot {
    pub data: String,
    pub media_type: &'static str,
    pub w: u32,
    pub h: u32,
}

/// Komut sonucu: çıkış kodu (zaman aşımı/durdurmada `None`), birleşik çıktı.
pub struct CmdResult {
    pub code: Option<i32>,
    pub output: String,
    pub timed_out: bool,
}

const MEMBERS: [&str; 17] = [
    "screenshot",
    "zoom",
    "left_click",
    "right_click",
    "middle_click",
    "double_click",
    "triple_click",
    "left_click_drag",
    "mouse_move",
    "left_mouse_down",
    "left_mouse_up",
    "cursor_position",
    "scroll",
    "type",
    "key",
    "hold_key",
    "wait",
];

/// Toolset üyesi mi (screenshot/zoom dahil).
pub fn is_member(name: &str) -> bool {
    MEMBERS.contains(&name)
}

// ---------------------------------------------------------------------------------------------
// Koordinat eşlemesi

/// Görüntü uzayı ⇄ fiziksel ekran: yakalanan monitörün dikdörtgeni ve görüntü boyutu.
#[derive(Clone, Copy, Debug)]
struct Map {
    l: i32,
    t: i32,
    mw: i32,
    mh: i32,
    sw: u32,
    sh: u32,
}

static MAP: Mutex<Option<Map>> = Mutex::new(None);

fn map_lock() -> MutexGuard<'static, Option<Map>> {
    MAP.lock().unwrap_or_else(|e| e.into_inner())
}

/// Uzun kenar MAX_EDGE'i geçmesin, oran korunsun.
fn scaled(w: i32, h: i32) -> (u32, u32) {
    let s = (MAX_EDGE as f64 / w.max(h).max(1) as f64).min(1.0);
    (((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1))
}

/// İmlecin bulunduğu monitör ve onun görüntü boyutu.
fn cursor_monitor() -> Res<Map> {
    unsafe {
        let mut p = POINT::default();
        GetCursorPos(&mut p)?;
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return Err("monitör bilgisi alınamadı".into());
        }
        let r = mi.rcMonitor;
        let (mw, mh) = ((r.right - r.left).max(1), (r.bottom - r.top).max(1));
        let (sw, sh) = scaled(mw, mh);
        Ok(Map { l: r.left, t: r.top, mw, mh, sw, sh })
    }
}

/// Son ekran görüntüsünün eşlemesi; henüz görüntü alınmadıysa imlecin monitörü (ve saklanır ki
/// sonraki eylemler aynı uzayda kalsın).
fn current_map() -> Res<Map> {
    let mut g = map_lock();
    if let Some(m) = *g {
        return Ok(m);
    }
    let m = cursor_monitor()?;
    *g = Some(m);
    Ok(m)
}

impl Map {
    /// Görüntü pikseli → fiziksel ekran pikseli (pikselin ortası eşlenir).
    fn to_screen(&self, x: f64, y: f64) -> Res<(i32, i32)> {
        if !(x >= 0.0 && y >= 0.0 && x <= self.sw as f64 && y <= self.sh as f64) {
            return Err(format!("koordinat ({x}, {y}) görüntünün dışında ({}x{})", self.sw, self.sh).into());
        }
        let px = self.l + ((x + 0.5) * self.mw as f64 / self.sw as f64).floor() as i32;
        let py = self.t + ((y + 0.5) * self.mh as f64 / self.sh as f64).floor() as i32;
        Ok((px.clamp(self.l, self.l + self.mw - 1), py.clamp(self.t, self.t + self.mh - 1)))
    }

    /// Fiziksel piksel → görüntü pikseli (pikselin ortası; to_screen ile gidip gelince aynı kalır).
    fn from_screen(&self, px: i32, py: i32) -> (i64, i64) {
        let x = ((px - self.l) as f64 + 0.5) * self.sw as f64 / self.mw as f64;
        let y = ((py - self.t) as f64 + 0.5) * self.sh as f64 / self.mh as f64;
        let (x, y) = (x.floor() as i64, y.floor() as i64);
        (x, y)
    }
}

/// İş parçacığını geçici olarak monitör başına DPI farkında v2'ye alır: süreç ayarı ne olursa
/// olsun koordinatlar fiziksel pikseldir.
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

/// COM'u bu iş parçacığında açar; başarılıysa kapanışta dengeler.
struct Com(bool);

impl Com {
    fn init(mode: COINIT) -> Self {
        Self(unsafe { CoInitializeEx(None, mode) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Ekran görüntüsü

/// Ekranın fiziksel dikdörtgenini BGRX olarak yakalar; görünür imleç üstüne çizilir. Yakalamadan
/// dışlanan pencereler (WDA_EXCLUDEFROMCAPTURE, engram'ın katmanı) görünmez.
fn grab(l: i32, t: i32, w: i32, h: i32) -> Res<Vec<u8>> {
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err("ekran DC'si alınamadı".into());
        }
        let mem = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // yukarıdan aşağı
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let r = (|| -> Res<Vec<u8>> {
            let bmp = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old = SelectObject(mem, HGDIOBJ(bmp.0));
            let r = (|| -> Res<Vec<u8>> {
                BitBlt(mem, 0, 0, w, h, Some(screen), l, t, SRCCOPY | CAPTUREBLT)?;
                draw_cursor(mem, l, t);
                let _ = GdiFlush();
                if bits.is_null() {
                    return Err("bit eşlem boş".into());
                }
                Ok(std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec())
            })();
            SelectObject(mem, old);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            r
        })();
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        r
    }
}

/// Görünür imleci (sıcak noktası hizalı) DC'ye çizer.
unsafe fn draw_cursor(dc: HDC, l: i32, t: i32) {
    unsafe {
        let mut ci = CURSORINFO { cbSize: size_of::<CURSORINFO>() as u32, ..Default::default() };
        if GetCursorInfo(&mut ci).is_err() || ci.flags.0 & CURSOR_SHOWING.0 == 0 || ci.hCursor.is_invalid() {
            return;
        }
        let icon = HICON(ci.hCursor.0);
        let mut ii = ICONINFO::default();
        let (hx, hy) = if GetIconInfo(icon, &mut ii).is_ok() {
            if !ii.hbmMask.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(ii.hbmMask.0));
            }
            if !ii.hbmColor.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(ii.hbmColor.0));
            }
            (ii.xHotspot as i32, ii.yHotspot as i32)
        } else {
            (0, 0)
        };
        let _ = DrawIconEx(dc, ci.ptScreenPos.x - hx - l, ci.ptScreenPos.y - hy - t, icon, 0, 0, 0, None, DI_NORMAL);
    }
}

/// BGRX pikselleri (w×h) ow×oh'a ölçekleyip JPEG'e çevirir (WIC).
fn encode_jpeg(px: &[u8], w: u32, h: u32, ow: u32, oh: u32) -> Res<Vec<u8>> {
    let _com = Com::init(COINIT_MULTITHREADED);
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let bmp = wic.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat32bppBGR, w * 4, px)?;
        let mut src: IWICBitmapSource = bmp.cast()?;
        if (ow, oh) != (w, h) {
            let sc = wic.CreateBitmapScaler()?;
            if sc.Initialize(&src, ow, oh, WICBitmapInterpolationModeHighQualityCubic).is_err() {
                sc.Initialize(&src, ow, oh, WICBitmapInterpolationModeFant)?;
            }
            src = sc.cast()?;
        }
        // JPEG kodlayıcı 24 bit BGR ister.
        let conv = wic.CreateFormatConverter()?;
        conv.Initialize(&src, &GUID_WICPixelFormat24bppBGR, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true)?;
        let enc = wic.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let (mut frame, mut bag) = (None, None);
        enc.CreateNewFrame(&mut frame, &mut bag)?;
        let frame = frame.ok_or("JPEG karesi oluşturulamadı")?;
        if let Some(bag) = &bag {
            let mut name = wide("ImageQuality");
            let opt = PROPBAG2 { pstrName: PWSTR(name.as_mut_ptr()), ..Default::default() };
            let mut v = VARIANT::default();
            (*v.Anonymous.Anonymous).vt = VT_R4;
            (*v.Anonymous.Anonymous).Anonymous.fltVal = JPEG_QUALITY;
            let _ = bag.Write(1, &opt, &v);
        }
        frame.Initialize(bag.as_ref())?;
        frame.SetSize(ow, oh)?;
        let mut fmt = GUID_WICPixelFormat24bppBGR;
        frame.SetPixelFormat(&mut fmt)?;
        frame.WriteSource(&conv, std::ptr::null())?;
        frame.Commit()?;
        enc.Commit()?;
        let mut st = STATSTG::default();
        stream.Stat(&mut st, STATFLAG_NONAME)?;
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut out = vec![0u8; st.cbSize as usize];
        let mut read = 0u32;
        stream.Read(out.as_mut_ptr().cast(), out.len() as u32, Some(&mut read)).ok()?;
        out.truncate(read as usize);
        Ok(out)
    }
}

/// İmlecin bulunduğu monitörün görüntüsü (imleç çizili, uzun kenar ≤ 1366, JPEG). Eşleme
/// saklanır: sonraki koordinatlar bu görüntünün uzayındadır. Ön plan penceresinin değil imlecin
/// monitörü seçilir: kullanıcının baktığı yer çoğunlukla orasıdır ve Claude imleci taşıyarak
/// monitör değiştirebilir.
pub fn screenshot() -> Res<Shot> {
    let _dpi = Dpi::set();
    let m = cursor_monitor()?;
    let px = grab(m.l, m.t, m.mw, m.mh)?;
    let jpg = encode_jpeg(&px, m.mw as u32, m.mh as u32, m.sw, m.sh)?;
    *map_lock() = Some(m);
    Ok(Shot { data: crate::config::base64(&jpg), media_type: "image/jpeg", w: m.sw, h: m.sh })
}

/// Görüntü uzayındaki [x0, y0, x1, y1] bölgesini doğal çözünürlükte yakalar, oranı korunarak
/// normal ekran görüntüsü boyutuna sığdırır.
pub fn zoom(region: [i32; 4]) -> Res<Shot> {
    let _dpi = Dpi::set();
    let m = current_map()?;
    let (x0, x1) = (region[0].min(region[2]), region[0].max(region[2]));
    let (y0, y1) = (region[1].min(region[3]), region[1].max(region[3]));
    if x0 < 0 || y0 < 0 || x1 > m.sw as i32 || y1 > m.sh as i32 || x1 == x0 || y1 == y0 {
        return Err(format!("geçersiz bölge {region:?} (görüntü {}x{})", m.sw, m.sh).into());
    }
    let fx = m.mw as f64 / m.sw as f64;
    let fy = m.mh as f64 / m.sh as f64;
    let l = m.l + (x0 as f64 * fx).floor() as i32;
    let t = m.t + (y0 as f64 * fy).floor() as i32;
    let r = (m.l + (x1 as f64 * fx).ceil() as i32).min(m.l + m.mw);
    let b = (m.t + (y1 as f64 * fy).ceil() as i32).min(m.t + m.mh);
    let (w, h) = ((r - l).max(1), (b - t).max(1));
    let s = (m.sw as f64 / w as f64).min(m.sh as f64 / h as f64);
    let (ow, oh) = (((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1));
    let px = grab(l, t, w, h)?;
    let jpg = encode_jpeg(&px, w as u32, h as u32, ow, oh)?;
    Ok(Shot { data: crate::config::base64(&jpg), media_type: "image/jpeg", w: ow, h: oh })
}

// ---------------------------------------------------------------------------------------------
// Girdi: fare ve klavye

fn send(inputs: &[INPUT]) -> Res<()> {
    let n = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if n as usize != inputs.len() {
        return Err("girdi gönderilemedi (yönetici olarak çalışan bir pencere engelliyor olabilir)".into());
    }
    Ok(())
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

fn key_input(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn ms(n: u64) {
    sleep(Duration::from_millis(n));
}

/// `stop` kalkmışsa durur.
fn check(stop: &AtomicBool) -> Res<()> {
    if stop.load(Ordering::Relaxed) { Err("durduruldu".into()) } else { Ok(()) }
}

/// Saniye kadar bekler; arada `stop`a bakar.
fn wait_checked(secs: f64, stop: &AtomicBool) -> Res<()> {
    let end = Instant::now() + Duration::from_secs_f64(secs.clamp(0.0, 300.0));
    loop {
        check(stop)?;
        let now = Instant::now();
        if now >= end {
            return Ok(());
        }
        sleep((end - now).min(Duration::from_millis(100)));
    }
}

/// İmleci fiziksel (sanal masaüstü) piksele taşır: önce gerçek bir mutlak hareket olayı,
/// tutmazsa SetCursorPos.
fn move_to(x: i32, y: i32) -> Res<()> {
    unsafe {
        let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1) as i64;
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1) as i64;
        let nx = (((x - vx) as i64 * 65536 + vw - 1) / vw).clamp(0, 65535) as i32;
        let ny = (((y - vy) as i64 * 65536 + vh - 1) / vh).clamp(0, 65535) as i32;
        send(&[mouse_input(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, nx, ny, 0)])?;
        let mut p = POINT::default();
        if GetCursorPos(&mut p).is_ok() && (p.x, p.y) != (x, y) {
            let _ = SetCursorPos(x, y);
        }
    }
    Ok(())
}

fn cursor() -> Res<(i32, i32)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p)? };
    Ok((p.x, p.y))
}

#[derive(Clone, Copy)]
enum Button {
    Left,
    Right,
    Middle,
}

impl Button {
    fn flags(self) -> (MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS) {
        match self {
            Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
            Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
            Button::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        }
    }
}

/// Yerinde `count` kez tıklar; çoklu tıklar çift tıklama süresi içinde kalır.
fn click(b: Button, count: u32) -> Res<()> {
    let (down, up) = b.flags();
    let gap = (unsafe { GetDoubleClickTime() } as u64 / 4).clamp(20, 80);
    for i in 0..count {
        send(&[mouse_input(down, 0, 0, 0)])?;
        ms(30);
        send(&[mouse_input(up, 0, 0, 0)])?;
        if i + 1 < count {
            ms(gap);
        }
    }
    Ok(())
}

/// Bir tuş: sanal tuş (genişletilmiş mi) ya da Unicode karakter (UTF-16 birimi).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Key {
    Vk(u16, bool),
    Unit(u16),
}

impl Key {
    fn input(self, up: bool) -> INPUT {
        let upf = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
        match self {
            Key::Vk(vk, ext) => {
                let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
                let extf = if ext { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) };
                key_input(vk, scan, upf | extf)
            }
            Key::Unit(u) => key_input(0, u, KEYEVENTF_UNICODE | upf),
        }
    }
}

/// Basılı tutulan tuşlar; düşerken (hata, durdurma dahil) ters sırayla bırakılır.
struct Held(Vec<Key>);

impl Held {
    fn press(keys: &[Key]) -> Res<Held> {
        let mut h = Held(Vec::new());
        for &k in keys {
            send(&[k.input(false)])?;
            h.0.push(k);
            ms(10);
        }
        Ok(h)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        for k in self.0.iter().rev() {
            let _ = send(&[k.input(true)]);
            ms(5);
        }
    }
}

/// Değiştirici adı → tuş.
fn modifier(n: &str) -> Option<Key> {
    Some(match n {
        "ctrl" | "control" | "control_l" | "ctrl_l" | "lctrl" => Key::Vk(VK_LCONTROL.0, false),
        "control_r" | "ctrl_r" | "rctrl" => Key::Vk(VK_RCONTROL.0, true),
        "shift" | "shift_l" | "lshift" => Key::Vk(VK_LSHIFT.0, false),
        "shift_r" | "rshift" => Key::Vk(VK_RSHIFT.0, false),
        "alt" | "alt_l" | "lalt" | "meta" | "meta_l" | "option" => Key::Vk(VK_LMENU.0, false),
        "alt_r" | "ralt" | "meta_r" | "altgr" | "iso_level3_shift" => Key::Vk(VK_RMENU.0, true),
        "super" | "super_l" | "win" | "windows" | "cmd" | "command" | "lwin" | "hyper" | "hyper_l" => {
            Key::Vk(VK_LWIN.0, true)
        }
        "super_r" | "rwin" | "hyper_r" => Key::Vk(VK_RWIN.0, true),
        _ => return None,
    })
}

/// xdotool tarzı adlar → (sanal tuş, genişletilmiş).
fn named(n: &str) -> Option<(u16, bool)> {
    let e = |v: VIRTUAL_KEY| Some((v.0, true));
    let p = |v: VIRTUAL_KEY| Some((v.0, false));
    match n {
        "return" | "enter" | "ret" => p(VK_RETURN),
        "kp_enter" => e(VK_RETURN),
        "escape" | "esc" => p(VK_ESCAPE),
        "tab" | "iso_left_tab" => p(VK_TAB),
        "backspace" | "back_space" | "back" | "bksp" => p(VK_BACK),
        "delete" | "del" | "kp_delete" => e(VK_DELETE),
        "insert" | "ins" | "kp_insert" => e(VK_INSERT),
        "home" | "kp_home" => e(VK_HOME),
        "end" | "kp_end" => e(VK_END),
        "page_up" | "pageup" | "prior" | "pgup" | "kp_page_up" | "kp_prior" => e(VK_PRIOR),
        "page_down" | "pagedown" | "next" | "pgdn" | "kp_page_down" | "kp_next" => e(VK_NEXT),
        "up" | "kp_up" => e(VK_UP),
        "down" | "kp_down" => e(VK_DOWN),
        "left" | "kp_left" => e(VK_LEFT),
        "right" | "kp_right" => e(VK_RIGHT),
        "space" | "spacebar" => p(VK_SPACE),
        "print" | "printscreen" | "print_screen" | "prtsc" | "prtscn" | "sys_req" | "snapshot" => e(VK_SNAPSHOT),
        "pause" | "break" => p(VK_PAUSE),
        "caps_lock" | "capslock" => p(VK_CAPITAL),
        "num_lock" | "numlock" => e(VK_NUMLOCK),
        "scroll_lock" | "scrolllock" => p(VK_SCROLL),
        "menu" | "apps" | "context_menu" => e(VK_APPS),
        "kp_add" | "kp_plus" => p(VK_ADD),
        "kp_subtract" | "kp_minus" => p(VK_SUBTRACT),
        "kp_multiply" => p(VK_MULTIPLY),
        "kp_divide" => e(VK_DIVIDE),
        "kp_decimal" => p(VK_DECIMAL),
        "kp_separator" => p(VK_SEPARATOR),
        "xf86audiomute" | "audiomute" | "volumemute" | "mute" => e(VK_VOLUME_MUTE),
        "xf86audioraisevolume" | "volumeup" | "volume_up" => e(VK_VOLUME_UP),
        "xf86audiolowervolume" | "volumedown" | "volume_down" => e(VK_VOLUME_DOWN),
        "xf86audioplay" | "xf86audiopause" | "play_pause" | "playpause" | "mediaplaypause" => e(VK_MEDIA_PLAY_PAUSE),
        "xf86audionext" | "medianext" | "media_next" => e(VK_MEDIA_NEXT_TRACK),
        "xf86audioprev" | "mediaprev" | "media_prev" | "mediaprevious" => e(VK_MEDIA_PREV_TRACK),
        "xf86audiostop" | "mediastop" | "media_stop" => e(VK_MEDIA_STOP),
        "xf86back" | "browser_back" => e(VK_BROWSER_BACK),
        "xf86forward" | "browser_forward" => e(VK_BROWSER_FORWARD),
        "xf86reload" | "xf86refresh" | "browser_refresh" => e(VK_BROWSER_REFRESH),
        "xf86search" | "browser_search" => e(VK_BROWSER_SEARCH),
        "xf86homepage" | "browser_home" => e(VK_BROWSER_HOME),
        _ => {
            if let Some(d) = n.strip_prefix("kp_").and_then(|d| d.parse::<u16>().ok()).filter(|d| *d <= 9) {
                return p(VIRTUAL_KEY(VK_NUMPAD0.0 + d));
            }
            if let Some(f) = n.strip_prefix('f').and_then(|d| d.parse::<u16>().ok()).filter(|f| (1..=24).contains(f)) {
                return p(VIRTUAL_KEY(VK_F1.0 + f - 1));
            }
            None
        }
    }
}

/// X11 keysym adı → karakter (noktalama).
fn symbol(n: &str) -> Option<char> {
    Some(match n {
        "minus" => '-',
        "plus" => '+',
        "equal" => '=',
        "comma" => ',',
        "period" | "dot" => '.',
        "slash" => '/',
        "backslash" => '\\',
        "semicolon" => ';',
        "colon" => ':',
        "apostrophe" | "quoteright" => '\'',
        "quotedbl" => '"',
        "grave" | "quoteleft" | "dead_grave" => '`',
        "asciitilde" | "tilde" => '~',
        "exclam" => '!',
        "at" => '@',
        "numbersign" => '#',
        "dollar" => '$',
        "percent" => '%',
        "asciicircum" => '^',
        "ampersand" => '&',
        "asterisk" => '*',
        "parenleft" => '(',
        "parenright" => ')',
        "bracketleft" => '[',
        "bracketright" => ']',
        "braceleft" => '{',
        "braceright" => '}',
        "bar" => '|',
        "less" => '<',
        "greater" => '>',
        "question" => '?',
        "underscore" => '_',
        _ => return None,
    })
}

/// Düzende karakteri tek başına ya da yalnız Shift ile üreten sanal tuş. ToUnicodeEx'in 0x4
/// bayrağı klavye durumunu (ölü tuşları) bozmaz.
fn plain_key(unit: u16, hkl: HKL) -> Option<(u16, bool)> {
    let mut state = [0u8; 256];
    let mut out = [0u16; 4];
    for shift in [false, true] {
        state[VK_SHIFT.0 as usize] = if shift { 0x80 } else { 0 };
        for vk in 0x20u16..0xE3 {
            if (0x60..=0x6F).contains(&vk) || (0x70..=0x87).contains(&vk) || hotkey_like(vk) {
                continue; // sayısal tuş takımı, F tuşları, değiştiriciler
            }
            let n = unsafe {
                let scan = MapVirtualKeyExW(vk as u32, MAPVK_VK_TO_VSC, Some(hkl));
                if scan == 0 {
                    continue;
                }
                ToUnicodeEx(vk as u32, scan, &state, &mut out, 0x4, Some(hkl))
            };
            if n == 1 && out[0] == unit {
                return Some((vk, shift));
            }
        }
    }
    None
}

fn hotkey_like(vk: u16) -> bool {
    crate::hotkey::is_modifier(vk) || matches!(VIRTUAL_KEY(vk), VK_APPS | VK_SLEEP)
}

/// Ön plandaki pencerenin klavye düzeninde karakteri üreten tuş ve gereken değiştiriciler.
fn char_key(c: char, shortcut: bool) -> Vec<Key> {
    // Kısayolda (ctrl/alt/super ile) harfler düzenden bağımsız sanal tuşlardır: ctrl+i = VK_I.
    if shortcut && c.is_ascii_alphanumeric() {
        return vec![Key::Vk(c.to_ascii_uppercase() as u16, false)];
    }
    // Bu dört tuşun sanal kodu her düzende aynıdır; kısayollar (ctrl+plus) onları bekler.
    if shortcut {
        let oem = match c {
            '+' => Some(VK_OEM_PLUS),
            '-' => Some(VK_OEM_MINUS),
            ',' => Some(VK_OEM_COMMA),
            '.' => Some(VK_OEM_PERIOD),
            _ => None,
        };
        if let Some(v) = oem {
            return vec![Key::Vk(v.0, false)];
        }
    }
    let mut buf = [0u16; 2];
    let units = c.encode_utf16(&mut buf);
    if units.len() == 1 {
        let hkl = unsafe { GetKeyboardLayout(GetWindowThreadProcessId(GetForegroundWindow(), None)) };
        // Önce AltGr'siz bir tuş ara (Türkçe Q'da "i" AltGr+I değil, kendi tuşu).
        if let Some((vk, shift)) = plain_key(units[0], hkl) {
            let mut v = Vec::new();
            if shift {
                v.push(Key::Vk(VK_LSHIFT.0, false));
            }
            v.push(Key::Vk(vk, false));
            return v;
        }
        let r = unsafe { VkKeyScanExW(units[0], hkl) };
        if r != -1 {
            let (vk, st) = ((r as u16) & 0xFF, ((r as u16) >> 8) & 0xFF);
            // 6 = AltGr (ctrl+alt); başka bit varsa Unicode'a düşülür.
            if st & !0x7 == 0 {
                let mut v = Vec::new();
                if st & 2 != 0 {
                    v.push(Key::Vk(VK_LCONTROL.0, false));
                }
                if st & 4 != 0 {
                    v.push(Key::Vk(VK_LMENU.0, false));
                }
                if st & 1 != 0 {
                    v.push(Key::Vk(VK_LSHIFT.0, false));
                }
                v.push(Key::Vk(vk, false));
                return v;
            }
        }
    }
    if c.is_ascii_alphanumeric() {
        return vec![Key::Vk(c.to_ascii_uppercase() as u16, false)];
    }
    units.iter().map(|&u| Key::Unit(u)).collect()
}

/// "ctrl+shift+Escape" gibi tek bir birleşimi basılacak tuş sırasına çevirir.
fn parse_combo(s: &str) -> Res<Vec<Key>> {
    // '+' ayırıcıdır; boş parçadan sonra gelirse tuşun kendisidir ("ctrl++", "+").
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if c == '+' && !cur.trim().is_empty() {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    let parts: Vec<String> =
        parts.into_iter().map(|p| if p.trim().is_empty() { p } else { p.trim().to_string() }).collect();
    let Some((last, mods)) = parts.split_last() else { return Err("boş tuş".into()) };
    let mut keys: Vec<Key> = Vec::new();
    let mut shortcut = false;
    for m in mods {
        let k = modifier(&m.to_lowercase()).ok_or_else(|| format!("bilinmeyen değiştirici: {m}"))?;
        if !matches!(k, Key::Vk(v, _) if v == VK_LSHIFT.0 || v == VK_RSHIFT.0) {
            shortcut = true;
        }
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    let lower = last.to_lowercase();
    let main: Vec<Key> = if let Some(k) = modifier(&lower) {
        vec![k]
    } else if let Some((vk, ext)) = named(&lower) {
        vec![Key::Vk(vk, ext)]
    } else {
        let mut chars = last.chars();
        let c = match (chars.next(), chars.next()) {
            // Tek ASCII karakterde büyük/küçük harf fark etmez ("A" = "a").
            (Some(c), None) => {
                if c.is_ascii() {
                    c.to_ascii_lowercase()
                } else {
                    c
                }
            }
            _ => symbol(&lower).ok_or_else(|| format!("bilinmeyen tuş: {last}"))?,
        };
        char_key(c, shortcut)
    };
    for k in main {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    Ok(keys)
}

/// Boşlukla ayrılmış birleşim dizisi ("ctrl+a Delete").
fn parse_keys(text: &str) -> Res<Vec<Vec<Key>>> {
    let t = text.trim();
    if t.is_empty() {
        // Tek boşluk karakteri de bir tuştur.
        return if text.contains(' ') { Ok(vec![vec![Key::Vk(VK_SPACE.0, false)]]) } else { Err("tuş verilmedi".into()) };
    }
    t.split_whitespace().map(parse_combo).collect()
}

/// Tuş adının neye çevrildiğini okunur yazar (tanı için).
pub fn describe_keys(text: &str) -> String {
    match parse_keys(text) {
        Ok(seq) => seq
            .iter()
            .map(|c| {
                c.iter()
                    .map(|k| match k {
                        Key::Vk(v, true) => format!("VK{v:02X}(ext)"),
                        Key::Vk(v, false) => format!("VK{v:02X}"),
                        Key::Unit(u) => format!("U+{u:04X}"),
                    })
                    .collect::<Vec<_>>()
                    .join("+")
            })
            .collect::<Vec<_>>()
            .join(" "),
        Err(e) => format!("hata: {e}"),
    }
}

/// Birleşimi bir kez basıp bırakır.
fn tap(keys: &[Key]) -> Res<()> {
    let h = Held::press(keys)?;
    ms(20);
    drop(h);
    Ok(())
}

/// Değiştirici listesi ("shift", "ctrl+shift").
fn parse_mods(input: &Value) -> Res<Vec<Key>> {
    let Some(t) = input.get("text").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()) else {
        return Ok(Vec::new());
    };
    let mut keys = Vec::new();
    for part in t.split(['+', ' ']).filter(|p| !p.is_empty()) {
        let k = modifier(&part.to_lowercase()).ok_or_else(|| format!("bilinmeyen değiştirici: {part}"))?;
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    Ok(keys)
}

/// Metni klavye odağına yazar: Unicode olaylarıyla (Türkçe harfler, emoji), satır sonu Enter,
/// sekme Tab. Uygulamalar yetişsin diye küçük parçalar halinde.
fn type_text(text: &str, stop: &AtomicBool) -> Res<()> {
    const CHUNK: usize = 6;
    let mut batch: Vec<INPUT> = Vec::new();
    let mut n = 0;
    let mut chars = text.chars().peekable();
    let flush = |batch: &mut Vec<INPUT>| -> Res<()> {
        if !batch.is_empty() {
            send(batch)?;
            batch.clear();
        }
        Ok(())
    };
    while let Some(c) = chars.next() {
        match c {
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                let k = Key::Vk(VK_RETURN.0, false);
                batch.extend([k.input(false), k.input(true)]);
                flush(&mut batch)?;
                ms(40);
                n = 0;
                check(stop)?;
                continue;
            }
            '\t' => {
                let k = Key::Vk(VK_TAB.0, false);
                batch.extend([k.input(false), k.input(true)]);
            }
            _ => {
                let mut buf = [0u16; 2];
                for &u in c.encode_utf16(&mut buf).iter() {
                    batch.extend([Key::Unit(u).input(false), Key::Unit(u).input(true)]);
                }
            }
        }
        n += 1;
        if n >= CHUNK {
            flush(&mut batch)?;
            n = 0;
            ms(15);
            check(stop)?;
        }
    }
    flush(&mut batch)
}

fn coord(input: &Value, key: &str) -> Res<Option<(f64, f64)>> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) if a.len() == 2 => match (a[0].as_f64(), a[1].as_f64()) {
            (Some(x), Some(y)) => Ok(Some((x, y))),
            _ => Err(format!("{key} sayı olmalı").into()),
        },
        Some(v) => Err(format!("{key} [x, y] olmalı: {v}").into()),
    }
}

fn need_coord(input: &Value, key: &str) -> Res<(f64, f64)> {
    coord(input, key)?.ok_or_else(|| format!("{key} eksik").into())
}

fn duration(input: &Value) -> Res<f64> {
    let d = input.get("duration").and_then(Value::as_f64).ok_or("duration eksik")?;
    if !(d >= 0.0) {
        return Err("duration geçersiz".into());
    }
    Ok(d.min(300.0))
}

fn text_of(input: &Value) -> Res<&str> {
    input.get("text").and_then(Value::as_str).ok_or_else(|| "text eksik".into())
}

/// Varsa `coordinate`a gider.
fn move_opt(input: &Value) -> Res<()> {
    if let Some((x, y)) = coord(input, "coordinate")? {
        let (px, py) = current_map()?.to_screen(x, y)?;
        move_to(px, py)?;
        ms(50);
    }
    Ok(())
}

/// Görüntü dışındaki üyeleri çalıştırır; modeli bilgilendiren kısa metin döner ("OK" ya da
/// imleç konumu). Uzun eylemlerde (yazma, bekleme, tekrar, tuş tutma) adımlar arasında `stop`a
/// bakılır; kalkmışsa "durduruldu" hatası döner. Basılan tuşlar her yolda bırakılır.
pub fn act(name: &str, input: &Value, stop: &AtomicBool) -> Res<String> {
    let _dpi = Dpi::set();
    check(stop)?;
    let ok = || Ok("OK".to_string());
    match name {
        "left_click" | "right_click" | "middle_click" | "double_click" | "triple_click" => {
            let (b, n) = match name {
                "right_click" => (Button::Right, 1),
                "middle_click" => (Button::Middle, 1),
                "double_click" => (Button::Left, 2),
                "triple_click" => (Button::Left, 3),
                _ => (Button::Left, 1),
            };
            let mods = parse_mods(input)?;
            move_opt(input)?;
            let _held = Held::press(&mods)?;
            click(b, n)?;
            ok()
        }
        "left_click_drag" => {
            let m = current_map()?;
            let (x0, y0) = need_coord(input, "start_coordinate")?;
            let (x1, y1) = need_coord(input, "coordinate")?;
            let ((sx, sy), (ex, ey)) = (m.to_screen(x0, y0)?, m.to_screen(x1, y1)?);
            let mods = parse_mods(input)?;
            move_to(sx, sy)?;
            ms(60);
            let _held = Held::press(&mods)?;
            send(&[mouse_input(MOUSEEVENTF_LEFTDOWN, 0, 0, 0)])?;
            // Bırakma her yolda: ara adımlar hata verse de düğme basılı kalmasın.
            let r = (|| -> Res<()> {
                ms(80);
                const STEPS: i32 = 12;
                for i in 1..=STEPS {
                    move_to(sx + (ex - sx) * i / STEPS, sy + (ey - sy) * i / STEPS)?;
                    ms(15);
                }
                ms(80);
                Ok(())
            })();
            let up = send(&[mouse_input(MOUSEEVENTF_LEFTUP, 0, 0, 0)]);
            r?;
            up?;
            ok()
        }
        "mouse_move" => {
            let (x, y) = need_coord(input, "coordinate")?;
            let (px, py) = current_map()?.to_screen(x, y)?;
            move_to(px, py)?;
            ok()
        }
        "left_mouse_down" => {
            move_opt(input)?;
            send(&[mouse_input(MOUSEEVENTF_LEFTDOWN, 0, 0, 0)])?;
            ok()
        }
        "left_mouse_up" => {
            move_opt(input)?;
            send(&[mouse_input(MOUSEEVENTF_LEFTUP, 0, 0, 0)])?;
            ok()
        }
        "cursor_position" => {
            let m = current_map()?;
            let (px, py) = cursor()?;
            let (x, y) = m.from_screen(px, py);
            let outside = x < 0 || y < 0 || x >= m.sw as i64 || y >= m.sh as i64;
            Ok(if outside { format!("[{x}, {y}] (imleç görüntülenen monitörün dışında)") } else { format!("[{x}, {y}]") })
        }
        "scroll" => {
            let dir = input.get("scroll_direction").and_then(Value::as_str).unwrap_or("down").to_lowercase();
            let amount =
                input.get("scroll_amount").and_then(Value::as_f64).unwrap_or(3.0).round().clamp(1.0, 100.0) as u32;
            let (flag, delta) = match dir.as_str() {
                "up" => (MOUSEEVENTF_WHEEL, WHEEL_DELTA as i32),
                "down" => (MOUSEEVENTF_WHEEL, -(WHEEL_DELTA as i32)),
                "left" => (MOUSEEVENTF_HWHEEL, -(WHEEL_DELTA as i32)),
                "right" => (MOUSEEVENTF_HWHEEL, WHEEL_DELTA as i32),
                d => return Err(format!("bilinmeyen kaydırma yönü: {d}").into()),
            };
            let mods = parse_mods(input)?;
            move_opt(input)?;
            let _held = Held::press(&mods)?;
            for _ in 0..amount {
                send(&[mouse_input(flag, 0, 0, delta)])?;
                ms(30);
                check(stop)?;
            }
            ok()
        }
        "type" => {
            type_text(text_of(input)?, stop)?;
            ok()
        }
        "key" => {
            let seq = parse_keys(text_of(input)?)?;
            let repeat = input.get("repeat").and_then(Value::as_f64).unwrap_or(1.0).round().clamp(1.0, 100.0) as u32;
            for i in 0..repeat {
                for (j, combo) in seq.iter().enumerate() {
                    if j > 0 {
                        ms(30);
                    }
                    tap(combo)?;
                }
                if i + 1 < repeat {
                    ms(30);
                    check(stop)?;
                }
            }
            ok()
        }
        "hold_key" => {
            let keys: Vec<Key> = parse_keys(text_of(input)?)?.concat();
            let d = duration(input)?;
            let _held = Held::press(&keys)?;
            wait_checked(d, stop)?;
            ok()
        }
        "wait" => {
            wait_checked(duration(input)?, stop)?;
            ok()
        }
        "screenshot" | "zoom" => Err(format!("{name} görüntü döndürür; screenshot()/zoom() ile çağrılmalı").into()),
        _ => Err(format!("bilinmeyen eylem: {name}").into()),
    }
}

// ---------------------------------------------------------------------------------------------
// Komutlar ve terminal

/// Bir argümanı CommandLineToArgvW kurallarına göre tırnaklar.
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.into();
    }
    let mut s = String::from('"');
    let mut bs = 0;
    for c in arg.chars() {
        match c {
            '\\' => bs += 1,
            '"' => {
                s.push_str(&"\\".repeat(bs * 2 + 1));
                s.push('"');
                bs = 0;
            }
            _ => {
                s.push_str(&"\\".repeat(bs));
                s.push(c);
                bs = 0;
            }
        }
    }
    s.push_str(&"\\".repeat(bs * 2));
    s.push('"');
    s
}

fn system32() -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    std::path::Path::new(&root).join("System32")
}

/// Dosya var mı (uygulama yürütme takma adları, ör. WindowsApps\wt.exe, okunamayan yeniden
/// ayrıştırma noktalarıdır; onlar da sayılır).
fn exists(p: &std::path::Path) -> bool {
    p.is_file() || std::fs::symlink_metadata(p).is_ok()
}

/// PATH'te (ve verilen ek yerlerde) bir exe arar.
fn find_exe(name: &str, extra: &[std::path::PathBuf]) -> Option<String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .chain(extra.iter().cloned())
        .find(|p| exists(p))
        .map(|p| p.display().to_string())
}

fn local_appdata() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA").map(Into::into).unwrap_or_else(std::env::temp_dir)
}

fn pwsh() -> Option<String> {
    let pf = std::env::var_os("ProgramFiles").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Program Files".into());
    find_exe(
        "pwsh.exe",
        &[pf.join("PowerShell").join("7").join("pwsh.exe"), local_appdata().join("Microsoft\\WindowsApps\\pwsh.exe")],
    )
}

/// PowerShell -EncodedCommand değeri: UTF-16LE'nin base64'ü (tırnaklama derdi yok).
fn ps_encoded(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    crate::config::base64(&bytes)
}

/// Kabuğa göre süreç: exe, komut satırı ve eklenecek ortam değişkenleri.
struct ShellCmd {
    exe: String,
    line: String,
    env: Vec<(&'static str, String)>,
}

/// Kabuğa göre komut. `interactive`: görünür terminalde açık kalacak biçim.
fn shell_line(shell: &str, command: &str, interactive: bool) -> Res<ShellCmd> {
    let sys = system32();
    let ps = |exe: String| -> Res<ShellCmd> {
        if interactive {
            let line = format!("{} -NoExit -ExecutionPolicy Bypass -EncodedCommand {}", quote(&exe), ps_encoded(command));
            return Ok(ShellCmd { exe, line, env: Vec::new() });
        }
        // Betik ortam değişkeniyle gider: tırnaklama derdi yok ve -EncodedCommand'ın aksine
        // hatalar CLIXML değil düz metin yazılır. Çıkış kodu: son yerel komutunki, yoksa hata
        // yazıldıysa 1 (iex kendisi hep 0 döndürür).
        if command.len() > 30_000 {
            return Err("komut çok uzun (en çok ~30 000 karakter); bir betik dosyasına yazıp onu çalıştır".into());
        }
        let line = format!(
            "{} -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command \"{}\"",
            quote(&exe),
            "[Console]::OutputEncoding=[Text.Encoding]::UTF8;$ProgressPreference='SilentlyContinue';$e0=$Error.Count;\
             iex $env:ENGRAM_PS;if($LASTEXITCODE){exit $LASTEXITCODE};if($Error.Count -gt $e0){exit 1}"
        );
        Ok(ShellCmd { exe, line, env: vec![("ENGRAM_PS", command.to_string())] })
    };
    Ok(match shell.trim().to_lowercase().as_str() {
        "" | "powershell" | "ps" | "windows_powershell" => {
            ps(sys.join("WindowsPowerShell\\v1.0\\powershell.exe").display().to_string())?
        }
        "pwsh" => ps(pwsh().ok_or("pwsh (PowerShell 7) yüklü değil; \"powershell\" kullanılabilir")?)?,
        "cmd" => {
            let exe = sys.join("cmd.exe").display().to_string();
            let line = if interactive {
                format!("{} /d /k {command}", quote(&exe))
            } else {
                format!("{} /d /s /c \"chcp 65001>nul & {command}\"", quote(&exe))
            };
            ShellCmd { exe, line, env: Vec::new() }
        }
        "wsl" | "bash" => {
            let exe = sys.join("wsl.exe").display().to_string();
            let script = if interactive { format!("{command}; exec bash -l") } else { command.to_string() };
            let line = format!("{} -e bash -lc {}", quote(&exe), quote(&script));
            // wsl.exe kendi iletilerini de UTF-8 yazsın.
            ShellCmd { exe, line, env: vec![("WSL_UTF8", "1".into())] }
        }
        s => return Err(format!("bilinmeyen kabuk: {s} (powershell, pwsh, cmd, wsl)").into()),
    })
}

/// Ortam bloğu (UTF-16, çift sıfırla biter): bu sürecinki artı verilen değişkenler.
fn env_block(extra: &[(&str, String)]) -> Vec<u16> {
    let mut vars: Vec<(String, String)> = std::env::vars_os()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
        .filter(|(k, _)| !extra.iter().any(|(e, _)| e.eq_ignore_ascii_case(k)))
        .collect();
    vars.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    vars.sort_by_key(|(k, _)| k.to_uppercase());
    let mut b: Vec<u16> = Vec::new();
    for (k, v) in vars {
        b.extend(format!("{k}={v}").encode_utf16());
        b.push(0);
    }
    b.push(0);
    b
}

fn default_dir(cwd: Option<&str>) -> Res<String> {
    if let Some(d) = cwd.map(str::trim).filter(|d| !d.is_empty()) {
        if !std::path::Path::new(d).is_dir() {
            return Err(format!("klasör yok: {d}").into());
        }
        return Ok(d.to_string());
    }
    Ok(std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into()))
}

/// Çıktı baytlarını metne çevirir: UTF-16LE görünüyorsa (wsl.exe'nin kendi hataları) ondan,
/// değilse UTF-8; geçersizse OEM kod sayfası.
fn decode(b: &[u8]) -> String {
    let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b);
    let pairs = b.len() / 2;
    if pairs >= 2 {
        let odd_zero = b.iter().skip(1).step_by(2).filter(|&&x| x == 0).count();
        let even_zero = b.iter().step_by(2).filter(|&&x| x == 0).count();
        if odd_zero * 10 >= pairs * 4 && even_zero * 10 < pairs {
            let b = b.strip_prefix(&[0xFF, 0xFE]).unwrap_or(b);
            let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            return String::from_utf16_lossy(&units);
        }
    }
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => unsafe {
            use windows::Win32::Globalization::{CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar};
            let n = MultiByteToWideChar(CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), b, None);
            if n <= 0 {
                return String::from_utf8_lossy(b).into_owned();
            }
            let mut w = vec![0u16; n as usize];
            MultiByteToWideChar(CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), b, Some(&mut w));
            String::from_utf16_lossy(&w)
        },
    }
}

/// ANSI kaçışlarını ayıklar, satır sonlarını \n'e indirir; \r ile yeniden yazılan satırların
/// (ilerleme çubukları) yalnız son hali kalır.
fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            match it.peek() {
                Some('[') => {
                    it.next();
                    while let Some(&d) = it.peek() {
                        it.next();
                        if ('@'..='~').contains(&d) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: BEL ya da ESC \ ile biter.
                    while let Some(d) = it.next() {
                        if d == '\x07' || (d == '\x1b' && it.peek() == Some(&'\\')) {
                            it.next();
                            break;
                        }
                    }
                }
                _ => {
                    it.next();
                }
            }
            continue;
        }
        out.push(c);
    }
    out.split('\n')
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            line.rsplit('\r').next().unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Okuyucu iş parçacığının biriktirdiği en çok bayt (sondan tutulur).
const MAX_BYTES: usize = 4 << 20;

/// Komut çalıştırır, çıktıyı toplar. Kabuklar: "powershell" (Windows PowerShell 5, UTF-8 çıktı),
/// "pwsh" (yüklüyse), "cmd" (chcp 65001), "wsl" (varsayılan dağıtımda bash -lc). Pencere
/// açılmaz; stdin NUL'dur (soru soran komutlar beklemek yerine hemen hata verir); stdout ve
/// stderr tek boruda birleşir. Zaman aşımında ya da `stop` kalkınca süreç ağacı (iş nesnesi)
/// öldürülür ve o ana kadarki çıktı notla döner (`timed_out` = true). Normal bitişte komutun
/// başlattığı arka plan uygulamaları (Start-Process notepad gibi) yaşamaya devam eder.
/// Çıktının yalnız son ~30 000 karakteri döner. `cwd` yoksa kullanıcı klasörü.
pub fn run_command(shell: &str, command: &str, cwd: Option<&str>, timeout_s: u64, stop: &AtomicBool) -> Res<CmdResult> {
    let ShellCmd { exe, line, env } = shell_line(shell, command, false)?;
    let dir = default_dir(cwd)?;
    let env = (!env.is_empty()).then(|| env_block(&env));
    unsafe {
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: true.into(),
        };
        let (mut rd, mut wr) = (HANDLE::default(), HANDLE::default());
        CreatePipe(&mut rd, &mut wr, Some(&sa), 0)?;
        let _ = SetHandleInformation(rd, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
        let nul = match CreateFileW(
            w!("NUL"),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(&sa),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                let _ = CloseHandle(rd);
                let _ = CloseHandle(wr);
                return Err(e.into());
            }
        };
        // Çocuk yalnız bu iki tanıtıcıyı devralsın (başka iş parçacıklarının açık boruları değil).
        let handles = [nul, wr];
        let mut size = 0usize;
        let _ = InitializeProcThreadAttributeList(None, 1, None, &mut size);
        let mut attr = vec![0usize; size.div_ceil(size_of::<usize>()).max(1)];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(attr.as_mut_ptr().cast());
        let mut has_list = InitializeProcThreadAttributeList(Some(list), 1, None, &mut size).is_ok();
        if has_list
            && UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(handles.as_ptr().cast()),
                size_of_val(&handles),
                None,
                None,
            )
            .is_err()
        {
            DeleteProcThreadAttributeList(list);
            has_list = false;
        }
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = nul;
        si.StartupInfo.hStdOutput = wr;
        si.StartupInfo.hStdError = wr;
        let mut flags = CREATE_NO_WINDOW | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT;
        if has_list {
            si.lpAttributeList = list;
            flags |= EXTENDED_STARTUPINFO_PRESENT;
        }
        let exe_w = wide(&exe);
        let mut line_w = wide(&line);
        let dir_w = wide(&dir);
        let mut pi = PROCESS_INFORMATION::default();
        let r = CreateProcessW(
            PCWSTR(exe_w.as_ptr()),
            Some(PWSTR(line_w.as_mut_ptr())),
            None,
            None,
            true,
            flags,
            env.as_ref().map(|e| e.as_ptr() as *const core::ffi::c_void),
            PCWSTR(dir_w.as_ptr()),
            &si.StartupInfo,
            &mut pi,
        );
        if has_list {
            DeleteProcThreadAttributeList(list);
        }
        let _ = CloseHandle(wr);
        let _ = CloseHandle(nul);
        if let Err(e) = r {
            let _ = CloseHandle(rd);
            return Err(format!("{shell} başlatılamadı: {}", e.message()).into());
        }

        // İş nesnesi: zaman aşımında bütün ağaç birden ölür.
        let job = CreateJobObjectW(None, PCWSTR::null())
            .ok()
            .filter(|j| set_kill_on_close(*j, true) && AssignProcessToJobObject(*j, pi.hProcess).is_ok());
        ResumeThread(pi.hThread);
        let _ = CloseHandle(pi.hThread);

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (tx, rx) = mpsc::channel::<()>();
        let reader = {
            let buf = buf.clone();
            let rd = rd.0 as usize;
            std::thread::spawn(move || {
                let h = HANDLE(rd as *mut core::ffi::c_void);
                let mut chunk = vec![0u8; 16 * 1024];
                loop {
                    let mut n = 0u32;
                    if ReadFile(h, Some(&mut chunk), Some(&mut n), None).is_err() || n == 0 {
                        break;
                    }
                    let mut g = buf.lock().unwrap_or_else(|e| e.into_inner());
                    g.extend_from_slice(&chunk[..n as usize]);
                    if g.len() > MAX_BYTES {
                        let cut = g.len() - MAX_BYTES;
                        g.drain(..cut);
                    }
                }
                let _ = CloseHandle(h);
                let _ = tx.send(());
            })
        };

        let start = Instant::now();
        let limit = Duration::from_secs(timeout_s.max(1));
        let mut why: Option<&str> = None;
        loop {
            if WaitForSingleObject(pi.hProcess, 50) == WAIT_OBJECT_0 {
                break;
            }
            if stop.load(Ordering::Relaxed) {
                why = Some("durduruldu");
                break;
            }
            if start.elapsed() >= limit {
                why = Some("zaman aşımı");
                break;
            }
        }
        let code = if why.is_some() {
            match job {
                Some(j) => {
                    let _ = TerminateJobObject(j, 1);
                }
                None => {
                    let _ = TerminateProcess(pi.hProcess, 1);
                }
            }
            WaitForSingleObject(pi.hProcess, 2000);
            None
        } else {
            let mut c = 0u32;
            GetExitCodeProcess(pi.hProcess, &mut c).ok().map(|_| c as i32)
        };
        if let Some(j) = job {
            // Normal bitişte geride kalan uygulamalar iş nesnesi kapanınca ölmesin.
            if why.is_none() {
                set_kill_on_close(j, false);
            }
            let _ = CloseHandle(j);
        }
        let _ = CloseHandle(pi.hProcess);
        // Boruyu tutan torunlar olabilir: okuyucuyu biraz bekle, sonra okumasını iptal et.
        if rx.recv_timeout(Duration::from_millis(1500)).is_err() {
            use std::os::windows::io::AsRawHandle;
            let _ = CancelSynchronousIo(HANDLE(reader.as_raw_handle()));
            let _ = rx.recv_timeout(Duration::from_millis(500));
        }

        let bytes = buf.lock().map(|g| g.clone()).unwrap_or_else(|e| e.into_inner().clone());
        let mut out = clean(&decode(&bytes)).trim_end().to_string();
        let n = out.chars().count();
        if n > MAX_OUTPUT_CHARS {
            let skip = out.char_indices().nth(n - MAX_OUTPUT_CHARS).map_or(0, |(i, _)| i);
            out = format!(
                "[… çıktının başı kesildi; toplam {n} karakter, son {MAX_OUTPUT_CHARS}]\n{}",
                out[skip..].trim_start_matches('\n')
            );
        }
        match why {
            Some("durduruldu") => out.push_str("\n[kullanıcı durdurdu; süreç sonlandırıldı]"),
            Some(_) => out.push_str(&format!("\n[zaman aşımı: {} sn içinde bitmedi; süreç sonlandırıldı]", limit.as_secs())),
            None => {}
        }
        Ok(CmdResult { code, output: out.trim_start_matches('\n').to_string(), timed_out: why.is_some() })
    }
}

fn set_kill_on_close(job: HANDLE, on: bool) -> bool {
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = if on { JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE } else { JOB_OBJECT_LIMIT(0) };
    unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .is_ok()
    }
}

/// Tanıtıcı devretmeden süreç başlatır (görünür pencereler için).
fn spawn(exe: &str, line: &str, dir: &str, flags: PROCESS_CREATION_FLAGS) -> Res<()> {
    let exe_w = wide(exe);
    let mut line_w = wide(line);
    let dir_w = wide(dir);
    let si = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(exe_w.as_ptr()),
            Some(PWSTR(line_w.as_mut_ptr())),
            None,
            None,
            false,
            flags,
            None,
            PCWSTR(dir_w.as_ptr()),
            &si,
            &mut pi,
        )
        .map_err(|e| format!("başlatılamadı: {exe}: {}", e.message()))?;
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
    }
    Ok(())
}

/// Komutu görünür bir terminal penceresinde çalıştırır ve hemen döner (kullanıcının görmek
/// istediği etkileşimli işler: ssh oturumu gibi). Windows Terminal varsa onda, yoksa düz konsol
/// penceresinde. Komut bitince kabuk açık kalır.
pub fn open_terminal(shell: &str, command: &str, cwd: Option<&str>) -> Res<()> {
    let ShellCmd { exe, line, .. } = shell_line(shell, command, true)?;
    let dir = default_dir(cwd)?;
    if let Some(wt) = find_exe("wt.exe", &[local_appdata().join("Microsoft\\WindowsApps\\wt.exe")]) {
        // wt ';' ile alt komut ayırır: komuttakiler kaçırılır.
        let full = format!("{} -d {} {}", quote(&wt), quote(&dir), line.replace(';', "\\;"));
        if spawn(&wt, &full, &dir, PROCESS_CREATION_FLAGS(0)).is_ok() {
            return Ok(());
        }
    }
    spawn(&exe, &line, &dir, CREATE_NEW_CONSOLE)
}

/// Dosya, klasör, uygulama ya da adres açar (kabuğun varsayılan eylemiyle, çoğunlukla "open";
/// "notepad", "spotify" gibi adlar App Paths ve PATH üzerinden çözülür, %ORTAM% değişkenleri
/// açılır). Hata penceresi göstermez; açılamazsa açıklayıcı hata döner.
pub fn open(target: &str, args: Option<&str>) -> Res<()> {
    let target = target.trim().trim_matches('"');
    if target.is_empty() {
        return Err("açılacak şey verilmedi".into());
    }
    let _com = Com::init(COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    let t = wide(target);
    let a = args.map(str::trim).filter(|a| !a.is_empty()).map(wide);
    let dir = wide(&std::env::var("USERPROFILE").unwrap_or_default());
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC | SEE_MASK_DOENVSUBST,
        lpFile: PCWSTR(t.as_ptr()),
        lpParameters: a.as_ref().map_or(PCWSTR::null(), |a| PCWSTR(a.as_ptr())),
        lpDirectory: PCWSTR(dir.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    match unsafe { ShellExecuteExW(&mut sei) } {
        Ok(()) => Ok(()),
        Err(e) => {
            let why = match sei.hInstApp.0 as isize {
                2 | 3 => "bulunamadı".to_string(),
                5 => "erişim reddedildi".to_string(),
                31 => "bu tür için ilişkili uygulama yok".to_string(),
                _ => e.message().trim().to_string(),
            };
            Err(format!("açılamadı: {target}: {why}").into())
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Pencereler

struct Win {
    hwnd: HWND,
    title: String,
}

/// Görünür, başlıklı, sahipsiz üst düzey pencereler (Z sırasıyla, en üstteki önce).
fn top_windows() -> Vec<Win> {
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        unsafe {
            let v = &mut *(l.0 as *mut Vec<Win>);
            if let Some(title) = usable(h) {
                v.push(Win { hwnd: h, title });
            }
        }
        true.into()
    }
    let mut v: Vec<Win> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut v as *mut Vec<Win> as isize));
    }
    v
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
        let mut class = [0u16; 64];
        let n = GetClassNameW(h, &mut class);
        if String::from_utf16_lossy(&class[..n.max(0) as usize]) == "Progman" {
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

fn process_name(h: HWND) -> String {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return "?".into() };
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let r = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n);
        let _ = CloseHandle(p);
        if r.is_err() {
            return "?".into();
        }
        let path = String::from_utf16_lossy(&buf[..n as usize]);
        path.rsplit('\\').next().unwrap_or(&path).to_string()
    }
}

/// Açık pencerelerin listesi (görünür, başlıklı üst düzey pencereler): "başlık — süreç.exe"
/// satırları, en üstteki önce; ön plandaki ve simge durumundakiler işaretli.
pub fn windows_list() -> String {
    let fg = unsafe { GetForegroundWindow() };
    let lines: Vec<String> = top_windows()
        .iter()
        .map(|w| {
            let mut s = format!("{} — {}", w.title, process_name(w.hwnd));
            if w.hwnd == fg {
                s.push_str(" (ön planda)");
            }
            if unsafe { IsIconic(w.hwnd) }.as_bool() {
                s.push_str(" (simge durumunda)");
            }
            s
        })
        .collect();
    if lines.is_empty() { "(açık pencere yok)".into() } else { lines.join("\n") }
}

/// Başlığında verilen metni (büyük/küçük harf duyarsız) içeren en üstteki pencereyi öne getirir;
/// simge durumundaysa geri yükler.
pub fn focus_window(title_part: &str) -> Res<String> {
    let needle = title_part.trim().to_lowercase();
    if needle.is_empty() {
        return Err("pencere başlığı verilmedi".into());
    }
    let win = top_windows()
        .into_iter()
        .find(|w| w.title.to_lowercase().contains(&needle))
        .ok_or_else(|| format!("başlığında \"{title_part}\" geçen pencere yok"))?;
    let h = win.hwnd;
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        if !(SetForegroundWindow(h).as_bool() && GetForegroundWindow() == h) {
            // Ön plan kilidi: ön plandaki iş parçacığının girdisine bağlanıp ALT dokunuşuyla
            // "son girdi bizden" koşulunu sağla, sonra yeniden dene.
            let me = GetCurrentThreadId();
            let fg_thread = GetWindowThreadProcessId(GetForegroundWindow(), None);
            let attached = fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, true).as_bool();
            let alt = Key::Vk(VK_MENU.0, false);
            let _ = send(&[alt.input(false), alt.input(true)]);
            let _ = SetForegroundWindow(h);
            let _ = BringWindowToTop(h);
            if attached {
                let _ = AttachThreadInput(me, fg_thread, false);
            }
        }
        ms(60);
        if GetForegroundWindow() == h {
            Ok(format!("öne getirildi: {}", win.title))
        } else {
            Err(format!("öne getirilemedi: {} (Windows ön plan kilidi)", win.title).into())
        }
    }
}
