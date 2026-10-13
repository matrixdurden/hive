//! Ajan: kişilik + kurallar + not indeksi sistem isteminde; araçlarla notları arar, okur, yazar,
//! bağlar; araç döngüsünü sürer. Arayüzden ayrı iş parçacığında çalışır, metni olay olay verir.

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use crate::claude::{self, Event, Request, Tool};
use crate::config::{Config, Confirm};
use crate::notes::{self, Vault};
use crate::util::Res;

/// İlk açılışta `sistem/kisilik.md`. `{ad}` ayarlardaki hitap adıyla değişir.
const DEFAULT_PERSONA: &str = "---
tags: [sistem]
---
# Kişilik

Bu dosya senin kim olduğunu söyler. İstediğin gibi değiştir; bir sonraki mesajda geçerli olur.

Sen {ad}'sin: kullanıcının ikinci beyni. Onun notlarını tutar, düzenler, bağlar ve notlarından cevap verirsin. Türkçe konuşursun, kullanıcıya \"sen\" dersin.

## Konuşma tarzı

Doğal, dürüst ve kısa. Sakin ve net; pohpohlamaz, vaaz vermezsin. Konuşma dili; resmi değil, laubali de değil.

- Doğrudan sohbet et. Araçlardan, notlara bakmaktan, \"günlükte şu kayıt var\" gibi raporlardan bahsetme; bilgiyi konuşmanın içinde doğalmış gibi kullan. \"Notlarıma baktım\", \"kaydettim\" deme; sadece bil ve konuş.
- Kısa onaylar kullan ama her seferinde aynısını değil; üst üste aynı kalıbı tekrarlama. Her cümleye hitap ekleme.
- Uydurma. Bilmiyorsan bilmediğini söyle. Emin değilsen sor.

## Hedef

(İstersen buraya ana hedefini yaz; her konuşmada o anki konuya bağlayarak seni ona yönlendirir.)
";

/// İlk açılışta `sistem/kurallar.md`.
const DEFAULT_RULES: &str = "---
tags: [sistem]
---
# Hafıza kuralları

Bu dosya notların nasıl tutulacağını söyler. Klasör adlarını, şablonu, neyin kaydedileceğini buradan değiştir.

- Notlara yalnızca gerekince bak: kullanıcı bir şey sorduğunda, geçmişe atıf yaptığında ya da cevap notlarda olabilecekken (search_notes, gerekirse read_note). Selamlaşma, sohbet, genel bilgi ve bilgisayar işleri için notlara bakma.
- Yalnızca kalıcı değeri olan bir şeyi yaz: yeni bir kişi, karar, plan, tercih, öğrenilen bir bilgi ya da kullanıcının “not al” dediği şey. Önce var mı diye ara; varsa append ile ekle, yoksa create ile oluştur. Selamlaşma, sohbet, sorduğu sorular, ekrana bakmak gibi geçici şeyler yazılmaz.
- Klasörler: kisiler/, projeler/, fikirler/, gunluk/ (gerekirse yeni klasör). Dosya adları küçük harf, ASCII, tire ile: kisiler/ayse.md.
- Günlüğe (append_daily) yalnızca günün kalıcı olaylarını yaz (bir iş bitti, bir karar verildi, bir şey öğrenildi), kısa ve [[bağlantı]]lı. Her konuşmayı yazma; saat kendiliğinden eklenir.
- Yeni not şablonu: başta `---` / `tags: [kisi]` / `olusturma: YYYY-AA-GG` / `---`, sonra `# Başlık`, sonra bilgiler; sonda `## Bağlantılar` altında ilgili [[notlar]]. Kişi notlarına: nasıl tanıştınız, önemli ayrıntılar, planlar.
- Notları silme, kullanıcının yazdığını bozma; eklemeyi tercih et.
- sistem/ klasörüne dokunma; orası kullanıcının.
- Not yazdığını söyleme; sessizce yaz.
";

/// İlk açılışta `sistem/rutinler.md`.
const DEFAULT_ROUTINES: &str = "---
tags: [sistem]
---
# Rutinler

Bir rutinin adını söyleyince {ad} adımlarını sırayla yapar. İstediğin gibi değiştir, yenisini ekle.

## Çalışma modu
- Spotify'da odaklanmaya uygun sakin bir liste çal, sesi %25 yap.
- Discord'u küçült.
- 50 dakikalık bir mola hatırlatıcısı kur.

## Mola
- Müziği duraklat.
- 10 dakika sonra \"mola bitti\" diye hatırlat.

## Gece
- Müziği durdur.
- Bugün günlükte ne yazıyorsa iki cümleyle özetle.
";

/// Bir turun arayüze/sese giden olayları.
#[derive(Debug, Clone)]
pub enum Out {
    Text(String),
    /// Araç çağrıları arasındaki kısa ilerleme notu ("Terminali açıyorum").
    Progress(String),
    /// Araç çağrıldı: kısa açıklama (ör. "search_notes: Ayşe"); durum satırı ve log için.
    Tool(String),
    /// Not yazıldı / değişti (yol).
    Changed(String),
    /// Bilgisayar kontrolü başlıyor: engram penceresi önden çekilsin, ekran ışığı "çalışıyor"a geçsin.
    Computer,
    Done,
    Error(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Text,
    Voice,
}

/// Bir turun dışarıyla bağları: olaylar, kullanıcı onayı ve durdurma.
pub struct Hooks<'a> {
    pub on: &'a mut dyn FnMut(Out),
    /// Onay sorar ve cevabı bekler: (soru, ayrıntı) → evet mi.
    pub confirm: &'a mut dyn FnMut(&str, &str) -> bool,
    pub stop: &'a AtomicBool,
}

pub struct Agent {
    pub cfg: Config,
    pub vault: Vault,
    /// Konuşma geçmişi (Messages API biçiminde), yalnızca sona eklenir.
    pub history: Vec<Value>,
    /// Son isteğin kullanım bilgisi (log için).
    pub usage: claude::Usage,
    /// Bu turda bilgisayarı kontrol izni alındı mı; bilgisayar kontrolü bu turda başladı mı.
    control_ok: bool,
    control_started: bool,
}

/// Bilgisayar kontrolünde bir tur bu kadar adım sürebilir.
const MAX_ROUNDS: usize = 60;

