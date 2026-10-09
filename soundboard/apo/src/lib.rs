//! soundboard APO: mikrofonun ses zincirine takılan uç nokta efekti (EFX).
//!
//! audiodg.exe içinde, mikrofonu açan her uygulamadan önce çalışır. Kurulumda yerini aldığı
//! sürücü efekti varsa (Realtek gürültü engelleme vb.) onu içinde çalıştırır ve her çağrıyı
//! ona iletir; ardından `bus`taki çalan sesleri mikrofon sinyaline ekler.
//!
//! COM nesnesi elle kurulur: biçim anlaşmasında S_FALSE ile öneri döndürmek ve iç efektin
//! HRESULT'larını olduğu gibi iletmek gerekiyor. Gerçek zamanlı yolda (`APOProcess`) bellek
//! ayırma, kilit ve dosya işi yok; panik audiodg'yi düşürür, o yüzden dizinleme de yok.

#[path = "../../src/bus.rs"]
mod bus;
mod log;

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::Mutex;
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU32, AtomicU64};

use windows::Win32::Foundation::*;
use windows::Win32::Media::Audio::Apo::*;
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::*;
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::core::{BOOL, GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, PCWSTR};

use bus::{Bus, Mixer};

/// {1da5d803-d492-4edd-8c23-e0c0ffee7f0e},4: uç noktanın kayıt defterindeki GUID'i.
const PKEY_AUDIOENDPOINT_GUID: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0x1da5d803_d492_4edd_8c23_e0c0ffee7f0e), pid: 4 };

// audiodg içindeki tüm örnekler aynı eşlemi paylaşır; bir kez açılır, hiç kapanmaz.
static BUS: AtomicPtr<Bus> = AtomicPtr::new(null_mut());
static BUS_WRITABLE: AtomicBool = AtomicBool::new(false);
static BUS_OPENING: Mutex<()> = Mutex::new(());

fn bus() -> Option<&'static Bus> {
    unsafe { BUS.load(Acquire).as_ref() }
}

fn ensure_bus() {
    let _guard = BUS_OPENING.lock();
    if bus().is_some() {
        return;
    }
    match bus::open() {
        Ok((b, writable)) => {
            BUS_WRITABLE.store(writable, Relaxed);
            BUS.store(b as *const Bus as *mut Bus, Release);
            log!("bus açıldı ({})", if writable { "okuma/yazma" } else { "salt okunur" });
        }
        Err(e) => log!("bus açılamadı: {e}"),
    }
}

/// Yerini aldığımız sürücü efekti.
struct Child {
    apo: IAudioProcessingObject,
    rt: IAudioProcessingObjectRT,
    cfg: IAudioProcessingObjectConfiguration,
    fx: Option<IAudioSystemEffects2>,
}

#[derive(Default)]
struct State {
    child: Option<Child>,
    channels: usize,
    rate: f32,
    mixer: Mixer,
    /// Uç nokta GUID'inin özeti (bilinmiyorsa 0): ana ve yedek örnekler bununla buluşur.
    endpoint: u64,
}

// --- Ana/yedek eşgüdümü ---
//
// Ana örnek (SFX) her çağrıda uç noktası için "şimdi çalıştım" yazar. Yedek örnek (EFX) aynı
// uç noktada ana örnek son 50 ms içinde çalıştıysa karıştırmaz: o akış zaten SFX'ten geçiyor.
// Aynı audiodg sürecindeler; tablo kilitsiz.

struct Seen {
    endpoint: AtomicU64,
    at: AtomicI64,
}

static SEEN: [Seen; 16] = [const { Seen { endpoint: AtomicU64::new(0), at: AtomicI64::new(0) } }; 16];
static QPC_FREQ: AtomicI64 = AtomicI64::new(0);

fn endpoint_key(guid: &str) -> u64 {
    // FNV-1a; 0 "bilinmiyor" demek.
    guid.to_ascii_lowercase()
        .bytes()
        .fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
        .max(1)
}

