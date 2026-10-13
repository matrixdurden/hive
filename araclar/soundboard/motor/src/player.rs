//! Çalma iş parçacığı: dosyaları çözer, `bus` yuvalarını gerçek zamanın biraz önünde doldurur.
//! Çalan bir şey yokken komut bekler, hiç uyanmaz.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::bus::{Bus, LEAD, VOICES};
use crate::decode::{self, Decoder};
use crate::log;

enum Cmd {
    Toggle(u64, PathBuf),
    StopAll,
}

#[derive(Clone, Copy, Debug)]
pub struct Playing {
    pub id: u64,
    pub started: Instant,
    /// Saniye; bilinmiyorsa 0.
    pub duration: f64,
}

pub struct Player {
    tx: Sender<Cmd>,
    playing: Arc<Mutex<Vec<Playing>>>,
}

impl Player {
    /// `changed`: çalan listesi değişince (arayüzü uyandırmak için). `started`: yeni ses başlayınca.
    pub fn start(bus: &'static Bus, changed: impl Fn() + Send + 'static, started: impl Fn() + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let playing = Arc::new(Mutex::new(Vec::new()));
        let list = playing.clone();
        let _ = std::thread::Builder::new().name("calma".into()).spawn(move || {
            if let Err(e) = decode::startup() {
                log!("Media Foundation başlatılamadı: {e}");
                return;
            }
            run(bus, rx, &list, changed, started);
        });
        Self { tx, playing }
    }

    /// Çalıyorsa durdurur, çalmıyorsa baştan başlatır.
    pub fn toggle(&self, id: u64, path: PathBuf) {
        let _ = self.tx.send(Cmd::Toggle(id, path));
    }

    pub fn stop_all(&self) {
        let _ = self.tx.send(Cmd::StopAll);
    }

    pub fn playing(&self) -> Vec<Playing> {
        self.playing.lock().unwrap().clone()
    }
}

struct Active {
    id: u64,
    dec: Decoder,
    started: Instant,
    /// Dosya bitti: tüm okurlar sonuna varınca (bu an) yuva boşalır.
    done: Option<Instant>,
}

impl Active {
    fn info(&self) -> Playing {
        Playing { id: self.id, started: self.started, duration: self.dec.duration }
    }
}

/// Yuvayı `until` saniyelik ses birikene dek doldurur. Dosya biterse `true`.
fn fill(v: &crate::bus::Voice, dec: &mut Decoder, until: f64) -> bool {
    let target = (until * dec.rate as f64) as u64;
    while v.written() < target {
        let chunk = dec.read((target - v.written()).min(4096) as usize);
        if chunk.is_empty() {
            v.finish();
            return true;
        }
        v.push(chunk);
    }
    false
}

fn run(bus: &Bus, rx: Receiver<Cmd>, list: &Mutex<Vec<Playing>>, changed: impl Fn(), started: impl Fn()) {
    let mut slots: [Option<Active>; VOICES] = Default::default();
    loop {
        let busy = slots.iter().any(Option::is_some);
        let first = if busy {
            rx.recv_timeout(Duration::from_millis(20))
        } else {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };
        let mut cmds = Vec::new();
        match first {
            Ok(c) => cmds.push(c),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        cmds.extend(rx.try_iter());
        let mut dirty = false;

        for cmd in cmds {
            dirty = true;
            match cmd {
                Cmd::StopAll => {
                    for (i, s) in slots.iter_mut().enumerate() {
                        if s.take().is_some() {
                            bus.voices[i].stop();
                        }
                    }
                }
                Cmd::Toggle(id, path) => {
                    if let Some(i) = slots.iter().position(|s| s.as_ref().is_some_and(|a| a.id == id)) {
                        bus.voices[i].stop();
                        slots[i] = None;
                        continue;
                    }
                    let mut dec = match Decoder::open(&path) {
                        Ok(d) => d,
                        Err(e) => {
                            log!("açılamadı: {}: {e}", path.display());
                            continue;
                        }
                    };
                    // Boş yuva yoksa en eskisi susar.
                    let i = slots.iter().position(Option::is_none).unwrap_or_else(|| {
                        (0..VOICES).min_by_key(|&i| slots[i].as_ref().map(|a| a.started)).unwrap_or(0)
                    });
                    let v = &bus.voices[i];
                    v.begin(dec.rate);
                    let eof = fill(v, &mut dec, LEAD);
                    v.publish();
                    let now = Instant::now();
                    let done = eof.then(|| now + Duration::from_secs_f64(v.written() as f64 / dec.rate as f64 + 0.25));
                    slots[i] = Some(Active { id, dec, started: now, done });
                    started();
                }
            }
        }

        for (i, slot) in slots.iter_mut().enumerate() {
            let Some(a) = slot else { continue };
            let v = &bus.voices[i];
            match a.done {
                Some(t) if Instant::now() >= t => {
                    v.stop();
                    *slot = None;
                    dirty = true;
                }
                Some(_) => {}
                None => {
                    if fill(v, &mut a.dec, a.started.elapsed().as_secs_f64() + LEAD) {
                        let secs = v.written() as f64 / a.dec.rate as f64;
                        a.dec.duration = secs;
                        a.done = Some(a.started + Duration::from_secs_f64(secs + 0.25));
                        dirty = true;
                    }
                }
            }
        }

        if dirty {
            *list.lock().unwrap() = slots.iter().flatten().map(Active::info).collect();
            changed();
        }
    }
}
