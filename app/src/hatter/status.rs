//! Dock'un sağ ucundaki durum: ağ, ses, pil, saat. Hiçbiri yoklanmaz: ağ arayüz bildirimiyle,
//! ses Core Audio bildirimiyle, pil güç bildirimiyle değişince okunur; saat dakika başında bir
//! kez uyanır. Bildirimler başka iş parçacıklarından gelir, dock'un penceresine mesaj olarak
//! iletilir.

use std::ffi::c_void;

use windows::Win32::Foundation::*;
use windows::Win32::Globalization::GetDateFormatEx;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::*;
use windows::Win32::NetworkManagement::IpHelper::*;
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::AF_UNSPEC;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::System::Power::{
    GetSystemPowerStatus, HPOWERNOTIFY, RegisterPowerSettingNotification, SYSTEM_POWER_STATUS,
    UnregisterPowerSettingNotification,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::SystemServices::{GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, implement, w};

/// Durum değişti: wParam hangisi (`NET`, `VOLUME`, `DEVICE`).
pub const WM_STATUS: u32 = WM_APP + 20;
pub const NET: usize = 1;
pub const VOLUME: usize = 2;
pub const DEVICE: usize = 3;

const ICON_WIFI: &str = "\u{E701}";
const ICON_ETHERNET: &str = "\u{E839}";
const ICON_VOLUME: &str = "\u{E767}";
const ICON_MUTE: &str = "\u{E74F}";

fn post(hwnd: usize, what: usize) {
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_STATUS, WPARAM(what), LPARAM(0));
    }
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeClient(usize);

impl IAudioEndpointVolumeCallback_Impl for VolumeClient_Impl {
    fn OnNotify(&self, _: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows::core::Result<()> {
        post(self.0, VOLUME);
        Ok(())
    }
}

#[implement(IMMNotificationClient)]
struct DeviceClient(usize);

impl IMMNotificationClient_Impl for DeviceClient_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _: &PCWSTR) -> windows::core::Result<()> {
        if flow == eRender && role == eConsole {
            post(self.0, DEVICE);
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

unsafe extern "system" fn on_interface(ctx: *const c_void, _: *const MIB_IPINTERFACE_ROW, _: MIB_NOTIFICATION_TYPE) {
    post(ctx as usize, NET);
}

#[derive(Clone, Copy, PartialEq)]
pub enum Net {
    None,
    Wifi,
    Ethernet,
    Other,
}

pub struct Status {
    hwnd: HWND,
    pub net: Net,
    /// Ses düzeyi (0..1) ve sessiz mi; ses cihazı yoksa `None`.
    pub volume: Option<(f32, bool)>,
    /// Pil yüzdesi ve prizde mi; pil yoksa `None`.
    pub battery: Option<(u8, bool)>,
    pub time: String,
    pub date: String,
    enumerator: Option<IMMDeviceEnumerator>,
    devices: Option<IMMNotificationClient>,
    endpoint: Option<(IAudioEndpointVolume, IAudioEndpointVolumeCallback)>,
    power: Vec<HPOWERNOTIFY>,
    ip: HANDLE,
}

impl Status {
    pub fn new(hwnd: HWND, live: bool) -> Self {
        let mut s = Status {
            hwnd,
            net: Net::None,
            volume: None,
            battery: None,
            time: String::new(),
            date: String::new(),
            enumerator: None,
            devices: None,
            endpoint: None,
            power: Vec::new(),
            ip: HANDLE::default(),
        };
        let h = hwnd.0 as usize;
        unsafe {
            if let Ok(e) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) {
                if live {
                    let c: IMMNotificationClient = DeviceClient(h).into();
                    if e.RegisterEndpointNotificationCallback(&c).is_ok() {
                        s.devices = Some(c);
                    }
                }
                s.enumerator = Some(e);
            }
            if live {
                for guid in [&GUID_ACDC_POWER_SOURCE, &GUID_BATTERY_PERCENTAGE_REMAINING] {
                    if let Ok(p) = RegisterPowerSettingNotification(HANDLE(hwnd.0), guid, DEVICE_NOTIFY_WINDOW_HANDLE) {
                        s.power.push(p);
                    }
                }
                let mut ip = HANDLE::default();
                if NotifyIpInterfaceChange(AF_UNSPEC, Some(on_interface), Some(h as *const c_void), false, &mut ip)
                    == NO_ERROR
                {
                    s.ip = ip;
                }
            }
        }
        s.attach_volume(live);
        s.read_net();
        s.read_battery();
        s.read_time();
        s
    }

    /// Varsayılan çıkışa bağlanır (değişince yeniden).
    pub fn attach_volume(&mut self, live: bool) {
        if let Some((v, c)) = self.endpoint.take() {
            unsafe {
                let _ = v.UnregisterControlChangeNotify(&c);
            }
        }
        self.volume = None;
        let Some(e) = &self.enumerator else { return };
        unsafe {
            let Ok(d) = e.GetDefaultAudioEndpoint(eRender, eConsole) else { return };
            let Ok(v) = d.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) else { return };
            let c: IAudioEndpointVolumeCallback = VolumeClient(self.hwnd.0 as usize).into();
            if live {
                let _ = v.RegisterControlChangeNotify(&c);
            }
            self.endpoint = Some((v, c));
        }
        self.read_volume();
    }

    pub fn read_volume(&mut self) {
        self.volume = self.endpoint.as_ref().and_then(|(v, _)| unsafe {
            Some((v.GetMasterVolumeLevelScalar().ok()?, v.GetMute().ok()?.as_bool()))
        });
    }

