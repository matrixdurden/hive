//! Mikrofona bağlama: APO'yu Program Files'a yazar, kaydeder ve her mikrofonun efekt zincirine
//! takar. Yönetici ister: `soundboard --kur` / `--kaldir` yükseltilmiş olarak çalışır.
//!
//! Zincirde nereye takılacağı (yakalamada sıra: EFX → MFX → SFX → uygulama):
//!
//! - Bileşik sürücü, SFX boş: soundboard SFX'te tek başına ve SFX mod listesi cihazın tüm
//!   modlarına genişletilir. Ses sürücünün tüm işlemesinden (gürültü engelleme, otomatik kazanç)
//!   sonra, her modda (varsayılan, iletişim, raw) girer. En iyi yer.
//! - Bileşik sürücü, SFX'te sürücü efekti var: onun modlarını değiştirmemek için EFX listesinin
//!   sonu. EFX her akışa uygulanır ama ardından gelen MFX sesi işler (uyarlamalı gürültü
//!   engelleme sesi kısabilir).
//! - Tekli sürücü: EFX yuvası tek CLSID. soundboard yuvayı alır; oradaki sürücü efektinin
//!   CLSID'i saklanır, APO onu içinde çalıştırır, kaldırınca geri yazılır.
//!
//! Windows MFX listesindeki üçüncü parti APO'yu kullanmıyor (oluşturup hemen bırakıyor).

use std::path::PathBuf;

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, HANDLE, HWND, LUID, WIN32_ERROR};
use windows::Win32::Media::Audio::*;
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES,
    TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{GUID, PCWSTR, w};

use crate::bus::{self, APO_CLSID, APO_CLSID_FALLBACK, FMTID, guid_str, parse_guid};
use crate::log;
use crate::util::{Res, wide};

static DLL: &[u8] = include_bytes!(env!("SOUNDBOARD_APO_DLL"));

const FX: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d}";
const MODES: &str = "{d3993a3f-99c2-4402-b5ec-a92a0367664b}";
const MODE_DEFAULT: &str = "{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}";
/// "Tüm iyileştirmeleri devre dışı bırak" (PKEY_AudioEndpoint_Disable_SysFx).
const DISABLE_SYSFX: &str = "{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5";
const IAUDIOPROCESSINGOBJECT: &str = "{FD7F2B29-24D0-4B5C-B177-592C39F9CA10}";

/// PKEY_FX_EndpointEffectClsid: tekli EFX yuvası.
fn efx() -> String {
    format!("{FX},7")
}

/// PKEY_CompositeFX_{Stream,Mode,Endpoint}EffectClsid: bileşik zincirler.
fn composite(pid: u32) -> String {
    format!("{FX},{pid}")
}

/// PKEY_{MFX,EFX}_ProcessingModes_Supported_For_Streaming.
fn modes(pid: u32) -> String {
    format!("{MODES},{pid}")
}

fn created_modes() -> String {
    format!("{},2", guid_str(&FMTID).to_lowercase())
}

/// SFX mod listesini soundboard değiştirdi mi; değiştirdiyse önceki liste.
fn created_sfx_modes() -> String {
    format!("{},3", guid_str(&FMTID).to_lowercase())
}

fn saved_sfx_modes() -> String {
    format!("{},4", guid_str(&FMTID).to_lowercase())
}

/// "İyileştirmeleri kapat" soundboard yüzünden kapatıldıysa önceki değeri: FxProperties'teki (5) ve
/// uç noktanın Properties anahtarındaki (6). İkisi de FxProperties'te saklanır.
fn saved_sysfx(n: u32) -> String {
    format!("{},{n}", guid_str(&FMTID).to_lowercase())
}

/// soundboard'ün bir mikrofonun FxProperties'ine yazabildiği kendi değerleri (1..6).
fn own_values() -> Vec<String> {
    (1..=6).map(|n| format!("{},{n}", guid_str(&FMTID).to_lowercase())).collect()
}

const AUDIO_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Audio";
const PROTECTED: &str = "DisableProtectedAudioDG";
/// `DisableProtectedAudioDG`'nin soundboard'den önceki değeri; yoksa ABSENT.
const PROTECTED_SAVED: &str = "lyrebird_DisableProtectedAudioDG";
const ABSENT: u32 = u32::MAX;

pub struct Mic {
    /// Kayıt defterindeki uç nokta GUID'i: `{...}`.
    pub guid: String,
    pub name: String,
    pub device: IMMDevice,
}

