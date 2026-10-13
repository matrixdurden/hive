//! Ayarlar, bölüm bölüm: Ses (ElevenLabs, konuşan ses, uyandırma, sesimi öğret, bas-konuş),
//! Bilgisayar (kontrol, onay), Beyin (Anthropic anahtarı, model, düşünme, not klasörü, hitap) ve
//! Uygulama (kullanım, dil, tema, oturumda başlama, çıkış, kaldırma). API anahtarları DPAPI ile
//! saklanır, maskeli görünür.

use windows::Win32::Foundation::HWND;

use crate::app::Shared;
use crate::gfx::{Gfx, Rect};
use crate::pages::VoiceReq;
use crate::ui::*;

const ROW: f32 = 64.0;
const SECTION: f32 = 52.0;
const MAX_W: f32 = 760.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Field(usize),
    Seg(usize, usize),
    Toggle(usize),
    Button(Act),
}

/// Düğmelerin işi.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Act {
    Spotify,
    LocalVoice,
    GeminiVoice,
    Mic,
    PickVoice,
    Train,
    VoiceNote,
    Browse,
    Quit,
    Remove,
}

impl Act {
    fn label(self) -> &'static str {
        match self {
            Act::LocalVoice | Act::GeminiVoice => t!("Choose", "Seç"),
            Act::Spotify => {
                if crate::spotify::connected() {
                    t!("Disconnect", "Çıkış")
                } else {
                    t!("Connect", "Bağlan")
                }
            }
            Act::Mic => t!("Change", "Değiştir"),
            Act::PickVoice => t!("Choose", "Seç"),
            Act::Train => t!("Start", "Başla"),
            Act::VoiceNote => t!("Open note", "Notu aç"),
            Act::Browse => t!("Browse…", "Gözat…"),
            Act::Quit => t!("Quit", "Çık"),
            Act::Remove => t!("Remove", "Kaldır"),
        }
    }
}

/// Yazı alanları sırası.
const F_ANTHROPIC: usize = 0;
const F_ELEVEN: usize = 1;
const F_VAULT: usize = 2;
const F_NAME: usize = 3;
const F_HOTKEY: usize = 4;
const F_BUDGET: usize = 5;
const F_VOICE: usize = 6;
const F_GROQ: usize = 7;
const F_GEMINI: usize = 8;
const F_SPOTIFY: usize = 9;

/// Seçenek satırları.
const S_MODEL: usize = 0;
const S_EFFORT: usize = 1;
const S_LANG: usize = 2;
const S_THEME: usize = 3;
const S_TTS: usize = 4;
const S_CONFIRM: usize = 5;

/// Aç/kapa satırları.
const T_WAKE: usize = 0;
const T_CONTROL: usize = 1;
const T_AUTOSTART: usize = 3;
const T_LOCK: usize = 4;

const MODELS: [(&str, &str); 2] = [("Opus 5.5", "claude-opus-5-5"), ("Sonnet 5.5", "claude-sonnet-5-5")];
const EFFORTS: [&str; 3] = ["low", "medium", "high"];
const TTS: [&str; 5] = ["xtts", "gemini", "auto", "windows", "kapali"];
const CONFIRMS: [crate::config::Confirm; 3] = [crate::config::Confirm::Always, crate::config::Confirm::Risky, crate::config::Confirm::Never];

/// Satırın sağındaki kontrol.
#[derive(Clone, Copy)]
enum Ctl {
    /// Bölüm başlığı.
    Section,
    Field(usize),
    /// Yazı alanı ve yanında düğme.
    FieldButton(usize, Act),
    Seg(usize),
    Toggle(usize),
    Button(Act),
}

pub struct SettingsPage {
    shared: Shared,
    fields: Vec<TextField>,
    autostart: bool,
    scroll: f32,
    hover: Hit,
    pressed: Hit,
    w: f32,
    h: f32,
}

impl SettingsPage {
    pub fn new(shared: Shared, _hwnd: HWND) -> Self {
        let c = shared.borrow().config.clone();
        let mut fields = vec![
            TextField::new(&c.anthropic_key, "sk-ant-…"),
            TextField::new(&c.elevenlabs_key, "sk_…"),
            TextField::new(&c.vault.display().to_string(), ""),
            TextField::new(&c.name, "Matrix"),
            TextField::new(&c.hotkey, "F8"),
            TextField::new(&format!("{:.0}", c.budget), "200"),
            TextField::new(&c.voice, t!("default voice", "varsayılan ses")),
            TextField::new(&c.groq_key, "gsk_…"),
            TextField::new(&c.gemini_key, "AIza…"),
            TextField::new(&crate::spotify::client_id(), "Client ID"),
        ];
        fields[F_GROQ].mask = true;
        fields[F_GEMINI].mask = true;
        fields[F_ANTHROPIC].mask = true;
        fields[F_ELEVEN].mask = true;
        let autostart = crate::shell::autostart();
        SettingsPage { shared, fields, autostart, scroll: 0.0, hover: Hit::None, pressed: Hit::None, w: 0.0, h: 0.0 }
    }

