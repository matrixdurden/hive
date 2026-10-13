//! Hafıza sayfası: solda klasör ağacı ve arama, ortada okunaklı bir sütunda çizilmiş not
//! (başlıklar, listeler, tıklanabilir [[bağlantılar]]), sağda geri bağlantılar. Not burada
//! düzenlenebilir (ham markdown); "Klasörde göster" dosyayı Gezgin'de seçili açar.

use std::cell::RefCell;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;

use crate::page::Shared;
use crate::gfx::{Color, Gfx, Rect};
use crate::notes::md::{self, Kind, Style};
use crate::ui::*;

const TREE_W: f32 = 230.0;
const SIDE_W: f32 = 190.0;
const ROW: f32 = 28.0;
const TOP: f32 = HEADER + 12.0;
/// Notların üstündeki satır: başlık ("Tüm notlar · 9") ve düğmeler (Düzenle, Klasörü aç...).
const BAR_CY: f32 = TOP + 19.0;

const CARD_H: f32 = 132.0;
/// Okuma sütununun en geniş hali: satırlar ~75 karakteri geçmesin.
const READ_W: f32 = 700.0;
const CARD_MIN_W: f32 = 250.0;
const GAP: f32 = 12.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Search,
    /// Ağaçta "Tüm notlar".
    Home,
    /// Genel bakışta kart.
    Card(usize),
    /// Ağaçta not.
    Tree(usize),
    /// Ağaçta klasör (açılır/kapanır).
    Folder(usize),
    /// Gövdede bağlantı: hedef adı.
    Link(usize, usize),
    /// Geri bağlantı listesinde not.
    Back(usize),
    /// Başlık düğmeleri.
    Reveal,
    Edit,
    Save,
    Cancel,
    OpenFolder,
    /// Düzenleyici.
    Editor,
}

/// Çizilmiş bir blok: düzeni, konumu ve bağlantı aralıkları.
struct Item {
    kind: Kind,
    layout: Option<IDWriteTextLayout>,
    y: f32,
    h: f32,
    x: f32,
    /// (utf16 başlangıç, uzunluk, stil, hedef)
    spans: Vec<(u32, u32, Style, String)>,
    number: usize,
}

struct Rendered {
    key: (String, u64, i32),
    items: Vec<Item>,
    total: f32,
}

pub struct NotesPage {
    shared: Shared,
    search: TextField,
    /// Seçili not (vault indeksi).
    current: Option<usize>,
    /// Kapalı klasörler.
    collapsed: Vec<String>,
    tree_scroll: f32,
    scroll: f32,
    rendered: RefCell<Option<Rendered>>,
    hover: Hit,
    pressed: Hit,
    w: f32,
    h: f32,
    /// Son çizimdeki yalnızca simgeli düğmeler ve açıklamaları (ipucu için).
    icon_tips: RefCell<Vec<(Hit, Rect, &'static str)>>,
    /// Düzenlenen not (yol) ve düzenleyici.
    editing: Option<String>,
    editor: TextField,
}

impl NotesPage {
    pub fn new(shared: Shared, _hwnd: HWND) -> Self {
        let mut p = NotesPage {
            shared,
            search: TextField::new("", t!("Search notes", "Notlarda ara")),
            current: None,
            collapsed: Vec::new(),
            tree_scroll: 0.0,
            scroll: 0.0,
            rendered: RefCell::new(None),
            hover: Hit::None,
            pressed: Hit::None,
            w: 0.0,
            h: 0.0,
            icon_tips: RefCell::new(Vec::new()),
            editing: None,
            editor: {
                let mut e = TextField::new("", "");
                e.multiline = true;
                e.enter_newline = true;
                e.reading = true;
                e.max_lines = usize::MAX;
                e
            },
        };
        // Açılışta genel bakış.
        p.current = None;
        p
    }

    fn side_visible(&self) -> bool {
        self.w > 760.0 && self.current.is_some()
    }

    fn note_rect(&self) -> Rect {
        let r = if self.side_visible() { self.w - SIDE_W } else { self.w };
        Rect::new(TREE_W, HEADER, r, self.h)
    }

    /// Okuma sütunu: not alanının ortasında, en çok READ_W. (sol, genişlik)
    fn read_col(&self) -> (f32, f32) {
        let nr = self.note_rect();
        let w = (nr.w() - 2.0 * PAD - 8.0).clamp(80.0, READ_W);
        let l = nr.l + ((nr.w() - w) / 2.0).max(PAD);
        (l, w)
    }

