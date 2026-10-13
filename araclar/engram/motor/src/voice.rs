//! Ses: "Hey Matrix" ile uyanma, komutu dinleme, yazıya çevirme, cevabı seslendirme. Üç iş
//! parçacığı var: mikrofon (çerçeveler ve konuşma algılama), çekirdek (uyandırma eşleme ve
//! yazıya çevirme: ağ işi) ve konuşmacı (cevabı cümle cümle seslendirme). Durumu arayüz iş
//! parçacığı tutar ve ekran ışığını sürer; iş parçacıkları ona WM_VOICE ile haber verir.
//!
//! Sesli tepkiler ("Efendim?", "Hı?", "Buyur.") ve uyandırma sözü `sistem/ses.md` notunda durur:
//! her başlığın altındaki satırlardan biri rastgele söylenir. Bu cümleler bir kez seslendirilip
//! önbelleğe yazılır, uyanınca anında çalar.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer, WM_APP};

use crate::agent::{Mode, Out};
use crate::audio::{self, Capture, Player};
use crate::brain::Brain;
use crate::config::Config;
use crate::overlay;
use crate::wake::{self, Spotter, Vad, VadEvent};

/// İş parçacıklarından olay geldi.
pub const WM_VOICE: u32 = WM_APP + 30;
/// Mikrofon seviyesi: wParam = seviye × 1000.
pub const WM_LEVEL: u32 = WM_APP + 31;
/// Bas-konuş tuşu ve Esc (klavye kancasından): wParam 1 basıldı, 0 bırakıldı, 2 Esc.
pub const WM_KEY: u32 = WM_APP + 32;
/// Ekran ışığındaki onay düğmeleri: wParam 1 evet, 0 hayır.
pub const WM_CONFIRM: u32 = WM_APP + 33;
/// Konuşurken çıkış seviyesini ekran ışığına aktaran zamanlayıcı.
pub const TIMER: usize = 0x7663;

/// Mikrofonun neyi dinlediği: arayüz belirler, mikrofon ve çekirdek okur.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Phase {
    Off = 0,
    /// Uyandırma sözünü bekliyor.
    Wake = 1,
    /// Uyandı, komutu dinliyor.
    Command = 2,
    /// Cevaptan sonra kısa süre uyandırmasız dinliyor.
    FollowUp = 3,
    /// Evet/hayır bekliyor.
    Confirm = 4,
    /// Bas-konuş: tuş bırakılana kadar ham kayıt.
    Ptt = 5,
    /// Uyandırma sözünü öğreniyor.
    Train = 6,
    /// Sesini tanıyor: okunan cümleler ses izine eklenir.
    Enroll = 7,
}

impl Phase {
    fn from(v: u8) -> Phase {
        [Phase::Off, Phase::Wake, Phase::Command, Phase::FollowUp, Phase::Confirm, Phase::Ptt, Phase::Train, Phase::Enroll].get(v as usize).copied().unwrap_or(Phase::Off)
    }

    /// Bu evrede konuşma başlamazsa ne kadar sonra vazgeçilir.
    fn patience(self) -> Option<Duration> {
        match self {
            Phase::Command => Some(Duration::from_secs(7)),
            Phase::FollowUp => Some(Duration::from_secs(5)),
            Phase::Confirm => Some(Duration::from_secs(12)),
            _ => None,
        }
    }
}

/// İş parçacıklarından arayüze.
enum Ev {
    /// Uyandırma sözü duyuldu; aynı nefeste söylenen komut varsa o.
    Wake(Option<String>),
    /// Dinlenen konuşma yazıya çevrildi (boş: anlaşılmadı). Hangi evrede söylendiği.
    Heard(Phase, String),
    /// Dinlerken konuşma başladı.
    SpeechStart,
    /// Kullanıcı konuşurken araya girdi: susuldu, komut dinleniyor.
    BargeIn,
    /// Dinleme süresi doldu, konuşma gelmedi.
    Timeout,
    /// Öğrenme: kaç örnek oldu, son örnek kullanılabildi mi.
    Trained(usize, bool),
    /// Okunan cümle ses izine eklendi mi.
    Enrolled(bool),
    /// Seslendirme bitti (`gen` o anki cevabın kuşağı).
    SpeakDone(u64),
    Error(String),
}

/// Çekirdeğe iş.
enum Job {
    Segment(Phase, Vec<i16>),
    /// Konuşurken araya girildi: ses kullanıcınınsa söz kesilir.
    Barge(Vec<i16>),
    Ptt(Vec<i16>),
    Config(CoreCfg),
    ClearTraining,
}

#[derive(Clone, Default)]
struct CoreCfg {
    groq_key: String,
    gemini_key: String,
    eleven_key: String,
    /// Uyandırma sözleri (notta birden çok olabilir).
    phrases: Vec<String>,
    /// Yalnızca kullanıcının sesine tepki ver.
    voice_lock: bool,
}

/// Konuşmacıya iş. Her iş sıraya girdiği andaki kuşağı taşır: susturulduktan sonra sırada
/// kalanlar atılır.
enum Say {
    /// Akıştan gelen cümle.
    Text(String, u64),
    /// Nottaki hazır cümle: önbellekte varsa oradan.
    Phrase(String, PathBuf, u64),
    /// Cevap bitti: çalma bitince SpeakDone.
    End(u64),
}

/// Konuşan ses.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum Engine {
    /// XTTS, bu bilgisayarın ekran kartında (bedava).
    Local,
    Eleven,
    Gemini,
    /// Windows'un yerleşik sesi (yalnızca açıkça seçilirse).
    Windows,
    /// Konuşmaz: cevaplar yalnızca ekranda, uyanınca çan.
    #[default]
    Silent,
}

#[derive(Clone, Default)]
struct SpeakCfg {
    engine: Engine,
    eleven_key: String,
    voice: String,
    model: String,
    /// Yerel sesin adı ve hızı.
    local_voice: String,
    local_speed: f32,
    gemini_key: String,
    gemini_voice: String,
}

/// Üç iş parçacığının ortak durumu.
struct Shared {
    hwnd: usize,
    phase: AtomicU8,
    since: Mutex<Instant>,
    events: Mutex<VecDeque<Ev>>,
    ptt: Mutex<Vec<i16>>,
    player: Player,
    /// Konuşmacının kuşağı: susturunca artar, sıradaki işler atılır.
    epoch: AtomicU64,
    /// Kullanıcı şu an konuşuyor (konuşma algılayıcıya göre).
    talking: AtomicBool,
    /// Çekirdek söz kesmeyi onayladı (ses kullanıcının): mikrofon iş parçacığı susturur.
    barge_go: AtomicBool,
    speak: Mutex<SpeakCfg>,
}

impl Shared {
    fn post(&self, e: Ev) {
        self.events.lock().unwrap().push_back(e);
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_VOICE, WPARAM(0), LPARAM(0));
        }
    }

    fn phase(&self) -> Phase {
        Phase::from(self.phase.load(Ordering::Relaxed))
    }
}

