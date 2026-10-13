//! Işıkla Bluetooth LE bağlantısı. Bütün Bluetooth işi tek bir iş parçacığında: sayfa ve müzik
//! komutları kuyruğa atar, iş parçacığı sırayla yazar. Kuyrukta aynı türden birden çok komut
//! birikirse (kaydırıcı sürüklenirken, müzikte) yalnızca sonuncusu gider. Bağlantının durumu
//! değişince pencereye `WM_LINK` gelir.
//!
//! Bağlanmak: kayıtlı adres varsa önce doğrudan ona, olmazsa tarama (reklamdaki adı desteklenen
//! bir modelinki olan ilk cihaz). Bağlantı `GattSession::MaintainConnection` ile açık tutulur;
//! ışık uzaklaşıp dönerse Windows kendisi yeniden bağlanır.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Devices::Bluetooth::Advertisement::*;
use windows::Devices::Bluetooth::GenericAttributeProfile::*;
use windows::Devices::Bluetooth::{BluetoothCacheMode, BluetoothConnectionStatus, BluetoothLEDevice};
use windows::Foundation::TypedEventHandler;
use windows::Storage::Streams::{DataReader, DataWriter};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::surucu::{self, Cmd, Model};

/// Bağlantının durumu değişti (bağlantı iş parçacığından ve Bluetooth olaylarından).
pub const WM_LINK: u32 = WM_APP + 90;

/// Tarama bu kadar sürer.
const SCAN: Duration = Duration::from_secs(10);

type Res<T> = Result<T, String>;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum State {
    #[default]
    Off,
    Searching,
    Connecting,
    Connected,
}

#[derive(Clone, Default)]
pub struct Status {
    pub state: State,
    /// Bağlanılan (ya da bağlanılmaya çalışılan) ışığın adı.
    pub name: String,
    pub model: Option<&'static Model>,
    pub address: u64,
    /// Bağlıyken ışık ulaşılabilir mi (uzaklaşınca Windows yeniden dener).
    pub reachable: bool,
    /// Son bağlanma ya da yazma hatası.
    pub error: Option<String>,
}

enum Msg {
    Connect(Option<u64>),
    Disconnect,
    Send(Cmd),
    Quit,
}

/// Komut gönderebilen kopya (müzik iş parçacığı için).
#[derive(Clone)]
pub struct Tx(Sender<Msg>);

impl Tx {
    pub fn send(&self, cmd: Cmd) {
        let _ = self.0.send(Msg::Send(cmd));
    }

    /// Hiçbir yere gitmeyen kopya (testler).
    #[cfg(test)]
    pub fn test() -> Self {
        Tx(mpsc::channel().0)
    }
}

pub struct Link {
    tx: Sender<Msg>,
    status: Arc<Mutex<Status>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Link {
    pub fn start(hwnd: HWND) -> Self {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let (s, h) = (status.clone(), hwnd.0 as usize);
        let thread = std::thread::Builder::new().name("lights".into()).spawn(move || worker(rx, s, h)).ok();
        Self { tx, status, thread }
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    pub fn connected(&self) -> bool {
        self.status.lock().unwrap().state == State::Connected
    }

    /// `address`: son bağlanılan ışık (yoksa taranır).
    pub fn connect(&self, address: Option<u64>) {
        let _ = self.tx.send(Msg::Connect(address));
    }

    pub fn disconnect(&self) {
        let _ = self.tx.send(Msg::Disconnect);
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(Msg::Send(cmd));
    }

    pub fn tx(&self) -> Tx {
        Tx(self.tx.clone())
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn post(hwnd: usize) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_LINK, WPARAM(0), LPARAM(0));
    }
}

fn worker(rx: Receiver<Msg>, status: Arc<Mutex<Status>>, hwnd: usize) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let set = |f: &dyn Fn(&mut Status)| {
        f(&mut status.lock().unwrap());
        post(hwnd);
    };
    let mut conn: Option<Conn> = None;
    // Komut toplarken gelen başka tür mesaj: sonraki turda işlenir.
    let mut next: Option<Msg> = None;
    loop {
        let msg = match next.take() {
            Some(m) => m,
            None => match rx.recv() {
                Ok(m) => m,
                Err(_) => break,
            },
        };
        match msg {
            Msg::Quit => break,
            Msg::Disconnect => {
                conn = None;
                set(&|s| {
                    s.state = State::Off;
                    s.error = None;
                });
            }
            Msg::Connect(address) => {
                conn = None;
                match connect(address, &status, hwnd) {
                    Ok(c) => {
                        let (name, model, address) = (c.name.clone(), c.model, c.address);
                        conn = Some(c);
                        set(&|s| {
                            *s = Status { state: State::Connected, name: name.clone(), model: Some(model), address, reachable: true, error: None }
                        });
                        ortak::log!("lights: bağlandı · {name} ({address:012X})");
                    }
                    Err(e) => {
                        ortak::log!("lights: bağlanılamadı · {e}");
                        set(&|s| {
                            s.state = State::Off;
                            s.error = Some(e.clone());
                        });
                    }
                }
            }
            Msg::Send(cmd) => {
                let mut cmds = vec![cmd];
                while let Ok(m) = rx.try_recv() {
                    match m {
                        Msg::Send(c) => {
                            cmds.retain(|x| !x.same_kind(&c));
                            cmds.push(c);
                        }
                        other => {
                            next = Some(other);
                            break;
                        }
                    }
                }
                let Some(c) = conn.as_mut() else { continue };
                for cmd in cmds {
                    let Some(data) = (c.model.encode)(cmd) else { continue };
                    if let Err(e) = c.write(&data) {
                        ortak::log!("lights: yazılamadı ({cmd:?}) · {e}");
                        set(&|s| s.error = Some(e.clone()));
                        break;
                    }
                }
            }
        }
    }
    drop(conn);
}