    fn editor_rect(&self) -> Rect {
        let (l, w) = self.read_col();
        Rect::new(l - 14.0, TOP + 44.0, l + w + 14.0, self.h - 16.0)
    }

    fn search_rect(&self) -> Rect {
        Rect::new(PAD, TOP, TREE_W - 12.0, TOP + 32.0)
    }

    fn home_rect(&self) -> Rect {
        Rect::new(PAD - 8.0, TOP + 44.0, TREE_W - 12.0, TOP + 44.0 + ROW)
    }

    /// Genel bakıştaki kartlar: (not, kart) son değişene göre; arama varsa ona göre.
    fn cards(&self) -> Vec<(usize, Rect)> {
        let s = self.shared.borrow();
        let q = self.search.text.trim();
        let mut ids: Vec<usize> = if q.is_empty() {
            let mut v: Vec<usize> = (0..s.vault.notes.len()).filter(|&i| !crate::notes::is_system(&s.vault.notes[i].path)).collect();
            v.sort_by(|a, b| s.vault.notes[*b].mtime.cmp(&s.vault.notes[*a].mtime).then_with(|| s.vault.notes[*a].path.cmp(&s.vault.notes[*b].path)));
            v
        } else {
            s.vault.search(q, 60).into_iter().map(|h| h.0).collect()
        };
        ids.retain(|&i| i < s.vault.notes.len());
        let area = self.note_rect();
        let inner_w = area.w() - 2.0 * PAD;
        let cols = ((inner_w + GAP) / (CARD_MIN_W + GAP)).floor().max(1.0) as usize;
        let cw = (inner_w - (cols as f32 - 1.0) * GAP) / cols as f32;
        let top = TOP + 44.0 - self.scroll;
        ids.into_iter()
            .enumerate()
            .map(|(k, i)| {
                let (c, r) = (k % cols, k / cols);
                let l = area.l + PAD + c as f32 * (cw + GAP);
                let t = top + r as f32 * (CARD_H + GAP);
                (i, Rect::new(l, t, l + cw, t + CARD_H))
            })
            .collect()
    }

    /// Ağaç satırları: (y, klasör mü, klasör adı, not indeksi). İlk satır "Tüm notlar".
    fn tree_rows(&self) -> Vec<(f32, bool, String, usize)> {
        let s = self.shared.borrow();
        let q = self.search.text.trim();
        let mut rows = Vec::new();
        let mut y = TOP + 44.0 + ROW + 6.0 - self.tree_scroll;
        if !q.is_empty() {
            for (i, _, _) in s.vault.search(q, 40) {
                rows.push((y, false, String::new(), i));
                y += ROW;
            }
            return rows;
        }
        for (folder, notes) in s.vault.folders() {
            let name = if folder.is_empty() { t!("(root)", "(kök)").to_string() } else { folder.clone() };
            rows.push((y, true, name, 0));
            y += ROW;
            if self.collapsed.contains(&folder) {
                continue;
            }
            for i in notes {
                rows.push((y, false, String::new(), i));
                y += ROW;
            }
        }
        rows
    }

    fn tree_max_scroll(&self) -> f32 {
        let n = self.tree_rows().len() as f32;
        (TOP + 44.0 + n * ROW + 12.0 - self.h).max(0.0)
    }

    fn open(&mut self, i: usize) {
        self.save();
        self.current = Some(i);
        self.scroll = 0.0;
        self.hover = Hit::None;
    }

    fn open_by_name(&mut self, name: &str) {
        let r = self.shared.borrow().vault.resolve(name);
        if let Some(i) = r {
            self.open(i);
        }
    }

