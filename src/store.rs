//! What hark saves: subscriptions (standard OPML), one cache file per feed, listening state for
//! the episodes you've touched, and preferences. Plain text, written atomically.

use std::{collections::HashMap, path::PathBuf};

use quick_xml::{Reader, events::Event};

use crate::{
    feed::{Episode, Show},
    paths::{cache_dir, data_dir, fnv, write_atomic},
};

/// A subscription: the feed url and the name to show until the feed itself has loaded.
#[derive(Clone, Debug, PartialEq)]
pub struct Sub {
    pub url: String,
    pub title: String,
}

fn subs_path() -> PathBuf {
    data_dir().join("subscriptions.opml")
}

pub fn load_subs() -> Vec<Sub> {
    std::fs::read(subs_path()).map(|b| parse_opml(&b)).unwrap_or_default()
}

pub fn save_subs(subs: &[Sub]) -> std::io::Result<()> {
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<opml version=\"2.0\">\n  <head><title>hark subscriptions</title></head>\n  <body>\n");
    for s in subs {
        x.push_str(&format!("    <outline type=\"rss\" text=\"{0}\" title=\"{0}\" xmlUrl=\"{1}\"/>\n", esc(&s.title), esc(&s.url)));
    }
    x.push_str("  </body>\n</opml>\n");
    write_atomic(&subs_path(), x.as_bytes())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Every `<outline xmlUrl=…>` at any depth (other apps nest them in folders).
pub fn parse_opml(xml: &[u8]) -> Vec<Sub> {
    let mut r = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out: Vec<Sub> = Vec::new();
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e) | Event::Empty(e)) if e.name().as_ref().eq_ignore_ascii_case("outline") => {
                let get = |k: &str| e.try_get_attribute(k).ok().flatten().and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok()).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
                if let Some(url) = get("xmlUrl").or_else(|| get("xmlurl"))
                    && !out.iter().any(|s| s.url == url)
                {
                    let title = get("title").or_else(|| get("text")).unwrap_or_else(|| url.clone());
                    out.push(Sub { url, title });
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// A feed as last fetched, plus what the server needs to answer "not modified".
#[derive(Default)]
pub struct Cached {
    pub show: Show,
    pub etag: String,
    pub last_modified: String,
    /// When it was fetched (Unix seconds).
    pub fetched: i64,
}

fn feed_path(url: &str) -> PathBuf {
    cache_dir().join("feeds").join(format!("{:016x}.tsv", fnv(url)))
}

/// Tabs and newlines would break the line format; titles never need them.
fn clean(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

// shortcut: show notes aren't cached (Phase 1 doesn't show them); they arrive with each refresh.
pub fn save_feed(url: &str, c: &Cached) -> std::io::Result<()> {
    let s = &c.show;
    let mut out = format!(
        "{}\t{}\t{}\t{}\t{}\t{}\n",
        clean(&c.etag),
        clean(&c.last_modified),
        c.fetched,
        clean(&s.title),
        clean(&s.author),
        clean(s.image.as_deref().unwrap_or(""))
    );
    for e in &s.episodes {
        out.push_str(&format!("{}\t{}\t{}\t{}\t{}\t{}\n", clean(&e.guid), e.published, e.duration_s, clean(&e.url), clean(&e.mime), clean(&e.title)));
    }
    write_atomic(&feed_path(url), out.as_bytes())
}

pub fn load_feed(url: &str) -> Option<Cached> {
    let text = std::fs::read_to_string(feed_path(url)).ok()?;
    let mut lines = text.lines();
    let h: Vec<&str> = lines.next()?.split('\t').collect();
    let field = |i: usize| h.get(i).copied().unwrap_or("").to_string();
    let mut show = Show { title: field(3), author: field(4), image: Some(field(5)).filter(|s| !s.is_empty()), ..Default::default() };
    for l in lines {
        let f: Vec<&str> = l.splitn(6, '\t').collect();
        if let [guid, published, dur, url, mime, title] = f[..] {
            show.episodes.push(Episode {
                guid: guid.into(),
                published: published.parse().unwrap_or(0),
                duration_s: dur.parse().unwrap_or(0),
                url: url.into(),
                mime: mime.into(),
                title: title.into(),
                ..Default::default()
            });
        }
    }
    Some(Cached { show, etag: field(0), last_modified: field(1), fetched: field(2).parse().unwrap_or(0) })
}

pub fn forget_feed(url: &str) {
    let _ = std::fs::remove_file(feed_path(url));
}

/// Listening state of one episode.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Listen {
    /// Seconds in; 0 = not started.
    pub pos: f64,
    pub played: bool,
}

fn state_path() -> PathBuf {
    data_dir().join("state.tsv")
}

/// guid → state, only for episodes you've touched.
pub fn load_state() -> HashMap<String, Listen> {
    let text = std::fs::read_to_string(state_path()).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let guid = f.next()?.to_string();
            let pos = f.next()?.parse().ok()?;
            let played = f.next()? == "1";
            Some((guid, Listen { pos, played }))
        })
        .collect()
}

pub fn save_state(state: &HashMap<String, Listen>) -> std::io::Result<()> {
    let mut out = String::with_capacity(state.len() * 48);
    for (g, l) in state {
        if l.played || l.pos > 0.0 {
            out.push_str(&format!("{}\t{:.1}\t{}\n", clean(g), l.pos, l.played as u8));
        }
    }
    write_atomic(&state_path(), out.as_bytes())
}

/// Speed and volume, remembered between runs.
pub struct Prefs {
    pub speed: f32,
    /// 0..=100
    pub volume: u8,
}

pub fn load_prefs() -> Prefs {
    let text = std::fs::read_to_string(data_dir().join("prefs")).unwrap_or_default();
    let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')?.trim().parse::<f32>().ok());
    Prefs { speed: get("speed").unwrap_or(1.0).clamp(0.5, 3.0), volume: get("volume").unwrap_or(100.0).clamp(0.0, 100.0) as u8 }
}

pub fn save_prefs(p: &Prefs) -> std::io::Result<()> {
    write_atomic(&data_dir().join("prefs"), format!("speed={}\nvolume={}\n", p.speed, p.volume).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opml_from_other_apps() {
        let x = br#"<?xml version="1.0"?><opml version="1.0"><body>
          <outline text="feeds">
            <outline type="rss" text="A &amp; B" xmlUrl="https://a.example/feed?x=1&amp;y=2"/>
            <outline text="no title attr" xmlUrl="https://b.example/rss"></outline>
            <outline text="dupe" xmlUrl="https://b.example/rss"/>
            <outline text="a folder without a feed"/>
          </outline></body></opml>"#;
        let subs = parse_opml(x);
        assert_eq!(subs, [Sub { url: "https://a.example/feed?x=1&y=2".into(), title: "A & B".into() }, Sub { url: "https://b.example/rss".into(), title: "no title attr".into() }]);
    }

    #[test]
    fn opml_round_trip() {
        let subs = vec![Sub { url: "https://x.example/f?a=1&b=\"2\"".into(), title: "Tom & <Jerry>".into() }];
        let mut x = String::new();
        for s in &subs {
            x.push_str(&format!("<outline text=\"{}\" xmlUrl=\"{}\"/>", esc(&s.title), esc(&s.url)));
        }
        assert_eq!(parse_opml(format!("<opml><body>{x}</body></opml>").as_bytes()), subs);
    }
}