fn mark_main(endpoint: u64) {
    if endpoint == 0 {
        return;
    }
    let now = bus::qpc();
    for s in &SEEN {
        let e = s.endpoint.load(Relaxed);
        if e == endpoint || (e == 0 && s.endpoint.compare_exchange(0, endpoint, Relaxed, Relaxed).is_ok_and(|_| true)) {
            s.at.store(now, Relaxed);
            return;
        }
    }
}

fn main_active(endpoint: u64) -> bool {
    let freq = QPC_FREQ.load(Relaxed).max(1);
    SEEN.iter().any(|s| s.endpoint.load(Relaxed) == endpoint && (bus::qpc() - s.at.load(Relaxed)) * 20 < freq)
}

/// Arayüz işaretçileri nesnenin başında: her arayüz kendi vtable alanını gösterir.
///
/// audiodg APO'ları COM toplamasıyla (aggregation) oluşturur: arayüzlerin IUnknown'ı dıştaki
/// nesneye (`outer`) yönlenir, gerçek sayım ve arayüz sorgusu `inner` üzerinden yapılır.
/// Toplanmadan oluşturulunca `outer` kendi `inner` alanımızı gösterir.
#[repr(C)]
struct Apo {
    apo: &'static IAudioProcessingObject_Vtbl,
    rt: &'static IAudioProcessingObjectRT_Vtbl,
    cfg: &'static IAudioProcessingObjectConfiguration_Vtbl,
    fx: &'static IAudioSystemEffects2_Vtbl,
    inner: &'static IUnknown_Vtbl,
    outer: *mut c_void,
    refs: AtomicU32,
    /// `APO_CLSID_FALLBACK` ile oluşturuldu: ana örnek çalışıyorsa susar.
    fallback: bool,
    // Motor, yapılandırma çağrılarını işleme sırasında yapmaz (LockForProcess sözleşmesi).
    state: UnsafeCell<State>,
}

const SLOT_APO: usize = 0;
const SLOT_RT: usize = 1;
const SLOT_CFG: usize = 2;
const SLOT_FX: usize = 3;
const SLOT_INNER: usize = 4;

unsafe fn obj<'a, const S: usize>(this: *mut c_void) -> &'a Apo {
    unsafe { &*(this as *const *const c_void).sub(S).cast::<Apo>() }
}

unsafe fn state<'a, const S: usize>(this: *mut c_void) -> &'a mut State {
    unsafe { &mut *obj::<S>(this).state.get() }
}

fn slot_ptr(o: &Apo, slot: usize) -> *mut c_void {
    unsafe { (o as *const Apo as *mut *const c_void).add(slot).cast() }
}

// --- Gerçek IUnknown (toplanmada dıştaki nesnenin tuttuğu) ---

unsafe extern "system" fn inner_query_interface(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    unsafe {
        if iid.is_null() || out.is_null() {
            return E_POINTER;
        }
        let o = obj::<SLOT_INNER>(this);
        let iid = *iid;
        let slot = if iid == IUnknown::IID {
            SLOT_INNER
        } else if iid == IAudioProcessingObject::IID {
            SLOT_APO
        } else if iid == IAudioProcessingObjectRT::IID {
            SLOT_RT
        } else if iid == IAudioProcessingObjectConfiguration::IID {
            SLOT_CFG
        } else if iid == IAudioSystemEffects::IID || iid == IAudioSystemEffects2::IID {
            SLOT_FX
        } else {
            *out = null_mut();
            return E_NOINTERFACE;
        };
        let p = slot_ptr(o, slot);
        // Arayüzün kendi AddRef'i: toplanmışsa dıştaki nesneyi sayar.
        ((**(p as *const *const IUnknown_Vtbl)).AddRef)(p);
        *out = p;
        S_OK
    }
}

unsafe extern "system" fn inner_add_ref(this: *mut c_void) -> u32 {
    unsafe { obj::<SLOT_INNER>(this).refs.fetch_add(1, Relaxed) + 1 }
}

unsafe extern "system" fn inner_release(this: *mut c_void) -> u32 {
    unsafe {
        let o = obj::<SLOT_INNER>(this);
        let left = o.refs.fetch_sub(1, AcqRel) - 1;
        if left == 0 {
            drop(Box::from_raw(o as *const Apo as *mut Apo));
        }
        left
    }
}