/// Toolset'in üyeleri; yalnızca bakan ve bekleyenler dışındakiler ekranda bir şey değiştirir.
const MEMBERS: [&str; 17] = [
    "screenshot",
    "zoom",
    "cursor_position",
    "wait",
    "left_click",
    "right_click",
    "middle_click",
    "double_click",
    "triple_click",
    "left_click_drag",
    "mouse_move",
    "left_mouse_down",
    "left_mouse_up",
    "scroll",
    "type",
    "key",
    "hold_key",
];
const PASSIVE: usize = 4;

/// Bilgisayar kontrolü: ekranı görme ve fare/klavye. Kapalıysa yalnızca bakabilir.
fn computer_toolset(control: bool) -> Value {
    let mut t = json!({"type": "computer_toolset_20260801"});
    if !control {
        let configs: serde_json::Map<String, Value> = MEMBERS[PASSIVE..].iter().map(|m| (m.to_string(), json!({"enabled": false}))).collect();
        t["configs"] = Value::Object(configs);
    }
    t
}

/// Sistem isteminin bilgisayar bölümü: makine, kabuklar, nasıl çalışılır.
fn computer_guide(control: bool, confirm: Confirm) -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let host = std::env::var("COMPUTERNAME").unwrap_or_default();
    let control_line = if control {
        "Fare ve klavyeyi kullanabilirsin (computer araçları): tıkla, yaz, kısayol bas, kaydır. Her tıklamadan önce güncel ekran görüntüsüne bak; işi bitirince bir ekran görüntüsüyle doğrula."
    } else {
        "Fare/klavye kontrolü ayarlardan kapalı: ekrana yalnızca bakabilirsin (screenshot, zoom)."
    };
    let confirm_line = match confirm {
        Confirm::Always => "Komutlar ve bilgisayar kontrolü kullanıcıdan onay ister; onay penceresini engram kendisi gösterir, sen ayrıca 'izin veriyor musun' diye sorma.",
        Confirm::Risky => "Sıradan komutlar onaysız çalışır; riskli olanlarda engram kullanıcıya sorar. run_command ve open_terminal'de `risky` alanını dürüstçe doldur: dosya silme ya da üzerine yazma, program kurma/kaldırma, süreç öldürme, kapatma/yeniden başlatma, git push / reset --hard / force, sudo, sistem ya da ağ ayarı, para harcama, mesaj/e-posta gönderme riskli (true); okuma, listeleme, durum, arama, derleme, test riskli değil (false). Sen ayrıca 'izin veriyor musun' diye sorma.",
        Confirm::Never => "Komutlar onaysız çalışır; geri alınamaz işlerden (silme, biçimlendirme, zorla push, kapatma) önce yine de sor.",
    };
    format!(
        "# Bilgisayar\n\
Kullanıcının bilgisayarındasın: Windows 11, bilgisayar adı {host}, kullanıcı {user}, ana klasör {home}. WSL (Linux) kurulu; kullanıcı geliştirmeyi orada yapar.\n\
- \"Ekranımdaki bu ne\" gibi sorularda önce screenshot al, ayrıntı gerekiyorsa zoom yap; gördüğünü kısaca anlat.\n\
- {control_line}\n\
- Terminal işleri için run_command (powershell, cmd ya da wsl). Komutlar etkileşimsiz çalışır: parola ya da onay bekleyen komutlar takılır. SSH için `ssh -o BatchMode=yes sunucu 'komut'` kullan (sunucu adları ~/.ssh/config'te; WSL'deki ve Windows'taki ayrı olabilir, gerekirse önce bak). Kullanıcı canlı bir oturum isterse (ssh'a bağlan, bir şeyi izle) open_terminal ile görünür pencerede aç.\n\
- Uygulama, dosya, klasör ya da adres açmak için open; açık pencereler için list_windows, birini öne getirmek için focus_window. Bunlar ekran görüntüsünden ucuz ve hızlı: önce bunları dene.\n\
- Müzik için önce spotify aracı (kullanıcının hesabı, Premium). \"Bana müzik ver\" gibi isteklerde saate, güne ve kullanıcının o anki işine göre ruh hâli seç; playlists ile listelerine bak, gerekirse tracks ile bir listenin içeriğine, top_artists ile zevkine bak, en uygun listeyi play ile çal (karışık için value 1) ve tek cümleyle ne seçtiğini söyle. Kullanıcı bir şarkıyı/sanatçıyı sevdiğini ya da sevmediğini söylerse fikirler/muzik.md'ye yaz, sonraki seçimlerde kullan. Spotify bağlı değilse ya da diğer uygulamalarda media aracı (çal, durdur, geç, sar). Bunun için ekrana bakıp tıklama.\n\
- Ses seviyesi (volume), çıkış cihazı (audio_output), pencere yaslama/küçültme (window), uygulama açma (app), dosya bulma (files), pano (clipboard), açık sekme ve sayfa metni (browser), pil/işlemci/kilitleme (system) için doğrudan bu araçları kullan: anında çalışırlar.\n\
- Hız önemli: işe ekran görüntüsüyle değil bildiğin yolla başla (open, focus_window, klavye kısayolları). Bilgisayar eylemlerini tek yanıtta toplu gönder, gerekmedikçe wait ve zoom kullanma, ekran görüntüsünü yalnızca yer bulmak ya da sonucu doğrulamak için al.\n\
- {confirm_line}\n\
- Kullanıcının bilgisayarıyla ilgili öğrendiğin kalıcı şeyleri (sunucu adları, proje yolları, sık komutlar) notlara yaz: bilgisayar/ klasörüne.\n\
- Bir iş birden çok adım sürecekse başlamadan tek kısa cümleyle ne yapacağını söyle; bitince sonucu bir iki cümleyle söyle."
    )
}

fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "search_notes",
            description: "Notlarda ara (başlık, etiket, gövde). Cevap notlarda olabilecekken ya da yeni bir not yazmadan önce (var mı diye) kullan; selamlaşma ve sohbet için kullanma.",
            schema: json!({"type":"object","properties":{"query":{"type":"string","description":"Aranacak kelimeler"},"limit":{"type":"integer","description":"En çok sonuç (varsayılan 8)"}},"required":["query","limit"],"additionalProperties":false}),
        },
        Tool {
            name: "read_note",
            description: "Bir notun tamamını oku. Yol kök klasöre göre, ör. kisiler/ayse.md; başlık da olur.",
            schema: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
        },
        Tool {
            name: "write_note",
            description: "Not oluştur ya da değiştir. mode: create (yoksa oluştur, varsa hata), replace (tamamını yaz), append (sona ekle). Markdown; başta `---` ön bilgi (tags, olusturma) kullan; ilgili notlara [[bağlantı]] koy.",
            schema: json!({"type":"object","properties":{"path":{"type":"string","description":"ör. kisiler/ayse.md"},"content":{"type":"string"},"mode":{"type":"string","enum":["create","replace","append"]}},"required":["path","content","mode"],"additionalProperties":false}),
        },
        Tool {
            name: "append_daily",
            description: "Bugünün günlüğüne (gunluk/YYYY-AA-GG.md) bir satır ekle; saat kendiliğinden yazılır. Yalnızca günün kalıcı olayları için (bir iş bitti, karar verildi, bir şey öğrenildi), ilgili [[bağlantı]]larla. Sohbeti, selamlaşmayı, soruları yazma.",
            schema: json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false}),
        },
        Tool {
            name: "link_notes",
            description: "Bir nota diğerine [[bağlantı]] ekle (yoksa). İki yönlü istersen iki kez çağır.",
            schema: json!({"type":"object","properties":{"from":{"type":"string"},"to":{"type":"string"}},"required":["from","to"],"additionalProperties":false}),
        },
        Tool {
            name: "today",
            description: "Bugünün tarihi, günü ve saati.",
            schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        },
        Tool {
            name: "run_command",
            description: "Kullanıcının bilgisayarında komut çalıştır ve çıktıyı al (etkileşimsiz; stdin kapalı). shell: powershell, cmd ya da wsl (bash -lc). cwd boşsa ana klasör. timeout_s: en çok 600.",
            schema: json!({"type":"object","properties":{"shell":{"type":"string","enum":["powershell","cmd","wsl"]},"command":{"type":"string"},"cwd":{"type":"string","description":"Çalışma klasörü; boş bırakılabilir"},"timeout_s":{"type":"integer","description":"Saniye (varsayılan 60)"},"risky":{"type":"boolean","description":"Geri alınması zor bir şey yapıyor mu (silme, kurma, kapatma, push, sudo...)"}},"required":["shell","command","cwd","timeout_s","risky"],"additionalProperties":false}),
        },
        Tool {
            name: "open_terminal",
            description: "Görünür bir terminal penceresinde komut başlat (ssh oturumu, uzun süren ya da izlenecek işler). Çıktıyı göremezsin; hemen döner.",
            schema: json!({"type":"object","properties":{"shell":{"type":"string","enum":["powershell","cmd","wsl"]},"command":{"type":"string"},"cwd":{"type":"string"},"risky":{"type":"boolean","description":"Geri alınması zor bir şey yapıyor mu"}},"required":["shell","command","cwd","risky"],"additionalProperties":false}),
        },
        Tool {
            name: "open",
            description: "Uygulama, dosya, klasör ya da web adresi aç (ör. \"spotify\", \"notepad\", \"C:\\\\Users\", \"https://...\"). args isteğe bağlı, boş olabilir.",
            schema: json!({"type":"object","properties":{"target":{"type":"string"},"args":{"type":"string"}},"required":["target","args"],"additionalProperties":false}),
        },
        Tool {
            name: "media",
            description: "Çalan müzik/video (Spotify, tarayıcı...): durum, çal, duraklat, geç, önceki, saniyeye sar. Ekrana bakıp tıklamaktan çok daha hızlı; müzikle ilgili her istekte önce bunu kullan. app boşsa o an çalan uygulama; \"spotify\" kapalıysa açılır. seek: seconds saniyesine sarar ve çalar.",
            schema: json!({"type":"object","properties":{"action":{"type":"string","enum":["status","play","pause","toggle","next","previous","seek"]},"app":{"type":"string","description":"ör. spotify; boş: o an çalan"},"seconds":{"type":"number","description":"seek için hedef saniye; diğerlerinde 0"}},"required":["action","app","seconds"],"additionalProperties":false}),
        },
        Tool {
            name: "reminder",
            description: "Hatırlatıcı ve zamanlayıcı: add (seconds sonra text'i sesli hatırlatır; pomodoro için 1500), list, cancel (id). engram kapanıp açılsa da kalır.",
            schema: json!({"type":"object","properties":{"action":{"type":"string","enum":["add","list","cancel"]},"seconds":{"type":"integer","description":"add için kaç saniye sonra; diğerlerinde 0"},"text":{"type":"string"},"id":{"type":"integer","description":"cancel için; diğerlerinde 0"}},"required":["action","seconds","text","id"],"additionalProperties":false}),
        },
        Tool {
            name: "spotify",
            description: "Kullanıcının Spotify hesabı (Premium). action: now (ne çalıyor), playlists (listeleri), tracks (target listesindeki şarkılar; \"liked\" beğenilenler), top_artists / top_tracks (kind: short_term|medium_term|long_term), recent, search (target sorgu; kind: track|artist|playlist|album), play (target: liste/albüm/sanatçı/şarkı uri ya da id, \"liked\", virgülle birden çok şarkı; boşsa devam; value 1 karışık çal), pause, resume, next, previous, seek (value saniye), volume (value 0-100), shuffle (value 1/0), repeat (value 0 kapalı,1 liste,2 şarkı), queue (target şarkı), like, unlike (çalan şarkı).",
            schema: json!({"type":"object","properties":{"action":{"type":"string","enum":["now","playlists","tracks","top_artists","top_tracks","recent","search","play","pause","resume","next","previous","seek","volume","shuffle","repeat","queue","like","unlike"]},"target":{"type":"string"},"kind":{"type":"string"},"value":{"type":"number"}},"required":["action","target","kind","value"],"additionalProperties":false}),
        },
        Tool {
            name: "volume",
            description: "Ses seviyesi: get, set (value %), up/down (value adım, varsayılan 10), mute, unmute. target boş: ana ses; uygulama adı (discord, brave, spotify...) o uygulamanın sesi; get için \"*\" bütün uygulamalar.",
            schema: json!({"type":"object","properties":{"action":{"type":"string","enum":["get","set","up","down","mute","unmute"]},"value":{"type":"number"},"target":{"type":"string"}},"required":["action","value","target"],"additionalProperties":false}),
        },
        Tool {
            name: "audio_output",
            description: "Ses çıkış cihazları: list ya da set (name: cihaz adının bir parçası, ör. kulaklık, speaker, LIVE300).",
            schema: json!({"type":"object","properties":{"action":{"type":"string","enum":["list","set"]},"name":{"type":"string"}},"required":["action","name"],"additionalProperties":false}),
        },
        Tool {
            name: "window",
            description: "Pencere: snap_left, snap_right (ekranın yarısına yasla), maximize, minimize, restore, close, top (hep üstte aç/kapa), minimize_all (masaüstünü göster). title: başlığın ya da uygulamanın bir parçası.",
            schema: json!({"type":"object","properties":{"action":{"type":"string","enum":["snap_left","snap_right","maximize","minimize","restore","close","top","minimize_all"]},"title":{"type":"string"}},"required":["action","title"],"additionalProperties":false}),
        },
        Tool {
            name: "app",
            description: "Başlat menüsündeki uygulamalar (yazım hatasına toleranslı): find (eşleşenleri listele) ya da launch (en iyisini aç).",
            schema: json!({"type":"object","properties":{"action":{"type":"string","enum":["find","launch"]},"query":{"type":"string"}},"required":["action","query"],"additionalProperties":false}),
        },
        Tool {
            name: "files",
            description: "Kullanıcının dosyaları (Masaüstü, İndirilenler, Belgeler, Resimler, Videolar, Müzik): find (query ad parçası, boşsa son dosyalar; kind: pdf/image/video/audio/document/archive/code ya da boş; days: son kaç gün, 0 hepsi; limit), open (path), reveal (path, Gezgin'de göster).",
            schema: json!({"type":"object","properties":{"action":{"type":"string","enum":["find","open","reveal"]},"query":{"type":"string"},"kind":{"type":"string"},"days":{"type":"integer"},"limit":{"type":"integer"},"path":{"type":"string"}},"required":["action","query","kind","days","limit","path"],"additionalProperties":false}),
        },
        Tool {
            name: "clipboard",
            description: "Pano: get (metin, kopyalanmış dosyalar ya da resim bilgisi) ya da set (text'i panoya koy).",
            schema: json!({"type":"object","properties":{"action":{"type":"string","enum":["get","set"]},"text":{"type":"string"}},"required":["action","text"],"additionalProperties":false}),
        },
        Tool {
            name: "browser",
            description: "Tarayıcı: tab (öndeki Brave/Chrome/Edge sekmesinin başlığı ve adresi), fetch (url'deki sayfanın düz metni; \"bu sayfayı özetle\" için önce tab, sonra fetch).",
            schema: json!({"type":"object","properties":{"action":{"type":"string","enum":["tab","fetch"]},"url":{"type":"string"}},"required":["action","url"],"additionalProperties":false}),
        },
        Tool {
            name: "system",
            description: "Bilgisayar: status (pil, işlemci, bellek, açık kalma süresi, disk), lock (kilitle), sleep (uykuya al), display_off (ekranı kapat).",
            schema: json!({"type":"object","properties":{"action":{"type":"string","enum":["status","lock","sleep","display_off"]}},"required":["action"],"additionalProperties":false}),
        },
        Tool {
            name: "list_windows",
            description: "Açık pencerelerin listesi: başlık ve uygulama.",
            schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        },
        Tool {
            name: "focus_window",
            description: "Başlığında verilen yazı geçen pencereyi öne getir.",
            schema: json!({"type":"object","properties":{"title":{"type":"string"}},"required":["title"],"additionalProperties":false}),
        },
    ]
}

