//! Beyin: tek konuşma, iki giriş (yazı ve ses). Ajan ayrı iş parçacığında çalışır; olayları
//! pencereye WM_AGENT ile gelir, burada konuşma dökümüne işlenir. Ajan onay isteyince iş
//! parçacığı cevabı bekler; soru arayüzde (ve sesliyse ekran ışığında) görünür, cevap `answer`
//! ile döner. `cancel` turu her yerden durdurur.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::agent::{Agent, Hooks, Mode, Out};
use crate::config::Config;

/// Ajandan olay geldi.
pub const WM_AGENT: u32 = WM_APP + 80;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    User,
    Assistant,
}

pub struct Msg {
    pub role: Role,
    pub mode: Mode,
    pub text: String,
    /// Ajanın attığı adımlar (araçlar, ilerleme notları): dökümde soluk satırlar.
    pub steps: Vec<String>,
    pub error: Option<String>,
}

/// Bekleyen onay sorusu.
#[derive(Clone, Debug)]
pub struct Ask {
    pub question: String,
    pub detail: String,
}

/// İş parçacığından arayüze.
enum Ev {
    Out(Out),
    Ask(Ask),
}

pub struct Brain {
    hwnd: HWND,
    agent: Arc<Mutex<Option<Agent>>>,
    events: Arc<Mutex<VecDeque<Ev>>>,
    stop: Arc<AtomicBool>,
    reply: Arc<Mutex<Option<mpsc::Sender<bool>>>>,
    pub msgs: Vec<Msg>,
    pub busy: bool,
    /// Süren turun türü.
    pub mode: Mode,
    pub ask: Option<Ask>,
    /// Son adımın kısa açıklaması ("Ekrana bakıyorum"); tur bitince boşalır.
    pub status: String,
    /// Döküm her değiştiğinde artar (çizim önbellekleri için).
    pub rev: u64,
}

impl Brain {
    pub fn new(hwnd: HWND) -> Self {
        Brain {
            hwnd,
            agent: Arc::new(Mutex::new(None)),
            events: Arc::new(Mutex::new(VecDeque::new())),
            stop: Arc::new(AtomicBool::new(false)),
            reply: Arc::new(Mutex::new(None)),
            msgs: Vec::new(),
            busy: false,
            mode: Mode::Text,
            ask: None,
            status: String::new(),
            rev: 0,
        }
    }

    /// Kullanıcının sözü ya da yazısı: tur başlar (meşgulse yok sayılır, `false`).
    pub fn send(&mut self, text: &str, mode: Mode, cfg: &Config) -> bool {
        let text = text.trim().to_string();
        if text.is_empty() || self.busy {
            return false;
        }
        self.msgs.push(Msg { role: Role::User, mode, text: text.clone(), steps: Vec::new(), error: None });
        self.msgs.push(Msg { role: Role::Assistant, mode, text: String::new(), steps: Vec::new(), error: None });
        self.busy = true;
        self.mode = mode;
        self.status.clear();
        self.rev += 1;
        self.stop.store(false, Ordering::Relaxed);
        let cfg = cfg.clone();
        let (agent, events, stop, reply, hwnd) = (self.agent.clone(), self.events.clone(), self.stop.clone(), self.reply.clone(), self.hwnd.0 as usize);
        std::thread::spawn(move || {
            let post = |e: Ev| {
                events.lock().unwrap().push_back(e);
                unsafe {
                    let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_AGENT, WPARAM(0), LPARAM(0));
                }
            };
            let mut guard = agent.lock().unwrap();
            match guard.as_mut() {
                Some(a) => a.sync(&cfg),
                None => *guard = Some(Agent::new(cfg.clone())),
            }
            let a = guard.as_mut().unwrap();
            let mut on = |o: Out| post(Ev::Out(o));
            let mut confirm = |q: &str, d: &str| {
                let (tx, rx) = mpsc::channel();
                *reply.lock().unwrap() = Some(tx);
                post(Ev::Ask(Ask { question: q.to_string(), detail: d.to_string() }));
                loop {
                    match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(yes) => return yes,
                        Err(mpsc::RecvTimeoutError::Timeout) if !stop.load(Ordering::Relaxed) => {}
                        Err(_) => return false,
                    }
                }
            };
            let r = a.turn(&text, mode, &mut Hooks { on: &mut on, confirm: &mut confirm, stop: &stop });
            if let Err(e) = r {
                crate::log!("ajan hatası: {e}");
                post(Ev::Out(Out::Error(e.to_string())));
                post(Ev::Out(Out::Done));
            }
        });
        true
    }

    /// Süren turu durdurur; bekleyen onay "hayır" sayılır.
    pub fn cancel(&mut self) {
        if self.busy {
            self.stop.store(true, Ordering::Relaxed);
        }
        self.answer(false);
    }

    /// Onay sorusunun cevabı.
    pub fn answer(&mut self, yes: bool) {
        if let Some(tx) = self.reply.lock().unwrap().take() {
            let _ = tx.send(yes);
        }
        if self.ask.take().is_some() {
            self.rev += 1;
        }
    }

    /// Konuşmayı baştan başlatır (meşgulken olmaz).
    pub fn clear(&mut self) {
        if self.busy {
            return;
        }
        self.msgs.clear();
        self.rev += 1;
        if let Ok(mut g) = self.agent.try_lock()
            && let Some(a) = g.as_mut()
        {
            a.reset();
        }
    }

    /// WM_AGENT: biriken olayları dökümüne işler ve sesin, ekran ışığının da görmesi için
    /// döndürür. Onay sorusu `ask`'a düşer.
    pub fn on_events(&mut self) -> Vec<Out> {
        let items: Vec<Ev> = self.events.lock().unwrap().drain(..).collect();
        let mut outs = Vec::new();
        for e in items {
            let o = match e {
                Ev::Ask(a) => {
                    self.ask = Some(a);
                    self.rev += 1;
                    continue;
                }
                Ev::Out(o) => o,
            };
            let last = self.msgs.last_mut();
            match &o {
                Out::Text(t) => {
                    if let Some(m) = last {
                        m.text.push_str(t);
                    }
                }
                Out::Progress(t) => {
                    self.status = t.clone();
                    if let Some(m) = last {
                        m.steps.push(t.clone());
                    }
                }
                Out::Tool(t) => {
                    crate::log!("araç: {t}");
                    // Not araçları dökümde görünmez (sohbet düz konuşma kalsın), bilgisayardakiler görünür.
                    let quiet = ["search_notes", "read_note", "write_note", "append_daily", "link_notes"].iter().any(|p| t.starts_with(p));
                    if !quiet {
                        self.status = t.clone();
                        if let Some(m) = last {
                            m.steps.push(t.clone());
                        }
                    }
                }
                Out::Error(e) => {
                    if let Some(m) = last {
                        m.error = Some(e.clone());
                    }
                }
                Out::Done => {
                    self.busy = false;
                    self.status.clear();
                    self.ask = None;
                    self.reply.lock().unwrap().take();
                }
                Out::Changed(_) | Out::Computer => {}
            }
            self.rev += 1;
            outs.push(o);
        }
        outs
    }
}