static INNER_VTBL: IUnknown_Vtbl =
    IUnknown_Vtbl { QueryInterface: inner_query_interface, AddRef: inner_add_ref, Release: inner_release };

// --- Arayüzlerin IUnknown'ı: dıştaki nesneye yönlenir ---

unsafe fn outer(this: *mut c_void, s: usize) -> (*mut c_void, &'static IUnknown_Vtbl) {
    unsafe {
        let o = &*(this as *const *const c_void).sub(s).cast::<Apo>();
        (o.outer, &**(o.outer as *const *const IUnknown_Vtbl))
    }
}

unsafe extern "system" fn query_interface<const S: usize>(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    unsafe {
        let (p, v) = outer(this, S);
        (v.QueryInterface)(p, iid, out)
    }
}

unsafe extern "system" fn add_ref<const S: usize>(this: *mut c_void) -> u32 {
    unsafe {
        let (p, v) = outer(this, S);
        (v.AddRef)(p)
    }
}

unsafe extern "system" fn release<const S: usize>(this: *mut c_void) -> u32 {
    unsafe {
        let (p, v) = outer(this, S);
        (v.Release)(p)
    }
}

const fn unknown<const S: usize>() -> IUnknown_Vtbl {
    IUnknown_Vtbl { QueryInterface: query_interface::<S>, AddRef: add_ref::<S>, Release: release::<S> }
}

// --- IAudioProcessingObject ---

unsafe fn reset_(this: *mut c_void) -> HRESULT {
    unsafe {
        match &state::<SLOT_APO>(this).child {
            Some(c) => c.apo.Reset().into(),
            None => S_OK,
        }
    }
}

unsafe fn get_latency_(this: *mut c_void, out: *mut i64) -> HRESULT {
    unsafe {
        if out.is_null() {
            return E_POINTER;
        }
        match &state::<SLOT_APO>(this).child {
            Some(c) => (c.apo.vtable().GetLatency)(c.apo.as_raw(), out),
            None => {
                *out = 0;
                S_OK
            }
        }
    }
}

fn copy_wide(dst: &mut [u16; 256], s: &str) {
    dst.fill(0);
    for (d, c) in dst.iter_mut().take(255).zip(s.encode_utf16()) {
        *d = c;
    }
}

unsafe fn get_registration_properties_(this: *mut c_void, out: *mut *mut APO_REG_PROPERTIES) -> HRESULT {
    unsafe {
        if out.is_null() {
            return E_POINTER;
        }
        let p = CoTaskMemAlloc(size_of::<APO_REG_PROPERTIES>()) as *mut APO_REG_PROPERTIES;
        if p.is_null() {
            return E_OUTOFMEMORY;
        }
        // İç efekt varsa onun bayrakları geçerli (yerinde çalışmayabilir, kanal sayısı değiştirebilir).
        let child = state::<SLOT_APO>(this).child.as_ref().and_then(|c| c.apo.GetRegistrationProperties().ok());
        let mut props = match child {
            Some(cp) => {
                let copy = *cp;
                CoTaskMemFree(Some(cp as *const c_void));
                copy
            }
            None => APO_REG_PROPERTIES {
                Flags: APO_FLAG(APO_FLAG_INPLACE.0 | APO_FLAG_DEFAULT.0),
                u32MinInputConnections: 1,
                u32MaxInputConnections: 1,
                u32MinOutputConnections: 1,
                u32MaxOutputConnections: 1,
                u32MaxInstances: u32::MAX,
                ..Default::default()
            },
        };
        props.clsid = if obj::<SLOT_APO>(this).fallback { bus::APO_CLSID_FALLBACK } else { bus::APO_CLSID };
        copy_wide(&mut props.szFriendlyName, "lyrebird");
        copy_wide(&mut props.szCopyrightInfo, "MIT");
        props.u32MajorVersion = 1;
        props.u32MinorVersion = 0;
        props.u32NumAPOInterfaces = 1;
        props.iidAPOInterfaceList = [IAudioProcessingObject::IID];
        p.write(props);
        *out = p;
        S_OK
    }
}