/// Arayüzün gördüğü durum.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Idle,
    Listening,
    Thinking,
    Speaking,
    Confirm,
    FollowUp,
    Training,
}

/// Nottaki sesli tepkiler.
#[derive(Default)]
struct Phrases {
    sections: HashMap<String, Vec<String>>,
    last: HashMap<String, usize>,
    /// Notun son hali (değişince yeniden okunur, önbellek ısıtılır).
    source: String,
}

pub const NOTE: &str = "ses";

/// İlk açılışta `sistem/ses.md`. `{ad}` hitap adıyla değişir.
pub const DEFAULT_NOTE: &str = "---
tags: [sistem]
---
# Ses

Bu dosya {ad}'in sesini belirler. Her başlığın altındaki satırlardan biri rastgele söylenir, üst üste aynısı değil. İstediğin gibi değiştir, ekle, sil; kaydettiğin an geçerli olur.

## Uyandırma sözü
Bunu duyunca uyanır. Birden çok yazabilirsin. Değiştirirsen Ayarlar'dan sesini yeniden öğret.
- hey matrix

## Uyanınca
- Efendim?
- Hı?
- Buyur.
- Dinliyorum.
- Söyle.

## Anlamayınca
- Tam anlayamadım, bir daha söyler misin?
- Kaçırdım, tekrar eder misin?

## Onay isterken
- Çalıştırayım mı?
- Onaylıyor musun?

## Onaylanınca
- Tamam.
- Hallediyorum.

## Vazgeçilince
- Peki, yapmıyorum.
- Tamam, vazgeçtim.

## Durdurulunca
- Tamam, durdum.
- Peki.

## Hata olunca
- Şu an bağlanamıyorum.
- Bir sorun çıktı, sonra tekrar dene.
";

/// Notun bölümleri: "## Başlık" altındaki "- satır"lar; başlıklar küçük harf anahtar.
fn parse_sections(body: &str, name: &str) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut cur: Option<String> = None;
    for line in crate::notes::md::strip_frontmatter(body).lines() {
        let l = line.trim();
        if let Some(h) = l.strip_prefix("## ") {
            cur = Some(crate::notes::key(h));
            continue;
        }
        if let (Some(c), Some(item)) = (&cur, l.strip_prefix("- ").or(l.strip_prefix("* "))) {
            let item = item.trim().replace("{ad}", name);
            if !item.is_empty() {
                out.entry(c.clone()).or_default().push(item);
            }
        }
    }
    out
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn cache_dir() -> PathBuf {
    crate::util::data_dir().join("ses-onbellek")
}

/// Sesini tanıtırken okunan cümleler: farklı sesler, uzunluklar, soru ve sayı.
const SENTENCES: [&str; 8] = [
    "Bugün hava çok güzel, biraz yürüyüşe çıkmak istiyorum.",
    "Spotify'da sakin bir şeyler aç ve sesi biraz kıs.",
    "Yarın sabah dokuzda toplantım var, bana hatırlatır mısın?",
    "Masaüstüne en son indirdiğim dosyayı bulup açar mısın?",
    "Dün akşam okuduğum makale yapay zekâ ile ilgiliydi.",
    "Şu an saat kaç, bir de bataryam ne durumda?",
    "Çalışma moduna geç, bildirimleri kapat ve müziği başlat.",
    "Üç yüz kırk beş lira harcadım, bunu not alır mısın?",
];

/// "Yalnızca benim sesim" eşiği (ses izi benzerliği): kendi kayıtların ~0,5–0,65, başka sesler
/// ~0,05–0,25 aldı.
const VOICE_THR: f32 = 0.30;

fn spotter_dir() -> PathBuf {
    crate::util::data_dir().join("uyandirma")
}

impl SpeakCfg {
    fn from(cfg: &Config) -> Self {
        let engine = match cfg.tts.as_str() {
            "xtts" => Engine::Local,
            "gemini" if !cfg.gemini_key.is_empty() => Engine::Gemini,
            "windows" => Engine::Windows,
            "kapali" => Engine::Silent,
            _ if cfg.elevenlabs_key.is_empty() => Engine::Silent,
            _ => Engine::Eleven,
        };
        SpeakCfg {
            engine,
            eleven_key: cfg.elevenlabs_key.clone(),
            voice: if cfg.voice.is_empty() { crate::eleven::DEFAULT_VOICE.to_string() } else { cfg.voice.clone() },
            model: cfg.voice_model.clone(),
            local_voice: cfg.local_voice.clone(),
            local_speed: cfg.local_speed,
            gemini_key: cfg.gemini_key.clone(),
            gemini_voice: cfg.gemini_voice.clone(),
        }
    }

    fn silent(&self) -> bool {
        self.engine == Engine::Silent
    }

    /// Önbellek dosyası: motor, ses ve cümleye göre.
    fn cache_path(&self, text: &str) -> PathBuf {
        let engine = match self.engine {
            Engine::Local => format!("xtts-{}-{:.2}", self.local_voice, self.local_speed),
            Engine::Windows => "windows".to_string(),
            Engine::Gemini => format!("gemini-{}", self.gemini_voice),
            _ => format!("{}-{}", self.voice, self.model),
        };
        cache_dir().join(format!("{:016x}.wav", fnv(&format!("{engine}|{text}"))))
    }

    /// Cümleyi seslendirir (tamamı).
    fn synth(&self, text: &str) -> crate::util::Res<(Vec<i16>, u32)> {
        match self.engine {
            Engine::Silent => Err("ses kapalı".into()),
            Engine::Windows => audio::windows_tts(text, "tr-TR"),
            Engine::Local => {
                crate::xtts::wait_ready(Duration::from_secs(60));
                Ok((crate::xtts::speak_all(text, &self.local_voice, self.local_speed)?, crate::xtts::RATE))
            }
            Engine::Gemini => crate::gemini::tts(&self.gemini_key, &self.gemini_voice, text),
            Engine::Eleven => Ok((crate::eleven::tts(&self.eleven_key, &self.voice, &self.model, text)?, crate::eleven::TTS_RATE)),
        }
    }
}

pub struct Voice {
    sh: Arc<Shared>,
    capture: Option<Capture>,
    core: mpsc::Sender<Job>,
    speaker: mpsc::Sender<Say>,
    pub state: State,
    phrases: Phrases,
    /// Seslendirilmeyi bekleyen cevap parçası (cümle bitince konuşmacıya gider).
    pending: String,
    /// Ekran ışığında gösterilen cevap.
    answer: String,
    /// Bu cevabın kuşağı (SpeakDone eşleşmesi).
    answer_gen: u64,
    /// Bu turda ses çıktısı var mı (yazılı turda konuşulmaz).
    voiced: bool,
    /// Öğrenme sırasında örnek sayısı.
    pub trained: usize,
    pub train_msg: String,
    cfg: Config,
    timer_on: bool,
    /// Sihirbazda okunan cümle sırası.
    sentence: usize,
}