fn weekday(date: &str) -> &'static str {
    // Zeller benzeri: 2000-01-01 Cumartesi.
    let p: Vec<u32> = date.split('-').filter_map(|s| s.parse().ok()).collect();
    if p.len() != 3 {
        return "";
    }
    let (y, m, d) = (p[0] as i64, p[1] as i64, p[2] as i64);
    let (y, m) = if m < 3 { (y - 1, m + 12) } else { (y, m) };
    let k = y % 100;
    let j = y / 100;
    let h = (d + 13 * (m + 1) / 5 + k + k / 4 + j / 4 + 5 * j) % 7;
    ["Cumartesi", "Pazar", "Pazartesi", "Salı", "Çarşamba", "Perşembe", "Cuma"][h as usize]
}

impl Agent {
    pub fn new(cfg: Config) -> Self {
        let vault = Vault::open(&cfg.vault);
        Agent { cfg, vault, history: Vec::new(), usage: Default::default(), control_ok: false, control_started: false }
    }

    /// Ayarlar değişti: anahtar, klasör, model, ad ya da bilgisayar ayarı değiştiyse konuşma baştan başlar.
    pub fn sync(&mut self, cfg: &Config) {
        let stale = self.cfg.anthropic_key != cfg.anthropic_key
            || self.cfg.vault != cfg.vault
            || self.cfg.model != cfg.model
            || self.cfg.name != cfg.name
            || self.cfg.control != cfg.control
            || self.cfg.confirm != cfg.confirm;
        if stale {
            *self = Agent::new(cfg.clone());
        } else {
            self.cfg = cfg.clone();
            self.vault.refresh();
        }
    }

    /// Konuşmayı unut.
    pub fn reset(&mut self) {
        self.history.clear();
    }

    /// Sistem istemi, hepsi not klasöründen: `sistem/kisilik.md` (kim, nasıl konuşur, hedef),
    /// `sistem/kurallar.md` (hafızayı nasıl tutar) ve not indeksi. Kullanıcı bu iki dosyayı
    /// düzenler, bir sonraki mesajda yürürlüğe girer. Yoksa varsayılanlarıyla oluşturulur.
    fn system(&mut self, mode: Mode) -> Vec<Value> {
        let name = self.cfg.name.clone();
        let persona = self.vault.system_note("kisilik", DEFAULT_PERSONA).replace("{ad}", &name);
        let rules = self.vault.system_note("kurallar", DEFAULT_RULES);
        let routines = self.vault.system_note("rutinler", DEFAULT_ROUTINES);
        let index = format!(
            "Not klasörünün indeksi ({} not); her satır `yol #etiketler | ilk satır`:\n{}",
            self.vault.notes.iter().filter(|n| !notes::is_system(&n.path)).count(),
            self.vault.index_text()
        );
        let mode_hint = match mode {
            Mode::Voice => "Şu an sesli konuşuyorsunuz ([ses]): 1-3 kısa cümle, markdown yok, liste yok, sembol ve adres okuma; sayıları ve tarihleri konuşma dilinde söyle. Cevabın sesli okunacak.",
            Mode::Text => "Şu an yazışıyorsunuz ([yazi]): sohbet gibi; liste ve başlık yok, [[bağlantı]] kullanabilirsin.",
        };
        vec![
            claude::system_block(
                &format!(
                    "{persona}\n\n{rules}\n\n{}\n\n# Rutinler (sistem/rutinler.md)\nKullanıcı bir rutinin adını söylerse adımlarını sırayla, sormadan uygula; bitince tek cümleyle söyle.\n{}",
                    computer_guide(self.cfg.control, self.cfg.confirm),
                    crate::notes::md::strip_frontmatter(&routines)
                ),
                true,
            ),
            claude::system_block(&index, true),
            claude::system_block(mode_hint, false),
        ]
    }