/// `APOInitSystemEffects`, `2` ve `3`'ün ortak başı: uç nokta özellikleri hep aynı yerde.
#[repr(C)]
struct InitPrefix {
    base: APOInitBaseStruct,
    endpoint: *mut c_void,
}

unsafe fn endpoint_guid(store: &IPropertyStore) -> Option<String> {
    unsafe {
        let value = store.GetValue(&PKEY_AUDIOENDPOINT_GUID).ok()?;
        let p = PropVariantToStringAlloc(&value).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

fn saved_clsid(endpoint: &str) -> Option<GUID> {
    let key: Vec<u16> = bus::fx_key(endpoint).encode_utf16().chain(Some(0)).collect();
    let value: Vec<u16> = bus::saved_value().encode_utf16().chain(Some(0)).collect();
    let mut buf = [0u16; 64];
    let mut len = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
    }
    let s = String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]);
    bus::parse_guid(&s).filter(|g| *g != bus::APO_CLSID && *g != bus::APO_CLSID_FALLBACK)
}

unsafe fn load_child(clsid: &GUID, cb: u32, data: *const u8) -> windows::core::Result<Child> {
    unsafe {
        let apo: IAudioProcessingObject = CoCreateInstance(clsid, None, CLSCTX_INPROC_SERVER)?;
        // İç efekt kendi CLSID'ini bekler: başlatma verisinin kopyasında onu yaz.
        let mut copy = std::slice::from_raw_parts(data, cb as usize).to_vec();
        std::ptr::addr_of_mut!((*(copy.as_mut_ptr() as *mut APOInitBaseStruct)).clsid).write_unaligned(*clsid);
        apo.Initialize(&copy)?;
        Ok(Child { rt: apo.cast()?, cfg: apo.cast()?, fx: apo.cast().ok(), apo })
    }
}

unsafe fn initialize_(this: *mut c_void, cb: u32, data: *const u8) -> HRESULT {
    unsafe {
        if data.is_null() || (cb as usize) < size_of::<APOInitBaseStruct>() {
            return E_INVALIDARG;
        }
        let st = state::<SLOT_APO>(this);
        if (cb as usize) >= size_of::<InitPrefix>() {
            let prefix = &*(data as *const InitPrefix);
            let endpoint = IPropertyStore::from_raw_borrowed(&prefix.endpoint).and_then(|s| endpoint_guid(s));
            let saved = endpoint.as_deref().and_then(saved_clsid);
            st.endpoint = endpoint.as_deref().map_or(0, endpoint_key);
            // Sürüm yapının boyutundan anlaşılır; mod ve "yalnızca keşif" bayrağı sonda.
            let tail = if cb as usize == size_of::<APOInitSystemEffects2>() {
                let i = &*(data as *const APOInitSystemEffects2);
                format!(
                    "v2, mod {}, keşif {}",
                    bus::guid_str(&i.AudioProcessingMode),
                    i.InitializeForDiscoveryOnly.as_bool()
                )
            } else if cb as usize == size_of::<APOInitSystemEffects3>() {
                let i = &*(data as *const APOInitSystemEffects3);
                format!(
                    "v3, mod {}, keşif {}",
                    bus::guid_str(&i.AudioProcessingMode),
                    i.InitializeForDiscoveryOnly.as_bool()
                )
            } else {
                format!("{cb} bayt")
            };
            log!(
                "başlatılıyor: uç nokta {}, iç efekt {}, {tail}",
                endpoint.as_deref().unwrap_or("?"),
                saved.map_or("yok".into(), |g| bus::guid_str(&g))
            );
            if let Some(clsid) = saved {
                match load_child(&clsid, cb, data) {
                    Ok(child) => st.child = Some(child),
                    Err(e) => log!("iç efekt yüklenemedi, yalnızca lyrebird çalışacak: {e}"),
                }
            }
        }
        ensure_bus();
        let mut f = 0;
        let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut f);
        QPC_FREQ.store(f, Relaxed);
        S_OK
    }
}

struct Format {
    float: bool,
    channels: u32,
    rate: f32,
}