impl Voice {
    pub fn new(hwnd: HWND) -> Voice {
        let sh = Arc::new(Shared {
            hwnd: hwnd.0 as usize,
            phase: AtomicU8::new(Phase::Off as u8),
            since: Mutex::new(Instant::now()),
            events: Mutex::new(VecDeque::new()),
            ptt: Mutex::new(Vec::new()),
            player: Player::new(),
            epoch: AtomicU64::new(0),
            talking: AtomicBool::new(false),
            barge_go: AtomicBool::new(false),
            speak: Mutex::new(SpeakCfg::default()),
        });
        let (core, core_rx) = mpsc::channel();
        let (speaker, speaker_rx) = mpsc::channel();
        {
            let sh = sh.clone();
            std::thread::spawn(move || core_loop(sh, core_rx));
        }
        {
            let sh = sh.clone();
            std::thread::spawn(move || speaker_loop(sh, speaker_rx));
        }
        overlay::init(hwnd, WM_CONFIRM);
        let trained = Spotter::load(&spotter_dir()).count();
        Voice {
            sh,
            capture: None,
            core,
            speaker,
            state: State::Idle,
            phrases: Phrases::default(),
            pending: String::new(),
            answer: String::new(),
            answer_gen: 0,
            voiced: false,
            trained,
            train_msg: String::new(),
            cfg: Config::default(),
            timer_on: false,
            sentence: 0,
        }
    }

    fn hwnd(&self) -> HWND {
        HWND(self.sh.hwnd as *mut _)
    }

    fn set_phase(&self, p: Phase) {
        self.sh.phase.store(p as u8, Ordering::Relaxed);
        *self.sh.since.lock().unwrap() = Instant::now();
    }

    /// Ayarlar ya da ses notu değişti: mikrofonu açar/kapatır, cümleleri yeniden okur.
    pub fn apply(&mut self, cfg: &Config, note: &str) {
        self.cfg = cfg.clone();
        audio::set_mic(&cfg.mic);
        let sc = SpeakCfg::from(cfg);
        // Ses sunucusu hem yerel sesi hem "yalnızca benim sesim"i taşır.
        if sc.engine == Engine::Local || cfg.voice_lock {
            crate::xtts::start();
        } else {
            crate::xtts::stop();
        }
        let speak_changed = {
            let mut cur = self.sh.speak.lock().unwrap();
            let changed = cur.voice != sc.voice
                || cur.engine != sc.engine
                || cur.model != sc.model
                || cur.eleven_key != sc.eleven_key
                || cur.local_voice != sc.local_voice
                || cur.local_speed != sc.local_speed
                || cur.gemini_voice != sc.gemini_voice;
            *cur = sc.clone();
            changed
        };
        if note != self.phrases.source || speak_changed {
            self.phrases.sections = parse_sections(note, &cfg.name);
            self.phrases.source = note.to_string();
            self.warm_cache(sc);
        }
        let phrases = self.phrases.sections.get("uyandırma sözü").cloned().unwrap_or_else(|| vec![cfg.wake_word.clone()]);
        let _ = self.core.send(Job::Config(CoreCfg {
            groq_key: cfg.groq_key.clone(),
            gemini_key: cfg.gemini_key.clone(),
            eleven_key: cfg.elevenlabs_key.clone(),
            phrases,
            voice_lock: cfg.voice_lock,
        }));
        self.ensure_capture(cfg.wake || self.state != State::Idle);
        if self.state == State::Idle {
            self.set_phase(if cfg.wake && self.capture.is_some() { Phase::Wake } else { Phase::Off });
        }
    }

    /// Nottaki bütün cümleleri arka planda seslendirip önbelleğe yazar.
    fn warm_cache(&self, sc: SpeakCfg) {
        let items: Vec<String> = self
            .phrases
            .sections
            .iter()
            .filter(|(k, _)| k.as_str() != "uyandırma sözü")
            .flat_map(|(_, v)| v.iter().cloned())
            .collect();
        if sc.silent() {
            return;
        }
        std::thread::spawn(move || {
            // Yerel ses yükleniyorsa hazır olmasını bekle.
            if sc.engine == Engine::Local && !crate::xtts::wait_ready(Duration::from_secs(200)) {
                return;
            }
            let _ = std::fs::create_dir_all(cache_dir());
            for text in items {
                let path = sc.cache_path(&text);
                if path.exists() {
                    continue;
                }
                match sc.synth(&text) {
                    Ok((pcm, rate)) => {
                        let _ = std::fs::write(&path, audio::wav(&pcm, rate));
                    }
                    Err(e) => {
                        crate::log!("ses önbelleği: {e}");
                        return;
                    }
                }
            }
        });
    }