unsafe fn mic(device: &IMMDevice) -> Option<Mic> {
    unsafe {
        let p = device.GetId().ok()?;
        let id = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        // {0.0.1.00000000}.{guid}
        let guid = id?.split_once("}.")?.1.to_string();
        // Takılı olmayan cihazın adı okunamayabilir; kaldırırken yine de bulunmalı.
        let name = (|| {
            let value = device.OpenPropertyStore(STGM_READ).ok()?.GetValue(&PKEY_Device_FriendlyName).ok()?;
            let p = PropVariantToStringAlloc(&value).ok()?;
            let name = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            name
        })();
        Some(Mic { guid, name: name.unwrap_or_default(), device: device.clone() })
    }
}

/// COM başlatılmış bir iş parçacığında çağrılmalı.
pub fn mics(state: DEVICE_STATE) -> Res<Vec<Mic>> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let list = enumerator.EnumAudioEndpoints(eCapture, state)?;
        Ok((0..list.GetCount()?).filter_map(|i| list.Item(i).ok()).filter_map(|d| mic(&d)).collect())
    }
}

pub fn default_mic() -> Option<Mic> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        mic(&enumerator.GetDefaultAudioEndpoint(eCapture, eConsole).ok()?)
    }
}

/// Mikrofon efekti Program Files'ta var mı (sürümü ne olursa olsun).
pub fn present() -> bool {
    install_dir().join("lyrebird_apo.dll").is_file()
}

/// Program Files'taki DLL bu sürümle aynı mı? Değilse yeniden bağlamak gerekir.
pub fn current() -> bool {
    std::fs::read(install_dir().join("lyrebird_apo.dll")).is_ok_and(|d| d == DLL)
}

fn ours(s: &str) -> bool {
    parse_guid(s).is_some_and(|g| CLSIDS.contains(&g))
}

/// Bu mikrofonun efekt zincirinde soundboard var mı (yönetici gerekmez).
pub fn attached(guid: &str) -> bool {
    let Ok(k) = Key::read(&bus::fx_key(guid)) else { return false };
    k.get(&efx()).is_some_and(|s| ours(&s))
        || [14, 15].iter().any(|&p| k.multi(&composite(p)).iter().flatten().any(|s| ours(s)))
}

// --- Yükseltilmiş işlem ---

/// Bu exe'yi yönetici olarak `arg` ile çalıştırır ve bitmesini bekler. Başarılıysa `true`.
pub fn run_elevated(arg: &str) -> Res<bool> {
    let exe = wide(&std::env::current_exe()?.to_string_lossy());
    let params = wide(arg);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: HWND::default(),
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info)?;
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        Ok(code == 0)
    }
}

fn check(e: WIN32_ERROR, what: &str) -> Res<()> {
    if e.is_ok() {
        Ok(())
    } else {
        Err(format!("{what}: {}", windows::core::Error::from(e.to_hresult()).message()).into())
    }
}

/// FxProperties anahtarları yöneticiye de kapalı: yedekleme/geri yükleme ayrıcalığıyla açılır.
fn enable_privileges() -> Res<()> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token)?;
        for name in [w!("SeBackupPrivilege"), w!("SeRestorePrivilege")] {
            let mut luid = LUID::default();
            LookupPrivilegeValueW(None, name, &mut luid)?;
            let tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
            };
            AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None)?;
        }
        let _ = CloseHandle(token);
        Ok(())
    }
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

