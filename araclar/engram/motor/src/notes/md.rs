//! Markdown'ın küçük bir alt kümesi: başlıklar, listeler, kod blokları, alıntı, çizgi; satır
//! içinde kalın, eğik, kod, `[[not|takma ad]]`, `[yazı](url)`, `#etiket`. Çizim için bloklara ve
//! parçalara ayırır; indeks için bağlantı ve etiketleri çıkarır.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Heading(u8),
    Para,
    /// Madde; derinlik girintiden.
    Bullet(u8),
    Numbered(u8),
    Task(u8, bool),
    Code,
    Quote,
    Rule,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    Plain,
    Bold,
    Italic,
    Code,
    /// `[[hedef]]`: görünen yazı ve hedef.
    Link,
    Url,
    Tag,
}

#[derive(Clone, Debug)]
pub struct Span {
    pub text: String,
    pub style: Style,
    /// Link / Url hedefi.
    pub target: String,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub kind: Kind,
    pub spans: Vec<Span>,
}

impl Block {
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

/// Baştaki `---` ... `---` ön bilgisini atar.
pub fn strip_frontmatter(body: &str) -> &str {
    let b = body.trim_start_matches('\u{feff}');
    if let Some(rest) = b.strip_prefix("---")
        && (rest.starts_with('\n') || rest.starts_with("\r\n"))
        && let Some(end) = rest.find("\n---")
    {
        let after = &rest[end + 4..];
        return after.trim_start_matches(['\r', '\n']);
    }
    b
}

/// Ön bilgideki `anahtar: değer` satırları.
pub fn frontmatter(body: &str) -> Vec<(String, String)> {
    let b = body.trim_start_matches('\u{feff}');
    let Some(rest) = b.strip_prefix("---") else { return Vec::new() };
    let Some(end) = rest.find("\n---") else { return Vec::new() };
    rest[..end]
        .lines()
        .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

/// Gövdedeki `[[hedef]]` ve `[[hedef|ad]]` hedefleri (`#başlık` kısmı atılır).
pub fn wikilinks(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("[[") {
        rest = &rest[i + 2..];
        let Some(j) = rest.find("]]") else { break };
        let inner = &rest[..j];
        let target = inner.split(['|', '#']).next().unwrap_or("").trim();
        if !target.is_empty() && !out.iter().any(|t: &String| t == target) {
            out.push(target.to_string());
        }
        rest = &rest[j + 2..];
    }
    out
}

/// Ön bilgideki `tags:` ve gövdedeki `#etiket`ler (başlık `#` değil: `#` sonrası harfle başlar).
pub fn tags(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (k, v) in frontmatter(body) {
        if k == "tags" || k == "etiketler" {
            for t in v.trim_matches(['[', ']']).split([',', ' ']) {
                let t = t.trim().trim_start_matches('#');
                if !t.is_empty() && !out.iter().any(|o| o == t) {
                    out.push(t.to_string());
                }
            }
        }
    }
    let text = strip_frontmatter(body);
    let mut in_code = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let bytes: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == '#' && (i == 0 || bytes[i - 1].is_whitespace()) && i + 1 < bytes.len() && bytes[i + 1].is_alphabetic() {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && (bytes[j].is_alphanumeric() || bytes[j] == '_' || bytes[j] == '-' || bytes[j] == '/') {
                    j += 1;
                }
                let t: String = bytes[start..j].iter().collect();
                if !out.iter().any(|o| *o == t) {
                    out.push(t);
                }
                i = j;
            } else {
                i += 1;
            }
        }
    }
    out
}

/// Satır içi parçalar.
pub fn inline(line: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let push_plain = |plain: &mut String, spans: &mut Vec<Span>| {
        if !plain.is_empty() {
            spans.push(Span { text: std::mem::take(plain), style: Style::Plain, target: String::new() });
        }
    };
    let find = |from: usize, pat: &str| -> Option<usize> {
        let p: Vec<char> = pat.chars().collect();
        (from..chars.len().saturating_sub(p.len() - 1)).find(|&k| chars[k..k + p.len()] == p[..])
    };
    while i < chars.len() {
        let rest2: String = chars[i..chars.len().min(i + 2)].iter().collect();
        if rest2 == "[["
            && let Some(j) = find(i + 2, "]]")
        {
            push_plain(&mut plain, &mut spans);
            let inner: String = chars[i + 2..j].iter().collect();
            let (target, alias) = inner.split_once('|').map_or((inner.as_str(), None), |(t, a)| (t, Some(a)));
            let target = target.split('#').next().unwrap_or("").trim().to_string();
            let text = alias.map_or(target.clone(), |a| a.trim().to_string());
            spans.push(Span { text, style: Style::Link, target });
            i = j + 2;
            continue;
        }
        if rest2 == "**"
            && let Some(j) = find(i + 2, "**")
        {
            push_plain(&mut plain, &mut spans);
            spans.push(Span { text: chars[i + 2..j].iter().collect(), style: Style::Bold, target: String::new() });
            i = j + 2;
            continue;
        }
        if chars[i] == '`'
            && let Some(j) = (i + 1..chars.len()).find(|&k| chars[k] == '`')
        {
            push_plain(&mut plain, &mut spans);
            spans.push(Span { text: chars[i + 1..j].iter().collect(), style: Style::Code, target: String::new() });
            i = j + 1;
            continue;
        }
        if (chars[i] == '*' || chars[i] == '_')
            && i + 1 < chars.len()
            && !chars[i + 1].is_whitespace()
            && let Some(j) = (i + 1..chars.len()).find(|&k| chars[k] == chars[i])
            && j > i + 1
        {
            push_plain(&mut plain, &mut spans);
            spans.push(Span { text: chars[i + 1..j].iter().collect(), style: Style::Italic, target: String::new() });
            i = j + 1;
            continue;
        }
        if chars[i] == '['
            && let Some(j) = (i + 1..chars.len()).find(|&k| chars[k] == ']')
            && j + 1 < chars.len()
            && chars[j + 1] == '('
            && let Some(k) = (j + 2..chars.len()).find(|&k| chars[k] == ')')
        {
            push_plain(&mut plain, &mut spans);
            spans.push(Span {
                text: chars[i + 1..j].iter().collect(),
                style: Style::Url,
                target: chars[j + 2..k].iter().collect(),
            });
            i = k + 1;
            continue;
        }
        if chars[i] == '#' && (i == 0 || chars[i - 1].is_whitespace()) && i + 1 < chars.len() && chars[i + 1].is_alphabetic() {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_' || chars[j] == '-' || chars[j] == '/') {
                j += 1;
            }
            push_plain(&mut plain, &mut spans);
            spans.push(Span { text: chars[i..j].iter().collect(), style: Style::Tag, target: String::new() });
            i = j;
            continue;
        }
        plain.push(chars[i]);
        i += 1;
    }
    push_plain(&mut plain, &mut spans);
    spans
}