    fn ensure_capture(&mut self, on: bool) {
        if on && self.capture.is_none() {
            let sh = self.sh.clone();
            let mut vad = Vad::new();
            let core = self.core.clone();
            let mut n = 0u32;
            let mut started = false;
            // Söz kesme: konuşurken son yarım saniye tutulur (cümlenin başı kaybolmasın); kendi
            // sesimizin mikrofona ne kadar sızdığı (kulaklıkta ~0, hoparlörde yüksek) öğrenilir.
            let mut ring: VecDeque<Vec<i16>> = VecDeque::new();
            let mut coupling = 0.5f32;
            // Sızma oranı kaç çerçevede öğrenildi (öğrenilince söz kesme daha erken açılır).
            let mut learned = 0u32;
            // Mikrofonun gürültü tabanı (konuşma yokken seviye; yavaş iner, çok yavaş çıkar).
            let mut floor = 0.2f32;
            // Söz kesme adayı çekirdekte doğrulanıyor (ne zaman soruldu).
            let mut checking: Option<Instant> = None;
            let mut loud = 0u32;
            let mut playing_since: Option<Instant> = None;
            let r = Capture::start(Box::new(move |frame: &[i16]| {
                let phase = sh.phase();
                n = n.wrapping_add(1);
                let listening = matches!(phase, Phase::Command | Phase::FollowUp | Phase::Confirm | Phase::Ptt | Phase::Train | Phase::Enroll);
                if listening && n % 2 == 0 {
                    let lv = (audio::level(frame) * 1000.0) as usize;
                    unsafe {
                        let _ = PostMessageW(Some(HWND(sh.hwnd as *mut _)), WM_LEVEL, WPARAM(lv), LPARAM(0));
                    }
                }
                if phase == Phase::Ptt {
                    sh.ptt.lock().unwrap().extend_from_slice(frame);
                    return;
                }
                // Kendi sesimiz çalarken yankısı komut sanılmaz; ama kullanıcı yankının belirgin
                // üstünde konuşursa susulur ve dinlemeye geçilir.
                if sh.player.busy() {
                    let since = *playing_since.get_or_insert_with(Instant::now);
                    ring.push_back(frame.to_vec());
                    if ring.len() > 60 {
                        ring.pop_front();
                    }
                    let (lv, out) = (audio::level(frame), sh.player.level());
                    // Söz kesme için: kendi sesimizin sızmasının, mikrofon gürültüsünün ve sakin bir
                    // konuşma seviyesinin belirgin üstünde olmalı.
                    let thr = (out * coupling + 0.2).max(floor + 0.25).max(0.42);
                    if lv > thr {
                        loud += 1;
                    } else {
                        loud = 0;
                        if out > 0.2 {
                            coupling = (coupling * 0.97 + (lv / out).min(1.2) * 0.03).clamp(0.02, 1.2);
                            learned = learned.saturating_add(1);
                        }
                    }
                    let settle = Duration::from_millis(if learned > 50 { 150 } else { 400 });
                    // Yüksek ses kullanıcının mı? Çekirdeğe sorulur, cevap gelene kadar dinlenmeye devam.
                    if loud >= 7 && since.elapsed() > settle && phase != Phase::Off && checking.is_none_or(|t| t.elapsed() > Duration::from_millis(1500)) {
                        loud = 0;
                        checking = Some(Instant::now());
                        let tail: Vec<i16> = ring.iter().rev().take(40).collect::<Vec<_>>().into_iter().rev().flatten().copied().collect();
                        let _ = core.send(Job::Barge(tail));
                    }
                    if sh.barge_go.swap(false, Ordering::Relaxed) {
                        checking = None;
                        sh.epoch.fetch_add(1, Ordering::Relaxed);
                        sh.player.stop();
                        sh.phase.store(Phase::Command as u8, Ordering::Relaxed);
                        *sh.since.lock().unwrap() = Instant::now();
                        vad.reset();
                        for f in ring.drain(..) {
                            let _ = vad.push(&f);
                        }
                        started = true;
                        playing_since = None;
                        sh.post(Ev::BargeIn);
                        return;
                    }
                    vad.reset();
                    started = false;
                    sh.talking.store(false, Ordering::Relaxed);
                    *sh.since.lock().unwrap() = Instant::now();
                    return;
                }
                playing_since = None;
                ring.clear();
                checking = None;
                sh.barge_go.store(false, Ordering::Relaxed);
                // Gürültü tabanı: sessizken izlenir.
                let lv = audio::level(frame);
                floor = if lv < floor { floor * 0.9 + lv * 0.1 } else { floor * 0.999 + lv * 0.001 };
                // Uyandırma sözü kısa ve tek nefes; komutta düşünürken verilen duraklamalar kesmesin.
                vad.silence_ms = if phase == Phase::Wake { 450 } else { 800 };
                if phase == Phase::Off {
                    vad.reset();
                    started = false;
                    return;
                }
                match vad.push(frame) {
                    VadEvent::Start => {
                        started = true;
                        if listening {
                            sh.post(Ev::SpeechStart);
                        }
                    }
                    VadEvent::End(seg) => {
                        started = false;
                        let _ = core.send(Job::Segment(phase, seg));
                    }
                    VadEvent::None => {}
                }
                sh.talking.store(started, Ordering::Relaxed);
                if !started
                    && let Some(p) = phase.patience()
                    && sh.since.lock().unwrap().elapsed() > p
                {
                    *sh.since.lock().unwrap() = Instant::now();
                    sh.post(Ev::Timeout);
                }
            }));
            match r {
                Ok(c) => self.capture = Some(c),
                Err(e) => crate::log!("mikrofon açılamadı: {e}"),
            }
        } else if !on && self.capture.is_some() {
            self.capture = None;
        }
    }

    /// Nottaki bölümden rastgele (bir öncekinden farklı) bir cümle söyler.
    fn say_phrase(&mut self, section: &str) {
        let sc = self.sh.speak.lock().unwrap().clone();
        // Ses yok ya da yerel ses henüz yükleniyor: uyanınca çan çalar.
        let local_wait = sc.engine == Engine::Local && crate::xtts::status() != crate::xtts::Status::Ready;
        if sc.silent() || local_wait {
            if section == "uyanınca" {
                let _ = self.speaker.send(Say::Phrase(String::new(), PathBuf::new(), self.sh.epoch.load(Ordering::Relaxed)));
            }
            return;
        }
        let Some(list) = self.phrases.sections.get(section).filter(|l| !l.is_empty()) else { return };
        let mut i = (fnv(&format!("{:?}", Instant::now())) % list.len() as u64) as usize;
        if list.len() > 1 && self.phrases.last.get(section) == Some(&i) {
            i = (i + 1) % list.len();
        }
        self.phrases.last.insert(section.to_string(), i);
        let text = list[i].clone();
        let path = self.sh.speak.lock().unwrap().cache_path(&text);
        let _ = self.speaker.send(Say::Phrase(text, path, self.sh.epoch.load(Ordering::Relaxed)));
    }

    /// Konuşmayı keser, sıradakileri atar.
    fn hush(&mut self) {
        self.sh.epoch.fetch_add(1, Ordering::Relaxed);
        self.sh.player.stop();
        self.pending.clear();
        // Süren turdan gelmeye devam eden yazı artık seslendirilmez.
        self.voiced = false;
    }

    fn show(&self, mode: overlay::Mode, title: &str, text: &str) {
        overlay::set(mode, title, text);
    }

    /// Uyandı: "Efendim?" der, komutu dinler.
    pub fn listen(&mut self, brain: &mut Brain) {
        if brain.busy {
            brain.cancel();
        }
        self.hush();
        self.ensure_capture(true);
        self.state = State::Listening;
        self.answer.clear();
        self.show(overlay::Mode::Listening, t!("Listening", "Dinliyorum"), "");
        // Kullanıcı sözü bitirmeden devam ediyorsa "Efendim?" araya girmez: dinleme kesilmesin.
        if !self.sh.talking.load(Ordering::Relaxed) {
            self.say_phrase("uyanınca");
        }
        self.set_phase(Phase::Command);
    }

    /// Dinleme bitti, hiçbir şey olmadı: ışık söner, uyandırma beklenir.
    fn idle(&mut self) {
        self.state = State::Idle;
        self.show(overlay::Mode::Hidden, "", "");
        let listen = self.cfg.wake;
        self.set_phase(if listen && self.capture.is_some() { Phase::Wake } else { Phase::Off });
        if !listen {
            self.ensure_capture(false);
        }
        self.stop_timer();
    }

    /// Duyulan komutu beyne verir.
    fn ask_brain(&mut self, text: &str, brain: &mut Brain) {
        self.state = State::Thinking;
        self.answer.clear();
        self.pending.clear();
        self.voiced = true;
        self.answer_gen = self.sh.epoch.load(Ordering::Relaxed);
        self.show(overlay::Mode::Thinking, t!("Thinking", "Düşünüyorum"), text);
        // Düşünürken de uyandırma sözü dinlenir: söylenirse bu tur bırakılır.
        self.set_phase(if self.cfg.wake { Phase::Wake } else { Phase::Off });
        if !brain.send(text, Mode::Voice, &self.cfg) {
            self.idle();
        }
    }

