//! Çalan şarkı: Windows'un ortak medya denetimi (Spotify, tarayıcı, ... hepsi buraya yazar).
//! Spotify'ın kendi API'ına, girişe gerek yok. Ayrı bir iş parçacığı oturumları izler: olay
//! gelince durumu okur, değiştiyse paylaşılan `Now`'a yazıp hive'ın penceresine haber verir.
//! Düğmeler (çal, sonraki, ...) de aynı iş parçacığına gönderilir; WinRT beklemeleri arayüzü
//! kilitlemesin.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Playback,
};
use windows::Storage::Streams::{DataReader, InputStreamOptions};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Çalan şarkı değişti (hive'ın penceresine).
pub const WM_MEDIA: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 70;

#[derive(Clone, Default, PartialEq)]
pub struct Now {
    /// Bir oturum var mı (Spotify ya da, izin verildiyse, başka bir oynatıcı açık).
    pub active: bool,
    /// Oturum Spotify'ın mı.
    pub spotify: bool,
    /// Oynatıcının uygulama kimliği (AUMID): karta tıklayınca o açılır.
    pub app: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub can_prev: bool,
    pub can_next: bool,
    pub can_seek: bool,
    /// Saniye: `at` anındaki konum ve şarkının uzunluğu (bilinmiyorsa 0).
    pub position: f64,
    pub duration: f64,
    pub at: Option<Instant>,
    /// Kapak resmi (JPEG / PNG baytları) ve her yeni kapakta artan sayaç.
    pub art: Option<Arc<Vec<u8>>>,
    pub art_gen: u32,
}

impl Now {
    /// Şu anki konum (saniye): çalıyorsa son okumadan bu yana geçen süre eklenir.
    pub fn position_now(&self) -> f64 {
        let p = match (self.playing, self.at) {
            (true, Some(at)) => self.position + at.elapsed().as_secs_f64(),
            _ => self.position,
        };
        if self.duration > 0.0 { p.clamp(0.0, self.duration) } else { p.max(0.0) }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Cmd {
    PlayPause,
    Next,
    Prev,
    /// Saniye.
    Seek(f64),
}

enum Msg {
    Changed,
    Cmd(Cmd),
    /// Spotify dışındaki oynatıcılar da görünsün mü.
    Others(bool),
    Quit,
}

pub struct Media {
    tx: Sender<Msg>,
    pub now: Arc<Mutex<Now>>,
}

/// Düğmeleri medya iş parçacığına iletir (widget'ın elinde).
#[derive(Clone)]
pub struct Remote(Sender<Msg>);

impl Remote {
    /// Hiçbir yere gitmeyen (önizleme için).
    pub fn dummy() -> Self {
        Remote(mpsc::channel().0)
    }

    pub fn send(&self, c: Cmd) {
        let _ = self.0.send(Msg::Cmd(c));
        // Oynatıcı durumu bazen olaysız değiştirir: biraz sonra yeniden bakılır.
        let tx = self.0.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            let _ = tx.send(Msg::Changed);
        });
    }
}

impl Media {
    /// İzlemeyi başlatır; değişiklikte `hwnd`'ye WM_MEDIA gönderilir.
    pub fn start(hwnd: HWND, others: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        let now = Arc::new(Mutex::new(Now::default()));
        let (tx2, now2, hwnd) = (tx.clone(), now.clone(), hwnd.0 as usize);
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            if let Err(e) = run(rx, tx2, now2, hwnd, others) {
                crate::log!("mockturtle: medya denetimi açılamadı: {e}");
            }
        });
        Self { tx, now }
    }

    pub fn remote(&self) -> Remote {
        Remote(self.tx.clone())
    }

    pub fn set_others(&self, on: bool) {
        let _ = self.tx.send(Msg::Others(on));
    }

    pub fn now(&self) -> Now {
        self.now.lock().unwrap().clone()
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Quit);
    }
}

/// İzlenen oturum ve olay kayıtları.
struct Watched {
    session: Session,
    id: String,
    tokens: [i64; 3],
}

impl Drop for Watched {
    fn drop(&mut self) {
        let _ = self.session.RemoveMediaPropertiesChanged(self.tokens[0]);
        let _ = self.session.RemovePlaybackInfoChanged(self.tokens[1]);
        let _ = self.session.RemoveTimelinePropertiesChanged(self.tokens[2]);
    }
}

/// Spotify'ın masaüstü uygulaması ("Spotify.exe", Microsoft Store'daki "SpotifyAB...") ya da
/// tarayıcıya kurulan web uygulaması (PWA). PWA'nın kimliği "Brave._crx_<uygulama>": Chromium
/// uzun kimliği ortasından kısaltır, Spotify'ınki (pjibgclleladliembfgfagdaldikeohf)
/// "pjibgcllel…dikeohf" diye kalır.
fn is_spotify(id: &str) -> bool {
    let id = id.to_lowercase();
    id.contains("spotify") || (id.contains("_crx_pjibgcll") && id.contains("dikeohf"))
}