    /// Satırlar sırayla: (başlık, açıklama, kontrol).
    /// winloom modunda oturumda başlama ve kaldırma winloom'ın: o satırlar yok.
    fn rows(&self) -> Vec<(&'static str, String, Ctl)> {
        let mut rows = self.all_rows();
        if crate::util::hub() {
            rows.retain(|(_, _, ctl)| !matches!(ctl, Ctl::Toggle(T_AUTOSTART) | Ctl::Button(Act::Remove)));
        }
        rows
    }

    fn all_rows(&self) -> Vec<(&'static str, String, Ctl)> {
        let s = self.shared.borrow();
        let c = &s.config;
        let dev = crate::util::installed_copy().is_none();
        let trained = if s.trained >= 3 {
            t!(format!("Learned from {} samples: recognised on this computer", s.trained), format!("{} örnekten öğrendi: bu bilgisayarda tanınır", s.trained))
        } else if !s.train_msg.is_empty() && s.voice_state == crate::voice::State::Training {
            t!(format!("Say it now: {}", s.train_msg), format!("Şimdi söyle: {}", s.train_msg))
        } else if c.groq_key.is_empty() && c.gemini_key.is_empty() && c.elevenlabs_key.is_empty() {
            t!("~1 min: say the wake phrase 3 times, then read 8 sentences", "~1 dk: uyandırma sözünü 3 kez söyle, sonra 8 cümle oku").into()
        } else {
            t!("~1 min: say the wake phrase 3 times, then read 8 sentences", "~1 dk: uyandırma sözünü 3 kez söyle, sonra 8 cümle oku").into()
        };
        vec![
            (t!("Voice", "Ses"), String::new(), Ctl::Section),
            (
                t!("Groq API key", "Groq API anahtarı"),
                t!("To hear you: free at console.groq.com (Whisper)", "Seni duymak için: console.groq.com'dan ücretsiz (Whisper)").into(),
                Ctl::Field(F_GROQ),
            ),
            (
                t!("Gemini API key", "Gemini API anahtarı"),
                t!("Hearing if there is no Groq key, and its voice; from aistudio.google.com", "Groq yoksa duyma, ister konuşan ses; aistudio.google.com'dan").into(),
                Ctl::Field(F_GEMINI),
            ),
            (
                t!("ElevenLabs API key", "ElevenLabs API anahtarı"),
                t!("Its voice; optional", "Konuşan sesi; isteğe bağlı").into(),
                Ctl::Field(F_ELEVEN),
            ),
            (
                t!("Speaking voice", "Konuşan ses"),
                t!("Local runs free on your graphics card; Windows' own voice is robotic", "Yerel: ekran kartında bedava; Windows'un kendi sesi robotiktir").into(),
                Ctl::Seg(S_TTS),
            ),
            (
                t!("Local voice", "Yerel ses"),
                format!("{} · {}", c.local_voice, match crate::xtts::status() {
                    crate::xtts::Status::Ready => t!("ready", "hazır"),
                    crate::xtts::Status::Loading => t!("loading…", "yükleniyor…"),
                    crate::xtts::Status::Failed => t!("not running (make engram-ses)", "çalışmıyor (make engram-ses)"),
                    crate::xtts::Status::Off => t!("off", "kapalı"),
                }),
                Ctl::Button(Act::LocalVoice),
            ),
            (t!("Gemini voice", "Gemini sesi"), c.gemini_voice.clone(), Ctl::Button(Act::GeminiVoice)),
            (t!("ElevenLabs voice ID", "ElevenLabs ses kimliği"), t!("From elevenlabs.io › Voices; empty for the default", "elevenlabs.io › Voices'tan; boşsa varsayılan").into(), Ctl::FieldButton(F_VOICE, Act::PickVoice)),
            (
                t!("Wake by voice", "Sesle uyan"),
                t!(format!("Say \"{}\"; the microphone stays on", s.wake_phrase), format!("\"{}\" de; mikrofon açık kalır", s.wake_phrase)),
                Ctl::Toggle(T_WAKE),
            ),
            (
                t!("Microphone", "Mikrofon"),
                if c.mic.is_empty() {
                    t!("Windows default. A Bluetooth headset drops to call quality while listening; prefer the laptop mic", "Windows'un varsayılanı. Bluetooth kulaklık dinlerken görüşme kalitesine düşer; dizüstünün mikrofonu daha iyi").into()
                } else {
                    crate::audio::mics().into_iter().find(|m| m.0 == c.mic).map_or_else(|| t!("Selected microphone is not connected", "Seçili mikrofon takılı değil").into(), |m| m.1)
                },
                Ctl::Button(Act::Mic),
            ),
            (
                t!("Only my voice", "Yalnızca benim sesim"),
                t!("Music and other people don't trigger it; learned from \"Teach my voice\"", "Müzik ve başkaları tetiklemez; \"Sesimi tanıt\"tan öğrenir").into(),
                Ctl::Toggle(T_LOCK),
            ),
            (t!("Wake phrase and replies", "Uyandırma sözü ve cevaplar"), "sistem/ses.md".into(), Ctl::Button(Act::VoiceNote)),
            (t!("Teach my voice", "Sesimi tanıt"), trained, Ctl::Button(Act::Train)),
            (t!("Push-to-talk key", "Bas-konuş tuşu"), t!("Hold it anywhere and talk", "Her yerde basılı tutup konuş").into(), Ctl::Field(F_HOTKEY)),
            (
                "Spotify",
                if crate::spotify::connected() {
                    t!("Connected: \"give me some music\" picks from your playlists", "Bağlı: \"bana müzik ver\" listelerinden seçer").into()
                } else {
                    t!("Client ID from developer.spotify.com (redirect http://127.0.0.1:8737/spotify)", "developer.spotify.com'daki Client ID (yönlendirme http://127.0.0.1:8737/spotify)").into()
                },
                Ctl::FieldButton(F_SPOTIFY, Act::Spotify),
            ),
            (t!("Computer", "Bilgisayar"), String::new(), Ctl::Section),
            (
                t!("Control the computer", "Bilgisayarı kontrol et"),
                t!("Mouse and keyboard; when off it can only look at the screen", "Fare ve klavye; kapalıyken yalnızca ekrana bakar").into(),
                Ctl::Toggle(T_CONTROL),
            ),
            (
                t!("Ask before", "Onay"),
                t!("Risky only: deleting, installing, shutting down, pushing…", "Yalnızca riskli: silme, kurma, kapatma, push…").into(),
                Ctl::Seg(S_CONFIRM),
            ),
            (t!("Brain", "Beyin"), String::new(), Ctl::Section),
            (
                t!("Anthropic API key", "Anthropic API anahtarı"),
                t!("From platform.claude.com; stored encrypted for this user", "platform.claude.com'dan; bu kullanıcıya şifreli saklanır").into(),
                Ctl::Field(F_ANTHROPIC),
            ),
            (t!("Model", "Model"), t!("Opus is the smartest; Sonnet is faster and cheaper", "Opus en zekisi; Sonnet daha hızlı ve ucuz").into(), Ctl::Seg(S_MODEL)),
            (t!("Thinking", "Düşünme"), t!("How long it thinks in written chat", "Yazılı sohbette ne kadar düşünsün").into(), Ctl::Seg(S_EFFORT)),
            (
                t!("Brain folder", "Beyin klasörü"),
                t!("Plain markdown notes; Obsidian can open the same folder", "Düz markdown notlar; Obsidian aynı klasörü açabilir").into(),
                Ctl::FieldButton(F_VAULT, Act::Browse),
            ),
            (
                t!("Name", "Hitap adı"),
                t!("Personality and rules are notes too: sistem/kisilik.md, sistem/kurallar.md", "Kişilik ve kurallar da not: sistem/kisilik.md, sistem/kurallar.md").into(),
                Ctl::Field(F_NAME),
            ),
            (t!("App", "Uygulama"), String::new(), Ctl::Section),
            (
                t!("API usage", "API kullanımı"),
                format!("{} · {}", crate::usage::summary(c.budget), crate::usage::tokens()),
                Ctl::Field(F_BUDGET),
            ),
            (t!("Language", "Dil"), String::new(), Ctl::Seg(S_LANG)),
            (t!("Theme", "Tema"), String::new(), Ctl::Seg(S_THEME)),
            (
                t!("Start with Windows", "Windows ile başlat"),
                if dev { t!("Not available in a development copy", "Geliştirme kopyasında kullanılamaz").into() } else { t!("Starts quietly in the tray when you sign in", "Oturum açınca tepside sessizce başlar").into() },
                Ctl::Toggle(T_AUTOSTART),
            ),
            (t!("Quit", "Kapat"), t!("engram shuts down completely", "engram tamamen kapanır").into(), Ctl::Button(Act::Quit)),
            (t!("Remove engram", "engram'ı kaldır"), t!("Settings and keys are deleted; your notes stay", "Ayarlar ve anahtarlar silinir; notların kalır").into(), Ctl::Button(Act::Remove)),
        ]
    }