    /// Gövdeyi düzene çeker (not, genişlik ya da içerik değişince).
    fn render(&self, g: &Gfx) {
        let s = self.shared.borrow();
        let Some(i) = self.current.filter(|&i| i < s.vault.notes.len()) else {
            *self.rendered.borrow_mut() = None;
            return;
        };
        let n = &s.vault.notes[i];
        let width = self.read_col().1;
        let key = (n.path.clone(), n.mtime, width as i32);
        if self.rendered.borrow().as_ref().is_some_and(|r| r.key == key) {
            return;
        }
        let mut blocks = md::parse(&n.body);
        // Başlık zaten üstte: notun ilk satırı aynı başlıksa ikinci kez yazılmaz.
        if blocks.first().is_some_and(|b| b.kind == Kind::Heading(1) && crate::notes::key(&b.text()) == crate::notes::key(&n.title)) {
            blocks.remove(0);
        }
        let mut items = Vec::new();
        let mut y = 0.0;
        let mut number = 0;
        for b in blocks {
            // Başlıkların üstünde nefes payı, maddeler sıkı, paragraflar arası ferah.
            let (format, indent, gap) = match b.kind {
                Kind::Heading(1) => (&g.f.read_h1, 0.0, 14.0),
                Kind::Heading(2) => (&g.f.read_h2, 0.0, 10.0),
                Kind::Heading(_) => (&g.f.read_h3, 0.0, 8.0),
                Kind::Bullet(d) | Kind::Task(d, _) => (&g.f.read, 24.0 + d as f32 * 20.0, 4.0),
                Kind::Numbered(d) => (&g.f.read, 28.0 + d as f32 * 20.0, 4.0),
                Kind::Code => (&g.f.read_mono, 14.0, 14.0),
                Kind::Quote => (&g.f.read, 18.0, 12.0),
                Kind::Rule => (&g.f.read, 0.0, 14.0),
                Kind::Para => (&g.f.read, 0.0, 14.0),
            };
            if matches!(b.kind, Kind::Heading(_)) && y > 0.0 {
                y += 18.0;
            }
            number = if let Kind::Numbered(_) = b.kind { number + 1 } else { 0 };
            if b.kind == Kind::Rule {
                items.push(Item { kind: b.kind, layout: None, y, h: 1.0, x: 0.0, spans: Vec::new(), number });
                y += 1.0 + gap * 2.0;
                continue;
            }
            let text = b.text();
            let mut spans = Vec::new();
            let mut pos = 0u32;
            for sp in &b.spans {
                let len = sp.text.encode_utf16().count() as u32;
                spans.push((pos, len, sp.style, sp.target.clone()));
                pos += len;
            }
            let layout = g.layout(&text, format, width - indent);
            if let Some(l) = &layout {
                for (start, len, style, _) in &spans {
                    match style {
                        Style::Bold => Gfx::style_range(l, *start, *len, true, false, false, None),
                        Style::Italic => Gfx::style_range(l, *start, *len, false, true, false, None),
                        Style::Code => Gfx::style_range(l, *start, *len, false, false, false, Some(&g.f.mono)),
                        Style::Link | Style::Url => Gfx::style_range(l, *start, *len, false, false, false, None),
                        _ => {}
                    }
                }
            }
            let h = layout.as_ref().map_or(0.0, Gfx::layout_height) + if b.kind == Kind::Code { 24.0 } else { 0.0 };
            items.push(Item { kind: b.kind, layout, y, h, x: indent, spans, number });
            y += h + gap;
        }
        *self.rendered.borrow_mut() = Some(Rendered { key, items, total: y });
    }

    fn max_scroll(&self) -> f32 {
        if self.current.is_none() {
            let n = self.cards().len() as f32;
            let area = self.note_rect();
            let inner_w = area.w() - 2.0 * PAD;
            let cols = ((inner_w + GAP) / (CARD_MIN_W + GAP)).floor().max(1.0);
            let rows = (n / cols).ceil();
            return (TOP + 44.0 + rows * (CARD_H + GAP) + 12.0 - self.h).max(0.0);
        }
        let total = self.rendered.borrow().as_ref().map_or(0.0, |r| r.total);
        (TOP + 44.0 + total + 40.0 - self.h).max(0.0)
    }

