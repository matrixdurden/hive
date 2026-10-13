//! Çalan müzik ve video: Windows'un medya denetimi (SMTC; Spotify, tarayıcı, Medya Oynatıcı...).
//! Ekrana bakıp tıklamadan tek adımda çal, durdur, geç, istenen saniyeye sar. Spotify kapalıysa
//! açılır ve oturumu belirene kadar beklenir.

use std::time::{Duration, Instant};

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};

use crate::util::Res;

fn manager() -> Res<Manager> {
    Ok(Manager::RequestAsync()?.join()?)
}

/// Oturumun uygulama kimliği (ör. "Spotify.exe", "Chrome", "308046B0AF4A39CB").
fn app_id(s: &Session) -> String {
    s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default()
}

/// İstenen uygulamanın oturumu ("" = Windows'un geçerli oturumu). Adla eşleşen yoksa (ör.
/// Spotify tarayıcıda web uygulaması olarak açık: kimliği "Brave._crx_...") geçerli oturum.
fn find(mgr: &Manager, app: &str) -> Option<Session> {
    let app = app.trim().to_lowercase();
    let sessions = mgr.GetSessions().ok()?;
    let all: Vec<Session> = (0..sessions.Size().ok()?).filter_map(|i| sessions.GetAt(i).ok()).collect();
    if !app.is_empty()
        && let Some(s) = all.iter().find(|s| app_id(s).to_lowercase().contains(&app))
    {
        return Some(s.clone());
    }
    mgr.GetCurrentSession().ok().or_else(|| all.into_iter().next())
}

fn describe(s: &Session) -> String {
    let app = app_id(s);
    let (title, artist) = s
        .TryGetMediaPropertiesAsync()
        .and_then(|op| op.join())
        .map(|p| (p.Title().map(|h| h.to_string()).unwrap_or_default(), p.Artist().map(|h| h.to_string()).unwrap_or_default()))
        .unwrap_or_default();
    let state = match s.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()) {
        Ok(Status::Playing) => "çalıyor",
        Ok(Status::Paused) => "duraklatıldı",
        Ok(Status::Stopped) => "durdu",
        _ => "bilinmiyor",
    };
    let (pos, end) = s
        .GetTimelineProperties()
        .map(|t| (t.Position().map_or(0, |d| d.Duration), t.EndTime().map_or(0, |d| d.Duration)))
        .unwrap_or_default();
    let secs = |ticks: i64| ticks / 10_000_000;
    let clock = |s: i64| format!("{}:{:02}", s / 60, s % 60);
    let seek = s.GetPlaybackInfo().and_then(|i| i.Controls()).and_then(|c| c.IsPlaybackPositionEnabled()).unwrap_or(false);
    format!(
        "{app}: {artist} — {title} ({state}, {} / {}{})",
        clock(secs(pos)),
        clock(secs(end)),
        if seek { "" } else { ", sarma desteklenmiyor" }
    )
}

/// Açık medya oturumlarının listesi.
pub fn status() -> Res<String> {
    let mgr = manager()?;
    let sessions = mgr.GetSessions()?;
    let lines: Vec<String> = (0..sessions.Size()?).filter_map(|i| sessions.GetAt(i).ok()).map(|s| describe(&s)).collect();
    Ok(if lines.is_empty() { "Çalan ya da açık medya yok.".into() } else { lines.join("\n") })
}

/// Eylem: play, pause, toggle, next, previous, seek (saniyeye sar ve çal). `app` boşsa geçerli
/// oturum; "spotify" kapalıysa açılır.
pub fn act(app: &str, action: &str, seconds: f64) -> Res<String> {
    let mgr = manager()?;
    let mut session = find(&mgr, app);
    // Hiç medya yoksa ve Spotify isteniyorsa uygulama açılır.
    if session.is_none() && app.trim().eq_ignore_ascii_case("spotify") {
        crate::computer::open("spotify:", None)?;
        let t = Instant::now();
        while session.is_none() && t.elapsed() < Duration::from_secs(12) {
            std::thread::sleep(Duration::from_millis(300));
            session = find(&mgr, app);
        }
    }
    let s = session.ok_or_else(|| {
        if app.trim().is_empty() { "Açık medya yok.".to_string() } else { format!("{app} için medya oturumu yok (uygulama açık ve bir şey yüklü mü?)") }
    })?;
    let ok = |r: windows::core::Result<bool>| -> Res<()> {
        if r? { Ok(()) } else { Err("uygulama bu isteği kabul etmedi".into()) }
    };
    match action {
        "play" => ok(s.TryPlayAsync()?.join())?,
        "pause" => ok(s.TryPauseAsync()?.join())?,
        "toggle" => ok(s.TryTogglePlayPauseAsync()?.join())?,
        "next" => ok(s.TrySkipNextAsync()?.join())?,
        "previous" => ok(s.TrySkipPreviousAsync()?.join())?,
        "seek" => {
            let ticks = (seconds.max(0.0) * 10_000_000.0) as i64;
            ok(s.TryChangePlaybackPositionAsync(ticks)?.join())?;
            if !matches!(s.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()), Ok(Status::Playing)) {
                let _ = s.TryPlayAsync()?.join();
            }
        }
        "status" => {}
        _ => return Err(format!("bilinmeyen eylem: {action}").into()),
    }
    // Uygulama durumu güncellesin.
    std::thread::sleep(Duration::from_millis(250));
    Ok(describe(&s))
}