impl Key {
    fn open(path: &str, backup: bool) -> Res<Self> {
        let p = wide(path);
        let mut h = HKEY::default();
        let opts = if backup { REG_OPTION_BACKUP_RESTORE } else { REG_OPTION_NON_VOLATILE };
        let e = unsafe {
            RegCreateKeyExW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(p.as_ptr()),
                None,
                PCWSTR::null(),
                opts,
                KEY_ALL_ACCESS,
                None,
                &mut h,
                None,
            )
        };
        check(e, path)?;
        Ok(Self(h))
    }

    fn read(path: &str) -> Res<Self> {
        let p = wide(path);
        let mut h = HKEY::default();
        check(unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(p.as_ptr()), None, KEY_READ, &mut h) }, path)?;
        Ok(Self(h))
    }

    fn get(&self, name: &str) -> Option<String> {
        let n = wide(name);
        let mut buf = [0u16; 512];
        let mut len = (buf.len() * 2) as u32;
        let e = unsafe {
            RegGetValueW(
                self.0,
                None,
                PCWSTR(n.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut len),
            )
        };
        e.is_ok().then(|| String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]))
    }

    fn multi(&self, name: &str) -> Option<Vec<String>> {
        let n = wide(name);
        let mut buf = [0u16; 2048];
        let mut len = (buf.len() * 2) as u32;
        let e = unsafe {
            RegGetValueW(
                self.0,
                None,
                PCWSTR(n.as_ptr()),
                RRF_RT_REG_MULTI_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut len),
            )
        };
        e.is_ok().then(|| {
            String::from_utf16_lossy(&buf[..len as usize / 2])
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
    }

    fn set_multi(&self, name: &str, items: &[String]) -> Res<()> {
        let mut v: Vec<u16> = Vec::new();
        for s in items {
            v.extend(s.encode_utf16());
            v.push(0);
        }
        v.push(0);
        self.set_raw(name, REG_MULTI_SZ, &v.iter().flat_map(|c| c.to_le_bytes()).collect::<Vec<u8>>())
    }

    fn dword(&self, name: &str) -> Option<u32> {
        let n = wide(name);
        let (mut v, mut len) = (0u32, 4u32);
        let e = unsafe {
            RegGetValueW(
                self.0,
                None,
                PCWSTR(n.as_ptr()),
                RRF_RT_REG_DWORD,
                None,
                Some((&mut v as *mut u32).cast()),
                Some(&mut len),
            )
        };
        e.is_ok().then_some(v)
    }

    fn exists(&self, name: &str) -> bool {
        let n = wide(name);
        unsafe { RegGetValueW(self.0, None, PCWSTR(n.as_ptr()), RRF_RT_ANY, None, None, None).is_ok() }
    }

    fn set_raw(&self, name: &str, kind: REG_VALUE_TYPE, data: &[u8]) -> Res<()> {
        let n = wide(name);
        check(unsafe { RegSetValueExW(self.0, PCWSTR(n.as_ptr()), None, kind, Some(data)) }, name)
    }

    fn set(&self, name: &str, value: &str) -> Res<()> {
        let v = wide(value);
        self.set_raw(name, REG_SZ, unsafe { std::slice::from_raw_parts(v.as_ptr().cast(), v.len() * 2) })
    }

    fn set_dword(&self, name: &str, value: u32) -> Res<()> {
        self.set_raw(name, REG_DWORD, &value.to_le_bytes())
    }

    fn delete(&self, name: &str) {
        let n = wide(name);
        unsafe {
            let _ = RegDeleteValueW(self.0, PCWSTR(n.as_ptr()));
        }
    }
}

fn install_dir() -> PathBuf {
    let base = std::env::var_os("ProgramW6432")
        .or_else(|| std::env::var_os("ProgramFiles"))
        .unwrap_or(r"C:\Program Files".into());
    PathBuf::from(base).join("lyrebird")
}

/// DLL'i yazar. Eskisi audiodg'de yüklüyse üzerine yazılamaz: kenara alınır, sonra silinir.
fn write_dll() -> Res<PathBuf> {
    let dir = install_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("lyrebird_apo.dll");
    if std::fs::read(&path).is_ok_and(|old| old == DLL) {
        return Ok(path);
    }
    if path.exists() {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let old = dir.join(format!("lyrebird_apo.{stamp}.eski"));
        std::fs::rename(&path, &old)?;
        unsafe {
            let _ = MoveFileExW(PCWSTR(wide(&old.to_string_lossy()).as_ptr()), None, MOVEFILE_DELAY_UNTIL_REBOOT);
        }
    }
    std::fs::write(&path, DLL)?;
    Ok(path)
}

