//! Düzenlenebilir yazı alanı: imleç, seçim (klavye ve fare), kelime atlama, pano (Ctrl+A/C/X/V),
//! geri al / yinele, tek ya da çok satır (otomatik büyür, sonra kayar), maskeli (API anahtarı).
//! Düzenleme çekirdeği Gfx'siz çalışır (testler `key`/`char` ile sürer); çizim ve fare
//! DirectWrite düzeninden konum bulur. Konumlar metinde bayt, DirectWrite'ta UTF-16 sırasıdır.
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use windows::Win32::Graphics::DirectWrite::*;
use windows::core::BOOL;

use crate::gfx::{Gfx, Rect};
use crate::ui::{accent, pal};

/// Yatay iç boşluk.
const PAD_X: f32 = 11.0;
/// Çok satırlı alanın üst/alt boşluğu.
const PAD_Y: f32 = 9.0;
/// Tek satır: yükseklik = satır yüksekliği + bu.
const SINGLE_PAD: f32 = 16.0;
const RADIUS: f32 = 6.0;
const UNDO_MAX: usize = 200;

const VK_BACK: u16 = 0x08;
const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0D;
const VK_ESCAPE: u16 = 0x1B;
const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_INSERT: u16 = 0x2D;
const VK_DELETE: u16 = 0x2E;

// ---------------------------------------------------------------------------------------------
// Değiştirici tuşlar ve pano: testlerde taklit edilir.

#[derive(Clone, Copy, Default, Debug)]
struct Mods {
    ctrl: bool,
    shift: bool,
    alt: bool,
}

#[cfg(test)]
thread_local! {
    static TEST_MODS: Cell<Mods> = Cell::new(Mods::default());
    static TEST_CLIP: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn mods() -> Mods {
    #[cfg(test)]
    return TEST_MODS.with(Cell::get);
    #[cfg(not(test))]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
        let down = |k: u16| unsafe { GetKeyState(k as i32) } < 0;
        Mods { ctrl: down(VK_CONTROL.0), shift: down(VK_SHIFT.0), alt: down(VK_MENU.0) }
    }
}

fn clip_get() -> Option<String> {
    #[cfg(test)]
    return TEST_CLIP.with(|c| c.borrow().clone());
    #[cfg(not(test))]
    crate::util::clipboard_text()
}

fn clip_set(s: &str) {
    #[cfg(test)]
    TEST_CLIP.with(|c| *c.borrow_mut() = Some(s.to_string()));
    #[cfg(not(test))]
    set_clipboard_text(s);
}

/// Panoya metin koyar (CF_UNICODETEXT, satır sonları CRLF). Pano başkasındaysa birkaç kez dener.
pub fn set_clipboard_text(s: &str) -> bool {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
    let w: Vec<u16> = s.replace("\r\n", "\n").replace('\n', "\r\n").encode_utf16().chain(Some(0)).collect();
    unsafe {
        // Sahipsiz (NULL) açılan panoya SetClipboardData yazamayabilir: etkin pencere sahip olur.
        let owner = GetActiveWindow();
        let owner = (!owner.is_invalid()).then_some(owner);
        let mut open = false;
        for _ in 0..5 {
            if OpenClipboard(owner).is_ok() {
                open = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !open {
            return false;
        }
        let ok = (|| {
            EmptyClipboard().ok()?;
            let h = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2).ok()?;
            let p = GlobalLock(h) as *mut u16;
            if p.is_null() {
                let _ = GlobalFree(Some(h));
                return None;
            }
            std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
            let _ = GlobalUnlock(h);
            // Başarılıysa bellek artık panonun; başarısızsa bizim.
            if SetClipboardData(13, Some(HANDLE(h.0))).is_err() {
                let _ = GlobalFree(Some(h));
                return None;
            }
            Some(())
        })()
        .is_some();
        let _ = CloseClipboard();
        ok
    }
}

// ---------------------------------------------------------------------------------------------
// Metin yardımcıları (Gfx'siz).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    Space,
    Newline,
    Word,
    Punct,
}

/// Kelime sınıfı: harf/rakam (ğüşıöçİ dahil, Unicode) ve `_` kelime, boşluk, satır sonu, diğerleri
/// noktalama.
fn class(c: char) -> Class {
    match c {
        '\n' => Class::Newline,
        c if c.is_whitespace() => Class::Space,
        c if c.is_alphanumeric() || c == '_' => Class::Word,
        _ => Class::Punct,
    }
}

/// Önceki karaktere yapışan karakterler: birleşen işaretler, değişken seçiciler, ten rengi, ZWJ.
/// Ok tuşları bunların arasında durmaz (emoji ve aksanlı harfler tek parça gibi davranır).
fn attaches(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF
        | 0x200D | 0xFE00..=0xFE0F | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F | 0xE0100..=0xE01EF)
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn char_before(s: &str, i: usize) -> Option<char> {
    s[..i].chars().next_back()
}

fn char_at(s: &str, i: usize) -> Option<char> {
    s[i..].chars().next()
}

/// Sonraki karakter sınırı (yapışan karakterler atlanır).
fn next_pos(s: &str, i: usize) -> usize {
    let Some(c) = char_at(s, i) else { return i };
    let mut j = i + c.len_utf8();
    let mut prev = c;
    while let Some(n) = char_at(s, j) {
        if attaches(n) || prev == '\u{200D}' {
            j += n.len_utf8();
            prev = n;
        } else {
            break;
        }
    }
    j
}

/// Önceki karakter sınırı (yapışan karakterler atlanır).
fn prev_pos(s: &str, i: usize) -> usize {
    let mut j = i;
    while let Some(c) = char_before(s, j) {
        j -= c.len_utf8();
        let joined = char_before(s, j) == Some('\u{200D}');
        if !attaches(c) && !joined {
            break;
        }
    }
    j
}

/// Ctrl+Sol: boşlukları geç, sonra aynı sınıftaki karakterleri. Satır sonu tek adım.
fn word_left(s: &str, i: usize) -> usize {
    let mut j = i;
    while let Some(c) = char_before(s, j).filter(|&c| class(c) == Class::Space) {
        j -= c.len_utf8();
    }
    match char_before(s, j) {
        None => j,
        Some('\n') => {
            if j < i {
                j
            } else {
                j - 1
            }
        }
        Some(c) => {
            let k = class(c);
            while let Some(c) = char_before(s, j).filter(|&c| class(c) == k) {
                j -= c.len_utf8();
            }
            j
        }
    }
}