    /// Bir aracı çalıştırır; sonuç içeriği (metin ya da görüntü; modelin göreceği) ve hata mı.
    fn run_tool(&mut self, name: &str, input: &Value, toolset: Option<&str>, h: &mut Hooks) -> (Value, bool) {
        if h.stop.load(Ordering::Relaxed) {
            return (json!(t!("Stopped by the user.", "Kullanıcı durdurdu.")), true);
        }
        if toolset == Some("computer") {
            return self.run_computer(name, input, h);
        }
        let (text, err) = self.run_text_tool(name, input, h);
        (json!(text), err)
    }

    /// Bilgisayar kontrolü (toolset üyeleri): bakmak serbest, değiştirmek onaylı.
    fn run_computer(&mut self, name: &str, input: &Value, h: &mut Hooks) -> (Value, bool) {
        if !self.control_started {
            self.control_started = true;
            (h.on)(Out::Computer);
            // engram penceresi çekilsin, ekran görüntüsüne girmesin.
            std::thread::sleep(std::time::Duration::from_millis(350));
        }
        let passive = MEMBERS[..PASSIVE].contains(&name);
        if !passive && self.cfg.confirm == Confirm::Always && !self.control_ok {
            let what = describe_action(name, input);
            if !(h.confirm)(t!("Shall I take control of your computer?", "Bilgisayarını kontrol edeyim mi?"), &what) {
                return (json!(t!("The user did not allow controlling the computer.", "Kullanıcı bilgisayarın kontrolüne izin vermedi.")), true);
            }
            self.control_ok = true;
        }
        (h.on)(Out::Tool(describe_action(name, input)));
        let shot = |r: crate::util::Res<crate::computer::Shot>| match r {
            Ok(s) => (json!([{"type": "image", "source": {"type": "base64", "media_type": s.media_type, "data": s.data}}]), false),
            Err(e) => (json!(format!("Hata: {e}")), true),
        };
        match name {
            "screenshot" => shot(crate::computer::screenshot()),
            "zoom" => {
                let r: Vec<i32> = input["region"].as_array().map(|a| a.iter().filter_map(|v| v.as_i64()).map(|v| v as i32).collect()).unwrap_or_default();
                if r.len() != 4 {
                    return (json!("Hata: region [x0, y0, x1, y1] olmalı"), true);
                }
                shot(crate::computer::zoom([r[0], r[1], r[2], r[3]]))
            }
            _ => match crate::computer::act(name, input, h.stop) {
                Ok(t) => (json!(t), false),
                Err(e) => (json!(format!("Hata: {e}")), true),
            },
        }
    }