fn remove_old_dlls() {
    if let Ok(entries) = std::fs::read_dir(install_dir()) {
        for e in entries.flatten() {
            if e.path().extension().is_some_and(|x| x == "eski") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

const CLSIDS: [GUID; 2] = [APO_CLSID, APO_CLSID_FALLBACK];

fn clsid_key(c: &GUID) -> String {
    format!(r"SOFTWARE\Classes\CLSID\{}", guid_str(c))
}

fn apo_key(c: &GUID) -> String {
    format!(r"SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{}", guid_str(c))
}

fn register(dll: &std::path::Path) -> Res<()> {
    for c in &CLSIDS {
        register_one(dll, c)?;
    }
    Ok(())
}

fn register_one(dll: &std::path::Path, c: &GUID) -> Res<()> {
    Key::open(&clsid_key(c), false)?.set("", "lyrebird")?;
    let inproc = Key::open(&format!(r"{}\InprocServer32", clsid_key(c)), false)?;
    inproc.set("", &dll.to_string_lossy())?;
    inproc.set("ThreadingModel", "Both")?;
    let apo = Key::open(&apo_key(c), false)?;
    apo.set("FriendlyName", "lyrebird")?;
    apo.set("Copyright", "MIT")?;
    for (name, value) in [
        ("MajorVersion", 1),
        ("MinorVersion", 0),
        ("Flags", 0xF), // yerinde + örnek/hız/bit eşleşmeli
        ("MinInputConnections", 1),
        ("MaxInputConnections", 1),
        ("MinOutputConnections", 1),
        ("MaxOutputConnections", 1),
        ("MaxInstances", u32::MAX),
        ("NumAPOInterfaces", 1),
    ] {
        apo.set_dword(name, value)?;
    }
    apo.set("APOInterface0", IAUDIOPROCESSINGOBJECT)
}

fn endpoint_key(guid: &str) -> String {
    format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\{guid}")
}

/// "İyileştirmeleri kapat" açıksa APO'lar hiç yüklenmez: kapatılır, önceki değer saklanır.
fn enable_effects(guid: &str) -> Res<()> {
    let fx = Key::open(&bus::fx_key(guid), true)?;
    for (n, path) in [(5, bus::fx_key(guid)), (6, format!(r"{}\Properties", endpoint_key(guid)))] {
        if let Ok(k) = Key::open(&path, true)
            && let Some(v) = k.dword(DISABLE_SYSFX).filter(|&v| v != 0)
        {
            if fx.dword(&saved_sysfx(n)).is_none() {
                fx.set_dword(&saved_sysfx(n), v)?;
            }
            k.set_dword(DISABLE_SYSFX, 0)?;
        }
    }
    Ok(())
}

/// enable_effects'in değiştirdiğini geri koyar.
fn restore_effects(guid: &str, fx: &Key) -> Res<()> {
    for (n, path) in [(5, bus::fx_key(guid)), (6, format!(r"{}\Properties", endpoint_key(guid)))] {
        if let Some(v) = fx.dword(&saved_sysfx(n)) {
            Key::open(&path, true)?.set_dword(DISABLE_SYSFX, v)?;
            fx.delete(&saved_sysfx(n));
        }
    }
    Ok(())
}

/// İmzasız APO'ların eski Windows'ta yüklenebilmesi için korumalı audiodg kapatılır (Equalizer
/// APO da böyle yapar). Önceki durum saklanır, kaldırınca aynen geri konur.
fn protected_off() -> Res<()> {
    let k = Key::open(AUDIO_KEY, false)?;
    if k.dword(PROTECTED_SAVED).is_none() {
        k.set_dword(PROTECTED_SAVED, k.dword(PROTECTED).unwrap_or(ABSENT))?;
    }
    k.set_dword(PROTECTED, 1)
}

fn protected_restore() -> Res<()> {
    let k = Key::open(AUDIO_KEY, false)?;
    match k.dword(PROTECTED_SAVED) {
        Some(ABSENT) => k.delete(PROTECTED),
        Some(v) => k.set_dword(PROTECTED, v)?,
        // Önceki değeri kaydetmeyen eski sürümün kurulumu: Windows'un varsayılanı değerin hiç
        // olmamasıdır. Equalizer APO kuruluysa (o da 1 yazar) dokunulmaz.
        None => {
            let eq = std::env::var_os("ProgramFiles")
                .is_some_and(|p| std::path::Path::new(&p).join("EqualizerAPO").exists());
            if !eq && k.dword(PROTECTED) == Some(1) {
                k.delete(PROTECTED);
            }
        }
    }
    k.delete(PROTECTED_SAVED);
    Ok(())
}

/// Bileşik listeden soundboard'ü çıkarır; liste boşalırsa değer silinir.
fn unlist(k: &Key, name: &str) -> Res<bool> {
    let Some(list) = k.multi(name) else { return Ok(false) };
    let rest: Vec<String> = list.iter().filter(|s| !ours(s)).cloned().collect();
    if rest.len() == list.len() {
        return Ok(false);
    }
    if rest.is_empty() {
        k.delete(name)
    } else {
        k.set_multi(name, &rest)?
    }
    Ok(true)
}

/// Bileşik listenin sonuna yalnızca `clsid` olarak soundboard koyar; listenin boyunu döndürür.
fn append(k: &Key, pid: u32, clsid: &GUID) -> Res<usize> {
    let mut list: Vec<String> = k.multi(&composite(pid)).unwrap_or_default().into_iter().filter(|s| !ours(s)).collect();
    list.push(guid_str(clsid));
    k.set_multi(&composite(pid), &list)?;
    Ok(list.len())
}

/// Tekli EFX yuvasını eski sahibine geri verir.
fn unslot(k: &Key) -> Res<bool> {
    let taken = k.get(&efx()).is_some_and(|s| ours(&s));
    if taken {
        match k.get(&bus::saved_value()).filter(|s| parse_guid(s).is_some()) {
            Some(orig) => k.set(&efx(), &orig)?,
            None => k.delete(&efx()),
        }
    }
    if k.dword(&created_modes()) == Some(1) {
        k.delete(&modes(7));
    }
    k.delete(&bus::saved_value());
    k.delete(&created_modes());
    Ok(taken)
}

fn attach(m: &Mic) -> Res<()> {
    let k = Key::open(&bus::fx_key(&m.guid), true)?;
    let where_;
    if [13, 14, 15, 16, 17, 18].iter().any(|&p| k.exists(&composite(p))) {
        unslot(&k)?;
        unlist(&k, &composite(14))?;
        let vendor_sfx = k.multi(&composite(13)).is_some_and(|l| l.iter().any(|s| !ours(s)));
        if vendor_sfx {
            unlist(&k, &composite(13))?;
            let list = append(&k, 15, &APO_CLSID)?;
            where_ = format!("EFX zincirinin sonu ({} efekt)", list);
        } else {
            // SFX boş: soundboard orada tek başına, sürücünün tüm işlemesinden sonra. Mod listesi
            // cihazın desteklediği tüm modlar olur, yoksa SFX iletişim ve raw akışlarında çalışmaz.
            k.set_multi(&composite(13), &[guid_str(&APO_CLSID)])?;
            // Raw akışlar SFX'i atlar: onlar için EFX'te yedek örnek (SFX çalışırken susar).
            append(&k, 15, &APO_CLSID_FALLBACK)?;
            if k.dword(&created_sfx_modes()) != Some(1) {
                let mut all: Vec<String> = vec![MODE_DEFAULT.to_string()];
                for p in [5, 6, 7] {
                    for m in k.multi(&modes(p)).unwrap_or_default() {
                        if !all.iter().any(|x| x.eq_ignore_ascii_case(&m)) {
                            all.push(m);
                        }
                    }
                }
                if let Some(old) = k.multi(&modes(5)) {
                    k.set_multi(&saved_sfx_modes(), &old)?;
                }
                k.set_multi(&modes(5), &all)?;
                k.set_dword(&created_sfx_modes(), 1)?;
            }
            where_ = "SFX (tek efekt, tüm modlar), raw için EFX'te yedek".to_string();
        }
    } else {
        let current = k.get(&efx()).filter(|s| !s.is_empty());
        if !current.as_deref().is_some_and(ours) {
            // Sürücü güncellemesi yuvayı geri almış olabilir: saklanan hep yuvadaki son yabancı efekt.
            k.set(&bus::saved_value(), current.as_deref().unwrap_or(""))?;
            k.set(&efx(), &guid_str(&APO_CLSID))?;
        }
        if !k.exists(&modes(7)) {
            k.set_multi(&modes(7), &[MODE_DEFAULT.to_string()])?;
            k.set_dword(&created_modes(), 1)?;
        }
        let saved = k.get(&bus::saved_value()).filter(|s| !s.is_empty());
        where_ = format!("EFX yuvası (önceki efekt: {})", saved.as_deref().unwrap_or("yok"));
    }
    enable_effects(&m.guid)?;
    log!("bağlandı: {} {}: {where_}", m.name, m.guid);
    Ok(())
}

fn detach(m: &Mic) -> Res<()> {
    let Ok(k) = Key::open(&bus::fx_key(&m.guid), true) else { return Ok(()) };
    let mut any = unslot(&k)?;
    if k.dword(&created_sfx_modes()) == Some(1) {
        match k.multi(&saved_sfx_modes()) {
            Some(old) => k.set_multi(&modes(5), &old)?,
            None => k.delete(&modes(5)),
        }
        k.delete(&saved_sfx_modes());
        k.delete(&created_sfx_modes());
    }
    for p in [13, 14, 15] {
        any |= unlist(&k, &composite(p))?;
    }
    restore_effects(&m.guid, &k)?;
    if any {
        log!("ayrıldı: {} {}", m.name, m.guid);
    }
    Ok(())
}

/// Değişikliklerin yüklenmesi için ses hizmetlerini yeniden başlatır (sesler ~1 sn kesilir).
fn restart_audio() -> Res<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Restart-Service -Force AudioEndpointBuilder; Start-Service Audiosrv",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !out.status.success() {
        return Err(
            format!("ses hizmeti yeniden başlatılamadı: {}", String::from_utf8_lossy(&out.stderr).trim()).into()
        );
    }
    Ok(())
}

/// Kurar; yarıda bir şey ters giderse yazdığı her şeyi geri alır (kalıntı bırakmaz).
pub fn install() -> Res<()> {
    enable_privileges()?;
    let result = install_inner();
    if let Err(e) = &result {
        log!("kurulum yarıda kaldı ({e}), geri alınıyor");
        if let Err(e) = uninstall() {
            log!("geri alınamadı: {e}");
        }
    }
    result
}

fn install_inner() -> Res<()> {
    let dll = write_dll()?;
    register(&dll)?;
    bus::create()?;
    protected_off()?;
    let list = mics(DEVICE_STATE_ACTIVE)?;
    if list.is_empty() {
        return Err("etkin mikrofon yok".into());
    }
    for m in &list {
        attach(m)?;
    }
    restart_audio()?;
    remove_old_dlls();
    Ok(())
}

/// Kurulumun yaptığı her şeyi geri alır: mikrofon zincirleri ve ayarları, korumalı audiodg
/// ayarı, COM ve APO kayıtları, Program Files ve ProgramData klasörleri.
pub fn uninstall() -> Res<()> {
    enable_privileges()?;
    for m in mics(DEVICE_STATE(DEVICE_STATEMASK_ALL))? {
        detach(&m)?;
    }
    protected_restore()?;
    // DLL'i ve ortak belleği tutan audiodg kapansın.
    restart_audio()?;
    for path in CLSIDS.iter().flat_map(|c| [clsid_key(c), apo_key(c)]) {
        let p = wide(&path);
        let e = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(p.as_ptr())) };
        if e.is_err() && e != ERROR_FILE_NOT_FOUND {
            return Err(format!("{path} silinemedi: {e:?}").into());
        }
    }
    for dir in [install_dir(), bus::dir()] {
        remove_dir(&dir);
    }
    Ok(())
}