/// Ctrl+Sağ (Windows usulü): bulunduğu kelimeyi, sonra boşlukları geç. Satır sonu tek adım.
fn word_right(s: &str, i: usize) -> usize {
    let mut j = i;
    match char_at(s, j) {
        None => return j,
        Some('\n') => return j + 1,
        Some(c) if class(c) != Class::Space => {
            let k = class(c);
            while let Some(c) = char_at(s, j).filter(|&c| class(c) == k) {
                j += c.len_utf8();
            }
        }
        _ => {}
    }
    while let Some(c) = char_at(s, j).filter(|&c| class(c) == Class::Space) {
        j += c.len_utf8();
    }
    j
}

/// Çift tıklamada seçilecek kelime: konumdaki (ya da hemen önceki) karakterin sınıfından aralık.
fn word_at(s: &str, i: usize) -> (usize, usize) {
    let pick = match (char_at(s, i), char_before(s, i)) {
        (Some(c), Some(p)) if class(c) != Class::Word && class(p) == Class::Word => p,
        (Some(c), _) if c != '\n' => c,
        (_, Some(p)) if p != '\n' => p,
        _ => return (i, i),
    };
    let k = class(pick);
    let (mut a, mut b) = (i, i);
    if char_at(s, i) != Some(pick) {
        a -= pick.len_utf8();
        b = i;
    }
    while let Some(c) = char_before(s, a).filter(|&c| class(c) == k) {
        a -= c.len_utf8();
    }
    while let Some(c) = char_at(s, b).filter(|&c| class(c) == k) {
        b += c.len_utf8();
    }
    (a, b)
}

/// Mantıksal satırın (iki `\n` arası) sınırları; satır sonu dahil değil.
fn line_at(s: &str, i: usize) -> (usize, usize) {
    let a = s[..i].rfind('\n').map_or(0, |p| p + 1);
    let b = s[i..].find('\n').map_or(s.len(), |p| i + p);
    (a, b)
}

/// Bayt konumu → gösterilen metindeki UTF-16 konumu (maskede her karakter bir nokta).
fn to_u16(s: &str, mask: bool, i: usize) -> u32 {
    let t = &s[..floor_boundary(s, i)];
    if mask { t.chars().count() as u32 } else { t.encode_utf16().count() as u32 }
}

/// UTF-16 konumu → bayt konumu; vekil çiftin ortası sonraki karaktere yuvarlanır.
fn from_u16(s: &str, mask: bool, u: u32) -> usize {
    let mut n = 0u32;
    for (b, c) in s.char_indices() {
        if n >= u {
            return b;
        }
        n += if mask { 1 } else { c.len_utf16() as u32 };
    }
    s.len()
}

// ---------------------------------------------------------------------------------------------
// Düzen önbelleği.

#[derive(Clone, Copy, Debug)]
struct Line {
    start: u32,
    len: u32,
    /// Satır sonu karakterinin uzunluğu (yumuşak kırılmada 0).
    nl: u32,
    ws: u32,
    top: f32,
    h: f32,
}

struct Cache {
    shown: String,
    font: u8,
    width: f32,
    layout: IDWriteTextLayout,
    lines: Vec<Line>,
    /// İçeriğin genişliği (sondaki boşluklar dahil) ve yüksekliği.
    w: f32,
    h: f32,
}

impl Cache {
    fn lh(&self) -> f32 {
        self.lines.first().map_or(18.0, |l| l.h)
    }

    /// Konumun bulunduğu görsel satır. Yumuşak kırılma noktası sonraki satırın başıdır.
    fn line_of(&self, p: u32) -> usize {
        self.lines.iter().position(|l| p < l.start + l.len).unwrap_or(self.lines.len().saturating_sub(1))
    }

    /// Satırdaki en büyük imleç konumu (satır sonundan ve kırılmadaki boşluktan önce).
    fn line_max(&self, i: usize) -> u32 {
        let l = self.lines[i];
        let end = l.start + l.len - l.nl;
        if l.nl == 0 && i + 1 < self.lines.len() && l.ws > 0 { end - 1 } else { end }
    }

    /// İmlecin düzendeki yeri: (x, satır üstü, satır yüksekliği).
    fn caret(&self, p: u32) -> (f32, f32, f32) {
        if self.lines.is_empty() {
            return (0.0, 0.0, 18.0);
        }
        let l = self.lines[self.line_of(p)];
        let (mut x, mut y) = (0.0f32, 0.0f32);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            if p > l.start && p < l.start + l.len - l.nl {
                let _ = self.layout.HitTestTextPosition(p, false, &mut x, &mut y, &mut m);
            } else if p > l.start {
                // Satır sonunda: önceki karakterin arka kenarı (son satırın sonu da böyle güvenli).
                let _ = self.layout.HitTestTextPosition(p - 1, true, &mut x, &mut y, &mut m);
            }
        }
        (x, l.top, l.h)
    }

    /// `i` satırında `x`'e en yakın imleç konumu.
    fn hit_line(&self, i: usize, x: f32) -> u32 {
        let l = self.lines[i];
        let (mut trailing, mut inside) = (BOOL(0), BOOL(0));
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        let p = unsafe {
            match self.layout.HitTestPoint(x, l.top + l.h / 2.0, &mut trailing, &mut inside, &mut m) {
                Ok(()) => m.textPosition + if trailing.as_bool() { m.length } else { 0 },
                Err(_) => l.start,
            }
        };
        p.clamp(l.start, self.line_max(i))
    }

    /// Düzendeki noktaya en yakın imleç konumu.
    fn hit(&self, x: f32, y: f32) -> u32 {
        if self.lines.is_empty() {
            return 0;
        }
        let i = self.lines.iter().position(|l| y < l.top + l.h).unwrap_or(self.lines.len() - 1);
        self.hit_line(i, x)
    }

    /// Aralığın seçim dikdörtgenleri (düzen koordinatında); seçili satır sonları küçük bir kutu.
    fn range_rects(&self, a: u32, b: u32) -> Vec<Rect> {
        let mut out = Vec::new();
        if b <= a {
            return out;
        }
        unsafe {
            let mut n = 0u32;
            let _ = self.layout.HitTestTextRange(a, b - a, 0.0, 0.0, None, &mut n);
            let mut v = vec![DWRITE_HIT_TEST_METRICS::default(); n.max(1) as usize];
            if self.layout.HitTestTextRange(a, b - a, 0.0, 0.0, Some(&mut v), &mut n).is_ok() {
                for m in &v[..(n as usize).min(v.len())] {
                    out.push(Rect::new(m.left, m.top, m.left + m.width, m.top + m.height));
                }
            }
        }
        for l in &self.lines {
            let nl = l.start + l.len - l.nl;
            if l.nl > 0 && nl >= a && nl < b {
                let (x, t, h) = self.caret(nl);
                out.push(Rect::new(x, t, x + 5.0, t + h));
            }
        }
        out
    }
}

/// Geri alma adımı.
#[derive(Clone)]
struct Snap {
    text: String,
    cursor: usize,
    anchor: usize,
}