    /// Notlar, zaman ve bilgisayar araçları: metin sonuç.
    fn run_text_tool(&mut self, name: &str, input: &Value, h: &mut Hooks) -> (String, bool) {
        let Hooks { on, confirm, stop } = h;
        let s = |k: &str| input[k].as_str().unwrap_or("").to_string();
        let r: Res<String> = (|| {
            Ok(match name {
                "search_notes" => {
                    let limit = input["limit"].as_u64().unwrap_or(8).clamp(1, 30) as usize;
                    on(Out::Tool(format!("search_notes: {}", s("query"))));
                    let hits = self.vault.search(&s("query"), limit);
                    if hits.is_empty() {
                        "Sonuç yok.".to_string()
                    } else {
                        hits.iter().map(|(i, _, snip)| format!("{} | {}", self.vault.notes[*i].path, snip)).collect::<Vec<_>>().join("\n")
                    }
                }
                "read_note" => {
                    on(Out::Tool(format!("read_note: {}", s("path"))));
                    match self.vault.get(&s("path")) {
                        Some(n) => format!("# {} ({})\n{}", n.title, n.path, n.body),
                        None => return Err(format!("not yok: {}", s("path")).into()),
                    }
                }
                "write_note" => {
                    let (path, content, mode) = (s("path"), s("content"), s("mode"));
                    on(Out::Tool(format!("write_note: {path} ({mode})")));
                    let exists = self.vault.get(&path).is_some();
                    if notes::is_system(&self.vault.safe_path(&path)?) {
                        return Err("sistem/ klasörü kullanıcının; oraya yazılmaz".into());
                    }
                    let rel = match mode.as_str() {
                        "create" if exists => return Err(format!("zaten var: {path}; append ya da replace kullan").into()),
                        "append" => self.vault.append(&path, &content)?,
                        _ => self.vault.write(&path, &content)?,
                    };
                    on(Out::Changed(rel.clone()));
                    format!("yazıldı: {rel}")
                }
                "append_daily" => {
                    let (date, time) = crate::util::now();
                    let path = format!("gunluk/{date}.md");
                    on(Out::Tool(format!("append_daily: {}", s("text"))));
                    if self.vault.get(&path).is_none() {
                        self.vault.write(&path, &format!("---\ntags: [gunluk]\nolusturma: {date}\n---\n# {date} {}\n\n", weekday(&date)))?;
                    }
                    let rel = self.vault.append(&path, &format!("- {time} {}", strip_time(s("text").trim())))?;
                    on(Out::Changed(rel.clone()));
                    format!("eklendi: {rel}")
                }
                "link_notes" => {
                    let (from, to) = (s("from"), s("to"));
                    on(Out::Tool(format!("link_notes: {from} → {to}")));
                    let Some(n) = self.vault.get(&from).cloned() else { return Err(format!("not yok: {from}").into()) };
                    let target = self.vault.get(&to).map_or(to.trim_end_matches(".md").rsplit('/').next().unwrap_or(&to).to_string(), |t| t.title.clone());
                    if n.links.contains(&notes::key(&target)) {
                        format!("zaten bağlı: {from} → {target}")
                    } else {
                        let mut body = n.body.clone();
                        if !body.contains("## Bağlantılar") {
                            if !body.ends_with('\n') {
                                body.push('\n');
                            }
                            body.push_str("\n## Bağlantılar\n");
                        }
                        if !body.ends_with('\n') {
                            body.push('\n');
                        }
                        body.push_str(&format!("- [[{target}]]\n"));
                        let rel = self.vault.write(&n.path, &body)?;
                        on(Out::Changed(rel.clone()));
                        format!("bağlandı: {rel} → {target}")
                    }
                }
                "today" => {
                    let (date, time) = crate::util::now();
                    format!("{date} {} {time}", weekday(&date))
                }
                "run_command" | "open_terminal" => {
                    let (shell, command, cwd) = (s("shell"), s("command"), s("cwd"));
                    let risky = input["risky"].as_bool().unwrap_or(true) || dangerous(&command);
                    let cwd = Some(cwd.trim()).filter(|c| !c.is_empty());
                    let shown = if shell == "powershell" { command.clone() } else { format!("{shell}: {command}") };
                    on(Out::Tool(shown.clone()));
                    let ask = match self.cfg.confirm {
                        Confirm::Always => true,
                        Confirm::Risky => risky,
                        Confirm::Never => false,
                    };
                    if ask {
                        let q = if name == "open_terminal" {
                            t!("Open a terminal and run this?", "Terminalde açayım mı?")
                        } else {
                            t!("Run this command?", "Komutu çalıştırayım mı?")
                        };
                        if !confirm(q, &shown) {
                            return Err(t!("The user did not allow this command.", "Kullanıcı bu komuta izin vermedi.").into());
                        }
                    }
                    if name == "open_terminal" {
                        crate::computer::open_terminal(&shell, &command, cwd)?;
                        "terminal açıldı".to_string()
                    } else {
                        let timeout = input["timeout_s"].as_u64().unwrap_or(60).clamp(1, 600);
                        let r = crate::computer::run_command(&shell, &command, cwd, timeout, stop)?;
                        on(Out::Tool(format!("çıkış kodu {}", r.code.map_or("-".into(), |c| c.to_string()))));
                        let mut out = r.output;
                        if out.trim().is_empty() {
                            out = "(çıktı yok)".into();
                        }
                        let code = r.code.map_or("yok".into(), |c| c.to_string());
                        if r.timed_out { format!("{out}\n[zaman aşımı: süreç durduruldu]") } else { format!("{out}\n[çıkış kodu: {code}]") }
                    }
                }
                "open" => {
                    let (target, args) = (s("target"), s("args"));
                    on(Out::Tool(format!("open: {target}")));
                    crate::computer::open(&target, Some(args.trim()).filter(|a| !a.is_empty()))?;
                    format!("açıldı: {target}")
                }
                "media" => {
                    let (action, app) = (s("action"), s("app"));
                    let secs = input["seconds"].as_f64().unwrap_or(0.0);
                    on(Out::Tool(match action.as_str() {
                        "seek" => format!("{} {secs:.0}. saniye", t!("Media: seek to", "Müzik: sar")),
                        a => format!("{}: {a}", t!("Media", "Müzik")),
                    }));
                    if action == "status" && app.trim().is_empty() { crate::media::status()? } else { crate::media::act(&app, &action, secs)? }
                }
                "reminder" => {
                    let action = s("action");
                    on(Out::Tool(format!("{}: {action}", t!("Reminder", "Hatırlatıcı"))));
                    match action.as_str() {
                        "add" => crate::reminders::add(input["seconds"].as_u64().unwrap_or(60), &s("text")),
                        "cancel" => crate::reminders::cancel(input["id"].as_u64().unwrap_or(0)),
                        _ => crate::reminders::list(),
                    }
                }
                "spotify" => {
                    let (action, target, kind) = (s("action"), s("target"), s("kind"));
                    let value = input["value"].as_f64().unwrap_or(0.0);
                    on(Out::Tool(format!("Spotify: {action} {target}")));
                    if !crate::spotify::connected() {
                        return Err("Spotify bağlı değil (Ayarlar > Spotify > Bağlan); şimdilik media aracını kullan".into());
                    }
                    match action.as_str() {
                        "now" => crate::spotify::now()?,
                        "playlists" => crate::spotify::playlists()?,
                        "tracks" => crate::spotify::playlist_tracks(&target, 30)?,
                        "top_artists" | "top_tracks" => {
                            let range = if kind.is_empty() { "medium_term" } else { kind.as_str() };
                            crate::spotify::top(if action == "top_artists" { "artists" } else { "tracks" }, range)?
                        }
                        "recent" => crate::spotify::recent()?,
                        "search" => crate::spotify::search(&target, if kind.is_empty() { "track" } else { kind.as_str() })?,
                        "play" => crate::spotify::play(&target, (value >= 1.0).then_some(true))?,
                        "queue" => crate::spotify::queue(&target)?,
                        "like" | "unlike" => crate::spotify::like(action == "like")?,
                        a => crate::spotify::control(a, value)?,
                    }
                }
                "volume" => {
                    let (action, target) = (s("action"), s("target"));
                    on(Out::Tool(format!("{}: {action} {target}", t!("Volume", "Ses"))));
                    crate::system::volume(&action, input["value"].as_f64().unwrap_or(0.0), &target)?
                }
                "audio_output" => {
                    on(Out::Tool(format!("{}: {}", t!("Output", "Çıkış"), s("name"))));
                    if s("action") == "set" { crate::system::set_output(&s("name"))? } else { crate::system::outputs()? }
                }
                "window" => {
                    let action = s("action");
                    on(Out::Tool(format!("{}: {action} {}", t!("Window", "Pencere"), s("title"))));
                    crate::system::window(&action, &s("title"))?
                }
                "app" => {
                    on(Out::Tool(format!("{}: {}", t!("App", "Uygulama"), s("query"))));
                    if s("action") == "launch" { crate::system::launch(&s("query"))? } else { crate::system::apps(&s("query"))? }
                }
                "files" => {
                    let action = s("action");
                    on(Out::Tool(format!("{}: {action} {}{}", t!("Files", "Dosyalar"), s("query"), s("path"))));
                    match action.as_str() {
                        "open" => crate::system::open_file(&s("path"))?,
                        "reveal" => crate::system::reveal_file(&s("path"))?,
                        _ => crate::system::find_files(&s("query"), &s("kind"), input["days"].as_u64().unwrap_or(0) as u32, input["limit"].as_u64().unwrap_or(20) as usize)?,
                    }
                }
                "clipboard" => {
                    on(Out::Tool(format!("{}: {}", t!("Clipboard", "Pano"), s("action"))));
                    if s("action") == "set" { crate::system::clipboard_set(&s("text"))? } else { crate::system::clipboard_get()? }
                }
                "browser" => {
                    on(Out::Tool(format!("{}: {}", t!("Browser", "Tarayıcı"), s("action"))));
                    if s("action") == "fetch" { crate::system::fetch_page(&s("url"))? } else { crate::system::browser_tab()? }
                }
                "system" => {
                    let action = s("action");
                    on(Out::Tool(format!("{}: {action}", t!("System", "Sistem"))));
                    if action == "status" {
                        crate::system::status()?
                    } else {
                        let ask = match self.cfg.confirm {
                            Confirm::Always => true,
                            Confirm::Risky => action == "sleep",
                            Confirm::Never => false,
                        };
                        if ask && !confirm(t!("Shall I do this?", "Bunu yapayım mı?"), &action) {
                            return Err(t!("The user did not allow it.", "Kullanıcı izin vermedi.").into());
                        }
                        crate::system::power(&action)?
                    }
                }
                "list_windows" => {
                    on(Out::Tool("list_windows".into()));
                    crate::computer::windows_list()
                }
                "focus_window" => {
                    on(Out::Tool(format!("focus_window: {}", s("title"))));
                    crate::computer::focus_window(&s("title"))?
                }
                _ => return Err(format!("bilinmeyen araç: {name}").into()),
            })
        })();
        match r {
            Ok(s) => (s, false),
            Err(e) => (e.to_string(), true),
        }
    }