/// Klasörü siler; kilitli dosya kaldıysa (audiodg hâlâ tutuyorsa) Windows açılışta siler.
fn remove_dir(dir: &std::path::Path) {
    if !dir.exists() || std::fs::remove_dir_all(dir).is_ok() {
        return;
    }
    let later = |p: &std::path::Path| unsafe {
        let _ = MoveFileExW(PCWSTR(wide(&p.to_string_lossy()).as_ptr()), None, MOVEFILE_DELAY_UNTIL_REBOOT);
    };
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        later(&e.path());
    }
    later(dir);
    log!("{} açılışta silinecek (dosya kullanımda)", dir.display());
}

/// Kurulumdan geriye kalan her şey (sistem tarafı); boşsa iz yok. Yönetici gerekmez.
pub fn leftovers() -> Vec<String> {
    let mut left = Vec::new();
    for dir in [install_dir(), bus::dir()] {
        if dir.exists() {
            left.push(dir.display().to_string());
        }
    }
    for path in CLSIDS.iter().flat_map(|c| [clsid_key(c), apo_key(c)]) {
        if Key::read(&path).is_ok() {
            left.push(format!(r"HKLM\{path}"));
        }
    }
    if Key::read(AUDIO_KEY).is_ok_and(|k| k.exists(PROTECTED_SAVED)) {
        left.push(format!(r"HKLM\{AUDIO_KEY}\{PROTECTED_SAVED}"));
    }
    for m in mics(DEVICE_STATE(DEVICE_STATEMASK_ALL)).unwrap_or_default() {
        let Ok(k) = Key::read(&bus::fx_key(&m.guid)) else { continue };
        if attached(&m.guid) || own_values().iter().any(|v| k.exists(v)) {
            left.push(format!("{} mikrofonunun efekt ayarları", m.name));
        }
    }
    left
}