    fn col(&self) -> (f32, f32) {
        let w = (self.w - 2.0 * PAD).min(MAX_W);
        let l = (self.w - w) / 2.0;
        (l, w)
    }

    /// Satırların dikey yerleri.
    fn row_rects(&self) -> Vec<Rect> {
        let (l, w) = self.col();
        let mut t = HEADER + 4.0 - self.scroll;
        self.rows()
            .iter()
            .map(|(_, _, ctl)| {
                let h = if matches!(ctl, Ctl::Section) { SECTION } else { ROW };
                let r = Rect::new(l, t, l + w, t + h);
                t += h;
                r
            })
            .collect()
    }

    fn max_scroll(&self) -> f32 {
        let content: f32 = self.rows().iter().map(|r| if matches!(r.2, Ctl::Section) { SECTION } else { ROW }).sum();
        (HEADER + 4.0 + content + 24.0 - self.h).max(0.0)
    }

    fn seg_labels(&self, s: usize) -> Vec<String> {
        match s {
            S_MODEL => MODELS.iter().map(|m| m.0.to_string()).collect(),
            S_EFFORT => vec![t!("low", "az").into(), t!("medium", "orta").into(), t!("high", "çok").into()],
            S_LANG => vec!["English".into(), "Türkçe".into()],
            S_TTS => vec![t!("Local", "Yerel").into(), "Gemini".into(), "ElevenLabs".into(), "Windows".into(), t!("Off", "Kapalı").into()],
            S_CONFIRM => vec![t!("Always", "Her zaman").into(), t!("Risky only", "Yalnızca riskli").into(), t!("Never", "Hiç").into()],
            _ => vec!["Claude".into(), "Windows".into()],
        }
    }

