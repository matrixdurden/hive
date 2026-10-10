//! Atlama listesi: Windows görev çubuğunun sağ tıkta gösterdikleri. İki kaynaktan okunur:
//!
//! - Son açılan dosyalar: Windows'un uygulama için tuttuğu liste (`IApplicationDocumentLists`,
//!   uygulama kimliğiyle).
//! - Uygulamanın kendi listesi: görevler ("Yeni pencere", "Gizli pencere") ve kendi kategorileri
//!   ("Son klasörler"). Uygulama bunları yazar ama Windows okuma arayüzü vermez; liste
//!   `Recent\CustomDestinations\*.customDestinations-ms` dosyalarında kısayol (Shell Link) olarak
//!   durur. Dosyalardaki her kısayol açılır, hedefi bu uygulamanın exe'si olanlar alınır.

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IPersistStream};
use windows::Win32::UI::Shell::Common::IObjectArray;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{GUID, Interface, PCWSTR, w};

use crate::util::wide;

/// Listede bir satır: açılacak dosya ya da program ve argümanları.
#[derive(Clone, Debug)]
pub struct Entry {
    pub title: String,
    pub file: String,
    pub args: String,
    pub dir: String,
    /// Uygulamanın kendi listesinden (görev ya da kategori); değilse son açılan dosya.
    pub own: bool,
}

impl Entry {
    pub fn open(&self) {
        let (f, a, d) = (wide(&self.file), wide(&self.args), wide(&self.dir));
        let opt = |s: &Vec<u16>, src: &str| if src.is_empty() { PCWSTR::null() } else { PCWSTR(s.as_ptr()) };
        unsafe {
            let _ = ShellExecuteW(None, w!("open"), PCWSTR(f.as_ptr()), opt(&a, &self.args), opt(&d, &self.dir), SW_SHOWNORMAL);
        }
    }
}

/// System.Title: kısayolun listede görünen adı.
const PKEY_TITLE: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9), pid: 2 };

/// Shell Link başlığı: boy (0x4C) ve CLSID_ShellLink.
const LNK_MAGIC: [u8; 20] = [
    0x4c, 0, 0, 0, 0x01, 0x14, 0x02, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46,
];

fn take_string(p: windows::core::PWSTR) -> String {
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}

/// Uygulamanın atlama listesi: önce kendi listesi (en çok 10), sonra son dosyalar (en çok 8).
/// `aumids`: denenecek uygulama kimlikleri; `exe`: çalışan ya da kısayolun exe'si.
pub fn list(aumids: &[String], exe: Option<&str>) -> Vec<Entry> {
    let mut out = own_list(aumids, exe, 10);
    for a in aumids {
        let recent = recent(a, 8);
        if !recent.is_empty() {
            out.extend(recent);
            break;
        }
    }
    out
}

/// Atlama listesi dosyasının adı: uygulama kimliğinin (büyük harf, UTF-16LE) CRC-64'ü
/// (ters bitli, polinom 0x92C64265D32139A4, başlangıç hepsi 1, sonda XOR yok), baştaki
/// sıfırlar olmadan onaltılık.
fn appid_hash(id: &str) -> String {
    let mut crc = u64::MAX;
    for b in id.to_uppercase().encode_utf16().flat_map(u16::to_le_bytes) {
        crc ^= b as u64;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x92c6_4265_d321_39a4 } else { crc >> 1 };
        }
    }
    format!("{crc:x}")
}

fn custom_dir() -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from(std::env::var_os("APPDATA")?).join(r"Microsoft\Windows\Recent\CustomDestinations"))
}

