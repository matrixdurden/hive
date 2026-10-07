//! tweedle: ses çıkışları ve girişleri tek yerde. Listeden tıklanan cihaz Windows'un varsayılanı
//! olur; kısayollarla bir sonraki çıkışa / girişe geçilir, mikrofon susturulur. Kısayolla yapılan
//! değişiklik ekranın altında kısa bir yazıyla görünür.
//!
//! Susturma bütün mikrofonlara uygulanır: hangi uygulama hangi mikrofonu seçmiş olursa olsun
//! ses gitmez. Mikrofon başka yerden (dizüstünün mikrofon tuşu, Windows) kapanıp açılsa da
//! ekranın altında görünür.
//!
//! Kulaklık gidince müzik hoparlörden devam etmez: Windows ses cihazı gidince sesi sormadan bir
//! sonrakine aktarır, uygulamalara haber vermez. tweedle varsayılan çıkışı izler; çıkış
//! değiştiğinde eski cihaz artık bağlı değilse (Bluetooth koptu, USB çekildi) çalan her medyayı
//! duraklatır. Elle cihaz değiştirmek bir şey yapmaz: eski cihaz hâlâ bağlıdır.
//!
//! Motor hive sürecinin içinde: Core Audio olaylarını hive'ın penceresine iletir, medya
//! oturumlarını (Spotify, tarayıcı, ...) Windows'un ortak medya denetiminden duraklatır.
//! Çal/duraklat tuşuna basmaz: zaten duran bir şeyi başlatmaz. Varsayılan cihazı değiştirmek
//! için Windows'un Ses ayarlarının kullandığı (belgelenmemiş) IPolicyConfig arayüzü kullanılır.

use std::path::PathBuf;

use lyrebird_motor::hotkey::{self, Hotkey};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as Sessions,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Playback,
};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, STGM_READ,
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{GUID, PCWSTR, implement};

use crate::gfx::{Gfx, Rect};
use crate::osd;
use crate::ui::*;
use crate::util::{self, wide};


/// Ses cihazı olayı (Core Audio'nun iş parçacığından).
pub const WM_DEVICE: u32 = WM_APP + 50;
/// Varsayılan mikrofonun sessizliği ya da sesi değişti.
pub const WM_MIC: u32 = WM_APP + 51;
pub const TIMER_RECHECK: usize = 501;
/// Kısayol kimlikleri: sonraki çıkış, sonraki giriş, mikrofonu sustur.
pub const HK_BASE: i32 = 5001;

/// Çıkış değişti ama eski cihaz hâlâ bağlı görünüyor: bu aralıkla bu kadar kez daha bakılır
/// (Windows bazen önce varsayılanı değiştirir, cihazın durumunu sonra).
const RECHECK_MS: u32 = 300;
const RECHECKS: u8 = 4;

const ROW: f32 = 44.0;
const ICON_CHECK: &str = "\u{E73E}";
const ICON_HEADPHONE: &str = "\u{E7F6}";
const ICON_MIC_OFF: &str = "\u{F781}";

const KEYS: usize = 3;
const DEFAULT_KEYS: [&str; KEYS] = ["Ctrl+Alt+O", "Ctrl+Alt+I", "Ctrl+Alt+K"];
const KEY_NAMES: [&str; KEYS] = ["cikis", "giris", "sustur"];

pub fn dir() -> PathBuf {
    util::data_dir().join("tweedle")
}

fn ini() -> PathBuf {
    dir().join("ayarlar.ini")
}

pub fn installed() -> bool {
    dir().is_dir()
}

struct Settings {
    /// Dosyada olmayan kısayol varsayılanını alır, boş bırakılan kapalıdır.
    keys: [Option<Hotkey>; KEYS],
}