    fn seg_selected(&self, s: usize) -> usize {
        let c = &self.shared.borrow().config;
        match s {
            S_MODEL => MODELS.iter().position(|m| m.1 == c.model).unwrap_or(0),
            S_EFFORT => EFFORTS.iter().position(|e| *e == c.effort).unwrap_or(1),
            S_LANG => crate::i18n::turkish() as usize,
            S_TTS => TTS.iter().position(|t| *t == c.tts).unwrap_or(0),
            S_CONFIRM => CONFIRMS.iter().position(|x| *x == c.confirm).unwrap_or(1),
            _ => (theme() == Theme::Windows) as usize,
        }
    }

    fn toggled(&self, t: usize) -> bool {
        let c = &self.shared.borrow().config;
        match t {
            T_WAKE => c.wake,
            T_CONTROL => c.control,
            T_LOCK => c.voice_lock,

            _ => self.autostart,
        }
    }

    fn toggle_enabled(&self, t: usize) -> bool {
        t != T_AUTOSTART || crate::util::installed_copy().is_some()
    }

    fn segs(&self, g: &Gfx, row: Rect, labels: &[String]) -> Vec<Rect> {
        let mut right = row.r - 4.0;
        let mut v: Vec<Rect> = labels
            .iter()
            .rev()
            .map(|l| {
                let w = g.measure(l, &g.f.button) + 26.0;
                let r = Rect::new(right - w, row.cy() - 14.0, right, row.cy() + 14.0);
                right -= w;
                r
            })
            .collect();
        v.reverse();
        v
    }

    fn field_rect(&self, f: usize, row: Rect, button_w: f32) -> Rect {
        let h = 34.0;
        if f == F_BUDGET || f == F_HOTKEY {
            return Rect::new(row.r - 120.0, row.cy() - h / 2.0, row.r - 4.0, row.cy() + h / 2.0);
        }
        let w = (row.w() * 0.46).max(200.0);
        let r = row.r - 4.0 - button_w;
        Rect::new(r - w, row.cy() - h / 2.0, r, row.cy() + h / 2.0)
    }

    fn button_rect(&self, g: &Gfx, a: Act, row: Rect) -> Rect {
        button_rect_right(g, row.r - 4.0, row.cy() - 17.0, a.label(), false)
    }