    /// Genel bakış: kartlar.
    fn paint_overview(&self, g: &Gfx) {
        let s = self.shared.borrow();
        let area = self.note_rect();
        let cards = self.cards();
        let head = if self.search.text.trim().is_empty() {
            t!(format!("All notes · {} · newest first", cards.len()), format!("Tüm notlar · {} · en yeni önce", cards.len()))
        } else {
            t!(format!("Results · {}", cards.len()), format!("Sonuçlar · {}", cards.len()))
        };
        g.text(&head, &g.f.small, Rect::new(area.l + PAD, TOP + 8.0, area.r - PAD, TOP + 30.0), pal().faint);
        if cards.is_empty() {
            let msg = if s.vault.notes.is_empty() {
                t!("No notes yet. Talk to it in Chat and they will appear here.", "Henüz not yok. Sohbet'te konuş, burada belirir.")
            } else {
                t!("Nothing matches", "Eşleşen yok")
            };
            g.text(msg, &g.f.text, Rect::new(area.l + PAD, TOP + 44.0, area.r - PAD, TOP + 70.0), pal().faint);
            return;
        }
        g.clip(Rect::new(area.l, TOP + 36.0, area.r, self.h), || {
            for (i, r) in cards {
                if r.b < HEADER || r.t > self.h {
                    continue;
                }
                let n = &s.vault.notes[i];
                let hovered = self.hover == Hit::Card(i);
                g.fill(r, 10.0, if hovered { pal().sel } else { pal().panel });
                g.stroke(r, 10.0, if hovered { pal().faint } else { pal().line }, 1.0);
                let (l, t) = (r.l + 14.0, r.t + 10.0);
                g.text(&n.title, &g.f.strong, Rect::new(l, t, r.r - 12.0, t + 24.0), pal().text);
                let folder = if n.folder().is_empty() { t!("(root)", "(kök)").to_string() } else { n.folder().to_string() };
                let meta = format!("{folder}  ·  {}", crate::util::short_date(n.mtime));
                g.text(&meta, &g.f.small, Rect::new(l, t + 24.0, r.r - 12.0, t + 42.0), pal().faint);
                // Önizleme etiketlerin üstünde biter; taşan kısım kırpılır.
                let body = n.preview();
                let bottom = if n.tags.is_empty() { r.b - 12.0 } else { r.b - 38.0 };
                if let Some(lay) = g.layout(&body, &g.f.small, r.w() - 28.0) {
                    g.clip(Rect::new(l, t + 48.0, r.r - 12.0, bottom), || g.draw_layout(&lay, l, t + 48.0, pal().muted, &[]));
                }
                let mut x = l;
                for tag in n.tags.iter().take(4) {
                    let label = format!("#{tag}");
                    let w = g.measure(&label, &g.f.small) + 14.0;
                    if x + w > r.r - 12.0 {
                        break;
                    }
                    let cr = Rect::new(x, r.b - 30.0, x + w, r.b - 12.0);
                    g.fill(cr, 9.0, pal().hover);
                    g.text(&label, &g.f.small_center, cr, pal().muted);
                    x += w + 6.0;
                }
            }
        });
    }

    fn body_origin(&self) -> (f32, f32) {
        (self.read_col().0, TOP + 52.0 - self.scroll)
    }

    /// Başlık düğmeleri, sağdan sola: (düğme, yer, yazı, simge, ana eylem mi).
    fn head_buttons(&self, g: &Gfx) -> Vec<(Hit, Rect, &'static str, Option<&'static str>, bool)> {
        let items: Vec<(Hit, &'static str, Option<&'static str>, bool)> = if self.editing.is_some() {
            vec![(Hit::Save, t!("Save", "Kaydet"), None, true), (Hit::Cancel, t!("Cancel", "Vazgeç"), None, false)]
        } else if self.current.is_some() {
            vec![(Hit::Edit, t!("Edit", "Düzenle"), Some("\u{E70F}"), false), (Hit::Reveal, t!("Show in folder", "Klasörde göster"), Some(ICON_FOLDER), false)]
        } else {
            vec![(Hit::OpenFolder, t!("Open folder", "Klasörü aç"), Some(ICON_FOLDER), false)]
        };
        // Notların üstündeki satırda, sağa yaslı. Yazılı düğmeler sığmıyorsa simgeli olanlar
        // yalnızca simge kalır (açıklama ipucunda).
        let area = self.note_rect();
        let full: f32 = items.iter().map(|(_, l, i, _)| button_rect(g, 0.0, 0.0, l, i.is_some()).w() + 8.0).sum();
        let compact = area.r - PAD - full < area.l + PAD + 220.0;
        let mut right = area.r - PAD;
        let mut tips = Vec::new();
        let out: Vec<_> = items
            .into_iter()
            .map(|(h, label, icon, primary)| {
                let r = if compact && icon.is_some() {
                    Rect::new(right - 36.0, BAR_CY - 16.0, right, BAR_CY + 16.0)
                } else {
                    let b = button_rect_right(g, right, BAR_CY - 16.0, label, icon.is_some());
                    Rect::new(b.l, b.t, b.r, b.t + 32.0)
                };
                right = r.l - 8.0;
                if compact && icon.is_some() {
                    tips.push((h, r, label));
                }
                (h, r, if compact && icon.is_some() { "" } else { label }, icon, primary)
            })
            .collect();
        *self.icon_tips.borrow_mut() = tips;
        out
    }

    fn backlinks(&self) -> Vec<usize> {
        let s = self.shared.borrow();
        self.current.and_then(|i| s.vault.backlinks.get(&i).cloned()).unwrap_or_default()
    }