impl Settings {
    fn load() -> Self {
        let mut s = Settings { keys: DEFAULT_KEYS.map(Hotkey::parse) };
        let text = std::fs::read_to_string(ini()).unwrap_or_default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            if let Some(i) = KEY_NAMES.iter().position(|n| *n == k.trim()) {
                s.keys[i] = Hotkey::parse(v);
            }
        }
        s
    }

    fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(dir())?;
        let text: String = KEY_NAMES
            .iter()
            .zip(&self.keys)
            .map(|(n, k)| format!("{n}={}\n", k.map(|k| k.to_string()).unwrap_or_default()))
            .collect();
        std::fs::write(ini(), text)
    }
}

pub fn install() -> Result<(), String> {
    Settings::load().save().map_err(|e| format!("{}: {e}", ini().display()))
}

pub fn uninstall() -> Result<(), String> {
    match std::fs::remove_dir_all(dir()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{} {}: {e}", t!("could not delete", "silinemedi:"), dir().display()))
        }
        _ => Ok(()),
    }
}

pub fn leftovers() -> Vec<String> {
    let d = dir();
    d.exists().then(|| d.display().to_string()).into_iter().collect()
}

// --- Ses cihazları ---

#[derive(Clone)]
struct Device {
    id: String,
    name: String,
}

fn post(hwnd: usize, msg: u32) {
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(0));
    }
}

/// Core Audio olaylarını pencereye iletir; kendisi hiçbir şeye bakmaz (olay iş parçacığında
/// Core Audio çağırmak yasak).
#[implement(IMMNotificationClient)]
struct Client(usize);