    /// Satırın kontrollerinin kapladığı genişlik (başlık yazısı bunun solunda biter).
    fn control_w(&self, g: &Gfx, ctl: Ctl, r: Rect) -> f32 {
        match ctl {
            Ctl::Section => 0.0,
            Ctl::Field(f) => self.field_rect(f, r, 0.0).w() + 20.0,
            Ctl::FieldButton(f, a) => {
                let bw = self.button_rect(g, a, r).w() + 8.0;
                self.field_rect(f, r, bw).w() + bw + 20.0
            }
            Ctl::Seg(s) => {
                let labels = self.seg_labels(s);
                r.r - self.segs(g, r, &labels)[0].l + 24.0
            }
            Ctl::Toggle(_) => 70.0,
            Ctl::Button(a) => self.button_rect(g, a, r).w() + 24.0,
        }
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return Hit::None;
        }
        let rows = self.rows();
        for (r, (_, _, ctl)) in self.row_rects().into_iter().zip(rows) {
            if !r.contains(x, y) {
                continue;
            }
            return match ctl {
                Ctl::Section => Hit::None,
                Ctl::Field(f) => {
                    if self.field_rect(f, r, 0.0).contains(x, y) { Hit::Field(f) } else { Hit::None }
                }
                Ctl::FieldButton(f, a) => {
                    let b = self.button_rect(g, a, r);
                    if b.contains(x, y) {
                        Hit::Button(a)
                    } else if self.field_rect(f, r, b.w() + 8.0).contains(x, y) {
                        Hit::Field(f)
                    } else {
                        Hit::None
                    }
                }
                Ctl::Seg(s) => {
                    let labels = self.seg_labels(s);
                    self.segs(g, r, &labels).iter().position(|sr| sr.contains(x, y)).map_or(Hit::None, |k| Hit::Seg(s, k))
                }
                Ctl::Toggle(t) => {
                    if self.toggle_enabled(t) && toggle_rect(r.r - 4.0, r.cy()).contains(x, y) { Hit::Toggle(t) } else { Hit::None }
                }
                Ctl::Button(a) => {
                    if self.button_rect(g, a, r).contains(x, y) { Hit::Button(a) } else { Hit::None }
                }
            };
        }
        Hit::None
    }

    /// Alanın yeri (fare olayları için).
    fn field_at(&self, g: &Gfx, f: usize) -> Option<Rect> {
        let rows = self.rows();
        self.row_rects().into_iter().zip(rows).find_map(|(r, (_, _, ctl))| match ctl {
            Ctl::Field(x) if x == f => Some(self.field_rect(f, r, 0.0)),
            Ctl::FieldButton(x, a) if x == f => Some(self.field_rect(f, r, self.button_rect(g, a, r).w() + 8.0)),
            _ => None,
        })
    }

    /// Alandaki yazıyı ayarlara işler ve kaydeder.
    fn commit(&mut self, f: usize) {
        let text = self.fields[f].text.trim().to_string();
        let mut voice = false;
        {
            let mut s = self.shared.borrow_mut();
            match f {
                F_ANTHROPIC => s.config.anthropic_key = text,
                F_GROQ => {
                    voice = s.config.groq_key != text;
                    s.config.groq_key = text;
                }
                F_GEMINI => {
                    voice = s.config.gemini_key != text;
                    s.config.gemini_key = text;
                }
                F_ELEVEN => {
                    voice = s.config.elevenlabs_key != text;
                    s.config.elevenlabs_key = text;
                }
                F_VOICE => {
                    voice = s.config.voice != text;
                    s.config.voice = text;
                }
                F_VAULT => {
                    drop(s);
                    self.set_vault(&text);
                    return;
                }
                F_NAME => {
                    if !text.is_empty() && s.config.name != text {
                        s.config.name = text;
                        voice = true;
                    }
                }
                F_HOTKEY => {
                    if crate::hotkey::Hotkey::parse(&text).is_some() {
                        voice = s.config.hotkey != text;
                        s.config.hotkey = text;
                    }
                }
                F_BUDGET => {
                    if let Ok(b) = text.replace(',', ".").parse::<f64>() {
                        s.config.budget = b.max(0.0);
                    }
                }
                _ => {}
            }
            s.config.save();
            if voice {
                s.voice_req = Some(VoiceReq::Apply);
            }
        }
    }

    /// Beyin klasörünü değiştirir. Yeni klasör boşsa ve eskisinde not varsa notlar kopyalanır
    /// (eskiler yerinde kalır).
    fn set_vault(&mut self, text: &str) {
        let path = std::path::PathBuf::from(text.trim());
        let old = self.shared.borrow().config.vault.clone();
        if text.trim().is_empty() || path == old {
            self.fields[F_VAULT].set_text(&old.display().to_string());
            return;
        }
        let empty = std::fs::read_dir(&path).map_or(true, |mut d| d.next().is_none());
        let has_notes = !self.shared.borrow().vault.notes.is_empty();
        if empty && has_notes {
            let q = t!(
                format!("Copy your notes to the new folder?\n\n{}\n\nThe originals stay where they are.", path.display()),
                format!("Notların yeni klasöre kopyalansın mı?\n\n{}\n\nAsılları yerinde kalır.", path.display())
            );
            if crate::util::ask(&q)
                && let Err(e) = copy_dir(&old, &path)
            {
                crate::util::error_box(&format!("{}\n\n{e}", t!("Could not copy the notes:", "Notlar kopyalanamadı:")));
            }
        }
        let _ = std::fs::create_dir_all(&path);
        let mut s = self.shared.borrow_mut();
        s.config.vault = path.clone();
        s.vault = crate::notes::Vault::open(&path);
        s.config.save();
        s.voice_req = Some(VoiceReq::Apply);
        drop(s);
        self.fields[F_VAULT].set_text(&path.display().to_string());
    }

    fn focus(&mut self, f: Option<usize>) {
        for (i, fl) in self.fields.iter_mut().enumerate() {
            fl.focused = Some(i) == f;
        }
    }

    fn blur_all(&mut self) {
        let focused: Vec<usize> = self.fields.iter().enumerate().filter(|(_, f)| f.focused).map(|(i, _)| i).collect();
        for f in focused {
            self.commit(f);
        }
        self.focus(None);
    }

    fn choose(&mut self, seg: usize, k: usize) {
        let mut st = self.shared.borrow_mut();
        match seg {
            S_MODEL => st.config.model = MODELS[k.min(1)].1.into(),
            S_EFFORT => st.config.effort = EFFORTS[k.min(2)].into(),
            S_LANG => {
                crate::i18n::set_turkish(k == 1);
                st.config.turkish = k == 1;
                crate::shell::sync_shortcut();
            }
            S_TTS => {
                st.config.tts = TTS[k.min(4)].into();
                st.voice_req = Some(VoiceReq::Apply);
            }
            S_CONFIRM => st.config.confirm = CONFIRMS[k.min(2)],
            _ => st.set_theme = Some(if k == 1 { Theme::Windows } else { Theme::Claude }),
        }
        st.config.save();
    }

    fn flip(&mut self, t: usize) {
        if t == T_AUTOSTART {
            if let Some(exe) = crate::util::installed_copy() {
                crate::shell::set_autostart((!self.autostart).then_some(exe.as_str()));
                self.autostart = crate::shell::autostart();
            }
            return;
        }
        let mut s = self.shared.borrow_mut();
        match t {
            T_WAKE => {
                s.config.wake = !s.config.wake;
                s.voice_req = Some(VoiceReq::Apply);
            }
            T_LOCK => {
                s.config.voice_lock = !s.config.voice_lock;
                s.voice_req = Some(VoiceReq::Apply);
            }
            _ => s.config.control = !s.config.control,
        }
        s.config.save();
    }

    fn act(&mut self, a: Act) {
        match a {
            Act::Spotify => {
                if crate::spotify::connected() {
                    crate::spotify::logout();
                    return;
                }
                let id = self.fields[F_SPOTIFY].text.trim().to_string();
                if id.is_empty() {
                    crate::util::error_box(t!("Paste the Client ID first.", "Önce Client ID'yi yapıştır."));
                    return;
                }
                let hwnd = self.shared.borrow().hwnd.0 as usize;
                // Giriş tarayıcıda olur ve dakikalar sürebilir: arayüzü bekletmez.
                std::thread::spawn(move || {
                    match crate::spotify::login(&id) {
                        Ok(_) => crate::log!("spotify bağlandı"),
                        Err(e) => crate::util::error_box(&format!("Spotify: {e}")),
                    }
                    crate::app::redraw(windows::Win32::Foundation::HWND(hwnd as *mut _));
                });
            }
            Act::GeminiVoice => {
                let (hwnd, cur) = {
                    let s = self.shared.borrow();
                    (s.hwnd, s.config.gemini_voice.clone())
                };
                let list: Vec<String> = crate::gemini::VOICES.iter().map(|v| v.to_string()).collect();
                if let Some(k) = crate::util::popup_menu(hwnd, &list, list.iter().position(|v| *v == cur)) {
                    let mut s = self.shared.borrow_mut();
                    s.config.gemini_voice = list[k].clone();
                    s.config.save();
                    s.voice_req = Some(VoiceReq::Apply);
                }
            }
            Act::LocalVoice => {
                let (hwnd, cur) = {
                    let s = self.shared.borrow();
                    (s.hwnd, s.config.local_voice.clone())
                };
                match crate::xtts::speakers() {
                    Ok(list) if !list.is_empty() => {
                        let checked = list.iter().position(|v| *v == cur);
                        if let Some(k) = crate::util::popup_menu(hwnd, &list, checked) {
                            let mut s = self.shared.borrow_mut();
                            s.config.local_voice = list[k].clone();
                            s.config.save();
                            s.voice_req = Some(VoiceReq::Apply);
                        }
                    }
                    _ => crate::util::error_box(t!(
                        "The local voice is not ready yet. Choose \"Local\" above and wait about 40 seconds.",
                        "Yerel ses henüz hazır değil. Yukarıda \"Yerel\"i seç ve 40 saniye kadar bekle."
                    )),
                }
            }
            Act::Mic => {
                let (hwnd, cur) = {
                    let s = self.shared.borrow();
                    (s.hwnd, s.config.mic.clone())
                };
                let mics = crate::audio::mics();
                let mut items = vec![t!("Windows default", "Windows'un varsayılanı").to_string()];
                items.extend(mics.iter().map(|m| m.1.clone()));
                let checked = if cur.is_empty() { Some(0) } else { mics.iter().position(|m| m.0 == cur).map(|i| i + 1) };
                if let Some(k) = crate::util::popup_menu(hwnd, &items, checked) {
                    let mut s = self.shared.borrow_mut();
                    s.config.mic = if k == 0 { String::new() } else { mics[k - 1].0.clone() };
                    s.config.save();
                    s.voice_req = Some(VoiceReq::Apply);
                }
            }
            Act::PickVoice => {
                self.blur_all();
                let (hwnd, key, cur) = {
                    let s = self.shared.borrow();
                    (s.hwnd, s.config.elevenlabs_key.clone(), s.config.voice.clone())
                };
                if key.is_empty() {
                    crate::util::error_box(t!("Enter the ElevenLabs API key first.", "Önce ElevenLabs API anahtarını gir."));
                    return;
                }
                match crate::eleven::voices(&key) {
                    Ok(voices) if !voices.is_empty() => {
                        let items: Vec<String> = voices.iter().map(|v| v.1.clone()).collect();
                        let checked = voices.iter().position(|v| v.0 == cur);
                        if let Some(k) = crate::util::popup_menu(hwnd, &items, checked) {
                            let id = voices[k].0.clone();
                            self.fields[F_VOICE].set_text(&id);
                            let mut s = self.shared.borrow_mut();
                            s.config.voice = id;
                            s.config.save();
                            s.voice_req = Some(VoiceReq::Apply);
                        }
                    }
                    Ok(_) => crate::util::error_box(t!("No voices in this ElevenLabs account.", "Bu ElevenLabs hesabında ses yok.")),
                    Err(e) => crate::util::error_box(&e.to_string()),
                }
            }
            Act::Train => self.shared.borrow_mut().voice_req = Some(VoiceReq::Train),
            Act::VoiceNote => {
                let mut s = self.shared.borrow_mut();
                s.open_note = Some(format!("{}/{}.md", crate::notes::SYSTEM_DIR, crate::voice::NOTE));
                s.open_tab = Some(crate::app::Tab::Memory);
            }
            Act::Browse => {
                let (hwnd, cur) = {
                    let s = self.shared.borrow();
                    (s.hwnd, s.config.vault.clone())
                };
                if let Some(p) = crate::util::pick_folder(hwnd, &cur) {
                    self.set_vault(&p.display().to_string());
                }
            }
            Act::Quit => self.shared.borrow_mut().quit = true,
            Act::Remove => self.shared.borrow_mut().remove_requested = true,
        }
    }

    fn redraw(&self) {
        crate::app::redraw(self.shared.borrow().hwnd);
    }
}