/// Gösterilecek oturum: Spotify; izin varsa yoksa Windows'un şu anki oturumu.
fn pick(m: &Manager, others: bool) -> Option<Session> {
    let list = m.GetSessions().ok()?;
    for i in 0..list.Size().unwrap_or(0) {
        if let Ok(s) = list.GetAt(i)
            && s.SourceAppUserModelId().is_ok_and(|id| is_spotify(&id.to_string()))
        {
            return Some(s);
        }
    }
    others.then(|| m.GetCurrentSession().ok()).flatten()
}

fn run(rx: Receiver<Msg>, tx: Sender<Msg>, now: Arc<Mutex<Now>>, hwnd: usize, mut others: bool) -> windows::core::Result<()> {
    let m = Manager::RequestAsync()?.join()?;
    let changed = |tx: &Sender<Msg>| {
        let tx = tx.clone();
        move || {
            let _ = tx.send(Msg::Changed);
        }
    };
    let f = changed(&tx);
    let t1 = m.SessionsChanged(&TypedEventHandler::new(move |_, _| {
        f();
        Ok(())
    }))?;
    let f = changed(&tx);
    let t2 = m.CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
        f();
        Ok(())
    }))?;

    let mut watched: Option<Watched> = None;
    let mut art_key = String::new();
    loop {
        // Oturumu seç; değiştiyse olaylarına abone ol.
        let s = pick(&m, others);
        let id = s.as_ref().and_then(|s| s.SourceAppUserModelId().ok()).map(|h| h.to_string()).unwrap_or_default();
        if watched.as_ref().map(|w| &w.id) != Some(&id) {
            watched = None;
            if let Some(s) = s {
                let (a, b, c) = (changed(&tx), changed(&tx), changed(&tx));
                let tokens = [
                    s.MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
                        a();
                        Ok(())
                    }))?,
                    s.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
                        b();
                        Ok(())
                    }))?,
                    s.TimelinePropertiesChanged(&TypedEventHandler::new(move |_, _| {
                        c();
                        Ok(())
                    }))?,
                ];
                watched = Some(Watched { session: s, id: id.clone(), tokens });
                art_key.clear();
            }
        }

        let prev = now.lock().unwrap().clone();
        let next = match &watched {
            Some(w) => read(&w.session, &id, &prev, &mut art_key).unwrap_or_else(|_| prev.clone()),
            None => Now::default(),
        };
        if next != prev {
            *now.lock().unwrap() = next;
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_MEDIA, WPARAM(0), LPARAM(0));
            }
        }

        // Olay gelmese de arada bir bakılır (bazı oynatıcılar her değişikliği bildirmez).
        let msg = match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(m) => m,
            Err(RecvTimeoutError::Timeout) => Msg::Changed,
            Err(RecvTimeoutError::Disconnected) => Msg::Quit,
        };
        let mut quit = false;
        let mut handle = |m: Msg, others: &mut bool| match m {
            Msg::Cmd(c) => command(watched.as_ref().map(|w| &w.session), c),
            Msg::Others(o) => *others = o,
            Msg::Quit => quit = true,
            Msg::Changed => {}
        };
        handle(msg, &mut others);
        // Olaylar art arda gelir (şarkı değişince üçü birden): kısa bir süre toplanır.
        std::thread::sleep(Duration::from_millis(60));
        while let Ok(m) = rx.try_recv() {
            handle(m, &mut others);
        }
        if quit {
            break;
        }
    }
    drop(watched);
    let _ = m.RemoveSessionsChanged(t1);
    let _ = m.RemoveCurrentSessionChanged(t2);
    Ok(())
}

/// 100 ns birimli zaman (TimeSpan, DateTime) → saniye.
fn secs(ticks: i64) -> f64 {
    ticks as f64 / 1e7
}