impl IMMNotificationClient_Impl for Client_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        post(self.0, WM_DEVICE);
        Ok(())
    }

    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        post(self.0, WM_DEVICE);
        Ok(())
    }

    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        post(self.0, WM_DEVICE);
        Ok(())
    }

    fn OnDefaultDeviceChanged(&self, _: EDataFlow, role: ERole, _: &PCWSTR) -> windows::core::Result<()> {
        if role == eConsole {
            post(self.0, WM_DEVICE);
        }
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Varsayılan mikrofonun sessizliği başka yerden (dizüstünün mikrofon tuşu, Windows) değişince.
#[implement(IAudioEndpointVolumeCallback)]
struct MicClient(usize);

impl IAudioEndpointVolumeCallback_Impl for MicClient_Impl {
    fn OnNotify(&self, _: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows::core::Result<()> {
        post(self.0, WM_MIC);
        Ok(())
    }
}

/// Varsayılan mikrofon: kimliği, ses arayüzü, olay kaydı.
struct Mic {
    id: String,
    volume: IAudioEndpointVolume,
    client: IAudioEndpointVolumeCallback,
}

impl Drop for Mic {
    fn drop(&mut self) {
        unsafe {
            let _ = self.volume.UnregisterControlChangeNotify(&self.client);
        }
    }
}

/// Windows'un Ses ayarlarının varsayılan cihazı değiştirdiği arayüz. Yalnızca
/// `SetDefaultEndpoint` çağrılır; öncekiler sıra tutsun diye var.
#[allow(non_snake_case)]
mod policy {
    use windows::Win32::Media::Audio::ERole;
    use windows::core::{HRESULT, IUnknown, IUnknown_Vtbl, PCWSTR, interface};

    #[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    pub unsafe trait IPolicyConfig: IUnknown {
        fn GetMixFormat(&self) -> HRESULT;
        fn GetDeviceFormat(&self) -> HRESULT;
        fn ResetDeviceFormat(&self) -> HRESULT;
        fn SetDeviceFormat(&self) -> HRESULT;
        fn GetProcessingPeriod(&self) -> HRESULT;
        fn SetProcessingPeriod(&self) -> HRESULT;
        fn GetShareMode(&self) -> HRESULT;
        fn SetShareMode(&self) -> HRESULT;
        fn GetPropertyValue(&self) -> HRESULT;
        fn SetPropertyValue(&self) -> HRESULT;
        pub fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
    }
}
use policy::IPolicyConfig;

const POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

fn set_default(id: &str) {
    let id = wide(id);
    unsafe {
        if let Ok(p) = CoCreateInstance::<_, IPolicyConfig>(&POLICY_CONFIG, None, CLSCTX_ALL) {
            for role in [eConsole, eMultimedia, eCommunications] {
                let _ = p.SetDefaultEndpoint(PCWSTR(id.as_ptr()), role);
            }
        }
    }
}

fn device_name(d: &IMMDevice) -> Option<String> {
    unsafe {
        let value = d.OpenPropertyStore(STGM_READ).ok()?.GetValue(&PKEY_Device_FriendlyName).ok()?;
        let p = PropVariantToStringAlloc(&value).ok()?;
        let name = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        name
    }
}

fn device_id(d: &IMMDevice) -> Option<String> {
    unsafe {
        let p = d.GetId().ok()?;
        let id = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        id
    }
}

fn device(d: &IMMDevice) -> Option<Device> {
    let id = device_id(d)?;
    Some(Device { name: device_name(d).unwrap_or_else(|| id.clone()), id })
}

/// "Kulaklıklar (MATRIX PODS)" → "MATRIX PODS": kısa bildirim için cihazın kendi adı.
fn short_name(name: &str) -> &str {
    name.split_once(" (").and_then(|(_, r)| r.strip_suffix(')')).unwrap_or(name)
}

// --- Medya oturumları ---

/// Çalan her medyayı duraklatır (duranlara dokunmaz).
fn pause_playing() -> windows::core::Result<()> {
    let list = Sessions::RequestAsync()?.join()?.GetSessions()?;
    for i in 0..list.Size()? {
        let s = list.GetAt(i)?;
        if s.GetPlaybackInfo()?.PlaybackStatus()? == Playback::Playing {
            let _ = s.TryPauseAsync()?.join();
        }
    }
    Ok(())
}

/// `hive --tweedle-test`: ses cihazlarını ve medya oturumlarını yazdırır, hiçbir şeyi değiştirmez.
pub fn probe() -> String {
    let mut s = String::new();
    unsafe {
        let Ok(e) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else {
            return "ses cihazları okunamadı\n".into();
        };
        for (flow, title) in [(eRender, "çıkış"), (eCapture, "giriş")] {
            let default = e.GetDefaultAudioEndpoint(flow, eConsole).ok().and_then(|d| device_name(&d));
            s += &format!("varsayılan {title}: {}\n", default.as_deref().unwrap_or("yok"));
            let Ok(list) = e.EnumAudioEndpoints(flow, DEVICE_STATE(DEVICE_STATEMASK_ALL)) else { continue };
            for i in 0..list.GetCount().unwrap_or(0) {
                let Ok(d) = list.Item(i) else { continue };
                let state = match d.GetState() {
                    Ok(DEVICE_STATE_ACTIVE) => "bağlı",
                    Ok(DEVICE_STATE_UNPLUGGED) => "çekilmiş",
                    Ok(DEVICE_STATE_NOTPRESENT) => "yok",
                    Ok(DEVICE_STATE_DISABLED) => "kapalı",
                    _ => "?",
                };
                let muted = (flow == eCapture)
                    .then(|| d.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).and_then(|v| v.GetMute()).ok())
                    .flatten()
                    .map_or("", |m| if m.as_bool() { " (sessiz)" } else { "" });
                s += &format!("  {state}: {}{muted}\n", device_name(&d).unwrap_or_default());
            }
        }
    }
    let sessions = (|| -> windows::core::Result<Vec<String>> {
        let list = Sessions::RequestAsync()?.join()?.GetSessions()?;
        (0..list.Size()?)
            .map(|i| {
                let x = list.GetAt(i)?;
                let st = x.GetPlaybackInfo()?.PlaybackStatus()?;
                let st = if st == Playback::Playing {
                    "çalıyor"
                } else if st == Playback::Paused {
                    "duraklatılmış"
                } else {
                    "duruyor"
                };
                Ok(format!("  {}: {st}", x.SourceAppUserModelId()?))
            })
            .collect()
    })();
    match sessions {
        Ok(l) => {
            s += &format!("medya oturumları: {}\n{}", l.len(), l.iter().map(|x| format!("{x}\n")).collect::<String>())
        }
        Err(e) => s += &format!("medya oturumları okunamadı: {e}\n"),
    }
    s += &format!("kurulu: {}\n", installed());
    s
}