    // --- Arayüz olayları ---

    /// WM_VOICE.
    pub fn on_events(&mut self, brain: &mut Brain) {
        let items: Vec<Ev> = self.sh.events.lock().unwrap().drain(..).collect();
        for e in items {
            match e {
                Ev::Wake(rest) => {
                    crate::log!("uyandı");
                    match rest.filter(|r| r.split_whitespace().count() >= 2) {
                        Some(cmd) => {
                            self.hush();
                            if brain.busy {
                                brain.cancel();
                            }
                            self.ask_brain(&cmd, brain);
                        }
                        None => self.listen(brain),
                    }
                }
                Ev::BargeIn => {
                    crate::log!("söz kesildi");
                    self.voiced = false;
                    if brain.busy {
                        brain.cancel();
                    }
                    self.pending.clear();
                    self.answer.clear();
                    self.state = State::Listening;
                    self.show(overlay::Mode::Listening, t!("Listening", "Dinliyorum"), "…");
                }
                Ev::SpeechStart => {
                    if matches!(self.state, State::Listening | State::FollowUp) {
                        self.state = State::Listening;
                        self.show(overlay::Mode::Listening, t!("Listening", "Dinliyorum"), "…");
                    }
                }
                Ev::Heard(phase, text) => self.heard(phase, text, brain),
                Ev::Timeout => match self.state {
                    State::Listening | State::FollowUp => self.idle(),
                    State::Confirm => {
                        // Sesli cevap gelmedi: soru ekranda kalır, tıklanabilir.
                        self.set_phase(Phase::Off);
                    }
                    _ => {}
                },
                Ev::Enrolled(ok) => {
                    if self.state != State::Training {
                        continue;
                    }
                    if ok {
                        self.sentence += 1;
                    }
                    if self.sentence >= SENTENCES.len() {
                        self.train_msg = t!("Done. I know your voice now.", "Tamam. Artık sesini tanıyorum.").into();
                        self.show(overlay::Mode::Speaking, t!("Learned", "Öğrendim"), &self.train_msg.clone());
                        self.state = State::Idle;
                        self.set_phase(if self.cfg.wake { Phase::Wake } else { Phase::Off });
                        self.start_timer_once_hide();
                    } else {
                        self.show_sentence(if ok { "" } else { t!("Didn't catch that, read it again: ", "Net duyamadım, tekrar oku: ") });
                        self.set_phase(Phase::Enroll);
                    }
                }
                Ev::Trained(n, ok) => {
                    self.trained = n;
                    self.train_msg = if ok {
                        if n >= 3 { t!("Learned. You can say it now.", "Öğrendim. Artık söyleyebilirsin.").into() } else { format!("{n}/3") }
                    } else {
                        t!("Didn't catch that clearly, say it again.", "Net duyamadım, bir daha söyle.").into()
                    };
                    let phrase = self.wake_phrase();
                    if n >= 3 {
                        // Uyandırma sözü tamam; ses sunucusu hazırsa cümlelerle ses izi çıkarılır.
                        if crate::xtts::status() == crate::xtts::Status::Ready {
                            self.sentence = 0;
                            self.show_sentence("");
                            self.set_phase(Phase::Enroll);
                        } else {
                            self.show(overlay::Mode::Speaking, t!("Learned", "Öğrendim"), &self.train_msg.clone());
                            self.state = State::Idle;
                            self.set_phase(if self.cfg.wake { Phase::Wake } else { Phase::Off });
                            self.start_timer_once_hide();
                        }
                    } else {
                        self.show(overlay::Mode::Listening, t!("Learning your voice", "Sesini öğreniyorum"), &format!("\"{phrase}\" de · {}/3", n));
                    }
                }
                Ev::SpeakDone(g) => {
                    if g == self.answer_gen && self.state == State::Speaking {
                        // Cevap bitti: kısa süre uyandırmasız dinle.
                        self.state = State::FollowUp;
                        self.show(overlay::Mode::Listening, t!("Go on", "Devam edebilirsin"), &self.answer.clone());
                        self.set_phase(Phase::FollowUp);
                    }
                }
                Ev::Error(e) => {
                    crate::log!("ses: {e}");
                    if self.state != State::Idle {
                        self.show(overlay::Mode::Speaking, t!("Problem", "Sorun"), &e);
                        self.say_phrase("hata olunca");
                        self.state = State::Speaking;
                        self.answer_gen = self.sh.epoch.load(Ordering::Relaxed);
                        let _ = self.speaker.send(Say::End(self.answer_gen));
                    }
                }
            }
        }
    }

    fn heard(&mut self, phase: Phase, text: String, brain: &mut Brain) {
        crate::log!("duyuldu ({phase:?}): {text}");
        match phase {
            Phase::Confirm => {
                if brain.ask.is_none() {
                    return;
                }
                match wake::yes_no(&text) {
                    Some(yes) => self.confirm(yes, brain),
                    None => {
                        self.say_phrase("anlamayınca");
                        self.set_phase(Phase::Confirm);
                    }
                }
            }
            Phase::Command | Phase::FollowUp | Phase::Ptt => {
                let t = text.trim();
                if t.is_empty() {
                    if phase == Phase::FollowUp {
                        self.idle();
                    } else {
                        self.say_phrase("anlamayınca");
                        self.state = State::Listening;
                        self.set_phase(Phase::Command);
                    }
                    return;
                }
                // "hey matrix ..." diye başlanmışsa söz atılır.
                let phrases = self.phrases.sections.get("uyandırma sözü").cloned().unwrap_or_default();
                let cmd = phrases.iter().find_map(|p| wake::match_wake(t, p).filter(|r| !r.is_empty())).unwrap_or_else(|| t.to_string());
                self.ask_brain(&cmd, brain);
            }
            _ => {}
        }
    }

    pub fn wake_phrase(&self) -> String {
        self.phrases.sections.get("uyandırma sözü").and_then(|v| v.first().cloned()).unwrap_or_else(|| self.cfg.wake_word.clone())
    }