/// Klasörü içeriğiyle kopyalar (gizli klasörler hariç: .obsidian dahil edilir, .git değil).
fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let name = e.file_name();
        if name == ".git" {
            continue;
        }
        let (src, dst) = (e.path(), to.join(&name));
        if e.file_type()?.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

impl Page for SettingsPage {
    fn layout(&mut self, w: f32, h: f32, _head_x: f32, _head_r: f32) {
        self.w = w;
        self.h = h;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn paint(&self, g: &Gfx) {
        let rows = self.rows();
        g.clip(Rect::new(0.0, HEADER, self.w, self.h), || {
            for (r, (title, sub, ctl)) in self.row_rects().into_iter().zip(rows) {
                if r.b < HEADER || r.t > self.h {
                    continue;
                }
                if let Ctl::Section = ctl {
                    g.text(title, &g.f.heading, Rect::new(r.l, r.t + 14.0, r.r, r.b), pal().text);
                    continue;
                }
                setting_row(g, r, title, &sub, self.control_w(g, ctl, r));
                match ctl {
                    Ctl::Section => {}
                    Ctl::Field(f) => self.fields[f].draw(g, self.field_rect(f, r, 0.0), self.hover == Hit::Field(f)),
                    Ctl::FieldButton(f, a) => {
                        let b = self.button_rect(g, a, r);
                        self.fields[f].draw(g, self.field_rect(f, r, b.w() + 8.0), self.hover == Hit::Field(f));
                        button(g, b, a.label(), None, None, self.hover == Hit::Button(a));
                    }
                    Ctl::Seg(s) => {
                        let labels = self.seg_labels(s);
                        let segs = self.segs(g, r, &labels);
                        let sel = self.seg_selected(s);
                        let (first, last) = (segs[0], segs[segs.len() - 1]);
                        g.fill(Rect::new(first.l - 3.0, first.t - 3.0, last.r + 3.0, last.b + 3.0), 8.0, pal().hover);
                        for (k, (sr, l)) in segs.iter().zip(&labels).enumerate() {
                            let on = k == sel;
                            let hov = self.hover == Hit::Seg(s, k);
                            if on {
                                g.fill(*sr, 6.0, pal().sel);
                            } else if hov {
                                g.fill(*sr, 6.0, pal().sel.alpha(0.6));
                            }
                            g.text(l, &g.f.button, *sr, if on || hov { pal().text } else { pal().muted });
                        }
                    }
                    Ctl::Toggle(t) => toggle(g, toggle_rect(r.r - 4.0, r.cy()), self.toggled(t), accent(), self.toggle_enabled(t)),
                    Ctl::Button(a) => {
                        let primary = a == Act::Train && self.shared.borrow().voice_state == crate::voice::State::Training;
                        button(g, self.button_rect(g, a, r), a.label(), None, primary.then(accent), self.hover == Hit::Button(a));
                    }
                }
            }
        });
    }

    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        if let Some(f) = self.fields.iter().position(|f| f.dragging())
            && let Some(r) = self.field_at(g, f)
        {
            self.fields[f].mouse_move(g, r, x, y);
            self.redraw();
        }
        let hit = self.hit(g, x, y);
        if hit != self.hover {
            self.hover = hit;
            self.redraw();
        }
    }