// --- Motor ve sayfa ---

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Out(usize),
    In(usize),
    Key(usize),
}

/// Sayfanın bölümleri (çıkış, giriş, kısayollar): başlıkların üst kenarı, kutular ve satırlar.
struct Layout {
    titles: [f32; 3],
    panels: [Rect; 3],
    rows: [Vec<Rect>; 3],
    bottom: f32,
}

pub struct Tweedle {
    hwnd: HWND,
    enumerator: Option<IMMDeviceEnumerator>,
    client: Option<IMMNotificationClient>,
    /// Bağlı çıkışlar ve girişler, ada göre sıralı.
    outs: Vec<Device>,
    ins: Vec<Device>,
    /// Şu anki varsayılan çıkış (koptuğunu anlamak için).
    out: Option<Device>,
    mic: Option<Mic>,
    muted: bool,
    /// Kısayolla en son seçilen çıkış ve giriş: Windows yeni varsayılanı bir an geç bildirir,
    /// arka arkaya basışlar bundan devam eder.
    chosen: [Option<String>; 2],
    /// Çıkıştan düşen ama hâlâ bağlı görünen cihaz ve kalan bakma sayısı.
    recheck: Option<(String, u8)>,
    settings: Settings,
    conflicts: [bool; KEYS],
    registered: Vec<i32>,
    /// Kısayolu atanan satır.
    bind: Option<usize>,
    scroll: f32,
    hover: Hit,
    pressed: Hit,
    w: f32,
    h: f32,
    head_x: f32,
    head_r: f32,
}

impl Tweedle {
    pub fn start(hwnd: HWND) -> Self {
        let mut t = Self {
            hwnd,
            enumerator: None,
            client: None,
            outs: Vec::new(),
            ins: Vec::new(),
            out: None,
            mic: None,
            muted: false,
            chosen: [None, None],
            recheck: None,
            settings: Settings::load(),
            conflicts: [false; KEYS],
            registered: Vec::new(),
            bind: None,
            scroll: 0.0,
            hover: Hit::None,
            pressed: Hit::None,
            w: 0.0,
            h: 0.0,
            head_x: 0.0,
            head_r: 0.0,
        };
        // hive'ın arayüz iş parçacığı COM'u zaten başlatmış (STA).
        if let Ok(e) = unsafe { CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) } {
            let client: IMMNotificationClient = Client(hwnd.0 as usize).into();
            let _ = unsafe { e.RegisterEndpointNotificationCallback(&client) };
            t.enumerator = Some(e);
            t.client = Some(client);
        }
        t.out = t.default_device(eRender);
        t.refresh();
        t.register();
        t
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn default_device(&self, flow: EDataFlow) -> Option<Device> {
        let e = self.enumerator.as_ref()?;
        unsafe { device(&e.GetDefaultAudioEndpoint(flow, eConsole).ok()?) }
    }