    /// Beynin olayları: sesli turda cevap ekranda akar ve cümle cümle seslendirilir;
    /// bilgisayar kontrolünde ışık "çalışıyor"a geçer.
    pub fn on_agent(&mut self, outs: &[Out], brain: &Brain) {
        let voiced = brain.mode == Mode::Voice && self.voiced;
        for o in outs {
            match o {
                Out::Text(t) if voiced => {
                    self.answer.push_str(t);
                    self.pending.push_str(t);
                    self.flush_sentences(false);
                    if self.state == State::Thinking {
                        self.state = State::Speaking;
                        self.start_timer();
                    }
                    if self.state == State::Speaking {
                        self.show(overlay::Mode::Speaking, &self.cfg.name.clone(), &crate::notes::md::plain(&self.answer));
                    }
                }
                Out::Progress(t) | Out::Tool(t) if voiced || overlay::mode() == overlay::Mode::Working => {
                    if !matches!(self.state, State::Confirm) {
                        let mode = if overlay::mode() == overlay::Mode::Working { overlay::Mode::Working } else { overlay::Mode::Thinking };
                        self.show(mode, t, &crate::notes::md::plain(&self.answer));
                    }
                }
                Out::Computer => {
                    self.show(overlay::Mode::Working, t!("Working on your computer", "Bilgisayarında çalışıyorum"), "");
                }
                Out::Done => {
                    if voiced {
                        self.flush_sentences(true);
                        self.answer_gen = self.sh.epoch.load(Ordering::Relaxed);
                        if self.answer.trim().is_empty() && brain.msgs.last().and_then(|m| m.error.as_ref()).is_some() {
                            self.say_phrase("hata olunca");
                        }
                        self.state = State::Speaking;
                        self.start_timer();
                        let _ = self.speaker.send(Say::End(self.answer_gen));
                        self.voiced = false;
                    } else if overlay::mode() == overlay::Mode::Working {
                        self.show(overlay::Mode::Hidden, "", "");
                    }
                }
                _ => {}
            }
        }
        // Onay sorusu geldi.
        if let Some(a) = &brain.ask
            && self.state != State::Confirm
            && (voiced || brain.mode == Mode::Voice || overlay::mode() != overlay::Mode::Hidden)
        {
            let (q, d) = (a.question.clone(), a.detail.clone());
            self.state = State::Confirm;
            self.show(overlay::Mode::Confirm, &q, &d);
            self.say_phrase("onay isterken");
            self.ensure_capture(true);
            self.set_phase(Phase::Confirm);
        }
    }

    /// Biriken cevaptan tamamlanan cümleleri konuşmacıya verir.
    fn flush_sentences(&mut self, all: bool) {
        loop {
            let cut = self
                .pending
                .char_indices()
                .find(|&(i, c)| matches!(c, '.' | '!' | '?' | '…' | ':' | ';' | '\n') && i >= 12 && self.pending[i + c.len_utf8()..].starts_with([' ', '\n']))
                .map(|(i, c)| i + c.len_utf8());
            match cut {
                Some(n) => {
                    let s: String = self.pending.drain(..n).collect();
                    self.speak_text(&s);
                }
                None => break,
            }
        }
        if all && !self.pending.trim().is_empty() {
            let s = std::mem::take(&mut self.pending);
            self.speak_text(&s);
        }
    }

    /// Kendiliğinden söylenen şey (hatırlatma): ışıkta görünür, sesli okunur, sonra kısa süre
    /// cevap beklenir.
    pub fn announce(&mut self, title: &str, text: &str) {
        self.hush();
        self.answer = text.to_string();
        self.show(overlay::Mode::Speaking, title, text);
        if self.sh.speak.lock().unwrap().silent() {
            let _ = self.speaker.send(Say::Phrase(String::new(), PathBuf::new(), self.sh.epoch.load(Ordering::Relaxed)));
        } else {
            self.speak_text(&format!("{title}: {text}"));
        }
        self.state = State::Speaking;
        self.answer_gen = self.sh.epoch.load(Ordering::Relaxed);
        self.ensure_capture(true);
        self.start_timer();
        let _ = self.speaker.send(Say::End(self.answer_gen));
    }

    fn speak_text(&self, s: &str) {
        let s = crate::notes::md::plain(s);
        let s = s.trim();
        if !s.is_empty() {
            let _ = self.speaker.send(Say::Text(s.to_string(), self.sh.epoch.load(Ordering::Relaxed)));
        }
    }

    /// Onay cevabı (sesle ya da ekran ışığındaki düğmeyle).
    pub fn confirm(&mut self, yes: bool, brain: &mut Brain) {
        if brain.ask.is_none() {
            return;
        }
        brain.answer(yes);
        self.say_phrase(if yes { "onaylanınca" } else { "vazgeçilince" });
        self.state = State::Thinking;
        self.show(if yes { overlay::Mode::Working } else { overlay::Mode::Thinking }, if yes { t!("Working", "Çalışıyorum") } else { t!("Cancelled", "Vazgeçildi") }, "");
        self.set_phase(if self.cfg.wake { Phase::Wake } else { Phase::Off });
    }

    /// Bas-konuş tuşu (1 basıldı, 0 bırakıldı) ya da Esc (2).
    pub fn key(&mut self, code: usize, brain: &mut Brain) {
        match code {
            2 => {
                // Esc: engram sesli ya da ekranda bir şey yaparken her şeyi durdurur ve mikrofonu kapatır.
                if self.state != State::Idle || overlay::mode() != overlay::Mode::Hidden || brain.busy && brain.mode == Mode::Voice {
                    self.pause(brain);
                }
            }
            1 => {
                if self.cfg.ptt == crate::config::PttMode::Toggle && self.state == State::Listening {
                    self.ptt_release();
                    return;
                }
                if brain.busy {
                    brain.cancel();
                }
                self.hush();
                        self.ensure_capture(true);
                self.sh.ptt.lock().unwrap().clear();
                self.state = State::Listening;
                self.answer.clear();
                self.show(overlay::Mode::Listening, t!("Listening", "Dinliyorum"), "");
                self.set_phase(Phase::Ptt);
            }
            _ => {
                if self.cfg.ptt == crate::config::PttMode::Hold {
                    self.ptt_release();
                }
            }
        }
    }

    fn ptt_release(&mut self) {
        if self.sh.phase() != Phase::Ptt {
            return;
        }
        let buf = std::mem::take(&mut *self.sh.ptt.lock().unwrap());
        self.set_phase(Phase::Off);
        if buf.len() < audio::RATE as usize / 4 {
            self.idle();
            return;
        }
        self.state = State::Thinking;
        self.show(overlay::Mode::Thinking, t!("Listening", "Dinliyorum"), "…");
        let _ = self.core.send(Job::Ptt(buf));
    }

    /// Her şeyi durdurur: tur, konuşma, ışık.
    /// Esc: tur, konuşma, dinleme ve ışık durur; uyandırma sözü yine çalışır.
    pub fn pause(&mut self, brain: &mut Brain) {
        crate::log!("durduruldu");
        brain.cancel();
        self.hush();
        self.idle();
    }

    pub fn stop(&mut self, brain: &mut Brain) {
        let was = brain.busy || self.state != State::Idle;
        brain.cancel();
        self.hush();
        if was {
            self.say_phrase("durdurulunca");
        }
        self.idle();
    }

    /// Sihirbazda okunacak cümle.
    fn show_sentence(&mut self, prefix: &str) {
        let n = self.sentence.min(SENTENCES.len() - 1);
        self.train_msg = format!("{}/{}", n + 1, SENTENCES.len());
        self.show(
            overlay::Mode::Listening,
            &t!(format!("Read aloud · {}/{}", n + 1, SENTENCES.len()), format!("Sesli oku · {}/{}", n + 1, SENTENCES.len())),
            &format!("{prefix}{}", SENTENCES[n]),
        );
    }

