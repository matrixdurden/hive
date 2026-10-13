//! Not klasörü (vault): düz markdown dosyaları, `[[bağlantı]]` ile birbirine bağlı. Klasör baştan
//! sona okunur, bellekte indekslenir (kişisel bir arşiv birkaç MB'ı geçmez): başlık, etiketler,
//! bağlantılar, geri bağlantılar, arama. Yazma işleri geçici dosya + yeniden adlandırma ile
//! yapılır; Obsidian aynı klasörü açabilir.
#![allow(dead_code)]

pub mod md;

use std::collections::HashMap;

/// Ajanın kendi notları (kişilik, kurallar): kullanıcı düzenler, arama ve grafik dışında kalır.
pub const SYSTEM_DIR: &str = "sistem";

pub fn is_system(path: &str) -> bool {
    path.starts_with("sistem/")
}
use std::path::{Path, PathBuf};

use crate::util::Res;

#[derive(Clone, Debug)]
pub struct Note {
    /// Kök klasöre göre yol, `/` ile, `.md` dahil: `kisiler/ayse.md`.
    pub path: String,
    /// Dosya adı (uzantısız): bağlantıların hedefi.
    pub title: String,
    pub body: String,
    /// Gövdedeki `[[...]]` hedefleri (anahtar biçiminde).
    pub links: Vec<String>,
    pub tags: Vec<String>,
    /// Son değişiklik (saniye, UNIX).
    pub mtime: u64,
}

impl Note {
    /// Ön bilgi (frontmatter) dışındaki ilk dolu satır.
    pub fn first_line(&self) -> &str {
        md::strip_frontmatter(&self.body).lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).unwrap_or("")
    }

    pub fn folder(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(d, _)| d)
    }

    /// Kart önizlemesi: ilk birkaç dolu satır, markdown işaretleri ayıklanmış düz metin.
    pub fn preview(&self) -> String {
        let mut out = String::new();
        for line in md::strip_frontmatter(&self.body).lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let l = line.trim_start_matches(['-', '*', '>']).trim_start_matches("[ ]").trim_start_matches("[x]").trim();
            let plain: String = md::inline(l).into_iter().map(|s| s.text).collect();
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(plain.trim());
            if out.chars().count() > 160 {
                break;
            }
        }
        out.chars().take(200).collect()
    }
}

pub struct Vault {
    pub root: PathBuf,
    pub notes: Vec<Note>,
    /// Anahtar (küçük harf başlık ya da yol) → not.
    by_key: HashMap<String, usize>,
    /// Not → ona bağlanan notlar.
    pub backlinks: HashMap<usize, Vec<usize>>,
}

/// Türkçe'ye uygun küçük harf anahtar: "Ayşe" ve "AYŞE" aynı, "I" → "ı", "İ" → "i".
pub fn key(s: &str) -> String {
    s.trim().chars().map(|c| match c {
        'I' => 'ı',
        'İ' => 'i',
        c => c.to_lowercase().next().unwrap_or(c),
    }).collect()
}

/// Dosya adı için güvenli ad: boşluklar `-`, Türkçe harfler ASCII, yalnızca harf/rakam/-/_.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in key(s).chars() {
        let c = match c {
            'ç' => 'c',
            'ğ' => 'g',
            'ı' => 'i',
            'ö' => 'o',
            'ş' => 's',
            'ü' => 'u',
            ' ' | '\t' | '/' | '\\' => '-',
            c => c,
        };
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        }
    }
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_string()
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

impl Vault {
    pub fn open(root: &Path) -> Self {
        let mut v = Vault { root: root.to_path_buf(), notes: Vec::new(), by_key: HashMap::new(), backlinks: HashMap::new() };
        v.rescan();
        v
    }

