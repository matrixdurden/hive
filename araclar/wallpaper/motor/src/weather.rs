//! `iWeather`: bulunduğun yerin şu anki havası. Konum IP'den bulunur (şehir düzeyinde, geojs.io)
//! ya da `wallpaper.ini`'deki `konum = enlem, boylam` satırından gelir; hava Open-Meteo'dan.
//! İkisi de anahtar istemez. Yalnızca etkin duvar kâğıdı `iWeather` okuyorsa ve 30 dakikada bir
//! sorulur; motor duraklatılmışken hiç sorulmaz.
//!
//! `iWeather` = (bulut, yağmur, kar, sis), her biri 0..1. Gök gürültülü fırtınada yağmur 1'dir.
//! Bilinmiyorsa (çevrimdışı, ilk yanıttan önce) hepsi 0: açık hava.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use crate::log;
use crate::update;
use crate::util::Res;

const EVERY: Duration = Duration::from_secs(30 * 60);
const RETRY: Duration = Duration::from_secs(5 * 60);

static CURRENT: Mutex<[f32; 4]> = Mutex::new([0.0; 4]);
static WANTED: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
/// `wallpaper.ini`'den elle verilen konum; yoksa IP'den bulunur.
static MANUAL: Mutex<Option<(f64, f64)>> = Mutex::new(None);

pub fn current() -> [f32; 4] {
    *CURRENT.lock().unwrap()
}

pub fn set_location(text: Option<&str>) {
    *MANUAL.lock().unwrap() = text.and_then(parse_location);
}

fn parse_location(s: &str) -> Option<(f64, f64)> {
    let (a, b) = s.split_once(',')?;
    let (lat, lon) = (a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?);
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

/// Etkin duvar kâğıdı hava istiyor mu ve motor çalışıyor mu? İlk istekte arka plan iş parçacığı
/// başlar; istenmediği sürece ağa çıkmaz.
pub fn want(on: bool) {
    WANTED.store(on, Ordering::Relaxed);
    if on && !STARTED.swap(true, Ordering::Relaxed) {
        let _ = std::thread::Builder::new().name("hava".into()).spawn(run);
    }
}

fn run() {
    let mut ip_location: Option<(f64, f64)> = None;
    let mut next: Option<SystemTime> = None;
    loop {
        let due = next.is_none_or(|t| SystemTime::now() >= t);
        if due && WANTED.load(Ordering::Relaxed) {
            let manual = *MANUAL.lock().unwrap();
            let location = match manual.or(ip_location) {
                Some(l) => Ok(l),
                None => locate().inspect(|&l| ip_location = Some(l)),
            };
            match location.and_then(|(lat, lon)| fetch(lat, lon)) {
                Ok(w) => {
                    log!("hava: bulut {:.2}, yağmur {:.2}, kar {:.2}, sis {:.2}", w[0], w[1], w[2], w[3]);
                    *CURRENT.lock().unwrap() = w;
                    next = Some(SystemTime::now() + EVERY);
                }
                Err(e) => {
                    log!("hava alınamadı: {e}");
                    next = Some(SystemTime::now() + RETRY);
                }
            }
        }
        std::thread::sleep(Duration::from_secs(20));
    }
}

/// IP'den kabaca konum (şehir). Yanıtta enlem/boylam metin olarak gelir.
fn locate() -> Res<(f64, f64)> {
    let body = body("https://get.geojs.io/v1/ip/geo.json")?;
    let lat = number(&body, "latitude").ok_or("konumda enlem yok")?;
    let lon = number(&body, "longitude").ok_or("konumda boylam yok")?;
    Ok((lat, lon))
}

fn fetch(lat: f64, lon: f64) -> Res<[f32; 4]> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat:.2}&longitude={lon:.2}&current=weather_code,cloud_cover"
    );
    let body = body(&url)?;
    // `current_units` da aynı anahtarları taşır: değerler `"current":{` sonrasında.
    let cur = body.find("\"current\":{").map(|i| &body[i..]).ok_or("yanıtta current yok")?;
    let code = number(cur, "weather_code").ok_or("yanıtta weather_code yok")? as u32;
    let cloud = number(cur, "cloud_cover").unwrap_or(0.0) as f32 / 100.0;
    Ok(from_wmo(code, cloud))
}

/// WMO hava kodu → (bulut, yağmur, kar, sis).
fn from_wmo(code: u32, cloud: f32) -> [f32; 4] {
    let (rain, snow, fog) = match code {
        45 | 48 => (0.0, 0.0, 1.0),
        51 | 53 | 55 | 56 | 57 => (0.25, 0.0, 0.2),
        61 | 80 => (0.4, 0.0, 0.0),
        63 | 81 | 66 => (0.65, 0.0, 0.0),
        65 | 82 | 67 => (0.9, 0.0, 0.0),
        71 | 85 | 77 => (0.0, 0.35, 0.0),
        73 => (0.0, 0.6, 0.0),
        75 | 86 => (0.0, 0.9, 0.0),
        95..=99 => (1.0, 0.0, 0.0),
        _ => (0.0, 0.0, 0.0),
    };
    let cloud = if rain + snow > 0.0 { cloud.max(0.8) } else { cloud };
    [cloud.clamp(0.0, 1.0), rain, snow, fog]
}

fn body(url: &str) -> Res<String> {
    match update::get(url, false)? {
        (200, b) => Ok(String::from_utf8_lossy(&b).into_owned()),
        (status, _) => Err(format!("HTTP {status}").into()),
    }
}

/// `"anahtar": 41.2` ya da `"anahtar":"41.2"` biçiminden sayıyı okur.
fn number(json: &str, key: &str) -> Option<f64> {
    let rest = &json[json.find(&format!("\"{key}\""))? + key.len() + 2..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start().trim_start_matches('"');
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_numbers() {
        assert_eq!(number(r#"{"latitude":"41.0214","longitude": 28.99}"#, "latitude"), Some(41.0214));
        assert_eq!(number(r#"{"latitude":"41.0214","longitude": 28.99}"#, "longitude"), Some(28.99));
        assert_eq!(number(r#"{"x":-3}"#, "x"), Some(-3.0));
        assert_eq!(parse_location("41.0, 29.0"), Some((41.0, 29.0)));
        assert_eq!(parse_location("141.0, 29.0"), None);
    }

    #[test]
    fn maps_codes() {
        assert_eq!(from_wmo(0, 0.1), [0.1, 0.0, 0.0, 0.0]);
        assert_eq!(from_wmo(63, 0.5), [0.8, 0.65, 0.0, 0.0]);
        assert_eq!(from_wmo(95, 1.0)[1], 1.0);
    }
}