    fn list(&self, flow: EDataFlow) -> Vec<Device> {
        let Some(e) = self.enumerator.as_ref() else { return Vec::new() };
        let mut v: Vec<Device> = unsafe {
            let Ok(list) = e.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) else { return Vec::new() };
            (0..list.GetCount().unwrap_or(0)).filter_map(|i| list.Item(i).ok().and_then(|d| device(&d))).collect()
        };
        v.sort_by_key(|d| d.name.to_lowercase());
        v
    }

    /// Varsayılan mikrofon değiştiyse yenisine bağlanır (sessizlik olayları ve seviye).
    fn attach_mic(&mut self) {
        let Some(e) = self.enumerator.as_ref() else { return };
        let Some(d) = unsafe { e.GetDefaultAudioEndpoint(eCapture, eConsole) }.ok() else {
            self.mic = None;
            return;
        };
        let id = device_id(&d).unwrap_or_default();
        if self.mic.as_ref().is_some_and(|m| m.id == id) {
            return;
        }
        self.mic = None;
        unsafe {
            let Ok(volume) = d.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) else { return };
            let client: IAudioEndpointVolumeCallback = MicClient(self.hwnd.0 as usize).into();
            let _ = volume.RegisterControlChangeNotify(&client);
            self.mic = Some(Mic { id, volume, client });
        }
    }

    /// Listeleri, varsayılan mikrofonu ve sessizliğini yeniden okur.
    fn refresh(&mut self) {
        self.outs = self.list(eRender);
        self.ins = self.list(eCapture);
        self.attach_mic();
        self.read_mute();
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn read_mute(&mut self) {
        self.muted = self.mic.as_ref().and_then(|m| unsafe { m.volume.GetMute() }.ok()).is_some_and(|m| m.as_bool());
    }

    /// Cihaz hâlâ bağlı ve çalışır mı (kopmuş Bluetooth, çekilmiş USB değil).
    fn active(&self, id: &str) -> bool {
        let Some(e) = self.enumerator.as_ref() else { return false };
        let id = wide(id);
        unsafe { e.GetDevice(PCWSTR(id.as_ptr())).and_then(|d| d.GetState()).is_ok_and(|s| s == DEVICE_STATE_ACTIVE) }
    }

    /// WM_DEVICE: listeleri tazeler; varsayılan çıkış değiştiyse eski cihaz gitti mi bakar.
    pub fn device_changed(&mut self) {
        self.refresh();
        self.redraw();
        let now = self.default_device(eRender);
        // Windows seçileni bildirdi: bundan sonra gerçek varsayılandan devam edilir.
        let actual = [now.as_ref().map(|d| d.id.clone()), self.mic.as_ref().map(|m| m.id.clone())];
        for (c, a) in self.chosen.iter_mut().zip(actual) {
            if *c == a {
                *c = None;
            }
        }
        if now.as_ref().map(|d| &d.id) == self.out.as_ref().map(|d| &d.id) {
            return;
        }
        let old = std::mem::replace(&mut self.out, now);
        self.recheck = None;
        if let Some(old) = old {
            if self.active(&old.id) {
                // Elle değiştirildi ya da Windows cihazın durumunu henüz yazmadı.
                self.recheck = Some((old.id, RECHECKS));
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_RECHECK, RECHECK_MS, None);
                }
            } else {
                self.gone();
            }
        }
    }

    /// WM_MIC: sessizlik başka yerden de (dizüstünün mikrofon tuşu, Windows) değişebilir.
    pub fn mic_changed(&mut self) {
        let before = self.muted;
        self.read_mute();
        if self.muted != before {
            self.show_mute();
            self.redraw();
        }
    }

    pub fn timer(&mut self, id: usize) {
        if id != TIMER_RECHECK {
            return;
        }
        match self.recheck.take() {
            Some((old, _)) if !self.active(&old) => self.gone(),
            Some((old, n)) if n > 1 => {
                self.recheck = Some((old, n - 1));
                return;
            }
            _ => {}
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_RECHECK);
        }
    }

    /// Çıkış olan cihaz koptu: çalanı duraklat. Medya denetimi bekletir (WinRT): arka planda.
    fn gone(&self) {
        std::thread::spawn(|| unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = pause_playing();
        });
    }

    // --- Cihaz değiştirme ---

    /// Kısayol: listede bir sonraki cihaza geçer ve ekranda gösterir.
    fn cycle(&mut self, flow: EDataFlow) {
        self.refresh();
        let k = if flow == eRender { 0 } else { 1 };
        let list = if flow == eRender { &self.outs } else { &self.ins };
        let actual = if flow == eRender { self.out.as_ref().map(|d| &d.id) } else { self.mic.as_ref().map(|m| &m.id) };
        // Son seçilen hâlâ listedeyse ondan devam: Windows henüz bildirmemiş olabilir.
        let current = self.chosen[k].as_ref().filter(|id| list.iter().any(|d| &d.id == *id)).or(actual);
        if list.is_empty() {
            return;
        }
        let i = list.iter().position(|d| Some(&d.id) == current).map_or(0, |i| (i + 1) % list.len());
        let next = list[i].clone();
        self.chosen[k] = Some(next.id.clone());
        set_default(&next.id);
        self.device_changed();
        osd::show(if flow == eRender { ICON_HEADPHONE } else { ICON_MIC }, 0x38bdf8, short_name(&next.name));
    }

    /// Bütün mikrofonları susturur ya da açar: hangi uygulama hangisini seçmiş olursa olsun.
    fn toggle_mute(&mut self) {
        let Some(e) = self.enumerator.as_ref() else { return };
        let mute = !self.muted;
        unsafe {
            let Ok(list) = e.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) else { return };
            for i in 0..list.GetCount().unwrap_or(0) {
                if let Ok(v) = list.Item(i).and_then(|d| d.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)) {
                    let _ = v.SetMute(mute, std::ptr::null());
                }
            }
        }
        self.read_mute();
        self.redraw();
        self.show_mute();
    }

    fn show_mute(&mut self) {
        if self.muted {
            osd::show(ICON_MIC_OFF, osd::RED, t!("Microphone off", "Mikrofon kapalı"));
        } else {
            osd::show(ICON_MIC, osd::GREEN, t!("Microphone on", "Mikrofon açık"));
        }
    }

    // --- Kısayollar ---

    fn unregister(&mut self) {
        for id in self.registered.drain(..) {
            unsafe {
                let _ = UnregisterHotKey(Some(self.hwnd), id);
            }
        }
    }

    /// Hepsini baştan kaydeder. Başka bir uygulamanın tuttuğu kısayol kırmızı görünür.
    fn register(&mut self) {
        self.unregister();
        for (i, key) in self.settings.keys.iter().enumerate() {
            let id = HK_BASE + i as i32;
            let ok = key.is_some_and(|h| {
                unsafe { RegisterHotKey(Some(self.hwnd), id, h.mods | MOD_NOREPEAT, h.vk as u32) }.is_ok()
            });
            if ok {
                self.registered.push(id);
            }
            self.conflicts[i] = key.is_some() && !ok;
        }
    }

    fn start_bind(&mut self, i: usize) {
        self.unregister();
        self.bind = Some(i);
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
        self.redraw();
    }

    /// `Some(None)`: kısayolu sil; `None`: vazgeç.
    fn end_bind(&mut self, key: Option<Option<Hotkey>>) {
        let Some(i) = self.bind.take() else { return };
        if let Some(key) = key {
            // Aynı kısayol başka satırdaysa oradan alınır.
            for k in &mut self.settings.keys {
                if key.is_some() && *k == key {
                    *k = None;
                }
            }
            self.settings.keys[i] = key;
            let _ = self.settings.save();
        }
        self.register();
        self.redraw();
    }

    pub fn hotkey(&mut self, id: i32) {
        match id - HK_BASE {
            0 => self.cycle(eRender),
            1 => self.cycle(eCapture),
            2 => self.toggle_mute(),
            _ => {}
        }
    }

    /// Kısayol atarken Alt ile gelen menü ve bip sesi olmasın.
    pub fn binding(&self) -> bool {
        self.bind.is_some()
    }

    pub fn kill_focus(&mut self) {
        self.end_bind(None);
    }

    // --- Yerleşim ---

    fn layout_all(&self) -> Layout {
        let (l, r) = (PAD, self.w - PAD);
        let mut y = HEADER + 16.0 - self.scroll;
        let mut titles = [0.0; 3];
        let mut panels = [Rect::new(0.0, 0.0, 0.0, 0.0); 3];
        let mut rows: [Vec<Rect>; 3] = Default::default();
        for (s, n) in [self.outs.len().max(1), self.ins.len().max(1), KEYS].into_iter().enumerate() {
            titles[s] = y;
            let top = y + 30.0;
            let row =
                |i: usize| Rect::new(l + 6.0, top + 6.0 + i as f32 * ROW, r - 6.0, top + 6.0 + (i + 1) as f32 * ROW);
            rows[s] = (0..n).map(row).collect();
            panels[s] = Rect::new(l, top, r, top + 12.0 + n as f32 * ROW);
            y = panels[s].b + 24.0;
        }
        Layout { titles, panels, rows, bottom: y + self.scroll }
    }

    fn max_scroll(&self) -> f32 {
        (self.layout_all().bottom - self.h).max(0.0)
    }

    fn chip_text(&self, i: usize) -> String {
        match (self.bind == Some(i), self.settings.keys[i]) {
            (true, _) => t!("press a key", "tuşa bas").into(),
            (false, Some(k)) => k.to_string(),
            (false, None) => t!("shortcut", "kısayol").into(),
        }
    }

    fn key_chip(&self, g: &Gfx, row: Rect, i: usize) -> Rect {
        chip_rect(g, row.r - 10.0, row.cy(), &self.chip_text(i))
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return Hit::None;
        }
        let [outs, ins, keys] = &self.layout_all().rows;
        if let Some(i) = outs.iter().take(self.outs.len()).position(|r| r.contains(x, y)) {
            return Hit::Out(i);
        }
        if let Some(i) = ins.iter().take(self.ins.len()).position(|r| r.contains(x, y)) {
            return Hit::In(i);
        }
        if let Some(i) = keys.iter().enumerate().position(|(i, r)| self.key_chip(g, *r, i).contains(x, y)) {
            return Hit::Key(i);
        }
        Hit::None
    }

    // --- Çizim ---

    fn paint_devices(&self, g: &Gfx, rows: &[Rect], list: &[Device], current: Option<&str>, mic: bool) {
        if list.is_empty() {
            let r = rows[0];
            g.text(t!("No device", "Cihaz yok"), &g.f.text, Rect::new(r.l + 14.0, r.t, r.r, r.b), FAINT);
            return;
        }
        for (i, (r, d)) in rows.iter().zip(list).enumerate() {
            let selected = current == Some(d.id.as_str());
            let hit = if mic { Hit::In(i) } else { Hit::Out(i) };
            if self.hover == hit && !selected {
                g.fill(*r, 8.0, HOVER);
            }
            let (icon, color) = match (mic, selected, self.muted) {
                (false, _, _) => (ICON_HEADPHONE, if selected { accent() } else { MUTED }),
                (true, _, true) => (ICON_MIC_OFF, if selected { RED } else { MUTED }),
                (true, _, false) => (ICON_MIC, if selected { accent() } else { MUTED }),
            };
            g.text(icon, &g.f.icon_small, Rect::new(r.l + 12.0, r.t, r.l + 32.0, r.b), color);
            let mut right = r.r - 12.0;
            if selected {
                g.text(ICON_CHECK, &g.f.icon_small, Rect::new(r.r - 34.0, r.t, r.r - 12.0, r.b), accent());
                right -= 30.0;
            }
            g.text(&d.name, &g.f.text, Rect::new(r.l + 44.0, r.t, right, r.b), if selected { TEXT } else { MUTED });
        }
    }

    pub fn paint(&self, g: &Gfx) {
        let w = self.w;
        let watching = self.enumerator.is_some();
        let (dot, head) = match () {
            _ if !watching => (RED, t!("Cannot read audio devices", "Ses cihazları okunamıyor")),
            _ if self.muted => (RED, t!("Microphone off · all microphones", "Mikrofon kapalı · bütün mikrofonlar")),
            _ => (GREEN, t!("Music pauses when headphones drop", "Kulaklık kopunca müzik durur")),
        };
        status(g, self.head_x + 16.0, HEAD_CY, dot, head, MUTED, self.head_r - 8.0);

        let l = self.layout_all();
        g.clip(Rect::new(0.0, HEADER, w, self.h), || {
            let titles = [t!("Output", "Çıkış"), t!("Input", "Giriş"), t!("Shortcuts", "Kısayollar")];
            for (s, title) in titles.iter().enumerate() {
                let t = l.titles[s];
                g.text(title, &g.f.strong, Rect::new(PAD, t, w - PAD, t + 24.0), TEXT);
                g.fill(l.panels[s], 12.0, PANEL);
                g.stroke(l.panels[s], 12.0, LINE, 1.0);
            }
            let [outs, ins, keys] = &l.rows;
            self.paint_devices(g, outs, &self.outs, self.out.as_ref().map(|d| d.id.as_str()), false);
            self.paint_devices(g, ins, &self.ins, self.mic.as_ref().map(|m| m.id.as_str()), true);

            let names = [
                t!("Next output", "Sonraki çıkış"),
                t!("Next input", "Sonraki giriş"),
                t!("Mute all microphones", "Bütün mikrofonları sustur"),
            ];
            for (i, (r, name)) in keys.iter().zip(names).enumerate() {
                if i > 0 {
                    g.fill(Rect::new(r.l + 8.0, r.t, r.r - 8.0, r.t + 1.0), 0.0, HOVER);
                }
                g.text(name, &g.f.text, Rect::new(r.l + 14.0, r.t, r.r - 160.0, r.b), TEXT);
                let (c, b) = match () {
                    _ if self.bind == Some(i) => (accent(), accent()),
                    _ if self.conflicts[i] => (RED, LINE),
                    _ if self.settings.keys[i].is_none() => (FAINT, LINE),
                    _ if self.hover == Hit::Key(i) => (TEXT, FAINT),
                    _ => (MUTED, LINE),
                };
                chip(g, self.key_chip(g, *r, i), &self.chip_text(i), c, b);
            }
        });
    }
}

