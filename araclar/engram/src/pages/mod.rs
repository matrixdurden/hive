//! Sayfalar ve aralarında paylaşılan durum. Her sayfa `ui::Page`'i uygular; pencere olayları
//! sayfanın kendi köşesine göre iletir. Sayfalar pencereden bir şey isteyince (sekme değiştir,
//! tema, çık, dinle) `State`'e yazar, pencere her mesajdan sonra bakar.

pub mod home;
pub mod notes;
pub mod settings;

use std::cell::RefCell;
use std::rc::Rc;

use windows::Win32::Foundation::HWND;

use crate::page::Tab;
use crate::brain::Brain;
use crate::config::Config;
use crate::notes::Vault;
use crate::ui::Page;

/// Sayfaların sese istekleri (ses pencerede yaşar).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VoiceReq {
    /// Uyandırma sözü söylenmiş gibi dinlemeye başla.
    Listen,
    /// Dinlemeyi, konuşmayı ve süren turu durdur.
    Stop,
    /// Uyandırma sözünü sesimden öğren.
    Train,
    /// Ayarlar değişti: sesi yeniden kur.
    Apply,
}

pub struct State {
    pub config: Config,
    pub vault: Vault,
    pub brain: Brain,
    pub hwnd: HWND,
    /// Sesin durumu ve seviyesi (küre için; pencere günceller).
    pub voice_state: crate::voice::State,
    pub level: f32,
    /// Nottaki ilk uyandırma sözü (gösterim için).
    pub wake_phrase: String,
    /// Öğrenilmiş uyandırma örneği sayısı ve son öğrenme mesajı.
    pub trained: usize,
    pub train_msg: String,
    /// Sayfaların pencereye istekleri.
    pub open_tab: Option<Tab>,
    pub voice_req: Option<VoiceReq>,
    pub redraw: bool,
    /// Hafıza sayfası bu notu açsın (yol ya da başlık).
    pub open_note: Option<String>,
}

impl State {
    pub fn new(config: Config, hwnd: HWND) -> Self {
        let vault = Vault::open(&config.vault);
        State {
            wake_phrase: config.wake_word.clone(),
            config,
            vault,
            brain: Brain::new(hwnd),
            hwnd,
            voice_state: crate::voice::State::Idle,
            level: 0.0,
            trained: 0,
            train_msg: String::new(),
            open_tab: None,
            voice_req: None,
            redraw: false,
            open_note: None,
        }
    }
}

/// Sayfalar `app::Tab::ALL` sırasıyla.
pub fn all(shared: &Rc<RefCell<State>>, hwnd: HWND) -> Vec<Box<dyn Page>> {
    vec![
        Box::new(home::HomePage::new(shared.clone(), hwnd)),
        Box::new(notes::NotesPage::new(shared.clone(), hwnd)),
        Box::new(settings::SettingsPage::new(shared.clone(), hwnd)),
    ]
}