    fn mouse_leave(&mut self) {
        if self.hover != Hit::None {
            self.hover = Hit::None;
            self.redraw();
        }
    }

    fn mouse_down(&mut self, g: &Gfx, x: f32, y: f32) {
        self.pressed = self.hit(g, x, y);
        match self.pressed {
            Hit::Field(f) => {
                let was: Vec<usize> = self.fields.iter().enumerate().filter(|(i, fl)| fl.focused && *i != f).map(|(i, _)| i).collect();
                for w in was {
                    self.commit(w);
                }
                self.focus(Some(f));
                if let Some(r) = self.field_at(g, f) {
                    self.fields[f].mouse_down(g, r, x, y);
                }
            }
            _ => self.blur_all(),
        }
        self.redraw();
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        self.fields.iter_mut().for_each(|f| f.mouse_up());
        let hit = self.hit(g, x, y);
        if hit != std::mem::replace(&mut self.pressed, Hit::None) {
            return;
        }
        match hit {
            Hit::Seg(s, k) => self.choose(s, k),
            Hit::Toggle(t) => self.flip(t),
            Hit::Button(a) => self.act(a),
            _ => {}
        }
        self.redraw();
    }

    fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        self.scroll = (self.scroll - delta * 60.0).clamp(0.0, self.max_scroll());
        self.hover = self.hit(g, x, y);
        self.redraw();
    }

    fn key(&mut self, vk: u16) -> bool {
        let Some(f) = self.fields.iter().position(|f| f.focused) else { return false };
        if self.fields[f].key(vk) {
            self.commit(f);
            self.focus(None);
        } else if !self.fields[f].focused {
            // Esc ile bırakıldı.
            self.commit(f);
        }
        self.redraw();
        true
    }

    fn char(&mut self, c: char) -> bool {
        let Some(f) = self.fields.iter().position(|f| f.focused) else { return false };
        let used = self.fields[f].char(c);
        if used {
            self.redraw();
        }
        used
    }

    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        y >= HEADER || self.hit(g, x, y) != Hit::None
    }

    fn set_visible(&mut self, visible: bool) {
        if visible {
            self.autostart = crate::shell::autostart();
            // Dışarıdan değişmiş olabilecek alanlar.
            let c = self.shared.borrow().config.clone();
            if !self.fields.iter().any(|f| f.focused) {
                self.fields[F_ANTHROPIC].set_text(&c.anthropic_key);
                self.fields[F_ELEVEN].set_text(&c.elevenlabs_key);
                self.fields[F_VAULT].set_text(&c.vault.display().to_string());
                self.fields[F_NAME].set_text(&c.name);
                self.fields[F_HOTKEY].set_text(&c.hotkey);
                self.fields[F_BUDGET].set_text(&format!("{:.0}", c.budget));
                self.fields[F_VOICE].set_text(&c.voice);
                self.fields[F_GROQ].set_text(&c.groq_key);
                self.fields[F_GEMINI].set_text(&c.gemini_key);
            }
        } else {
            self.blur_all();
            self.hover = Hit::None;
        }
    }

    fn kill_focus(&mut self) {
        self.blur_all();
    }
}