unsafe fn format(t: &IAudioMediaType) -> Option<Format> {
    unsafe {
        let mut f = UNCOMPRESSEDAUDIOFORMAT::default();
        t.GetUncompressedAudioFormat(&mut f).ok()?;
        Some(Format {
            float: f.guidFormatType == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT && f.dwBytesPerSampleContainer == 4,
            channels: f.dwSamplesPerFrame,
            rate: f.fFramesPerSecond,
        })
    }
}

fn describe(p: *mut c_void) -> String {
    match unsafe { IAudioMediaType::from_raw_borrowed(&p).and_then(|t| format(t)) } {
        Some(f) => format!("{}{}ch/{}", if f.float { "f32 " } else { "int " }, f.channels, f.rate),
        None => "-".into(),
    }
}

/// İç efekt yokken: yerinde çalışırız, giriş ve çıkış aynı float biçimde olmalı.
unsafe fn own_format(opposite: *mut c_void, requested: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    let hr = unsafe { own_format_inner(opposite, requested, out) };
    if hr != S_OK {
        log!("biçim: istenen {}, karşı {} → {:?}", describe(requested), describe(opposite), hr);
    }
    hr
}

unsafe fn own_format_inner(opposite: *mut c_void, requested: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    unsafe {
        if out.is_null() {
            return E_POINTER;
        }
        *out = null_mut();
        let Some(req) = IAudioMediaType::from_raw_borrowed(&requested) else {
            return E_POINTER;
        };
        let Some(rf) = format(req) else {
            return APOERR_FORMAT_NOT_SUPPORTED;
        };
        if let Some(opp) = IAudioMediaType::from_raw_borrowed(&opposite) {
            let Some(of) = format(opp) else {
                return APOERR_FORMAT_NOT_SUPPORTED;
            };
            if !rf.float || rf.channels != of.channels || rf.rate != of.rate {
                if !of.float {
                    return APOERR_FORMAT_NOT_SUPPORTED;
                }
                *out = opp.clone().into_raw();
                return S_FALSE;
            }
        } else if !rf.float {
            return APOERR_FORMAT_NOT_SUPPORTED;
        }
        *out = req.clone().into_raw();
        S_OK
    }
}

unsafe fn is_input_format_supported_(
    this: *mut c_void,
    opposite: *mut c_void,
    requested: *mut c_void,
    out: *mut *mut c_void,
) -> HRESULT {
    unsafe {
        match &state::<SLOT_APO>(this).child {
            Some(c) => (c.apo.vtable().IsInputFormatSupported)(c.apo.as_raw(), opposite, requested, out),
            None => own_format(opposite, requested, out),
        }
    }
}

unsafe fn is_output_format_supported_(
    this: *mut c_void,
    opposite: *mut c_void,
    requested: *mut c_void,
    out: *mut *mut c_void,
) -> HRESULT {
    unsafe {
        match &state::<SLOT_APO>(this).child {
            Some(c) => (c.apo.vtable().IsOutputFormatSupported)(c.apo.as_raw(), opposite, requested, out),
            None => own_format(opposite, requested, out),
        }
    }
}

unsafe fn get_input_channel_count_(this: *mut c_void, out: *mut u32) -> HRESULT {
    unsafe {
        if out.is_null() {
            return E_POINTER;
        }
        let st = state::<SLOT_APO>(this);
        match &st.child {
            Some(c) => (c.apo.vtable().GetInputChannelCount)(c.apo.as_raw(), out),
            None => {
                *out = st.channels as u32;
                S_OK
            }
        }
    }
}

// Teşhis: başarısız yapılandırma çağrıları apo.log'a (gerçek zamanlı yol hariç).
unsafe extern "system" fn reset(this: *mut c_void) -> HRESULT {
    let hr = unsafe { reset_(this) };
    if hr.is_err() {
        log!("reset başarısız: {hr:?}");
    }
    hr
}

unsafe extern "system" fn get_latency(this: *mut c_void, out: *mut i64) -> HRESULT {
    let hr = unsafe { get_latency_(this, out) };
    if hr.is_err() {
        log!("get_latency başarısız: {hr:?}");
    }
    hr
}