/// Bir CustomDestinations dosyasındaki kısayollar (aynı adlı olan bir kez).
fn links_in(path: &std::path::Path, max: usize) -> Vec<Entry> {
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    let mut out: Vec<Entry> = Vec::new();
    let mut at = 0;
    while let Some(off) = bytes[at..].windows(LNK_MAGIC.len()).position(|w| w == LNK_MAGIC) {
        let start = at + off;
        at = start + LNK_MAGIC.len();
        if let Some(e) = link(&bytes[start..])
            && !out.iter().any(|o| o.title == e.title)
        {
            out.push(e);
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

/// Uygulamanın kendi listesi. Kimliği biliniyorsa doğrudan onun dosyası; bilinmiyorsa (kimliğini
/// süreç içinde koyan uygulamalar, VS Code gibi) hedefi bu exe olan kısayolları taşıyan dosya.
/// Aynı exe'nin başka profilleri de olabilir (tarayıcılar): ayrı bir veri klasörüyle
/// (--user-data-dir) açanların dosyası yalnızca başka yoksa seçilir.
fn own_list(aumids: &[String], exe: Option<&str>, max: usize) -> Vec<Entry> {
    let Some(dir) = custom_dir() else { return Vec::new() };
    for a in aumids {
        let l = links_in(&dir.join(format!("{}.customDestinations-ms", appid_hash(a))), max);
        if !l.is_empty() {
            return l;
        }
    }
    let Some(exe) = exe else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut fallback = Vec::new();
    for f in rd.flatten() {
        let l: Vec<Entry> = links_in(&f.path(), max).into_iter().filter(|e| e.file.eq_ignore_ascii_case(exe)).collect();
        if l.is_empty() {
            continue;
        }
        if l.iter().any(|e| e.args.contains("--user-data-dir")) {
            if fallback.is_empty() {
                fallback = l;
            }
            continue;
        }
        return l;
    }
    fallback
}

/// Windows'un uygulama için tuttuğu son açılan dosyalar (var olanlar).
fn recent(aumid: &str, max: u32) -> Vec<Entry> {
    let r = (|| -> windows::core::Result<Vec<Entry>> {
        unsafe {
            let lists: IApplicationDocumentLists = CoCreateInstance(&ApplicationDocumentLists, None, CLSCTX_INPROC_SERVER)?;
            let id = wide(aumid);
            lists.SetAppID(PCWSTR(id.as_ptr()))?;
            let arr: IObjectArray = lists.GetList(ADLT_RECENT, max)?;
            let mut out = Vec::new();
            for i in 0..arr.GetCount()? {
                let Ok(item) = arr.GetAt::<IShellItem>(i) else { continue };
                let Ok(p) = item.GetDisplayName(SIGDN_FILESYSPATH) else { continue };
                let path = take_string(p);
                if path.is_empty() || !std::path::Path::new(&path).exists() {
                    continue;
                }
                let title = item.GetDisplayName(SIGDN_NORMALDISPLAY).map(take_string).unwrap_or_else(|_| path.clone());
                out.push(Entry { title, file: path, args: String::new(), dir: String::new(), own: false });
            }
            Ok(out)
        }
    })();
    r.unwrap_or_default()
}

/// "@C:\\...\\code.exe,-123" gibi kaynak başvurusunu çözer (görevlerin adları böyle yazılır).
fn indirect(s: &str) -> String {
    if !s.starts_with('@') {
        return s.to_string();
    }
    let src = wide(s);
    let mut buf = [0u16; 512];
    match unsafe { SHLoadIndirectString(PCWSTR(src.as_ptr()), &mut buf, None) } {
        Ok(()) => String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)]),
        Err(_) => String::new(),
    }
}

/// Bellekteki bir kısayolu açar: hedef, argümanlar, çalışma klasörü, başlık.
fn link(data: &[u8]) -> Option<Entry> {
    unsafe {
        let stream = SHCreateMemStream(Some(data))?;
        let persist: IPersistStream = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        persist.Load(&stream).ok()?;
        let link: IShellLinkW = persist.cast().ok()?;
        let mut buf = [0u16; 1024];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let file = String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)]);
        if file.is_empty() {
            return None;
        }
        let mut abuf = [0u16; 2048];
        let _ = link.GetArguments(&mut abuf);
        let args = String::from_utf16_lossy(&abuf[..abuf.iter().position(|&c| c == 0).unwrap_or(0)]);
        let mut dbuf = [0u16; 1024];
        let _ = link.GetWorkingDirectory(&mut dbuf);
        let dir = String::from_utf16_lossy(&dbuf[..dbuf.iter().position(|&c| c == 0).unwrap_or(0)]);
        let title = link
            .cast::<IPropertyStore>()
            .ok()
            .and_then(|ps| ps.GetValue(&PKEY_TITLE).ok())
            .and_then(|v| PropVariantToStringAlloc(&v).ok().map(take_string))
            .or_else(|| {
                let mut d = [0u16; 512];
                link.GetDescription(&mut d).ok()?;
                Some(String::from_utf16_lossy(&d[..d.iter().position(|&c| c == 0).unwrap_or(0)]))
            })
            .map(|t| indirect(&t))
            .filter(|t| !t.is_empty())?;
        Some(Entry { title, file, args, dir, own: true })
    }
}

/// `hive --jump-test <exe> [kimlik]`: atlama listesini yazdırır.
pub fn probe(exe: &str, aumid: Option<&str>) -> String {
    let ids: Vec<String> = aumid.map(String::from).into_iter().collect();
    let l = list(&ids, Some(exe));
    let mut s = format!("{} satır\n", l.len());
    for e in l {
        s += &format!("  [{}] {} -> {} {}\n", if e.own { "liste" } else { "son" }, e.title, e.file, e.args);
    }
    s
}