    /// Klasörü baştan okur.
    pub fn rescan(&mut self) {
        let _ = std::fs::create_dir_all(&self.root);
        let mut notes = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                if p.is_dir() {
                    stack.push(p);
                } else if name.to_ascii_lowercase().ends_with(".md")
                    && let Ok(body) = std::fs::read_to_string(&p)
                {
                    notes.push(self.make_note(&p, body));
                }
            }
        }
        notes.sort_by(|a, b| a.path.cmp(&b.path));
        self.notes = notes;
        self.reindex();
    }

    fn make_note(&self, p: &Path, body: String) -> Note {
        let rel = p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().replace('\\', "/");
        let title = rel.rsplit_once('/').map_or(rel.as_str(), |(_, f)| f).trim_end_matches(".md").to_string();
        let links = md::wikilinks(&body).into_iter().map(|l| key(&l)).collect();
        let tags = md::tags(&body);
        Note { path: rel, title, body, links, tags, mtime: mtime(p) }
    }

    fn reindex(&mut self) {
        self.by_key.clear();
        for (i, n) in self.notes.iter().enumerate() {
            self.by_key.insert(key(&n.path.trim_end_matches(".md")), i);
            self.by_key.entry(key(&n.title)).or_insert(i);
        }
        let mut back: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, n) in self.notes.iter().enumerate() {
            for l in &n.links {
                if let Some(&j) = self.by_key.get(l)
                    && j != i
                {
                    let v = back.entry(j).or_default();
                    if !v.contains(&i) {
                        v.push(i);
                    }
                }
            }
        }
        self.backlinks = back;
    }

    /// Bağlantı hedefi ya da yol → not.
    pub fn resolve(&self, name: &str) -> Option<usize> {
        let k = key(name.trim_end_matches(".md").replace('\\', "/").as_str());
        self.by_key.get(&k).copied().or_else(|| {
            // Yol verilmişse yalnızca dosya adına da bak.
            k.rsplit_once('/').and_then(|(_, f)| self.by_key.get(f).copied())
        })
    }

    pub fn get(&self, path: &str) -> Option<&Note> {
        self.resolve(path).map(|i| &self.notes[i])
    }

    /// Yolu kök altında, `.md` uzantılı, `..` içermeyen hale getirir.
    pub fn safe_path(&self, path: &str) -> Res<String> {
        let mut p = path.trim().replace('\\', "/");
        while p.starts_with("./") || p.starts_with('/') {
            p = p.trim_start_matches("./").trim_start_matches('/').to_string();
        }
        if p.is_empty() || p.split('/').any(|s| s == ".." || s.is_empty()) || p.contains(':') {
            return Err(format!("{}: {path}", t!("invalid note path", "geçersiz not yolu")).into());
        }
        if !p.to_ascii_lowercase().ends_with(".md") {
            p.push_str(".md");
        }
        Ok(p)
    }

    pub fn full(&self, rel: &str) -> PathBuf {
        self.root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR))
    }

    /// Dosyayı yazar (geçici dosya + yeniden adlandırma), indeksi günceller.
    pub fn write(&mut self, path: &str, content: &str) -> Res<String> {
        let rel = self.safe_path(path)?;
        let full = self.full(&rel);
        if let Some(dir) = full.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = full.with_extension("md.tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &full)?;
        let note = self.make_note(&full, content.to_string());
        match self.notes.iter().position(|n| n.path == rel) {
            Some(i) => self.notes[i] = note,
            None => {
                self.notes.push(note);
                self.notes.sort_by(|a, b| a.path.cmp(&b.path));
            }
        }
        self.reindex();
        Ok(rel)
    }

    /// Sona ekler; dosya yoksa oluşturur.
    pub fn append(&mut self, path: &str, text: &str) -> Res<String> {
        let rel = self.safe_path(path)?;
        let mut body = self.get(&rel).map(|n| n.body.clone()).unwrap_or_default();
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(text);
        if !body.ends_with('\n') {
            body.push('\n');
        }
        self.write(&rel, &body)
    }

    /// Diskte değişen ya da silinen notları yeniden okur (dosya izleyici, dış düzenleyici).
    pub fn refresh(&mut self) -> bool {
        let mut changed = false;
        let before: Vec<(String, u64)> = self.notes.iter().map(|n| (n.path.clone(), n.mtime)).collect();
        let mut fresh = Vault { root: self.root.clone(), notes: Vec::new(), by_key: HashMap::new(), backlinks: HashMap::new() };
        fresh.rescan();
        let after: Vec<(String, u64)> = fresh.notes.iter().map(|n| (n.path.clone(), n.mtime)).collect();
        if before != after {
            changed = true;
            *self = fresh;
        }
        changed
    }

    // --- Arama ---

    /// Başlık, etiket ve gövdede arama; puana göre, en iyi `limit` sonuç: (not, puan, parça).
    pub fn search(&self, query: &str, limit: usize) -> Vec<(usize, i32, String)> {
        let q = key(query);
        let terms: Vec<&str> = q.split_whitespace().filter(|t| t.len() >= 2).collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(usize, i32, String)> = self
            .notes
            .iter()
            .enumerate()
            .filter(|(_, n)| !is_system(&n.path))
            .filter_map(|(i, n)| {
                let title = key(&n.title);
                let body = key(&n.body);
                let mut score = 0;
                let mut snippet = String::new();
                for t in &terms {
                    if title == *t {
                        score += 30;
                    } else if title.contains(t) {
                        score += 12;
                    }
                    if n.tags.iter().any(|g| key(g).contains(t)) {
                        score += 6;
                    }
                    let count = body.matches(t).count().min(10) as i32;
                    score += count * 2;
                    if snippet.is_empty()
                        && let Some(line) = n.body.lines().find(|l| key(l).contains(t))
                    {
                        snippet = line.trim().chars().take(160).collect();
                    }
                }
                (score > 0).then_some((i, score, snippet))
            })
            .collect();
        hits.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| self.notes[b.0].mtime.cmp(&self.notes[a.0].mtime)));
        hits.truncate(limit);
        hits
    }

    /// Sistem notu: yoksa varsayılanla oluşturur, içeriğini verir.
    pub fn system_note(&mut self, name: &str, default: &str) -> String {
        let path = format!("{SYSTEM_DIR}/{name}.md");
        if let Some(n) = self.get(&path) {
            return n.body.clone();
        }
        let _ = self.write(&path, default);
        default.to_string()
    }

    /// Sistem istemine giden indeks: her not bir satır, `yol | etiketler | ilk satır`.
    pub fn index_text(&self) -> String {
        let mut s = String::new();
        for n in self.notes.iter().filter(|n| !is_system(&n.path)) {
            let tags = if n.tags.is_empty() { String::new() } else { format!(" #{}", n.tags.join(" #")) };
            let first: String = n.first_line().chars().take(100).collect();
            s.push_str(&format!("{}{} | {}\n", n.path, tags, first));
        }
        s
    }

    /// Klasörler (kök dahil, sıralı) ve içlerindeki notlar: ağaç görünümü için.
    pub fn folders(&self) -> Vec<(String, Vec<usize>)> {
        let mut map: std::collections::BTreeMap<String, Vec<usize>> = std::collections::BTreeMap::new();
        for (i, n) in self.notes.iter().enumerate() {
            map.entry(n.folder().to_string()).or_default().push(i);
        }
        map.into_iter().collect()
    }
}

/// `engram --vault-test`: klasörü tarar, indeksi ve geri bağlantıları yazdırır.
pub fn probe(root: &Path) -> String {
    let v = Vault::open(root);
    let mut s = format!("klasör: {}\nnot: {}\n", root.display(), v.notes.len());
    for (i, n) in v.notes.iter().enumerate() {
        let back = v.backlinks.get(&i).map_or(0, Vec::len);
        s += &format!("  {} | bağlantı {} | geri {} | #{}\n", n.path, n.links.len(), back, n.tags.join(" #"));
    }
    s += "--- indeks ---\n";
    s += &v.index_text();
    s
}