unsafe extern "system" fn get_registration_properties(this: *mut c_void, out: *mut *mut APO_REG_PROPERTIES) -> HRESULT {
    let hr = unsafe { get_registration_properties_(this, out) };
    if hr.is_err() {
        log!("get_registration_properties başarısız: {hr:?}");
    }
    hr
}

unsafe extern "system" fn initialize(this: *mut c_void, cb: u32, data: *const u8) -> HRESULT {
    let hr = unsafe { initialize_(this, cb, data) };
    if hr.is_err() {
        log!("initialize başarısız: {hr:?}");
    }
    hr
}

unsafe extern "system" fn is_input_format_supported(
    this: *mut c_void,
    opposite: *mut c_void,
    requested: *mut c_void,
    out: *mut *mut c_void,
) -> HRESULT {
    let hr = unsafe { is_input_format_supported_(this, opposite, requested, out) };
    if hr.is_err() {
        log!("is_input_format_supported başarısız: {hr:?}");
    }
    hr
}

unsafe extern "system" fn is_output_format_supported(
    this: *mut c_void,
    opposite: *mut c_void,
    requested: *mut c_void,
    out: *mut *mut c_void,
) -> HRESULT {
    let hr = unsafe { is_output_format_supported_(this, opposite, requested, out) };
    if hr.is_err() {
        log!("is_output_format_supported başarısız: {hr:?}");
    }
    hr
}

unsafe extern "system" fn get_input_channel_count(this: *mut c_void, out: *mut u32) -> HRESULT {
    let hr = unsafe { get_input_channel_count_(this, out) };
    if hr.is_err() {
        log!("get_input_channel_count başarısız: {hr:?}");
    }
    hr
}

unsafe extern "system" fn lock_for_process(
    this: *mut c_void,
    n_in: u32,
    inputs: *const *const APO_CONNECTION_DESCRIPTOR,
    n_out: u32,
    outputs: *const *const APO_CONNECTION_DESCRIPTOR,
) -> HRESULT {
    let hr = unsafe { lock_for_process_(this, n_in, inputs, n_out, outputs) };
    if hr.is_err() {
        log!("lock_for_process başarısız: {hr:?}");
    }
    hr
}

unsafe extern "system" fn unlock_for_process(this: *mut c_void) -> HRESULT {
    let hr = unsafe { unlock_for_process_(this) };
    if hr.is_err() {
        log!("unlock_for_process başarısız: {hr:?}");
    }
    hr
}

unsafe extern "system" fn get_effects_list(
    this: *mut c_void,
    ids: *mut *mut GUID,
    count: *mut u32,
    event: HANDLE,
) -> HRESULT {
    let hr = unsafe { get_effects_list_(this, ids, count, event) };
    if hr.is_err() {
        log!("get_effects_list başarısız: {hr:?}");
    }
    hr
}

static APO_VTBL: IAudioProcessingObject_Vtbl = IAudioProcessingObject_Vtbl {
    base__: unknown::<SLOT_APO>(),
    Reset: reset,
    GetLatency: get_latency,
    GetRegistrationProperties: get_registration_properties,
    Initialize: initialize,
    IsInputFormatSupported: is_input_format_supported,
    IsOutputFormatSupported: is_output_format_supported,
    GetInputChannelCount: get_input_channel_count,
};

// --- IAudioProcessingObjectConfiguration ---

unsafe fn lock_for_process_(
    this: *mut c_void,
    n_in: u32,
    inputs: *const *const APO_CONNECTION_DESCRIPTOR,
    n_out: u32,
    outputs: *const *const APO_CONNECTION_DESCRIPTOR,
) -> HRESULT {
    unsafe {
        let st = state::<SLOT_CFG>(this);
        if let Some(c) = &st.child {
            let hr = (c.cfg.vtable().LockForProcess)(c.cfg.as_raw(), n_in, inputs, n_out, outputs);
            if hr.is_err() {
                return hr;
            }
        }
        if n_out < 1 || outputs.is_null() || (*outputs).is_null() {
            return APOERR_NUM_CONNECTIONS_INVALID;
        }
        let f = (*(*outputs)).pFormat.as_ref().and_then(|t| format(t));
        (st.channels, st.rate) = match f {
            Some(f) if f.float => (f.channels as usize, f.rate),
            // Float olmayan bir çıkışa (olmamalı) dokunmayız.
            _ => (0, 0.0),
        };
        st.mixer = Mixer::default();
        ensure_bus();
        log!(
            "kilitlendi: {} kanal, {} Hz{}",
            st.channels,
            st.rate,
            if st.child.is_some() { ", iç efektle" } else { "" }
        );
        S_OK
    }
}