/// Son düzenlemenin türü: ardışık yazma / silme tek adımda birleşir.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    None,
    Type,
    Back,
    Del,
    Other,
}

/// Fare sürüklemesinin birimi (tek, çift, üç tık).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Unit {
    Char,
    Word,
    Line,
}

/// Çizim yerleşimi: düzen, metnin çizim başlangıcı (kaydırma dahil) ve görünür alan.
struct Geo {
    c: Rc<Cache>,
    ox: f32,
    oy: f32,
    view: Rect,
}

/// Yazı alanı. `text` okunabilir; dışarıdan değiştirmek için `set_text`/`clear` kullan (doğrudan
/// atanırsa bir sonraki olayda fark edilir: imleç sona gider, geri alma geçmişi silinir).
///
/// Tuşlar: oklar (+Ctrl kelime, +Shift seçim), Home/End (+Ctrl baş/son), PageUp/PageDown,
/// Backspace/Delete (+Ctrl kelime), Ctrl+A/C/X/V/Z/Y, Ctrl+Shift+Z, Shift+Delete, Ctrl/Shift+Insert,
/// Esc odağı bırakır. Enter: tek satırda ve çok satırda gönderir (`key` → `true`), Shift+Enter çok
/// satırda yeni satır. `enter_newline`: Enter yeni satır, Ctrl+Enter / Ctrl+S gönderir, Tab dört
/// boşluk. Diğer durumlarda Tab kullanılmaz (`false`, sayfa karar verir).
pub struct TextField {
    pub text: String,
    pub focused: bool,
    /// Gizli yazı (noktalar); kopyalama ve kesme kapalı.
    pub mask: bool,
    pub placeholder: String,
    pub multiline: bool,
    pub enter_newline: bool,
    /// Eş aralıklı yazı tipi.
    pub mono: bool,
    /// Okuma yazı tipi (`g.f.read`, 15 px, geniş satır aralığı; `mono` ile `read_mono`): büyük
    /// not düzenleyicisi için.
    pub reading: bool,
    /// Çok satırlı alan bu kadar satıra kadar büyür, sonra kayar.
    pub max_lines: usize,
    cursor: usize,
    anchor: usize,
    /// Son bilinen metin: `text` dışarıdan değişti mi.
    shadow: String,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    last: Op,
    /// Yukarı/aşağıda korunan sütun: düzende x ya da (düzen yoksa) karakter sütunu.
    pref_x: Option<f32>,
    pref_col: Option<usize>,
    dragging: bool,
    unit: Unit,
    origin: (usize, usize),
    clicks: u32,
    last_click: (u32, f32, f32),
    scroll_x: Cell<f32>,
    scroll_y: Cell<f32>,
    /// Bir sonraki çizimde imleç görünür yere kaydırılsın.
    follow: Cell<bool>,
    view_lines: Cell<usize>,
    cache: RefCell<Option<Rc<Cache>>>,
}

impl TextField {
    pub fn new(text: &str, placeholder: &str) -> Self {
        TextField {
            text: text.to_string(),
            focused: false,
            mask: false,
            placeholder: placeholder.to_string(),
            multiline: false,
            enter_newline: false,
            mono: false,
            reading: false,
            max_lines: 6,
            cursor: text.len(),
            anchor: text.len(),
            shadow: text.to_string(),
            undo: Vec::new(),
            redo: Vec::new(),
            last: Op::None,
            pref_x: None,
            pref_col: None,
            dragging: false,
            unit: Unit::Char,
            origin: (0, 0),
            clicks: 0,
            last_click: (0, 0.0, 0.0),
            scroll_x: Cell::new(0.0),
            scroll_y: Cell::new(0.0),
            follow: Cell::new(true),
            view_lines: Cell::new(6),
            cache: RefCell::new(None),
        }
    }

    /// Metni değiştirir: imleç sona, seçim ve geri alma geçmişi sıfırlanır.
    pub fn set_text(&mut self, s: &str) {
        self.text = s.to_string();
        self.shadow = self.text.clone();
        self.cursor = self.text.len();
        self.anchor = self.cursor;
        self.undo.clear();
        self.redo.clear();
        self.last = Op::None;
        self.pref_x = None;
        self.pref_col = None;
        self.scroll_x.set(0.0);
        self.scroll_y.set(0.0);
        self.follow.set(true);
    }

    pub fn clear(&mut self) {
        self.set_text("");
    }

    pub fn select_all(&mut self) {
        self.sync();
        self.anchor = 0;
        self.cursor = self.text.len();
        self.last = Op::None;
        self.follow.set(true);
    }

    pub fn has_selection(&self) -> bool {
        let (a, b) = self.sel();
        a != b
    }

    pub fn selected_text(&self) -> String {
        let (a, b) = self.sel();
        self.text[a..b].to_string()
    }

    /// Sürükleme sürüyor mu.
    pub fn dragging(&self) -> bool {
        self.dragging
    }

    /// Seçim (küçük, büyük), char sınırına ve metne kırpılmış.
    fn sel(&self) -> (usize, usize) {
        let c = floor_boundary(&self.text, self.cursor);
        let a = floor_boundary(&self.text, self.anchor);
        (c.min(a), c.max(a))
    }

    /// `text` dışarıdan değiştiyse durumu sıfırlar; konumları char sınırına kırpar.
    fn sync(&mut self) {
        if self.text != self.shadow {
            self.shadow = self.text.clone();
            self.cursor = self.text.len();
            self.anchor = self.cursor;
            self.undo.clear();
            self.redo.clear();
            self.last = Op::None;
        }
        self.cursor = floor_boundary(&self.text, self.cursor);
        self.anchor = floor_boundary(&self.text, self.anchor);
    }

    /// Gösterilen metin (maskeliyse her karakter bir nokta).
    fn shown(&self) -> String {
        if self.mask { "•".repeat(self.text.chars().count()) } else { self.text.clone() }
    }

    fn move_to(&mut self, p: usize, extend: bool) {
        self.cursor = floor_boundary(&self.text, p);
        if !extend {
            self.anchor = self.cursor;
        }
        self.last = Op::None;
        self.follow.set(true);
    }

    fn snap(&self) -> Snap {
        Snap { text: self.text.clone(), cursor: self.cursor, anchor: self.anchor }
    }

