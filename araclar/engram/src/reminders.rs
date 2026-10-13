//! Hatırlatıcılar ve zamanlayıcılar ("20 dakika sonra hatırlat", pomodoro). Diskte durur
//! (`hatirlaticilar.ini`, satır başına `kimlik|unix saniye|metin`): engram yeniden başlasa da
//! kaybolmaz; kapalıyken vakti geçenler açılınca hemen söylenir. Vakit gelince pencereye
//! WM_REMINDER gider, metin `take_fired` ile alınır.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_REMINDER: u32 = WM_APP + 85;

struct Item {
    id: u64,
    due: u64,
    text: String,
}

static ITEMS: Mutex<Vec<Item>> = Mutex::new(Vec::new());
static FIRED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static TARGET: Mutex<usize> = Mutex::new(0);

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn file() -> std::path::PathBuf {
    crate::util::data_dir().join("hatirlaticilar.ini")
}

fn save(items: &[Item]) {
    let text: String = items.iter().map(|i| format!("{}|{}|{}\n", i.id, i.due, i.text.replace('\n', " "))).collect();
    let _ = std::fs::write(file(), text);
}

/// Pencere hazır olunca: kayıtlı hatırlatıcıları yükler, denetleyiciyi başlatır.
pub fn init(hwnd: HWND) {
    *TARGET.lock().unwrap() = hwnd.0 as usize;
    let mut items = ITEMS.lock().unwrap();
    for line in std::fs::read_to_string(file()).unwrap_or_default().lines() {
        let mut p = line.splitn(3, '|');
        if let (Some(id), Some(due), Some(text)) = (p.next(), p.next(), p.next())
            && let (Ok(id), Ok(due)) = (id.parse(), due.parse())
        {
            items.push(Item { id, due, text: text.to_string() });
        }
    }
    drop(items);
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let t = now();
            let mut items = ITEMS.lock().unwrap();
            let due: Vec<String> = items.iter().filter(|i| i.due <= t).map(|i| i.text.clone()).collect();
            if due.is_empty() {
                continue;
            }
            items.retain(|i| i.due > t);
            save(&items);
            drop(items);
            FIRED.lock().unwrap().extend(due);
            unsafe {
                let _ = PostMessageW(Some(HWND(*TARGET.lock().unwrap() as *mut _)), WM_REMINDER, WPARAM(0), LPARAM(0));
            }
        }
    });
}

/// `seconds` sonra `text`'i hatırlatır.
pub fn add(seconds: u64, text: &str) -> String {
    let mut items = ITEMS.lock().unwrap();
    let id = items.iter().map(|i| i.id).max().unwrap_or(0) + 1;
    items.push(Item { id, due: now() + seconds.max(1), text: text.trim().to_string() });
    save(&items);
    format!("hatırlatıcı #{id} kuruldu: {} sonra \"{}\"", human(seconds), text.trim())
}

pub fn list() -> String {
    let items = ITEMS.lock().unwrap();
    if items.is_empty() {
        return "Kurulu hatırlatıcı yok.".into();
    }
    let t = now();
    items.iter().map(|i| format!("#{}: {} sonra — {}", i.id, human(i.due.saturating_sub(t)), i.text)).collect::<Vec<_>>().join("\n")
}

pub fn cancel(id: u64) -> String {
    let mut items = ITEMS.lock().unwrap();
    let before = items.len();
    items.retain(|i| i.id != id);
    save(&items);
    if items.len() < before { format!("#{id} iptal edildi") } else { format!("#{id} bulunamadı") }
}

/// Vakti gelen metinler.
pub fn take_fired() -> Vec<String> {
    std::mem::take(&mut *FIRED.lock().unwrap())
}

fn human(s: u64) -> String {
    match s {
        0..60 => format!("{s} sn"),
        60..3600 => format!("{} dk", s.div_ceil(60)),
        _ => format!("{} sa {} dk", s / 3600, (s % 3600) / 60),
    }
}