unsafe fn unlock_for_process_(this: *mut c_void) -> HRESULT {
    unsafe {
        let st = state::<SLOT_CFG>(this);
        st.channels = 0;
        match &st.child {
            Some(c) => c.cfg.UnlockForProcess().into(),
            None => S_OK,
        }
    }
}

static CFG_VTBL: IAudioProcessingObjectConfiguration_Vtbl = IAudioProcessingObjectConfiguration_Vtbl {
    base__: unknown::<SLOT_CFG>(),
    LockForProcess: lock_for_process,
    UnlockForProcess: unlock_for_process,
};

// --- IAudioProcessingObjectRT: gerçek zamanlı yol ---

unsafe extern "system" fn apo_process(
    this: *mut c_void,
    n_in: u32,
    inputs: *const *const APO_CONNECTION_PROPERTY,
    n_out: u32,
    outputs: *mut *mut APO_CONNECTION_PROPERTY,
) {
    unsafe {
        let st = state::<SLOT_RT>(this);
        if n_out < 1 || outputs.is_null() || (*outputs).is_null() {
            return;
        }
        let out = &mut **outputs;
        match &st.child {
            Some(c) => (c.rt.vtable().APOProcess)(c.rt.as_raw(), n_in, inputs, n_out, outputs),
            None => {
                if n_in < 1 || inputs.is_null() || (*inputs).is_null() {
                    return;
                }
                let inp = &**inputs;
                if inp.pBuffer != out.pBuffer && st.channels > 0 {
                    let n = inp.u32ValidFrameCount as usize * st.channels;
                    std::ptr::copy_nonoverlapping(inp.pBuffer as *const f32, out.pBuffer as *mut f32, n);
                }
                out.u32ValidFrameCount = inp.u32ValidFrameCount;
                out.u32BufferFlags = inp.u32BufferFlags;
            }
        }
        let Some(bus) = bus() else { return };
        if st.channels == 0 || out.pBuffer == 0 {
            return;
        }
        let quiet = obj::<SLOT_RT>(this).fallback && main_active(st.endpoint);
        if !obj::<SLOT_RT>(this).fallback {
            mark_main(st.endpoint);
        }
        if BUS_WRITABLE.load(Relaxed) && !quiet {
            bus.beat();
        }
        let silent = out.u32BufferFlags == BUFFER_SILENT;
        if silent && !st.mixer.busy(bus) {
            return;
        }
        let buf =
            std::slice::from_raw_parts_mut(out.pBuffer as *mut f32, out.u32ValidFrameCount as usize * st.channels);
        if quiet {
            // Yalnızca imleçleri ilerlet; tampona dokunma.
            st.mixer.mix(bus, buf, st.channels, st.rate, 0.0);
            return;
        }
        if silent {
            buf.fill(0.0);
        }
        if st.mixer.mix(bus, buf, st.channels, st.rate, bus.gain()) {
            out.u32BufferFlags = BUFFER_VALID;
        }
    }
}

unsafe extern "system" fn calc_input_frames(this: *mut c_void, frames: u32) -> u32 {
    unsafe {
        match &state::<SLOT_RT>(this).child {
            Some(c) => c.rt.CalcInputFrames(frames),
            None => frames,
        }
    }
}

unsafe extern "system" fn calc_output_frames(this: *mut c_void, frames: u32) -> u32 {
    unsafe {
        match &state::<SLOT_RT>(this).child {
            Some(c) => c.rt.CalcOutputFrames(frames),
            None => frames,
        }
    }
}