fn read(s: &Session, id: &str, prev: &Now, art_key: &mut String) -> windows::core::Result<Now> {
    let props = s.TryGetMediaPropertiesAsync()?.join()?;
    let info = s.GetPlaybackInfo()?;
    let controls = info.Controls()?;
    let tl = s.GetTimelineProperties()?;
    let playing = info.PlaybackStatus()? == Playback::Playing;
    let title = props.Title()?.to_string();
    let artist = props.Artist()?.to_string();

    // Konum, oynatıcının son yazdığı andan bu yana ilerlemiştir (Spotify yalnızca çal /
    // duraklat / atla anında yazar).
    let start = secs(tl.StartTime()?.Duration);
    let end = secs(tl.EndTime()?.Duration);
    let mut position = secs(tl.Position()?.Duration) - start;
    let updated = tl.LastUpdatedTime()?.UniversalTime;
    let ft = unsafe { GetSystemTimeAsFileTime() };
    let now_ticks = ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64;
    if playing && updated > 0 && now_ticks > updated {
        position += secs(now_ticks - updated);
    }

    // Kapak yalnızca şarkı değişince (ya da henüz gelmediyse) okunur.
    let key = format!("{id}\n{title}\n{artist}");
    let (mut art, mut art_gen) = (prev.art.clone(), prev.art_gen);
    if *art_key != key || art.is_none() {
        let bytes = match props.Thumbnail() {
            Ok(t) => match thumbnail(&t) {
                Ok(b) if !b.is_empty() => Some(b),
                Ok(_) => None,
                Err(e) => {
                    crate::log!("mockturtle: kapak okunamadı ({title}): {e}");
                    None
                }
            },
            Err(_) => None,
        };
        if *art_key != key || bytes.is_some() {
            art = bytes.map(Arc::new);
            art_gen = art_gen.wrapping_add(1);
        }
        *art_key = key;
    }

    Ok(Now {
        active: true,
        spotify: is_spotify(id),
        app: id.to_string(),
        title,
        artist,
        playing,
        can_prev: controls.IsPreviousEnabled()?,
        can_next: controls.IsNextEnabled()?,
        can_seek: controls.IsPlaybackPositionEnabled().unwrap_or(false) && end > start,
        position,
        duration: (end - start).max(0.0),
        at: Some(Instant::now()),
        art,
        art_gen,
    })
}

/// Kapak baytları. Bazı oynatıcılar (tarayıcılar) akışın boyunu bildirmez: parça parça,
/// akış bitene kadar okunur.
fn thumbnail(t: &windows::Storage::Streams::IRandomAccessStreamReference) -> windows::core::Result<Vec<u8>> {
    let stream = t.OpenReadAsync()?.join()?;
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
    reader.SetInputStreamOptions(InputStreamOptions::Partial)?;
    let mut out = Vec::new();
    loop {
        let n = reader.LoadAsync(64 * 1024)?.join()?;
        if n == 0 {
            break;
        }
        let mut buf = vec![0u8; n as usize];
        reader.ReadBytes(&mut buf)?;
        out.extend_from_slice(&buf);
        if out.len() > 8 << 20 {
            break;
        }
    }
    Ok(out)
}

fn command(s: Option<&Session>, c: Cmd) {
    let Some(s) = s else { return };
    let r = match c {
        Cmd::PlayPause => s.TryTogglePlayPauseAsync().and_then(|a| a.join()),
        Cmd::Next => s.TrySkipNextAsync().and_then(|a| a.join()),
        Cmd::Prev => s.TrySkipPreviousAsync().and_then(|a| a.join()),
        Cmd::Seek(t) => {
            let start = s.GetTimelineProperties().and_then(|tl| tl.StartTime()).map_or(0, |t| t.Duration);
            s.TryChangePlaybackPositionAsync(start + (t * 1e7) as i64).and_then(|a| a.join())
        }
    };
    if let Err(e) = r {
        crate::log!("mockturtle: {c:?} olmadı: {e}");
    }
}

/// `hive --mockturtle-test`: görülen oturumlar ve gösterilecek olanın durumu.
pub fn probe() -> (String, Option<Now>) {
    let mut shown = None;
    let r = (|| -> windows::core::Result<String> {
        let m = Manager::RequestAsync()?.join()?;
        let list = m.GetSessions()?;
        let mut s = format!("medya oturumları: {}\n", list.Size()?);
        for i in 0..list.Size()? {
            s += &format!("  {}\n", list.GetAt(i)?.SourceAppUserModelId()?);
        }
        match pick(&m, true) {
            Some(x) => {
                let id = x.SourceAppUserModelId()?.to_string();
                let n = read(&x, &id, &Now::default(), &mut String::new())?;
                s += &format!(
                    "gösterilen: {id} (spotify: {})\n  {} · {}\n  {} · {:.0}/{:.0} sn · kapak {} bayt\n",
                    n.spotify,
                    n.title,
                    n.artist,
                    if n.playing { "çalıyor" } else { "duruyor" },
                    n.position,
                    n.duration,
                    n.art.as_ref().map_or(0, |a| a.len())
                );
                shown = Some(n);
            }
            None => s += "gösterilen: yok\n",
        }
        Ok(s)
    })();
    (r.unwrap_or_else(|e| format!("medya denetimi okunamadı: {e}\n")), shown)
}