// --- Bağlanmak ---

fn connect(address: Option<u64>, status: &Arc<Mutex<Status>>, hwnd: usize) -> Res<Conn> {
    let set = |state: State, name: &str| {
        let mut s = status.lock().unwrap();
        s.state = state;
        s.name = name.to_string();
        s.error = None;
        drop(s);
        post(hwnd);
    };
    if let Some(a) = address.filter(|a| *a != 0) {
        let name = status.lock().unwrap().name.clone();
        set(State::Connecting, &name);
        match Conn::open(a, status, hwnd) {
            Ok(c) => return Ok(c),
            Err(e) => ortak::log!("lights: kayıtlı ışığa bağlanılamadı ({a:012X}) · {e}; taranıyor"),
        }
    }
    set(State::Searching, "");
    let Some((a, name, _)) = scan(SCAN, |_, _| {})? else {
        return Err(t!(
            format!("No light found. Is it on and nearby? If its phone app is connected, close it. Supported: {}", surucu::names()),
            format!("Işık bulunamadı. Açık ve yakında mı? Telefon uygulaması bağlıysa kapat. Desteklenen: {}", surucu::names())
        ));
    };
    set(State::Connecting, &name);
    Conn::open(a, status, hwnd)
}

/// Reklam dinler: desteklenen ilk ışığı (adres, ad, model) döndürür, `timeout` içinde yoksa `None`.
/// `seen` her adlı reklamda çağrılır (komut satırındaki tarama listesi için).
fn scan(timeout: Duration, mut seen: impl FnMut(u64, &str)) -> Res<Option<(u64, String, &'static Model)>> {
    let err = |e: windows_core::Error| format!("Bluetooth: {}", e.message());
    let watcher = BluetoothLEAdvertisementWatcher::new().map_err(err)?;
    // Etkin tarama: ad bazı cihazlarda yalnızca tarama cevabında gelir.
    watcher.SetScanningMode(BluetoothLEScanningMode::Active).map_err(err)?;
    let (tx, rx) = mpsc::channel::<(u64, String)>();
    let handler = TypedEventHandler::<BluetoothLEAdvertisementWatcher, BluetoothLEAdvertisementReceivedEventArgs>::new(move |_, args| {
        if let Some(a) = args.as_ref() {
            let name = a.Advertisement()?.LocalName()?.to_string();
            if !name.is_empty() {
                let _ = tx.send((a.BluetoothAddress()?, name));
            }
        }
        Ok(())
    });
    let token = watcher.Received(&handler).map_err(err)?;
    watcher.Start().map_err(|e| no_bluetooth(&e))?;
    let end = Instant::now() + timeout;
    let mut found = None;
    while found.is_none() {
        let now = Instant::now();
        if now >= end {
            break;
        }
        match rx.recv_timeout((end - now).min(Duration::from_millis(250))) {
            Ok((a, name)) => {
                seen(a, &name);
                if let Some(m) = surucu::for_name(&name) {
                    found = Some((a, name, m));
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if watcher.Status().map_err(err)? == BluetoothLEAdvertisementWatcherStatus::Aborted {
                    let _ = watcher.RemoveReceived(token);
                    return Err(t!("Bluetooth is off or missing", "Bluetooth kapalı ya da yok").into());
                }
            }
            Err(_) => break,
        }
    }
    let _ = watcher.Stop();
    let _ = watcher.RemoveReceived(token);
    Ok(found)
}

fn no_bluetooth(e: &windows_core::Error) -> String {
    t!(format!("Bluetooth is off or missing ({})", e.message()), format!("Bluetooth kapalı ya da yok ({})", e.message()))
}

fn gatt_error(s: GattCommunicationStatus) -> String {
    match s {
        GattCommunicationStatus::Unreachable => t!("the light is unreachable", "ışığa ulaşılamıyor").into(),
        GattCommunicationStatus::AccessDenied => t!("access denied", "erişim reddedildi").into(),
        GattCommunicationStatus::ProtocolError => t!("the light refused", "ışık reddetti").into(),
        s => format!("GATT {}", s.0),
    }
}

/// Açık bağlantı: yazılan özellik, kapatırken bırakılacaklar.
struct Conn {
    name: String,
    model: &'static Model,
    address: u64,
    device: BluetoothLEDevice,
    session: Option<GattSession>,
    service: GattDeviceService,
    ch: GattCharacteristic,
    /// Cevaplı yazma (destekliyorsa; olmazsa cevapsız).
    with_response: bool,
    without_ok: bool,
    status_token: i64,
    notify: Option<(GattCharacteristic, i64)>,
}

impl Conn {
    fn open(address: u64, status: &Arc<Mutex<Status>>, hwnd: usize) -> Res<Conn> {
        let err = |e: windows_core::Error| format!("Bluetooth: {}", e.message());
        let device = BluetoothLEDevice::FromBluetoothAddressAsync(address)
            .and_then(|op| op.join())
            .map_err(|_| t!("The light is not reachable", "Işığa ulaşılamıyor").to_string())?;
        let name = device.Name().map(|n| n.to_string()).unwrap_or_default();
        let saved = status.lock().unwrap().model;
        let model = surucu::for_name(&name).or(saved).ok_or_else(|| {
            t!(format!("{name} is not a supported light"), format!("{name} desteklenen bir ışık değil"))
        })?;
        let name = if name.is_empty() { model.name.to_string() } else { name };

        // Bağlantıyı açık tut: her komutta yeniden bağlanmasın, ışık dönünce kendiliğinden bağlansın.
        let session = device
            .BluetoothDeviceId()
            .and_then(|id| GattSession::FromDeviceIdAsync(&id))
            .and_then(|op| op.join())
            .and_then(|s| s.SetMaintainConnection(true).map(|_| s))
            .map_err(|e| ortak::log!("lights: oturum açılamadı · {}", e.message()))
            .ok();

        // Önce önbellekten (hızlı), bulunamazsa cihazın kendisinden.
        let mut found = None;
        for mode in [BluetoothCacheMode::Cached, BluetoothCacheMode::Uncached] {
            let r = device.GetGattServicesWithCacheModeAsync(mode).and_then(|op| op.join()).map_err(err)?;
            let st = r.Status().map_err(err)?;
            if st != GattCommunicationStatus::Success {
                if mode == BluetoothCacheMode::Uncached {
                    return Err(gatt_error(st));
                }
                continue;
            }
            let services = r.Services().map_err(err)?;
            for i in 0..services.Size().map_err(err)? {
                let s = services.GetAt(i).map_err(err)?;
                let Ok(cr) = s.GetCharacteristicsForUuidWithCacheModeAsync(model.write, mode).and_then(|op| op.join()) else { continue };
                if cr.Status().ok() == Some(GattCommunicationStatus::Success)
                    && let Ok(list) = cr.Characteristics()
                    && list.Size().unwrap_or(0) > 0
                    && let Ok(ch) = list.GetAt(0)
                {
                    found = Some((s, ch));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let Some((service, ch)) = found else {
            return Err(t!("The light's control channel was not found", "Işığın komut kanalı bulunamadı").into());
        };
        let props = ch.CharacteristicProperties().map_err(err)?;
        let with = props.contains(GattCharacteristicProperties::Write);
        let without = props.contains(GattCharacteristicProperties::WriteWithoutResponse);

        let st = status_handler(&device, hwnd, status);
        let notify = model.notify.and_then(|u| subscribe(&service, u));

        let mut c = Conn {
            name,
            model,
            address,
            device,
            session,
            service,
            ch,
            with_response: with || !without,
            without_ok: without,
            status_token: st.unwrap_or(0),
            notify,
        };
        if !model.hello.is_empty() {
            c.write(model.hello)?;
        }
        Ok(c)
    }

    fn write(&mut self, data: &[u8]) -> Res<()> {
        let err = |e: windows_core::Error| format!("Bluetooth: {}", e.message());
        let send = |with: bool| -> Res<GattCommunicationStatus> {
            let w = DataWriter::new().map_err(err)?;
            w.WriteBytes(data).map_err(err)?;
            let buf = w.DetachBuffer().map_err(err)?;
            let opt = if with { GattWriteOption::WriteWithResponse } else { GattWriteOption::WriteWithoutResponse };
            self.ch.WriteValueWithOptionAsync(&buf, opt).and_then(|op| op.join()).map_err(err)
        };
        let st = send(self.with_response)?;
        if st == GattCommunicationStatus::Success {
            return Ok(());
        }
        // Cevaplı yazmayı reddeden ışıklar: cevapsız dene, tutarsa öyle devam et.
        if self.with_response && self.without_ok && send(false)? == GattCommunicationStatus::Success {
            self.with_response = false;
            return Ok(());
        }
        Err(gatt_error(st))
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        if let Some((ch, token)) = self.notify.take() {
            let _ = ch.RemoveValueChanged(token);
        }
        let _ = self.device.RemoveConnectionStatusChanged(self.status_token);
        let _ = self.service.Close();
        if let Some(s) = self.session.take() {
            let _ = s.Close();
        }
        let _ = self.device.Close();
        ortak::log!("lights: bağlantı kapandı · {}", self.name);
    }
}

/// Işık uzaklaşınca / dönünce durumdaki `reachable` değişir.
fn status_handler(device: &BluetoothLEDevice, hwnd: usize, status: &Arc<Mutex<Status>>) -> Option<i64> {
    // Olay iş parçacığı havuzundan gelir; durum paylaşılan kopyada.
    let status = status.clone();
    let handler = TypedEventHandler::<BluetoothLEDevice, windows_core::IInspectable>::new(move |dev, _| {
        if let Some(d) = dev.as_ref() {
            let up = d.ConnectionStatus()? == BluetoothConnectionStatus::Connected;
            let mut s = status.lock().unwrap();
            if s.state == State::Connected && s.reachable != up {
                s.reachable = up;
                drop(s);
                ortak::log!("lights: {}", if up { "ışık yeniden ulaşılabilir" } else { "ışığa ulaşılamıyor" });
                post(hwnd);
            }
        }
        Ok(())
    });
    device.ConnectionStatusChanged(&handler).ok()
}

/// Işığın cevaplarını loga yazar (protokolü anlamak için).
fn subscribe(service: &GattDeviceService, uuid: windows_core::GUID) -> Option<(GattCharacteristic, i64)> {
    let list = service.GetCharacteristicsForUuidAsync(uuid).ok()?.join().ok()?.Characteristics().ok()?;
    let ch = list.GetAt(0).ok()?;
    let handler = TypedEventHandler::<GattCharacteristic, GattValueChangedEventArgs>::new(|_, args| {
        if let Some(a) = args.as_ref() {
            let buf = a.CharacteristicValue()?;
            let mut v = vec![0u8; buf.Length()? as usize];
            DataReader::FromBuffer(&buf)?.ReadBytes(&mut v)?;
            ortak::log!("lights: ışık cevabı · {}", v.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "));
        }
        Ok(())
    });
    let token = ch.ValueChanged(&handler).ok()?;
    let ok = ch
        .WriteClientCharacteristicConfigurationDescriptorAsync(GattClientCharacteristicConfigurationDescriptorValue::Notify)
        .and_then(|op| op.join())
        .ok()
        == Some(GattCommunicationStatus::Success);
    if !ok {
        let _ = ch.RemoveValueChanged(token);
        return None;
    }
    Some((ch, token))
}

// --- Komut satırı ---

/// `winloom --lights-test`: her modelin örnek paketleri, sonra 10 saniyelik tarama (yakındaki
/// adlı Bluetooth cihazları, desteklenenler işaretli). Işık olmadan da çalışır.
pub fn cli_test() -> i32 {
    for m in surucu::MODELS {
        println!("{} paketleri:", m.name);
        for cmd in [Cmd::Power(true), Cmd::Color([255, 0, 0]), Cmd::Brightness(0.5), Cmd::Effect(1), Cmd::Speed(0.5), Cmd::Reverse(false)] {
            if let Some(p) = (m.encode)(cmd) {
                println!("  {cmd:?}: {}", p.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "));
            }
        }
    }
    println!("\n10 saniye taranıyor…");
    let mut names = std::collections::BTreeMap::new();
    let r = scan(SCAN, |a, n| {
        if names.insert(a, n.to_string()).is_none() {
            let mark = if surucu::for_name(n).is_some() { "  ← desteklenen ışık" } else { "" };
            println!("  {a:012X}  {n}{mark}");
        }
    });
    match r {
        Ok(Some((a, n, m))) => {
            println!("\nBulundu: {n} ({a:012X}, model {}). Bağlanılıyor…", m.id);
            let status = Arc::new(Mutex::new(Status { model: Some(m), ..Default::default() }));
            match Conn::open(a, &status, 0) {
                Ok(c) => {
                    println!("Bağlandı. Komut kanalı: {:?}, cevaplı yazma: {}", m.write, c.with_response);
                    0
                }
                Err(e) => {
                    println!("Bağlanılamadı: {e}");
                    1
                }
            }
        }
        Ok(None) => {
            println!("\nDesteklenen ışık bulunamadı ({} cihaz görüldü).", names.len());
            0
        }
        Err(e) => {
            println!("\n{e}");
            1
        }
    }
}