    /// Geçmiş sınırsız büyümesin: en eski turlar düşer. Kesim bir kullanıcı mesajında (kullanıcının
    /// kendi yazısı) başlar ki araç sonuçları çağrısız kalmasın. Kesilen geçmişte düşünme blokları
    /// artık önekiyle eşleşmez: kalanlardan da çıkarılır.
    fn trim_history(&mut self) {
        const MAX: usize = 60;
        if self.history.len() <= MAX {
            return;
        }
        let cut = self.history.len() - MAX;
        let is_user_text = |m: &Value| m["role"] == "user" && (m["content"].is_string() || m["content"][0]["type"] == "text");
        let start = (cut..self.history.len()).find(|&i| is_user_text(&self.history[i])).unwrap_or(self.history.len());
        self.history.drain(..start);
        for m in &mut self.history {
            if let Some(blocks) = m["content"].as_array_mut() {
                blocks.retain(|b| !matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking")));
            }
        }
        self.history.retain(|m| !m["content"].as_array().is_some_and(|a| a.is_empty()));
    }

    /// Eski ekran görüntüleri geçmişte kalmasın: her istekte yeniden gönderilir ve pahalıdır.
    /// Yalnızca en sonuncusu durur, öncekilerin yerine kısa bir not.
    fn drop_old_images(&mut self) {
        let mut seen = false;
        for m in self.history.iter_mut().rev() {
            let Some(blocks) = m["content"].as_array_mut() else { continue };
            for b in blocks.iter_mut().rev() {
                let Some(inner) = b["content"].as_array_mut() else { continue };
                for c in inner.iter_mut().rev() {
                    if c["type"] == "image" {
                        if seen {
                            *c = json!({"type": "text", "text": "[eski ekran görüntüsü]"});
                        }
                        seen = true;
                    }
                }
            }
        }
    }

    /// Kullanıcı yazısını geçmişe ekler; son mesaj zaten kullanıcınınsa (yarıda kalan tur, araç
    /// sonuçları) aynı mesaja eklenir: iki kullanıcı mesajı art arda gelmesin.
    fn push_user(&mut self, text: String) {
        if let Some(last) = self.history.last_mut()
            && last["role"] == "user"
        {
            if let Some(s) = last["content"].as_str().map(String::from) {
                last["content"] = json!([{"type": "text", "text": s}]);
            }
            if let Some(a) = last["content"].as_array_mut() {
                a.push(json!({"type": "text", "text": text}));
                return;
            }
        }
        self.history.push(json!({"role": "user", "content": text}));
    }

    /// Bir kullanıcı turu: araç döngüsünü sonuna kadar sürer, olayları `h.on`'a verir.
    pub fn turn(&mut self, text: &str, mode: Mode, h: &mut Hooks) -> Res<()> {
        let tag = match mode {
            Mode::Voice => "[ses]",
            Mode::Text => "[yazi]",
        };
        self.trim_history();
        self.drop_old_images();
        self.push_user(format!("{tag} {text}"));
        self.control_ok = false;
        self.control_started = false;
        let tools = tools();
        let raw = [computer_toolset(self.cfg.control)];
        for _ in 0..MAX_ROUNDS {
            if h.stop.load(Ordering::Relaxed) {
                break;
            }
            // Kişilik ve kurallar diskte değişmiş olabilir.
            self.vault.refresh();
            let system = self.system(mode);
            let req = Request {
                api_key: &self.cfg.anthropic_key,
                model: &self.cfg.model,
                max_tokens: 32_000,
                effort: if mode == Mode::Voice { "low" } else { self.cfg.effort.as_str() },
                system,
                messages: &self.history,
                tools: &tools,
                raw_tools: &raw,
            };
            let mut calls: Vec<(String, String, Value, Option<String>)> = Vec::new();
            let stop = h.stop;
            let on = &mut *h.on;
            let reply = claude::stream(&req, &mut |ev| {
                match ev {
                    Event::Text(t) => on(Out::Text(t)),
                    Event::Progress(t) => on(Out::Progress(t)),
                    Event::ToolUse { id, name, input, toolset } => calls.push((id, name, input, toolset)),
                    Event::Stop => {}
                }
                !stop.load(Ordering::Relaxed)
            })?;
            self.usage = reply.usage.clone();
            crate::usage::add(&self.cfg.model, &reply.usage);
            crate::log!(
                "claude: {} giriş, {} çıkış, önbellek okuma {}, yazma {}, durma {}",
                reply.usage.input,
                reply.usage.output,
                reply.usage.cache_read,
                reply.usage.cache_write,
                reply.stop_reason
            );
            // Yarıda kesilen yanıt geçmişe girmez: araç çağrıları cevapsız kalırdı.
            let cut = h.stop.load(Ordering::Relaxed) && reply.stop_reason.is_empty();
            if !reply.content.is_empty() && !cut {
                self.history.push(json!({"role": "assistant", "content": reply.content}));
            }
            if cut {
                break;
            }
            match reply.stop_reason.as_str() {
                "tool_use" if !calls.is_empty() => {
                    let mut results = Vec::new();
                    // Bir bilgisayar eylemi başarısız olunca aynı turdaki sonrakiler çalışmaz.
                    let mut computer_failed = false;
                    for (id, name, input, toolset) in &calls {
                        let (content, is_err) = if computer_failed && toolset.is_some() {
                            (json!("Not executed: an earlier computer action in this turn failed."), true)
                        } else {
                            self.run_tool(name, input, toolset.as_deref(), h)
                        };
                        if is_err && toolset.is_some() {
                            computer_failed = true;
                        }
                        let mut r = json!({"type": "tool_result", "tool_use_id": id, "content": content});
                        if let Some(t) = toolset {
                            r["toolset_name"] = json!(t);
                        }
                        if is_err {
                            r["is_error"] = json!(true);
                        }
                        results.push(r);
                    }
                    self.history.push(json!({"role": "user", "content": results}));
                }
                "refusal" => {
                    (h.on)(Out::Text(t!("I can't help with that.", "Buna yardım edemem.").to_string()));
                    break;
                }
                "max_tokens" => {
                    (h.on)(Out::Text(" …".into()));
                    break;
                }
                _ => break,
            }
        }
        (h.on)(Out::Done);
        Ok(())
    }
}

/// Modelin "riskli değil" dediği komutlarda da kesin sorulacak kalıplar.
fn dangerous(cmd: &str) -> bool {
    let c = format!(" {} ", cmd.to_lowercase().replace(['\n', ';', '&', '|', '('], " "));
    const WORDS: [&str; 34] = [
        " rm ", " rmdir ", " del ", " erase ", " rd ", "remove-item", " format ", "format-volume", "clear-disk", " mkfs", " dd ",
        " shutdown", " reboot", "restart-computer", "stop-computer", " sudo ", " runas ", "stop-process", " taskkill", " kill ",
        " pkill", "reg delete", "remove-itemproperty", "set-executionpolicy", " diskpart", " git push", "reset --hard", " --force",
        " drop ", " truncate ", "uninstall", "winget install", "apt install", " chmod -r",
    ];
    WORDS.iter().any(|w| c.contains(w)) || c.contains("curl ") && c.contains(" sh") || c.contains(" > ") && !c.contains(" > /dev/null")
}

/// Bilgisayar eyleminin okunur kısa açıklaması (onay ve durum satırı için).
fn describe_action(name: &str, input: &Value) -> String {
    let xy = |k: &str| input[k].as_array().map(|a| format!("({}, {})", a.first().and_then(Value::as_i64).unwrap_or(0), a.get(1).and_then(Value::as_i64).unwrap_or(0)));
    let text = input["text"].as_str().unwrap_or("");
    match name {
        "screenshot" => t!("Looking at the screen", "Ekrana bakıyorum").into(),
        "zoom" => t!("Looking closer", "Yakından bakıyorum").into(),
        "left_click" | "double_click" | "triple_click" | "right_click" | "middle_click" => {
            let what = match name {
                "double_click" => t!("Double click", "Çift tık"),
                "right_click" => t!("Right click", "Sağ tık"),
                _ => t!("Click", "Tıklama"),
            };
            format!("{what} {}", xy("coordinate").unwrap_or_default())
        }
        "type" => {
            let short: String = text.chars().take(60).collect();
            format!("{}: {short}", t!("Typing", "Yazıyorum"))
        }
        "key" => format!("{}: {text}", t!("Key", "Tuş")),
        "scroll" => t!("Scrolling", "Kaydırıyorum").into(),
        "left_click_drag" => t!("Dragging", "Sürüklüyorum").into(),
        "wait" => t!("Waiting", "Bekliyorum").into(),
        _ => name.to_string(),
    }
}

/// `engram --chat-test "..."`: pencere açmadan bir tur; metin ve araç çağrıları konsola.
pub fn probe(cfg: &Config, text: &str) -> i32 {
    use std::io::Write;
    let mut agent = Agent::new(cfg.clone());
    println!("notlar: {} ({})", agent.vault.notes.len(), cfg.vault.display());
    let stop = AtomicBool::new(false);
    let mut on = |o: Out| {
        match o {
            Out::Text(t) => {
                print!("{t}");
                let _ = std::io::stdout().flush();
            }
            Out::Tool(t) => println!("\n  [araç] {t}"),
            Out::Changed(p) => println!("  [değişti] {p}"),
            Out::Progress(t) => println!("\n  [ilerleme] {t}"),
            Out::Computer => println!("  [bilgisayar]"),
            Out::Done => println!(),
            Out::Error(e) => println!("\n  [hata] {e}"),
        }
    };
    // Komut satırında onay: konsoldan e/h.
    let mut confirm = |q: &str, d: &str| {
        print!("\n  [onay] {q} {d} (e/h) ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        matches!(line.trim(), "e" | "evet" | "y" | "yes")
    };
    let r = agent.turn(text, Mode::Text, &mut Hooks { on: &mut on, confirm: &mut confirm, stop: &stop });
    match r {
        Ok(()) => {
            let u = &agent.usage;
            println!("kullanım: giriş {} / çıkış {} / önbellek okuma {} / yazma {}", u.input, u.output, u.cache_read, u.cache_write);
            0
        }
        Err(e) => {
            println!("hata: {e}");
            1
        }
    }
}

/// Modelin satırın başına kendisi koyduğu saat ("14:27 ", "21:10 - "): saat zaten eklenir.
fn strip_time(text: &str) -> &str {
    let mut t = text;
    loop {
        let b = t.as_bytes();
        let is_time = b.len() >= 5 && b[0].is_ascii_digit() && b[1].is_ascii_digit() && b[2] == b':' && b[3].is_ascii_digit() && b[4].is_ascii_digit();
        if !is_time {
            return t;
        }
        t = t[5..].trim_start().trim_start_matches(['-', '–']).trim_start();
    }
}
