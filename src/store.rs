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
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        clean(&c.etag),
        clean(&c.last_modified),
        c.fetched,
        clean(&s.title),
        clean(&s.author),
        clean(s.image.as_deref().unwrap_or("")),
        clean(&s.categories.join("|"))
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
    let categories = field(6).split('|').filter(|c| !c.is_empty()).map(str::to_string).collect();
    let mut show = Show { title: field(3), author: field(4), image: Some(field(5)).filter(|s| !s.is_empty()), categories, ..Default::default() };
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
    // a cache from before categories were kept (6 fields): forget its validators so the next
    // refresh fetches the whole feed once instead of getting "not modified"
    let old = h.len() < 7;
    let (etag, last_modified) = if old { (String::new(), String::new()) } else { (field(0), field(1)) };
    Some(Cached { show, etag, last_modified, fetched: field(2).parse().unwrap_or(0) })
}

pub fn forget_feed(url: &str) {
    let _ = std::fs::remove_file(feed_path(url));
    let _ = std::fs::remove_file(notes_path(url));
}

/// Listening state of one episode.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Listen {
    /// Seconds in; 0 = not started.
    pub pos: f64,
    pub played: bool,
    /// When it last changed (Unix seconds), for "In progress" order.
    pub updated: i64,
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
            let updated = f.next().and_then(|u| u.parse().ok()).unwrap_or(0);
            Some((guid, Listen { pos, played, updated }))
        })
        .collect()
}

pub fn save_state(state: &HashMap<String, Listen>) -> std::io::Result<()> {
    let mut out = String::with_capacity(state.len() * 48);
    for (g, l) in state {
        if l.played || l.pos > 0.0 {
            out.push_str(&format!("{}\t{:.1}\t{}\t{}\n", clean(g), l.pos, l.played as u8, l.updated));
        }
    }
    write_atomic(&state_path(), out.as_bytes())
}

/// Speed, volume and theme, remembered between runs.
pub struct Prefs {
    pub speed: f32,
    /// 0..=100
    pub volume: u8,
    pub theme: usize,
    /// Cover art in the sidebar (`i` toggles).
    pub art: bool,
}

pub fn load_prefs() -> Prefs {
    let text = std::fs::read_to_string(data_dir().join("prefs")).unwrap_or_default();
    let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')?.trim().parse::<f32>().ok());
    let theme = text.lines().find_map(|l| l.strip_prefix("theme=")).map_or(0, |t| crate::theme::by_name(t.trim()));
    let art = !text.lines().any(|l| l.trim() == "art=off");
    Prefs { speed: get("speed").unwrap_or(1.0).clamp(0.5, 3.0), volume: get("volume").unwrap_or(100.0).clamp(0.0, 100.0) as u8, theme, art }
}

pub fn save_prefs(p: &Prefs) -> std::io::Result<()> {
    let text = format!("speed={}\nvolume={}\ntheme={}\nart={}\n", p.speed, p.volume, crate::theme::name(p.theme), if p.art { "on" } else { "off" });
    write_atomic(&data_dir().join("prefs"), text.as_bytes())
}

/// The queue: (feed url, episode guid), in play order.
pub fn load_queue() -> Vec<(String, String)> {
    let text = std::fs::read_to_string(data_dir().join("queue.tsv")).unwrap_or_default();
    text.lines().filter_map(|l| l.split_once('\t')).map(|(u, g)| (u.to_string(), g.to_string())).collect()
}

pub fn save_queue<'a>(q: impl Iterator<Item = (&'a str, &'a str)>) -> std::io::Result<()> {
    let text: String = q.map(|(u, g)| format!("{}\t{}\n", clean(u), clean(g))).collect();
    write_atomic(&data_dir().join("queue.tsv"), text.as_bytes())
}

fn notes_path(url: &str) -> PathBuf {
    cache_dir().join("feeds").join(format!("{:016x}.notes", fnv(url)))
}

/// Show notes live in their own file per feed and are read one episode at a time, when shown:
/// together they're most of a feed's size (6 MB for The Daily).
pub fn save_notes(url: &str, eps: &[Episode]) -> std::io::Result<()> {
    let mut out = String::new();
    for e in eps.iter().filter(|e| !e.notes.is_empty()) {
        out.push_str(&format!("{}\t{}\n", clean(&e.guid), escape_line(&e.notes)));
    }
    write_atomic(&notes_path(url), out.as_bytes())
}

pub fn has_notes(url: &str) -> bool {
    notes_path(url).exists()
}

pub fn load_notes(url: &str, guid: &str) -> Option<String> {
    let text = std::fs::read_to_string(notes_path(url)).ok()?;
    let line = text.lines().find_map(|l| l.strip_prefix(guid)?.strip_prefix('\t'))?;
    Some(unescape_line(line))
}

/// Multi-line text on one line: `\` → `\\`, newline → `\n`, tab → `\t`.
fn escape_line(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\t', "\\t").replace('\r', "")
}

fn unescape_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match (c, c == '\\').1.then(|| it.next()).flatten() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(o) => out.push(o),
            None if c != '\\' => out.push(c),
            None => {}
        }
    }
    out
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
    fn notes_escape_round_trip() {
        for s in ["plain", "two\nlines", "tab\there", "back\\slash\\n not a newline", "", "end\\"] {
            let e = escape_line(s);
            assert!(!e.contains('\n') && !e.contains('\t'), "{e:?}");
            assert_eq!(unescape_line(&e), s);
        }
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