    fn hit(&self, g: &Gfx, x: f32, y: f32) -> Hit {
        if let Some(b) = self.head_buttons(g).into_iter().find(|b| b.1.contains(x, y)) {
            return b.0;
        }
        if self.editing.is_some() && self.editor_rect().contains(x, y) {
            return Hit::Editor;
        }
        if self.search_rect().contains(x, y) {
            return Hit::Search;
        }
        if x < TREE_W {
            if self.search.text.trim().is_empty() && self.home_rect().contains(x, y) {
                return Hit::Home;
            }
            for (ry, folder, name, i) in self.tree_rows() {
                if y >= ry && y < ry + ROW && y >= TOP + 40.0 {
                    return if folder { Hit::Folder(self.folder_index(&name)) } else { Hit::Tree(i) };
                }
            }
            return Hit::None;
        }
        if self.current.is_none() {
            if y < TOP + 36.0 {
                return Hit::None;
            }
            return self.cards().into_iter().find(|(_, r)| r.contains(x, y)).map_or(Hit::None, |(i, _)| Hit::Card(i));
        }
        let nr = self.note_rect();
        if self.side_visible() && x >= nr.r {
            let top = TOP + 44.0;
            for (k, _) in self.backlinks().iter().enumerate() {
                let ry = top + k as f32 * ROW;
                if y >= ry && y < ry + ROW {
                    return Hit::Back(k);
                }
            }
            return Hit::None;
        }
        // Gövde: bağlantı vuruşu.
        let (ox, oy) = self.body_origin();
        if let Some(r) = self.rendered.borrow().as_ref() {
            for (ii, it) in r.items.iter().enumerate() {
                let (Some(l), true) = (&it.layout, y >= oy + it.y && y < oy + it.y + it.h) else { continue };
                if let Some(pos) = Gfx::hit_layout(l, x - ox - it.x, y - oy - it.y)
                    && let Some(si) = it.spans.iter().position(|(s, n, st, _)| matches!(st, Style::Link | Style::Url) && pos >= *s && pos < s + n)
                {
                    return Hit::Link(ii, si);
                }
            }
        }
        Hit::None
    }

    fn folder_index(&self, name: &str) -> usize {
        let s = self.shared.borrow();
        let folders = s.vault.folders();
        folders.iter().position(|(f, _)| (if f.is_empty() { t!("(root)", "(kök)").to_string() } else { f.clone() }) == name).unwrap_or(0)
    }

    fn redraw(&self) {
        crate::page::redraw(self.shared.borrow().hwnd);
    }

    /// Açık notu düzenlemeye başlar: ham markdown düzenleyicide.
    fn edit(&mut self) {
        let note = {
            let s = self.shared.borrow();
            self.current.and_then(|i| s.vault.notes.get(i)).map(|n| (n.path.clone(), n.body.clone()))
        };
        if let Some((path, body)) = note {
            self.editor.set_text(&body);
            self.editor.focused = true;
            self.editing = Some(path);
        }
    }

    /// Düzenlemeyi kaydeder (değiştiyse) ve okuma görünümüne döner.
    fn save(&mut self) {
        let Some(path) = self.editing.take() else { return };
        self.editor.focused = false;
        let mut s = self.shared.borrow_mut();
        let changed = s.vault.get(&path).is_none_or(|n| n.body != self.editor.text);
        if changed && let Err(e) = s.vault.write(&path, &self.editor.text) {
            crate::log!("not kaydedilemedi: {path}: {e}");
        }
        self.current = s.vault.notes.iter().position(|n| n.path == path);
        *self.rendered.borrow_mut() = None;
    }

