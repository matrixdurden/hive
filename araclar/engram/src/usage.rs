//! Harcama takibi: API bakiyeyi bir uç noktadan vermiyor (yalnızca yönetici anahtarıyla), bu
//! yüzden her cevabın `usage` bilgisinden fiyat tablosuyla tahmin edilir ve aylık toplanır
//! (`kullanim.ini`). Ayarlar'da "bu ay şu kadar / bütçe" olarak görünür.

use std::sync::Mutex;

use crate::util::data_dir;

/// (model öneki, giriş, çıkış, önbellek okuma, önbellek yazma) — milyon token başına dolar.
const PRICES: [(&str, f64, f64, f64, f64); 5] = [
    ("claude-opus-5-5", 4.0, 20.0, 0.20, 5.0),
    ("claude-opus", 5.0, 25.0, 0.50, 6.25),
    ("claude-sonnet-5-5", 2.0, 10.0, 0.20, 2.5),
    ("claude-sonnet", 2.0, 10.0, 0.20, 2.5),
    ("claude-haiku-5-5", 0.10, 0.50, 0.01, 0.125),
];

#[derive(Clone, Default, Debug)]
pub struct Month {
    /// "YYYY-AA"
    pub month: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: f64,
    pub calls: u64,
}

static CURRENT: Mutex<Option<Month>> = Mutex::new(None);

fn file() -> std::path::PathBuf {
    data_dir().join("kullanim.ini")
}

fn load() -> Month {
    let mut m = Month::default();
    for line in std::fs::read_to_string(file()).unwrap_or_default().lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k.trim() {
            "ay" => m.month = v.to_string(),
            "giris" => m.input = v.parse().unwrap_or(0),
            "cikis" => m.output = v.parse().unwrap_or(0),
            "onbellek_okuma" => m.cache_read = v.parse().unwrap_or(0),
            "onbellek_yazma" => m.cache_write = v.parse().unwrap_or(0),
            "maliyet" => m.cost = v.parse().unwrap_or(0.0),
            "istek" => m.calls = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    m
}

fn save(m: &Month) {
    let _ = std::fs::create_dir_all(data_dir());
    let text = format!(
        "ay={}\ngiris={}\ncikis={}\nonbellek_okuma={}\nonbellek_yazma={}\nmaliyet={:.6}\nistek={}\n",
        m.month, m.input, m.output, m.cache_read, m.cache_write, m.cost, m.calls
    );
    let _ = std::fs::write(file(), text);
}

/// Bu ayın toplamı (ay değiştiyse sıfırdan).
pub fn month() -> Month {
    let now = crate::util::now().0[..7].to_string();
    let mut g = CURRENT.lock().unwrap();
    let m = g.get_or_insert_with(load);
    if m.month != now {
        *m = Month { month: now, ..Default::default() };
        save(m);
    }
    m.clone()
}

/// Bir isteğin maliyeti (dolar).
pub fn cost(model: &str, u: &crate::claude::Usage) -> f64 {
    let p = PRICES.iter().find(|p| model.starts_with(p.0)).unwrap_or(&PRICES[0]);
    (u.input as f64 * p.1 + u.output as f64 * p.2 + u.cache_read as f64 * p.3 + u.cache_write as f64 * p.4) / 1e6
}

/// Bir isteği aya ekler.
pub fn add(model: &str, u: &crate::claude::Usage) {
    let c = cost(model, u);
    let _ = month();
    let mut g = CURRENT.lock().unwrap();
    if let Some(m) = g.as_mut() {
        m.input += u.input;
        m.output += u.output;
        m.cache_read += u.cache_read;
        m.cache_write += u.cache_write;
        m.cost += c;
        m.calls += 1;
        save(m);
    }
}

/// Kısa özet: "1,23 $ / 200 $ · 48 istek".
pub fn summary(budget: f64) -> String {
    let m = month();
    let pct = if budget > 0.0 { (m.cost / budget * 100.0).round() as u32 } else { 0 };
    t!(
        format!("${:.2} of ${:.0} this month ({pct}%) · {} requests", m.cost, budget, m.calls),
        format!("Bu ay {:.2} $ / {:.0} $ (%{pct}) · {} istek", m.cost, budget, m.calls)
    )
}

/// Token özeti: "giriş 12,3k · çıkış 4,1k · önbellek 80%".
pub fn tokens() -> String {
    let m = month();
    let k = |v: u64| if v >= 1_000_000 { format!("{:.1}M", v as f64 / 1e6) } else if v >= 1000 { format!("{:.1}k", v as f64 / 1e3) } else { v.to_string() };
    let total_in = m.input + m.cache_read + m.cache_write;
    let hit = if total_in > 0 { (m.cache_read * 100 / total_in) as u32 } else { 0 };
    t!(
        format!("in {} · out {} · cache hit {hit}%", k(total_in), k(m.output)),
        format!("giriş {} · çıkış {} · önbellek isabeti %{hit}", k(total_in), k(m.output))
    )
}
