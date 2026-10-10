//! Ekranın yenilemesine eş kare saati. Animasyonlar her kare için bir istek bırakır (`request`);
//! saat bir sonraki ekran yenilemesini (DWM'nin bir sonraki birleştirmesi, `DwmFlush`) bekleyip
//! isteyen pencerelere mesajı gönderir. Windows zamanlayıcısı ~15,6 ms adımla ve ekranla
//! hizasız çalışır (60 Hz'de bile kare atlar, 144 Hz'i hiç tutturamaz); bu saat ekran kaç Hz ise
//! o kadar, düzenli kare verir. İstek yokken iş parçacığı uyur, işlemci harcamaz.

use std::sync::{Condvar, Mutex, Once};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Bekleyen istekler: (pencere, mesaj). Her biri bir kare için; animasyon sürdükçe yenilenir.
static WANT: Mutex<Vec<(isize, u32)>> = Mutex::new(Vec::new());
static WAKE: Condvar = Condvar::new();
static START: Once = Once::new();

/// En kısa kare aralığı: DWM boştayken DwmFlush hemen dönerse saat dönüp durmasın (240 Hz).
const MIN_FRAME: Duration = Duration::from_micros(4100);

/// Bir sonraki ekran yenilemesinde `hwnd`'ye `msg` gönderilsin (aynı istek bir kez sayılır).
pub fn request(hwnd: HWND, msg: u32) {
    START.call_once(|| {
        std::thread::spawn(run);
    });
    let mut w = WANT.lock().unwrap();
    let key = (hwnd.0 as isize, msg);
    if !w.contains(&key) {
        w.push(key);
    }
    WAKE.notify_one();
}

fn run() {
    let mut last = Instant::now();
    loop {
        {
            let mut w = WANT.lock().unwrap();
            while w.is_empty() {
                w = WAKE.wait(w).unwrap();
            }
        }
        if unsafe { DwmFlush() }.is_err() {
            // Birleştirme kapalı (uzak masaüstü gibi): yaklaşık 60 Hz.
            std::thread::sleep(Duration::from_millis(16));
        }
        let since = last.elapsed();
        if since < MIN_FRAME {
            std::thread::sleep(MIN_FRAME - since);
        }
        last = Instant::now();
        let due = std::mem::take(&mut *WANT.lock().unwrap());
        for (h, m) in due {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), m, WPARAM(0), LPARAM(0));
            }
        }
    }
}
