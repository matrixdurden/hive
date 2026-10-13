//! Ayarlar, bölüm bölüm: Ses (ElevenLabs, konuşan ses, uyandırma, sesimi öğret, bas-konuş),
//! Bilgisayar (kontrol, onay), Beyin (Anthropic anahtarı, model, düşünme, not klasörü, hitap) ve
//! Uygulama (kullanım). API anahtarları DPAPI ile saklanır, maskeli görünür. Düzen sayfa kitinin
//! (`ortak::form`); yazı alanları engram'ın kendi `TextField`'ı, kitin ayırdığı yere çizilir.

use ortak::form::{self, Ctl, Form, Item, Row};
use windows::Win32::Foundation::HWND;

use crate::page::Shared;
use crate::gfx::{Gfx, Rect};
use crate::pages::VoiceReq;
use crate::ui::*;

/// Yazı alanlarının genişliği: uzun (anahtar, klasör; yanında düğme varsa ikisi birlikte) ve kısa.
const FIELD_W: f32 = 360.0;
const SHORT_W: f32 = 120.0;

/// Tıklanan / üstüne gelinen.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    /// Kitin kontrolleri (seçenek, aç/kapa, düğme).
    Form(form::Hit),
    Field(usize),
    /// Yazı alanının yanındaki düğme.
    Act(Act),
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
        }
    }

    /// Düğme satırının işi.
    fn of_row(id: u32) -> Option<Act> {
        Some(match id {
            R_LOCAL => Act::LocalVoice,
            R_GEMINI_VOICE => Act::GeminiVoice,
            R_MIC => Act::Mic,
            R_VOICE_NOTE => Act::VoiceNote,
            R_TRAIN => Act::Train,
            _ => return None,
        })
    }

    /// Yazı alanının yanındaki düğme.
    fn of_field(f: usize) -> Option<Act> {
        Some(match f {
            F_VOICE => Act::PickVoice,
            F_SPOTIFY => Act::Spotify,
            F_VAULT => Act::Browse,
            _ => return None,
        })
    }
}

/// Yazı alanları sırası; alanın satırının kimliği de budur.
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

/// Öteki satırların kimlikleri (alanlardan sonra).
const R_TTS: u32 = 10;
const R_LOCAL: u32 = 11;
const R_GEMINI_VOICE: u32 = 12;
const R_WAKE: u32 = 13;
const R_MIC: u32 = 14;
const R_LOCK: u32 = 15;
const R_VOICE_NOTE: u32 = 16;
const R_TRAIN: u32 = 17;
const R_CONTROL: u32 = 18;
const R_CONFIRM: u32 = 19;
const R_MODEL: u32 = 20;
const R_EFFORT: u32 = 21;

const MODELS: [(&str, &str); 2] = [("Opus 5.5", "claude-opus-5-5"), ("Sonnet 5.5", "claude-sonnet-5-5")];
const EFFORTS: [&str; 3] = ["low", "medium", "high"];
const TTS: [&str; 5] = ["xtts", "gemini", "auto", "windows", "kapali"];
const CONFIRMS: [crate::config::Confirm; 3] = [crate::config::Confirm::Always, crate::config::Confirm::Risky, crate::config::Confirm::Never];

