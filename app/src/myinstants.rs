//! Myinstants (myinstants.com): meme ve ses efekti arşivi. Resmi bir API yok: arama sayfasının
//! HTML'i okunur. Boş aramada Türkiye'de popüler olanlar gelir. Sesler mp3.

use crate::net;
use crate::util::Res;

const SITE: &str = "https://www.myinstants.com";

pub struct Instant {
    pub name: String,
    /// mp3'ün tam adresi.
    pub url: String,
}

/// Arar; boş aramada Türkiye'de popüler olanlar. İş parçacığında çağrılır (bekletir).
pub fn search(query: &str) -> Res<Vec<Instant>> {
    let q = query.trim();
    let url =
        if q.is_empty() { format!("{SITE}/en/index/tr/") } else { format!("{SITE}/en/search/?name={}", encode(q)) };
    let html = String::from_utf8_lossy(&net::download(&url)?).into_owned();
    if html.contains("<title>Just a moment") {
        return Err(t!(
            "Myinstants is not letting us in right now (Cloudflare check); try again in a bit",
            "Myinstants şu an girişe izin vermiyor (Cloudflare doğrulaması); biraz sonra dene"
        )
        .into());
    }
    Ok(parse(&html))
}

pub fn download(url: &str) -> Res<Vec<u8>> {
    net::download(url)
}

/// Her sonuç bir `<div class="instant">`: `play('/media/sounds/…mp3', …)` ve
/// `<a class="instant-link …">Ad</a>`.
fn parse(html: &str) -> Vec<Instant> {
    html.split(r#"<div class="instant">"#)
        .skip(1)
        .filter_map(|block| {
            let path = between(block, "play('", "'")?;
            let link = between(block, r#"class="instant-link"#, "</a>")?;
            let name = unescape(link.split_once('>')?.1.trim());
            let url = if path.starts_with('/') { format!("{SITE}{path}") } else { path.to_string() };
            (!name.is_empty() && url.starts_with("https://")).then_some(Instant { name, url })
        })
        .collect()
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let rest = &s[s.find(a)? + a.len()..];
    Some(&rest[..rest.find(b)?])
}

/// `&amp;`, `&#39;`, `&#x27;` gibi HTML karakter adları.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let c = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Arama sorgusu: harf ve rakamlar olduğu gibi, boşluk `+`, gerisi `%XX` (UTF-8).
fn encode(q: &str) -> String {
    q.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Ses adından dosya adı: Windows'un yasakladığı karakterler atılır, kısaltılır.
pub fn file_name(name: &str) -> String {
    let clean: String = name.chars().filter(|c| !c.is_control() && !r#"<>:"/\|?*"#.contains(*c)).take(60).collect();
    let clean = clean.trim().trim_end_matches('.').trim();
    if clean.is_empty() { "ses".into() } else { clean.into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_result() {
        let html = r#"<div class="instant">
<div class="circle small-button-background" style="background-color:#FF0000;"></div>
<button class="small-button" onclick="play('/media/sounds/vine-boom.mp3', 'loader-81126', 'vine-boom-sound-70972')" title="Play" type="button"></button>
<a href="/en/instant/vine-boom-sound-70972/" class="instant-link link-secondary">Rock &amp; Roll &#x27;boom&#39;</a>"#;
        let r = parse(html);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "Rock & Roll 'boom'");
        assert_eq!(r[0].url, "https://www.myinstants.com/media/sounds/vine-boom.mp3");
    }

    /// Gerçek site: `cargo test -p hive myinstants -- --ignored`.
    #[test]
    #[ignore]
    fn live_search_and_download() {
        for q in ["vine boom", "ayıp", ""] {
            let r = search(q).unwrap();
            assert!(!r.is_empty(), "{q}: sonuç yok");
            println!("{q:?}: {} sonuç, ilk: {} ({})", r.len(), r[0].name, r[0].url);
        }
        let mp3 = download(&search("vine boom").unwrap()[0].url).unwrap();
        assert!(mp3.starts_with(b"ID3") || mp3[0] == 0xFF);
        println!("mp3: {} bayt", mp3.len());
    }

    #[test]
    fn encodes_turkish() {
        assert_eq!(encode("ayıp et"), "ay%C4%B1p+et");
        assert_eq!(file_name(r#"a/b: "c"?"#), "ab c");
    }
}