    /// `a..b`'yi `ins` ile değiştirir; imleç eklenenin sonuna. Ardışık aynı tür düzenleme tek adım
    /// (seçim değiştiren düzenleme yeni adım açar, sonraki yazılar ona katılır).
    fn edit(&mut self, a: usize, b: usize, ins: &str, op: Op) {
        if a == b && ins.is_empty() {
            return;
        }
        let merge = op != Op::Other && op == self.last && self.cursor == self.anchor;
        if !merge {
            let s = self.snap();
            self.undo.push(s);
            if self.undo.len() > UNDO_MAX {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.text.replace_range(a..b, ins);
        self.shadow = self.text.clone();
        self.cursor = a + ins.len();
        self.anchor = self.cursor;
        self.last = op;
        self.pref_x = None;
        self.pref_col = None;
        self.follow.set(true);
    }

    /// Seçimi `s` ile değiştirir.
    fn insert(&mut self, s: &str, op: Op) {
        let (a, b) = self.sel();
        self.edit(a, b, s, op);
    }

    /// Seçim varsa onu, yoksa `a..b`'yi siler.
    fn delete(&mut self, a: usize, b: usize, op: Op) {
        let (s, e) = self.sel();
        if s != e {
            self.edit(s, e, "", Op::Other);
        } else {
            self.edit(a.min(b), a.max(b), "", op);
        }
    }

    fn restore(&mut self, s: Snap) {
        self.text = s.text;
        self.shadow = self.text.clone();
        self.cursor = s.cursor;
        self.anchor = s.anchor;
        self.last = Op::None;
        self.follow.set(true);
    }

    fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            let cur = self.snap();
            self.redo.push(cur);
            self.restore(s);
        }
    }

    fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            let cur = self.snap();
            self.undo.push(cur);
            self.restore(s);
        }
    }

    fn copy(&self) {
        if !self.mask && self.has_selection() {
            clip_set(&self.selected_text());
        }
    }

    /// Yapıştırma: `\r` atılır; tek satırda uçtaki satır sonları kırpılır, içtekiler boşluk olur.
    fn paste(&mut self) {
        let Some(s) = clip_get() else { return };
        let mut s = s.replace("\r\n", "\n").replace('\r', "\n");
        s.retain(|c| !c.is_control() || c == '\n' || c == '\t');
        if !self.multiline {
            s = s.trim_matches('\n').replace(['\n', '\t'], " ");
        }
        if !s.is_empty() {
            self.insert(&s, Op::Other);
        }
    }

    fn word_left(&self, i: usize) -> usize {
        if self.mask { 0 } else { word_left(&self.text, i) }
    }

    fn word_right(&self, i: usize) -> usize {
        if self.mask { self.text.len() } else { word_right(&self.text, i) }
    }

    /// Son çizimin düzeni hâlâ geçerliyse.
    fn live_cache(&self) -> Option<Rc<Cache>> {
        let c = self.cache.borrow().clone()?;
        (c.font == self.font_id() && c.shown == self.shown() && !c.lines.is_empty()).then_some(c)
    }

    /// Görsel satırın (düzen yoksa mantıksal satırın) sınırları.
    fn row_bounds(&self, i: usize) -> (usize, usize) {
        if self.multiline
            && let Some(c) = self.live_cache()
        {
            let k = c.line_of(to_u16(&self.text, self.mask, i));
            let s = from_u16(&self.text, self.mask, c.lines[k].start);
            let e = from_u16(&self.text, self.mask, c.line_max(k));
            return (s, e);
        }
        if self.multiline { line_at(&self.text, i) } else { (0, self.text.len()) }
    }

    /// İmleci `n` satır yukarı (eksi) ya da aşağı taşır; sütun korunur. Düzen yoksa (çizilmeden
    /// önce, testlerde) mantıksal satırlar ve karakter sütunu kullanılır.
    fn vertical(&mut self, n: i32, extend: bool) {
        let cur = self.cursor;
        let p = if let Some(c) = self.live_cache() {
            let p16 = to_u16(&self.text, self.mask, cur);
            let i = c.line_of(p16) as i32;
            let x = *self.pref_x.get_or_insert_with(|| c.caret(p16).0);
            let t = i + n;
            if t < 0 {
                0
            } else if t >= c.lines.len() as i32 {
                self.text.len()
            } else {
                from_u16(&self.text, self.mask, c.hit_line(t as usize, x))
            }
        } else {
            let (mut s, _) = line_at(&self.text, cur);
            let col = *self.pref_col.get_or_insert_with(|| self.text[s..cur].chars().count());
            let mut k = n;
            let mut edge = None;
            while k < 0 {
                if s == 0 {
                    edge = Some(0);
                    break;
                }
                s = line_at(&self.text, s - 1).0;
                k += 1;
            }
            while k > 0 {
                let (_, e) = line_at(&self.text, s);
                if e >= self.text.len() {
                    edge = Some(self.text.len());
                    break;
                }
                s = e + 1;
                k -= 1;
            }
            edge.unwrap_or_else(|| {
                let (_, e) = line_at(&self.text, s);
                self.text[s..e].char_indices().nth(col).map_or(e, |(b, _)| s + b)
            })
        };
        self.move_to(p, extend);
    }

    /// Tuş (WM_KEYDOWN). Gönderme (Enter vb.) istenirse `true`; Esc odağı bırakır (`focused`
    /// sonra `false` olur). Odakta değilse hiçbir şey yapmaz.
    pub fn key(&mut self, vk: u16) -> bool {
        if !self.focused {
            return false;
        }
        self.sync();
        let m = mods();
        // AltGr = Ctrl+Alt: kısayol değil, yazı.
        let ctrl = m.ctrl && !m.alt;
        let shift = m.shift;
        if !matches!(vk, VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT) {
            self.pref_x = None;
            self.pref_col = None;
        }
        let (a, b) = self.sel();
        let cur = self.cursor;
        let len = self.text.len();
        match vk {
            VK_LEFT => {
                let p = if ctrl {
                    self.word_left(cur)
                } else if a != b && !shift {
                    a
                } else {
                    prev_pos(&self.text, cur)
                };
                self.move_to(p, shift);
            }
            VK_RIGHT => {
                let p = if ctrl {
                    self.word_right(cur)
                } else if a != b && !shift {
                    b
                } else {
                    next_pos(&self.text, cur)
                };
                self.move_to(p, shift);
            }
            VK_HOME => {
                let p = if ctrl { 0 } else { self.row_bounds(cur).0 };
                self.move_to(p, shift);
            }
            VK_END => {
                let p = if ctrl { len } else { self.row_bounds(cur).1 };
                self.move_to(p, shift);
            }
            VK_UP | VK_DOWN if !self.multiline => self.move_to(if vk == VK_UP { 0 } else { len }, shift),
            VK_UP => self.vertical(-1, shift),
            VK_DOWN => self.vertical(1, shift),
            VK_PRIOR | VK_NEXT if self.multiline => {
                let n = self.view_lines.get().max(1) as i32;
                self.vertical(if vk == VK_PRIOR { -n } else { n }, shift);
            }
            VK_BACK => {
                let p = if ctrl { self.word_left(cur) } else { prev_pos(&self.text, cur) };
                self.delete(p, cur, if ctrl { Op::Other } else { Op::Back });
            }
            VK_DELETE if shift && !ctrl => {
                if !self.mask && a != b {
                    self.copy();
                    self.delete(a, b, Op::Other);
                }
            }
            VK_DELETE => {
                let p = if ctrl { self.word_right(cur) } else { next_pos(&self.text, cur) };
                self.delete(cur, p, if ctrl { Op::Other } else { Op::Del });
            }
            VK_INSERT if ctrl && !shift => self.copy(),
            VK_INSERT if shift && !ctrl => self.paste(),
            VK_RETURN => {
                if self.enter_newline {
                    if ctrl {
                        return true;
                    }
                    self.insert("\n", Op::Other);
                } else if self.multiline && shift && !ctrl {
                    self.insert("\n", Op::Other);
                } else {
                    return true;
                }
            }
            VK_TAB if self.enter_newline && !ctrl && !shift => self.insert("    ", Op::Other),
            VK_ESCAPE => {
                self.focused = false;
                self.dragging = false;
            }
            0x41 if ctrl => self.select_all(),
            0x43 if ctrl => self.copy(),
            0x58 if ctrl => {
                if !self.mask && a != b {
                    self.copy();
                    self.delete(a, b, Op::Other);
                }
            }
            0x56 if ctrl => self.paste(),
            0x5A if ctrl && shift => self.redo(),
            0x5A if ctrl => self.undo(),
            0x59 if ctrl => self.redo(),
            0x53 if ctrl && self.enter_newline => return true,
            _ => {}
        }
        false
    }

    /// Yazılan karakter (WM_CHAR); seçimi değiştirir. Denetim karakterleri (Enter, Backspace,
    /// Ctrl+harf kodları) `key`'de işlenir, burada yalnızca yutulur. Kullandıysa `true`.
    pub fn char(&mut self, c: char) -> bool {
        if !self.focused {
            return false;
        }
        if c.is_control() {
            return true;
        }
        self.sync();
        let mut buf = [0u8; 4];
        self.insert(c.encode_utf8(&mut buf), Op::Type);
        true
    }

    // -----------------------------------------------------------------------------------------
    // Çizim ve fare.

    fn font_id(&self) -> u8 {
        match (self.reading, self.mono) {
            (true, true) => 4,
            (true, false) => 3,
            (false, true) => 2,
            _ if self.multiline => 1,
            _ => 0,
        }
    }

    /// Tek satır `text` (13 px), çok satır `body` (14 px), `mono` Cascadia Mono, `reading` okuma
    /// yazısı.
    fn format<'a>(&self, g: &'a Gfx) -> &'a IDWriteTextFormat {
        match self.font_id() {
            4 => &g.f.read_mono,
            3 => &g.f.read,
            2 => &g.f.mono,
            1 => &g.f.body,
            _ => &g.f.text,
        }
    }

    /// `width` iç genişlikte düzen (önbellekten).
    fn cache_for(&self, g: &Gfx, width: f32) -> Option<Rc<Cache>> {
        let shown = self.shown();
        let font = self.font_id();
        let width = if self.multiline { width.max(1.0).floor() } else { 0.0 };
        if let Some(c) = self.cache.borrow().as_ref()
            && c.font == font
            && c.width == width
            && c.shown == shown
        {
            return Some(c.clone());
        }
        let layout = g.layout(&shown, self.format(g), if self.multiline { width } else { 100_000.0 })?;
        let mut lines = Vec::new();
        let mut tm = DWRITE_TEXT_METRICS::default();
        unsafe {
            if !self.multiline {
                let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            }
            let mut n = 0u32;
            let _ = layout.GetLineMetrics(None, &mut n);
            let mut v = vec![DWRITE_LINE_METRICS::default(); n.max(1) as usize];
            if layout.GetLineMetrics(Some(&mut v), &mut n).is_ok() {
                let (mut start, mut top) = (0u32, 0.0f32);
                for m in &v[..(n as usize).min(v.len())] {
                    lines.push(Line { start, len: m.length, nl: m.newlineLength, ws: m.trailingWhitespaceLength, top, h: m.height });
                    start += m.length;
                    top += m.height;
                }
            }
            let _ = layout.GetMetrics(&mut tm);
        }
        if lines.is_empty() {
            return None;
        }
        let h = lines.iter().map(|l| l.h).sum();
        let c = Rc::new(Cache { shown, font, width, layout, lines, w: tm.widthIncludingTrailingWhitespace, h });
        *self.cache.borrow_mut() = Some(c.clone());
        Some(c)
    }

    /// Yerleşim; kaydırma sınırlara çekilir, `follow` ise imleç görünür yere kaydırılır.
    fn geo(&self, g: &Gfx, r: Rect, follow: bool) -> Option<Geo> {
        let (l, rr) = (r.l + PAD_X, r.r - PAD_X);
        let c = self.cache_for(g, rr - l)?;
        let lh = c.lh();
        let caret = || c.caret(to_u16(&self.text, self.mask, self.cursor));
        if !self.multiline {
            let view = Rect::new(l, r.t, rr, r.b);
            let mut sx = self.scroll_x.get();
            if follow {
                let (x, _, _) = caret();
                if x - sx > view.w() - 2.0 {
                    sx = x - view.w() + 2.0;
                }
                if x < sx {
                    sx = (x - view.w() / 3.0).max(0.0);
                }
            }
            sx = sx.clamp(0.0, (c.w + 2.0 - view.w()).max(0.0));
            self.scroll_x.set(sx);
            self.scroll_y.set(0.0);
            let oy = r.cy() - lh / 2.0;
            return Some(Geo { ox: l - sx, oy, view, c });
        }
        // Tek satırlık yükseklikteyse dikeyde ortalı, değilse üstten.
        let centered = r.h() < 2.0 * lh + 2.0 * PAD_Y;
        let top = if centered { r.cy() - lh / 2.0 } else { r.t + PAD_Y };
        let view = Rect::new(l, top, rr, if centered { top + lh } else { r.b - PAD_Y });
        self.view_lines.set(((view.h() / lh).floor() as usize).max(1));
        let mut sy = self.scroll_y.get();
        if follow {
            let (_, t, h) = caret();
            if t + h > sy + view.h() {
                sy = t + h - view.h();
            }
            if t < sy {
                sy = t;
            }
        }
        sy = sy.clamp(0.0, (c.h - view.h()).max(0.0));
        self.scroll_y.set(sy);
        self.scroll_x.set(0.0);
        Some(Geo { ox: l, oy: top - sy, view, c })
    }

    /// `r` içinde çizer: zemin ve kenarlık, seçim, yazı (ya da yer tutucu), imleç (yanıp sönmez).
    pub fn draw(&self, g: &Gfx, r: Rect, hovered: bool) {
        let (fill, line) = match () {
            _ if self.focused => (pal().layer, accent()),
            _ if hovered => (pal().hover.alpha(1.4), pal().faint),
            _ => (pal().hover, pal().line),
        };
        g.fill(r, RADIUS, fill);
        g.stroke(r, RADIUS, line, 1.0);
        let follow = self.follow.replace(false) && self.focused;
        let Some(geo) = self.geo(g, r, follow) else { return };
        let c = &geo.c;
        let clip = if self.multiline {
            Rect::new(r.l + 1.0, r.t + 1.0, r.r - 1.0, r.b - 1.0)
        } else {
            Rect::new(geo.view.l - 1.0, r.t + 1.0, geo.view.r + 1.0, r.b - 1.0)
        };
        g.clip(clip, || {
            let (a, b) = self.sel();
            if self.focused && a != b {
                let (a, b) = (to_u16(&self.text, self.mask, a), to_u16(&self.text, self.mask, b));
                for s in c.range_rects(a, b) {
                    let s = Rect::new(geo.ox + s.l, geo.oy + s.t, geo.ox + s.r, geo.oy + s.b);
                    g.fill(s, 0.0, accent().alpha(0.35));
                }
            }
            if self.text.is_empty() {
                if !self.placeholder.is_empty()
                    && let Some(p) = g.layout(&self.placeholder, self.format(g), geo.view.w())
                {
                    unsafe {
                        let _ = p.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                    }
                    g.draw_layout(&p, geo.ox, geo.oy, pal().faint, &[]);
                }
            } else {
                g.draw_layout(&c.layout, geo.ox, geo.oy, pal().text, &[]);
            }
            if self.focused {
                let (x, t, h) = c.caret(to_u16(&self.text, self.mask, self.cursor));
                let x = geo.ox + x;
                g.fill(Rect::new(x - 0.75, geo.oy + t, x + 0.75, geo.oy + t + h), 0.0, pal().text);
            }
        });
        // Taşan çok satırlı alanda ince kaydırma göstergesi.
        if self.multiline && c.h > geo.view.h() + 0.5 {
            let track = Rect::new(r.r - 6.0, r.t + 4.0, r.r - 3.0, r.b - 4.0);
            let th = (track.h() * geo.view.h() / c.h).max(16.0);
            let t = track.t + (track.h() - th) * (self.scroll_y.get() / (c.h - geo.view.h()));
            g.fill(Rect::new(track.l, t, track.r, t + th), 1.5, pal().faint.alpha(0.6));
        }
    }

    /// `width` genişlikte gereken yükseklik: tek satırda sabit (satır + 16), çok satırda
    /// 1..`max_lines` satır + 2×9. Otomatik büyüyen alanın yerleşimi için.
    pub fn height(&self, g: &Gfx, width: f32) -> f32 {
        let Some(c) = self.cache_for(g, width - 2.0 * PAD_X) else {
            return if self.multiline { 18.0 + 2.0 * PAD_Y } else { 34.0 };
        };
        if !self.multiline {
            return (c.lh() + SINGLE_PAD).round();
        }
        let n = c.lines.len().clamp(1, self.max_lines.max(1));
        (c.lines[..n].iter().map(|l| l.h).sum::<f32>() + 2.0 * PAD_Y).ceil()
    }

    /// Noktadaki imleç konumu (bayt).
    fn hit_pos(&self, g: &Gfx, r: Rect, x: f32, y: f32) -> Option<usize> {
        let geo = self.geo(g, r, false)?;
        let p = geo.c.hit(x - geo.ox, y - geo.oy);
        Some(from_u16(&self.text, self.mask, p))
    }

    /// Sürüklemede birimine göre seçimi uzatır.
    fn drag_to(&mut self, p: usize) {
        let (s, e) = match self.unit {
            Unit::Char => {
                self.cursor = p;
                self.follow.set(true);
                return;
            }
            Unit::Word if !self.mask => word_at(&self.text, p),
            Unit::Line if self.multiline => line_at(&self.text, p),
            _ => (0, self.text.len()),
        };
        let (o0, o1) = self.origin;
        if p < o0 {
            self.anchor = o1;
            self.cursor = s;
        } else {
            self.anchor = o0;
            self.cursor = e.max(o1);
        }
        self.follow.set(true);
    }

    /// Tıklama: odaklanır, imleci yerleştirir; Shift ile seçimi uzatır, çift tık kelimeyi, üç tık
    /// satırı (tek satırda hepsini) seçer, sürükleme seçer. `r`: `draw`'a verilen dikdörtgen.
    pub fn mouse_down(&mut self, g: &Gfx, r: Rect, x: f32, y: f32) {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
        use windows::Win32::UI::WindowsAndMessaging::GetMessageTime;
        self.sync();
        self.focused = true;
        self.last = Op::None;
        self.pref_x = None;
        self.pref_col = None;
        let Some(p) = self.hit_pos(g, r, x, y) else { return };
        let now = unsafe { GetMessageTime() } as u32;
        let (t, lx, ly) = self.last_click;
        let shift = mods().shift;
        let repeat = self.clicks > 0
            && now.wrapping_sub(t) <= unsafe { GetDoubleClickTime() }
            && (x - lx).abs() <= 4.0
            && (y - ly).abs() <= 4.0;
        self.clicks = if repeat && !shift { self.clicks % 3 + 1 } else { 1 };
        self.last_click = (now, x, y);
        self.dragging = true;
        match self.clicks {
            _ if shift => {
                self.unit = Unit::Char;
                self.cursor = p;
            }
            1 => {
                self.unit = Unit::Char;
                self.cursor = p;
                self.anchor = p;
            }
            n => {
                self.unit = if n == 2 { Unit::Word } else { Unit::Line };
                let (s, e) = match self.unit {
                    Unit::Word if !self.mask => word_at(&self.text, p),
                    Unit::Line if self.multiline => line_at(&self.text, p),
                    _ => (0, self.text.len()),
                };
                self.origin = (s, e);
                self.anchor = s;
                self.cursor = e;
            }
        }
        self.follow.set(true);
    }

    /// Sürükleme (yalnızca tuş basılıyken; fare alanın dışına da çıkabilir, alan kayar).
    pub fn mouse_move(&mut self, g: &Gfx, r: Rect, x: f32, y: f32) {
        if !self.dragging {
            return;
        }
        self.sync();
        if let Some(p) = self.hit_pos(g, r, x, y) {
            self.drag_to(p);
        }
    }

    pub fn mouse_up(&mut self) {
        self.dragging = false;
    }

    /// Fare tekerleği: taşan çok satırlı alanı kaydırır; kullandıysa `true`.
    pub fn wheel(&mut self, g: &Gfx, r: Rect, delta: f32) -> bool {
        if !self.multiline {
            return false;
        }
        let Some(geo) = self.geo(g, r, false) else { return false };
        let max = geo.c.h - geo.view.h();
        if max <= 0.5 {
            return false;
        }
        let sy = (self.scroll_y.get() - delta * geo.c.lh() * 3.0).clamp(0.0, max);
        self.scroll_y.set(sy);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_mods(ctrl: bool, shift: bool) {
        TEST_MODS.with(|m| m.set(Mods { ctrl, shift, alt: false }));
    }

    fn field(s: &str) -> TextField {
        let mut f = TextField::new(s, "");
        f.focused = true;
        f
    }

    fn typ(f: &mut TextField, s: &str) {
        set_mods(false, false);
        for c in s.chars() {
            f.char(c);
        }
    }

    fn press(f: &mut TextField, vk: u16, ctrl: bool, shift: bool) -> bool {
        set_mods(ctrl, shift);
        let r = f.key(vk);
        set_mods(false, false);
        r
    }

    #[test]
    fn edit_utf16_mapping() {
        let s = "ağı😀b";
        assert_eq!(to_u16(s, false, 0), 0);
        assert_eq!(to_u16(s, false, 1), 1);
        assert_eq!(to_u16(s, false, 3), 2); // ğ 2 bayt
        assert_eq!(to_u16(s, false, 5), 3); // ı 2 bayt
        assert_eq!(to_u16(s, false, 9), 5); // emoji 4 bayt, 2 birim
        assert_eq!(to_u16(s, false, s.len()), 6);
        assert_eq!(from_u16(s, false, 3), 5);
        assert_eq!(from_u16(s, false, 4), 9); // vekil çiftin ortası → sonraki karakter
        assert_eq!(from_u16(s, false, 5), 9);
        assert_eq!(from_u16(s, false, 6), s.len());
        assert_eq!(from_u16(s, false, 99), s.len());
        // Maske: karakter başına bir birim.
        assert_eq!(to_u16(s, true, 9), 4);
        assert_eq!(from_u16(s, true, 4), 9);
        // Char sınırı dışı konum aşağı yuvarlanır.
        assert_eq!(to_u16(s, false, 2), 1);
    }

    #[test]
    fn edit_word_boundaries() {
        let s = "Merhaba dünya, İstanbul'da şık_ı";
        assert_eq!(word_right(s, 0), "Merhaba ".len());
        let p = word_right(s, "Merhaba ".len());
        assert_eq!(&s[..p], "Merhaba dünya");
        let p = word_right(s, p);
        assert_eq!(&s[..p], "Merhaba dünya, ");
        let p = word_right(s, p);
        assert_eq!(&s[..p], "Merhaba dünya, İstanbul");
        assert_eq!(&s[..word_left(s, s.len())], "Merhaba dünya, İstanbul'da ");
        let e = "Merhaba dünya, ".len();
        assert_eq!(&s[..word_left(s, e)], "Merhaba dünya");
        assert_eq!(&s[..word_left(s, "Merhaba dün".len())], "Merhaba ");
        let (a, b) = word_at(s, "Merhaba dü".len());
        assert_eq!(&s[a..b], "dünya");
        let (a, b) = word_at(s, s.len());
        assert_eq!(&s[a..b], "şık_ı");
        let (a, b) = word_at(s, "Merhaba".len());
        assert_eq!(&s[a..b], "Merhaba");
        let t = "ab\ncd";
        assert_eq!(word_right(t, 2), 3);
        assert_eq!(word_left(t, 3), 2);
        assert_eq!(word_left(t, 2), 0);
    }

    #[test]
    fn edit_clusters() {
        let s = "a👍🏽b"; // ten rengi yapışır
        assert_eq!(next_pos(s, 1), 1 + 8);
        assert_eq!(prev_pos(s, 9), 1);
        let z = "x👨\u{200D}💻y"; // ZWJ dizisi
        assert_eq!(next_pos(z, 1), z.len() - 1);
        assert_eq!(prev_pos(z, z.len() - 1), 1);
        let e = "e\u{301}x";
        assert_eq!(next_pos(e, 0), 3);
        assert_eq!(prev_pos(e, 3), 0);
    }

    #[test]
    fn edit_typing_and_undo() {
        let mut f = field("");
        typ(&mut f, "merhaba");
        assert_eq!(f.text, "merhaba");
        press(&mut f, VK_BACK, false, false);
        press(&mut f, VK_BACK, false, false);
        assert_eq!(f.text, "merha");
        // Ardışık yazma tek adım, ardışık silme tek adım.
        press(&mut f, 0x5A, true, false);
        assert_eq!(f.text, "merhaba");
        press(&mut f, 0x5A, true, false);
        assert_eq!(f.text, "");
        press(&mut f, 0x59, true, false);
        assert_eq!(f.text, "merhaba");
        press(&mut f, 0x5A, true, true); // Ctrl+Shift+Z
        assert_eq!(f.text, "merha");
        // Yeni düzenleme yinelemeyi siler.
        typ(&mut f, "!");
        press(&mut f, 0x59, true, false);
        assert_eq!(f.text, "merha!");
        // İmleç hareketi birleşmeyi böler.
        press(&mut f, VK_LEFT, false, false);
        typ(&mut f, "x");
        press(&mut f, VK_END, false, false);
        typ(&mut f, "y");
        assert_eq!(f.text, "merhax!y");
        press(&mut f, 0x5A, true, false);
        assert_eq!(f.text, "merhax!");
        press(&mut f, 0x5A, true, false);
        assert_eq!(f.text, "merha!");
        // Seçimin yerine yazılan kelime tek adım.
        press(&mut f, 0x41, true, false);
        typ(&mut f, "yeni");
        press(&mut f, 0x5A, true, false);
        assert_eq!(f.text, "merha!");
    }

    #[test]
    fn edit_selection_and_clipboard() {
        let mut f = field("ağaç dalı");
        press(&mut f, VK_HOME, false, false);
        press(&mut f, VK_RIGHT, true, true); // Ctrl+Shift+Sağ
        assert_eq!(f.selected_text(), "ağaç ");
        press(&mut f, 0x43, true, false);
        assert_eq!(clip_get().as_deref(), Some("ağaç "));
        press(&mut f, 0x58, true, false);
        assert_eq!(f.text, "dalı");
        press(&mut f, VK_END, false, false);
        TEST_CLIP.with(|c| *c.borrow_mut() = Some(" bir\r\niki\n".into()));
        press(&mut f, 0x56, true, false);
        assert_eq!(f.text, "dalı bir iki"); // tek satır: satır sonları boşluk, uçlar kırpılır
        press(&mut f, 0x5A, true, false);
        assert_eq!(f.text, "dalı");
        press(&mut f, 0x5A, true, false);
        assert_eq!(f.text, "ağaç dalı");
        press(&mut f, 0x41, true, false);
        assert!(f.has_selection());
        typ(&mut f, "yeni");
        assert_eq!(f.text, "yeni");
        press(&mut f, VK_LEFT, false, true);
        press(&mut f, VK_LEFT, false, true);
        assert_eq!(f.selected_text(), "ni");
        press(&mut f, VK_LEFT, false, false);
        assert!(!f.has_selection());
        typ(&mut f, "-");
        assert_eq!(f.text, "ye-ni");
    }

    #[test]
    fn edit_mask_blocks_copy() {
        let mut f = field("sk-ant-gizli");
        f.mask = true;
        TEST_CLIP.with(|c| *c.borrow_mut() = Some("önceki".into()));
        press(&mut f, 0x41, true, false);
        press(&mut f, 0x43, true, false);
        press(&mut f, 0x58, true, false);
        press(&mut f, VK_DELETE, false, true);
        assert_eq!(f.text, "sk-ant-gizli");
        assert_eq!(clip_get().as_deref(), Some("önceki"));
        press(&mut f, VK_END, false, false);
        press(&mut f, VK_LEFT, true, false);
        assert_eq!(f.cursor, 0);
    }

    #[test]
    fn edit_word_delete() {
        let mut f = field("bir iki üç");
        press(&mut f, VK_BACK, true, false);
        assert_eq!(f.text, "bir iki ");
        press(&mut f, VK_BACK, true, false);
        assert_eq!(f.text, "bir ");
        press(&mut f, VK_HOME, false, false);
        press(&mut f, VK_DELETE, true, false);
        assert_eq!(f.text, "");
        press(&mut f, 0x5A, true, false);
        assert_eq!(f.text, "bir ");
        press(&mut f, 0x5A, true, false);
        assert_eq!(f.text, "bir iki ");
        let mut f = field("çğ😀");
        press(&mut f, VK_BACK, false, false);
        assert_eq!(f.text, "çğ");
        press(&mut f, VK_HOME, false, false);
        press(&mut f, VK_DELETE, false, false);
        assert_eq!(f.text, "ğ");
    }

    #[test]
    fn edit_enter_modes() {
        let mut f = field("a");
        assert!(press(&mut f, VK_RETURN, false, false));
        f.multiline = true;
        assert!(press(&mut f, VK_RETURN, false, false));
        assert!(!press(&mut f, VK_RETURN, false, true));
        assert_eq!(f.text, "a\n");
        assert!(press(&mut f, VK_RETURN, true, false));
        f.enter_newline = true;
        assert!(!press(&mut f, VK_RETURN, false, false));
        assert_eq!(f.text, "a\n\n");
        assert!(press(&mut f, VK_RETURN, true, false));
        assert!(press(&mut f, 0x53, true, false));
        assert!(!press(&mut f, VK_TAB, false, false));
        assert_eq!(f.text, "a\n\n    ");
        let mut s = field("x");
        assert!(!press(&mut s, VK_TAB, false, false));
        assert_eq!(s.text, "x");
        assert!(press(&mut s, VK_RETURN, false, true));
        press(&mut s, VK_ESCAPE, false, false);
        assert!(!s.focused);
        assert!(!s.char('z'));
        assert!(!s.key(VK_BACK));
        assert_eq!(s.text, "x");
    }

    #[test]
    fn edit_control_chars_swallowed() {
        let mut f = field("ab");
        for c in ['\r', '\n', '\x08', '\x01', '\x03', '\x16', '\x18', '\x1a', '\x7f', '\t'] {
            assert!(f.char(c));
        }
        assert_eq!(f.text, "ab");
        typ(&mut f, "ı😀");
        assert_eq!(f.text, "abı😀");
    }

    #[test]
    fn edit_vertical_fallback() {
        let mut f = field("birinci\nik\nüçüncü satır");
        f.multiline = true;
        press(&mut f, VK_UP, false, false);
        assert_eq!(f.cursor, "birinci\nik".len());
        press(&mut f, VK_UP, false, false);
        assert_eq!(f.cursor, "birinci".len());
        press(&mut f, VK_DOWN, false, false);
        press(&mut f, VK_DOWN, false, false);
        assert_eq!(f.cursor, f.text.len());
        press(&mut f, VK_HOME, false, false);
        assert_eq!(f.cursor, "birinci\nik\n".len());
        press(&mut f, VK_RIGHT, false, false);
        press(&mut f, VK_RIGHT, false, false);
        press(&mut f, VK_UP, false, true);
        assert_eq!(f.selected_text(), "\nüç");
        press(&mut f, VK_UP, false, true);
        press(&mut f, VK_UP, false, true);
        assert_eq!(f.cursor, 0);
        press(&mut f, VK_END, false, false);
        assert_eq!(f.cursor, "birinci".len());
        press(&mut f, VK_NEXT, false, false);
        assert_eq!(f.cursor, f.text.len());
        let mut s = field("abc");
        press(&mut s, VK_UP, false, false);
        assert_eq!(s.cursor, 0);
        press(&mut s, VK_DOWN, false, false);
        assert_eq!(s.cursor, 3);
    }

    #[test]
    fn edit_external_change() {
        let mut f = field("uzun bir metin");
        press(&mut f, 0x41, true, false);
        typ(&mut f, "x");
        f.text = "ç".into(); // dışarıdan
        press(&mut f, VK_LEFT, false, false);
        assert_eq!(f.cursor, 0);
        press(&mut f, 0x5A, true, false); // geçmiş silindi
        assert_eq!(f.text, "ç");
        // Konum char sınırı ortasına düşse de çökmez.
        f.cursor = 1;
        f.anchor = 1;
        typ(&mut f, "a");
        assert_eq!(f.text, "aç");
        assert_eq!(f.selected_text(), "");
        f.set_text("yeni");
        assert_eq!((f.cursor, f.anchor), (4, 4));
        f.clear();
        assert_eq!(f.text, "");
        press(&mut f, VK_BACK, false, false);
        press(&mut f, VK_DELETE, false, false);
        press(&mut f, 0x5A, true, false);
        assert_eq!(f.text, "");
    }

    #[test]
    fn edit_multiline_paste_and_altgr() {
        let mut f = field("");
        f.multiline = true;
        TEST_CLIP.with(|c| *c.borrow_mut() = Some("a\r\nb\rc\x07".into()));
        press(&mut f, 0x56, true, false);
        assert_eq!(f.text, "a\nb\nc");
        // AltGr (Ctrl+Alt) kısayol sayılmaz.
        TEST_MODS.with(|m| m.set(Mods { ctrl: true, shift: false, alt: true }));
        f.key(0x41);
        assert!(!f.has_selection());
        set_mods(false, false);
        assert_eq!(line_at("ab\ncd\nef", 4), (3, 5));
        assert_eq!(line_at("ab\n", 3), (3, 3));
    }
}