pub struct SettingsPage {
    shared: Shared,
    fields: Vec<TextField>,
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
        SettingsPage { shared, fields, scroll: 0.0, hover: Hit::None, pressed: Hit::None, w: 0.0, h: 0.0 }
    }

    /// Sayfanın öğeleri. Dil, tema, oturumda başlama ve kaldırma winloom'ın Ayarlar'ında.
    fn items(&self) -> Vec<Item> {
        let s = self.shared.borrow();
        let c = &s.config;
        let field = |f: usize, title: &str, sub: String, w: f32| Item::Row(Row::new(f as u32, title, sub, Ctl::Space(w)));
        let button = |id: u32, title: &str, sub: String| Item::Row(Row::new(id, title, sub, Ctl::Button(Act::of_row(id).map_or("", Act::label).into())));
        let seg = |id: u32, title: &str, sub: &str, opts: &[&str], sel: usize| {
            Item::Row(Row::new(id, title, sub, Ctl::Seg(opts.iter().map(|o| o.to_string()).collect(), sel)))
        };
        let trained = if s.trained >= 3 {
            t!(format!("Learned from {} samples: recognised on this computer", s.trained), format!("{} örnekten öğrendi: bu bilgisayarda tanınır", s.trained))
        } else if !s.train_msg.is_empty() && s.voice_state == crate::voice::State::Training {
            t!(format!("Say it now: {}", s.train_msg), format!("Şimdi söyle: {}", s.train_msg))
        } else {
            t!("~1 min: say the wake phrase 3 times, then read 8 sentences", "~1 dk: uyandırma sözünü 3 kez söyle, sonra 8 cümle oku").into()
        };
        let training = s.voice_state == crate::voice::State::Training;
        let train_label = Act::Train.label().to_string();
        let mic = if c.mic.is_empty() {
            t!("Windows default. A Bluetooth headset drops to call quality while listening; prefer the laptop mic", "Windows'un varsayılanı. Bluetooth kulaklık dinlerken görüşme kalitesine düşer; dizüstünün mikrofonu daha iyi").into()
        } else {
            crate::audio::mics().into_iter().find(|m| m.0 == c.mic).map_or_else(|| t!("Selected microphone is not connected", "Seçili mikrofon takılı değil").into(), |m| m.1)
        };
        let local = match crate::xtts::status() {
            crate::xtts::Status::Ready => t!("ready", "hazır"),
            crate::xtts::Status::Loading => t!("loading…", "yükleniyor…"),
            crate::xtts::Status::Failed => t!("not running (make engram-ses)", "çalışmıyor (make engram-ses)"),
            crate::xtts::Status::Off => t!("off", "kapalı"),
        };
        let spotify = if crate::spotify::connected() {
            t!("Connected: \"give me some music\" picks from your playlists", "Bağlı: \"bana müzik ver\" listelerinden seçer")
        } else {
            t!("Client ID from developer.spotify.com (redirect http://127.0.0.1:8737/spotify)", "developer.spotify.com'daki Client ID (yönlendirme http://127.0.0.1:8737/spotify)")
        };
        vec![
            Item::Section(t!("Voice", "Ses").into()),
            field(F_GROQ, t!("Groq API key", "Groq API anahtarı"), t!("To hear you: free at console.groq.com (Whisper)", "Seni duymak için: console.groq.com'dan ücretsiz (Whisper)").into(), FIELD_W),
            field(
                F_GEMINI,
                t!("Gemini API key", "Gemini API anahtarı"),
                t!("Hearing if there is no Groq key, and its voice; from aistudio.google.com", "Groq yoksa duyma, ister konuşan ses; aistudio.google.com'dan").into(),
                FIELD_W,
            ),
            field(F_ELEVEN, t!("ElevenLabs API key", "ElevenLabs API anahtarı"), t!("Its voice; optional", "Konuşan sesi; isteğe bağlı").into(), FIELD_W),
            seg(
                R_TTS,
                t!("Speaking voice", "Konuşan ses"),
                t!("Local runs free on your graphics card; Windows' own voice is robotic", "Yerel: ekran kartında bedava; Windows'un kendi sesi robotiktir"),
                &[t!("Local", "Yerel"), "Gemini", "ElevenLabs", "Windows", t!("Off", "Kapalı")],
                TTS.iter().position(|t| *t == c.tts).unwrap_or(0),
            ),
            button(R_LOCAL, t!("Local voice", "Yerel ses"), format!("{} · {local}", c.local_voice)),
            button(R_GEMINI_VOICE, t!("Gemini voice", "Gemini sesi"), c.gemini_voice.clone()),
            field(F_VOICE, t!("ElevenLabs voice ID", "ElevenLabs ses kimliği"), t!("From elevenlabs.io › Voices; empty for the default", "elevenlabs.io › Voices'tan; boşsa varsayılan").into(), FIELD_W),
            Item::Row(Row::new(
                R_WAKE,
                t!("Wake by voice", "Sesle uyan"),
                t!(format!("Say \"{}\"; the microphone stays on", s.wake_phrase), format!("\"{}\" de; mikrofon açık kalır", s.wake_phrase)),
                Ctl::Toggle(c.wake),
            )),
            button(R_MIC, t!("Microphone", "Mikrofon"), mic),
            Item::Row(Row::new(
                R_LOCK,
                t!("Only my voice", "Yalnızca benim sesim"),
                t!("Music and other people don't trigger it; learned from \"Teach my voice\"", "Müzik ve başkaları tetiklemez; \"Sesimi tanıt\"tan öğrenir"),
                Ctl::Toggle(c.voice_lock),
            )),
            button(R_VOICE_NOTE, t!("Wake phrase and replies", "Uyandırma sözü ve cevaplar"), "sistem/ses.md".into()),
            Item::Row(Row::new(R_TRAIN, t!("Teach my voice", "Sesimi tanıt"), trained, if training { Ctl::Primary(train_label) } else { Ctl::Button(train_label) })),
            field(F_HOTKEY, t!("Push-to-talk key", "Bas-konuş tuşu"), t!("Hold it anywhere and talk", "Her yerde basılı tutup konuş").into(), SHORT_W),
            field(F_SPOTIFY, "Spotify", spotify.into(), FIELD_W),
            Item::Section(t!("Computer", "Bilgisayar").into()),
            Item::Row(Row::new(
                R_CONTROL,
                t!("Control the computer", "Bilgisayarı kontrol et"),
                t!("Mouse and keyboard; when off it can only look at the screen", "Fare ve klavye; kapalıyken yalnızca ekrana bakar"),
                Ctl::Toggle(c.control),
            )),
            seg(
                R_CONFIRM,
                t!("Ask before", "Onay"),
                t!("Risky only: deleting, installing, shutting down, pushing…", "Yalnızca riskli: silme, kurma, kapatma, push…"),
                &[t!("Always", "Her zaman"), t!("Risky only", "Yalnızca riskli"), t!("Never", "Hiç")],
                CONFIRMS.iter().position(|x| *x == c.confirm).unwrap_or(1),
            ),
            Item::Section(t!("Brain", "Beyin").into()),
            field(
                F_ANTHROPIC,
                t!("Anthropic API key", "Anthropic API anahtarı"),
                t!("From platform.claude.com; stored encrypted for this user", "platform.claude.com'dan; bu kullanıcıya şifreli saklanır").into(),
                FIELD_W,
            ),
            seg(
                R_MODEL,
                t!("Model", "Model"),
                t!("Opus is the smartest; Sonnet is faster and cheaper", "Opus en zekisi; Sonnet daha hızlı ve ucuz"),
                &MODELS.map(|m| m.0),
                MODELS.iter().position(|m| m.1 == c.model).unwrap_or(0),
            ),
            seg(
                R_EFFORT,
                t!("Thinking", "Düşünme"),
                t!("How long it thinks in written chat", "Yazılı sohbette ne kadar düşünsün"),
                &[t!("low", "az"), t!("medium", "orta"), t!("high", "çok")],
                EFFORTS.iter().position(|e| *e == c.effort).unwrap_or(1),
            ),
            field(F_VAULT, t!("Brain folder", "Beyin klasörü"), t!("Plain markdown notes; Obsidian can open the same folder", "Düz markdown notlar; Obsidian aynı klasörü açabilir").into(), FIELD_W),
            field(
                F_NAME,
                t!("Assistant's name", "Asistanın adı"),
                t!("It answers to it and winloom shows it under this name", "Ona böyle seslenirsin; winloom'da da bu adla görünür").into(),
                FIELD_W,
            ),
            Item::Section(t!("App", "Uygulama").into()),
            field(F_BUDGET, t!("API usage", "API kullanımı"), format!("{} · {}", crate::usage::summary(c.budget), crate::usage::tokens()), SHORT_W),
        ]
    }

    /// Yerleştirilmiş sayfa (kaydırma dahil).
    fn form(&self) -> Form {
        Form::new(self.items(), PAD, self.w - PAD, HEADER + 16.0 - self.scroll)
    }

    fn max_scroll(&self) -> f32 {
        (self.form().bottom() + self.scroll - self.h).max(0.0)
    }

    /// Yazı alanının yeri ve (varsa) sağındaki düğmesi: ikisi kitin ayırdığı yeri paylaşır.
    fn field_rects(g: &Gfx, form: &Form, f: usize) -> Option<(Rect, Option<(Act, Rect)>)> {
        let c = form.control(f as u32)?;
        Some(match Act::of_field(f) {
            Some(a) => {
                let b = button_rect_right(g, c.r, c.t, a.label(), false);
                (Rect::new(c.l, c.t, b.l - 8.0, c.b), Some((a, b)))
            }
            None => (c, None),
        })
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if y < HEADER {
            return Hit::None;
        }
        let form = self.form();
        match form.hit(g, x, y) {
            form::Hit::None => Hit::None,
            // Yazı alanı satırı: alan mı, düğmesi mi.
            form::Hit::Row(id) => match Self::field_rects(g, &form, id as usize) {
                Some((r, _)) if r.contains(x, y) => Hit::Field(id as usize),
                Some((_, Some((a, b)))) if b.contains(x, y) => Hit::Act(a),
                _ => Hit::None,
            },
            h => Hit::Form(h),
        }
    }

    /// Alanın yeri (fare olayları için).
    fn field_at(&self, g: &Gfx, f: usize) -> Option<Rect> {
        Self::field_rects(g, &self.form(), f).map(|(r, _)| r)
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

    fn choose(&mut self, id: u32, k: usize) {
        let mut st = self.shared.borrow_mut();
        match id {
            R_MODEL => st.config.model = MODELS[k.min(1)].1.into(),
            R_EFFORT => st.config.effort = EFFORTS[k.min(2)].into(),
            R_TTS => {
                st.config.tts = TTS[k.min(4)].into();
                st.voice_req = Some(VoiceReq::Apply);
            }
            _ => st.config.confirm = CONFIRMS[k.min(2)],
        }
        st.config.save();
    }

    fn flip(&mut self, id: u32) {
        let mut s = self.shared.borrow_mut();
        match id {
            R_WAKE => {
                s.config.wake = !s.config.wake;
                s.voice_req = Some(VoiceReq::Apply);
            }
            R_LOCK => {
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
                    crate::page::redraw(windows::Win32::Foundation::HWND(hwnd as *mut _));
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
                s.open_tab = Some(crate::page::Tab::Memory);
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
        }
    }

    fn redraw(&self) {
        crate::page::redraw(self.shared.borrow().hwnd);
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
        let form = self.form();
        let hover = if let Hit::Form(h) = self.hover { h } else { form::Hit::None };
        g.clip(Rect::new(0.0, HEADER, self.w, self.h), || {
            form.paint(g, hover, None);
            for (f, field) in self.fields.iter().enumerate() {
                let Some((r, act)) = Self::field_rects(g, &form, f) else { continue };
                if r.b < HEADER || r.t > self.h {
                    continue;
                }
                field.draw(g, r, self.hover == Hit::Field(f));
                if let Some((a, b)) = act {
                    button(g, b, a.label(), None, None, self.hover == Hit::Act(a));
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
            Hit::Form(form::Hit::Seg(id, k)) => self.choose(id, k),
            Hit::Form(form::Hit::Toggle(id)) => self.flip(id),
            Hit::Form(form::Hit::Button(id)) => {
                if let Some(a) = Act::of_row(id) {
                    self.act(a);
                }
            }
            Hit::Act(a) => self.act(a),
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

    fn interactive(&self, _g: &Gfx, _x: f32, y: f32) -> bool {
        y >= HEADER
    }

    fn set_visible(&mut self, visible: bool) {
        if visible {
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