    /// Uyandırma sözünü öğrenmeye başlar (Ayarlar).
    pub fn train(&mut self, brain: &mut Brain) {
        if brain.busy {
            brain.cancel();
        }
        self.hush();
        let _ = self.core.send(Job::ClearTraining);
        self.trained = 0;
        self.ensure_capture(true);
        self.state = State::Training;
        let phrase = self.wake_phrase();
        self.train_msg = "0/3".into();
        self.show(overlay::Mode::Listening, t!("Learning your voice", "Sesini öğreniyorum"), &format!("\"{phrase}\" de · 0/3"));
        self.set_phase(Phase::Train);
    }

    /// WM_LEVEL.
    pub fn level(&self, v: f32) {
        if matches!(self.state, State::Listening | State::FollowUp | State::Training | State::Confirm) {
            overlay::level(v);
        }
    }

    fn start_timer(&mut self) {
        if !self.timer_on {
            unsafe {
                SetTimer(Some(self.hwnd()), TIMER, 33, None);
            }
            self.timer_on = true;
        }
    }

    fn start_timer_once_hide(&mut self) {
        // Öğrenme bitti mesajı kısa süre görünür, sonra söner (tick'te).
        self.start_timer();
    }

    fn stop_timer(&mut self) {
        if self.timer_on {
            unsafe {
                let _ = KillTimer(Some(self.hwnd()), TIMER);
            }
            self.timer_on = false;
        }
    }

    /// TIMER: konuşurken çıkış seviyesini ışığa verir.
    pub fn tick(&mut self) {
        match self.state {
            State::Speaking | State::Thinking => overlay::level(self.sh.player.level()),
            State::Idle if overlay::mode() != overlay::Mode::Hidden && !self.sh.player.busy() => {
                // Öğrenme bitti bildirimi: biraz bekle, sonra kapat.
                let since = *self.sh.since.lock().unwrap();
                if since.elapsed() > Duration::from_millis(1800) {
                    self.show(overlay::Mode::Hidden, "", "");
                    self.stop_timer();
                }
            }
            _ => {}
        }
    }

    /// Çalan sesin seviyesi (küre için).
    pub fn out_level(&self) -> f32 {
        if self.state == State::Speaking { self.sh.player.level() } else { 0.0 }
    }
}

// --- Çekirdek: uyandırma eşleme ve yazıya çevirme ---

fn core_loop(sh: Arc<Shared>, rx: mpsc::Receiver<Job>) {
    let mut spotter = Spotter::load(&spotter_dir());
    let mut cfg = CoreCfg::default();
    // Yazıya çevirme: Groq (en hızlı), Gemini, ElevenLabs; ilki hata verirse sıradaki denenir.
    let stt = |cfg: &CoreCfg, pcm: &[i16]| -> Result<String, String> {
        let mut last = None;
        if !cfg.groq_key.is_empty() {
            match crate::groq::stt(&cfg.groq_key, pcm, audio::RATE, Some("tr")) {
                Ok(t) => return Ok(t),
                Err(e) => last = Some(e.to_string()),
            }
        }
        if !cfg.gemini_key.is_empty() {
            match crate::gemini::stt(&cfg.gemini_key, pcm, audio::RATE) {
                Ok(t) => return Ok(t),
                Err(e) => last = Some(e.to_string()),
            }
        }
        if cfg.eleven_key.is_empty() {
            return Err(last.unwrap_or_else(|| t!("Voice commands need a Groq or Gemini key (Settings).", "Sesli komut için Groq ya da Gemini anahtarı gerekli (Ayarlar).").into()));
        }
        crate::eleven::stt(&cfg.eleven_key, pcm, audio::RATE, Some("tur")).map_err(|e| e.to_string())
    };
    // Ses kullanıcının mı (yalnızca benim sesim açıksa ve ses izi varsa). Ses sunucusu hazır
    // değilse ya da ses çok kısaysa geçer.
    let is_me = |cfg: &CoreCfg, pcm: &[i16], what: &str| -> bool {
        if !cfg.voice_lock || crate::xtts::status() != crate::xtts::Status::Ready || pcm.len() < audio::RATE as usize / 2 {
            return true;
        }
        match crate::xtts::who(pcm) {
            Ok((_, 0)) => true,
            Ok((sim, _)) => {
                let me = sim >= VOICE_THR;
                if !me {
                    crate::log!("başka ses yok sayıldı ({what}, benzerlik {sim:.2})");
                }
                me
            }
            Err(_) => true,
        }
    };
    // İlk açılışta ses izi boşsa öğretilmiş uyandırma kayıtlarından çıkarılır.
    let mut seeded = false;
    while let Ok(job) = rx.recv() {
        if !seeded && cfg.voice_lock && crate::xtts::status() == crate::xtts::Status::Ready {
            seeded = true;
            if crate::xtts::prints() == Some(0) {
                let mut n = 0;
                for e in std::fs::read_dir(spotter_dir()).into_iter().flatten().flatten() {
                    if let Some((pcm, rate)) = std::fs::read(e.path()).ok().and_then(|b| audio::decode_wav(&b)) {
                        n = crate::xtts::enroll(&audio::resample(&pcm, rate, audio::RATE)).unwrap_or(n);
                    }
                }
                crate::log!("ses izi uyandırma kayıtlarından çıkarıldı ({n} örnek)");
            }
        }
        match job {
            Job::Config(c) => cfg = c,
            Job::Barge(pcm) => {
                if is_me(&cfg, &pcm, "söz kesme") {
                    sh.barge_go.store(true, Ordering::Relaxed);
                }
            }
            Job::ClearTraining => {
                spotter.clear();
                let _ = spotter.save(&spotter_dir());
                if crate::xtts::status() == crate::xtts::Status::Ready {
                    let _ = crate::xtts::forget();
                }
            }
            Job::Ptt(pcm) => match stt(&cfg, &pcm) {
                Ok(t) => sh.post(Ev::Heard(Phase::Ptt, t)),
                Err(e) => sh.post(Ev::Error(e)),
            },
            Job::Segment(phase, seg) => {
                // Evre bu arada değiştiyse eski kayıt atılır.
                if sh.phase() != phase && !(phase == Phase::Wake && sh.phase() == Phase::Wake) {
                    if !matches!(phase, Phase::Command | Phase::FollowUp | Phase::Confirm) || sh.phase() == Phase::Off {
                        continue;
                    }
                }
                let secs = seg.len() as f32 / audio::RATE as f32;
                match phase {
                    Phase::Wake => {
                        // Önce yerel eşleme (anında, buluta ses gitmez); emin olamazsa kısa
                        // konuşma yazıya çevrilip söz aranır. Bulutun bulduğu örnek yerel
                        // tanımaya eklenir: kullandıkça kendi sesine alışır.
                        let mut sim = None;
                        if spotter.count() > 0 && secs <= 2.6 {
                            sim = spotter.similarity(&seg);
                            if let Some(v) = sim {
                                crate::log!("uyandırma benzerliği {v:.2} (eşik {:.2})", spotter.threshold());
                                if v >= spotter.threshold() {
                                    if is_me(&cfg, &seg, "uyandırma") {
                                        sh.post(Ev::Wake(None));
                                    }
                                    continue;
                                }
                            }
                        }
                        let cloud = !cfg.groq_key.is_empty() || !cfg.gemini_key.is_empty() || !cfg.eleven_key.is_empty();
                        // Çok farklı sesler (öksürük, kapı) buluta gitmez.
                        let plausible = sim.is_none_or(|v| v >= 0.12) || secs > 2.6;
                        if cloud && plausible && (0.3..=7.0).contains(&secs)
                            && is_me(&cfg, &seg, "uyandırma")
                            && let Ok(t) = stt(&cfg, &seg)
                            && let Some(rest) = cfg.phrases.iter().find_map(|p| wake::match_wake(&t, p))
                        {
                            crate::log!("uyandı (bulut): {t}");
                            if rest.trim().is_empty() && secs <= 2.6 && spotter.adapt(&seg, 8) {
                                let _ = spotter.save(&spotter_dir());
                            }
                            sh.post(Ev::Wake(Some(rest)));
                        }
                    }
                    Phase::Command | Phase::FollowUp | Phase::Confirm => {
                        // Müzik ya da başkası: yok sayılır, dinleme sürer.
                        if !is_me(&cfg, &seg, "komut") {
                            continue;
                        }
                        match stt(&cfg, &seg) {
                            Ok(t) => sh.post(Ev::Heard(phase, t)),
                            Err(e) => sh.post(Ev::Error(e)),
                        }
                    }
                    Phase::Enroll => {
                        let ok = secs >= 1.0 && crate::xtts::enroll(&seg).is_ok();
                        sh.post(Ev::Enrolled(ok));
                    }
                    Phase::Train => {
                        let ok = spotter.add(&seg);
                        if ok && crate::xtts::status() == crate::xtts::Status::Ready {
                            let _ = crate::xtts::enroll(&seg);
                        }
                        if ok && spotter.count() >= 3 {
                            let _ = spotter.save(&spotter_dir());
                        }
                        sh.post(Ev::Trained(spotter.count(), ok));
                    }
                    _ => {}
                }
            }
        }
    }
}