impl ToolPage for Tweedle {
    fn layout(&mut self, w: f32, h: f32, head_x: f32, head_r: f32) {
        self.w = w;
        self.h = h;
        self.head_x = head_x;
        self.head_r = head_r;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn paint(&self, g: &Gfx) {
        Tweedle::paint(self, g)
    }

    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        let hit = self.hit(g, x, y);
        if hit != self.hover {
            self.hover = hit;
            self.redraw();
        }
    }

    fn mouse_leave(&mut self) {
        if self.hover != Hit::None {
            self.hover = Hit::None;
            self.redraw();
        }
    }

    fn mouse_down(&mut self, g: &Gfx, x: f32, y: f32) {
        self.pressed = self.hit(g, x, y);
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        let hit = self.hit(g, x, y);
        if hit != std::mem::replace(&mut self.pressed, Hit::None) {
            return;
        }
        self.end_bind(None);
        let chosen = match hit {
            Hit::Out(i) => self.outs.get(i),
            Hit::In(i) => self.ins.get(i),
            Hit::Key(i) => return self.start_bind(i),
            Hit::None => None,
        };
        if let Some(id) = chosen.map(|d| d.id.clone()) {
            set_default(&id);
            self.device_changed();
        }
    }

    fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        self.scroll = (self.scroll - delta * 60.0).clamp(0.0, self.max_scroll());
        self.hover = self.hit(g, x, y);
        self.redraw();
    }

    /// Kısayol atanıyorsa tuşu yakalar: Esc vazgeçer, Backspace / Delete siler.
    fn key(&mut self, vk: u16) -> bool {
        if self.bind.is_none() {
            return false;
        }
        if !hotkey::is_modifier(vk) {
            let key = match VIRTUAL_KEY(vk) {
                VK_ESCAPE => None,
                VK_BACK | VK_DELETE => Some(None),
                _ => Some(Some(Hotkey::pressed(vk))),
            };
            self.end_bind(key);
        }
        true
    }

    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        self.hit(g, x, y) != Hit::None
    }

    fn set_visible(&mut self, visible: bool) {
        if !visible {
            self.hover = Hit::None;
            self.end_bind(None);
        }
    }
}

impl Drop for Tweedle {
    fn drop(&mut self) {
        self.unregister();
        self.mic = None;
        unsafe {
            if let (Some(e), Some(c)) = (&self.enumerator, &self.client) {
                let _ = e.UnregisterEndpointNotificationCallback(c);
            }
            let _ = KillTimer(Some(self.hwnd), TIMER_RECHECK);
        }
    }
}