    fn paint_tree(&self, g: &Gfx) {
        let s = self.shared.borrow();
        self.search.draw(g, self.search_rect(), self.hover == Hit::Search);
        g.clip(Rect::new(0.0, TOP + 40.0, TREE_W, self.h), || {
            if self.search.text.trim().is_empty() {
                let r = self.home_rect();
                let on = self.current.is_none();
                if on {
                    g.fill(r, 4.0, pal().sel);
                    g.fill(Rect::new(r.l, r.cy() - 7.0, r.l + 3.0, r.cy() + 7.0), 1.5, accent());
                } else if self.hover == Hit::Home {
                    g.fill(r, 4.0, pal().hover);
                }
                g.text("\u{E8A9}", &g.f.icon_small, Rect::new(r.l + 6.0, r.t, r.l + 24.0, r.b), if on { pal().text } else { pal().muted });
                g.text(t!("All notes", "Tüm notlar"), &g.f.text, Rect::new(r.l + 28.0, r.t, r.r - 6.0, r.b), if on { pal().text } else { pal().muted });
            }
            for (y, folder, name, i) in self.tree_rows() {
                let r = Rect::new(PAD - 8.0, y, TREE_W - 12.0, y + ROW);
                if folder {
                    let open = !self.collapsed.iter().any(|c| (if c.is_empty() { t!("(root)", "(kök)").to_string() } else { c.clone() }) == name);
                    let glyph = if open { "\u{E70D}" } else { "\u{E76C}" };
                    g.text(glyph, &g.f.icon_small, Rect::new(r.l + 4.0, r.t, r.l + 20.0, r.b), pal().faint);
                    g.text(&name, &g.f.strong, Rect::new(r.l + 24.0, r.t, r.r, r.b), pal().muted);
                    continue;
                }
                let n = &s.vault.notes[i];
                let selected = self.current == Some(i);
                if selected {
                    g.fill(r, 4.0, pal().sel);
                    g.fill(Rect::new(r.l, r.cy() - 7.0, r.l + 3.0, r.cy() + 7.0), 1.5, accent());
                } else if self.hover == Hit::Tree(i) {
                    g.fill(r, 4.0, pal().hover);
                }
                let x = if self.search.text.trim().is_empty() { r.l + 28.0 } else { r.l + 10.0 };
                let label = if self.search.text.trim().is_empty() { n.title.clone() } else { n.path.trim_end_matches(".md").to_string() };
                g.text(&label, &g.f.text, Rect::new(x, r.t, r.r - 6.0, r.b), if selected { pal().text } else { pal().muted });
            }
        });
        g.fill(Rect::new(TREE_W - 1.0, HEADER + 8.0, TREE_W, self.h), 0.0, pal().line);
    }

    fn paint_body(&self, g: &Gfx) {
        let nr = self.note_rect();
        if self.current.is_none() {
            self.paint_overview(g);
            return;
        }
        let s = self.shared.borrow();
        let Some(i) = self.current.filter(|&i| i < s.vault.notes.len()) else { return };
        let n = &s.vault.notes[i];
        let (ox, oy) = self.body_origin();
        g.clip(Rect::new(nr.l, HEADER, nr.r, self.h), || {
            // Başlık ve yol.
            g.text(&n.title, &g.f.h1, Rect::new(ox, oy - 40.0, nr.r - PAD, oy - 8.0), pal().text);
            let meta = if n.tags.is_empty() { n.path.clone() } else { format!("{}  ·  #{}", n.path, n.tags.join(" #")) };
            g.text(&meta, &g.f.small, Rect::new(ox, oy - 8.0, nr.r - PAD, oy + 10.0), pal().faint);
            let Some(r) = self.rendered.borrow().as_ref().map(|r| r.items.iter().map(|it| (it.kind, it.y, it.h, it.x, it.number, it.layout.clone(), it.spans.clone())).collect::<Vec<_>>()) else { return };
            let body_top = oy + 26.0;
            for (ii, (kind, y, h, x, number, layout, spans)) in r.into_iter().enumerate() {
                let top = body_top + y;
                if top + h < HEADER || top > self.h {
                    continue;
                }
                match kind {
                    Kind::Rule => {
                        g.fill(Rect::new(ox, top, nr.r - PAD, top + 1.0), 0.0, pal().line);
                        continue;
                    }
                    Kind::Code => g.fill(Rect::new(ox, top, nr.r - PAD, top + h), 6.0, pal().panel),
                    Kind::Quote => g.fill(Rect::new(ox, top, ox + 3.0, top + h), 1.5, pal().faint),
                    Kind::Bullet(_) => g.circle(ox + x - 12.0, top + 11.0, 2.5, pal().muted),
                    Kind::Task(_, done) => {
                        let b = Rect::new(ox + x - 19.0, top + 4.0, ox + x - 5.0, top + 18.0);
                        g.stroke(b, 3.0, pal().muted, 1.0);
                        if done {
                            g.text("\u{E73E}", &g.f.icon_small, b, accent());
                        }
                    }
                    Kind::Numbered(_) => {
                        g.text(&format!("{number}."), &g.f.body, Rect::new(ox + x - 24.0, top, ox + x, top + 22.0), pal().muted)
                    }
                    _ => {}
                }
                let Some(l) = layout else { continue };
                let colors: Vec<(u32, u32, Color)> = spans
                    .iter()
                    .enumerate()
                    .filter_map(|(si, (s, n, st, _))| match st {
                        Style::Link | Style::Url => Some((*s, *n, if self.hover == Hit::Link(ii, si) { pal().text } else { accent() })),
                        Style::Tag => Some((*s, *n, pal().faint)),
                        _ => None,
                    })
                    .collect();
                let color = match kind {
                    Kind::Quote => pal().muted,
                    Kind::Task(_, true) => pal().faint,
                    _ => pal().text,
                };
                let pad = if kind == Kind::Code { 8.0 } else { 0.0 };
                g.draw_layout(&l, ox + x + pad, top + pad, color, &colors);
            }
        });
    }