// --- Konuşmacı ---

fn speaker_loop(sh: Arc<Shared>, rx: mpsc::Receiver<Say>) {
    while let Ok(item) = rx.recv() {
        let epoch = match &item {
            Say::Text(_, e) | Say::Phrase(_, _, e) | Say::End(e) => *e,
        };
        // Sıraya girdikten sonra susturulduysa atılır.
        if epoch != sh.epoch.load(Ordering::Relaxed) {
            continue;
        }
        let sc = sh.speak.lock().unwrap().clone();
        match item {
            Say::End(g) => {
                sh.player.wait();
                if sh.epoch.load(Ordering::Relaxed) == g {
                    sh.post(Ev::SpeakDone(g));
                }
            }
            Say::Phrase(text, path, _) => {
                if sc.silent() || text.is_empty() {
                    if text.is_empty() {
                        sh.player.play(&chime(), audio::RATE);
                    }
                    continue;
                }
                if let Some((pcm, rate)) = std::fs::read(&path).ok().and_then(|b| audio::decode_wav(&b)) {
                    sh.player.play(&pcm, rate);
                    continue;
                }
                match sc.synth(&text) {
                    Ok((pcm, rate)) => {
                        let _ = std::fs::create_dir_all(cache_dir());
                        let _ = std::fs::write(&path, audio::wav(&pcm, rate));
                        if sh.epoch.load(Ordering::Relaxed) == epoch {
                            sh.player.play(&pcm, rate);
                        }
                    }
                    Err(e) => crate::log!("seslendirilemedi: {e}"),
                }
            }
            Say::Text(text, _) => {
                if sh.epoch.load(Ordering::Relaxed) != epoch || sc.silent() {
                    continue;
                }
                if sc.engine == Engine::Local {
                    crate::xtts::wait_ready(Duration::from_secs(45));
                    let r = crate::xtts::speak(&text, &sc.local_voice, sc.local_speed, &mut |pcm| {
                        if sh.epoch.load(Ordering::Relaxed) != epoch {
                            return false;
                        }
                        sh.player.play(pcm, crate::xtts::RATE);
                        true
                    });
                    if let Err(e) = r {
                        crate::log!("yerel ses: {e}");
                        sh.post(Ev::Error(e.to_string()));
                    }
                    continue;
                }
                if sc.engine == Engine::Gemini {
                    match crate::gemini::tts(&sc.gemini_key, &sc.gemini_voice, &text) {
                        Ok((pcm, rate)) if sh.epoch.load(Ordering::Relaxed) == epoch => sh.player.play(&pcm, rate),
                        Ok(_) => {}
                        Err(e) => {
                            crate::log!("gemini sesi: {e}");
                            sh.post(Ev::Error(e.to_string()));
                        }
                    }
                    continue;
                }
                if sc.engine == Engine::Windows {
                    match audio::windows_tts(&text, "tr-TR") {
                        Ok((pcm, rate)) if sh.epoch.load(Ordering::Relaxed) == epoch => sh.player.play(&pcm, rate),
                        Ok(_) => {}
                        Err(e) => crate::log!("Windows sesi: {e}"),
                    }
                    continue;
                }
                let r = crate::eleven::tts_stream(&sc.eleven_key, &sc.voice, &sc.model, &text, &mut |pcm| {
                    if sh.epoch.load(Ordering::Relaxed) != epoch {
                        return false;
                    }
                    sh.player.play(pcm, crate::eleven::TTS_RATE);
                    true
                });
                if let Err(e) = r {
                    crate::log!("elevenlabs: {e}");
                    sh.post(Ev::Error(e.to_string()));
                }
            }
        }
    }
}

/// Sessiz kipte uyanma çanı: iki yumuşak ton (yukarı doğru), 16 kHz.
fn chime() -> Vec<i16> {
    let rate = audio::RATE as f32;
    let mut out = Vec::new();
    for (freq, dur) in [(880.0f32, 0.09f32), (1318.5, 0.16)] {
        let n = (dur * rate) as usize;
        for i in 0..n {
            let t = i as f32 / rate;
            let env = (t / 0.008).min(1.0) * (-(t / dur) * 4.0).exp();
            let v = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.7 + (4.0 * std::f32::consts::PI * freq * t).sin() * 0.12;
            out.push((v * env * 0.18 * i16::MAX as f32) as i16);
        }
    }
    out
}