static RT_VTBL: IAudioProcessingObjectRT_Vtbl = IAudioProcessingObjectRT_Vtbl {
    base__: unknown::<SLOT_RT>(),
    APOProcess: apo_process,
    CalcInputFrames: calc_input_frames,
    CalcOutputFrames: calc_output_frames,
};

// --- IAudioSystemEffects2: Windows'un efekt listesi iç efektten gelir ---

unsafe fn get_effects_list_(this: *mut c_void, ids: *mut *mut GUID, count: *mut u32, event: HANDLE) -> HRESULT {
    unsafe {
        if ids.is_null() || count.is_null() {
            return E_POINTER;
        }
        match state::<SLOT_FX>(this).child.as_ref().and_then(|c| c.fx.as_ref()) {
            Some(fx) => (fx.vtable().GetEffectsList)(fx.as_raw(), ids, count, event),
            None => {
                *ids = null_mut();
                *count = 0;
                S_OK
            }
        }
    }
}

static FX_VTBL: IAudioSystemEffects2_Vtbl = IAudioSystemEffects2_Vtbl {
    base__: IAudioSystemEffects_Vtbl { base__: unknown::<SLOT_FX>() },
    GetEffectsList: get_effects_list,
};

// --- Sınıf fabrikası ve DLL girişleri ---

#[repr(C)]
struct Factory {
    vtbl: &'static IClassFactory_Vtbl,
    fallback: bool,
}

static FACTORY: Factory = Factory { vtbl: &FACTORY_VTBL, fallback: false };
static FACTORY_FALLBACK: Factory = Factory { vtbl: &FACTORY_VTBL, fallback: true };

unsafe extern "system" fn factory_qi(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    unsafe {
        if iid.is_null() || out.is_null() {
            return E_POINTER;
        }
        if *iid == IUnknown::IID || *iid == IClassFactory::IID {
            *out = this;
            S_OK
        } else {
            *out = null_mut();
            E_NOINTERFACE
        }
    }
}

// Fabrika statik: sayım gerekmez.
unsafe extern "system" fn factory_ref(_: *mut c_void) -> u32 {
    1
}

unsafe extern "system" fn create_instance(
    factory: *mut c_void,
    outer: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    unsafe {
        if out.is_null() {
            return E_POINTER;
        }
        *out = null_mut();
        // Toplanırken yalnızca gerçek IUnknown istenebilir.
        if !outer.is_null() && (iid.is_null() || *iid != IUnknown::IID) {
            return CLASS_E_NOAGGREGATION;
        }
        let apo = Box::into_raw(Box::new(Apo {
            apo: &APO_VTBL,
            rt: &RT_VTBL,
            cfg: &CFG_VTBL,
            fx: &FX_VTBL,
            inner: &INNER_VTBL,
            outer,
            refs: AtomicU32::new(1),
            fallback: (*(factory as *const Factory)).fallback,
            state: UnsafeCell::new(State::default()),
        }));
        let inner = slot_ptr(&*apo, SLOT_INNER);
        if outer.is_null() {
            (*apo).outer = inner;
        }
        let hr = inner_query_interface(inner, iid, out);
        inner_release(inner);
        hr
    }
}

unsafe extern "system" fn lock_server(_: *mut c_void, _: BOOL) -> HRESULT {
    S_OK
}

static FACTORY_VTBL: IClassFactory_Vtbl = IClassFactory_Vtbl {
    base__: IUnknown_Vtbl { QueryInterface: factory_qi, AddRef: factory_ref, Release: factory_ref },
    CreateInstance: create_instance,
    LockServer: lock_server,
};

#[unsafe(no_mangle)]
unsafe extern "system" fn DllGetClassObject(clsid: *const GUID, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    unsafe {
        let factory = match clsid.as_ref() {
            Some(c) if *c == bus::APO_CLSID => &FACTORY,
            Some(c) if *c == bus::APO_CLSID_FALLBACK => &FACTORY_FALLBACK,
            _ => return CLASS_E_CLASSNOTAVAILABLE,
        };
        factory_qi(factory as *const Factory as *mut c_void, iid, out)
    }
}

/// Paylaşılan eşlem ve çalışan örnekler yüzünden DLL audiodg kapanana dek yüklü kalır.
#[unsafe(no_mangle)]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}