    fn paint_side(&self, g: &Gfx) {
        let nr = self.note_rect();
        let s = self.shared.borrow();
        g.fill(Rect::new(nr.r, HEADER + 8.0, nr.r + 1.0, self.h), 0.0, pal().line);
        let l = nr.r + 14.0;
        g.text(t!("Linked here", "Buraya bağlananlar"), &g.f.small, Rect::new(l, TOP + 8.0, self.w - 8.0, TOP + 30.0), pal().faint);
        let top = TOP + 44.0;
        let back = self.backlinks();
        if back.is_empty() {
            g.text("—", &g.f.text, Rect::new(l, top, self.w - 8.0, top + ROW), pal().faint);
        }
        for (k, i) in back.iter().enumerate() {
            let r = Rect::new(l - 6.0, top + k as f32 * ROW, self.w - 8.0, top + (k + 1) as f32 * ROW);
            if self.hover == Hit::Back(k) {
                g.fill(r, 4.0, pal().hover);
            }
            g.text(&s.vault.notes[*i].title, &g.f.text, Rect::new(r.l + 6.0, r.t, r.r - 4.0, r.b), accent());
        }
    }
}

impl Page for NotesPage {
    fn layout(&mut self, w: f32, h: f32, _head_x: f32, _head_r: f32) {
        self.w = w;
        self.h = h;
        // Sohbetten "bu notu aç" isteği.
        let req = self.shared.borrow_mut().open_note.take();
        if let Some(p) = req {
            self.open_by_name(&p);
        }
        if let Some(i) = self.current
            && i >= self.shared.borrow().vault.notes.len()
        {
            self.current = None;
        }
    }

    fn paint(&self, g: &Gfx) {
        self.render(g);
        for (h, r, label, icon, primary) in self.head_buttons(g) {
            match (label.is_empty(), icon) {
                (true, Some(ic)) => {
                    g.fill(r, 4.0, if self.hover == h { pal().hover.alpha(1.4) } else { pal().hover });
                    g.stroke(r, 4.0, pal().line, 1.0);
                    g.text(ic, &g.f.icon_small, r, pal().text);
                }
                _ => button(g, r, label, icon, primary.then(accent), self.hover == h),
            }
        }
        self.paint_tree(g);
        if self.editing.is_some() {
            self.editor.draw(g, self.editor_rect(), self.hover == Hit::Editor);
            g.text(t!("Ctrl+S saves", "Ctrl+S kaydeder"), &g.f.small, Rect::new(TREE_W + PAD, BAR_CY - 10.0, TREE_W + 240.0, BAR_CY + 10.0), pal().faint);
            return;
        }
        self.paint_body(g);
        if self.side_visible() {
            self.paint_side(g);
        }
    }

    fn mouse_move(&mut self, g: &Gfx, x: f32, y: f32) {
        if self.editing.is_some() {
            let r = self.editor_rect();
            self.editor.mouse_move(g, r, x, y);
            self.redraw();
        }
        if self.search.focused {
            let r = self.search_rect();
            self.search.mouse_move(g, r, x, y);
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
            Hit::Search => {
                let r = self.search_rect();
                self.search.mouse_down(g, r, x, y);
                self.editor.focused = false;
            }
            Hit::Editor => {
                let r = self.editor_rect();
                self.editor.mouse_down(g, r, x, y);
                self.search.focused = false;
            }
            Hit::Save | Hit::Cancel => {}
            _ => {
                self.search.focused = false;
                self.editor.focused = false;
            }
        }
        self.redraw();
    }