    /// Bağlı mı, neyle: ağ geçidi olan, açık bir Wi-Fi ya da Ethernet bağdaştırıcısı aranır.
    pub fn read_net(&mut self) {
        let mut size = 16 * 1024u32;
        let mut buf: Vec<u64> = vec![0; size as usize / 8];
        let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        unsafe {
            let mut r = GetAdaptersAddresses(AF_UNSPEC.0 as u32, flags, None, Some(buf.as_mut_ptr().cast()), &mut size);
            if r == ERROR_BUFFER_OVERFLOW.0 {
                buf = vec![0; size as usize / 8 + 1];
                r = GetAdaptersAddresses(AF_UNSPEC.0 as u32, flags, None, Some(buf.as_mut_ptr().cast()), &mut size);
            }
            if r != 0 {
                return;
            }
            let (mut wifi, mut ethernet, mut other) = (false, false, false);
            let mut a = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
            while !a.is_null() {
                let ad = &*a;
                if ad.OperStatus == IfOperStatusUp && !ad.FirstGatewayAddress.is_null() {
                    match ad.IfType {
                        71 => wifi = true,
                        6 => ethernet = true,
                        24 => {}
                        _ => other = true,
                    }
                }
                a = ad.Next;
            }
            self.net = if wifi {
                Net::Wifi
            } else if ethernet {
                Net::Ethernet
            } else if other {
                Net::Other
            } else {
                Net::None
            };
        }
    }

    pub fn read_battery(&mut self) {
        self.battery = battery();
    }

    /// Saati yazar; bir sonraki dakika başına kalan milisaniyeyi döndürür.
    pub fn read_time(&mut self) -> u32 {
        let t = unsafe { GetLocalTime() };
        self.time = format!("{:02}:{:02}", t.wHour, t.wMinute);
        let mut buf = [0u16; 128];
        let n = unsafe { GetDateFormatEx(PCWSTR::null(), Default::default(), Some(&t), w!("d MMMM dddd"), Some(&mut buf), PCWSTR::null()) };
        self.date = String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize]);
        (60 - t.wSecond as u32) * 1000 - t.wMilliseconds as u32 + 50
    }

    pub fn net_icon(&self) -> &'static str {
        if self.net == Net::Ethernet { ICON_ETHERNET } else { ICON_WIFI }
    }

    pub fn net_text(&self) -> &'static str {
        match self.net {
            Net::Wifi => t!("Wi-Fi connected", "Wi-Fi bağlı"),
            Net::Ethernet => t!("Ethernet connected", "Ethernet bağlı"),
            Net::Other => t!("Connected", "Bağlı"),
            Net::None => t!("Not connected", "Bağlantı yok"),
        }
    }

    pub fn volume_icon(&self) -> &'static str {
        match self.volume {
            Some((v, m)) if m || v < 0.005 => ICON_MUTE,
            _ => ICON_VOLUME,
        }
    }

    pub fn volume_text(&self) -> String {
        match self.volume {
            None => t!("No audio device", "Ses cihazı yok").into(),
            Some((_, true)) => t!("Sound off", "Ses kapalı").into(),
            Some((v, false)) => t!(format!("Volume {}%", (v * 100.0).round()), format!("Ses %{}", (v * 100.0).round())),
        }
    }

    pub fn battery_icon(&self) -> &'static str {
        battery_glyph(self.battery.map_or(100, |b| b.0))
    }

    /// Dock'ta simgenin yanındaki yüzde.
    pub fn battery_pct(&self) -> String {
        let p = self.battery.map_or(0, |b| b.0);
        t!(format!("{p}%"), format!("%{p}"))
    }

    pub fn battery_text(&self) -> String {
        match self.battery {
            Some((p, true)) => t!(format!("{p}% · plugged in"), format!("%{p} · prizde")),
            Some((p, false)) => t!(format!("{p}%"), format!("%{p}")),
            None => String::new(),
        }
    }
}

impl Drop for Status {
    fn drop(&mut self) {
        unsafe {
            if let Some((v, c)) = self.endpoint.take() {
                let _ = v.UnregisterControlChangeNotify(&c);
            }
            if let (Some(e), Some(c)) = (&self.enumerator, &self.devices) {
                let _ = e.UnregisterEndpointNotificationCallback(c);
            }
            for p in self.power.drain(..) {
                let _ = UnregisterPowerSettingNotification(p);
            }
            if !self.ip.is_invalid() && !self.ip.0.is_null() {
                let _ = CancelMibChangeNotify2(self.ip);
            }
        }
    }
}

/// Pil simgesi: dolulukla on basamak (Battery0..Battery10).
pub fn battery_glyph(percent: u8) -> &'static str {
    const LEVELS: [&str; 11] = [
        "\u{E850}", "\u{E851}", "\u{E852}", "\u{E853}", "\u{E854}", "\u{E855}", "\u{E856}", "\u{E857}", "\u{E858}",
        "\u{E859}", "\u{E83F}",
    ];
    LEVELS[(percent.min(100) as usize + 5) / 10]
}

/// Pil yüzdesi ve prizde mi; pil yoksa `None`.
pub fn battery() -> Option<(u8, bool)> {
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut s) }
        .ok()
        .filter(|_| s.BatteryFlag & 128 == 0 && s.BatteryFlag != 255)
        .map(|_| (s.BatteryLifePercent.min(100), s.ACLineStatus == 1))
}