/// Gövdeyi bloklara ayırır (ön bilgi atılır).
pub fn parse(body: &str) -> Vec<Block> {
    let text = strip_frontmatter(body);
    let mut blocks: Vec<Block> = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let mut code: Option<Vec<String>> = None;
    let flush_para = |para: &mut Vec<String>, blocks: &mut Vec<Block>| {
        if !para.is_empty() {
            let joined = para.join(" ");
            blocks.push(Block { kind: Kind::Para, spans: inline(&joined) });
            para.clear();
        }
    };
    for raw in text.lines() {
        let line = raw.trim_end();
        if let Some(c) = &mut code {
            if line.trim_start().starts_with("```") {
                let text = c.join("\n");
                blocks.push(Block { kind: Kind::Code, spans: vec![Span { text, style: Style::Code, target: String::new() }] });
                code = None;
            } else {
                c.push(line.to_string());
            }
            continue;
        }
        let trimmed = line.trim_start();
        let indent = (line.len() - trimmed.len()) as u8 / 2;
        if trimmed.starts_with("```") {
            flush_para(&mut para, &mut blocks);
            code = Some(Vec::new());
            continue;
        }
        if trimmed.is_empty() {
            flush_para(&mut para, &mut blocks);
            continue;
        }
        if trimmed == "---" || trimmed == "***" {
            flush_para(&mut para, &mut blocks);
            blocks.push(Block { kind: Kind::Rule, spans: Vec::new() });
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('#') {
            let level = 1 + rest.chars().take_while(|&c| c == '#').count() as u8;
            let content = rest.trim_start_matches('#');
            if content.starts_with(' ') && level <= 6 {
                flush_para(&mut para, &mut blocks);
                blocks.push(Block { kind: Kind::Heading(level), spans: inline(content.trim()) });
                continue;
            }
        }
        if let Some(rest) = trimmed.strip_prefix("> ") {
            flush_para(&mut para, &mut blocks);
            blocks.push(Block { kind: Kind::Quote, spans: inline(rest) });
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            flush_para(&mut para, &mut blocks);
            if let Some(t) = rest.strip_prefix("[ ] ") {
                blocks.push(Block { kind: Kind::Task(indent, false), spans: inline(t) });
            } else if let Some(t) = rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")) {
                blocks.push(Block { kind: Kind::Task(indent, true), spans: inline(t) });
            } else {
                blocks.push(Block { kind: Kind::Bullet(indent), spans: inline(rest) });
            }
            continue;
        }
        let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && trimmed[digits..].starts_with(". ") {
            flush_para(&mut para, &mut blocks);
            blocks.push(Block { kind: Kind::Numbered(indent), spans: inline(&trimmed[digits + 2..]) });
            continue;
        }
        para.push(trimmed.to_string());
    }
    if let Some(c) = code {
        blocks.push(Block { kind: Kind::Code, spans: vec![Span { text: c.join("\n"), style: Style::Code, target: String::new() }] });
    }
    flush_para(&mut para, &mut blocks);
    blocks
}

/// Sesli okunacak ya da tek satırda gösterilecek düz metin: başlık, liste ve satır içi işaretler
/// ayıklanır, `[[hedef|ad]]` → ad.
pub fn plain(s: &str) -> String {
    let mut out = String::new();
    for line in s.lines() {
        let l = line.trim().trim_start_matches('#').trim_start();
        let l = l.strip_prefix("- ").or(l.strip_prefix("* ")).unwrap_or(l);
        let text: String = inline(l).into_iter().map(|sp| sp.text).collect();
        if !out.is_empty() && !text.is_empty() {
            out.push(if line.trim().is_empty() { '\n' } else { ' ' });
        }
        out.push_str(&text);
    }
    if s.ends_with([' ', '\n']) {
        out.push(' ');
    }
    out
}