    fn mouse_up(&mut self, g: &Gfx, x: f32, y: f32) {
        self.editor.mouse_up();
        self.search.mouse_up();
        let hit = self.hit(g, x, y);
        if hit != std::mem::replace(&mut self.pressed, Hit::None) {
            return;
        }
        match hit {
            Hit::Tree(i) | Hit::Card(i) => self.open(i),
            Hit::Home => {
                self.current = None;
                self.scroll = 0.0;
            }
            Hit::Folder(k) => {
                let folders = self.shared.borrow().vault.folders();
                if let Some((f, _)) = folders.get(k) {
                    match self.collapsed.iter().position(|c| c == f) {
                        Some(p) => {
                            self.collapsed.remove(p);
                        }
                        None => self.collapsed.push(f.clone()),
                    }
                }
            }
            Hit::Back(k) => {
                if let Some(&i) = self.backlinks().get(k) {
                    self.open(i);
                }
            }
            Hit::Link(ii, si) => {
                let target = self.rendered.borrow().as_ref().and_then(|r| r.items.get(ii)).and_then(|it| it.spans.get(si)).map(|s| (s.2, s.3.clone()));
                if let Some((style, t)) = target {
                    match style {
                        Style::Url => crate::util::open_url(&t),
                        _ => self.open_by_name(&t),
                    }
                }
            }
            Hit::Reveal => {
                let full = {
                    let s = self.shared.borrow();
                    self.current.and_then(|i| s.vault.notes.get(i)).map(|n| s.vault.full(&n.path))
                };
                if let Some(p) = full {
                    crate::util::reveal(&p);
                }
            }
            Hit::OpenFolder => {
                let root = self.shared.borrow().vault.root.clone();
                let _ = std::fs::create_dir_all(&root);
                crate::util::open_url(&root.display().to_string());
            }
            Hit::Edit => self.edit(),
            Hit::Save => self.save(),
            Hit::Cancel => {
                self.editing = None;
                self.editor.focused = false;
            }
            Hit::Search | Hit::Editor | Hit::None => {}
        }
        self.redraw();
    }

    fn wheel(&mut self, g: &Gfx, x: f32, y: f32, delta: f32) {
        if self.editing.is_some() && x >= TREE_W {
            let r = self.editor_rect();
            self.editor.wheel(g, r, delta);
            self.redraw();
            return;
        }
        if x < TREE_W {
            self.tree_scroll = (self.tree_scroll - delta * ROW * 2.0).clamp(0.0, self.tree_max_scroll());
        } else {
            self.scroll = (self.scroll - delta * 60.0).clamp(0.0, self.max_scroll());
        }
        self.hover = self.hit(g, x, y);
        self.redraw();
    }

    fn key(&mut self, vk: u16) -> bool {
        if self.editing.is_some() && self.editor.focused {
            if self.editor.key(vk) {
                self.save();
            }
            self.redraw();
            return true;
        }
        if !self.search.focused {
            return false;
        }
        let enter = self.search.key(vk);
        if enter {
            // İlk sonucu aç.
            let first = self.tree_rows().into_iter().find(|r| !r.1).map(|r| r.3);
            if let Some(i) = first {
                self.open(i);
            }
        } else if !self.search.text.trim().is_empty() && self.current.is_some() {
            // Yazarken sonuçlar kart olarak görünsün.
            self.current = None;
            self.scroll = 0.0;
        }
        self.tree_scroll = 0.0;
        self.redraw();
        true
    }

    fn char(&mut self, c: char) -> bool {
        if self.editing.is_some() && self.editor.focused {
            if self.editor.char(c) {
                self.redraw();
            }
            return true;
        }
        if self.search.char(c) {
            self.tree_scroll = 0.0;
            if !self.search.text.trim().is_empty() {
                self.current = None;
                self.scroll = 0.0;
            }
            self.redraw();
            return true;
        }
        false
    }

    fn tip(&self) -> Option<(Rect, String)> {
        self.icon_tips.borrow().iter().find(|t| t.0 == self.hover).map(|t| (t.1, t.2.to_string()))
    }

    fn interactive(&self, g: &Gfx, x: f32, y: f32) -> bool {
        y >= HEADER || self.hit(g, x, y) != Hit::None
    }

    fn set_visible(&mut self, visible: bool) {
        if visible {
            // Dışarıda değişmiş olabilir.
            let changed = self.shared.borrow_mut().vault.refresh();
            if changed {
                *self.rendered.borrow_mut() = None;
            }
        } else {
            self.hover = Hit::None;
            self.search.focused = false;
        }
    }

    fn kill_focus(&mut self) {
        self.search.focused = false;
        self.editor.focused = false;
    }
}
